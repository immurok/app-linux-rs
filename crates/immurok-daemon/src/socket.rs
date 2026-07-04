//! PAM socket server — handles PAM authentication and CLI management requests.
//!
//! Listens on `~/.immurok/pam.sock` (chmod 0o666 so PAM-as-root can connect).
//! Verifies peer credentials via `SO_PEERCRED` (accept root or current user).
//! Dispatches parsed requests to handler functions and returns serialized responses.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tracing::{debug, info, warn};

use immurok_common::protocol;
use immurok_common::socket_proto::{parse_request, serialize_response, Request, Response};

use crate::coordinator::Coordinator;
use crate::ota;

/// Main socket server loop.
pub async fn serve(coordinator: Arc<Coordinator>, socket_path: &Path) {
    // Remove stale socket
    let _ = std::fs::remove_file(socket_path);

    let listener = match UnixListener::bind(socket_path) {
        Ok(l) => l,
        Err(e) => {
            warn!("Failed to bind PAM socket at {}: {}", socket_path.display(), e);
            return;
        }
    };

    // chmod 0o666 so PAM module running as root can connect
    if let Err(e) = std::fs::set_permissions(
        socket_path,
        std::fs::Permissions::from_mode(0o666),
    ) {
        warn!("Failed to chmod socket: {}", e);
    }

    info!("PAM socket server listening on {}", socket_path.display());

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let coord = coordinator.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, coord).await {
                        debug!("Client handler error: {}", e);
                    }
                });
            }
            Err(e) => {
                warn!("Socket accept error: {}", e);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// Peer PID from SO_PEERCRED — used to classify the caller (agent vs manual)
/// by walking /proc parent chain looking for an `imk run --agent` marker.
/// Returns None if the lookup fails (rare; only seen on lo-fi sockets).
fn peer_pid_of(stream: &UnixStream) -> Option<u32> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let ret = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if ret == 0 {
        Some(cred.pid as u32)
    } else {
        None
    }
}

/// Walk the parent chain of `start_pid` (max 12 levels) looking for an
/// `imk run --agent` marker file. Returns `Some(command)` on the first
/// match — highest-confidence signal that this PAM AUTH was triggered
/// by an AI agent wrap. `None` means either manual user action (terminal
/// sudo) or an agent we can't recognize. Mirrors macOS
/// AuthCallerClassifier.classify (Sources/AuthCallerClassifier.swift),
/// but Linux-flavored (/proc instead of libproc).
///
/// Marker format mirrors imk_main.rs AgentMarker.write:
///   line 1: expiry epoch (seconds)
///   line 2 (optional): wrapped command string
fn classify_agent_marker(start_pid: u32) -> Option<String> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let home = std::env::var("HOME").ok()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut pid = start_pid;
    for _ in 0..12 {
        let marker_path = format!("{}/.immurok/markers/{}", home, pid);
        if let Ok(content) = std::fs::read_to_string(&marker_path) {
            let mut lines = content.split('\n');
            if let Some(expiry_str) = lines.next() {
                if let Ok(expiry) = expiry_str.trim().parse::<u64>() {
                    if now <= expiry {
                        let cmd = lines.next().unwrap_or("").trim();
                        return Some(if cmd.is_empty() { "<unknown>".into() } else { cmd.into() });
                    }
                    // Stale: best-effort cleanup so it doesn't pollute.
                    let _ = std::fs::remove_file(&marker_path);
                }
            }
        }

        // Walk up: read /proc/<pid>/status, find PPid line.
        let parent = match read_ppid(pid) {
            Some(p) if p > 1 => p,
            _ => break, // hit init or unreadable
        };
        pid = parent;
    }
    None
}

fn read_ppid(pid: u32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("PPid:") {
            return rest.trim().parse().ok();
        }
    }
    None
}

/// Verify peer credentials via SO_PEERCRED.
/// Accept root (UID 0), current user, or polkitd (runs pam_immurok as its own UID).
fn verify_peer_credentials(stream: &UnixStream) -> Result<(), String> {
    use std::os::unix::io::AsRawFd;

    let fd = stream.as_raw_fd();
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;

    let ret = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };

    if ret != 0 {
        return Err("getsockopt(SO_PEERCRED) failed".into());
    }

    let my_uid = unsafe { libc::getuid() };
    if cred.uid != 0 && cred.uid != my_uid {
        // Check if peer is polkitd — PAM modules run inside polkitd's process
        let is_polkitd = unsafe {
            let pw = libc::getpwnam(c"polkitd".as_ptr());
            !pw.is_null() && (*pw).pw_uid == cred.uid
        };
        if !is_polkitd {
            return Err(format!(
                "Rejected peer UID {} (expected 0, {}, or polkitd)",
                cred.uid, my_uid
            ));
        }
    }

    Ok(())
}

/// Handle a single client connection.
async fn handle_client(
    mut stream: UnixStream,
    coord: Arc<Coordinator>,
) -> Result<(), String> {
    if let Err(e) = verify_peer_credentials(&stream) {
        warn!("Peer credential check failed: {}", e);
        return Err(e);
    }

    // Read first request with timeout
    let mut buf = vec![0u8; 512];
    let n = match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => n,
        Ok(Ok(_)) => return Ok(()), // EOF
        Ok(Err(e)) => return Err(format!("Read error: {}", e)),
        Err(_) => return Err("Read timeout".into()),
    };

    let raw = String::from_utf8_lossy(&buf[..n]);
    let line = raw.trim_matches(|c: char| c == '\0' || c == '\n' || c == '\r' || c == ' ');
    info!("Socket request: {}", line);

    // OTA commands use a persistent session
    if line.starts_with("OTA:") {
        ota::handle_ota_session(&mut stream, &coord, line).await;
        return Ok(());
    }

    // KEY:* commands (generate, delete, etc.)
    if line.starts_with("KEY:") {
        let resp = handle_key_command(line, &coord).await;
        let wire = format!("{}\n", resp);
        let _ = stream.write_all(wire.as_bytes()).await;
        return Ok(());
    }

    // AGENT_APPROVE:<command-string> — pre-execution approval for agent-
    // wrapped commands. The command may legitimately contain ':' so we
    // strip the prefix instead of going through the colon-split parser.
    if let Some(cmd) = line.strip_prefix("AGENT_APPROVE:") {
        let resp = handle_agent_approve(&coord, cmd, &mut stream).await;
        let wire = format!("{}\n", serialize_response(&resp));
        let _ = stream.write_all(wire.as_bytes()).await;
        return Ok(());
    }

    // LIST:<cat> and GET:<cat>:<name> — imk read paths. Multi-line
    // responses (LIST returns N+1 lines) so written raw without serialize.
    if let Some(cat) = line.strip_prefix("LIST:") {
        let resp = handle_list_keys(&coord, cat).await;
        let _ = stream.write_all(resp.as_bytes()).await;
        return Ok(());
    }
    if let Some(after) = line.strip_prefix("GET:") {
        // Disambiguate from GET:SETTINGS / GET:INFO which fall through to
        // parse_request — those use uppercase tokens, key categories use
        // lowercase ssh/otp/api.
        if after.starts_with("ssh:") || after.starts_with("otp:") || after.starts_with("api:") {
            let resp = handle_get_key(&coord, after, &mut stream).await;
            let _ = stream.write_all(resp.as_bytes()).await;
            return Ok(());
        }
    }

    let request = match parse_request(line) {
        Ok(r) => r,
        Err(e) => {
            let resp = serialize_response(&Response::Error(format!("PARSE_ERROR:{}", e)));
            let _ = stream.write_all(resp.as_bytes()).await;
            return Ok(());
        }
    };

    let response = dispatch_request(request, &coord, &mut stream).await;
    let wire = format!("{}\n", serialize_response(&response));
    let _ = stream.write_all(wire.as_bytes()).await;
    Ok(())
}

