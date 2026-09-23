//! `immurok-cli pam` — PAM 配置安装 / 检查 / 一键修复。
//!
//! 修复目标集合按 daemon 开关派生：sudo→sudo、polkit→polkit-1。
//! 登录屏 gdm-password 不进派生集（仅 make install / 手动面板处理）。

use crate::socket_client::DaemonClient;
use immurok_common::pam::pam_line_present;
pub use immurok_client::pam::{desired_services, services_to_repair};
use immurok_client::pam::{find_helper, run_helper as run_helper_shared, PamError};

/// 从 daemon GET:SETTINGS 读取 (sudo_on, polkit_on)。失败时默认全开（保守：宁可提示也不漏）。
fn fetch_toggles() -> (bool, bool) {
    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(_) => return (true, true),
    };
    let rsp = match client.send("GET:SETTINGS") {
        Ok(r) => r,
        Err(_) => return (true, true),
    };
    let parts: Vec<&str> = rsp.split(':').collect();
    if parts.first() != Some(&"OK") {
        return (true, true);
    }
    let mut sudo_on = true;
    let mut polkit_on = true;
    for p in &parts[1..] {
        if let Some((k, v)) = p.split_once('=') {
            match k {
                "sudo" => sudo_on = v == "1",
                "polkit" => polkit_on = v == "1",
                _ => {}
            }
        }
    }
    (sudo_on, polkit_on)
}

/// `immurok-cli pam check` — 列出派生目标的安装情况，有缺失则 exit 1。
pub fn run_check() {
    let (sudo_on, polkit_on) = fetch_toggles();
    let desired = desired_services(sudo_on, polkit_on);

    println!("PAM status (by enabled features):");
    let mut missing = Vec::new();
    for svc in &desired {
        let ok = pam_line_present(svc);
        let state = if ok {
            "\x1b[32mOK\x1b[0m"
        } else {
            missing.push(*svc);
            "\x1b[33mMISSING\x1b[0m"
        };
        println!("  {:<10} {}", svc, state);
    }
    // 登录屏：仅当文件已存在才提示，缺文件不报警。
    if std::path::Path::new("/etc/pam.d/gdm-password").exists() && !pam_line_present("gdm-password")
    {
        println!("  {:<14} \x1b[33mMISSING (login screen, optional)\x1b[0m", "gdm-password");
    }

    if desired.is_empty() {
        println!("\x1b[90mNo PAM-dependent features are enabled.\x1b[0m");
    }
    if missing.is_empty() {
        println!("\x1b[32mAll configured.\x1b[0m");
    } else {
        eprintln!(
            "\x1b[33m{} missing — run 'immurok-cli pam repair' to fix.\x1b[0m",
            missing.len()
        );
        std::process::exit(1);
    }
}

/// `immurok-cli pam repair` — 一次 pkexec 修复派生目标里所有缺失项。
pub fn run_repair() {
    let (sudo_on, polkit_on) = fetch_toggles();
    let to_fix = services_to_repair(sudo_on, polkit_on);

    if to_fix.is_empty() {
        println!("\x1b[32mNothing to repair — all configured.\x1b[0m");
        return;
    }
    println!("Will repair: {}", to_fix.join(", "));
    run_helper("add", &to_fix);
}

/// 经 pkexec 跑 immurok-pam-helper，一次处理多个服务。
pub fn run_helper(action: &str, services: &[&str]) {
    let helper = match find_helper() {
        Some(h) => h,
        None => {
            eprintln!("Error: immurok-pam-helper not found in PATH or next to this binary.");
            std::process::exit(1);
        }
    };
    println!("Running: pkexec {} {} {}", helper.display(), action, services.join(" "));
    match run_helper_shared(action, services) {
        Ok(report) => {
            for l in &report.lines {
                println!("{l}");
            }
            println!("\x1b[32mPAM {} done.\x1b[0m", action);
        }
        Err(PamError::HelperFailed { code, lines }) => {
            for l in &lines {
                println!("{l}");
            }
            if code == 0 {
                eprintln!("\x1b[31mPAM {} failed (see ERROR lines above).\x1b[0m", action);
            } else {
                eprintln!("\x1b[31mPAM helper failed (exit code: {})\x1b[0m", code);
            }
            std::process::exit(1);
        }
        Err(PamError::AuthCancelled) => {
            eprintln!("\x1b[31mPAM helper failed (exit code: 126)\x1b[0m");
            std::process::exit(1);
        }
        Err(PamError::NoPolkitAgent) => {
            eprintln!("\x1b[31mPAM helper failed (exit code: 127)\x1b[0m");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to run pkexec: {}", e);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use immurok_client::pam::helper_output_has_error;

    #[test]
    fn derives_from_toggles() {
        assert_eq!(desired_services(true, true), vec!["sudo", "polkit-1"]);
        assert_eq!(desired_services(true, false), vec!["sudo"]);
        assert_eq!(desired_services(false, true), vec!["polkit-1"]);
        assert!(desired_services(false, false).is_empty());
    }

    #[test]
    fn helper_error_detection() {
        // 空串 → false
        assert!(!helper_output_has_error(""));
        // 全 OK 行 → false
        assert!(!helper_output_has_error("OK:sudo\nOK:polkit-1\n"));
        // 含一行 ERROR: → true
        assert!(helper_output_has_error(
            "OK:sudo\nERROR:MODULE_NOT_INSTALLED(sudo)\n"
        ));
        // 仅一行 ERROR: → true
        assert!(helper_output_has_error("ERROR:MODULE_NOT_INSTALLED(sudo)"));
        // 前置空格缩进的 ERROR: 行 → true
        assert!(helper_output_has_error("  ERROR:something"));
        // 大写 OK 后缀不算 ERROR → false
        assert!(!helper_output_has_error("ALL_OK:done"));
    }
}
