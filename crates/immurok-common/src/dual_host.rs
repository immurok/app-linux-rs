//! Dual-host protocol decoding (firmware 1.6.12+).
//!
//! Everything here is a pure function so it can be unit-tested without a
//! device. See app-linux-rs/docs/dual-host-port.md for the wire format.

use crate::protocol::{
    CMD_SLOT_CLEAR, CMD_SLOT_STATUS, PAIR_BUTTON_CANCELLED, PAIR_BUTTON_CONFIRMED,
    PAIR_BUTTON_FP_OK, PAIR_BUTTON_TIMEOUT, RSP_ERR_NOT_PAIRED, RSP_OK, SLOT_1, SLOT_2,
};

/// Slot occupancy as reported by `CMD_SLOT_STATUS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotStatus {
    /// Firmware understands dual-host. `bitmap` bit0 = slot 1 occupied,
    /// bit1 = slot 2 occupied. `active` is 1 or 2.
    Supported { bitmap: u8, active: u8 },
    /// Firmware predates 1.6.12 and does not know 0x39.
    Unsupported,
}

impl SlotStatus {
    /// True when the slot this host talks through is empty while the other
    /// one is taken — i.e. pairing now would enroll this machine as the
    /// *second* host (fingerprint + button instead of button only).
    pub fn is_second_host(&self) -> bool {
        match *self {
            Self::Supported { bitmap, active } => {
                let active_bit = 1u8 << (active - 1);
                bitmap != 0 && bitmap & active_bit == 0
            }
            Self::Unsupported => false,
        }
    }
}

/// Decode a raw `CMD_SLOT_STATUS` response frame: `[0x39][0x00][bitmap][active]`.
///
/// Anything else — including the 2-byte error frame old firmware answers
/// with — decodes as [`SlotStatus::Unsupported`]. That is deliberately not
/// an error: "this device has no dual-host support" is a normal state the
/// UI degrades around.
pub fn parse_slot_status(frame: &[u8]) -> SlotStatus {
    if frame.len() < 4 || frame[0] != CMD_SLOT_STATUS || frame[1] != RSP_OK {
        return SlotStatus::Unsupported;
    }
    let active = frame[3];
    if active != SLOT_1 && active != SLOT_2 {
        return SlotStatus::Unsupported;
    }
    SlotStatus::Supported { bitmap: frame[2] & 0x03, active }
}

/// Decode the daemon's text answer to `SLOT:STATUS`, which is either
/// `OK:<bitmap>:<active>` or `OK:UNSUPPORTED`. Returns `None` for error
/// responses and malformed lines so callers can tell "device says no
/// dual-host" apart from "the query itself failed".
pub fn parse_slot_status_line(line: &str) -> Option<SlotStatus> {
    let parts: Vec<&str> = line.trim().split(':').collect();
    if parts.first() != Some(&"OK") {
        return None;
    }
    match parts.get(1) {
        Some(&"UNSUPPORTED") => Some(SlotStatus::Unsupported),
        Some(bitmap_str) => {
            let bitmap = bitmap_str.parse::<u8>().ok()?;
            let active = parts.get(2)?.parse::<u8>().ok()?;
            if active != SLOT_1 && active != SLOT_2 {
                return None;
            }
            Some(SlotStatus::Supported { bitmap: bitmap & 0x03, active })
        }
        None => None,
    }
}

/// Which host slot belongs to THIS computer, from the optional fourth field
/// of the daemon's `SLOT:STATUS` answer (`OK:<bitmap>:<active>:<mine>`).
///
/// This is deliberately separate from [`parse_slot_status_line`]: that decodes
/// what the DEVICE reported, while this reads what the DAEMON concluded from
/// its own challenge-response. `0`, a missing field (older daemon), or an
/// unparsable one all mean "not proven" — never guess a slot here, because a
/// wrong guess labels the user's own pairing as another computer's and points
/// destructive actions at the wrong host.
pub fn parse_slot_owner(line: &str) -> Option<u8> {
    let owner = line.trim().split(':').nth(3)?.parse::<u8>().ok()?;
    (owner == SLOT_1 || owner == SLOT_2).then_some(owner)
}

