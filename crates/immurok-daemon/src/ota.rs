//! OTA update handler — proxies OTA commands from socket to BLE OTA characteristic.
//!
//! The OTA session is managed over a persistent socket connection with line-based
//! protocol. Each command is processed one at a time, matching the Python
//! `ota-update.py` client.
//!
//! OTA IAP protocol (WCH method) — all via OTA characteristic (0xFEE1):
//!   - CMD_IAP_INFO (0x84): query IAP bootloader info
//!   - CMD_IAP_ERASE (0x81): erase Image B blocks
//!   - CMD_IAP_HEADER (0x85): send 96-byte encrypted firmware header
//!   - CMD_IAP_PROM (0x80): write data block (fire-and-forget for speed)
//!   - CMD_IAP_VERIFY (0x82): verify written data
//!   - CMD_IAP_END (0x83): finalize, verify signature, and reboot

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tracing::{info, warn};

use immurok_common::protocol;

use crate::coordinator::Coordinator;

/// OTA session timeout (seconds) — how long to wait for next socket command.
const OTA_SESSION_TIMEOUT_SECS: u64 = 120;

/// Default OTA command timeout (milliseconds) for write-and-read operations.
const OTA_CMD_TIMEOUT_MS: u64 = 5000;

/// Erase timeout (milliseconds) — erasing flash takes several seconds.
const OTA_ERASE_TIMEOUT_MS: u64 = 15000;

/// Handle a persistent OTA session over a Unix socket.
///
/// The first OTA command has already been read by the socket handler and is
/// passed as `first_line`. This function processes it, sends the response,
/// then enters a loop reading subsequent commands until OTA:END or disconnect.
pub async fn handle_ota_session(
    stream: &mut UnixStream,
    coord: &Arc<Coordinator>,
    first_line: &str,
) {
    info!("OTA session started");

    let mut reader = BufReader::new(stream);

    // Process first command
    let response = process_ota_command(first_line, coord).await;
    if send_line(&mut reader, &response).await.is_err() {
        return;
    }

    // Check if first command was END
    if first_line.starts_with("OTA:END") {
        info!("OTA session ended");
        return;
    }

    // Persistent connection: read subsequent commands line by line
    let mut line_buf = String::new();
    loop {
        line_buf.clear();
        let read_result = tokio::time::timeout(
            std::time::Duration::from_secs(OTA_SESSION_TIMEOUT_SECS),
            reader.read_line(&mut line_buf),
        )
        .await;

        match read_result {
            Ok(Ok(0)) => break,    // EOF
            Ok(Err(_)) => break,   // Read error
            Err(_) => {
                warn!("OTA session timeout");
                break;
            }
            Ok(Ok(_)) => {}
        }

        let request = line_buf.trim();
        if request.is_empty() {
            continue;
        }

        let response = process_ota_command(request, coord).await;
        if send_line(&mut reader, &response).await.is_err() {
            break;
        }

        // End session after OTA:END
        if request.starts_with("OTA:END") {
            break;
        }
    }

    info!("OTA session ended");
}

/// Send a response line (with newline) to the socket client.
async fn send_line(reader: &mut BufReader<&mut UnixStream>, line: &str) -> Result<(), ()> {
    let data = format!("{}\n", line);
    reader
        .get_mut()
        .write_all(data.as_bytes())
        .await
        .map_err(|_| ())
}

/// Process a single OTA command and return the response string.
async fn process_ota_command(line: &str, coord: &Arc<Coordinator>) -> String {
    let parts: Vec<&str> = line.splitn(5, ':').collect();
    if parts.len() < 2 || parts[0] != "OTA" {
        return "ERROR:INVALID_FORMAT".to_string();
    }

    let sub = parts[1];
    if sub != "WRITE" {
        info!("OTA command: {}", sub);
    }

    match sub {
        "VERSION" => handle_ota_version(coord).await,
        "INFO" => handle_ota_info(coord).await,
        "ERASE" => handle_ota_erase(coord).await,
        "HEADER" => handle_ota_header(&parts, coord).await,
        "WRITE" => handle_ota_write(&parts, coord).await,
        "VERIFY" => handle_ota_verify(&parts, coord).await,
        "END" => handle_ota_end(coord).await,
        _ => "ERROR:UNKNOWN_OTA_CMD".to_string(),
    }
}

/// OTA:VERSION → OK:<fw_version>
async fn handle_ota_version(coord: &Arc<Coordinator>) -> String {
    let status = coord.device_status.read().await;
    let ver = match status.as_ref() {
        Some(s) if !s.fw_version.is_empty() => s.fw_version.clone(),
        _ => "unknown".to_string(),
    };
    format!("OK:{}", ver)
}

