//! HTTP layer — manifest fetch and package download (blocking, ureq).

use std::io::Read;
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::error::FwUpdateError;

pub const DEFAULT_MANIFEST_URL: &str = "https://immurok.com/fw/manifest.json";
/// Hard cap on download size — .imfw is ≤216KB + header; 4MB is generous.
const MAX_DOWNLOAD_BYTES: u64 = 4 * 1024 * 1024;

/// Env override for testing against a local server.
pub fn manifest_url() -> String {
    std::env::var("IMMUROK_FW_MANIFEST_URL").unwrap_or_else(|_| DEFAULT_MANIFEST_URL.into())
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

pub fn fetch_manifest_body() -> Result<String, FwUpdateError> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build();
    agent
        .get(&manifest_url())
        .call()
        .map_err(|e| FwUpdateError::ManifestFetch(e.to_string()))?
        .into_string()
        .map_err(|e| FwUpdateError::ManifestFetch(e.to_string()))
}

/// Download a package and verify its sha256 against the manifest value.
pub fn download(url: &str, expected_sha256: &str) -> Result<Vec<u8>, FwUpdateError> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .build();
    let resp = agent
        .get(url)
        .call()
        .map_err(|e| FwUpdateError::Download(e.to_string()))?;
    let mut data = Vec::new();
    resp.into_reader()
        .take(MAX_DOWNLOAD_BYTES)
        .read_to_end(&mut data)
        .map_err(|e| FwUpdateError::Download(e.to_string()))?;
    if !sha256_hex(&data).eq_ignore_ascii_case(expected_sha256) {
        return Err(FwUpdateError::Sha256Mismatch);
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_known_vector() {
        // sha256("abc") — FIPS 180-2 test vector
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
