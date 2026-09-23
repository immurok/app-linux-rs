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
    /// Frame too similar to the previous one; progress does not advance.
    Overlap,
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
            0x06 => Self::Overlap,
            _ => Self::Failed,
        }
    }
}

/// Pairing progress, polled over `PAIR:PROGRESS`.
///
/// The daemon serves exactly one request per connection, so progress cannot
/// be streamed on the connection that is blocked inside PAIR:START — the
/// client opens a second connection and polls, same shape as FP:STATUS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PairProgress {
    #[default]
    Idle,
    /// Second-host enrollment: the device wants an already-enrolled finger.
    WaitFp,
    /// Waiting for the physical button press.
    WaitButton,
    /// Button pressed; the device is running ECDH.
    Ecdh,
    Done,
    Failed,
}

impl PairProgress {
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Idle => "IDLE",
            Self::WaitFp => "WAIT_FP",
            Self::WaitButton => "WAIT_BUTTON",
            Self::Ecdh => "ECDH",
            Self::Done => "DONE",
            Self::Failed => "FAILED",
        }
    }

    pub fn from_wire(s: &str) -> Option<Self> {
        Some(match s {
            "IDLE" => Self::Idle,
            "WAIT_FP" => Self::WaitFp,
            "WAIT_BUTTON" => Self::WaitButton,
            "ECDH" => Self::Ecdh,
            "DONE" => Self::Done,
            "FAILED" => Self::Failed,
            _ => return None,
        })
    }
}

/// Enrolled slot indices. Covers all 6 physical slots — index 5 is the
/// host-switch finger, which callers distinguish via
/// `protocol::SWITCH_FINGER_SLOT`.
pub fn fp_bitmap_slots(bitmap: u8) -> Vec<u8> {
    (0..crate::protocol::TOTAL_FINGERPRINT_SLOTS)
        .filter(|i| bitmap & (1 << i) != 0)
        .collect()
}

/// Helper to format fingerprint bitmap for display
pub fn fp_bitmap_display(bitmap: u8) -> String {
    (0..crate::protocol::TOTAL_FINGERPRINT_SLOTS)
        .map(|i| if bitmap & (1 << i) != 0 { "[■]" } else { "[ ]" })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_progress_wire_names_are_stable() {
        // The CLI matches on these strings to drive the two-step guide.
        assert_eq!(PairProgress::Idle.as_wire(), "IDLE");
        assert_eq!(PairProgress::WaitFp.as_wire(), "WAIT_FP");
        assert_eq!(PairProgress::WaitButton.as_wire(), "WAIT_BUTTON");
        assert_eq!(PairProgress::Ecdh.as_wire(), "ECDH");
        assert_eq!(PairProgress::Done.as_wire(), "DONE");
        assert_eq!(PairProgress::Failed.as_wire(), "FAILED");
        assert_eq!(PairProgress::default(), PairProgress::Idle);
    }

    #[test]
    fn bitmap_helpers_cover_the_switch_slot() {
        // Slot 5 is the host-switch finger; it must show up in listings,
        // otherwise a user cannot tell whether switching is set up.
        assert_eq!(fp_bitmap_slots(0b0010_0001), vec![0, 5]);
        assert_eq!(
            fp_bitmap_display(0b0010_0001),
            "[■] [ ] [ ] [ ] [ ] [■]"
        );
    }
}

#[cfg(test)]
mod enroll_overlap_tests {
    use super::*;

    #[test]
    fn overlap_is_its_own_event_not_a_failure() {
        assert_eq!(EnrollEvent::from_notification(0x06, 2, 6), EnrollEvent::Overlap);
        assert_eq!(EnrollEvent::from_notification(0x04, 6, 6), EnrollEvent::Complete);
        assert_eq!(EnrollEvent::from_notification(0x7f, 0, 6), EnrollEvent::Failed);
    }

    #[test]
    fn pair_progress_wire_roundtrip() {
        for p in [
            PairProgress::Idle,
            PairProgress::WaitFp,
            PairProgress::WaitButton,
            PairProgress::Ecdh,
            PairProgress::Done,
            PairProgress::Failed,
        ] {
            assert_eq!(PairProgress::from_wire(p.as_wire()), Some(p));
        }
        assert_eq!(PairProgress::from_wire("BOGUS"), None);
    }
}
