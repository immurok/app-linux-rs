//! Security module — ECDH P-256, HKDF-SHA256, HMAC, pairing persistence.
//!
//! All cryptographic parameters match firmware + docs/security.md:
//!   - ECDH P-256 ephemeral keypair
//!   - HKDF-SHA256 (Salt="immurok-pairing-salt", Info="immurok-shared-key")
//!   - HMAC-SHA256 truncated to 8 bytes for FP match notifications
//!   - Pairing data persisted to ~/.immurok/pairing.json (mode 0o600)

use std::fs;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use p256::ecdh::EphemeralSecret;
use p256::elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint};
use p256::{EncodedPoint, PublicKey};
use rand::rngs::OsRng;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use thiserror::Error;

use crate::protocol::{HKDF_INFO, HKDF_SALT, HMAC_TRUNCATED_LEN, IMMUROK_DIR, PAIRING_FILE};
use crate::types::PairingData;

type HmacSha256 = Hmac<Sha256>;

// ── Error type ────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum SecurityError {
    #[error("invalid peer public key: {0}")]
    InvalidPeerKey(String),
    #[error("HKDF expansion failed")]
    HkdfError,
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

// ── ECDH P-256 ────────────────────────────────────────────────

/// Generate an ephemeral P-256 keypair.
/// Returns `(secret, compressed_pubkey_33B)`.
pub fn generate_p256_keypair() -> (EphemeralSecret, [u8; 33]) {
    let secret = EphemeralSecret::random(&mut OsRng);
    let pubkey = secret.public_key();
    let encoded = pubkey.to_encoded_point(true); // compressed
    let bytes = encoded.as_bytes();
    let mut out = [0u8; 33];
    out.copy_from_slice(bytes);
    (secret, out)
}

/// Compute ECDH shared secret (32 bytes, big-endian x-coordinate).
/// Consumes `secret` (required by `EphemeralSecret::diffie_hellman`).
pub fn ecdh_shared_secret(
    secret: EphemeralSecret,
    peer_compressed_pubkey: &[u8],
) -> Result<[u8; 32], SecurityError> {
    let encoded = EncodedPoint::from_bytes(peer_compressed_pubkey)
        .map_err(|e| SecurityError::InvalidPeerKey(e.to_string()))?;
    let peer_pub = PublicKey::from_encoded_point(&encoded)
        .into_option()
        .ok_or_else(|| SecurityError::InvalidPeerKey("not on curve".into()))?;
    let shared = secret.diffie_hellman(&peer_pub);
    let raw = shared.raw_secret_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(raw.as_ref());
    Ok(out)
}

// ── HKDF-SHA256 ───────────────────────────────────────────────

/// Derive `shared_key` (32 bytes) from ECDH secret via HKDF-SHA256.
///
/// Salt = "immurok-pairing-salt" (20 B)
/// Info = "immurok-shared-key"  (18 B)
pub fn derive_shared_key(ecdh_secret: &[u8; 32]) -> Result<[u8; 32], SecurityError> {
    let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT), ecdh_secret);
    let mut okm = [0u8; 32];
    hk.expand(HKDF_INFO, &mut okm)
        .map_err(|_| SecurityError::HkdfError)?;
    Ok(okm)
}

// ── HMAC utilities ────────────────────────────────────────────

/// Compute HMAC-SHA256(key, data), return full 32-byte digest.
fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    let result = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

/// Compute HMAC-SHA256(key, data) truncated to 8 bytes.
pub fn hmac_truncated(key: &[u8], data: &[u8]) -> [u8; 8] {
    let full = hmac_sha256(key, data);
    let mut out = [0u8; 8];
    out.copy_from_slice(&full[..HMAC_TRUNCATED_LEN]);
    out
}

// ── Fingerprint match verification (0x21 notification) ────────

/// Verify a signed fingerprint match notification.
///
/// message = 0x21 || page_id (2 bytes LE)   [3 bytes total]
/// hmac    = HMAC-SHA256(shared_key, message)[0:8]
pub fn verify_fp_match_signed(key: &[u8; 32], page_id: u16, received_hmac: &[u8; 8]) -> bool {
    let mut msg = [0u8; 3];
    msg[0] = 0x21;
    msg[1..3].copy_from_slice(&page_id.to_le_bytes());
    let expected = hmac_truncated(key, &msg);
    expected.ct_eq(received_hmac).into()
}

/// Compute the HMAC for a FP match notification (for testing / firmware-side simulation).
pub fn compute_fp_match_hmac(key: &[u8; 32], page_id: u16) -> [u8; 8] {
    let mut msg = [0u8; 3];
    msg[0] = 0x21;
    msg[1..3].copy_from_slice(&page_id.to_le_bytes());
    hmac_truncated(key, &msg)
}

// ── Challenge-Response ────────────────────────────────────────

/// Compute HMAC-SHA256(key, nonce) truncated to 8 bytes.
/// Used for CMD_CHALLENGE / response verification.
pub fn compute_challenge_response(key: &[u8; 32], nonce: &[u8]) -> [u8; 8] {
    hmac_truncated(key, nonce)
}

/// Verify a challenge response.
pub fn verify_challenge_response(
    key: &[u8; 32],
    nonce: &[u8],
    received: &[u8; 8],
) -> bool {
    let expected = compute_challenge_response(key, nonce);
    expected.ct_eq(received).into()
}

// ── Factory reset HMAC ────────────────────────────────────────