/// Which `SLOT_CLEAR` path a request resolves to. The two paths are not
/// interchangeable on the daemon side: "own" is ungated and the device
/// reboots straight after answering, "other" runs a fingerprint gate and
/// leaves the link up. The daemon must therefore pick the path *before*
/// sending, which is why this decision is a separate pure function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearPath {
    Own,
    Other(u8),
    InvalidSlot,
    /// A far slot was named but the firmware cannot tell us which slot is
    /// active, so we cannot know which path to take.
    Unsupported,
}

pub fn resolve_clear_path(target: Option<u8>, status: SlotStatus) -> ClearPath {
    let Some(slot) = target else {
        // No explicit target: clear the slot this connection runs through.
        return ClearPath::Own;
    };
    if slot != SLOT_1 && slot != SLOT_2 {
        return ClearPath::InvalidSlot;
    }
    match status {
        SlotStatus::Supported { active, .. } => {
            if slot == active { ClearPath::Own } else { ClearPath::Other(slot) }
        }
        SlotStatus::Unsupported => ClearPath::Unsupported,
    }
}

/// Outcome of an own-slot `SLOT_CLEAR`, decoded from the response frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotClearAck {
    /// `[0x3C][0x00]` — the device confirms the slot is gone.
    Cleared,
    /// `[0x3C][0xF2]` — SEC_ERR_NOT_PAIRED from the firmware's pre-pair
    /// whitelist (hidkbd.c:4920). The slot the device is currently presenting
    /// is empty, so it refuses every command outside the six it allows while
    /// unpaired. This says nothing about whether OUR slot still holds a key —
    /// under dual-host the device may simply be sitting on the other slot.
    ///
    /// It must not be read as [`Refused`](Self::Refused): doing so kept local
    /// pairing forever, and since `pair` bails out whenever local pairing
    /// exists, the user was left with no software way to recover.
    SlotUnpaired,
    /// `[0x3C][other non-zero]` — the device explicitly refused and did NOT
    /// reboot. The slot is still occupied and the link is still up, so local
    /// pairing must be KEPT; dropping it would leave a zombie slot the host
    /// has forgotten about, occupying one of only two.
    Refused,
    /// Anything else, including old firmware answering an unknown command.
    Unrecognised,
}

/// Classify the response to an own-slot `CMD_SLOT_CLEAR` send. `first` /
/// `rest` are the already-split `(frame[0], frame[1..])` shape `ble_send`
/// hands back — same convention as [`parse_slot_status`].
pub fn classify_slot_clear_ack(first: u8, rest: &[u8]) -> SlotClearAck {
    if first != CMD_SLOT_CLEAR {
        return SlotClearAck::Unrecognised;
    }
    match rest.first() {
        Some(&RSP_OK) => SlotClearAck::Cleared,
        Some(&RSP_ERR_NOT_PAIRED) => SlotClearAck::SlotUnpaired,
        Some(_) => SlotClearAck::Refused,
        None => SlotClearAck::Unrecognised,
    }
}

/// Outcome of a `SLOT:CLEAR` request, decoded from the daemon's response line.
///
/// The daemon has four distinct answers and they mean very different things
/// to the user, so every caller must branch on the exact status field. A
/// `contains("CLEARED")` check matches three of them at once and reports the
/// two partial outcomes as a full success — including the case where a
/// `SLOT:CLEAR:N` aimed at the far host was re-routed to this host's own
/// slot because the active slot changed mid-request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnpairOutcome {
    /// `OK:CLEARED` — the device confirmed the slot is gone.
    Cleared,
    /// `OK:CLEARED_UNCONFIRMED` — local pairing dropped, device answer lost
    /// (normally the post-clear reboot) or unrecognised (old firmware).
    ClearedUnconfirmed,
    /// `OK:CLEARED_LOCAL_ONLY` — device offline; only local state changed.
    ClearedLocalOnly,
    /// `OK:CLEARED_SLOT_UNPAIRED` — the device is connected but sitting on a
    /// slot that is not paired, so it refused the command outright. Nothing
    /// on the device changed; only the local record was dropped.
    ClearedSlotUnpaired,
    /// `ERROR:SLOT_CLEAR_REFUSED` — device said no and nothing changed.
    Refused,
    /// Anything else, including transport errors and unknown status fields.
    Failed,
}

