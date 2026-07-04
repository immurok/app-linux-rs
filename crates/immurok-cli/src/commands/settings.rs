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

    // Check PAM installation status
    println!();
    println!("PAM status:");
    for (service, label) in &[
        ("sudo", "sudo"),
        ("polkit-1", "polkit"),
        ("gdm-password", "screen"),
    ] {
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
