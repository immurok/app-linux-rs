//! `immurok-cli ota <path>` — wireless firmware OTA via daemon.
//!
//! Protocol mirrors `ota/ota-update.py` (the canonical implementation):
//!   1. OTA:VERSION       → query current device firmware
//!   2. OTA:INFO          → image flag / size / block size / chip ID
//!   3. OTA:ERASE         → wipe Image B (~3–5 s)
//!   4. OTA:HEADER:<b64>  → send 96-byte signed header
//!   5. OTA:WRITE:<off_hex>:<b64> → encrypted body in 240-byte chunks
//!   6. OTA:END           → device verifies SHA256 + HMAC, reboots,
//!                          IAP swaps Image B → A
//!
//! Only `.imfw` (encrypted + signed) packages produced by `ota/ota-package.py`
//! are accepted — magic 0x494D4657 in bytes 0..4 of the header.

use std::time::Instant;

use base64::Engine;
use indicatif::{ProgressBar, ProgressStyle};

use crate::socket_client::DaemonClient;

/// CHUNK_SIZE must be ≤ 243 (BLE OTA characteristic write payload limit) and
/// 16-byte aligned (AES block boundary). 240 is the largest qualifying value.
const CHUNK_SIZE: usize = 240;
const IMAGE_B_SIZE: usize = 216 * 1024;
const IMFW_MAGIC: u32 = 0x494D4657; // "IMFW"
const IMFW_HEADER_SIZE_V1: usize = 96; // HMAC (legacy bootstrap)
const IMFW_HEADER_SIZE_V2: usize = 128; // ECDSA (1.6.0+)

pub fn run(path: &str) {
    let file_data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => super::error_exit(&format!("Cannot read '{}': {}", path, e)),
    };

    let imfw = match parse_imfw(&file_data) {
        Some(p) => p,
        None => super::error_exit(
            "Not a valid .imfw file (only encrypted+signed firmware supported).\n\
             Hint: build via `ota/build-ota.sh release` or run `ota/ota-package.py` on a .bin",
        ),
    };

    let fw_size = imfw.firmware.len();
    if fw_size == 0 {
        super::error_exit("Firmware payload is empty.");
    }
    if fw_size > IMAGE_B_SIZE {
        super::error_exit(&format!(
            "Firmware too large ({} bytes > {} bytes)",
            fw_size, IMAGE_B_SIZE
        ));
    }

    println!("Firmware: {}", path);
    println!("  Format version: {}", imfw.format_version);
    println!("  Hardware ID:    0x{:04X}", imfw.hw_id);
    println!(
        "  Plaintext size: {} bytes ({:.1} KB)",
        imfw.plaintext_size,
        imfw.plaintext_size as f64 / 1024.0
    );
    println!(
        "  Encrypted size: {} bytes ({:.1} KB)",
        fw_size,
        fw_size as f64 / 1024.0
    );

    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    println!();

    // ── Step 0: query current version (best-effort) ─────────────
    if let Ok(resp) = client.send("OTA:VERSION") {
        if let Some(cur) = resp.strip_prefix("OK:") {
            println!("  Current version: {}", cur.trim());
        }
    }

    let total_steps = 5;

    // ── Step 1: device info ─────────────────────────────────────
    println!("\n[1/{}] Querying device info...", total_steps);
    let info = match client.send("OTA:INFO") {
        Ok(r) => r,
        Err(e) => super::error_exit(&format!("OTA:INFO failed: {}", e)),
    };
    if let Some(rest) = info.strip_prefix("OK:") {
        // OK:image_flag:image_size:block_size:chip_id (all hex, no 0x)
        let parts: Vec<&str> = rest.split(':').collect();
        if parts.len() >= 4 {
            let flag = u32::from_str_radix(parts[0], 16).unwrap_or(0);
            let size = u32::from_str_radix(parts[1], 16).unwrap_or(0);
            let block = u32::from_str_radix(parts[2], 16).unwrap_or(0);
            let chip = u32::from_str_radix(parts[3], 16).unwrap_or(0);
            println!("  Image Flag: 0x{:02X}", flag);
            println!("  Image Size: {} bytes ({} KB)", size, size / 1024);
            println!("  Block Size: {} bytes", block);
            println!("  Chip ID:    0x{:04X}", chip);
        } else {
            println!("  Response: {}", info);
        }
    } else {
        super::error_exit(&format!("OTA:INFO error: {}", info));
    }

    // ── Step 2: erase Image B ───────────────────────────────────
    println!("\n[2/{}] Erasing Image B (~3-5 seconds)...", total_steps);
    let t0 = Instant::now();
    let erase_resp = client
        .send("OTA:ERASE")
        .unwrap_or_else(|e| super::error_exit(&format!("OTA:ERASE failed: {}", e)));
    if !is_ok(&erase_resp) {
        super::error_exit(&friendly_error("erase", &erase_resp));
    }
    println!("  Erase complete ({:.1}s)", t0.elapsed().as_secs_f64());

    // ── Step 3: header ──────────────────────────────────────────
    println!("\n[3/{}] Sending encrypted header...", total_steps);
    let hdr_b64 = base64::engine::general_purpose::STANDARD.encode(&imfw.header);
    let cmd = format!("OTA:HEADER:{}", hdr_b64);
    let header_resp = client
        .send(&cmd)
        .unwrap_or_else(|e| super::error_exit(&format!("OTA:HEADER failed: {}", e)));
    if !is_ok(&header_resp) {
        super::error_exit(&friendly_error("header", &header_resp));
    }
    println!("  Header accepted");

    // ── Step 4: chunked write ───────────────────────────────────
    let total_chunks = (fw_size + CHUNK_SIZE - 1) / CHUNK_SIZE;
    println!(
        "\n[4/{}] Writing encrypted data ({} chunks)...",
        total_steps, total_chunks
    );

    let pb = ProgressBar::new(total_chunks as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "  Writing [{bar:40.cyan/blue}] {percent:>3}% ({pos}/{len})",
        )
        .unwrap()
        .progress_chars("█░ "),
    );

    let t_write = Instant::now();
    for i in 0..total_chunks {
        let offset = i * CHUNK_SIZE;
        let end = (offset + CHUNK_SIZE).min(fw_size);
        let chunk = &imfw.firmware[offset..end];
        let b64 = base64::engine::general_purpose::STANDARD.encode(chunk);
        let cmd = format!("OTA:WRITE:{:04x}:{}", offset, b64);
        let resp = match client.send(&cmd) {
            Ok(r) => r,
            Err(e) => {
                pb.finish_and_clear();
                super::error_exit(&format!(
                    "OTA:WRITE failed @ offset 0x{:04x}: {}",
                    offset, e
                ));
            }
        };
        if !is_ok(&resp) {
            pb.finish_and_clear();
            super::error_exit(&format!(
                "OTA:WRITE rejected @ offset 0x{:04x}: {}",
                offset, friendly_error("write", &resp)
            ));
        }
        pb.set_position((i + 1) as u64);
    }
    pb.finish_and_clear();

    let elapsed = t_write.elapsed().as_secs_f64();
    let speed = if elapsed > 0.0 {
        (fw_size as f64) / elapsed / 1024.0
    } else {
        0.0
    };
    println!("  Write complete ({:.1}s, {:.1} KB/s)", elapsed, speed);

    // ── Step 5: verify + reboot ─────────────────────────────────
    println!("\n[5/{}] Verifying signature and rebooting...", total_steps);
    let end_resp = client
        .send("OTA:END")
        .unwrap_or_else(|e| super::error_exit(&format!("OTA:END failed: {}", e)));
    if is_ok(&end_resp) {
        println!("  Device rebooting");
    } else if end_resp.contains("SHA256") {
        super::error_exit("Integrity check failed (SHA256 mismatch)");
    } else if end_resp.contains("HMAC") {
        super::error_exit("Signature verification failed (unofficial firmware)");
    } else {
        super::error_exit(&friendly_error("end", &end_resp));
    }

    println!("\n\x1b[32mUpdate complete!\x1b[0m");
    println!("Device is rebooting, IAP will copy the new firmware automatically.");
    println!("Reconnection takes ~20 seconds.");
}

