//! Key store access: list from the daemon's cache, read secrets through the
//! device's fingerprint gate.
//!
//! Wire formats (`immurok-daemon/src/socket.rs`):
//!   KEY:CACHE:ssh    → `OK:[{"index":0,"name":"…","fingerprint":"…","public_key_blob":"<b64>"}]`
//!   KEY:CACHE:names  → `OK:[{"index":0,"category":"otp"|"api","name":"…","service":"…"}]`
//!   GET:otp:<name>   → `OK:123456` after a fingerprint touch (30 s gate)
//!   GET:api:<name>   → `OK:<value>` after a fingerprint touch
//!   KEY:DELETE:<cat>:<idx> → `OK:DELETED`
//!   GATE:CANCEL      → `OK:…`

use std::time::Duration;

use crate::keys_import::{build_key_add_cmd, build_otp_entry_payload, KeyAddCat, OtpEntry};
use crate::ssh_import::{build_ssh_import_payload, build_ssh_name_payload};
use crate::{fetch_key_cache, DaemonClient};
use immurok_common::protocol::{KEY_MAX_API, KEY_MAX_OTP, KEY_MAX_SSH, SECRET_LEN_OTP, VALUE_LEN_API};

/// Fingerprint gate is 30 s on the device; leave room for the BLE round trip.
pub const GATE_TIMEOUT: Duration = Duration::from_secs(40);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCategory {
    Ssh,
    Otp,
    Api,
}

impl KeyCategory {
    pub fn label(self) -> &'static str {
        match self {
            KeyCategory::Ssh => "SSH",
            KeyCategory::Otp => "OTP",
            KeyCategory::Api => "API",
        }
    }

    pub fn wire(self) -> &'static str {
        match self {
            KeyCategory::Ssh => "ssh",
            KeyCategory::Otp => "otp",
            KeyCategory::Api => "api",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEntry {
    pub index: u8,
    pub category: KeyCategory,
    pub name: String,
    /// Issuer / service (OTP only; empty otherwise).
    pub service: String,
    /// Base64 public key blob (SSH only; empty otherwise).
    pub ssh_pubkey_b64: String,
}

pub fn parse_key_cache(ssh: &[serde_json::Value], names: &[serde_json::Value]) -> Vec<KeyEntry> {
    let mut otp = Vec::new();
    let mut api = Vec::new();
    for e in names {
        let category = match e["category"].as_str() {
            Some("otp") => KeyCategory::Otp,
            Some("api") => KeyCategory::Api,
            _ => continue,
        };
        let entry = KeyEntry {
            index: e["index"].as_u64().unwrap_or(0) as u8,
            category,
            name: e["name"].as_str().unwrap_or("-").to_string(),
            service: e["service"].as_str().unwrap_or("").to_string(),
            ssh_pubkey_b64: String::new(),
        };
        match category {
            KeyCategory::Otp => otp.push(entry),
            _ => api.push(entry),
        }
    }
    let mut sshv: Vec<KeyEntry> = ssh
        .iter()
        .map(|e| KeyEntry {
            index: e["index"].as_u64().unwrap_or(0) as u8,
            category: KeyCategory::Ssh,
            name: e["name"].as_str().unwrap_or("-").to_string(),
            service: String::new(),
            ssh_pubkey_b64: e["public_key_blob"].as_str().unwrap_or("").to_string(),
        })
        .collect();
    otp.sort_by_key(|e| e.index);
    api.sort_by_key(|e| e.index);
    sshv.sort_by_key(|e| e.index);
    otp.extend(api);
    otp.extend(sshv);
    otp
}

/// Everything the daemon has cached. No device round-trip; works offline.
pub fn list_keys() -> Vec<KeyEntry> {
    let ssh = fetch_key_cache("ssh");
    let names = fetch_key_cache("names");
    parse_key_cache(&ssh, &names)
}

fn parse_secret_reply(rsp: &str) -> Result<String, String> {
    match rsp.trim().strip_prefix("OK:") {
        Some(v) => Ok(v.to_string()),
        None => Err(rsp.trim().to_string()),
    }
}

fn get_gated(cat: KeyCategory, name: &str) -> Result<String, String> {
    let cmd = format!("GET:{}:{}", cat.wire(), name);
    let rsp = DaemonClient::connect()?.send_with_timeout(&cmd, GATE_TIMEOUT)?;
    parse_secret_reply(&rsp)
}

/// Six-digit TOTP for the named OTP entry. Blocks until the user touches the
/// sensor or the gate times out.
pub fn get_otp(name: &str) -> Result<String, String> {
    get_gated(KeyCategory::Otp, name)
}

/// Stored API value for the named entry. Same gate as OTP.
pub fn get_api(name: &str) -> Result<String, String> {
    get_gated(KeyCategory::Api, name)
}

pub fn ssh_public_key_line(entry: &KeyEntry) -> String {
    format!("ecdsa-sha2-nistp256 {} {}", entry.ssh_pubkey_b64, entry.name)
}

/// Best effort: abort a pending fingerprint gate. Errors are ignored — if the
/// daemon is gone there is nothing left to cancel.
pub fn cancel_gate() {
    if let Ok(mut c) = DaemonClient::connect() {
        let _ = c.send("GATE:CANCEL");
    }
}

/// Firmware slot limit per category.
pub fn capacity(cat: KeyCategory) -> u8 {
    match cat {
        KeyCategory::Ssh => KEY_MAX_SSH,
        KeyCategory::Otp => KEY_MAX_OTP,
        KeyCategory::Api => KEY_MAX_API,
    }
}

fn parse_write_reply(rsp: &str) -> Result<(), String> {
    let rsp = rsp.trim();
    if rsp.starts_with("OK") {
        Ok(())
    } else {
        Err(rsp.to_string())
    }
}

fn send_write(cmd: &str) -> Result<(), String> {
    let rsp = DaemonClient::connect()?.send_with_timeout(cmd, GATE_TIMEOUT)?;
    parse_write_reply(&rsp)
}

/// Generate an ECDSA P-256 keypair on the device (`KEY:GENERATE`). The
/// name is cut to 15 bytes. Blocks on the fingerprint gate.
pub fn generate_ssh(name: &str) -> Result<(), String> {
    send_write(&format!("KEY:GENERATE:{}", hex::encode(build_ssh_name_payload(name))))
}

/// Import an existing P-256 private key (OpenSSH or SEC1 PEM text).
pub fn import_ssh(name: &str, pem_text: &str) -> Result<(), String> {
    let payload = build_ssh_import_payload(name, pem_text)?;
    send_write(&format!("KEY:IMPORT:{}", hex::encode(payload)))
}

/// Add one OTP entry. `entry.secret` is the decoded base32 secret.
pub fn add_otp(entry: &OtpEntry) -> Result<(), String> {
    if entry.secret.is_empty() {
        return Err("Secret cannot be empty.".into());
    }
    if entry.secret.len() > SECRET_LEN_OTP {
        return Err(format!("Secret too long: {} bytes decoded (device limit {}).", entry.secret.len(), SECRET_LEN_OTP));
    }
    send_write(&format!("KEY:OTP_IMPORT:{}", hex::encode(build_otp_entry_payload(entry))))
}

/// Add one API entry.
pub fn add_api(name: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("Value cannot be empty.".into());
    }
    if value.len() > VALUE_LEN_API {
        return Err(format!("Value too long: {} bytes (device limit {}).", value.len(), VALUE_LEN_API));
    }
    send_write(&build_key_add_cmd(KeyAddCat::Api, name, "", value.as_bytes()))
}

