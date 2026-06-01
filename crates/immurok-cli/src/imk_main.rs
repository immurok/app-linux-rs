//! `imk` — agent-aware command wrapper for AI tooling on Linux.
//!
//! Mirrors macOS `imk` (CLISources/main.swift, commit 6d9fbc2). When an AI
//! agent (Claude Code / Cursor / Cline / …) wraps a command with
//! `imk run --agent -- <cmd>`, this binary asks the daemon to surface a
//! desktop notification + terminal prompt for explicit fingerprint approval
//! BEFORE the wrapped command runs. On success the daemon arms a 5-minute
//! sudo pre-auth window so any sudo invoked inside the wrapped command
//! auto-passes without re-prompting.
//!
//! For now this binary only implements `run --agent` (the P0#1 surface).
//! `imk list` / `imk get` (cache reads) come in a later iteration.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use immurok_common::protocol;

const EXIT_REJECTED: i32 = 77; // EX_NOPERM — agent rejected by user
const EXIT_USAGE: i32 = 64;    // EX_USAGE
const EXIT_CONFIG: i32 = 78;   // EX_CONFIG — preflight failed (no SSH key)
const EXIT_GENERIC: i32 = 1;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exit = run(args);
    std::process::exit(exit);
}

fn run(args: Vec<String>) -> i32 {
    if args.is_empty() {
        return print_usage(EXIT_USAGE);
    }
    match args[0].as_str() {
        "run" => cmd_run(&args[1..]),
        "list" => cmd_list(&args[1..]),
        "get" => cmd_get(&args[1..]),
        "version" | "--version" | "-v" => {
            println!("imk {}", env!("CARGO_PKG_VERSION"));
            0
        }
        "help" | "--help" | "-h" => print_usage(0),
        other => {
            eprintln!("imk: unknown subcommand '{}'", other);
            print_usage(EXIT_USAGE)
        }
    }
}

fn print_usage(code: i32) -> i32 {
    eprintln!(
        "imk - immurok agent CLI\n\n\
        Usage:\n  \
          imk run --agent -- COMMAND [ARGS...]   Run COMMAND under agent approval\n  \
          imk list <ssh|otp|api>                 List cached key names\n  \
          imk get imk://<cat>/<name>             Print secret value to stdout\n  \
          imk version                            Print version\n\n\
        Notes:\n  \
          --agent surfaces a desktop notification and terminal prompt; you must\n  \
          touch the device to approve. On success a 5-minute sudo pre-auth\n  \
          window is armed so any sudo inside COMMAND auto-passes.\n  \
          \n  \
          'get' for api/otp categories triggers a fingerprint gate; ssh just\n  \
          returns the cached OpenSSH public key without device contact."
    );
    code
}

fn cmd_run(args: &[String]) -> i32 {
    let mut is_agent = false;
    let mut cmd_args: Vec<String> = Vec::new();
    let mut parsing_flags = true;

    let mut i = 0;
    while i < args.len() {
        if parsing_flags {
            match args[i].as_str() {
                "--" => {
                    parsing_flags = false;
                    i += 1;
                    continue;
                }
                "--agent" => {
                    is_agent = true;
                    i += 1;
                    continue;
                }
                _ => {}
            }
        }
        cmd_args.push(args[i].clone());
        i += 1;
    }

    if cmd_args.is_empty() {
        eprintln!("imk: nothing to run");
        return print_usage(EXIT_USAGE);
    }

    // Preflight: bail before AgentApproval if the wrapped command would
    // need an SSH key but the device's keystore is empty. Otherwise the
    // subprocess (git push / ssh / scp / rsync) silently falls back to
    // password auth and hangs on stdin — and in --agent mode the user
    // already burned a fingerprint on the (now useless) approval.
    if let Some(reason) = ssh_preflight_failure(&cmd_args) {
        eprintln!("{}", reason);
        return EXIT_CONFIG;
    }

    let cmd_string = cmd_args.join(" ");

    if is_agent {
        match request_agent_approval(&cmd_string) {
            ApprovalResult::Approved => {
                eprintln!("imk: agent command approved");
            }
            ApprovalResult::Rejected => {
                eprintln!("imk: agent command rejected by user");
                return EXIT_REJECTED;
            }
            ApprovalResult::Error(msg) => {
                eprintln!("imk: agent approval failed: {}", msg);
                return EXIT_GENERIC;
            }
        }
    }

    // Drop a marker keyed by our own PID so the daemon's PAM AUTH path can
    // walk the wrapped subprocess's parent chain and recognize "this is
    // running under an agent wrap" — useful when the 5-min sudo pre-auth
    // window expires mid-command and a late sudo arrives at raw AUTH. See
    // commit 31aa5f6 (macOS) for the originating defense-in-depth design.
    let marker_guard = if is_agent {
        AgentMarker::write(&cmd_string).map(MarkerGuard).ok()
    } else {
        None
    };

    // Spawn (rather than exec) so the marker can be cleaned up after the
    // wrapped command exits. Force SSH_AUTH_SOCK to the daemon's actual
    // agent socket: user shells often have a stale path baked in (e.g.
    // ~/.zshrc still pointing at ~/.immurok/agent.sock from a previous
    // Linux install), and the wrapped subprocess inherits that → git push
    // / ssh get "Connection refused" from the dead socket and silently
    // fall through to password.
    let mut command = Command::new(&cmd_args[0]);
    command.args(&cmd_args[1..]);
    if let Ok(sock) = ssh_agent_socket_path() {
        command.env("SSH_AUTH_SOCK", sock);
    }

    let exit_code = match command.spawn() {
        Ok(mut child) => match child.wait() {
            Ok(status) => {
                if let Some(code) = status.code() {
                    code
                } else {
                    // Killed by signal — propagate via 128+signum.
                    128 + status.signal().unwrap_or(0)
                }
            }
            Err(e) => {
                eprintln!("imk: wait failed: {}", e);
                EXIT_GENERIC
            }
        },
        Err(e) => {
            eprintln!("imk: spawn '{}' failed: {}", cmd_args[0], e);
            127
        }
    };

    drop(marker_guard);
    exit_code
}

