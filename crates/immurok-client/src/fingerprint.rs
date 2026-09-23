//! Fingerprint slots and enrollment primitives.
//!
//! Wire formats (`immurok-daemon/src/socket.rs`):
//!   FP:LIST            → `OK:<bitmap>`            (u8, bit N = slot N)
//!   FP:ENROLL:<slot>   → `OK:ENROLL_STARTED`      (after the FP-gate, if any;
//!                        the 6-frame capture continues on the device)
//!   FP:STATUS          → `OK:IDLE` | `OK:<status>:<current>:<total>`
//!                        (raw status code, see `protocol::ENROLL_*`)
//!   FP:ENROLL_CANCEL   → `OK:ENROLL_CANCELLED`
//!   FP:DELETE:<slot>   → `OK:DELETED`             (FP-gated when any finger exists)
//!   FP:VERIFY          → `OK:MATCH` | `OK:NO_MATCH` (FP-gated)

use immurok_common::protocol::{
    ENROLL_CAPTURED, ENROLL_COMPLETE, ENROLL_LIFT_FINGER, ENROLL_OVERLAP, ENROLL_PROCESSING,
    ENROLL_WAITING, MAX_FINGERPRINT_SLOTS, SWITCH_FINGER_SLOT, TOTAL_FINGERPRINT_SLOTS,
};

use crate::keys::GATE_TIMEOUT;
use crate::DaemonClient;

/// Slot reserved for the host-switch finger (firmware ≥ 1.6.4).
pub const SWITCH_SLOT: u8 = SWITCH_FINGER_SLOT;
/// Number of authentication slots (0..AUTH_SLOTS).
pub const AUTH_SLOTS: u8 = MAX_FINGERPRINT_SLOTS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FpSlots {
    pub bitmap: u8,
}

impl FpSlots {
    pub fn is_enrolled(self, slot: u8) -> bool {
        slot < TOTAL_FINGERPRINT_SLOTS && self.bitmap & (1 << slot) != 0
    }

    pub fn auth_slots(self) -> Vec<u8> {
        (0..AUTH_SLOTS).filter(|s| self.is_enrolled(*s)).collect()
    }

    pub fn auth_count(self) -> usize {
        self.auth_slots().len()
    }

    pub fn first_free_auth_slot(self) -> Option<u8> {
        (0..AUTH_SLOTS).find(|s| !self.is_enrolled(*s))
    }

    pub fn switch_enrolled(self) -> bool {
        self.is_enrolled(SWITCH_SLOT)
    }

    /// Any finger at all (auth or switch). The daemon gates enroll/delete
    /// behind a touch only when this is true.
    pub fn any(self) -> bool {
        self.bitmap & ((1 << TOTAL_FINGERPRINT_SLOTS) - 1) != 0
    }
}

pub fn parse_fp_list(line: &str) -> Option<FpSlots> {
    let bitmap = line.trim().strip_prefix("OK:")?.parse::<u8>().ok()?;
    Some(FpSlots { bitmap })
}

