//! SSH agent — implements the OpenSSH agent protocol over a Unix socket.
//!
//! Listens on `~/.immurok/agent.sock` (chmod 0o600).
//! Proxies sign requests to the hardware device via BLE (FP-gated ECDSA).
//!
//! Binary protocol: `[length:4B BE][type:1B][payload]`
//!
//! Supported messages:
//!   SSH_AGENTC_REQUEST_IDENTITIES (11) → SSH_AGENT_IDENTITIES_ANSWER (12)
//!   SSH_AGENTC_SIGN_REQUEST (13)       → SSH_AGENT_SIGN_RESPONSE (14)
//!   All other                          → SSH_AGENT_FAILURE (5)

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tracing::{debug, info, warn};

use immurok_common::protocol;

use crate::coordinator::Coordinator;
use crate::keystore;

// SSH agent message type constants
const SSH_AGENT_FAILURE: u8 = 5;
const SSH_AGENTC_REQUEST_IDENTITIES: u8 = 11;
const SSH_AGENT_IDENTITIES_ANSWER: u8 = 12;
const SSH_AGENTC_SIGN_REQUEST: u8 = 13;
const SSH_AGENT_SIGN_RESPONSE: u8 = 14;

/// Main SSH agent server loop.
pub async fn serve(coordinator: Arc<Coordinator>, socket_path: &Path) {
    // Remove stale socket
    let _ = std::fs::remove_file(socket_path);

    let listener = match UnixListener::bind(socket_path) {
        Ok(l) => l,
        Err(e) => {
            warn!(
                "Failed to bind SSH agent socket at {}: {}",
                socket_path.display(),
                e
            );
            return;
        }
    };

    // chmod 0o600 — only current user should access the agent
    if let Err(e) =
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))
    {
        warn!("Failed to chmod agent socket: {}", e);
    }

    info!("SSH agent listening on {}", socket_path.display());

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let coord = coordinator.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_agent_client(stream, coord).await {
                        debug!("SSH agent client error: {}", e);
                    }
                });
            }
            Err(e) => {
                warn!("SSH agent accept error: {}", e);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// Verify peer is current user via SO_PEERCRED.
fn verify_peer_uid(stream: &UnixStream) -> Result<(), String> {
    use std::os::unix::io::AsRawFd;

    let fd = stream.as_raw_fd();
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;

    let ret = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };

    if ret != 0 {
        return Err("getsockopt(SO_PEERCRED) failed".into());
    }

    let my_uid = unsafe { libc::getuid() };
    if cred.uid != my_uid {
        return Err(format!(
            "Rejected UID {} (expected {})",
            cred.uid, my_uid
        ));
    }

    Ok(())
}

/// Handle a single SSH agent client connection (multiple request/response exchanges).
async fn handle_agent_client(
    mut stream: UnixStream,
    coord: Arc<Coordinator>,
) -> Result<(), String> {
    if let Err(e) = verify_peer_uid(&stream) {
        warn!("SSH agent peer check failed: {}", e);
        return Err(e);
    }

    // SSH agent protocol: multiple request/response per connection
    loop {
        // Read message with timeout (SSH may hold connections for a while)
        let msg = match tokio::time::timeout(
            Duration::from_secs(120),
            read_agent_message(&mut stream),
        )
        .await
        {
            Ok(Ok(Some(msg))) => msg,
            Ok(Ok(None)) => return Ok(()), // Connection closed
            Ok(Err(e)) => return Err(format!("Read error: {}", e)),
            Err(_) => return Ok(()),        // Timeout, close gracefully
        };

        if msg.is_empty() {
            send_failure(&mut stream).await;
            continue;
        }

        let msg_type = msg[0];
        let payload = if msg.len() > 1 { &msg[1..] } else { &[] };

        match msg_type {
            SSH_AGENTC_REQUEST_IDENTITIES => {
                handle_request_identities(&mut stream, &coord).await;
            }
            SSH_AGENTC_SIGN_REQUEST => {
                handle_sign_request(&mut stream, &coord, payload).await;
            }
            _ => {
                debug!("SSH agent: unknown message type: {}", msg_type);
                send_failure(&mut stream).await;
            }
        }
    }
}