/// Dispatch a parsed request to the appropriate handler.
async fn dispatch_request(
    request: Request,
    coord: &Arc<Coordinator>,
    stream: &mut UnixStream,
) -> Response {
    match request {
        Request::Status => handle_status(coord).await,
        Request::Auth { user, service } => handle_auth(coord, &user, &service, stream).await,
        Request::FpList => handle_fp_list(coord).await,
        Request::FpEnroll { slot } => handle_fp_enroll(coord, slot, stream).await,
        Request::FpEnrollCancel => handle_fp_enroll_cancel(coord).await,
        Request::FpDelete { slot } => handle_fp_delete(coord, slot, stream).await,
        Request::FpVerify => handle_fp_verify(coord).await,
        Request::FpStatus => handle_fp_status(coord).await,
        Request::FpLastMatch => handle_fp_last_match(coord).await,
        Request::GateCancel => handle_gate_cancel(coord).await,
        Request::PairStatus => handle_pair_status(coord).await,
        Request::PairStart => handle_pair_start(coord).await,
        Request::PairReset => handle_pair_reset(coord).await,
        Request::SetUnlockSudo(v) => handle_set_setting(coord, "unlock_sudo", v).await,
        Request::SetUnlockPolkit(v) => handle_set_setting(coord, "unlock_polkit", v).await,
        Request::SetUnlockScreen(v) => handle_set_setting(coord, "unlock_screen", v).await,
        Request::SetLockScreen(v) => handle_set_setting(coord, "lock_screen", v).await,
        Request::GetSettings => handle_get_settings(coord).await,
        Request::GetInfo => handle_get_info(coord).await,
        // OTA commands handled in session wrapper, but in case of stray ones:
        Request::OtaStart { .. } | Request::OtaData(_) | Request::OtaFinish => {
            Response::Error("OTA commands require OTA session".into())
        }
    }
}

// ── STATUS ──────────────────────────────────────────────────

async fn handle_status(coord: &Arc<Coordinator>) -> Response {
    let connected = coord.is_connected.load(Ordering::Relaxed);
    let status = coord.device_status.read().await;
    let (name, battery, version) = if let Some(ref s) = *status {
        (
            "immurok".to_string(),
            s.battery,
            s.fw_version.clone(),
        )
    } else {
        (String::new(), 0, String::new())
    };

    Response::Status {
        connected,
        name,
        battery,
        version,
    }
}

// ── AUTH ────────────────────────────────────────────────────

/// Check if the service is allowed by user settings.
fn is_service_allowed(settings: &crate::settings::Settings, service: &str) -> bool {
    let s = service.to_lowercase();
    if s.contains("gdm") || s.contains("login") {
        return settings.unlock_screen;
    }
    if s == "polkit-1" {
        return settings.unlock_polkit;
    }
    // sudo and anything else
    settings.unlock_sudo
}

async fn handle_auth(
    coord: &Arc<Coordinator>,
    user: &str,
    service: &str,
    stream: &mut UnixStream,
) -> Response {
    // Classify caller via /proc parent-chain marker scan. Helps distinguish
    // "user typed sudo in terminal" from "agent's wrapped command escaped
    // the 5-min pre-auth window and re-hit raw AUTH". Currently only used
    // for log enrichment — daemon doesn't change behavior based on it
    // (Linux has no overlay distinguishing the two; the FP gate runs the
    // same way). But surfaced in journal for diagnostics.
    let agent_context = peer_pid_of(stream).and_then(classify_agent_marker);
    if let Some(ref cmd) = agent_context {
        info!(
            "AUTH request: user={}, service={} (agent context: {})",
            user, service, cmd
        );
    } else {
        info!("AUTH request: user={}, service={}", user, service);
    }

    // 1. Check settings
    {
        let settings = coord.settings.read().await;
        if !is_service_allowed(&settings, service) {
            info!("AUTH denied (service disabled): {}", service);
            return Response::Deny("SERVICE_DISABLED".into());
        }
    }

    // 2. Check pre-auth window (must match service binding)
    if coord.consume_pre_auth(service).await {
        info!("AUTH approved via pre-auth: user={} service={}", user, service);
        return Response::Ok("PRE_AUTH".into());
    }

    // 3. Device must be connected and verified
    if !coord.is_connected.load(Ordering::Relaxed) {
        warn!("AUTH denied: device not connected");
        return Response::Deny("NOT_CONNECTED".into());
    }
    if !coord.is_device_verified.load(Ordering::Relaxed) {
        warn!("AUTH denied: device not verified");
        return Response::Deny("NOT_VERIFIED".into());
    }

    // 4. Race between BLE AUTH_REQUEST and proactive FP match (0x21).
    //    GDM sends PAM auth immediately on lock — if the user touches the sensor,
    //    the 0x21 notification arrives via on_fp_match() which can approve the
    //    pending_pam channel, so we don't need to wait for AUTH_REQUEST's own
    //    WAIT_FP → second-touch cycle.
    let is_graphical = {
        let s = service.to_lowercase();
        s.contains("gdm") || s.contains("login") || s == "polkit-1"
    };
    let dialog_proc = if is_graphical {
        spawn_auth_dialog()
    } else {
        None
    };

    // Set up pending_pam so on_fp_match() can approve us via 0x21.
    // Refuse if another AUTH is already in flight — overwriting would
    // route the next FP match to the wrong PAM request and hang the
    // first one. PAM module sees BUSY and decides whether to retry.
    let (pending_tx, pending_rx) = tokio::sync::oneshot::channel::<bool>();
    if !coord.try_set_pending_pam(pending_tx).await {
        warn!("AUTH busy: another auth in flight (user={} service={})", user, service);
        kill_auth_dialog(dialog_proc);
        return Response::Error("BUSY".into());
    }

    let auth_fut = coord.ble_auth_request();
    let pending_fut = pending_rx;

    // Monitor PAM socket for disconnect — if the user cancels (Ctrl+C
    // or keypress), the socket closes and we should abort the BLE auth
    // to stop the device's green LED immediately.
    let mut disconnect_buf = [0u8; 1];
    let result = tokio::select! {
        // Path A: BLE AUTH_REQUEST completed (device-side fingerprint)
        ble_result = auth_fut => {
            match ble_result {
                Ok(true) => {
                    info!("AUTH approved via BLE: {}", user);
                    AuthResult::Approved
                }
                Ok(false) => {
                    info!("AUTH denied (FP mismatch or timeout): {}", user);
                    AuthResult::Denied
                }
                Err(e) => {
                    warn!("AUTH error: {}", e);
                    AuthResult::Denied
                }
            }
        }
        // Path B: on_fp_match() approved via 0x21 notification
        pending_result = pending_fut => {
            if pending_result.unwrap_or(false) {
                info!("AUTH approved via FP match (0x21): {}", user);
                AuthResult::Approved
            } else {
                AuthResult::Denied
            }
        }
        // Path C: PAM socket closed (user cancelled)
        r = stream.read(&mut disconnect_buf) => {
            match r {
                Ok(0) | Err(_) => {
                    info!("AUTH cancelled: PAM socket closed");
                    // Send GATE_CANCEL to stop device LED
                    let _ = coord.ble_send(protocol::CMD_GATE_CANCEL, vec![]).await;
                    AuthResult::Denied
                }
                Ok(_) => {
                    // Unexpected data — treat as cancel
                    info!("AUTH cancelled: unexpected data on PAM socket");
                    let _ = coord.ble_send(protocol::CMD_GATE_CANCEL, vec![]).await;
                    AuthResult::Denied
                }
            }
        }
    };

    kill_auth_dialog(dialog_proc);
    coord.deny_pending_pam().await; // clear any remaining pending

    match result {
        AuthResult::Approved => Response::Ok("AUTHENTICATED".into()),
        AuthResult::Denied => Response::Deny("FP_DENIED".into()),
        AuthResult::Timeout => Response::Deny("TIMEOUT".into()),
    }
}

