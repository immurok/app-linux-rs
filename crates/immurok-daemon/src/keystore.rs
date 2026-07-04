//! Key cache sync — syncs SSH/OTP/API keys between device and local cache.
//!
//! After BLE connection and verification, reads key entries via KEY_COUNT + KEY_READ.
//! Caches SSH public keys (for the SSH agent) and OTP/API names (for the CLI).
//!
//! SSH public key wire format (104 bytes):
//!   `\x00\x00\x00\x13ecdsa-sha2-nistp256\x00\x00\x00\x08nistp256\x00\x00\x00\x41\x04<x:32B><y:32B>`
//!
//! Fingerprint: `SHA256:<base64 of SHA256 of wire-format blob>` (no trailing `=`)

use std::path::Path;

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use immurok_common::protocol;

// ── Cache types ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshKeyCacheEntry {
    pub index: u8,
    pub name: String,
    /// SSH public key blob (104 bytes for ecdsa-sha2-nistp256), base64-encoded in JSON
    #[serde(with = "base64_bytes")]
    pub public_key_blob: Vec<u8>,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyNameEntry {
    pub index: u8,
    pub category: String, // "otp" or "api"
    pub name: String,
    /// Issuer / service (OTP only; empty for API and pre-upgrade caches).
    #[serde(default)]
    pub service: String,
}

/// Per-category cache digest. Mirrors macOS 1.2.7+ (commit 542c8cb): firmware
/// returns `[OK][count:1B][checksum:4B LE]` from KEY_COUNT; if (count,
/// checksum) match the saved digest the daemon skips the full per-entry
/// KEY_READ chain on connect / re-sync (~5–7s saved at 100+ entries).
///
/// Safety net: if the firmware reports `checksum == 0` (either old firmware
/// without the field, or genuinely empty category), the digest comparison
/// is treated as a miss — better to re-read a couple of entries than risk
/// serving stale data on an upgrade boundary.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct DigestEntry {
    pub count: u8,
    pub checksum: u32,
}

/// Serde helper for base64-encoded byte vectors.
mod base64_bytes {
    use base64::Engine;
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(data: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let b64 = base64::engine::general_purpose::STANDARD.encode(data);
        serializer.serialize_str(&b64)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(&s)
            .map_err(serde::de::Error::custom)
    }
}

// ── Public API ──────────────────────────────────────────────

/// Save SSH keys cache to a specific path (used by ble.rs sync).
pub fn save_ssh_keys_to(path: &Path, keys: &[SshKeyCacheEntry]) -> Result<(), String> {
    save_json(path, keys)
}

/// Save key names cache to a specific path (used by ble.rs sync).
pub fn save_key_names_to(path: &Path, names: &[KeyNameEntry]) -> Result<(), String> {
    save_json(path, names)
}

/// Load SSH keys from disk cache (used by ssh_agent).
pub fn load_ssh_keys(immurok_dir: &Path) -> Vec<SshKeyCacheEntry> {
    let path = immurok_dir.join(protocol::SSH_KEYS_FILE);
    load_json(&path).unwrap_or_default()
}

/// Load key names from disk cache.
pub fn load_key_names(immurok_dir: &Path) -> Vec<KeyNameEntry> {
    let path = immurok_dir.join(protocol::KEY_NAMES_FILE);
    load_json(&path).unwrap_or_default()
}

/// Load all per-category digests. Missing/unparseable → empty map (every
/// category misses → full re-read, the safe default).
pub fn load_digests(path: &Path) -> std::collections::HashMap<String, DigestEntry> {
    load_json(path).unwrap_or_default()
}

/// Atomically persist the digest map.
pub fn save_digests(
    path: &Path,
    digests: &std::collections::HashMap<String, DigestEntry>,
) -> Result<(), String> {
    save_json(path, digests)
}

// ── SSH public key wire format ──────────────────────────────

/// Build the SSH public key blob for ecdsa-sha2-nistp256.
///
/// Input: 64 bytes raw public key (x || y, big-endian).
/// Output: 104-byte SSH key blob.
pub fn build_ssh_public_key_blob(pubkey_be: &[u8]) -> Option<Vec<u8>> {
    if pubkey_be.len() != 64 {
        return None;
    }

    let mut blob = Vec::with_capacity(104);

    // string "ecdsa-sha2-nistp256" (4 + 19 = 23 bytes)
    let key_type = b"ecdsa-sha2-nistp256";
    blob.extend_from_slice(&(key_type.len() as u32).to_be_bytes());
    blob.extend_from_slice(key_type);

    // string "nistp256" (4 + 8 = 12 bytes)
    let curve_name = b"nistp256";
    blob.extend_from_slice(&(curve_name.len() as u32).to_be_bytes());
    blob.extend_from_slice(curve_name);

    // string 0x04 || x || y (4 + 65 = 69 bytes)
    let point_len: u32 = 65; // 1 + 32 + 32
    blob.extend_from_slice(&point_len.to_be_bytes());
    blob.push(0x04); // uncompressed point
    blob.extend_from_slice(pubkey_be);

    Some(blob)
}

