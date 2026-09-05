//! `immurok-cli pair` / `immurok-cli unpair` — pairing management.

use std::time::Duration;

use immurok_common::dual_host::{
    classify_unpair_response, parse_slot_status_line, SlotStatus, UnpairOutcome,
};

use crate::socket_client::DaemonClient;

pub fn run_pair() {
    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    // Check current pairing status. Exact match against the full line, not
    // `contains`: the daemon's answers are literally "OK:PAIRED" /
    // "OK:UNPAIRED", and a substring check here is the same class of bug
    // `run_factory_reset` almost shipped with (its failure string contains
    // "RESET" too).
    let pair_rsp = client.send("PAIR:STATUS").unwrap_or_default();
    // 设备明说它不认本机时，本地那份 pairing 已经是废纸，不该拿它把用户挡在
    // 门外 —— 那正好是 status 刚建议他跑 pair 的那种状态，再让他先去 unpair
    // 就是多绕一圈。配对成功时 daemon 会覆写本地记录。
    let device_unpaired = crate::socket_client::DaemonClient::connect()
        .and_then(|mut c| c.send("STATUS"))
        .map(|r| r.split(':').nth(5) == Some("1"))
        .unwrap_or(false);
    if pair_rsp == "OK:PAIRED" && device_unpaired {
        eprintln!(
            "Local pairing exists but the device says it is not paired with this \
             computer — replacing the stale record."
        );
    } else if pair_rsp == "OK:PAIRED" {
        eprintln!("Already paired. Unpair first with: immurok-cli unpair");
        std::process::exit(1);
    }

    // Resolve the slot picture BEFORE starting the handshake. Issuing both
    // concurrently would let WAIT_BUTTON come back first and the guide would
    // render the wrong branch (dual-host-port.md §3).
    let second_host = query_second_host();

    if second_host {
        println!("This device is already bound to another computer.");
        println!("Binding this one as the second host:");
        println!("  (1/2) Touch an enrolled fingerprint on the device");
        println!("  (2/2) Then press the device button");
    } else {
        println!("Starting pairing... Press the device button within 30s to confirm.");
    }

    // PAIR:START blocks for the whole flow and the daemon serves one request
    // per connection, so run it on a worker and poll progress from here.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = DaemonClient::connect()
            .and_then(|mut c| c.send_with_timeout("PAIR:START", Duration::from_secs(150)));
        let _ = tx.send(result);
    });

    let mut last_stage = String::new();
    let outcome = loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(r) => break r,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(line) = DaemonClient::connect().and_then(|mut c| c.send("PAIR:PROGRESS"))
                {
                    let stage = line.split(':').nth(1).unwrap_or("").to_string();
                    if stage != last_stage {
                        print_stage(&stage, second_host);
                        last_stage = stage;
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                break Err("pairing worker exited unexpectedly".to_string())
            }
        }
    };

    match outcome {
        // Exact match against the full line: the daemon's success answer is
        // literally "OK:PAIRED", not merely a response containing "PAIRED".
        Ok(rsp) if rsp == "OK:PAIRED" => {
            println!("\x1b[32mPairing successful!\x1b[0m");
        }
        // The daemon refuses a second concurrent PAIR:START rather than
        // stomping the progress state of one already waiting on the user.
        // That is not a generic failure — say so plainly.
        Ok(rsp) if rsp == "ERROR:PAIRING_IN_PROGRESS" => {
            eprintln!(
                "\x1b[31mAnother pairing attempt is already in progress on this device.\x1b[0m"
            );
            eprintln!("Wait for it to finish or time out, then try again.");
            std::process::exit(1);
        }
        Ok(rsp) => {
            eprintln!("\x1b[31mPairing failed: {}\x1b[0m", rsp);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("\x1b[31mPairing failed: {}\x1b[0m", e);
            std::process::exit(1);
        }
    }
}

/// True when this computer would be enrolled as the *second* host: our slot
/// is empty while the other one is taken. Firmware without dual-host support
/// answers UNSUPPORTED and is treated as a plain first pairing.
fn query_second_host() -> bool {
    let Ok(line) = DaemonClient::connect().and_then(|mut c| c.send("SLOT:STATUS")) else {
        return false;
    };
    matches!(parse_slot_status_line(&line), Some(s) if s.is_second_host())
}