/// OTA:INFO → write [0x84, 0x02, 0x00, 0x00], read → parse info → OK:xx:xxxxxxxx:xxxx:xxxx
async fn handle_ota_info(coord: &Arc<Coordinator>) -> String {
    if !coord.is_connected.load(std::sync::atomic::Ordering::Relaxed) {
        return "ERROR:OTA_NOT_AVAILABLE".to_string();
    }

    let cmd = vec![0x84, 0x02, 0x00, 0x00];
    let resp = match coord.ota_write_and_read(cmd, OTA_CMD_TIMEOUT_MS).await {
        Ok(r) => r,
        Err(e) => return format!("ERROR:{}", e),
    };

    if resp.len() < 9 {
        return "ERROR:NO_RESPONSE".to_string();
    }

    let image_flag = resp[0];
    let image_size = (resp[1] as u32)
        | ((resp[2] as u32) << 8)
        | ((resp[3] as u32) << 16)
        | ((resp[4] as u32) << 24);
    let block_size = (resp[5] as u16) | ((resp[6] as u16) << 8);
    let chip_id = (resp[7] as u16) | ((resp[8] as u16) << 8);

    let reply = format!(
        "OK:{:02x}:{:08x}:{:04x}:{:04x}",
        image_flag, image_size, block_size, chip_id
    );
    info!("OTA INFO: {}", reply);
    reply
}

/// OTA:ERASE → write [0x81, 0x04, 0x00, 0x00, blocks_lo, blocks_hi], read → OK or ERROR
async fn handle_ota_erase(coord: &Arc<Coordinator>) -> String {
    if !coord.is_connected.load(std::sync::atomic::Ordering::Relaxed) {
        return "ERROR:OTA_NOT_AVAILABLE".to_string();
    }

    let blocks = protocol::OTA_IMAGE_B_BLOCKS;
    let cmd = vec![
        0x81,
        0x04,
        0x00,
        0x00,
        (blocks & 0xFF) as u8,
        ((blocks >> 8) & 0xFF) as u8,
    ];
    let resp = match coord.ota_write_and_read(cmd, OTA_ERASE_TIMEOUT_MS).await {
        Ok(r) => r,
        Err(e) => return format!("ERROR:ERASE_TIMEOUT:{}", e),
    };

    if resp.is_empty() {
        return "ERROR:ERASE_TIMEOUT".to_string();
    }

    if resp[0] == 0x00 {
        info!("OTA ERASE ok");
        "OK".to_string()
    } else if resp[0] == immurok_common::protocol::RSP_ERR_LOW_BATTERY {
        warn!("OTA ERASE rejected: low-battery protection");
        "ERROR:LOW_BATTERY:OTA refused (device <5%, charge to retry)".to_string()
    } else {
        warn!("OTA ERASE failed: 0x{:02x}", resp[0]);
        format!("ERROR:ERASE_FAILED:{:02x}", resp[0])
    }
}

/// OTA:HEADER:<base64> → decode 96-byte header, write [0x85, len, header...], read → OK or ERROR
async fn handle_ota_header(parts: &[&str], coord: &Arc<Coordinator>) -> String {
    if parts.len() < 3 {
        return "ERROR:INVALID_FORMAT".to_string();
    }

    if !coord.is_connected.load(std::sync::atomic::Ordering::Relaxed) {
        return "ERROR:OTA_NOT_AVAILABLE".to_string();
    }

    // parts[2] onwards may contain colons from base64; rejoin them
    let b64_data = if parts.len() > 3 {
        parts[2..].join(":")
    } else {
        parts[2].to_string()
    };

    let header_data = match BASE64.decode(&b64_data) {
        Ok(d) => d,
        Err(_) => return "ERROR:INVALID_DATA".to_string(),
    };

    // 96 = v1 (HMAC, legacy bootstrap); 128 = v2 (ECDSA, 1.6.0+).
    if header_data.len() != 96 && header_data.len() != 128 {
        return "ERROR:INVALID_HEADER_SIZE".to_string();
    }

    // CMD_IAP_HEADER: [0x85, len, header_data...]
    let mut cmd = Vec::with_capacity(2 + header_data.len());
    cmd.push(0x85);
    cmd.push(header_data.len() as u8);
    cmd.extend_from_slice(&header_data);

    let resp = match coord.ota_write_and_read(cmd, OTA_CMD_TIMEOUT_MS).await {
        Ok(r) => r,
        Err(_) => return "ERROR:HEADER_TIMEOUT".to_string(),
    };

    if resp.is_empty() {
        return "ERROR:HEADER_TIMEOUT".to_string();
    }

    if resp[0] == 0x00 {
        info!("OTA HEADER accepted");
        "OK".to_string()
    } else {
        warn!("OTA HEADER rejected: 0x{:02x}", resp[0]);
        format!("ERROR:HEADER_REJECTED:{:02x}", resp[0])
    }
}