enum AuthResult {
    Approved,
    Denied,
    // Reserved for a future explicit timeout path; currently folded into Denied.
    #[allow(dead_code)]
    Timeout,
}

/// Spawn the auth-dialog GUI subprocess (shows "Touch sensor" prompt).
fn spawn_auth_dialog() -> Option<tokio::process::Child> {
    // Look for immurok-auth-dialog in PATH or next to the daemon binary
    let dialog_name = "immurok-auth-dialog";

    // Try next to our own binary first
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(dialog_name);
            if candidate.exists() {
                match tokio::process::Command::new(&candidate)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    Ok(child) => return Some(child),
                    Err(e) => debug!("Failed to spawn auth-dialog from exe dir: {}", e),
                }
            }
        }
    }

    // Try PATH
    match tokio::process::Command::new(dialog_name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => Some(child),
        Err(_) => {
            debug!("immurok-auth-dialog not found");
            None
        }
    }
}

/// Kill the auth-dialog subprocess. Sends SIGTERM (graceful) so the dialog's
/// signal handler can do a clean Adw quit + exit 0; if the child has
/// already exited, this is a no-op.
fn kill_auth_dialog(proc: Option<tokio::process::Child>) {
    if let Some(mut child) = proc {
        if let Some(pid) = child.id() {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        } else {
            let _ = child.start_kill();
        }
    }
}

/// Spawn the agent-mode dialog. Same script as the PAM auth dialog,
/// invoked with `--agent --command CMD --timeout SECS` so it shows the
/// command pill + countdown ring instead of the bare "Touch sensor"
/// prompt. Returns None if the script can't be located/spawned — caller
/// continues with FP gate just without UI, same fallback as PAM AUTH.
fn spawn_agent_dialog(cmd: &str, timeout_secs: u64) -> Option<tokio::process::Child> {
    let dialog_name = "immurok-auth-dialog";

    let try_spawn = |path: &std::path::Path| -> Option<tokio::process::Child> {
        tokio::process::Command::new(path)
            .arg("--agent")
            .arg("--command")
            .arg(cmd)
            .arg("--timeout")
            .arg(timeout_secs.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()
    };

    // 1. next to the daemon binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(dialog_name);
            if candidate.exists() {
                if let Some(child) = try_spawn(&candidate) {
                    return Some(child);
                }
            }
        }
    }

    // 2. fall back to PATH (`immurok-auth-dialog` installed via Makefile)
    try_spawn(std::path::Path::new(dialog_name))
}

/// Wait for the dialog process to exit. Resolves to None forever if no
/// dialog was spawned (so the tokio::select! arm just never fires).
async fn wait_dialog(
    proc: Option<&mut tokio::process::Child>,
) -> Option<std::process::ExitStatus> {
    let child = proc?;
    child.wait().await.ok()
}

// ── FP commands ─────────────────────────────────────────────

async fn handle_fp_list(coord: &Arc<Coordinator>) -> Response {
    if !coord.is_connected.load(Ordering::Relaxed) {
        return Response::Error("NOT_CONNECTED".into());
    }

    // Always query device for latest bitmap — avoids stale cache issues
    // from race conditions between gate RSP_OK and FP_LIST responses.
    // FP:LIST is only polled every ~2s, so one BLE round-trip is fine.
    match coord.ble_send(protocol::CMD_FP_LIST, vec![]).await {
        Ok((status, payload)) => {
            info!("FP:LIST BLE response: status=0x{:02x} payload={:?}", status, payload);
            if status == protocol::RSP_OK && !payload.is_empty() {
                let mut ds = coord.device_status.write().await;
                if let Some(ref mut s) = *ds {
                    s.fp_bitmap = payload[0];
                    info!("FP:LIST set bitmap={}", s.fp_bitmap);
                }
            }
        }
        Err(e) => {
            info!("FP:LIST BLE error: {}", e);
        }
    }

    let ds = coord.device_status.read().await;
    if let Some(ref s) = *ds {
        info!("FP:LIST returning {}", s.fp_bitmap);
        Response::Ok(format!("{}", s.fp_bitmap))
    } else {
        Response::Error("NO_STATUS".into())
    }
}

async fn handle_fp_enroll(coord: &Arc<Coordinator>, slot: u8, stream: &mut UnixStream) -> Response {
    if slot >= protocol::MAX_FINGERPRINT_SLOTS {
        return Response::Error("INVALID_SLOT".into());
    }
    if !coord.is_connected.load(Ordering::Relaxed) {
        return Response::Error("NOT_CONNECTED".into());
    }

    // Clear previous enroll event before starting
    *coord.last_enroll_event.write().await = None;

    // Mirrors mac sheet.onDisappear (215b3d1): if the CLI hangs up before
    // ENROLL_START's FP gate finishes (Ctrl+C, terminal close), fire
    // gate_cancel so the BLE worker can bail out and write GATE_CANCEL —
    // otherwise the firmware sits on s_pending_cmd=ENROLL_START until the
    // 25s timeout and any stray FP match in that window triggers an
    // unintended enrollment.
    let gated_fut = coord.ble_send_fp_gated(protocol::CMD_ENROLL_START, vec![slot]);
    let mut disconnect_buf = [0u8; 1];
    let result = tokio::select! {
        r = gated_fut => r,
        r = stream.read(&mut disconnect_buf) => {
            match r {
                Ok(0) | Err(_) => {
                    info!("ENROLL cancelled: CLI socket closed");
                    coord.gate_cancel.notify_one();
                }
                Ok(_) => {
                    info!("ENROLL cancelled: unexpected data on CLI socket");
                    coord.gate_cancel.notify_one();
                }
            }
            Err("cancelled".to_string())
        }
    };

    match result {
        // Ok(_) already covers every success case, including
        // RSP_ERR_FP_NOT_MATCH, so a dedicated guard arm for it is
        // unreachable (clippy::unreachable_patterns) — removed, no
        // behavior change.
        Ok(_) => Response::Ok("ENROLL_STARTED".into()),
        Err(e) => Response::Error(format!("ENROLL_FAILED:{}", e)),
    }
}