// ── REQUEST_IDENTITIES ──────────────────────────────────────

async fn handle_request_identities(stream: &mut UnixStream, coord: &Arc<Coordinator>) {
    let keys = keystore::load_ssh_keys(&coord.immurok_dir);

    let mut body = Vec::new();
    body.push(SSH_AGENT_IDENTITIES_ANSWER);

    // nkeys (uint32 BE)
    append_u32_be(&mut body, keys.len() as u32);

    for entry in &keys {
        // key blob (string)
        append_ssh_string(&mut body, &entry.public_key_blob);
        // comment (string) — use name
        append_ssh_string_str(&mut body, &entry.name);
    }

    send_agent_message(stream, &body).await;
}

// ── SIGN_REQUEST ────────────────────────────────────────────

async fn handle_sign_request(
    stream: &mut UnixStream,
    coord: &Arc<Coordinator>,
    payload: &[u8],
) {
    // Parse: [string key_blob][string data][uint32 flags]
    let mut offset = 0;

    let key_blob = match read_ssh_string(payload, &mut offset) {
        Some(b) => b,
        None => {
            send_failure(stream).await;
            return;
        }
    };

    let sign_data = match read_ssh_string(payload, &mut offset) {
        Some(d) => d,
        None => {
            send_failure(stream).await;
            return;
        }
    };

    // flags (optional uint32) — we ignore for now
    // let _flags = read_u32_be(payload, &mut offset);

    // Find key index in cache
    let keys = keystore::load_ssh_keys(&coord.immurok_dir);
    let matching = keys.iter().find(|k| k.public_key_blob == key_blob);
    let key_entry = match matching {
        Some(e) => e,
        None => {
            warn!("SSH agent: key not found in cache");
            send_failure(stream).await;
            return;
        }
    };

    let idx = key_entry.index;
    info!(
        "SSH agent: sign request for key idx={} ({}), data={} bytes",
        idx,
        key_entry.name,
        sign_data.len()
    );

    // Device must be connected and verified
    if !coord.is_connected.load(Ordering::Relaxed) {
        warn!("SSH agent: device not connected, waiting for reconnection...");
        // Wait up to 5 seconds for reconnection
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if coord.is_connected.load(Ordering::Relaxed) {
                break;
            }
        }
        if !coord.is_connected.load(Ordering::Relaxed) {
            warn!("SSH agent: reconnection timeout");
            send_failure(stream).await;
            return;
        }
        // Brief settle after reconnect
        tokio::time::sleep(Duration::from_millis(300)).await;
        info!("SSH agent: device reconnected, proceeding with sign");
    }

    if !coord.is_device_verified.load(Ordering::Relaxed) {
        warn!("SSH agent: device not verified");
        send_failure(stream).await;
        return;
    }

    // Hash the data with SHA-256
    let hash = Sha256::digest(&sign_data);

    // BLE KEY_SIGN: payload = [cat:1B][idx:1B][offset:1B][hash_data...]
    let mut sign_payload = vec![protocol::KEY_CAT_SSH, idx, 0];
    sign_payload.extend_from_slice(&hash);

    // KEY_SIGN is FP-gated — device prompts for fingerprint
    let sign_result = tokio::time::timeout(
        Duration::from_secs(50), // 30s FP gate + margin
        coord.ble_send_fp_gated(protocol::CMD_KEY_SIGN, sign_payload),
    )
    .await;

    let (status, key_sign_resp) = match sign_result {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => {
            warn!("SSH agent: KEY_SIGN BLE error: {}", e);
            send_failure(stream).await;
            return;
        }
        Err(_) => {
            warn!("SSH agent: KEY_SIGN timeout");
            send_failure(stream).await;
            return;
        }
    };

    if status != protocol::RSP_OK && status != protocol::RSP_FP_GATE_APPROVED {
        warn!("SSH agent: KEY_SIGN failed: status=0x{:02x}", status);
        send_failure(stream).await;
        return;
    }

    // After KEY_SIGN approval, read the actual signature via KEY_RESULT.
    // Wire-level response: [status:1B][total:1B][offset:1B][data:<=59B]
    // BUT `ble_send` already strips the status byte, so `data` here is
    // `[total:1B][offset:1B][chunk:<=59B]`. The previous code read
    // data[1]/data[2] as if status was still in front, picking up `offset`
    // as `total` (almost always 0x00 → truncate(0) wiped the buffer).
    // ECDSA P-256 signature is 64 bytes (r:32 + s:32), needs 2 chunks.
    let mut signature = Vec::new();
    let mut read_offset: u8 = 0;
    let mut read_ok = false;

    for _attempt in 0..4 {
        let result = coord
            .ble_send(protocol::CMD_KEY_RESULT, vec![read_offset])
            .await;

        match result {
            Ok((s, data)) if s == protocol::RSP_OK && data.len() >= 2 => {
                let total = data[0] as usize;
                let _chunk_off = data[1] as usize;
                let chunk = &data[2..];
                signature.extend_from_slice(chunk);
                if signature.len() >= total {
                    signature.truncate(total);
                    read_ok = true;
                    break;
                }
                read_offset = signature.len() as u8;
            }
            Ok((s, data)) => {
                warn!(
                    "SSH agent: KEY_RESULT unexpected: status=0x{:02x}, len={}",
                    s,
                    data.len()
                );
                break;
            }
            Err(e) => {
                warn!("SSH agent: KEY_RESULT error: {}", e);
                break;
            }
        }
    }

    if !read_ok || signature.len() != 64 {
        // Fallback: check if key_sign_resp contains the signature directly
        if key_sign_resp.len() == 64 {
            signature = key_sign_resp;
        } else {
            warn!(
                "SSH agent: failed to read signature (got {} bytes)",
                signature.len()
            );
            send_failure(stream).await;
            return;
        }
    }

    // Build SSH ECDSA signature response.
    // Firmware uECC is built with NATIVE_LITTLE_ENDIAN=1 (immurok_keystore.c:538)
    // so sig64 from the device is two LE-encoded scalars r||s. SSH mpints are
    // big-endian, so each half must be byte-reversed before framing.
    let mut r: Vec<u8> = signature[0..32].to_vec();
    r.reverse();
    let mut s: Vec<u8> = signature[32..64].to_vec();
    s.reverse();

    // ecdsa_sig = [mpint r][mpint s]
    let mut ecdsa_sig = Vec::new();
    append_ssh_mpint(&mut ecdsa_sig, &r);
    append_ssh_mpint(&mut ecdsa_sig, &s);

    // sig_blob = [string "ecdsa-sha2-nistp256"][string ecdsa_sig]
    let mut sig_blob = Vec::new();
    append_ssh_string_str(&mut sig_blob, "ecdsa-sha2-nistp256");
    append_ssh_string(&mut sig_blob, &ecdsa_sig);

    // body = [SSH_AGENT_SIGN_RESPONSE][string sig_blob]
    let mut body = Vec::new();
    body.push(SSH_AGENT_SIGN_RESPONSE);
    append_ssh_string(&mut body, &sig_blob);

    send_agent_message(stream, &body).await;
    info!(
        "SSH agent: sign response sent ({} bytes)",
        sig_blob.len()
    );
}