pub fn fp_list() -> Result<FpSlots, String> {
    let rsp = DaemonClient::connect()?.send("FP:LIST")?;
    match parse_fp_list(&rsp) {
        Some(s) => Ok(s),
        None => Err(rsp),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollStatus {
    Idle,
    Waiting { current: u8, total: u8 },
    Captured { current: u8, total: u8 },
    Processing,
    LiftFinger,
    /// Frame too similar to the previous one; shift the finger, no progress.
    Overlap,
    Complete,
    Failed,
}

pub fn parse_fp_status(line: &str) -> Option<EnrollStatus> {
    let parts: Vec<&str> = line.trim().split(':').collect();
    if parts.first() != Some(&"OK") {
        return None;
    }
    let second = *parts.get(1)?;
    if second == "IDLE" {
        return Some(EnrollStatus::Idle);
    }
    let status: u8 = second.parse().ok()?;
    let current: u8 = parts.get(2).and_then(|v| v.parse().ok()).unwrap_or(0);
    let total: u8 = parts.get(3).and_then(|v| v.parse().ok()).unwrap_or(6);
    Some(match status {
        ENROLL_WAITING => EnrollStatus::Waiting { current, total },
        ENROLL_CAPTURED => EnrollStatus::Captured { current, total },
        ENROLL_PROCESSING => EnrollStatus::Processing,
        ENROLL_LIFT_FINGER => EnrollStatus::LiftFinger,
        ENROLL_OVERLAP => EnrollStatus::Overlap,
        ENROLL_COMPLETE => EnrollStatus::Complete,
        _ => EnrollStatus::Failed,
    })
}

pub fn fp_status() -> Result<EnrollStatus, String> {
    let rsp = DaemonClient::connect()?.send("FP:STATUS")?;
    parse_fp_status(&rsp).ok_or_else(|| format!("unexpected FP:STATUS reply: {rsp}"))
}

/// Ask the device to start enrolling `slot`. Blocks through the FP-gate
/// (when fingers already exist) and returns once capture has begun; progress
/// is then polled with [`fp_status`].
pub fn enroll_start(slot: u8) -> Result<(), String> {
    let rsp = DaemonClient::connect()?.send_with_timeout(&format!("FP:ENROLL:{slot}"), GATE_TIMEOUT)?;
    if rsp == "OK:ENROLL_STARTED" {
        Ok(())
    } else {
        Err(rsp)
    }
}

/// Best effort: abort an enrollment (also unblocks a pending FP-gate wait).
pub fn enroll_cancel() {
    if let Ok(mut c) = DaemonClient::connect() {
        let _ = c.send("FP:ENROLL_CANCEL");
    }
}

pub fn fp_delete(slot: u8) -> Result<(), String> {
    let rsp = DaemonClient::connect()?.send_with_timeout(&format!("FP:DELETE:{slot}"), GATE_TIMEOUT)?;
    if rsp.starts_with("OK") {
        Ok(())
    } else {
        Err(rsp)
    }
}

/// One gated match attempt. `Ok(true)` = the touched finger matched.
pub fn fp_verify() -> Result<bool, String> {
    let rsp = DaemonClient::connect()?.send_with_timeout("FP:VERIFY", GATE_TIMEOUT)?;
    match rsp.as_str() {
        "OK:MATCH" => Ok(true),
        "OK:NO_MATCH" => Ok(false),
        _ => Err(rsp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitmap_helpers() {
        let none = FpSlots { bitmap: 0 };
        assert!(!none.any());
        assert_eq!(none.first_free_auth_slot(), Some(0));
        assert_eq!(none.auth_count(), 0);

        let s = FpSlots { bitmap: 0b10_0101 }; // slots 0, 2, 5
        assert!(s.is_enrolled(0) && s.is_enrolled(2) && s.is_enrolled(5));
        assert!(!s.is_enrolled(1) && !s.is_enrolled(6));
        assert_eq!(s.auth_slots(), vec![0, 2]);
        assert_eq!(s.auth_count(), 2);
        assert!(s.switch_enrolled());
        assert_eq!(s.first_free_auth_slot(), Some(1));

        let full = FpSlots { bitmap: 0b01_1111 };
        assert_eq!(full.first_free_auth_slot(), None);
        assert!(!full.switch_enrolled());
    }

    #[test]
    fn parses_fp_list() {
        assert_eq!(parse_fp_list("OK:37"), Some(FpSlots { bitmap: 37 }));
        assert_eq!(parse_fp_list("ERROR:NOT_CONNECTED"), None);
        assert_eq!(parse_fp_list("OK:abc"), None);
    }

    #[test]
    fn parses_fp_status() {
        assert_eq!(parse_fp_status("OK:IDLE"), Some(EnrollStatus::Idle));
        assert_eq!(parse_fp_status("OK:0:0:6"), Some(EnrollStatus::Waiting { current: 0, total: 6 }));
        assert_eq!(parse_fp_status("OK:1:2:6"), Some(EnrollStatus::Captured { current: 2, total: 6 }));
        assert_eq!(parse_fp_status("OK:2:2:6"), Some(EnrollStatus::Processing));
        assert_eq!(parse_fp_status("OK:3:2:6"), Some(EnrollStatus::LiftFinger));
        assert_eq!(parse_fp_status("OK:6:2:6"), Some(EnrollStatus::Overlap));
        assert_eq!(parse_fp_status("OK:4:6:6"), Some(EnrollStatus::Complete));
        assert_eq!(parse_fp_status("OK:255:0:6"), Some(EnrollStatus::Failed));
        // Unknown code = failure, never silently "waiting".
        assert_eq!(parse_fp_status("OK:9:0:6"), Some(EnrollStatus::Failed));
        assert_eq!(parse_fp_status("ERROR:NOT_CONNECTED"), None);
        assert_eq!(parse_fp_status("OK:"), None);
    }
}