/// Cancel an enrolment-in-progress. Two paths matter:
///  - ENROLL_START still waiting on the FP gate: BLE worker is parked
///    inside send_fp_gated_inner, so a queued BleCommand never reaches the
///    wire (the queue is blocked behind us, same root cause as mac
///    cdd6b07). Triggering `gate_cancel` lets the gated loop bail out and
///    write GATE_CANCEL via the helper directly.
///  - ENROLL_START already returned, firmware is in 12-step capture: BLE
///    worker is back in its main loop, ENROLL_CANCEL goes through the
///    queue normally.
///
/// Fire both — notify is no-op when no gate is active, ENROLL_CANCEL is
/// idempotent on the firmware side.
async fn handle_fp_enroll_cancel(coord: &Arc<Coordinator>) -> Response {
    if !coord.is_connected.load(Ordering::Relaxed) {
        return Response::Error("NOT_CONNECTED".into());
    }

    *coord.last_enroll_event.write().await = None;

    coord.gate_cancel.notify_one();

    match coord
        .ble_send(protocol::CMD_ENROLL_CANCEL, vec![])
        .await
    {
        Ok(_) => Response::Ok("ENROLL_CANCELLED".into()),
        Err(e) => Response::Error(format!("BLE_SEND_FAILED:{}", e)),
    }
}

async fn handle_fp_delete(coord: &Arc<Coordinator>, slot: u8, stream: &mut UnixStream) -> Response {
    if !coord.is_connected.load(Ordering::Relaxed) {
        return Response::Error("NOT_CONNECTED".into());
    }

    // Same socket-close cancel pattern as handle_fp_enroll. DELETE_FP is
    // FP-gated, and the gate occupies the BLE worker until the user
    // touches the sensor — without this, a CLI Ctrl+C leaves
    // s_pending_cmd=DELETE_FP armed and any subsequent FP match in the
    // 25s window deletes the slot.
    let gated_fut = coord.ble_send_fp_gated(protocol::CMD_DELETE_FP, vec![slot]);
    let mut disconnect_buf = [0u8; 1];
    let result = tokio::select! {
        r = gated_fut => r,
        r = stream.read(&mut disconnect_buf) => {
            match r {
                Ok(0) | Err(_) => {
                    info!("DELETE cancelled: CLI socket closed");
                    coord.gate_cancel.notify_one();
                }
                Ok(_) => {
                    info!("DELETE cancelled: unexpected data on CLI socket");
                    coord.gate_cancel.notify_one();
                }
            }
            Err("cancelled".to_string())
        }
    };

    match result {
        Ok(_) => {
            coord.fp_bitmap_stale.store(true, Ordering::Relaxed);
            Response::Ok("DELETED".into())
        }
        Err(e) => Response::Error(format!("DELETE_FAILED:{}", e)),
    }
}

async fn handle_fp_verify(coord: &Arc<Coordinator>) -> Response {
    if !coord.is_connected.load(Ordering::Relaxed) {
        return Response::Error("NOT_CONNECTED".into());
    }
    if !coord.is_device_verified.load(Ordering::Relaxed) {
        return Response::Error("NOT_VERIFIED".into());
    }

    match coord
        .ble_send_fp_gated(protocol::CMD_AUTH_REQUEST, vec![])
        .await
    {
        Ok((status, _))
            if status == protocol::RSP_OK || status == protocol::RSP_FP_GATE_APPROVED =>
        {
            Response::Ok("MATCH".into())
        }
        Ok((status, _)) if status == protocol::RSP_ERR_FP_NOT_MATCH => {
            Response::Ok("NO_MATCH".into())
        }
        Ok((status, _)) if status == protocol::RSP_ERR_TIMEOUT => {
            Response::Ok("NO_MATCH".into())
        }
        Ok((status, _)) => Response::Error(format!("VERIFY_FAILED:0x{:02x}", status)),
        Err(e) => Response::Error(format!("BLE_SEND_FAILED:{}", e)),
    }
}

async fn handle_fp_status(coord: &Arc<Coordinator>) -> Response {
    let ev = coord.last_enroll_event.read().await;
    match *ev {
        Some((status, current, total)) => {
            Response::Ok(format!("{}:{}:{}", status, current, total))
        }
        None => Response::Ok("IDLE".into()),
    }
}

async fn handle_fp_last_match(_coord: &Arc<Coordinator>) -> Response {
    // TODO: track last match page_id in coordinator
    Response::Ok("-1".into())
}

// ── GATE:CANCEL ─────────────────────────────────────────────

async fn handle_gate_cancel(coord: &Arc<Coordinator>) -> Response {
    if !coord.is_connected.load(Ordering::Relaxed) {
        return Response::Error("NOT_CONNECTED".into());
    }

    match coord
        .ble_send(protocol::CMD_GATE_CANCEL, vec![])
        .await
    {
        Ok(_) => Response::Ok("GATE_CANCELLED".into()),
        Err(e) => Response::Error(format!("BLE_SEND_FAILED:{}", e)),
    }
}

// ── PAIR commands ───────────────────────────────────────────

async fn handle_pair_status(coord: &Arc<Coordinator>) -> Response {
    let pairing = coord.pairing.read().await;
    if pairing.is_some() {
        Response::Ok("PAIRED".into())
    } else {
        Response::Ok("UNPAIRED".into())
    }
}

async fn handle_pair_start(coord: &Arc<Coordinator>) -> Response {
    if !coord.is_connected.load(Ordering::Relaxed) {
        return Response::Error("NOT_CONNECTED".into());
    }

    let (tx, rx) = tokio::sync::oneshot::channel();
    if coord
        .ble_cmd_tx
        .send(crate::coordinator::BleCommand::Pair { reply: tx })
        .await
        .is_err()
    {
        return Response::Error("BLE_CHANNEL_CLOSED".into());
    }

    match rx.await {
        Ok(Ok(_)) => Response::Ok("PAIRED".into()),
        Ok(Err(e)) => Response::Error(format!("PAIRING_FAILED:{}", e)),
        Err(_) => Response::Error("PAIR_REPLY_DROPPED".into()),
    }
}

async fn handle_pair_reset(coord: &Arc<Coordinator>) -> Response {
    // Send factory reset to device if connected and paired
    let is_paired = coord.pairing.read().await.is_some();
    if is_paired && coord.is_connected.load(Ordering::Relaxed) {
        // Compute reset HMAC
        let pairing = coord.pairing.read().await;
        if let Some(ref p) = *pairing {
            let hmac = immurok_common::security::compute_reset_hmac(&p.shared_key);
            drop(pairing);
            let _ = coord
                .ble_send(protocol::CMD_FACTORY_RESET, hmac.to_vec())
                .await;
        }
    }

    // Clear local pairing data
    let _ = immurok_common::security::clear_pairing();
    {
        let mut pairing = coord.pairing.write().await;
        *pairing = None;
    }
    info!("Pairing data cleared");
    Response::Ok("RESET".into())
}

