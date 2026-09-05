//! Manage the immurok-owned block in ~/.ssh/config that routes SSH through
//! the daemon's fingerprint-gated SSH agent (see docs spec 2026-07-31).
//!
//! Lives in the shared crate because the daemon can no longer write it: it
//! runs as a system user with ProtectHome=yes. The user-session agent applies
//! the toggle on the daemon's behalf.
//!
//! enable() prepends a sentinel-delimited `Host * / IdentityAgent <sock>`
//! block; disable() removes it. Both preserve all user content outside the
//! block. The socket path is written as a resolved absolute path — ssh does
//! no env expansion for us.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const BEGIN: &str = "# >>> immurok managed — do not edit (toggle in immurok app) >>>";
const END: &str = "# <<< immurok managed <<<";

/// Managed block text (always ends with a trailing newline).
fn block(agent_sock: &Path) -> String {
    format!(
        "{BEGIN}\nHost *\n    IdentityAgent {}\n{END}\n",
        agent_sock.display()
    )
}

/// Remove any existing managed block from `content`, returning remaining user
/// lines joined by '\n' with surrounding blank lines trimmed.
///
/// Only removes a well-formed block (matching BEGIN...END pair).
/// If BEGIN is present without a matching END, returns content unchanged
/// to preserve user content (unterminated block is degenerate but safe).
fn strip_block(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();

    // Find the BEGIN line
    let begin_idx = match lines.iter().position(|line| line.trim() == BEGIN) {
        Some(idx) => idx,
        None => return content.to_string(), // No BEGIN, return as-is
    };

    // Find the END line after BEGIN
    let end_idx = match lines[begin_idx..].iter().skip(1).position(|line| line.trim() == END) {
        Some(relative_idx) => begin_idx + 1 + relative_idx,
        None => return content.to_string(), // No END after BEGIN, return as-is (preserve content)
    };

    // Remove lines from begin_idx to end_idx inclusive
    let mut result = lines;
    result.drain(begin_idx..=end_idx);

    result.join("\n").trim_matches('\n').to_string()
}

fn config_path(home: &Path) -> PathBuf {
    home.join(".ssh").join("config")
}