// ── AgentMarker (defense-in-depth for late sudo after pre-auth expiry) ──

/// File at `~/.immurok/markers/<pid>`:
///   line 1: expiry epoch (seconds since 1970)
///   line 2 (optional): wrapped command, single line, ≤1024 chars
/// Mode 0600 so other local users can't fabricate markers under our PID.
/// Mirrors macOS `AgentMarker` (CLISources/AgentMarker.swift) format.
struct AgentMarker;

impl AgentMarker {
    const TTL_SECS: u64 = 3600;
    const MAX_CMD_LEN: usize = 1024;

    fn directory() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home).join(".immurok").join("markers")
    }

    fn path_for(pid: u32) -> PathBuf {
        Self::directory().join(pid.to_string())
    }

    fn write(command: &str) -> std::io::Result<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let dir = Self::directory();
        std::fs::create_dir_all(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;

        let pid = std::process::id();
        let expiry = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() + Self::TTL_SECS)
            .unwrap_or(0);

        // Sanitize: collapse newlines + cap length so the file stays
        // bounded even for runaway scripts.
        let cmd_one_line: String = command
            .replace(['\n', '\r'], " ")
            .chars()
            .take(Self::MAX_CMD_LEN)
            .collect();

        let path = Self::path_for(pid);
        let body = format!("{}\n{}\n", expiry, cmd_one_line);
        std::fs::write(&path, body)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(path)
    }

    fn remove(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
    }
}

/// RAII wrapper so the marker is removed on imk exit even if the wrapped
/// command panics or wait() errors out.
struct MarkerGuard(PathBuf);

impl Drop for MarkerGuard {
    fn drop(&mut self) {
        AgentMarker::remove(&self.0);
    }
}

/// Daemon's SSH agent socket path — same XDG_RUNTIME_DIR convention as
/// daemon's main.rs (with /run/user/$UID fallback resolved via $UID env
/// var which is always set by login/systemd-user). If neither is set,
/// fail-open: don't override SSH_AUTH_SOCK and let whatever is in the
/// parent env propagate.
fn ssh_agent_socket_path() -> Result<String, String> {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return Ok(format!(
            "{}/immurok/{}",
            runtime_dir,
            protocol::AGENT_SOCKET_NAME
        ));
    }
    if let Ok(uid) = std::env::var("UID") {
        return Ok(format!(
            "/run/user/{}/immurok/{}",
            uid,
            protocol::AGENT_SOCKET_NAME
        ));
    }
    Err("XDG_RUNTIME_DIR / UID not set".into())
}

enum ApprovalResult {
    Approved,
    Rejected,
    Error(String),
}