// ── AGENT_APPROVE ────────────────────────────────────────────

/// Pre-execution approval for `imk run --agent -- <cmd>`. Surfaces the
/// command to the user (desktop notification + the imk-side terminal
/// prompt), waits for fingerprint, then arms a 10-second sudo pre-auth
/// window. Required because firmware's CMD_AUTH_REQUEST (used by sudo
/// PAM) unconditionally re-enters the FP gate even when FP_CAT_AUTH
/// cooldown is active — without this software window, every wrapped
/// sudo would prompt for a second touch right after AGENT_APPROVE.
/// 10s is tight enough that `sudo -k` outside the launch burst
/// restores fresh-fingerprint behavior. Mirrors macOS
/// PAMSocketServer's handleAgentApprove.
async fn handle_agent_approve(
    coord: &Arc<Coordinator>,
    cmd: &str,
    stream: &mut UnixStream,
) -> Response {
    info!("AGENT_APPROVE for command: {}", cmd);

    if cmd.is_empty() {
        return Response::Error("EMPTY_COMMAND".into());
    }
    if !coord.is_connected.load(Ordering::Relaxed) {
        warn!("AGENT_APPROVE rejected — device not connected");
        return Response::Error("NOT_CONNECTED".into());
    }
    if !coord.is_device_verified.load(Ordering::Relaxed) {
        warn!("AGENT_APPROVE rejected — device not verified");
        return Response::Error("NOT_VERIFIED".into());
    }

    // Reuse the same in-flight slot as PAM AUTH — only one fingerprint
    // gate at a time, and overlapping AGENT_APPROVE + sudo must not race.
    let (pending_tx, pending_rx) = tokio::sync::oneshot::channel::<bool>();
    if !coord.try_set_pending_pam(pending_tx).await {
        warn!("AGENT_APPROVE: another auth in flight");
        return Response::Error("BUSY".into());
    }

    // GTK4/Adwaita dialog with the wrapped command pill + countdown bar +
    // explicit Cancel button. Replaces the previous notify-send (passive,
    // no interactivity, can't surface countdown). 30s countdown matches the
    // device's FP gate window; the daemon's own 35s tokio sleep below is
    // just safety margin.
    let mut dialog_proc = spawn_agent_dialog(cmd, 30);

    // Race AUTH_REQUEST (drives the device's FP gate) against on_fp_match
    // resolving the pending channel directly (when the user touches before
    // AUTH_REQUEST's WAIT_FP cycle finishes), against PAM-side disconnect
    // (imk closing the socket = explicit cancel), against the dialog's
    // own Cancel button (process exits with code 1).
    let auth_fut = coord.ble_auth_request();
    let mut disconnect_buf = [0u8; 1];
    let result = tokio::select! {
        ble_result = auth_fut => match ble_result {
            Ok(true) => true,
            Ok(false) => {
                info!("AGENT_APPROVE denied (FP mismatch or BLE timeout)");
                false
            }
            Err(e) => {
                warn!("AGENT_APPROVE BLE error: {}", e);
                false
            }
        },
        pending_result = pending_rx => pending_result.unwrap_or(false),
        r = stream.read(&mut disconnect_buf) => {
            match r {
                Ok(0) | Err(_) => {
                    info!("AGENT_APPROVE cancelled: imk socket closed");
                    coord.auth_dialog_cancel.notify_one();
                    false
                }
                Ok(_) => {
                    info!("AGENT_APPROVE cancelled: unexpected data on imk socket");
                    coord.auth_dialog_cancel.notify_one();
                    false
                }
            }
        }
        Some(status) = wait_dialog(dialog_proc.as_mut()) => {
            // Dialog exited on its own — by Cancel button (exit 1), close
            // button (exit 1), or some crash. Either way, treat as REJECT.
            // notify_one() reaches the in-flight AuthRequest's select loop
            // (a regular ble_send would block on the BleCommand queue
            // until the auth completes — defeating the point of cancel).
            info!("AGENT_APPROVE cancelled: dialog exited with {:?}", status.code());
            coord.auth_dialog_cancel.notify_one();
            false
        }
        _ = tokio::time::sleep(Duration::from_secs(35)) => {
            info!("AGENT_APPROVE timeout (35s)");
            coord.auth_dialog_cancel.notify_one();
            false
        }
    };

    // Always close the dialog on the way out (graceful SIGTERM so its
    // signal handler quits cleanly without an exit-1 stale signal).
    kill_auth_dialog(dialog_proc);

    coord.deny_pending_pam().await;

    if result {
        // 10s sudo pre-auth bridge: covers the latency between
        // AGENT_APPROVE returning and the wrapped command's sudo
        // hitting PAM. Firmware's FP_CAT_AUTH cooldown does NOT
        // short-circuit IMMUROK_CMD_AUTH_REQUEST (hidkbd.c:4564
        // unconditionally fp_gate_enter()s — unlike KEY_SIGN which
        // checks cooldown at hidkbd.c:4920), so without this window
        // sudo PAM would always re-prompt for fingerprint right
        // after the user just touched for AGENT_APPROVE.
        // 10s is tight enough that `sudo -k` issued outside the
        // launch burst restores fresh-fingerprint behavior.
        coord.set_pre_auth(
            Duration::from_secs(10),
            &["sudo", "sudo_local", "sudo-i"],
        ).await;
        info!("AGENT_APPROVE approved: 10s sudo pre-auth armed");
        Response::Ok("APPROVED".into())
    } else {
        Response::Deny("REJECTED".into())
    }
}

// ── imk LIST / GET (key reads) ──────────────────────────────

/// `LIST:<cat>` — returns cached entries for ssh/otp/api in the format
/// macOS imk expects (commit 50d0709):
///   OK:N\n
///   <name>\tecdsa-sha2-nistp256 <base64-blob>\n      (ssh: tab-separated)
///   <name>\n                                         (otp/api)
///   ...
///   \n  (terminator blank line)
///
/// All reads come straight from the daemon's local cache files
/// (ssh_keys.json / key_names.json) — populated on connect via
/// digest-cached sync_ssh_keys (P1#4). No BLE round-trip.
async fn handle_list_keys(coord: &Arc<Coordinator>, cat: &str) -> String {
    use base64::Engine;
    let cat = cat.trim();
    if !matches!(cat, "ssh" | "otp" | "api") {
        return "ERROR:UNKNOWN_CATEGORY\n".to_string();
    }

    if cat == "ssh" {
        let entries = crate::keystore::load_ssh_keys(&coord.immurok_dir);
        let mut out = format!("OK:{}\n", entries.len());
        for e in &entries {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&e.public_key_blob);
            out.push_str(&format!("{}\tecdsa-sha2-nistp256 {}\n", e.name, b64));
        }
        out.push('\n');
        return out;
    }

    let names = crate::keystore::load_key_names(&coord.immurok_dir);
    let filtered: Vec<_> = names.iter().filter(|e| e.category == cat).collect();
    let mut out = format!("OK:{}\n", filtered.len());
    for e in &filtered {
        out.push_str(&format!("{}\n", e.name));
    }
    out.push('\n');
    out
}

