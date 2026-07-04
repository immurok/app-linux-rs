//! `immurok-cli ota <path>` — manual wireless firmware push via daemon.
//!
//! Thin CLI wrapper over crate::fwupdate::push (protocol canonical
//! reference: ota/ota-update.py). No auto-retry here — the orchestrated
//! `fw update` command owns retry/resume semantics.

use indicatif::{ProgressBar, ProgressStyle};

use immurok_common::fwupdate::imfw;

use crate::fwupdate::error::FwUpdateError;
use crate::fwupdate::push::{self, PushEvent};
use crate::socket_client::DaemonClient;

pub fn run(path: &str) {
    let file_data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => super::error_exit(&format!("Cannot read '{}': {}", path, e)),
    };

    let pkg = match imfw::parse(&file_data) {
        Ok(p) => p,
        Err(e) => super::error_exit(&format!(
            "Not a valid .imfw file ({}).\n\
             Hint: build via `ota/build-ota.sh release` or run `ota/ota-package.py` on a .bin",
            e
        )),
    };

    println!("Firmware: {}", path);
    println!("  Format version: {}", pkg.format_version);
    println!("  Hardware ID:    0x{:04X}", pkg.hw_id);
    println!(
        "  Plaintext size: {} bytes ({:.1} KB)",
        pkg.fw_size,
        pkg.fw_size as f64 / 1024.0
    );
    println!(
        "  Encrypted size: {} bytes ({:.1} KB)",
        pkg.firmware.len(),
        pkg.firmware.len() as f64 / 1024.0
    );

    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    println!();

    // Best-effort current-version display (DaemonClient inherent send).
    if let Ok(resp) = client.send("OTA:VERSION") {
        if let Some(cur) = resp.strip_prefix("OK:") {
            println!("  Current version: {}", cur.trim());
        }
    }

    let total_chunks = pkg.firmware.len().div_ceil(imfw::CHUNK_SIZE);
    let pb = ProgressBar::new(total_chunks as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "  Writing [{bar:40.cyan/blue}] {percent:>3}% ({pos}/{len})",
        )
        .unwrap()
        .progress_chars("█░ "),
    );

    let mut progress = |ev: PushEvent| match ev {
        PushEvent::Stage("info") => println!("\n[1/5] Querying device info..."),
        PushEvent::Stage("erase") => println!("\n[2/5] Erasing Image B (~3-5 seconds)..."),
        PushEvent::Stage("header") => println!("\n[3/5] Sending encrypted header..."),
        PushEvent::Stage("write") => {
            println!("\n[4/5] Writing encrypted data ({} chunks)...", total_chunks)
        }
        PushEvent::Stage("end") => {
            pb.finish_and_clear();
            println!("\n[5/5] Verifying signature and rebooting...");
        }
        PushEvent::Stage(_) => {}
        PushEvent::DeviceInfo(payload) => {
            // OK payload: image_flag:image_size:block_size:chip_id (hex)
            let parts: Vec<&str> = payload.split(':').collect();
            if parts.len() >= 4 {
                let flag = u32::from_str_radix(parts[0], 16).unwrap_or(0);
                let size = u32::from_str_radix(parts[1], 16).unwrap_or(0);
                let block = u32::from_str_radix(parts[2], 16).unwrap_or(0);
                let chip = u32::from_str_radix(parts[3], 16).unwrap_or(0);
                println!("  Image Flag: 0x{:02X}", flag);
                println!("  Image Size: {} bytes ({} KB)", size, size / 1024);
                println!("  Block Size: {} bytes", block);
                println!("  Chip ID:    0x{:04X}", chip);
            }
        }
        PushEvent::Chunk { done, .. } => pb.set_position(done as u64),
    };

    match push::push_once(&mut client, &pkg, &mut progress) {
        Ok(()) => {
            println!("  Device rebooting");
            println!("\n\x1b[32mUpdate complete!\x1b[0m");
            println!("Device is rebooting, IAP will copy the new firmware automatically.");
            println!("Reconnection takes ~20 seconds.");
        }
        Err(e) => {
            pb.finish_and_clear();
            let msg = match &e {
                FwUpdateError::LowBattery => {
                    "Device battery too low for OTA (<5%). Charge and retry.".to_string()
                }
                other => other.to_string(),
            };
            super::error_exit(&msg);
        }
    }
}