pub fn delete_key(category: KeyCategory, index: u8) -> Result<(), String> {
    let cmd = format!("KEY:DELETE:{}:{}", category.wire(), index);
    let rsp = DaemonClient::connect()?.send_with_timeout(&cmd, GATE_TIMEOUT)?;
    if rsp.starts_with("OK") {
        Ok(())
    } else {
        Err(rsp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merges_caches_otp_api_ssh_in_that_order() {
        let ssh = vec![json!({"index": 2, "name": "work", "fingerprint": "SHA256:x", "public_key_blob": "QUJD"})];
        let names = vec![
            json!({"index": 1, "category": "api", "name": "openai", "service": ""}),
            json!({"index": 3, "category": "otp", "name": "github", "service": "GitHub"}),
            json!({"index": 0, "category": "otp", "name": "aws", "service": "Amazon"}),
        ];
        let out = parse_key_cache(&ssh, &names);
        let names_in_order: Vec<&str> = out.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names_in_order, ["aws", "github", "openai", "work"]);
        assert_eq!(out[0].category, KeyCategory::Otp);
        assert_eq!(out[0].service, "Amazon");
        assert_eq!(out[3].category, KeyCategory::Ssh);
        assert_eq!(out[3].ssh_pubkey_b64, "QUJD");
    }

    #[test]
    fn skips_unknown_categories() {
        let names = vec![json!({"index": 0, "category": "weird", "name": "x", "service": ""})];
        assert!(parse_key_cache(&[], &names).is_empty());
    }

    #[test]
    fn ssh_line_is_authorized_keys_format() {
        let e = KeyEntry {
            index: 0,
            category: KeyCategory::Ssh,
            name: "work".into(),
            service: String::new(),
            ssh_pubkey_b64: "QUJD".into(),
        };
        assert_eq!(ssh_public_key_line(&e), "ecdsa-sha2-nistp256 QUJD work");
    }

    #[test]
    fn otp_reply_parsing() {
        assert_eq!(parse_secret_reply("OK:123456"), Ok("123456".to_string()));
        assert_eq!(parse_secret_reply("ERROR:BUSY"), Err("ERROR:BUSY".to_string()));
        assert_eq!(parse_secret_reply("DENY:GATE_TIMEOUT"), Err("DENY:GATE_TIMEOUT".to_string()));
    }

    #[test]
    fn category_wire_names() {
        assert_eq!(KeyCategory::Otp.wire(), "otp");
        assert_eq!(KeyCategory::Api.wire(), "api");
        assert_eq!(KeyCategory::Ssh.wire(), "ssh");
    }

    #[test]
    fn capacity_matches_protocol_limits() {
        use immurok_common::protocol::{KEY_MAX_API, KEY_MAX_OTP, KEY_MAX_SSH};
        assert_eq!(capacity(KeyCategory::Ssh), KEY_MAX_SSH);
        assert_eq!(capacity(KeyCategory::Otp), KEY_MAX_OTP);
        assert_eq!(capacity(KeyCategory::Api), KEY_MAX_API);
    }

    #[test]
    fn write_reply_ok_prefix_only() {
        assert_eq!(parse_write_reply("OK:GENERATED"), Ok(()));
        assert_eq!(parse_write_reply("OK"), Ok(()));
        assert_eq!(parse_write_reply("ERROR:KEYSTORE_FULL"), Err("ERROR:KEYSTORE_FULL".to_string()));
    }
}