/// `GET:<cat>:<name>` — return secret material for a key by name.
///   ssh → cached OpenSSH public key (no BLE, no FP gate)
///   api → FP-gated KEY_READ; entry layout `name[32]+key[128]`, secret at off=32
///   otp → FP-gated KEY_OTP_GET; device computes 6-digit TOTP server-side
///
/// All FP-gated paths reuse try_set_pending_pam BUSY semantics so they
/// can't race a concurrent PAM AUTH or AGENT_APPROVE.
async fn handle_get_key(
    coord: &Arc<Coordinator>,
    after_get: &str,
    stream: &mut UnixStream,
) -> String {
    let parts: Vec<&str> = after_get.splitn(2, ':').collect();
    if parts.len() != 2 || parts[1].is_empty() {
        return "ERROR:USAGE\n".to_string();
    }
    let cat = parts[0];
    let name = parts[1];

    if !coord.is_connected.load(Ordering::Relaxed) && cat != "ssh" {
        // SSH GET serves from cache, so it's fine if the device is offline.
        return "ERROR:NOT_CONNECTED\n".to_string();
    }

    match cat {
        "ssh" => handle_get_ssh(coord, name).await,
        "api" => handle_get_api(coord, name, stream).await,
        "otp" => handle_get_otp(coord, name, stream).await,
        _ => "ERROR:UNKNOWN_CATEGORY\n".to_string(),
    }
}

async fn handle_get_ssh(coord: &Arc<Coordinator>, name: &str) -> String {
    use base64::Engine;
    let entries = crate::keystore::load_ssh_keys(&coord.immurok_dir);
    let entry = match entries.iter().find(|e| e.name == name) {
        Some(e) => e,
        None => return format!("ERROR:NOT_FOUND:{}\n", name),
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(&entry.public_key_blob);
    // OpenSSH authorized_keys format: <algo> <base64-blob> <comment>
    format!("OK:ecdsa-sha2-nistp256 {} {}\n", b64, entry.name)
}

/// Look up cache index for a name in (otp/api). Returns None if not present.
fn find_key_index(coord_immurok_dir: &std::path::Path, cat: &str, name: &str) -> Option<u8> {
    let names = crate::keystore::load_key_names(coord_immurok_dir);
    names
        .iter()
        .find(|e| e.category == cat && e.name == name)
        .map(|e| e.index)
}

async fn handle_get_api(
    coord: &Arc<Coordinator>,
    name: &str,
    _stream: &mut UnixStream,
) -> String {
    let idx = match find_key_index(&coord.immurok_dir, "api", name) {
        Some(i) => i,
        None => return format!("ERROR:NOT_FOUND:{}\n", name),
    };

    // Reuse the PAM in-flight slot — only one fingerprint gate at a time.
    let (pending_tx, _pending_rx) = tokio::sync::oneshot::channel::<bool>();
    if !coord.try_set_pending_pam(pending_tx).await {
        return "ERROR:BUSY\n".to_string();
    }

    // KEY_READ on api with offset=0 reads the full 160-byte entry
    // (name[32] + key[128]) chunked. Firmware FP-gates KEY_READ for non-SSH
    // categories; the gated send hides the WAIT_FP cycle from us.
    let read_result = coord
        .ble_send_fp_gated(protocol::CMD_KEY_READ, vec![protocol::KEY_CAT_API, idx, 0])
        .await;
    coord.deny_pending_pam().await;

    match read_result {
        Ok((status, _)) if status == protocol::RSP_OK => {}
        Ok((status, _)) => return format!("ERROR:READ_FAILED:0x{:02x}\n", status),
        Err(e) => return format!("ERROR:READ_FAILED:{}\n", e),
    }

    // Whether the gate just approved (placeholder payload) or the cooldown
    // let the command through immediately (payload = first chunk frame),
    // neither carries the assembled entry — run the chunked read loop from
    // offset 0 now that the cooldown is armed. KEY_READ chunked response:
    // [total][off][data...].
    let mut full = Vec::new();
    let mut offset: u8 = 0;
    loop {
        let r = coord
            .ble_send(
                protocol::CMD_KEY_READ,
                vec![protocol::KEY_CAT_API, idx, offset],
            )
            .await;
        let (status, p) = match r {
            Ok(v) => v,
            Err(e) => return format!("ERROR:READ_FAILED:{}\n", e),
        };
        if status != protocol::RSP_OK || p.len() < 2 {
            return format!("ERROR:READ_FAILED:0x{:02x}\n", status);
        }
        let total = p[0] as usize;
        // p[1] = chunk offset echo; p[2..] = data
        let chunk = &p[2..];
        full.extend_from_slice(chunk);
        if full.len() >= total {
            full.truncate(total);
            break;
        }
        offset = full.len() as u8;
    }
    decode_api_secret(&full, name)
}

fn decode_api_secret(entry: &[u8], name: &str) -> String {
    // api_entry_t: name[32] + key[128] = 160 bytes; secret offset = 32.
    // (Was previously 16 — see macOS commit 5d78bed for that fix.)
    if entry.len() <= protocol::NAME_LEN_API {
        return format!("ERROR:INVALID_DATA:{}\n", name);
    }
    let secret_bytes = &entry[protocol::NAME_LEN_API..];
    let trimmed: Vec<u8> = secret_bytes.iter().copied().take_while(|&b| b != 0).collect();
    if trimmed.is_empty() {
        return format!("ERROR:EMPTY_SECRET:{}\n", name);
    }
    match std::str::from_utf8(&trimmed) {
        Ok(s) => format!("OK:{}\n", s),
        Err(_) => format!("ERROR:NON_UTF8:{}\n", name),
    }
}

async fn handle_get_otp(
    coord: &Arc<Coordinator>,
    name: &str,
    _stream: &mut UnixStream,
) -> String {
    let idx = match find_key_index(&coord.immurok_dir, "otp", name) {
        Some(i) => i,
        None => return format!("ERROR:NOT_FOUND:{}\n", name),
    };

    let (pending_tx, _pending_rx) = tokio::sync::oneshot::channel::<bool>();
    if !coord.try_set_pending_pam(pending_tx).await {
        return "ERROR:BUSY\n".to_string();
    }
    let result = otp_get_inner(coord, idx).await;
    coord.deny_pending_pam().await;
    result
}

/// Build the KEY_OTP_GET payload: [idx:1B][unix_time:4B LE]. The firmware
/// has no clock — TOTP time comes from the host on every request (the FP
/// gate adds its own elapsed-time correction on-device).
fn otp_get_payload(idx: u8) -> Vec<u8> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0);
    let mut p = vec![idx];
    p.extend_from_slice(&now.to_le_bytes());
    p
}