fn request_agent_approval(cmd: &str) -> ApprovalResult {
    let socket_path = match daemon_socket_path() {
        Ok(p) => p,
        Err(e) => return ApprovalResult::Error(e),
    };

    eprintln!("imk: touch device to approve [{}]", truncate_for_log(cmd, 80));

    let stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(e) => {
            return ApprovalResult::Error(format!(
                "cannot connect to daemon at {} ({}). Is immurok-daemon running?",
                socket_path, e
            ));
        }
    };

    // 35s server-side timeout + small grace for the network round-trip.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(40)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let request = format!("AGENT_APPROVE:{}\n", cmd);
    {
        let mut writer = match stream.try_clone() {
            Ok(s) => s,
            Err(e) => return ApprovalResult::Error(format!("clone failed: {}", e)),
        };
        if let Err(e) = writer.write_all(request.as_bytes()) {
            return ApprovalResult::Error(format!("send failed: {}", e));
        }
    }

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    if let Err(e) = reader.read_line(&mut response) {
        return ApprovalResult::Error(format!("recv failed: {}", e));
    }
    let resp = response.trim();
    // Daemon serialize_response format: "OK:msg" / "DENY:msg" / "ERROR:msg"
    if let Some(rest) = resp.strip_prefix("OK:") {
        if rest == "APPROVED" {
            ApprovalResult::Approved
        } else {
            ApprovalResult::Error(format!("unexpected OK payload: {}", rest))
        }
    } else if let Some(rest) = resp.strip_prefix("DENY:") {
        // REJECTED is the explicit user-rejected case; anything else is
        // surfaced as an error so callers can distinguish "not allowed
        // right now" from "you said no".
        if rest == "REJECTED" {
            ApprovalResult::Rejected
        } else {
            ApprovalResult::Error(format!("denied: {}", rest))
        }
    } else if let Some(rest) = resp.strip_prefix("ERROR:") {
        ApprovalResult::Error(rest.to_string())
    } else {
        ApprovalResult::Error(format!("unexpected response: {}", resp))
    }
}

fn daemon_socket_path() -> Result<String, String> {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return Ok(format!(
            "{}/immurok/{}",
            runtime_dir,
            protocol::PAM_SOCKET_NAME
        ));
    }
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    Ok(format!(
        "{}/{}/{}",
        home,
        protocol::IMMUROK_DIR,
        protocol::PAM_SOCKET_NAME
    ))
}

fn cmd_list(args: &[String]) -> i32 {
    let cat = match args.first() {
        Some(c) => c.as_str(),
        None => {
            eprintln!("Usage: imk list <ssh|otp|api>");
            return EXIT_USAGE;
        }
    };
    if !matches!(cat, "ssh" | "otp" | "api") {
        eprintln!("Unknown category: {}", cat);
        return EXIT_USAGE;
    }

    // Daemon LIST response: "OK:N\n<line>\n<line>\n…\n\n" — multi-line with
    // a blank-line terminator. Reuse the AGENT socket but read until EOF
    // (server closes after writing the response).
    let resp = match send_request(&format!("LIST:{}\n", cat)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e);
            return EXIT_GENERIC;
        }
    };

    let lines: Vec<&str> = resp.split('\n').collect();
    let header = lines.first().copied().unwrap_or("");
    if let Some(rest) = header.strip_prefix("ERROR:") {
        eprintln!("Error: {}", rest);
        return EXIT_GENERIC;
    }
    let count_str = match header.strip_prefix("OK:") {
        Some(s) => s,
        None => {
            eprintln!("Unexpected response: {}", header);
            return EXIT_GENERIC;
        }
    };
    let count: usize = count_str.trim().parse().unwrap_or(0);
    if count == 0 {
        eprintln!("No {} keys", cat);
        return 0;
    }
    // Each subsequent non-empty line is one entry. For ssh the line is
    // `<name>\tecdsa-sha2-nistp256 <base64>` — print that as-is so the
    // caller can pipe to authorized_keys via `cut -f2`.
    for line in lines.iter().skip(1) {
        if line.is_empty() {
            continue;
        }
        println!("{}", line);
    }
    0
}

