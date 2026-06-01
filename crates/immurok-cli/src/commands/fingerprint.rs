//! `immurok-cli fp` — fingerprint management subcommands.

use crate::socket_client::DaemonClient;

/// 12-step guided enrollment hint. Mirrors macOS 6480c4a's
/// `enrollTitleKeyForStep` mapping, in plain text for the CLI.
/// `step` is the *next* frame the user should press for (1..=12); the
/// daemon's `current` field is the count of frames already captured, so
/// callers pass `current + 1`.
///
/// Step layout (matches firmware 12-frame template):
///   1     正中（第一次按压）
///   2..4  保持正中
///   5     向左偏（开始切换角度）
///   6..7  保持左偏
///   8     向右偏
///   9..10 保持右偏
///   11    向指尖方向偏
///   12    向手腕方向偏
fn enroll_step_hint(step: u8) -> &'static str {
    match step {
        1 => "指肚正中按压",
        2..=4 => "保持正中按压",
        5 => "稍向左偏 5–10°",
        6..=7 => "保持左偏",
        8 => "稍向右偏 5–10°",
        9..=10 => "保持右偏",
        11 => "稍向指尖方向偏",
        12 => "稍向手腕方向偏",
        _ => "保持稳定",
    }
}

/// List enrolled fingerprints.
pub fn run_list() {
    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    let rsp = client.send("FP:LIST").unwrap_or_else(|e| {
        super::error_exit(&format!("Failed to query fingerprints: {}", e));
    });

    let parts: Vec<&str> = rsp.split(':').collect();
    if parts.first() == Some(&"OK") && parts.len() > 1 {
        if let Ok(bitmap) = parts[1].parse::<u8>() {
            let slots = immurok_common::types::fp_bitmap_slots(bitmap);
            let display = immurok_common::types::fp_bitmap_display(bitmap);
            println!("Fingerprints: {}", display);
            if slots.is_empty() {
                println!("No fingerprints enrolled.");
            } else {
                println!("Enrolled slots: {:?}", slots);
            }
        } else {
            println!("Response: {}", rsp);
        }
    } else {
        eprintln!("Error: {}", rsp);
    }
}

/// Enroll a fingerprint to a slot (with live progress).
pub fn run_enroll(slot: u8) {
    if slot >= immurok_common::protocol::MAX_FINGERPRINT_SLOTS {
        super::error_exit(&format!(
            "Invalid slot {}. Must be 0-{}.",
            slot,
            immurok_common::protocol::MAX_FINGERPRINT_SLOTS - 1
        ));
    }

    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    // Start enrollment
    let cmd = format!("FP:ENROLL:{}", slot);
    let rsp = client.send(&cmd).unwrap_or_else(|e| {
        super::error_exit(&format!("Failed to start enrollment: {}", e));
    });

    if !rsp.contains("ENROLL_STARTED") {
        super::error_exit(&format!("Enrollment failed to start: {}", rsp));
    }

    println!(
        "开始录入指纹槽位 {}（共 12 帧，按提示调整按压角度）",
        slot
    );
    println!("  下一步：{}", enroll_step_hint(1));

    // Poll for enrollment progress (12 captures × 30s max each = 360s)
    let max_polls = 2400; // ~360 seconds at 150ms intervals
    let mut last_event = 255u8;
    let mut last_current = 255u8;

    for _ in 0..max_polls {
        std::thread::sleep(std::time::Duration::from_millis(150));

        let status_rsp = match client.send("FP:STATUS") {
            Ok(r) => r,
            Err(_) => continue,
        };

        let parts: Vec<&str> = status_rsp.split(':').collect();
        if parts.first() != Some(&"OK") || parts.len() < 2 {
            continue;
        }

        if parts[1] == "IDLE" {
            continue;
        }

        let event: u8 = match parts[1].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let current: u8 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
        let total: u8 = parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(12);

        if event == last_event && current == last_current {
            continue;
        }
        last_event = event;
        last_current = current;

        // `current` is the count of frames already captured, so the next
        // pose the user should adopt is for step = current + 1. Matches
        // mac fix 71f1ca0 ("下一帧"提示) — previously every prompt before
        // the 5th capture said "正中" because the hint indexed the
        // already-finished step rather than the upcoming one.
        let next_step = current.saturating_add(1).min(total.max(1));

        match event {
            0x00 => println!(
                "  请按压传感器 — 下一步：{}",
                enroll_step_hint(next_step)
            ),
            0x01 => {
                if current < total {
                    println!(
                        "  已捕获 [{}/{}]，请抬起手指 — 下一步：{}",
                        current,
                        total,
                        enroll_step_hint(next_step)
                    );
                } else {
                    println!("  已捕获 [{}/{}]", current, total);
                }
            }
            0x02 => println!("  正在处理..."),
            0x03 => {} // CAPTURED 消息已经告诉用户抬起
            0x04 => {
                println!("\x1b[32m指纹录入完成！\x1b[0m");
                return;
            }
            0xFF => {
                eprintln!("\x1b[31m指纹录入失败。\x1b[0m");
                std::process::exit(1);
            }
            _ => println!("  状态: 0x{:02x}", event),
        }
    }

    eprintln!("\x1b[31mEnrollment timed out.\x1b[0m");
    std::process::exit(1);
}

/// Delete a fingerprint from a slot.
pub fn run_delete(slot: u8) {
    if slot >= immurok_common::protocol::MAX_FINGERPRINT_SLOTS {
        super::error_exit(&format!(
            "Invalid slot {}. Must be 0-{}.",
            slot,
            immurok_common::protocol::MAX_FINGERPRINT_SLOTS - 1
        ));
    }

    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    let cmd = format!("FP:DELETE:{}", slot);
    let rsp = client.send(&cmd).unwrap_or_else(|e| {
        super::error_exit(&format!("Failed to delete fingerprint: {}", e));
    });

    if rsp.contains("DELETED") {
        println!("\x1b[32mFingerprint slot {} deleted.\x1b[0m", slot);
    } else {
        eprintln!("\x1b[31mDelete failed: {}\x1b[0m", rsp);
        std::process::exit(1);
    }
}

/// Verify fingerprint (test).
pub fn run_verify() {
    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    println!("Touch the fingerprint sensor to verify...");

    let rsp = client.send("FP:VERIFY").unwrap_or_else(|e| {
        super::error_exit(&format!("Verification failed: {}", e));
    });

    if rsp.contains("MATCH") && !rsp.contains("NO_MATCH") {
        println!("\x1b[32mFingerprint verified!\x1b[0m");
    } else if rsp.contains("NO_MATCH") {
        println!("\x1b[31mFingerprint does not match.\x1b[0m");
    } else {
        eprintln!("Verification result: {}", rsp);
    }
}