/// FP-gated TOTP fetch for one OTP slot. Shared by `GET:otp:<name>` and
/// `KEY:OTP:<idx>`.
async fn otp_get_inner(coord: &Arc<Coordinator>, idx: u8) -> String {
    let result = coord
        .ble_send_fp_gated(protocol::CMD_KEY_OTP_GET, otp_get_payload(idx))
        .await;

    match result {
        Ok((status, payload)) if status == protocol::RSP_OK => {
            // The fp_gated path may return just [RSP_OK] without the code —
            // re-issue the actual command after the cooldown is armed.
            let code_bytes = if payload.len() >= 6 {
                &payload[..]
            } else {
                let r = coord
                    .ble_send(protocol::CMD_KEY_OTP_GET, otp_get_payload(idx))
                    .await;
                let p = match r {
                    Ok((s, p)) if s == protocol::RSP_OK && p.len() >= 6 => p,
                    Ok((s, _)) => return format!("ERROR:OTP_FAILED:0x{:02x}\n", s),
                    Err(e) => return format!("ERROR:OTP_FAILED:{}\n", e),
                };
                return format_otp_response(&p);
            };
            format_otp_response(code_bytes)
        }
        Ok((status, _)) => format!("ERROR:OTP_FAILED:0x{:02x}\n", status),
        Err(e) => format!("ERROR:OTP_FAILED:{}\n", e),
    }
}

/// Format a TOTP response payload. The firmware always emits exactly six
/// ASCII digits — anything else means a framing bug upstream, and silently
/// dropping bytes would show the user a *wrong* code, so reject instead.
fn format_otp_response(code_bytes: &[u8]) -> String {
    if code_bytes.len() < 6 || !code_bytes[..6].iter().all(|b| b.is_ascii_digit()) {
        return format!("ERROR:OTP_INVALID:{}\n", hex::encode(code_bytes));
    }
    let s = String::from_utf8_lossy(&code_bytes[..6]).to_string();
    format!("OK:{}\n", s)
}

// ── SET / GET ───────────────────────────────────────────────

async fn handle_set_setting(coord: &Arc<Coordinator>, key: &str, value: bool) -> Response {
    {
        let mut settings = coord.settings.write().await;
        match key {
            "unlock_sudo" => settings.unlock_sudo = value,
            "unlock_polkit" => settings.unlock_polkit = value,
            "unlock_screen" => settings.unlock_screen = value,
            "lock_screen" => settings.lock_screen = value,
            _ => return Response::Error("UNKNOWN_KEY".into()),
        }
        if let Err(e) = settings.save(&coord.settings_path()) {
            warn!("Failed to save settings: {}", e);
            return Response::Error(format!("SAVE_FAILED:{}", e));
        }
    }
    info!("Setting {}={}", key, value);
    Response::Ok(String::new())
}

async fn handle_get_settings(coord: &Arc<Coordinator>) -> Response {
    let s = coord.settings.read().await;
    let msg = format!(
        "sudo={}:polkit={}:screen={}:lock={}",
        if s.unlock_sudo { "1" } else { "0" },
        if s.unlock_polkit { "1" } else { "0" },
        if s.unlock_screen { "1" } else { "0" },
        if s.lock_screen { "1" } else { "0" },
    );
    Response::Ok(msg)
}

async fn handle_get_info(coord: &Arc<Coordinator>) -> Response {
    let status = coord.device_status.read().await;
    let (fw, battery) = if let Some(ref s) = *status {
        (s.fw_version.clone(), s.battery)
    } else {
        ("-".into(), 0)
    };
    let connected = coord.is_connected.load(Ordering::Relaxed);
    let msg = format!(
        "fw={}:model=IK-1:connected={}:battery={}",
        fw,
        if connected { "1" } else { "0" },
        battery,
    );
    Response::Ok(msg)
}

// Map a write-path BLE status byte to a user-facing error string. Recognises
// firmware 1.3.1+ low-battery refusal (0xF4) so the CLI can show a clear
// "LOW_BATTERY" message instead of a raw hex code.
fn fmt_write_status(prefix: &str, status: u8) -> String {
    if status == protocol::RSP_ERR_LOW_BATTERY {
        return format!("ERROR:LOW_BATTERY:{} refused (device <5%, charge to retry)", prefix);
    }
    format!("ERROR:{}_FAILED:0x{:02x}", prefix, status)
}

// ── KEY commands ─────────────────────────────────────────────