/// Step markers only appear for the second host — first-host pairing is a
/// single action, so "(2/2)" there would invent a step that does not exist.
fn print_stage(stage: &str, second_host: bool) {
    let (step1, step2) = if second_host { ("(1/2) ", "(2/2) ") } else { ("", "") };
    match stage {
        "WAIT_FP" => println!("  {}→ waiting for your fingerprint...", step1),
        "WAIT_BUTTON" => {
            if second_host {
                println!("  {}\x1b[32m✓\x1b[0m fingerprint accepted", step1);
            }
            println!("  {}→ now press the device button...", step2);
        }
        "ECDH" => println!("  \x1b[32m✓\x1b[0m button pressed — exchanging keys..."),
        _ => {}
    }
}

pub fn run_unpair(slot: Option<u8>) {
    match slot {
        None => unpair_own(),
        Some(s) => run_unpair_target(s),
    }
}

/// Resolve what `--slot N` actually means BEFORE printing any consent text.
///
/// `N` is not necessarily the far host: `dual_host::resolve_clear_path` (and
/// the daemon's `SLOT:CLEAR:N` handler) route N through the *own*-clear path
/// whenever it names the slot this connection is talking through — the
/// device has no way to be told "clear the slot you're on" as if it were
/// somebody else's. `unpair_other`'s consent text ("that computer loses
/// access, this one is unaffected") is only true for the genuinely-other
/// case, so that text must never be shown before this is resolved. The
/// other two outcomes worth naming up front: the firmware predates
/// dual-host entirely (no far slot exists to name), and the named slot is
/// already empty (nothing to do).
fn run_unpair_target(slot: u8) {
    if slot != 1 && slot != 2 {
        super::error_exit("Slot must be 1 or 2. Run 'immurok-cli slot status' to see both.");
    }

    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    let rsp = client.send("SLOT:STATUS").unwrap_or_else(|e| {
        super::error_exit(&format!("Failed to read slot status: {}", e));
    });

    match parse_slot_status_line(&rsp) {
        None => super::error_exit(&format!(
            "Failed to read slot status: {}. If you meant to detach this computer, run \
             'immurok-cli unpair' with no --slot.",
            rsp
        )),
        Some(SlotStatus::Unsupported) => super::error_exit(
            "This device's firmware has no dual-host support, so there is no other host slot \
             to name. Run 'immurok-cli unpair' (no --slot) to detach this computer.",
        ),
        Some(SlotStatus::Supported { bitmap, active }) => {
            if slot == active {
                println!("Host slot {} is THIS computer's own slot.", slot);
                unpair_own();
            } else if bitmap & (1 << (slot - 1)) == 0 {
                println!("Host slot {} is already empty. Nothing to do.", slot);
            } else {
                unpair_other(slot);
            }
        }
    }
}

/// What an own-slot unpair would actually cost, resolved before any consent
/// text is printed.
///
/// The trap is firmware-shaped (see the module docs on `unpair_own`): once
/// the last occupied slot is cleared while fingerprints are still enrolled,
/// PAIR_INIT answers NEEDS_RESET and both FACTORY_RESET and DELETE_FP are
/// outside the firmware's pre-pair whitelist, so nothing but the destructive
/// button hold is left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnpairRisk {
    /// Another slot stays occupied, or no fingerprints are enrolled —
    /// re-pairing will still be possible.
    Reversible,
    /// Firmware predates 1.6.12 and does not understand SLOT_STATUS (0x39).
    /// It still enforces the PAIR_INIT/NEEDS_RESET refusal while
    /// fingerprints are enrolled (hidkbd.c:5241), but NOT the 1.6.12+
    /// six-command pre-pair whitelist (hidkbd.c:4908) that blocks
    /// FACTORY_RESET/DELETE_FP once unpaired — so factory-reset stays
    /// reachable as a (destructive) way back. Distinct from `Reversible`:
    /// "keys are kept" would still be materially incomplete here.
    OldFirmware,
    /// This host is the only paired one AND fingerprints are enrolled, on
    /// firmware that enforces the 1.6.12+ pre-pair whitelist.
    OneWayDoor,
    /// `SLOT:STATUS` answered `ERROR:NOT_CONNECTED`: the device is
    /// unreachable. `handle_slot_clear` (`socket.rs`) takes its
    /// not-connected + no-target branch in that case — it only drops local
    /// state and answers `OK:CLEARED_LOCAL_ONLY`, never sending a frame to
    /// the device. The device's active slot is therefore untouched by this
    /// run: not the OneWayDoor/Unknown story, nothing happens on the device.
    DeviceOffline,
    /// The pre-flight queries did not complete for some other reason
    /// (daemon unreachable, malformed answer), so we cannot rule the
    /// OneWayDoor trap out. Treated as dangerous: silence here is not
    /// evidence of safety.
    Unknown,
}

