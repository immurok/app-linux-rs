//! Dual-host (two computers per device) slots and pairing.
//!
//! Wire formats (`immurok-daemon/src/socket.rs`, decoding helpers in
//! `immurok-common::dual_host`):
//!   SLOT:STATUS     → `OK:<bitmap>:<active>[:<mine>]` | `OK:UNSUPPORTED`
//!   SLOT:CLEAR      → `OK:CLEARED` | `OK:CLEARED_UNCONFIRMED` | `OK:CLEARED_LOCAL_ONLY`
//!                     (this host's own slot; ungated)
//!   SLOT:CLEAR:<n>  → `OK:CLEARED` | `OK:CLEARED_SLOT_UNPAIRED`
//!                     (the other host's slot; always FP-gated)
//!   PAIR:START      → `OK:PAIRED` (up to 150 s; FP-gated for a second host)
//!   PAIR:PROGRESS   → `OK:<IDLE|WAIT_FP|WAIT_BUTTON|ECDH|DONE|FAILED>`

use std::time::Duration;

use immurok_common::dual_host::{parse_slot_owner, parse_slot_status_line, SlotStatus};
use immurok_common::types::PairProgress;

use crate::keys::GATE_TIMEOUT;
use crate::DaemonClient;

pub const PAIR_TIMEOUT: Duration = Duration::from_secs(150);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HostSlots {
    /// False on firmware without dual-host support (`OK:UNSUPPORTED`).
    pub supported: bool,
    pub slot1: bool,
    pub slot2: bool,
    /// Slot the device is currently presenting (1 or 2; 0 if unsupported).
    pub active: u8,
    /// Slot the daemon has cryptographically proven to be THIS computer's.
    /// `None` = not proven (unpaired, older daemon, or verification failed).
    pub mine: Option<u8>,
}

impl HostSlots {
    pub fn bound(self, n: u8) -> bool {
        match n {
            1 => self.slot1,
            2 => self.slot2,
            _ => false,
        }
    }

    pub fn both_bound(self) -> bool {
        self.slot1 && self.slot2
    }

    pub fn other_of(n: u8) -> u8 {
        if n == 1 {
            2
        } else {
            1
        }
    }
}

pub fn parse_host_slots(line: &str) -> Option<HostSlots> {
    Some(match parse_slot_status_line(line)? {
        SlotStatus::Unsupported => HostSlots { supported: false, ..HostSlots::default() },
        SlotStatus::Supported { bitmap, active } => HostSlots {
            supported: true,
            slot1: bitmap & 0b01 != 0,
            slot2: bitmap & 0b10 != 0,
            active,
            mine: parse_slot_owner(line),
        },
    })
}

pub fn slot_status() -> Result<HostSlots, String> {
    let rsp = DaemonClient::connect()?.send("SLOT:STATUS")?;
    parse_host_slots(&rsp).ok_or_else(|| format!("unexpected SLOT:STATUS reply: {rsp}"))
}

fn ok_if_ok(rsp: String) -> Result<(), String> {
    if rsp.starts_with("OK") {
        Ok(())
    } else {
        Err(rsp)
    }
}

/// Unbind the *other* computer. The device asks for an enrolled finger first.
pub fn clear_other_slot(n: u8) -> Result<(), String> {
    let rsp = DaemonClient::connect()?.send_with_timeout(&format!("SLOT:CLEAR:{n}"), GATE_TIMEOUT)?;
    ok_if_ok(rsp)
}

/// Unpair this computer (clears our own slot; fingerprints and keys stay).
pub fn clear_own_slot() -> Result<(), String> {
    let rsp = DaemonClient::connect()?.send("SLOT:CLEAR")?;
    ok_if_ok(rsp)
}

/// Blocks until the user confirms on the device (button, plus a touch when
/// enrolling as the second host) or the daemon gives up.
pub fn pair_start() -> Result<(), String> {
    let rsp = DaemonClient::connect()?.send_with_timeout("PAIR:START", PAIR_TIMEOUT)?;
    if rsp == "OK:PAIRED" {
        Ok(())
    } else {
        Err(rsp)
    }
}

pub fn parse_pair_progress(line: &str) -> Option<PairProgress> {
    PairProgress::from_wire(line.trim().strip_prefix("OK:")?)
}

pub fn pair_progress() -> Result<PairProgress, String> {
    let rsp = DaemonClient::connect()?.send("PAIR:PROGRESS")?;
    parse_pair_progress(&rsp).ok_or_else(|| format!("unexpected PAIR:PROGRESS reply: {rsp}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_with_owner() {
        let h = parse_host_slots("OK:3:1:2").unwrap();
        assert!(h.supported && h.slot1 && h.slot2);
        assert_eq!(h.active, 1);
        assert_eq!(h.mine, Some(2));
        assert!(h.both_bound());
        assert!(h.bound(1) && h.bound(2) && !h.bound(3));
    }

    #[test]
    fn owner_zero_or_missing_is_unproven() {
        assert_eq!(parse_host_slots("OK:1:1:0").unwrap().mine, None);
        assert_eq!(parse_host_slots("OK:1:1").unwrap().mine, None);
    }

    #[test]
    fn unsupported_firmware() {
        let h = parse_host_slots("OK:UNSUPPORTED").unwrap();
        assert!(!h.supported && !h.slot1 && !h.slot2);
        assert_eq!(h.mine, None);
    }

    #[test]
    fn rejects_errors() {
        assert_eq!(parse_host_slots("ERROR:NOT_CONNECTED"), None);
        assert_eq!(parse_host_slots("OK:3:9"), None);
    }

    #[test]
    fn other_slot() {
        assert_eq!(HostSlots::other_of(1), 2);
        assert_eq!(HostSlots::other_of(2), 1);
    }

    #[test]
    fn pair_progress_line() {
        assert_eq!(parse_pair_progress("OK:WAIT_BUTTON"), Some(PairProgress::WaitButton));
        assert_eq!(parse_pair_progress("OK:IDLE"), Some(PairProgress::Idle));
        assert_eq!(parse_pair_progress("ERROR:X"), None);
    }
}