async fn handle_key_command(line: &str, coord: &Arc<Coordinator>) -> String {
    let parts: Vec<&str> = line.splitn(4, ':').collect();
    if parts.len() < 2 {
        return "ERROR:INVALID_FORMAT".to_string();
    }

    if !coord.is_connected.load(Ordering::Relaxed) {
        return "ERROR:NOT_CONNECTED".to_string();
    }
    if !coord.is_device_verified.load(Ordering::Relaxed) {
        return "ERROR:NOT_VERIFIED".to_string();
    }

    match parts[1] {
        "GENERATE" if parts.len() >= 3 => {
            // KEY:GENERATE:<hex_name_16B>
            let name_hex = parts[2];
            let name_bytes = match hex::decode(name_hex) {
                Ok(b) if b.len() == 16 => b,
                _ => return "ERROR:INVALID_NAME".to_string(),
            };

            // KEY_GENERATE payload: [cat=SSH(0)][name:16B]
            let mut payload = vec![protocol::KEY_CAT_SSH];
            payload.extend_from_slice(&name_bytes);

            info!("Generating SSH keypair...");
            match coord.ble_send_fp_gated(protocol::CMD_KEY_GENERATE, payload).await {
                Ok((status, data)) if status == protocol::RSP_OK => {
                    let idx = if data.len() >= 3 { data[2] } else { 0 };
                    info!("SSH keypair generated at index {}", idx);
                    let _ = coord.sync_keys().await;
                    format!("OK:{}", idx)
                }
                Ok((status, _)) => fmt_write_status("GENERATE", status),
                Err(e) => format!("ERROR:{}", e),
            }
        }
        "DELETE" if parts.len() >= 4 => {
            // KEY:DELETE:<category>:<index>
            let cat = match parts[2] {
                "ssh" | "0" => protocol::KEY_CAT_SSH,
                "otp" | "1" => protocol::KEY_CAT_OTP,
                "api" | "2" => protocol::KEY_CAT_API,
                _ => return "ERROR:INVALID_CATEGORY".to_string(),
            };
            let idx: u8 = match parts[3].parse() {
                Ok(i) => i,
                Err(_) => return "ERROR:INVALID_INDEX".to_string(),
            };

            match coord.ble_send_fp_gated(protocol::CMD_KEY_DELETE, vec![cat, idx]).await {
                Ok((status, _)) if status == protocol::RSP_OK => {
                    let _ = coord.sync_keys().await;
                    "OK:DELETED".to_string()
                }
                Ok((status, _)) => fmt_write_status("KEY_DELETE", status),
                Err(e) => format!("ERROR:{}", e),
            }
        }
        "IMPORT" if parts.len() >= 3 => {
            // KEY:IMPORT:<hex_data_112B>
            // 112 bytes = name(16) + pubkey_LE(64) + privkey(32)
            let hex_data = parts[2];
            let key_data = match hex::decode(hex_data) {
                Ok(b) if b.len() == 112 => b,
                Ok(b) => {
                    return format!(
                        "ERROR:INVALID_DATA_LEN:expected 112 bytes, got {}",
                        b.len()
                    )
                }
                Err(e) => return format!("ERROR:INVALID_HEX:{}", e),
            };

            handle_key_import_inner(
                coord,
                protocol::KEY_CAT_SSH,
                &key_data,
                "SSH",
                protocol::KEY_MAX_SSH,
            )
            .await
        }
        // OTP single-entry import. Data mirrors otp_entry_t: name[30] +
        // service[30] + base32-decoded secret bytes (1..=32). A payload
        // missing the service field would land the secret in the wrong
        // struct slot, so the bounds are enforced here.
        "OTP_IMPORT" if parts.len() >= 3 => {
            let hex_data = parts[2];
            let min = protocol::NAME_LEN_OTP + protocol::SERVICE_LEN_OTP;
            let max = min + protocol::SECRET_LEN_OTP;
            let key_data = match hex::decode(hex_data) {
                Ok(b) if b.len() > min && b.len() <= max => b,
                Ok(b) => {
                    return format!(
                        "ERROR:INVALID_DATA_LEN:expected {}..={} bytes (name+service+secret), got {}",
                        min + 1,
                        max,
                        b.len()
                    )
                }
                Err(e) => return format!("ERROR:INVALID_HEX:{}", e),
            };
            handle_key_import_inner(
                coord,
                protocol::KEY_CAT_OTP,
                &key_data,
                "OTP",
                protocol::KEY_MAX_OTP,
            )
            .await
        }
        // FP-gated TOTP fetch by slot index (CLI `key otp <idx>`).
        "OTP" if parts.len() >= 3 => {
            let idx: u8 = match parts[2].parse() {
                Ok(i) => i,
                Err(_) => return "ERROR:INVALID_INDEX".to_string(),
            };
            let (pending_tx, _pending_rx) = tokio::sync::oneshot::channel::<bool>();
            if !coord.try_set_pending_pam(pending_tx).await {
                return "ERROR:BUSY".to_string();
            }
            let resp = otp_get_inner(coord, idx).await;
            coord.deny_pending_pam().await;
            resp.trim_end().to_string()
        }
        // API single-entry import. Data mirrors api_entry_t: name[32] +
        // value bytes (1..=128). Same staged write + FP-gated commit path.
        "API_IMPORT" if parts.len() >= 3 => {
            let hex_data = parts[2];
            let min = protocol::NAME_LEN_API;
            let max = min + protocol::VALUE_LEN_API;
            let key_data = match hex::decode(hex_data) {
                Ok(b) if b.len() > min && b.len() <= max => b,
                Ok(b) => {
                    return format!(
                        "ERROR:INVALID_DATA_LEN:expected {}..={} bytes (name+value), got {}",
                        min + 1,
                        max,
                        b.len()
                    )
                }
                Err(e) => return format!("ERROR:INVALID_HEX:{}", e),
            };
            handle_key_import_inner(
                coord,
                protocol::KEY_CAT_API,
                &key_data,
                "API",
                protocol::KEY_MAX_API,
            )
            .await
        }
        _ => "ERROR:UNKNOWN_KEY_CMD".to_string(),
    }
}

/// Import a key to the device: stage in chunks then commit. Append semantic.
///
/// Firmware (immurok_keystore.c:408-450) treats `idx == 0xFF` as "append new
/// entry at slot count++" and any other idx as "update existing slot, must
/// satisfy idx < count". Our previous code passed idx = count, which made
/// commit fail with `idx >= count` → SEC_ERR_INTERNAL (0xff) on every fresh
/// import. KEY_WRITE staging works fine with 0xFF too — the firmware's
/// stage_t resets when (cat, idx) changes, so 0xFF is a stable "fresh
/// staging" key for both write and commit.
async fn handle_key_import_inner(
    coord: &Arc<Coordinator>,
    cat: u8,
    key_data: &[u8],
    label: &str,
    max_count: u8,
) -> String {
    // Pre-flight KEY_COUNT only to refuse on full keystore. ble_send strips
    // the status byte, so the count sits at payload[0]; the 4-byte checksum
    // tail (firmware 1.2.7+) follows but we don't need it for import.
    let count = match coord
        .ble_send(protocol::CMD_KEY_COUNT, vec![cat])
        .await
    {
        Ok((status, payload)) if status == protocol::RSP_OK && !payload.is_empty() => {
            payload[0]
        }
        Ok((status, payload)) => {
            return format!("ERROR:KEY_COUNT_FAILED:0x{:02x}:len={}", status, payload.len());
        }
        Err(e) => {
            return format!("ERROR:KEY_COUNT_FAILED:{}", e);
        }
    };
    if count >= max_count {
        return format!(
            "ERROR:KEYSTORE_FULL:{} at capacity ({}/{})",
            label, count, max_count
        );
    }

    // Append-mode sentinel — see firmware doc above.
    let idx: u8 = 0xFF;

    info!("Importing {} key (append mode, will land at slot {})", label, count);

    // 2. Write in chunks via KEY_WRITE (max 59 bytes data per write)
    // KEY_WRITE payload: [cat:1B][idx:1B][off:1B][data...]
    // Max BLE payload = 62 bytes, so data portion = 62 - 3 = 59 bytes
    let max_chunk = 59;
    let mut offset: usize = 0;

    while offset < key_data.len() {
        let end = (offset + max_chunk).min(key_data.len());
        let chunk = &key_data[offset..end];

        let mut payload = vec![cat, idx, offset as u8];
        payload.extend_from_slice(chunk);

        match coord
            .ble_send(protocol::CMD_KEY_WRITE, payload)
            .await
        {
            Ok((status, _)) if status == protocol::RSP_OK => {}
            Ok((status, _)) => {
                if status == protocol::RSP_ERR_LOW_BATTERY {
                    return "ERROR:LOW_BATTERY:KEY_WRITE refused (device <5%, charge to retry)"
                        .to_string();
                }
                return format!("ERROR:KEY_WRITE_FAILED:offset={}:0x{:02x}", offset, status);
            }
            Err(e) => {
                return format!("ERROR:KEY_WRITE_FAILED:{}", e);
            }
        }

        offset = end;
    }

    // 3. Commit via KEY_COMMIT: [cat:1B][idx:1B]
    match coord
        .ble_send_fp_gated(protocol::CMD_KEY_COMMIT, vec![cat, idx])
        .await
    {
        Ok((status, _))
            if status == protocol::RSP_OK || status == protocol::RSP_FP_GATE_APPROVED =>
        {
            info!("{} key imported (appended at slot {})", label, count);
            let _ = coord.sync_keys().await;
            format!("OK:{}", count)
        }
        Ok((status, _)) => fmt_write_status("KEY_COMMIT", status),
        Err(e) => {
            format!("ERROR:KEY_COMMIT_FAILED:{}", e)
        }
    }
}

