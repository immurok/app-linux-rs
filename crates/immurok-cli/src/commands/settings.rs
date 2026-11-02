//! `immurok-cli settings` / `immurok-cli set` — show and toggle settings.

use crate::socket_client::DaemonClient;

/// Show all settings.
pub fn run_show() {
    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    let rsp = client.send("GET:SETTINGS").unwrap_or_else(|e| {
        super::error_exit(&format!("Failed to query settings: {}", e));
    });

    let parts: Vec<&str> = rsp.split(':').collect();
    if parts.first() != Some(&"OK") {
        super::error_exit(&format!("Unexpected response: {}", rsp));
    }

    println!("Settings:");

    for part in &parts[1..] {
        if let Some((k, v)) = part.split_once('=') {
            let label = match k {
                "sudo" => "sudo auth",
                "polkit" => "polkit auth",
                "screen" => "screen unlock",
                "lock" => "long-press lock",
                _ => k,
            };
            let state = if v == "1" {
                "\x1b[32mON\x1b[0m"
            } else {
                "\x1b[31mOFF\x1b[0m"
            };
            println!("  {:<16} {}", label, state);
        }
    }

    // Privilege separation status — the one line that says whether the PAM
    // channel can be hijacked by any process running as this user.
    println!();
    println!("Daemon:");
    match crate::socket_client::probe_isolation() {
        Some(iso) if iso.isolated => {
            println!(
                "  {:<16} \x1b[32misolated\x1b[0m (uid {}, {})",
                "privileges",
                iso.daemon_uid.unwrap_or(0),
                iso.socket
            );
        }
        Some(_) => {
            println!(
                "  {:<16} \x1b[31mNOT isolated\x1b[0m — running as your user; any of your \
                 processes can pass sudo. Run 'make install' to migrate.",
                "privileges"
            );
        }
        None => println!("  {:<16} unknown (daemon not reachable)", "privileges"),
    }

    // Check PAM installation status
    println!();
    println!("PAM status:");
    // 只列真正经过模块的服务。曾经有一行 ("gdm-password", "screen")，但它
    // 两头都不对：模块对 gdm-password 直接 PAM_IGNORE（pam/fp_policy.h），
    // 而「锁屏解锁」本身是 daemon 的 unlock_screen 开关（走 loginctl
    // unlock-session），跟 PAM 配置没关系 —— 印在 PAM 一节里只会让人以为
    // 锁屏解锁没装上。
    for (service, label) in &[("sudo", "sudo"), ("polkit-1", "polkit")] {
        let installed = immurok_common::pam::pam_line_present(service);
        let state = if installed {
            "\x1b[32minstalled\x1b[0m"
        } else {
            "\x1b[33mnot installed\x1b[0m"
        };
        println!("  {:<16} {}", label, state);
    }
}

/// Toggle a setting.
pub fn run_set(key: &str, value: &str) {
    let val = match super::parse_on_off(value) {
        Some(v) => v,
        None => super::error_exit("Value must be 'on' or 'off'."),
    };

    let cmd = match key {
        "sudo" => format!("SET:UNLOCK_SUDO:{}", val),
        "polkit" => format!("SET:UNLOCK_POLKIT:{}", val),
        "screen" => format!("SET:UNLOCK_SCREEN:{}", val),
        "lock" => format!("SET:LOCK_SCREEN:{}", val),
        _ => super::error_exit(&format!("Unknown setting: {}", key)),
    };

    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    let rsp = client.send(&cmd).unwrap_or_else(|e| {
        super::error_exit(&format!("Failed to set {}: {}", key, e));
    });

    if rsp.starts_with("OK") {
        let state = if val == "1" { "ON" } else { "OFF" };
        println!("\x1b[32m{} set to {}.\x1b[0m", key, state);
    } else {
        eprintln!("\x1b[31mFailed to set {}: {}\x1b[0m", key, rsp);
        std::process::exit(1);
    }
}