// ── Wire protocol helpers ───────────────────────────────────

/// Read an SSH agent message: [uint32 length][body].
/// Returns None on EOF.
async fn read_agent_message(
    stream: &mut UnixStream,
) -> Result<Option<Vec<u8>>, std::io::Error> {
    // Read 4-byte length
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }

    let msg_len = u32::from_be_bytes(len_buf) as usize;
    if msg_len == 0 || msg_len > 256 * 1024 {
        return Ok(None); // Sanity check
    }

    let mut body = vec![0u8; msg_len];
    stream.read_exact(&mut body).await?;

    Ok(Some(body))
}

/// Send an SSH agent message: [uint32 length][body].
async fn send_agent_message(stream: &mut UnixStream, body: &[u8]) {
    let len = body.len() as u32;
    let len_buf = len.to_be_bytes();
    let _ = stream.write_all(&len_buf).await;
    let _ = stream.write_all(body).await;
    let _ = stream.flush().await;
}

/// Send SSH_AGENT_FAILURE.
async fn send_failure(stream: &mut UnixStream) {
    send_agent_message(stream, &[SSH_AGENT_FAILURE]).await;
}

// ── SSH wire format helpers ─────────────────────────────────

/// Append a big-endian u32.
fn append_u32_be(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_be_bytes());
}