/// Classify the daemon's answer to `SLOT:CLEAR` / `SLOT:CLEAR:<n>`.
///
/// Matching is on the exact `<kind>:<status>` pair; unknown lines are
/// [`UnpairOutcome::Failed`] rather than optimistically read as success.
pub fn classify_unpair_response(line: &str) -> UnpairOutcome {
    let mut parts = line.trim().split(':');
    match (parts.next(), parts.next()) {
        (Some("OK"), Some("CLEARED")) => UnpairOutcome::Cleared,
        (Some("OK"), Some("CLEARED_UNCONFIRMED")) => UnpairOutcome::ClearedUnconfirmed,
        (Some("OK"), Some("CLEARED_LOCAL_ONLY")) => UnpairOutcome::ClearedLocalOnly,
        (Some("OK"), Some("CLEARED_SLOT_UNPAIRED")) => UnpairOutcome::ClearedSlotUnpaired,
        (Some("ERROR"), Some("SLOT_CLEAR_REFUSED")) => UnpairOutcome::Refused,
        _ => UnpairOutcome::Failed,
    }
}

/// Meaning of a `[0x34][status]` notification.
///
/// Unknown status codes MUST map to [`PairButtonEvent::Ignore`] and be
/// swallowed. Letting them fall through to the generic command-response
/// path is what broke second-host pairing: during pairing the pending
/// response slot is armed for `[0x30][pubkey:33]`, so a stray `[0x34][..]`
/// resolves it and pairing fails with "bad response".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairButtonEvent {
    /// Button pressed; the device starts ECDH. Completion still arrives
    /// separately as `[0x30][pubkey:33]`.
    Confirmed,
    /// Second-host enrollment: fingerprint half passed, now awaiting the button.
    FingerprintOk,
    /// Terminal outcome — timeout (0x00) or long-press cancel (0x02).
    Terminal(u8),
    /// Unrecognised — swallow.
    Ignore,
}

