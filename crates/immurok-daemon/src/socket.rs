//! PAM socket server — handles PAM authentication and CLI management requests.
//!
//! Listens on `<runtime_dir>/pam.sock` (chmod 0o666 so PAM-as-root can connect;
//! the directory itself is what keeps other users from rebinding it).
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

use immurok_common::dual_host::{self, ClearPath, SlotClearAck, SlotStatus};
use immurok_common::protocol;
use immurok_common::socket_proto::{parse_request, serialize_response, Request, Response};
use immurok_common::types::PairProgress;

use crate::coordinator::Coordinator;
use crate::ota;

/// Main socket server loop.
pub async fn serve(coordinator: Arc<Coordinator>, socket_path: &Path) {
    // Remove stale socket
    let _ = std::fs::remove_file(socket_path);

    let listener = match UnixListener::bind(socket_path) {
        Ok(l) => l,
        Err(e) => {
            // Answering PAM is the whole job — a daemon that cannot bind is
            // useless, and returning here would end the select! in main() with
            // a *successful* exit, which Restart=on-failure ignores. Exit
            // non-zero so systemd actually retries (and so a stale root-owned
            // socket file surfaces as a restart loop, not silence).
            tracing::error!(
                "Failed to bind PAM socket at {}: {} — exiting",
                socket_path.display(),
                e
            );
            std::process::exit(1);
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

/// Walk the parent chain of `start_pid` (max 12 levels) looking for a process
/// that registered itself via `AGENT_APPROVE`. Returns `Some(command)` on the
/// first match — the highest-confidence signal that this PAM AUTH was
/// triggered by an AI agent wrap. `None` means manual user action (terminal
/// sudo) or an agent we cannot recognise. Mirrors macOS
/// AuthCallerClassifier.classify (Sources/AuthCallerClassifier.swift), but
/// Linux-flavored (/proc instead of libproc).
///
/// Log enrichment only — it must never gate anything.
async fn classify_agent_claim(coord: &Arc<Coordinator>, start_pid: u32) -> Option<String> {
    let mut pid = start_pid;
    for _ in 0..12 {
        if let Some(cmd) = coord.agent_claim(pid).await {
            return Some(cmd);
        }
        match read_ppid(pid) {
            Some(p) if p > 1 => pid = p,
            _ => break, // hit init or unreadable
        }
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

/// Peer uid from SO_PEERCRED.
fn peer_uid_of(stream: &UnixStream) -> Option<u32> {
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
        Some(cred.uid)
    } else {
        None
    }
}

fn polkitd_uid() -> Option<u32> {
    unsafe {
        let pw = libc::getpwnam(c"polkitd".as_ptr());
        if pw.is_null() {
            None
        } else {
            Some((*pw).pw_uid)
        }
    }
}

/// What a request requires of its caller.
///
/// The socket is machine-wide now (0666 in a root-owned directory), so "who
/// may connect" is no longer the same question as "who may ask for this".
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Tier {
    /// PAM authentication. Only root and polkitd run pam_immurok.
    Auth,
    /// Harmless read-only status any local process may ask for.
    Public,
    /// Everything that touches the device or the stored secrets.
    Manage,
}

fn tier_of(line: &str) -> Tier {
    if line.starts_with("AUTH:") {
        return Tier::Auth;
    }
    match line {
        "STATUS" | "GET:INFO" | "GET:SETTINGS" | "PAIR:STATUS" => Tier::Public,
        _ => Tier::Manage,
    }
}

/// Requests the CLI and TUI issue on a timer. They change nothing and decide
/// nothing, so they are the ones that must not crowd the log.
fn is_routine_read(line: &str) -> bool {
    matches!(line, "STATUS" | "PAIR:STATUS" | "GET:INFO" | "GET:SETTINGS")
        || line.starts_with("KEY:CACHE:")
}

/// Decide whether `peer_uid` may issue a request of this tier.
///
/// Management is for whoever is logged in at this machine right now — the
/// device is paired per machine, and the person sitting at it is the one it
/// belongs to. When logind cannot answer we fall back to the recorded owner
/// uid rather than denying, so a broken/absent logind does not brick the CLI.
async fn authorize(tier: Tier, peer_uid: u32) -> Result<(), String> {
    if peer_uid == 0 {
        return Ok(());
    }
    match tier {
        Tier::Public => Ok(()),
        Tier::Auth => {
            if polkitd_uid() == Some(peer_uid) {
                Ok(())
            } else {
                Err(format!("uid {} may not request AUTH (root/polkitd only)", peer_uid))
            }
        }
        Tier::Manage => match crate::session::uid_is_active(peer_uid).await {
            Some(true) => Ok(()),
            Some(false) => Err(format!("uid {} has no active session on this machine", peer_uid)),
            None => match crate::session::owner_uid() {
                Some(owner) if owner == peer_uid => Ok(()),
                _ => Err(format!(
                    "uid {} is not the recorded owner (and logind is unavailable)",
                    peer_uid
                )),
            },
        },
    }
}

/// Handle a single client connection.
async fn handle_client(
    mut stream: UnixStream,
    coord: Arc<Coordinator>,
) -> Result<(), String> {
    let peer_uid = match peer_uid_of(&stream) {
        Some(uid) => uid,
        None => {
            warn!("Rejecting client: SO_PEERCRED unavailable");
            return Err("no peer credentials".into());
        }
    };

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
    // Routine reads go to debug. The TUI polls six of them every two seconds,
    // which at info fills the 500-line SUBSCRIBE:LOG ring buffer in about three
    // minutes — so with the TUI open, `immurok-cli logs` shows nothing but
    // "Socket request: STATUS" exactly when a BLE problem needs diagnosing.
    // Anything that changes state or asks for an authorization stays at info.
    if is_routine_read(line) {
        debug!("Socket request: {}", line);
    } else {
        info!("Socket request: {}", line);
    }

    // Authorize per command, not per connection: the socket is machine-wide,
    // so a plain STATUS and a KEY:WRITE arriving on it are very different
    // asks. Answer with a DENY rather than dropping the connection — a bare
    // reset shows up in the CLI as "Connection reset by peer", which tells
    // the user nothing.
    let tier = tier_of(line);
    if let Err(e) = authorize(tier, peer_uid).await {
        warn!("Denied {:?} request from uid {}: {}", tier, peer_uid, e);
        let resp = serialize_response(&Response::Deny("NOT_AUTHORIZED".into()));
        let _ = stream.write_all(format!("{}\n", resp).as_bytes()).await;
        return Ok(());
    }

    // The user-session agent holds this connection open for the life of the
    // session and renders whatever we push at it.
    if line == "SUBSCRIBE:SESSION" {
        return serve_session_agent(stream, coord, peer_uid).await;
    }

    // Log viewer (immurok-cli logs / the TUI panel). They used to `tail -F`
    // the file; it now lives in a directory they cannot enter.
    if line == "SUBSCRIBE:LOG" {
        return serve_log_stream(stream).await;
    }

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

/// Hold a session agent's subscription open: push UI events out, take its
/// cancels in.
///
/// Only one agent at a time — a second subscribe (reconnect after the daemon
/// restarted, or a second graphical login) replaces the first, and the
/// displaced connection deregisters only if it is still the registered one.
async fn serve_session_agent(
    mut stream: UnixStream,
    coord: Arc<Coordinator>,
    peer_uid: u32,
) -> Result<(), String> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mine = tx.clone();
    *coord.ui_out.write().await = Some(tx);
    info!("Session agent subscribed (uid {})", peer_uid);

    // Re-send the ssh_takeover intent on every subscribe: the agent may have
    // been down when the setting changed, and ~/.ssh/config must not drift.
    push_ssh_takeover(&coord).await;

    let mut buf = vec![0u8; 512];
    loop {
        tokio::select! {
            outbound = rx.recv() => match outbound {
                Some(l) => {
                    if stream.write_all(format!("{}\n", l).as_bytes()).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            read = stream.read(&mut buf) => match read {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    for l in String::from_utf8_lossy(&buf[..n]).lines() {
                        match l.trim() {
                            "UI:CANCEL" => {
                                info!("Session agent: user cancelled");
                                let _ = coord.ui_cancel_tx.send(());
                            }
                            "" => {}
                            other => debug!("Session agent sent unknown line: {}", other),
                        }
                    }
                }
            },
        }
    }

    let mut guard = coord.ui_out.write().await;
    if guard.as_ref().is_some_and(|t| t.same_channel(&mine)) {
        *guard = None;
        info!("Session agent disconnected");
    }
    Ok(())
}

/// Stream the daemon's log: the buffered tail first, then live lines.
async fn serve_log_stream(mut stream: UnixStream) -> Result<(), String> {
    let Some(sink) = crate::logbuf::global() else {
        let _ = stream.write_all(b"ERROR:NO_LOG_SINK\n").await;
        return Ok(());
    };

    // Subscribe before snapshotting: a line landing between the two shows up
    // twice, which is cosmetic, whereas the other order loses it.
    let mut rx = sink.subscribe();
    for line in sink.snapshot() {
        if stream.write_all(format!("{}\n", line).as_bytes()).await.is_err() {
            return Ok(());
        }
    }

    let mut probe = [0u8; 1];
    loop {
        tokio::select! {
            line = rx.recv() => match line {
                Ok(l) => {
                    if stream.write_all(format!("{}\n", l).as_bytes()).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    let note = format!("… {} log lines dropped (viewer too slow)\n", n);
                    if stream.write_all(note.as_bytes()).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            },
            // Viewer went away.
            r = stream.read(&mut probe) => match r {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            },
        }
    }
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
        Request::PairStart => {
            let resp = handle_pair_start(coord).await;
            // Whoever paired the device owns it. Recorded here (and by
            // `immurok-pam-helper migrate-daemon` for installs that predate
            // privilege separation) so handle_auth can refuse AUTH requests
            // aimed at other local accounts.
            if matches!(resp, Response::Ok(_)) {
                if let Some(uid) = peer_uid_of(stream) {
                    crate::session::record_owner(uid);
                }
            }
            resp
        }
        Request::PairProgress => handle_pair_progress(coord).await,
        Request::SlotStatus => handle_slot_status(coord).await,
        Request::SlotClear { slot } => handle_slot_clear(coord, slot).await,
        Request::FactoryReset => handle_factory_reset(coord).await,
        Request::SetUnlockSudo(v) => handle_set_setting(coord, "unlock_sudo", v).await,
        Request::SetUnlockPolkit(v) => handle_set_setting(coord, "unlock_polkit", v).await,
        Request::SetUnlockScreen(v) => handle_set_setting(coord, "unlock_screen", v).await,
        Request::SetLockScreen(v) => handle_set_setting(coord, "lock_screen", v).await,
        Request::SetSshTakeover(v) => handle_set_setting(coord, "ssh_takeover", v).await,
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
        device_unpaired: coord.device_reports_unpaired.load(Ordering::Relaxed),
        link_unbonded: coord.link_unbonded.load(Ordering::Relaxed),
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
    let agent_context = match peer_pid_of(stream) {
        Some(pid) => classify_agent_claim(coord, pid).await,
        None => None,
    };
    if let Some(ref cmd) = agent_context {
        info!(
            "AUTH request: user={}, service={} (agent context: {})",
            user, service, cmd
        );
    } else {
        info!("AUTH request: user={}, service={}", user, service);
    }

    // 0. The socket serves the whole machine now, so "who is this AUTH for?"
    //    became a real question: without this, a second local account running
    //    sudo would be authorized by the owner's touch. Only enforced once an
    //    owner is on record (migration and pairing both write it); an
    //    unpaired/never-migrated install falls through with a warning rather
    //    than locking the machine's only user out.
    match crate::session::owner_uid() {
        Some(owner) => {
            if crate::session::uid_of_user(user) != Some(owner) {
                warn!(
                    "AUTH denied: user {} is not the device owner (uid {})",
                    user, owner
                );
                return Response::Deny("NOT_OWNER".into());
            }
        }
        None => {
            warn!("AUTH: no owner recorded — skipping owner check");
        }
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
    // The prompt lives in the user's session now. Subscribe to cancels before
    // asking for the dialog so a fast click cannot slip through the gap.
    let mut ui_cancel = coord.ui_cancel_tx.subscribe();
    let showing_ui = is_graphical && coord.push_ui("UI:DIALOG:AUTH").await;

    // Set up pending_pam so on_fp_match() can approve us via 0x21.
    // Refuse if another AUTH is already in flight — overwriting would
    // route the next FP match to the wrong PAM request and hang the
    // first one. PAM module sees BUSY and decides whether to retry.
    let (pending_tx, pending_rx) = tokio::sync::oneshot::channel::<bool>();
    if !coord.try_set_pending_pam(pending_tx).await {
        warn!("AUTH busy: another auth in flight (user={} service={})", user, service);
        if showing_ui {
            coord.push_ui("UI:DISMISS").await;
        }
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
        // Path C: the user dismissed the dialog in their session.
        Ok(_) = ui_cancel.recv(), if showing_ui => {
            info!("AUTH cancelled: session agent reported cancel");
            coord.auth_dialog_cancel.notify_one();
            AuthResult::Denied
        }
        // Path D: PAM socket closed (user cancelled, e.g. Ctrl+C in sudo)
        r = stream.read(&mut disconnect_buf) => {
            // Bail the in-flight AuthRequest via the Notify — the single BLE
            // worker is parked in do_auth_request waiting for the touch, so a
            // GATE_CANCEL queued through ble_send() would sit behind it and
            // never reach the device (the LED would keep blinking until the
            // AUTH timeout). auth_dialog_cancel wakes the parked wait, which
            // writes GATE_CANCEL straight to the device to stop the LED now.
            match r {
                Ok(0) | Err(_) => {
                    info!("AUTH cancelled: PAM socket closed");
                    coord.auth_dialog_cancel.notify_one();
                    AuthResult::Denied
                }
                Ok(_) => {
                    // Unexpected data — treat as cancel
                    info!("AUTH cancelled: unexpected data on PAM socket");
                    coord.auth_dialog_cancel.notify_one();
                    AuthResult::Denied
                }
            }
        }
    };

    if showing_ui {
        coord.push_ui("UI:DISMISS").await;
    }
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
    // Slots 0-4 authenticate; slot 5 is the dedicated host-switch finger.
    // Both are enrollable — MAX_FINGERPRINT_SLOTS alone would reject slot 5.
    if slot > protocol::SWITCH_FINGER_SLOT {
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

    // Atomic check-and-set: a second PAIR:START while one is genuinely in
    // flight (WaitFp/WaitButton/Ecdh) must not clobber the first attempt's
    // progress with a spurious Idle.
    if !coord.try_begin_pairing().await {
        return Response::Error("PAIRING_IN_PROGRESS".into());
    }

    let (tx, rx) = tokio::sync::oneshot::channel();
    if coord
        .ble_cmd_tx
        .send(crate::coordinator::BleCommand::Pair { reply: tx })
        .await
        .is_err()
    {
        coord.set_pair_progress(PairProgress::Failed).await;
        return Response::Error("BLE_CHANNEL_CLOSED".into());
    }

    match rx.await {
        Ok(Ok(_)) => Response::Ok("PAIRED".into()),
        Ok(Err(e)) => Response::Error(format!("PAIRING_FAILED:{}", e)),
        Err(_) => {
            coord.set_pair_progress(PairProgress::Failed).await;
            Response::Error("PAIR_REPLY_DROPPED".into())
        }
    }
}

async fn handle_pair_progress(coord: &Arc<Coordinator>) -> Response {
    Response::Ok(coord.pair_progress().await.as_wire().to_string())
}

/// Drop this host's pairing material. Idempotent.
async fn clear_local_pairing(coord: &Arc<Coordinator>) {
    let _ = immurok_common::security::clear_pairing();
    *coord.pairing.write().await = None;
    // Both verification flags derive from the shared key we just dropped;
    // leaving them set would keep a stale "device verified" badge and, worse,
    // keep claiming one of the device's slots is ours.
    coord.is_device_verified.store(false, Ordering::Relaxed);
    coord.challenge_verified.store(false, Ordering::Relaxed);
    info!("Local pairing data cleared");
}

/// Read slot occupancy off the device.
///
/// `ble_send` hands back the response frame already split into
/// `(frame[0], frame[1..])`; the decoder works on the whole wire frame, so
/// stitch it back together rather than duplicating the format knowledge here.
async fn query_slot_status(coord: &Arc<Coordinator>) -> Result<SlotStatus, String> {
    let (first, rest) = coord.ble_send(protocol::CMD_SLOT_STATUS, vec![]).await?;
    let mut frame = Vec::with_capacity(1 + rest.len());
    frame.push(first);
    frame.extend_from_slice(&rest);
    Ok(dual_host::parse_slot_status(&frame))
}

async fn handle_slot_status(coord: &Arc<Coordinator>) -> Response {
    if !coord.is_connected.load(Ordering::Relaxed) {
        return Response::Error("NOT_CONNECTED".into());
    }
    match query_slot_status(coord).await {
        Ok(SlotStatus::Supported { bitmap, active }) => {
            // Fourth field: which slot belongs to THIS host, or 0 when that
            // cannot be proven. The shared key is per-slot, so a genuine
            // challenge-response verify means the slot the device is
            // presenting is ours. Anything weaker — the device sitting on an
            // empty slot, or the optimistic `is_device_verified` fallbacks —
            // is not proof, and a UI that guesses here mislabels the user's
            // own pairing as somebody else's.
            let occupied = bitmap & (1 << (active - 1)) != 0;
            let mine = if occupied
                && coord.challenge_verified.load(Ordering::Relaxed)
                && coord.pairing.read().await.is_some()
            {
                active
            } else {
                0
            };
            Response::Ok(format!("{}:{}:{}", bitmap, active, mine))
        }
        Ok(SlotStatus::Unsupported) => Response::Ok("UNSUPPORTED".into()),
        Err(e) => Response::Error(format!("SLOT_STATUS_FAILED:{}", e)),
    }
}

/// Clear this host's own slot.
///
/// The device answers `[0x3C][status]` and then reboots ~50 ms later on
/// success, so the answer frequently dies with the link — and by then the
/// slot on the device is already gone. Local pairing data is therefore
/// dropped on confirmed clear, on a lost/garbled answer, and on a link
/// error: re-pairing once too often costs far less than the split state
/// where this host still believes it is paired while the device slot is
/// empty (the user then sees "unpair does nothing").
///
/// The one case that must NOT drop local pairing is an explicit refusal
/// (`[0x3C][non-zero other than 0xF2]`): the device did not reboot, the slot
/// is still occupied, and the link is still up — dropping local state here
/// would create the reverse split (a zombie slot the host has forgotten
/// about, occupying one of only two).
///
/// `0xF2` (SEC_ERR_NOT_PAIRED) is deliberately NOT in that exception. It
/// means the slot the device is presenting is empty, so the firmware's
/// pre-pair whitelist refuses everything — including this command. Treating
/// it as a refusal is what deadlocked a user in the field: the device had
/// switched to its other (empty) slot, so `unpair` could never clear local
/// state, and `pair` bails out whenever local state exists. Nothing on the
/// device changes in this case, so dropping the now-unusable local record is
/// both safe and the only way out.
async fn clear_own_slot(coord: &Arc<Coordinator>, target: Option<u8>) -> Response {
    let payload = target.map(|s| vec![s]).unwrap_or_default();
    match coord.ble_send(protocol::CMD_SLOT_CLEAR, payload).await {
        Ok((first, rest)) => match dual_host::classify_slot_clear_ack(first, &rest) {
            SlotClearAck::Cleared => {
                clear_local_pairing(coord).await;
                Response::Ok("CLEARED".into())
            }
            SlotClearAck::SlotUnpaired => {
                warn!(
                    "SLOT_CLEAR (own): device is sitting on an unpaired slot \
                     (0xF2) — clearing local pairing only"
                );
                clear_local_pairing(coord).await;
                Response::Ok("CLEARED_SLOT_UNPAIRED".into())
            }
            SlotClearAck::Refused => {
                warn!("SLOT_CLEAR (own): device refused 0x{:02x} {:?}", first, rest);
                Response::Error("SLOT_CLEAR_REFUSED".into())
            }
            SlotClearAck::Unrecognised => {
                warn!("SLOT_CLEAR (own): unexpected response 0x{:02x} {:?}", first, rest);
                clear_local_pairing(coord).await;
                Response::Ok("CLEARED_UNCONFIRMED".into())
            }
        },
        Err(e) => {
            // Expected on the happy path: the reboot takes the answer with it.
            info!("SLOT_CLEAR (own): no answer ({}) — device most likely rebooted after clearing", e);
            clear_local_pairing(coord).await;
            Response::Ok("CLEARED_UNCONFIRMED".into())
        }
    }
}

/// Dual-host slot clear. `slot == None` means "the slot this host uses".
///
/// The two paths differ on the wire and cannot share a send call: own is
/// ungated and reboots the device, other runs a fingerprint gate and keeps
/// the link. So the path is resolved from SLOT_STATUS *before* sending.
async fn handle_slot_clear(coord: &Arc<Coordinator>, slot: Option<u8>) -> Response {
    if !coord.is_connected.load(Ordering::Relaxed) {
        // Clearing a far slot needs a fingerprint on the device, so it
        // cannot happen offline. Clearing our own can: drop local state and
        // say plainly that the device side is untouched.
        return match slot {
            None => {
                clear_local_pairing(coord).await;
                Response::Ok("CLEARED_LOCAL_ONLY".into())
            }
            Some(_) => Response::Error("NOT_CONNECTED".into()),
        };
    }

    // Only query the device when a target was named — a bare clear is
    // always the own path and must keep working on firmware without 0x39.
    let status = if slot.is_some() {
        match query_slot_status(coord).await {
            Ok(s) => s,
            Err(e) => return Response::Error(format!("SLOT_STATUS_FAILED:{}", e)),
        }
    } else {
        SlotStatus::Unsupported
    };

    match dual_host::resolve_clear_path(slot, status) {
        ClearPath::Own => clear_own_slot(coord, slot).await,
        ClearPath::Other(target) => {
            match coord
                .ble_send_fp_gated(protocol::CMD_SLOT_CLEAR, vec![target])
                .await
            {
                Ok(_) => {
                    info!("Slot {} cleared (other host)", target);
                    Response::Ok("CLEARED".into())
                }
                Err(e) => Response::Error(format!("SLOT_CLEAR_FAILED:{}", e)),
            }
        }
        ClearPath::InvalidSlot => Response::Error("INVALID_SLOT".into()),
        ClearPath::Unsupported => Response::Error("DUAL_HOST_UNSUPPORTED".into()),
    }
}

/// Wipe the device completely: every fingerprint (including the host-switch
/// finger), every SSH/OTP/API key, and both host slots.
///
/// This used to be what `unpair` did. Under dual-host that destroys the
/// *other* host's slot and all SSH private keys — which exist nowhere else —
/// so it now lives behind its own command with its own confirmation.
///
/// Division of labour: `SLOT:CLEAR` (unpair) is for detaching from a device
/// you no longer possess — it degrades gracefully offline by dropping local
/// state alone. `FACTORY_RESET` is for wiping a device you ARE holding, so
/// it requires a live connection and only drops local pairing once the
/// device has confirmed the wipe. Clearing it on failure (or when merely
/// offline) would strand the host with no pairing, a device that still has
/// its old key/slots, and no way back except the destructive 3-second
/// long-press — exactly the trap this task exists to close.
async fn handle_factory_reset(coord: &Arc<Coordinator>) -> Response {
    if !coord.is_connected.load(Ordering::Relaxed) {
        return Response::Error("NOT_CONNECTED".into());
    }

    // Firmware never reads the payload or verifies an HMAC on
    // CMD_FACTORY_RESET, so this is best-effort backward-compat, not a
    // precondition: send the HMAC when we have local pairing to derive one
    // from, otherwise an empty payload — honest about having no key rather
    // than faking zeros.
    let hmac = {
        let pairing = coord.pairing.read().await;
        pairing
            .as_ref()
            .map(|p| immurok_common::security::compute_reset_hmac(&p.shared_key))
    };
    let payload = hmac.map(|h| h.to_vec()).unwrap_or_default();

    // Fingerprint-gated: with no prints enrolled the firmware answers
    // RSP_OK immediately; with prints enrolled it answers RSP_WAIT_FP
    // (0x11) and only wipes after a touch. Using plain ble_send here would
    // report success without ever waiting for that touch.
    match coord
        .ble_send_fp_gated(protocol::CMD_FACTORY_RESET, payload)
        .await
    {
        Ok((status, _))
            if status == protocol::RSP_OK || status == protocol::RSP_FP_GATE_APPROVED =>
        {
            clear_local_pairing(coord).await;
            Response::Ok("RESET".into())
        }
        Ok((status, _)) => {
            warn!("FACTORY_RESET: device refused 0x{:02x}", status);
            Response::Error(format!("FACTORY_RESET_FAILED:0x{:02x}", status))
        }
        Err(e) => Response::Error(format!("FACTORY_RESET_FAILED:{}", e)),
    }
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

    // Register the caller so a later raw AUTH from one of its children can be
    // labelled as agent-driven in the log (see classify_agent_claim). The pid
    // comes from SO_PEERCRED, so it is the kernel's word, not the caller's.
    if let Some(pid) = peer_pid_of(stream) {
        coord.record_agent_claim(pid, cmd).await;
    }

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

    // The dialog (wrapped command pill + countdown + Cancel) is rendered by
    // the session agent; 30s matches the device's FP gate window, and the
    // daemon's own 35s sleep below is safety margin. No agent subscribed
    // means no window — the touch is still the gate.
    let mut ui_cancel = coord.ui_cancel_tx.subscribe();
    let showing_ui = coord.push_ui(format!("UI:DIALOG:AGENT:30:{}", cmd)).await;

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
        Ok(_) = ui_cancel.recv(), if showing_ui => {
            // The user hit Cancel (or closed the window) in their session.
            // notify_one() reaches the in-flight AuthRequest's select loop —
            // a regular ble_send would queue behind the parked auth and never
            // reach the device, defeating the point of cancelling.
            info!("AGENT_APPROVE cancelled by user");
            coord.auth_dialog_cancel.notify_one();
            false
        }
        _ = tokio::time::sleep(Duration::from_secs(35)) => {
            info!("AGENT_APPROVE timeout (35s)");
            coord.auth_dialog_cancel.notify_one();
            false
        }
    };

    // Always take the dialog down on the way out.
    if showing_ui {
        coord.push_ui("UI:DISMISS").await;
    }

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
/// `KEY:CACHE:<ssh|names>` → `OK:<json array>` on one line.
///
/// The payload is exactly what the on-disk cache holds, so callers parse it
/// the same way they used to parse the file.
fn key_cache_json(coord: &Arc<Coordinator>, kind: &str) -> String {
    let json = match kind.trim() {
        "ssh" => serde_json::to_string(&crate::keystore::load_ssh_keys(&coord.state_dir)),
        "names" => serde_json::to_string(&crate::keystore::load_key_names(&coord.state_dir)),
        _ => return "ERROR:UNKNOWN_CACHE".to_string(),
    };
    match json {
        Ok(j) => format!("OK:{}", j),
        Err(e) => format!("ERROR:SERIALIZE:{}", e),
    }
}

async fn handle_list_keys(coord: &Arc<Coordinator>, cat: &str) -> String {
    use base64::Engine;
    let cat = cat.trim();
    if !matches!(cat, "ssh" | "otp" | "api") {
        return "ERROR:UNKNOWN_CATEGORY\n".to_string();
    }

    if cat == "ssh" {
        let entries = crate::keystore::load_ssh_keys(&coord.state_dir);
        let mut out = format!("OK:{}\n", entries.len());
        for e in &entries {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&e.public_key_blob);
            out.push_str(&format!("{}\tecdsa-sha2-nistp256 {}\n", e.name, b64));
        }
        out.push('\n');
        return out;
    }

    let names = crate::keystore::load_key_names(&coord.state_dir);
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
    let entries = crate::keystore::load_ssh_keys(&coord.state_dir);
    let entry = match entries.iter().find(|e| e.name == name) {
        Some(e) => e,
        None => return format!("ERROR:NOT_FOUND:{}\n", name),
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(&entry.public_key_blob);
    // OpenSSH authorized_keys format: <algo> <base64-blob> <comment>
    format!("OK:ecdsa-sha2-nistp256 {} {}\n", b64, entry.name)
}

/// Look up cache index for a name in (otp/api). Returns None if not present.
fn find_key_index(coord_state_dir: &std::path::Path, cat: &str, name: &str) -> Option<u8> {
    let names = crate::keystore::load_key_names(coord_state_dir);
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
    let idx = match find_key_index(&coord.state_dir, "api", name) {
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
    let idx = match find_key_index(&coord.state_dir, "otp", name) {
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
            // ~/.ssh/config is the session agent's to write now — the daemon
            // runs with ProtectHome=yes and cannot reach it. We persist the
            // intent and push it; the agent reconciles (also on every
            // subscribe, so a change made while it was down is picked up).
            "ssh_takeover" => settings.ssh_takeover = value,
            _ => return Response::Error("UNKNOWN_KEY".into()),
        }
        if let Err(e) = settings.save(&coord.settings_path()) {
            warn!("Failed to save settings: {}", e);
            return Response::Error(format!("SAVE_FAILED:{}", e));
        }
    }
    info!("Setting {}={}", key, value);
    if key == "ssh_takeover" {
        push_ssh_takeover(coord).await;
    }
    Response::Ok(String::new())
}

/// Tell the session agent what the ssh_takeover intent is, so it can bring
/// ~/.ssh/config in line. No-op when no agent is subscribed; the next
/// subscribe re-sends it.
async fn push_ssh_takeover(coord: &Arc<Coordinator>) {
    let on = coord.settings.read().await.ssh_takeover;
    coord
        .push_ui(format!("SSH_TAKEOVER:{}", if on { "ON" } else { "OFF" }))
        .await;
}

async fn handle_get_settings(coord: &Arc<Coordinator>) -> Response {
    let s = coord.settings.read().await;
    let msg = format!(
        "sudo={}:polkit={}:screen={}:lock={}:ssh={}",
        if s.unlock_sudo { "1" } else { "0" },
        if s.unlock_polkit { "1" } else { "0" },
        if s.unlock_screen { "1" } else { "0" },
        if s.lock_screen { "1" } else { "0" },
        if s.ssh_takeover { "1" } else { "0" },
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
        "fw={}:model=IK-1:connected={}:battery={}:uid={}:sock={}",
        fw,
        if connected { "1" } else { "0" },
        battery,
        unsafe { libc::getuid() },
        immurok_common::paths::pam_socket().display(),
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

    // Cache reads answer from disk and must work with the device away — that
    // is the entire point of having a cache. Handled before the connectivity
    // checks below. The CLI and TUI used to read these files directly from
    // ~/.immurok; the daemon's state now lives in a 0700 directory they
    // cannot enter, so it hands the JSON over instead.
    if parts[1] == "CACHE" && parts.len() >= 3 {
        return key_cache_json(coord, parts[2]);
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