/// OTA:WRITE:<hex_offset>:<base64_data> → write [0x80, len, addr_lo, addr_hi, data...] (no read)
async fn handle_ota_write(parts: &[&str], coord: &Arc<Coordinator>) -> String {
    if parts.len() < 4 {
        return "ERROR:INVALID_FORMAT".to_string();
    }

    if !coord.is_connected.load(std::sync::atomic::Ordering::Relaxed) {
        return "ERROR:OTA_NOT_AVAILABLE".to_string();
    }

    let offset = match u32::from_str_radix(parts[2], 16) {
        Ok(v) => v,
        Err(_) => return "ERROR:INVALID_OFFSET".to_string(),
    };

    // parts[3] onwards may contain colons from base64; rejoin them
    let b64_data = if parts.len() > 4 {
        parts[3..].join(":")
    } else {
        parts[3].to_string()
    };

    let data = match BASE64.decode(&b64_data) {
        Ok(d) => d,
        Err(_) => return "ERROR:INVALID_DATA".to_string(),
    };

    let encoded_addr = (offset / 16) as u16;

    // CMD_IAP_PROM: [0x80, data_len, addr_lo, addr_hi, data...]
    let mut cmd = Vec::with_capacity(4 + data.len());
    cmd.push(0x80);
    cmd.push(data.len() as u8);
    cmd.push((encoded_addr & 0xFF) as u8);
    cmd.push(((encoded_addr >> 8) & 0xFF) as u8);
    cmd.extend_from_slice(&data);

    match coord.ota_write(cmd).await {
        Ok(()) => "OK".to_string(),
        Err(_) => "ERROR:WRITE_FAILED".to_string(),
    }
}

/// OTA:VERIFY:<hex_offset>:<base64_data> → write [0x82, len, addr_lo, addr_hi, data...], read → OK or ERROR
async fn handle_ota_verify(parts: &[&str], coord: &Arc<Coordinator>) -> String {
    if parts.len() < 4 {
        return "ERROR:INVALID_FORMAT".to_string();
    }

    if !coord.is_connected.load(std::sync::atomic::Ordering::Relaxed) {
        return "ERROR:OTA_NOT_AVAILABLE".to_string();
    }

    let offset = match u32::from_str_radix(parts[2], 16) {
        Ok(v) => v,
        Err(_) => return "ERROR:INVALID_OFFSET".to_string(),
    };

    let b64_data = if parts.len() > 4 {
        parts[3..].join(":")
    } else {
        parts[3].to_string()
    };

    let data = match BASE64.decode(&b64_data) {
        Ok(d) => d,
        Err(_) => return "ERROR:INVALID_DATA".to_string(),
    };

    let encoded_addr = (offset / 16) as u16;

    // CMD_IAP_VERIFY: [0x82, data_len, addr_lo, addr_hi, data...]
    let mut cmd = Vec::with_capacity(4 + data.len());
    cmd.push(0x82);
    cmd.push(data.len() as u8);
    cmd.push((encoded_addr & 0xFF) as u8);
    cmd.push(((encoded_addr >> 8) & 0xFF) as u8);
    cmd.extend_from_slice(&data);

    let resp = match coord.ota_write_and_read(cmd, OTA_CMD_TIMEOUT_MS).await {
        Ok(r) => r,
        Err(_) => return "ERROR:VERIFY_TIMEOUT".to_string(),
    };

    if resp.is_empty() {
        return "ERROR:VERIFY_TIMEOUT".to_string();
    }

    if resp[0] == 0x00 {
        "OK".to_string()
    } else if resp[0] == immurok_common::protocol::RSP_ERR_LOW_BATTERY {
        "ERROR:LOW_BATTERY:OTA refused (device <5%, charge to retry)".to_string()
    } else {
        format!("ERROR:VERIFY_FAILED:{:02x}", resp[0])
    }
}

/// OTA:END → write [0x83, 0x02, 0x00, 0x00], read → OK (device reboots, connection may drop)
async fn handle_ota_end(coord: &Arc<Coordinator>) -> String {
    if !coord.is_connected.load(std::sync::atomic::Ordering::Relaxed) {
        return "ERROR:OTA_NOT_AVAILABLE".to_string();
    }

    let cmd = vec![0x83, 0x02, 0x00, 0x00];
    match coord.ota_write_and_read(cmd, OTA_CMD_TIMEOUT_MS).await {
        Ok(resp) => {
            if !resp.is_empty() {
                if resp[0] == 0xF1 {
                    return "ERROR:SHA256_MISMATCH".to_string();
                }
                if resp[0] == 0xF2 {
                    return "ERROR:HMAC_MISMATCH".to_string();
                }
                info!("OTA END response: {}", hex::encode(&resp));
            }
            // Success or no meaningful error
            "OK".to_string()
        }
        Err(_) => {
            // Device rebooted — connection dropped, this is expected success
            info!("OTA END: device rebooted (connection dropped)");
            "OK".to_string()
        }
    }
}