/// Compute SSH fingerprint: `SHA256:<base64-no-padding>`.
pub fn compute_fingerprint(blob: &[u8]) -> String {
    let hash = Sha256::digest(blob);
    let b64 = base64::engine::general_purpose::STANDARD.encode(hash);
    let trimmed = b64.trim_end_matches('=');
    format!("SHA256:{}", trimmed)
}

/// Convert a 64-byte key from little-endian (device format) to big-endian (SSH format).
/// The key is two 32-byte integers (x, y). Each needs byte-reversal.
pub fn convert_endianness_64(le_data: &[u8]) -> Vec<u8> {
    if le_data.len() != 64 {
        return le_data.to_vec();
    }
    let mut be = vec![0u8; 64];
    // Reverse x (bytes 0..32)
    for i in 0..32 {
        be[i] = le_data[31 - i];
    }
    // Reverse y (bytes 32..64)
    for i in 0..32 {
        be[32 + i] = le_data[63 - i];
    }
    be
}

// ── JSON persistence helpers ────────────────────────────────

fn save_json<T: Serialize + ?Sized>(path: &Path, data: &T) -> Result<(), String> {
    let json =
        serde_json::to_string_pretty(data).map_err(|e| format!("JSON serialize: {}", e))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &json).map_err(|e| format!("Write {}: {}", tmp.display(), e))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| format!("Rename {} -> {}: {}", tmp.display(), path.display(), e))?;
    Ok(())
}

fn load_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_ssh_public_key_blob_length() {
        let pubkey = [0x42u8; 64];
        let blob = build_ssh_public_key_blob(&pubkey).expect("should build blob");
        // 4+19 + 4+8 + 4+65 = 104
        assert_eq!(blob.len(), 104);
    }

    #[test]
    fn test_build_ssh_public_key_blob_structure() {
        let mut pubkey = [0u8; 64];
        for i in 0..64 {
            pubkey[i] = i as u8;
        }
        let blob = build_ssh_public_key_blob(&pubkey).unwrap();

        // First 4 bytes: length of "ecdsa-sha2-nistp256" = 19
        assert_eq!(&blob[0..4], &[0, 0, 0, 19]);
        // bytes 4..23: "ecdsa-sha2-nistp256"
        assert_eq!(&blob[4..23], b"ecdsa-sha2-nistp256");
        // bytes 23..27: length of "nistp256" = 8
        assert_eq!(&blob[23..27], &[0, 0, 0, 8]);
        // bytes 27..35: "nistp256"
        assert_eq!(&blob[27..35], b"nistp256");
        // bytes 35..39: length of point = 65
        assert_eq!(&blob[35..39], &[0, 0, 0, 65]);
        // byte 39: 0x04 (uncompressed)
        assert_eq!(blob[39], 0x04);
        // bytes 40..104: x || y
        assert_eq!(&blob[40..104], &pubkey);
    }

    #[test]
    fn test_fingerprint_format() {
        let pubkey = [0x42u8; 64];
        let blob = build_ssh_public_key_blob(&pubkey).unwrap();
        let fp = compute_fingerprint(&blob);
        assert!(fp.starts_with("SHA256:"));
        // No trailing '=' characters
        assert!(!fp.ends_with('='));
    }

    #[test]
    fn test_convert_endianness_64() {
        let mut le = [0u8; 64];
        // x = 0x00010203...1F (little-endian)
        for i in 0..32 {
            le[i] = i as u8;
        }
        // y = 0x20212223...3F (little-endian)
        for i in 0..32 {
            le[32 + i] = (32 + i) as u8;
        }

        let be = convert_endianness_64(&le);
        assert_eq!(be.len(), 64);

        // x BE: reversed 0..32
        assert_eq!(be[0], 31);
        assert_eq!(be[31], 0);
        // y BE: reversed 32..64
        assert_eq!(be[32], 63);
        assert_eq!(be[63], 32);
    }

    #[test]
    fn test_invalid_pubkey_length() {
        assert!(build_ssh_public_key_blob(&[0u8; 63]).is_none());
        assert!(build_ssh_public_key_blob(&[0u8; 65]).is_none());
    }
}
