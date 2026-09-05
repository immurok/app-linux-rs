//! immurok-session-agent — the daemon's hands inside the user's session.
//!
//! The daemon runs as a dedicated system user with `ProtectHome=yes`: no
//! display, no session bus, no home directory. Everything that has to happen
//! *in the session* is therefore proxied through this small user-level
//! process, which holds one long-lived connection to the daemon socket:
//!
//!   daemon → agent   UI:DIALOG:AUTH                 show the touch prompt
//!                    UI:DIALOG:AGENT:<secs>:<cmd>   show the agent approval
//!                    UI:DISMISS                     take it down
//!                    NOTIFY:<text>                  desktop notification
//!                    SSH_TAKEOVER:ON|OFF            reconcile ~/.ssh/config
//!   agent  → daemon  UI:CANCEL                      user cancelled/closed it
//!
//! It holds no secrets and makes no authorization decisions — a same-uid
//! attacker can kill it or fake it, and all that buys them is *no* dialog or a
//! spurious cancel. Approval still requires a fingerprint on the device.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};

const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut backoff = RECONNECT_MIN;
    loop {
        match run().await {
            Ok(()) => {
                eprintln!("immurok-session-agent: daemon closed the connection");
                backoff = RECONNECT_MIN;
            }
            Err(e) => eprintln!("immurok-session-agent: {e}"),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

async fn run() -> Result<(), String> {
    let sock = immurok_common::paths::pam_socket();
    let stream = UnixStream::connect(&sock)
        .await
        .map_err(|e| format!("cannot connect to {}: {e}", sock.display()))?;
    let (rx, mut tx) = stream.into_split();

    tx.write_all(b"SUBSCRIBE:SESSION\n")
        .await
        .map_err(|e| format!("subscribe failed: {e}"))?;
    eprintln!("immurok-session-agent: subscribed via {}", sock.display());

    let mut lines = BufReader::new(rx).lines();
    let mut dialog: Option<Child> = None;

    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => handle(line.trim(), &mut dialog).await,
                Ok(None) => return Ok(()),
                Err(e) => return Err(format!("read failed: {e}")),
            },
            // The dialog exiting on its own is the user dismissing it —
            // Cancel and the window's close button both exit non-zero. A
            // zero exit only happens when we SIGTERM it ourselves, which is
            // the daemon telling us the request is already resolved.
            Some(code) = dialog_exit(&mut dialog) => {
                dialog = None;
                if code != Some(0) {
                    if tx.write_all(b"UI:CANCEL\n").await.is_err() {
                        return Err("cancel write failed".into());
                    }
                }
            }
        }
    }
}

async fn handle(line: &str, dialog: &mut Option<Child>) {
    if line == "UI:DIALOG:AUTH" {
        replace_dialog(dialog, spawn_dialog(&[]));
    } else if let Some(rest) = line.strip_prefix("UI:DIALOG:AGENT:") {
        // "<secs>:<command>" — the command may itself contain ':', so split once.
        let (secs, cmd) = rest.split_once(':').unwrap_or(("30", rest));
        replace_dialog(
            dialog,
            spawn_dialog(&["--agent", "--command", cmd, "--timeout", secs]),
        );
    } else if line == "UI:DISMISS" {
        take_down(dialog.take());
    } else if let Some(text) = line.strip_prefix("NOTIFY:") {
        notify(text);
    } else if let Some(state) = line.strip_prefix("SSH_TAKEOVER:") {
        let on = state == "ON";
        if let Err(e) = immurok_common::ssh_config::apply(on) {
            eprintln!("immurok-session-agent: ssh_takeover reconcile failed: {e}");
        }
    } else if !line.is_empty() {
        eprintln!("immurok-session-agent: unknown line: {line}");
    }
}

/// Wait for the current dialog, if there is one. Never resolves when there is
/// none, so it can sit in a `select!` arm without spinning.
async fn dialog_exit(dialog: &mut Option<Child>) -> Option<Option<i32>> {
    match dialog {
        Some(child) => Some(child.wait().await.ok().and_then(|s| s.code())),
        None => std::future::pending().await,
    }
}

fn replace_dialog(slot: &mut Option<Child>, next: Option<Child>) {
    // A second request while one is open shouldn't leave an orphan window.
    take_down(slot.take());
    *slot = next;
}

/// SIGTERM rather than SIGKILL: the dialog's handler quits Adw cleanly and
/// exits 0, which is how we tell "we closed it" from "the user did".
fn take_down(child: Option<Child>) {
    if let Some(mut child) = child {
        match child.id() {
            Some(pid) => unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            },
            None => {
                let _ = child.start_kill();
            }
        }
    }
}

fn dialog_path() -> PathBuf {
    // Next to our own binary first (both are installed together), then PATH.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("immurok-auth-dialog");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("immurok-auth-dialog")
}

fn spawn_dialog(args: &[&str]) -> Option<Child> {
    match Command::new(dialog_path())
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => Some(child),
        Err(e) => {
            eprintln!("immurok-session-agent: cannot spawn dialog: {e}");
            None
        }
    }
}

fn notify(text: &str) {
    let _ = Command::new("notify-send")
        .arg("immurok")
        .arg(text)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}