/// Pre-flight the two facts that decide [`UnpairRisk`]: slot occupancy and
/// whether any fingerprint is enrolled. Thin wrapper around
/// [`classify_unpair_risk`] that does the actual (side-effecting) daemon
/// round trips; the decision itself lives in the pure function so it can be
/// unit-tested without a daemon or device.
fn assess_unpair_risk() -> UnpairRisk {
    let Ok(slot_line) = DaemonClient::connect().and_then(|mut c| c.send("SLOT:STATUS")) else {
        return UnpairRisk::Unknown;
    };

    // FP:LIST is a second round trip, so only pay for it in the one case
    // that needs it: this host is the sole occupied slot. Every other
    // outcome is already fully decided by slot_line alone.
    let own_slot_only = matches!(
        parse_slot_status_line(&slot_line),
        Some(SlotStatus::Supported { bitmap, active }) if bitmap == 1u8 << (active - 1)
    );
    let fp_line = if own_slot_only {
        DaemonClient::connect().and_then(|mut c| c.send("FP:LIST")).ok()
    } else {
        None
    };

    classify_unpair_risk(&slot_line, fp_line.as_deref())
}

/// Pure decision over the two raw daemon answer lines `assess_unpair_risk`
/// polls (`SLOT:STATUS` and, when needed, `FP:LIST`).
///
/// `fp_line` is `None` when the caller skipped the `FP:LIST` round trip
/// (every outcome except "this is the sole occupied slot" is decided by
/// `slot_line` alone) — but if that round trip WAS needed and still came
/// back `None`, the omission itself must resolve to [`UnpairRisk::Unknown`],
/// the same as any other unreadable answer.
fn classify_unpair_risk(slot_line: &str, fp_line: Option<&str>) -> UnpairRisk {
    // Exact match, decided before parsing: `parse_slot_status_line` folds
    // every non-OK line to `None`, which would otherwise be
    // indistinguishable from a malformed answer. `handle_slot_status`
    // (socket.rs) only emits this line when `!coord.is_connected`.
    if slot_line.trim() == "ERROR:NOT_CONNECTED" {
        return UnpairRisk::DeviceOffline;
    }

    let (bitmap, active) = match parse_slot_status_line(slot_line) {
        Some(SlotStatus::Supported { bitmap, active }) => (bitmap, active),
        Some(SlotStatus::Unsupported) => return UnpairRisk::OldFirmware,
        None => return UnpairRisk::Unknown,
    };

    // With the other slot still occupied the firmware admits PAIR_INIT again
    // (slot2_enroll is true, hidkbd.c:5241), so this host can always come back.
    if bitmap != 1u8 << (active - 1) {
        return UnpairRisk::Reversible;
    }

    let Some(fp_line) = fp_line else { return UnpairRisk::Unknown };
    let mut fp_parts = fp_line.trim().split(':');
    let fp_bitmap = match (fp_parts.next(), fp_parts.next().map(str::parse::<u8>)) {
        (Some("OK"), Some(Ok(b))) => b,
        _ => return UnpairRisk::Unknown,
    };
    if fp_bitmap == 0 { UnpairRisk::Reversible } else { UnpairRisk::OneWayDoor }
}

