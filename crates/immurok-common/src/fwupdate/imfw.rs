//! .imfw package parsing + preflight validation.
//! Ported from FirmwareUpdateKit/IMFWPackage.swift (canonical layout in
//! ota/ota-package.py). v1 = 96B header (HMAC, ≤1.5.x); v2 = 128B (ECDSA).
//! Header (LE): <I magic, B version, B flags, H hw_id, I fw_size>,
//! v2 adds sec_version u16 @0x0C.

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ImfwError {
    #[error("file truncated")]
    Truncated,
    #[error("bad magic (not an .imfw package)")]
    BadMagic,
    #[error("unsupported format version {0}")]
    UnsupportedVersion(u8),
    #[error("payload shorter than declared fw_size")]
    SizeMismatch,
    #[error("firmware exceeds Image B size (216KB)")]
    TooLarge,
}

pub const IMFW_MAGIC: u32 = 0x494D4657; // "IMFW"
pub const HEADER_SIZE_V1: usize = 96;
pub const HEADER_SIZE_V2: usize = 128;
pub const IMAGE_B_SIZE: usize = 216 * 1024;
/// BLE OTA write payload ≤243B and 16B-aligned → 240 is the largest fit.
pub const CHUNK_SIZE: usize = 240;

#[derive(Debug)]
pub struct ImfwPackage<'a> {
    pub header: &'a [u8],
    pub firmware: &'a [u8], // encrypted body
    pub format_version: u8,
    pub hw_id: u16,
    pub fw_size: u32, // plaintext size declared in header
    pub sec_version: Option<u16>, // v2 only
}

pub fn parse(data: &[u8]) -> Result<ImfwPackage<'_>, ImfwError> {
    if data.len() < HEADER_SIZE_V1 {
        return Err(ImfwError::Truncated);
    }
    let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if magic != IMFW_MAGIC {
        return Err(ImfwError::BadMagic);
    }
    let version = data[4];
    if version != 1 && version != 2 {
        return Err(ImfwError::UnsupportedVersion(version));
    }
    let header_size = if version >= 2 { HEADER_SIZE_V2 } else { HEADER_SIZE_V1 };
    if data.len() < header_size {
        return Err(ImfwError::Truncated);
    }

    let hw_id = u16::from_le_bytes([data[6], data[7]]);
    let fw_size = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    let sec_version = if version >= 2 {
        Some(u16::from_le_bytes([data[0x0C], data[0x0D]]))
    } else {
        None
    };
    let firmware = &data[header_size..];

    if fw_size as usize > IMAGE_B_SIZE || firmware.len() > IMAGE_B_SIZE {
        return Err(ImfwError::TooLarge);
    }
    // Ciphertext may exceed fw_size by up to one AES block, never undershoot.
    // fw_size == 0 (no payload) is equally invalid.
    if fw_size == 0 || firmware.len() < fw_size as usize {
        return Err(ImfwError::SizeMismatch);
    }

    Ok(ImfwPackage {
        header: &data[..header_size],
        firmware,
        format_version: version,
        hw_id,
        fw_size,
        sec_version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic .imfw: header (v1 96B / v2 128B) + body.
    fn make(version: u8, fw_size: u32, body_len: usize) -> Vec<u8> {
        let header_size = if version >= 2 { HEADER_SIZE_V2 } else { HEADER_SIZE_V1 };
        let mut d = vec![0u8; header_size + body_len];
        d[0..4].copy_from_slice(&IMFW_MAGIC.to_le_bytes());
        d[4] = version;
        d[5] = 0; // flags
        d[6..8].copy_from_slice(&0x0001u16.to_le_bytes()); // hw_id
        d[8..12].copy_from_slice(&fw_size.to_le_bytes());
        if version >= 2 {
            d[0x0C..0x0E].copy_from_slice(&7u16.to_le_bytes()); // sec_version
        }
        d
    }

    #[test]
    fn parse_v1() {
        let data = make(1, 1024, 1024);
        let p = parse(&data).unwrap();
        assert_eq!(p.format_version, 1);
        assert_eq!(p.hw_id, 0x0001);
        assert_eq!(p.fw_size, 1024);
        assert_eq!(p.sec_version, None);
        assert_eq!(p.header.len(), HEADER_SIZE_V1);
        assert_eq!(p.firmware.len(), 1024);
    }

    #[test]
    fn parse_v2_has_sec_version() {
        let data = make(2, 1024, 1024);
        let p = parse(&data).unwrap();
        assert_eq!(p.format_version, 2);
        assert_eq!(p.sec_version, Some(7));
        assert_eq!(p.header.len(), HEADER_SIZE_V2);
    }

    #[test]
    fn bad_magic() {
        let mut data = make(1, 16, 16);
        data[0] = 0xFF;
        assert_eq!(parse(&data).unwrap_err(), ImfwError::BadMagic);
    }

    #[test]
    fn truncated() {
        assert_eq!(parse(&[0u8; 10]).unwrap_err(), ImfwError::Truncated);
        // v2 declared but only 96 bytes present
        let mut d = make(1, 0, 0);
        d[4] = 2;
        assert_eq!(parse(&d).unwrap_err(), ImfwError::Truncated);
    }

    #[test]
    fn unsupported_version() {
        let mut data = make(1, 16, 16);
        data[4] = 3;
        assert_eq!(parse(&data).unwrap_err(), ImfwError::UnsupportedVersion(3));
    }

    #[test]
    fn too_large() {
        // fw_size beyond Image B
        let data = make(2, (IMAGE_B_SIZE as u32) + 1, 16);
        assert_eq!(parse(&data).unwrap_err(), ImfwError::TooLarge);
    }

    #[test]
    fn size_mismatch() {
        // ciphertext may be slightly larger than fw_size (16B align) but never smaller
        let data = make(2, 1024, 512);
        assert_eq!(parse(&data).unwrap_err(), ImfwError::SizeMismatch);
        // fw_size == 0 (empty payload) is also invalid
        let data = make(2, 0, 0);
        assert_eq!(parse(&data).unwrap_err(), ImfwError::SizeMismatch);
    }

    #[test]
    fn aligned_slack_ok() {
        // body 16B larger than fw_size → OK
        let data = make(2, 1008, 1024);
        assert!(parse(&data).is_ok());
    }
}