/// Append an SSH string: [uint32 length][bytes].
fn append_ssh_string(buf: &mut Vec<u8>, data: &[u8]) {
    append_u32_be(buf, data.len() as u32);
    buf.extend_from_slice(data);
}

/// Append an SSH string from a &str.
fn append_ssh_string_str(buf: &mut Vec<u8>, s: &str) {
    append_ssh_string(buf, s.as_bytes());
}

/// Append an SSH mpint (big-endian integer with sign bit handling).
fn append_ssh_mpint(buf: &mut Vec<u8>, data: &[u8]) {
    if data.is_empty() {
        append_u32_be(buf, 0);
        return;
    }
    // If high bit is set, prepend 0x00 to indicate positive
    if data[0] & 0x80 != 0 {
        append_u32_be(buf, data.len() as u32 + 1);
        buf.push(0x00);
        buf.extend_from_slice(data);
    } else {
        append_u32_be(buf, data.len() as u32);
        buf.extend_from_slice(data);
    }
}

/// Read an SSH string from a buffer at a given offset. Updates offset.
fn read_ssh_string(data: &[u8], offset: &mut usize) -> Option<Vec<u8>> {
    if *offset + 4 > data.len() {
        return None;
    }
    let len = u32::from_be_bytes([
        data[*offset],
        data[*offset + 1],
        data[*offset + 2],
        data[*offset + 3],
    ]) as usize;
    *offset += 4;

    if *offset + len > data.len() {
        return None;
    }
    let result = data[*offset..*offset + len].to_vec();
    *offset += len;
    Some(result)
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ssh_string_roundtrip() {
        let mut buf = Vec::new();
        append_ssh_string(&mut buf, b"hello");

        let mut offset = 0;
        let result = read_ssh_string(&buf, &mut offset).unwrap();
        assert_eq!(result, b"hello");
        assert_eq!(offset, 9); // 4 + 5
    }

    #[test]
    fn test_ssh_mpint_positive() {
        let mut buf = Vec::new();
        // Value with high bit set → should prepend 0x00
        append_ssh_mpint(&mut buf, &[0xFF, 0x01]);
        assert_eq!(buf, &[0, 0, 0, 3, 0x00, 0xFF, 0x01]);
    }

    #[test]
    fn test_ssh_mpint_no_padding() {
        let mut buf = Vec::new();
        // Value with high bit clear → no padding
        append_ssh_mpint(&mut buf, &[0x7F, 0x01]);
        assert_eq!(buf, &[0, 0, 0, 2, 0x7F, 0x01]);
    }

    #[test]
    fn test_ssh_mpint_empty() {
        let mut buf = Vec::new();
        append_ssh_mpint(&mut buf, &[]);
        assert_eq!(buf, &[0, 0, 0, 0]);
    }

    #[test]
    fn test_read_ssh_string_eof() {
        let data = [0, 0, 0, 5, b'h', b'e']; // length says 5 but only 2 bytes
        let mut offset = 0;
        assert!(read_ssh_string(&data, &mut offset).is_none());
    }
}