// ── .imfw parser ────────────────────────────────────────────────

struct Imfw<'a> {
    header: &'a [u8],
    firmware: &'a [u8],
    format_version: u8,
    hw_id: u16,
    plaintext_size: u32,
}

fn parse_imfw(data: &[u8]) -> Option<Imfw<'_>> {
    if data.len() < IMFW_HEADER_SIZE_V1 {
        return None;
    }
    let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if magic != IMFW_MAGIC {
        return None;
    }
    // Header layout (matches ota/ota-package.py + ota-update.py):
    //   <I magic, B version, B flags, H hw_id, I fw_size, …signature/hmac>
    let format_version = data[4];
    let _flags = data[5];
    let hw_id = u16::from_le_bytes([data[6], data[7]]);
    let plaintext_size = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);

    // Header size depends on the format version: v2 (ECDSA) is 128 bytes, v1
    // (HMAC) is 96. The signature/payload start shifts accordingly.
    let header_size = if format_version >= 2 {
        IMFW_HEADER_SIZE_V2
    } else {
        IMFW_HEADER_SIZE_V1
    };
    if data.len() < header_size {
        return None;
    }

    Some(Imfw {
        header: &data[..header_size],
        firmware: &data[header_size..],
        format_version,
        hw_id,
        plaintext_size,
    })
}

// ── helpers ─────────────────────────────────────────────────────

fn is_ok(resp: &str) -> bool {
    let t = resp.trim();
    t == "OK" || t.starts_with("OK:")
}

/// Map daemon error responses to friendlier messages. The most useful one
/// is LOW_BATTERY (firmware 1.3.1+ refuses long writes < 5%).
fn friendly_error(stage: &str, resp: &str) -> String {
    let trimmed = resp.trim();
    if trimmed.starts_with("ERROR:LOW_BATTERY") {
        return "Device battery too low for OTA (<5%). Charge and retry.".to_string();
    }
    format!("OTA {} stage failed: {}", stage, trimmed)
}