fn cmd_get(args: &[String]) -> i32 {
    let reference = match args.first() {
        Some(r) => r.as_str(),
        None => {
            eprintln!("Usage: imk get imk://<category>/<name>");
            return EXIT_USAGE;
        }
    };
    let body = reference.strip_prefix("imk://").unwrap_or(reference);
    let mut split = body.splitn(2, '/');
    let cat = split.next().unwrap_or("");
    let name = split.next().unwrap_or("");
    if cat.is_empty() || name.is_empty() {
        eprintln!("Invalid reference: {} (expected imk://category/name)", reference);
        return EXIT_USAGE;
    }

    let resp = match send_request(&format!("GET:{}:{}\n", cat, name)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e);
            return EXIT_GENERIC;
        }
    };
    // Single-line response: "OK:<value>\n" or "ERROR:<reason>\n"
    let trimmed = resp.trim_end_matches('\n');
    if let Some(rest) = trimmed.strip_prefix("ERROR:") {
        eprintln!("Error: {}", rest);
        return EXIT_GENERIC;
    }
    let value = match trimmed.strip_prefix("OK:") {
        Some(v) => v,
        None => {
            eprintln!("Unexpected response: {}", trimmed);
            return EXIT_GENERIC;
        }
    };
    // Trailing newline only when stdout is a TTY — preserves
    // `export X=$(imk get …)` semantics on pipes/redirects (command
    // substitution strips trailing \n anyway).
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        println!("{}", value);
    } else {
        print!("{}", value);
    }
    0
}

/// Returns an error message if the wrapped command would need an SSH key on
/// the device but none is configured; None if the check passes / doesn't
/// apply / can't be answered (fail-open).
///
/// Mirrors macOS commit 46c15ec. Conservative scope:
///   - ssh / scp / sftp / rsync → strictly need SSH
///   - git {push|pull|fetch|clone|ls-remote} → only flag SSH-style remotes
///     (git@…, ssh://…, git+ssh://…). HTTPS remotes pass through.
fn ssh_preflight_failure(cmd_args: &[String]) -> Option<String> {
    let bin = std::path::Path::new(&cmd_args[0])
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    let strictly_needs_ssh = matches!(bin, "ssh" | "scp" | "sftp" | "rsync");
    let might_need_ssh = bin == "git"
        && cmd_args.len() > 1
        && matches!(
            cmd_args[1].as_str(),
            "push" | "pull" | "fetch" | "clone" | "ls-remote"
        );

    if !strictly_needs_ssh && !might_need_ssh {
        return None;
    }

    if might_need_ssh && !strictly_needs_ssh {
        let remote_url = if cmd_args[1] == "clone" {
            cmd_args.get(2).cloned().unwrap_or_default()
        } else {
            // git push|pull|fetch [remote] — positional remote before any
            // flag, default origin.
            let remote_name = cmd_args
                .get(2)
                .filter(|s| !s.starts_with('-'))
                .cloned()
                .unwrap_or_else(|| "origin".to_string());
            run_capture(&["git", "remote", "get-url", &remote_name]).unwrap_or_default()
        };
        let is_ssh = remote_url.starts_with("git@")
            || remote_url.starts_with("ssh://")
            || remote_url.starts_with("git+ssh://");
        if !is_ssh {
            return None;
        }
    }

    // Query daemon's SSH key cache via LIST:ssh. If daemon is unreachable
    // we fail-open (returning None) so the command runs and surfaces its
    // own error rather than us pretending to know.
    let resp = match send_request("LIST:ssh\n") {
        Ok(r) => r,
        Err(_) => return None,
    };
    let header = resp.split('\n').next().unwrap_or("");
    if let Some(count_str) = header.strip_prefix("OK:") {
        if count_str.trim().parse::<usize>().unwrap_or(0) == 0 {
            return Some(format!(
                "imk: device has no SSH keys but '{}' needs one. \
                 Generate / import one first:\n  \
                 immurok-cli key generate-ssh <name>\n  \
                 immurok-cli key import-ssh <name> <keyfile>",
                bin
            ));
        }
    }
    None
}

fn run_capture(argv: &[&str]) -> Option<String> {
    let out = std::process::Command::new(argv[0]).args(&argv[1..]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// Send a single request line to the daemon socket, read everything until
/// EOF (server closes after writing the full response). Used by LIST/GET
/// where the response may span multiple lines.
fn send_request(request: &str) -> Result<String, String> {
    use std::io::Read;
    let socket_path = daemon_socket_path()?;
    let stream = UnixStream::connect(&socket_path)
        .map_err(|e| format!("cannot connect to daemon at {} ({})", socket_path, e))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(45)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    {
        let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
        writer
            .write_all(request.as_bytes())
            .map_err(|e| format!("send failed: {}", e))?;
    }

    let mut reader = stream;
    let mut buf = String::new();
    reader
        .read_to_string(&mut buf)
        .map_err(|e| format!("recv failed: {}", e))?;
    Ok(buf)
}

fn truncate_for_log(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}
