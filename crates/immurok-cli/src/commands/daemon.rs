//! `immurok-cli daemon` — background service management.
//!
//! The daemon runs as a systemd user service (immurok-daemon.service,
//! installed to ~/.config/systemd/user/). Restart shells out to
//! `systemctl --user`, then polls the daemon socket to confirm it is back.

use std::time::Duration;

use crate::socket_client::DaemonClient;

/// Poll interval / cap for the post-restart socket check.
const POLL_INTERVAL_MS: u64 = 500;
const POLL_MAX: u32 = 20; // 20 × 500ms = 10s

pub fn run_restart() {
    println!("Restarting immurok-daemon...");
    let output = match std::process::Command::new("systemctl")
        .args(["--user", "restart", "immurok-daemon"])
        .output()
    {
        Ok(o) => o,
        Err(e) => super::error_exit(&format!(
            "Failed to run systemctl: {} (is this a systemd system?)",
            e
        )),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        super::error_exit(&format!(
            "systemctl --user restart immurok-daemon failed (exit {}):\n{}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        ));
    }

    // Confirm the daemon socket comes back up before declaring success.
    for attempt in 0..POLL_MAX {
        if DaemonClient::connect()
            .and_then(|mut c| c.send("STATUS"))
            .is_ok()
        {
            println!("\x1b[32mDaemon restarted and back online.\x1b[0m");
            return;
        }
        if attempt + 1 < POLL_MAX {
            std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
        }
    }
    super::error_exit(
        "Daemon restarted but socket not up after 10s — check logs (immurok-cli logs).",
    );
}