pub fn classify_pair_button(status: u8) -> PairButtonEvent {
    match status {
        PAIR_BUTTON_CONFIRMED => PairButtonEvent::Confirmed,
        PAIR_BUTTON_FP_OK => PairButtonEvent::FingerprintOk,
        PAIR_BUTTON_TIMEOUT | PAIR_BUTTON_CANCELLED => PairButtonEvent::Terminal(status),
        _ => PairButtonEvent::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_slot_status ─────────────────────────────────────

    #[test]
    fn slot_status_both_occupied() {
        let s = parse_slot_status(&[CMD_SLOT_STATUS, RSP_OK, 0x03, 0x01]);
        assert_eq!(s, SlotStatus::Supported { bitmap: 0x03, active: 1 });
    }

    #[test]
    fn slot_status_masks_reserved_bits() {
        // Only bit0/bit1 are defined; a future firmware setting a high bit
        // must not leak into occupancy checks.
        let s = parse_slot_status(&[CMD_SLOT_STATUS, RSP_OK, 0xF1, 0x02]);
        assert_eq!(s, SlotStatus::Supported { bitmap: 0x01, active: 2 });
    }

    #[test]
    fn slot_status_old_firmware_is_unsupported_not_error() {
        // Firmware < 1.6.12 does not know 0x39 and answers with a 2-byte
        // error frame. That means "no dual-host support", not a failure.
        assert_eq!(parse_slot_status(&[CMD_SLOT_STATUS, 0xFE]), SlotStatus::Unsupported);
    }

    #[test]
    fn slot_status_rejects_short_and_empty_frames() {
        assert_eq!(parse_slot_status(&[]), SlotStatus::Unsupported);
        assert_eq!(parse_slot_status(&[CMD_SLOT_STATUS, RSP_OK, 0x03]), SlotStatus::Unsupported);
    }

    #[test]
    fn slot_status_rejects_out_of_range_active() {
        assert_eq!(
            parse_slot_status(&[CMD_SLOT_STATUS, RSP_OK, 0x03, 0x05]),
            SlotStatus::Unsupported
        );
        assert_eq!(
            parse_slot_status(&[CMD_SLOT_STATUS, RSP_OK, 0x03, 0x00]),
            SlotStatus::Unsupported
        );
    }

    // ── is_second_host ────────────────────────────────────────

    #[test]
    fn second_host_when_active_slot_empty_and_other_occupied() {
        assert!(SlotStatus::Supported { bitmap: 0x01, active: 2 }.is_second_host());
        assert!(SlotStatus::Supported { bitmap: 0x02, active: 1 }.is_second_host());
    }

    #[test]
    fn not_second_host_when_active_slot_occupied_or_device_blank() {
        assert!(!SlotStatus::Supported { bitmap: 0x01, active: 1 }.is_second_host());
        assert!(!SlotStatus::Supported { bitmap: 0x03, active: 2 }.is_second_host());
        assert!(!SlotStatus::Supported { bitmap: 0x00, active: 1 }.is_second_host());
        assert!(!SlotStatus::Unsupported.is_second_host());
    }

    // ── parse_slot_status_line ────────────────────────────────

    #[test]
    fn status_line_round_trips() {
        assert_eq!(
            parse_slot_status_line("OK:3:1"),
            Some(SlotStatus::Supported { bitmap: 0x03, active: 1 })
        );
        assert_eq!(parse_slot_status_line("OK:UNSUPPORTED"), Some(SlotStatus::Unsupported));
    }

    #[test]
    fn status_line_rejects_errors_and_garbage() {
        assert_eq!(parse_slot_status_line("ERROR:NOT_CONNECTED"), None);
        assert_eq!(parse_slot_status_line("OK:3"), None);
        assert_eq!(parse_slot_status_line("OK:x:1"), None);
        assert_eq!(parse_slot_status_line("OK:3:9"), None);
        assert_eq!(parse_slot_status_line(""), None);
    }

    // ── parse_slot_owner ──────────────────────────────────────

    #[test]
    fn slot_owner_reads_the_fourth_field() {
        assert_eq!(parse_slot_owner("OK:3:1:1"), Some(1));
        assert_eq!(parse_slot_owner("OK:3:2:2"), Some(2));
    }

    #[test]
    fn slot_owner_unproven_cases_are_none() {
        // 0 = the daemon could not prove ownership.
        assert_eq!(parse_slot_owner("OK:3:1:0"), None);
        // Older daemon: field absent entirely.
        assert_eq!(parse_slot_owner("OK:3:1"), None);
        // Never invent a slot from garbage.
        assert_eq!(parse_slot_owner("OK:3:1:9"), None);
        assert_eq!(parse_slot_owner("OK:3:1:x"), None);
        assert_eq!(parse_slot_owner("OK:UNSUPPORTED"), None);
        assert_eq!(parse_slot_owner("ERROR:NOT_CONNECTED"), None);
    }

    #[test]
    fn slot_status_line_still_parses_with_the_owner_field_appended() {
        // Backwards compatibility both ways: the status decoder must ignore
        // the field it does not own.
        assert_eq!(
            parse_slot_status_line("OK:3:1:1"),
            Some(SlotStatus::Supported { bitmap: 0x03, active: 1 })
        );
    }

    // ── resolve_clear_path ────────────────────────────────────

    #[test]
    fn clear_path_defaults_to_own_even_on_old_firmware() {
        // No explicit target means "clear the slot I am talking through".
        // On firmware without 0x3C the send fails and the caller falls back
        // to clearing local state, which is the documented degrade.
        assert_eq!(resolve_clear_path(None, SlotStatus::Unsupported), ClearPath::Own);
        assert_eq!(
            resolve_clear_path(None, SlotStatus::Supported { bitmap: 0x03, active: 2 }),
            ClearPath::Own
        );
    }

    #[test]
    fn clear_path_own_when_target_is_active_slot() {
        assert_eq!(
            resolve_clear_path(Some(2), SlotStatus::Supported { bitmap: 0x03, active: 2 }),
            ClearPath::Own
        );
    }

    #[test]
    fn clear_path_other_when_target_is_the_far_slot() {
        assert_eq!(
            resolve_clear_path(Some(1), SlotStatus::Supported { bitmap: 0x03, active: 2 }),
            ClearPath::Other(1)
        );
    }

    #[test]
    fn clear_path_rejects_bad_slot_numbers() {
        let st = SlotStatus::Supported { bitmap: 0x03, active: 1 };
        assert_eq!(resolve_clear_path(Some(0), st), ClearPath::InvalidSlot);
        assert_eq!(resolve_clear_path(Some(3), st), ClearPath::InvalidSlot);
    }

    #[test]
    fn clear_path_cannot_target_far_slot_on_old_firmware() {
        assert_eq!(resolve_clear_path(Some(1), SlotStatus::Unsupported), ClearPath::Unsupported);
    }

    // ── classify_slot_clear_ack ───────────────────────────────

    #[test]
    fn slot_clear_ack_confirmed() {
        assert_eq!(classify_slot_clear_ack(CMD_SLOT_CLEAR, &[RSP_OK]), SlotClearAck::Cleared);
    }

    #[test]
    fn slot_clear_ack_explicit_refusal() {
        // [0x3C][0xFE]: device refused and did NOT reboot — must not be
        // conflated with a lost answer.
        assert_eq!(classify_slot_clear_ack(CMD_SLOT_CLEAR, &[0xFE]), SlotClearAck::Refused);
    }

    #[test]
    fn slot_clear_ack_not_paired_is_not_a_refusal() {
        // Regression pin (2026-08-04 field report). [0x3C][0xF2] is
        // SEC_ERR_NOT_PAIRED from the firmware's pre-pair whitelist
        // (hidkbd.c:4920): the slot the device is presenting is empty, so it
        // refuses every non-whitelisted command. That is NOT "I declined to
        // clear the slot". Classifying it as Refused made clear_own_slot keep
        // local pairing, so `unpair` could never succeed while the device sat
        // on the other slot — and `pair` refuses whenever local pairing
        // exists, which deadlocked the user with no software way out.
        assert_eq!(
            classify_slot_clear_ack(CMD_SLOT_CLEAR, &[RSP_ERR_NOT_PAIRED]),
            SlotClearAck::SlotUnpaired
        );
        assert_ne!(
            classify_slot_clear_ack(CMD_SLOT_CLEAR, &[RSP_ERR_NOT_PAIRED]),
            SlotClearAck::Refused
        );
    }

    #[test]
    fn slot_clear_ack_empty_rest_is_unrecognised() {
        assert_eq!(classify_slot_clear_ack(CMD_SLOT_CLEAR, &[]), SlotClearAck::Unrecognised);
    }

    #[test]
    fn slot_clear_ack_wrong_first_byte_is_unrecognised() {
        assert_eq!(classify_slot_clear_ack(0xFE, &[RSP_OK]), SlotClearAck::Unrecognised);
    }

    // ── classify_unpair_response ──────────────────────────────

    #[test]
    fn unpair_response_exact_daemon_strings() {
        // Every literal socket.rs can emit for SLOT:CLEAR.
        assert_eq!(classify_unpair_response("OK:CLEARED"), UnpairOutcome::Cleared);
        assert_eq!(
            classify_unpair_response("OK:CLEARED_UNCONFIRMED"),
            UnpairOutcome::ClearedUnconfirmed
        );
        assert_eq!(
            classify_unpair_response("OK:CLEARED_LOCAL_ONLY"),
            UnpairOutcome::ClearedLocalOnly
        );
        assert_eq!(classify_unpair_response("ERROR:SLOT_CLEAR_REFUSED"), UnpairOutcome::Refused);
    }

    #[test]
    fn unpair_partial_outcomes_are_not_cleared() {
        // Regression pin: `unpair --slot N` used to test the response with
        // `contains("CLEARED")`. When the active slot changes between the
        // CLI's SLOT:STATUS and the daemon's own re-resolution, the request
        // is re-routed to this host's OWN slot and can answer
        // CLEARED_UNCONFIRMED / CLEARED_LOCAL_ONLY — reported back as a
        // green "host slot N cleared" while the far host was never touched.
        assert_ne!(
            classify_unpair_response("OK:CLEARED_UNCONFIRMED"),
            UnpairOutcome::Cleared
        );
        assert_ne!(classify_unpair_response("OK:CLEARED_LOCAL_ONLY"), UnpairOutcome::Cleared);
        assert_ne!(classify_unpair_response("OK:CLEARED_SLOT_UNPAIRED"), UnpairOutcome::Cleared);
    }

    #[test]
    fn unpair_slot_unpaired_is_its_own_outcome() {
        // The device answered 0xF2 (its active slot is empty), so nothing on
        // the device changed even though the local record was dropped. It
        // must not read as a clean clear, and it must not read as Failed
        // either — the local half genuinely succeeded.
        assert_eq!(
            classify_unpair_response("OK:CLEARED_SLOT_UNPAIRED"),
            UnpairOutcome::ClearedSlotUnpaired
        );
    }

    #[test]
    fn unpair_response_rejects_errors_and_garbage() {
        assert_eq!(classify_unpair_response("ERROR:NOT_CONNECTED"), UnpairOutcome::Failed);
        assert_eq!(
            classify_unpair_response("ERROR:SLOT_CLEAR_FAILED:timeout"),
            UnpairOutcome::Failed
        );
        assert_eq!(classify_unpair_response("ERROR:INVALID_SLOT"), UnpairOutcome::Failed);
        assert_eq!(classify_unpair_response("ERROR:DUAL_HOST_UNSUPPORTED"), UnpairOutcome::Failed);
        assert_eq!(classify_unpair_response("OK"), UnpairOutcome::Failed);
        assert_eq!(classify_unpair_response(""), UnpairOutcome::Failed);
        // Right status word, wrong kind — must not read as success.
        assert_eq!(classify_unpair_response("ERROR:CLEARED"), UnpairOutcome::Failed);
    }

    // ── classify_pair_button ──────────────────────────────────

    #[test]
    fn pair_button_known_states() {
        assert_eq!(classify_pair_button(PAIR_BUTTON_CONFIRMED), PairButtonEvent::Confirmed);
        assert_eq!(classify_pair_button(PAIR_BUTTON_FP_OK), PairButtonEvent::FingerprintOk);
        assert_eq!(
            classify_pair_button(PAIR_BUTTON_TIMEOUT),
            PairButtonEvent::Terminal(PAIR_BUTTON_TIMEOUT)
        );
        assert_eq!(
            classify_pair_button(PAIR_BUTTON_CANCELLED),
            PairButtonEvent::Terminal(PAIR_BUTTON_CANCELLED)
        );
    }

    #[test]
    fn pair_button_unknown_state_is_ignored_never_a_command_response() {
        // This is the regression pin for the bug that made second-host
        // pairing impossible: an unrecognised 0x34 status used to fall
        // through to the generic command-response path and get eaten by
        // the oneshot that pairing had armed for [0x30][pubkey:33].
        assert_eq!(classify_pair_button(0x7F), PairButtonEvent::Ignore);
        assert_eq!(classify_pair_button(0xFF), PairButtonEvent::Ignore);
    }
}