/// Clear this computer's own slot. Fingerprints and stored keys stay put —
/// only the pairing key for this host goes away.
///
/// That is only the whole story while re-pairing stays possible. Firmware
/// 1.6.12+ admits just six commands before the active slot is paired
/// (`hidkbd.c:4908`) — GET_STATUS, GET_BATT_RAW, PAIR_INIT, PAIR_CONFIRM,
/// PAIR_STATUS, SLOT_STATUS. FACTORY_RESET and DELETE_FP are not among them,
/// and PAIR_INIT itself returns NEEDS_RESET when no slot is occupied and any
/// fingerprint is enrolled (`hidkbd.c:5241`). A single-host user who unpairs
/// with fingerprints enrolled therefore cannot pair, cannot reset and cannot
/// delete prints from software — only the 3 s button hold is left, and it
/// erases every key on the device. So the risk is resolved first and the
/// consent text tells the truth for the state the user is actually in.
fn unpair_own() {
    let risk = assess_unpair_risk();
    let confirmed = match risk {
        UnpairRisk::Reversible => {
            println!("This clears THIS computer's pairing slot on the device.");
            println!("Fingerprints and stored SSH / OTP / API keys are kept.");
            super::confirm("Unpair this computer?")
        }
        UnpairRisk::OldFirmware => {
            println!(
                "This device's firmware predates dual-host support (older than 1.6.12)."
            );
            println!("Unpairing only clears this computer's local pairing record — the");
            println!("device's own slot stays occupied. If fingerprints are still enrolled,");
            println!("re-pairing afterward will need a factory reset — which erases every");
            println!("fingerprint and every SSH / OTP / API key on the device.");
            println!();
            println!("\x1b[33mAvoid that by deleting fingerprints first, while still paired:\x1b[0m");
            println!("  1. immurok-cli fp list             # see which slots are enrolled");
            println!("  2. immurok-cli fp delete <slot>    # delete each one, while still paired");
            println!("  3. immurok-cli unpair              # then unpair; re-pairing stays plain");
            println!();
            super::confirm("Unpair this computer?")
        }
        UnpairRisk::DeviceOffline => {
            println!("The device is not connected.");
            println!("This clears only this computer's LOCAL pairing record — the device's own");
            println!("slot is left exactly as it is now, and nothing is lost on it.");
            println!();
            println!("Re-pairing later will still require clearing that slot on the device");
            println!("(re-run this command while connected) or a factory reset while");
            println!("fingerprints remain enrolled.");
            super::confirm("Unpair this computer?")
        }
        UnpairRisk::OneWayDoor => {
            println!(
                "\x1b[31mThis computer is the ONLY host paired to this device, and \
                 fingerprints are still enrolled.\x1b[0m"
            );
            println!();
            println!("Unpairing now cannot be undone from software:");
            println!("  · the device refuses a new pairing while any fingerprint is enrolled");
            println!("  · once no slot is paired it also refuses factory reset and fingerprint");
            println!("    deletion — those commands are not accepted before pairing");
            println!("  · the only way back would be holding the device button for 3 seconds,");
            println!("    which erases every fingerprint AND every SSH, OTP and API key stored");
            println!("    on the device. Those private keys exist nowhere else.");
            println!();
            println!("\x1b[33mDo this instead — it keeps the keys:\x1b[0m");
            println!("  1. immurok-cli fp list             # see which slots are enrolled");
            println!("  2. immurok-cli fp delete <slot>    # delete each one, while still paired");
            println!("  3. immurok-cli unpair              # then unpair; pairing still works");
            println!();
            super::confirm_literal_yes("Type 'yes' to unpair anyway:")
        }
        UnpairRisk::Unknown => {
            println!(
                "\x1b[33mCould not check the device's slot and fingerprint state.\x1b[0m"
            );
            println!();
            println!("That state is what decides whether this is safely reversible. It COULD be");
            println!("the one-way-door case: this computer the only paired host, with");
            println!("fingerprints still enrolled. If so, unpairing now cannot be undone from");
            println!("software — the device would refuse pairing, factory reset and fingerprint");
            println!("deletion alike, leaving only the 3-second button hold, which erases every");
            println!("SSH, OTP and API key. Nothing here confirms that IS the state — only that");
            println!("it could not be ruled out.");
            println!();
            println!("Check with 'immurok-cli slot status' and 'immurok-cli fp list' while the");
            println!("device is connected. If this is the only paired host, delete the enrolled");
            println!("fingerprints with 'immurok-cli fp delete <slot>' before unpairing.");
            println!();
            super::confirm_literal_yes("Type 'yes' to unpair anyway:")
        }
    };
    if !confirmed {
        println!("Cancelled.");
        return;
    }

    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    let rsp = client.send("SLOT:CLEAR").unwrap_or_else(|e| {
        super::error_exit(&format!("Unpair failed: {}", e));
    });

    match classify_unpair_response(&rsp) {
        UnpairOutcome::Cleared => println!("\x1b[32mUnpaired.\x1b[0m"),
        UnpairOutcome::ClearedUnconfirmed => {
            // Two possible causes, indistinguishable from here: (1) the
            // device cleared the slot and rebooted before its answer made
            // it back — the normal path; (2) the firmware is too old to
            // know this command at all, so nothing changed on the device
            // even though local pairing is now gone. Say both plainly
            // rather than claiming a success we cannot confirm.
            println!("\x1b[32mLocal pairing cleared.\x1b[0m");
            println!("This is normally because the device rebooted right after clearing its slot.");
            println!("If its firmware is older than 1.6.12, it may not have understood the command");
            println!("at all and could still hold this computer's slot — run 'immurok-cli slot status'");
            println!("(or re-pair) to check.");
        }
        UnpairOutcome::ClearedLocalOnly => {
            println!("\x1b[33mLocal pairing cleared, but the device was not reachable.\x1b[0m");
            println!("Its slot still holds this computer's key. Re-run this while connected,");
            println!("or hold the device button for 3 seconds to wipe it — that also erases");
            println!("every fingerprint and every SSH / OTP / API key on the device.");
        }
        UnpairOutcome::ClearedSlotUnpaired => {
            println!("\x1b[33mLocal pairing cleared — but nothing on the device changed.\x1b[0m");
            println!("The device is connected while sitting on a host slot that is empty, so it");
            println!("refuses every command until that slot is paired. If this computer's key");
            println!("lives in the OTHER slot, that pairing is still there and was NOT removed —");
            println!("touch the host-switch finger to bring the device back to it.");
            println!();
            println!("Run 'immurok-cli slot status' to see which slot the device is on.");
        }
        UnpairOutcome::Refused => {
            eprintln!("\x1b[31mThe device refused to clear this slot.\x1b[0m");
            eprintln!("This computer is still paired. Try again.");
            std::process::exit(1);
        }
        UnpairOutcome::Failed => {
            eprintln!("\x1b[31mUnpair failed: {}\x1b[0m", rsp);
            std::process::exit(1);
        }
    }
}