fn write_atomic(path: &Path, content: &str) -> io::Result<()> {
    let tmp = path.with_extension("immurok-tmp");
    std::fs::write(&tmp, content)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Ensure the managed block (pointing at `agent_sock`) sits at the top of
/// `<home>/.ssh/config`. Idempotent: replaces an existing block. Creates
/// ~/.ssh (0700) and the config file (0600) if missing.
pub fn enable(home: &Path, agent_sock: &Path) -> io::Result<()> {
    let ssh_dir = home.join(".ssh");
    std::fs::create_dir_all(&ssh_dir)?;
    std::fs::set_permissions(&ssh_dir, std::fs::Permissions::from_mode(0o700))?;

    let path = config_path(home);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let body = strip_block(&existing);

    let mut content = block(agent_sock);
    if !body.is_empty() {
        content.push('\n');
        content.push_str(&body);
        content.push('\n');
    }
    write_atomic(&path, &content)
}

/// Remove the managed block. No-op if the file or block is absent. Preserves
/// all other content.
pub fn disable(home: &Path) -> io::Result<()> {
    let path = config_path(home);
    let existing = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !existing.contains(BEGIN) {
        return Ok(());
    }
    let body = strip_block(&existing);
    let content = if body.is_empty() {
        String::new()
    } else {
        format!("{body}\n")
    };
    write_atomic(&path, &content)
}

/// Whether the managed block is currently present.
pub fn is_enabled(home: &Path) -> bool {
    std::fs::read_to_string(config_path(home))
        .map(|s| s.contains(BEGIN))
        .unwrap_or(false)
}

/// The daemon's SSH agent socket path — resolved identically to where the
/// daemon binds it (immurok_common::paths).
fn resolved_agent_sock() -> PathBuf {
    crate::paths::agent_socket()
}

/// Apply the toggle state using environment-resolved HOME + agent socket.
pub fn apply(enabled: bool) -> io::Result<()> {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "HOME not set"))?;
    if enabled {
        enable(&home, &resolved_agent_sock())
    } else {
        disable(&home)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn sock() -> PathBuf {
        PathBuf::from("/run/user/1000/immurok/agent.sock")
    }

    #[test]
    fn enable_on_missing_file_creates_block_at_top() {
        let home = tempfile::tempdir().unwrap();
        enable(home.path(), &sock()).unwrap();

        let cfg = home.path().join(".ssh").join("config");
        let content = std::fs::read_to_string(&cfg).unwrap();
        assert!(content.starts_with(BEGIN));
        assert!(content.contains("IdentityAgent /run/user/1000/immurok/agent.sock"));
        assert!(content.contains(END));
        // config is 0600
        let mode = std::fs::metadata(&cfg).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(is_enabled(home.path()));
    }

    #[test]
    fn enable_preserves_existing_user_content_below_block() {
        let home = tempfile::tempdir().unwrap();
        let ssh_dir = home.path().join(".ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();
        let cfg = ssh_dir.join("config");
        std::fs::write(&cfg, "Host myserver\n    HostName 1.2.3.4\n").unwrap();

        enable(home.path(), &sock()).unwrap();

        let content = std::fs::read_to_string(&cfg).unwrap();
        assert!(content.starts_with(BEGIN));
        assert!(content.contains("Host myserver"));
        assert!(content.contains("HostName 1.2.3.4"));
    }

    #[test]
    fn enable_is_idempotent_and_replaces_stale_path() {
        let home = tempfile::tempdir().unwrap();
        enable(home.path(), &PathBuf::from("/run/user/1000/immurok/agent.sock")).unwrap();
        enable(home.path(), &PathBuf::from("/run/user/1000/immurok/agent.sock")).unwrap();
        enable(home.path(), &PathBuf::from("/run/user/2000/immurok/agent.sock")).unwrap();

        let content = std::fs::read_to_string(home.path().join(".ssh").join("config")).unwrap();
        // exactly one block
        assert_eq!(content.matches(BEGIN).count(), 1);
        assert_eq!(content.matches(END).count(), 1);
        // reflects the latest path only
        assert!(content.contains("/run/user/2000/immurok/agent.sock"));
        assert!(!content.contains("/run/user/1000/immurok/agent.sock"));
    }

    #[test]
    fn disable_removes_block_keeps_user_content() {
        let home = tempfile::tempdir().unwrap();
        let ssh_dir = home.path().join(".ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();
        std::fs::write(ssh_dir.join("config"), "Host keep\n    User me\n").unwrap();

        enable(home.path(), &sock()).unwrap();
        disable(home.path()).unwrap();

        let content = std::fs::read_to_string(ssh_dir.join("config")).unwrap();
        assert!(!content.contains(BEGIN));
        assert!(!content.contains(END));
        assert!(content.contains("Host keep"));
        assert!(content.contains("User me"));
        assert!(!is_enabled(home.path()));
    }

    #[test]
    fn disable_on_missing_file_is_noop() {
        let home = tempfile::tempdir().unwrap();
        disable(home.path()).unwrap(); // must not error
        assert!(!is_enabled(home.path()));
    }

    #[test]
    fn socket_path_written_literally() {
        let home = tempfile::tempdir().unwrap();
        let odd = Path::new("/run/user/31337/immurok/agent.sock");
        enable(home.path(), odd).unwrap();
        let content = std::fs::read_to_string(home.path().join(".ssh").join("config")).unwrap();
        assert!(content.contains("IdentityAgent /run/user/31337/immurok/agent.sock"));
    }

    #[test]
    fn strip_block_keeps_content_when_block_unterminated() {
        // Simulate a corrupted/unterminated block: BEGIN present but no END
        let unterminated_config = format!(
            "{}\nHost *\n    IdentityAgent /run/user/1000/immurok/agent.sock\n\nHost myserver\n    HostName 1.2.3.4\n",
            BEGIN
        );

        let result = strip_block(&unterminated_config);

        // Should NOT strip anything when END is missing — preserves user content
        assert!(result.contains("Host myserver"), "unterminated block: user content was lost!");
        assert!(result.contains("HostName 1.2.3.4"), "unterminated block: user content was lost!");
    }

    #[test]
    fn strip_block_keeps_content_when_block_unterminated_round_trip() {
        // Verify the round-trip: enable then disable preserves user content even with unterminated block
        let home = tempfile::tempdir().unwrap();
        let ssh_dir = home.path().join(".ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();

        // Create a corrupted config: has BEGIN but no END
        let corrupted_config = format!(
            "{}\nHost *\n    IdentityAgent /old/agent.sock\n\nHost myserver\n    HostName 1.2.3.4\n",
            BEGIN
        );
        std::fs::write(ssh_dir.join("config"), &corrupted_config).unwrap();

        // enable() should handle the unterminated block gracefully
        enable(home.path(), &sock()).unwrap();
        let content_after_enable = std::fs::read_to_string(ssh_dir.join("config")).unwrap();
        assert!(content_after_enable.contains("Host myserver"), "enable() lost user content!");
        assert!(content_after_enable.contains("HostName 1.2.3.4"), "enable() lost user content!");

        // disable() should preserve content
        disable(home.path()).unwrap();
        let content_after_disable = std::fs::read_to_string(ssh_dir.join("config")).unwrap();
        assert!(content_after_disable.contains("Host myserver"), "disable() lost user content!");
        assert!(content_after_disable.contains("HostName 1.2.3.4"), "disable() lost user content!");
    }
}
