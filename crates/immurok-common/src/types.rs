use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingData {
    pub device_uuid: String,
    #[serde(with = "hex_key")]
    pub shared_key: [u8; 32],
    pub paired_at: String,
}

mod hex_key {
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(key: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer {
        serializer.serialize_str(&hex::encode(key))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 32], D::Error>
    where D: Deserializer<'de> {
        let s = String::deserialize(deserializer)?;
        let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;
        let mut arr = [0u8; 32];
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom("expected 32 bytes"));
        }
        arr.copy_from_slice(&bytes);
        Ok(arr)
    }
}

#[derive(Debug, Clone, Default)]
pub struct DeviceStatus {
    pub fp_bitmap: u8,
    pub paired: bool,
    pub battery: u8,
    pub fw_version: String,
    pub pending_match: Option<PendingMatch>,
}

#[derive(Debug, Clone)]
pub struct PendingMatch {
    pub page_id: u16,
    pub hmac: [u8; 8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum KeyCategory {
    Ssh = 0,
    Otp = 1,
    Api = 2,
}

impl KeyCategory {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Ssh),
            1 => Some(Self::Otp),
            2 => Some(Self::Api),
            _ => None,
        }
    }

    // Pre-existing public API, kept for compatibility (not std::str::FromStr).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "ssh" => Some(Self::Ssh),
            "otp" => Some(Self::Otp),
            "api" => Some(Self::Api),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollEvent {
    Waiting,
    Captured { current: u8, total: u8 },
    Processing,
    LiftFinger,
    Complete,
    Failed,
}

impl EnrollEvent {
    pub fn from_notification(status: u8, current: u8, total: u8) -> Self {
        match status {
            0x00 => Self::Waiting,
            0x01 => Self::Captured { current, total },
            0x02 => Self::Processing,
            0x03 => Self::LiftFinger,
            0x04 => Self::Complete,
            _ => Self::Failed,
        }
    }
}

/// Helper to check fingerprint bitmap
pub fn fp_bitmap_slots(bitmap: u8) -> Vec<u8> {
    (0..5).filter(|i| bitmap & (1 << i) != 0).collect()
}

/// Helper to format fingerprint bitmap for display
pub fn fp_bitmap_display(bitmap: u8) -> String {
    (0..5)
        .map(|i| if bitmap & (1 << i) != 0 { "[■]" } else { "[ ]" })
        .collect::<Vec<_>>()
        .join(" ")
}