/// Clear the *other* host's slot — for when that machine is lost or simply
/// not around. The device gates this on a fingerprint, so it needs the owner
/// present; this computer's own pairing is untouched.
fn unpair_other(slot: u8) {
    if slot != 1 && slot != 2 {
        super::error_exit("Slot must be 1 or 2. Run 'immurok-cli slot status' to see both.");
    }

    println!("This clears host slot {} — the OTHER computer's pairing.", slot);
    println!("That computer loses access to the device. This one is unaffected,");
    println!("and no fingerprints or keys are deleted.");
    if !super::confirm(&format!("Clear host slot {}?", slot)) {
        println!("Cancelled.");
        return;
    }

    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    println!("Touch an enrolled fingerprint on the device to authorize…");
    let rsp = client
        .send_with_timeout(&format!("SLOT:CLEAR:{}", slot), Duration::from_secs(60))
        .unwrap_or_else(|e| {
            super::error_exit(&format!("Clear failed: {}", e));
        });

    // Only OK:CLEARED means the far slot went away. The two partial
    // outcomes are answers from the OWN-clear path: `run_unpair_target`
    // resolved the target with its own SLOT:STATUS round trip and the
    // daemon re-resolves independently, so if the active slot changed in
    // between (host-switch finger, or a device reboot into the other slot)
    // the request lands on this computer's own slot instead. Reporting that
    // as "host slot N cleared" would be exactly backwards.
    match classify_unpair_response(&rsp) {
        UnpairOutcome::Cleared => {
            println!("\x1b[32mHost slot {} cleared.\x1b[0m", slot);
        }
        UnpairOutcome::ClearedUnconfirmed | UnpairOutcome::ClearedLocalOnly => {
            println!(
                "\x1b[33mThe device's active slot changed while this ran.\x1b[0m"
            );
            println!(
                "Host slot {} turned out to be THIS computer's own slot — it was cleared,",
                slot
            );
            println!("and this computer's local pairing was dropped with it. The OTHER host's");
            println!("slot was not touched.");
            println!("Run 'immurok-cli slot status' to see the current picture.");
        }
        UnpairOutcome::ClearedSlotUnpaired => {
            println!(
                "\x1b[33mThe device is sitting on an empty slot and refused the command.\x1b[0m"
            );
            println!("Host slot {} was NOT cleared, and nothing else on the device changed.", slot);
            println!("This computer's local pairing record was dropped, though — touch the");
            println!("host-switch finger to bring the device back to a paired slot first.");
            std::process::exit(1);
        }
        UnpairOutcome::Refused => {
            eprintln!("\x1b[31mThe device refused to clear that slot.\x1b[0m");
            eprintln!("Nothing changed. Run 'immurok-cli slot status' to check.");
            std::process::exit(1);
        }
        UnpairOutcome::Failed => {
            eprintln!("\x1b[31mClear failed: {}\x1b[0m", rsp);
            std::process::exit(1);
        }
    }
}

