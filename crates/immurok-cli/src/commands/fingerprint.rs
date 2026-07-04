//! `immurok-cli fp` — fingerprint management subcommands.

use crate::enroll_hint::step_hint;
use crate::socket_client::DaemonClient;

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

    // With fingerprints already enrolled the firmware FP-gates ENROLL_START:
    // an *enrolled* finger must authorize before capture begins. Announce
    // that before the blocking FP:ENROLL round-trip so the user knows what
    // the device is waiting for.
    let has_fingerprints = DaemonClient::connect()
        .and_then(|mut c| c.send("FP:LIST"))
        .ok()
        .and_then(|rsp| {
            let parts: Vec<&str> = rsp.split(':').collect();
            if parts.first() == Some(&"OK") {
                parts.get(1).and_then(|s| s.parse::<u8>().ok())
            } else {
                None
            }
        })
        .is_some_and(|bitmap| bitmap != 0);
    if has_fingerprints {
        println!("Verify with an enrolled finger to authorize enrollment…");
    }

    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    // Start enrollment (returns once the FP gate has been passed)
    let cmd = format!("FP:ENROLL:{}", slot);
    let rsp = client.send(&cmd).unwrap_or_else(|e| {
        super::error_exit(&format!("Failed to start enrollment: {}", e));
    });

    if !rsp.contains("ENROLL_STARTED") {
        super::error_exit(&format!("Enrollment failed to start: {}", rsp));
    }

    println!(
        "Enrolling fingerprint slot {} (6 captures — adjust finger position as prompted)",
        slot
    );
    println!("  Next: {}", step_hint(1));

    // Poll for enrollment progress (6 captures × 30s max each = 180s)
    let max_polls = 2400; // ~360 seconds at 150ms intervals
    let mut last_event = 255u8;
    let mut last_current = 255u8;

    for _ in 0..max_polls {
        std::thread::sleep(std::time::Duration::from_millis(150));

        // Fresh connection per poll — the daemon serves exactly one
        // request per connection.
        let status_rsp = match DaemonClient::connect().and_then(|mut c| c.send("FP:STATUS")) {
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
        let total: u8 = parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(6);

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
                "  Press the sensor — next: {}",
                step_hint(next_step)
            ),
            0x01 => {
                if current < total {
                    println!(
                        "  Captured [{}/{}] — lift your finger. Next: {}",
                        current,
                        total,
                        step_hint(next_step)
                    );
                } else {
                    println!("  Captured [{}/{}]", current, total);
                }
            }
            0x02 => println!("  Processing..."),
            0x03 => {} // CAPTURED 消息已经告诉用户抬起
            0x04 => {
                println!("\x1b[32mFingerprint enrolled!\x1b[0m");
                return;
            }
            0xFF => {
                eprintln!("\x1b[31mEnrollment failed.\x1b[0m");
                std::process::exit(1);
            }
            _ => println!("  Status: 0x{:02x}", event),
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