/// Compute full 32-byte HMAC for factory reset.
/// Input: "factory-reset" (13 bytes)
pub fn compute_reset_hmac(key: &[u8; 32]) -> [u8; 32] {
    hmac_sha256(key, b"factory-reset")
}

// ── Pairing data persistence ──────────────────────────────────

/// Resolve `~/.immurok/pairing.json`.
fn pairing_path() -> Result<PathBuf, SecurityError> {
    let home = dirs_home()?;
    Ok(home.join(IMMUROK_DIR).join(PAIRING_FILE))
}

fn dirs_home() -> Result<PathBuf, SecurityError> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| SecurityError::Io(io::Error::new(io::ErrorKind::NotFound, "HOME not set")))
}

/// Save `PairingData` to `~/.immurok/pairing.json` with mode 0o600.
/// Uses a temporary file + rename for atomicity.
pub fn save_pairing(data: &PairingData) -> Result<(), SecurityError> {
    let path = pairing_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(data)?;

    // Write to a temp file then rename for atomicity
    let tmp_path = path.with_extension("tmp");
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)?;
        use std::io::Write;
        file.write_all(json.as_bytes())?;
        file.flush()?;
    }
    fs::rename(&tmp_path, &path)?;

    // Ensure the final file has 0o600 (rename may inherit parent umask on some systems)
    fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;

    Ok(())
}

/// Load `PairingData` from `~/.immurok/pairing.json`.
/// Returns `None` if the file does not exist or is malformed.
pub fn load_pairing() -> Result<Option<PairingData>, SecurityError> {
    let path = pairing_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(&path)?;
    match serde_json::from_str(&contents) {
        Ok(data) => Ok(Some(data)),
        Err(_) => Ok(None),
    }
}

/// Delete `~/.immurok/pairing.json`.
/// Returns `true` if the file existed and was removed.
pub fn clear_pairing() -> Result<bool, SecurityError> {
    let path = pairing_path()?;
    if path.exists() {
        fs::remove_file(&path)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Save pairing data to an arbitrary path (used in tests).
pub fn save_pairing_to(data: &PairingData, path: &Path) -> Result<(), SecurityError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(data)?;
    let tmp_path = path.with_extension("tmp");
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)?;
        use std::io::Write;
        file.write_all(json.as_bytes())?;
        file.flush()?;
    }
    fs::rename(&tmp_path, path)?;
    fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok(())
}

/// Load pairing data from an arbitrary path (used in tests).
pub fn load_pairing_from(path: &Path) -> Result<Option<PairingData>, SecurityError> {
    if !path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(path)?;
    match serde_json::from_str(&contents) {
        Ok(data) => Ok(Some(data)),
        Err(_) => Ok(None),
    }
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_hkdf_derive_deterministic() {
        let secret = [0x42u8; 32];
        let key1 = derive_shared_key(&secret).expect("HKDF failed");
        let key2 = derive_shared_key(&secret).expect("HKDF failed");
        assert_eq!(key1, key2, "same input must yield same output");
        assert_ne!(key1, [0u8; 32], "output must be non-zero");
    }

    #[test]
    fn test_hmac_truncated_length() {
        let key = [0xAAu8; 32];
        let data = b"test data";
        let result = hmac_truncated(&key, data);
        assert_eq!(result.len(), 8, "truncated HMAC must be exactly 8 bytes");
    }

    #[test]
    fn test_verify_fp_match() {
        let key = [0x55u8; 32];
        let page_id: u16 = 3;

        // Compute correct HMAC
        let good_hmac = compute_fp_match_hmac(&key, page_id);
        assert!(
            verify_fp_match_signed(&key, page_id, &good_hmac),
            "correct HMAC must pass"
        );

        // Tampered HMAC must fail
        let mut bad_hmac = good_hmac;
        bad_hmac[0] ^= 0xFF;
        assert!(
            !verify_fp_match_signed(&key, page_id, &bad_hmac),
            "wrong HMAC must fail"
        );

        // Different page_id must fail
        assert!(
            !verify_fp_match_signed(&key, page_id + 1, &good_hmac),
            "wrong page_id must fail"
        );
    }

    #[test]
    fn test_challenge_roundtrip() {
        let key = [0x99u8; 32];
        let nonce = b"random-nonce-16b";

        let response = compute_challenge_response(&key, nonce);
        assert!(
            verify_challenge_response(&key, nonce, &response),
            "correct nonce must verify"
        );

        // Wrong nonce must fail
        let wrong_nonce = b"wrong-nonce-16b!";
        assert!(
            !verify_challenge_response(&key, wrong_nonce, &response),
            "wrong nonce must fail"
        );
    }

    #[test]
    fn test_pairing_save_load_roundtrip() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("pairing.json");

        let original = PairingData {
            device_uuid: "test-device-uuid-1234".to_string(),
            shared_key: [0xBBu8; 32],
            paired_at: "2024-01-01T00:00:00Z".to_string(),
        };

        save_pairing_to(&original, &path).expect("save failed");

        // File must exist with restrictive permissions
        let meta = fs::metadata(&path).expect("metadata");
        use std::os::unix::fs::MetadataExt;
        assert_eq!(meta.mode() & 0o777, 0o600, "file permissions must be 0o600");

        let loaded = load_pairing_from(&path)
            .expect("load failed")
            .expect("data must be present");

        assert_eq!(loaded.device_uuid, original.device_uuid);
        assert_eq!(loaded.shared_key, original.shared_key);
        assert_eq!(loaded.paired_at, original.paired_at);
    }
}