/// Wipe the device. This is what `unpair` used to do; it now stands alone
/// because under dual-host it also destroys the other host's slot.
///
/// The device runs this behind its own fingerprint gate (up to 30 s) on top
/// of the wipe itself, so this uses a 60 s read timeout rather than the
/// client's 60 s default — which is the same number today but, unlike the
/// default, is allowed to grow independently if the gate or wipe ever get
/// slower without forcing every other command to wait longer too.
pub fn run_factory_reset() {
    println!("\x1b[31mFactory reset destroys everything stored on the device:\x1b[0m");
    println!("  · all fingerprints, including the host-switch finger");
    println!("  · all SSH private keys — they exist ONLY on the device");
    println!("  · all OTP secrets and API keys");
    println!("  · both host pairings (this computer and the other one)");
    println!();
    if !super::confirm_literal_yes("This cannot be undone. Type 'yes' to confirm:") {
        println!("Cancelled.");
        return;
    }

    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    println!("Touch an enrolled fingerprint on the device to authorize the wipe…");
    let rsp = client
        .send_with_timeout("PAIR:FACTORY_RESET", Duration::from_secs(60))
        .unwrap_or_else(|e| {
            super::error_exit(&format!("Factory reset failed: {}", e));
        });

    // Exact match, not `contains("RESET")`: the failure response is
    // "ERROR:FACTORY_RESET_FAILED:..." which itself contains the substring
    // "RESET" and would otherwise read as success.
    if rsp == "OK:RESET" {
        println!("\x1b[32mDevice wiped and pairing cleared.\x1b[0m");
    } else {
        eprintln!("\x1b[31mFactory reset failed: {}\x1b[0m", rsp);
        eprintln!("The device was NOT wiped. This computer is still paired.");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── classify_unpair_risk ───────────────────────────────────

    #[test]
    fn risk_device_offline_on_exact_not_connected_line() {
        assert_eq!(classify_unpair_risk("ERROR:NOT_CONNECTED", None), UnpairRisk::DeviceOffline);
        // Whitespace-tolerant, same as every other line parser in this file.
        assert_eq!(
            classify_unpair_risk("  ERROR:NOT_CONNECTED\n", None),
            UnpairRisk::DeviceOffline
        );
    }

    #[test]
    fn risk_old_firmware_when_slot_status_unsupported() {
        assert_eq!(classify_unpair_risk("OK:UNSUPPORTED", None), UnpairRisk::OldFirmware);
    }

    #[test]
    fn risk_unknown_on_malformed_slot_line() {
        assert_eq!(classify_unpair_risk("garbage", None), UnpairRisk::Unknown);
        assert_eq!(classify_unpair_risk("", None), UnpairRisk::Unknown);
    }

    #[test]
    fn risk_reversible_when_other_slot_still_occupied() {
        // active=1, bitmap=0x03: slot 2 also occupied — fp_line not even
        // needed, this must resolve without it.
        assert_eq!(classify_unpair_risk("OK:3:1", None), UnpairRisk::Reversible);
    }

    #[test]
    fn risk_unknown_when_own_slot_only_but_fp_line_missing() {
        // active=1, bitmap=0x01: sole occupied slot, but the FP:LIST round
        // trip that decides Reversible-vs-OneWayDoor never came back.
        assert_eq!(classify_unpair_risk("OK:1:1", None), UnpairRisk::Unknown);
    }

    #[test]
    fn risk_unknown_on_malformed_fp_line() {
        assert_eq!(classify_unpair_risk("OK:1:1", Some("garbage")), UnpairRisk::Unknown);
        assert_eq!(classify_unpair_risk("OK:1:1", Some("OK:notanumber")), UnpairRisk::Unknown);
    }

    #[test]
    fn risk_reversible_when_own_slot_only_but_no_fingerprints() {
        assert_eq!(classify_unpair_risk("OK:1:1", Some("OK:0")), UnpairRisk::Reversible);
    }

    #[test]
    fn risk_one_way_door_when_own_slot_only_and_fingerprints_enrolled() {
        assert_eq!(classify_unpair_risk("OK:1:1", Some("OK:1")), UnpairRisk::OneWayDoor);
        assert_eq!(classify_unpair_risk("OK:2:2", Some("OK:31")), UnpairRisk::OneWayDoor);
    }

    #[test]
    fn risk_not_connected_wins_even_over_malformed_looking_input() {
        // Regression pin: the exact-line check must run before
        // parse_slot_status_line, or this would fall through to Unknown
        // instead of the more specific DeviceOffline.
        assert_ne!(classify_unpair_risk("ERROR:NOT_CONNECTED", None), UnpairRisk::Unknown);
    }
}
