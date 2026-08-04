//! `immurok-cli slot` — dual-host slot inspection.

use immurok_common::dual_host::{parse_slot_status_line, SlotStatus};

use crate::socket_client::DaemonClient;

pub fn run_status() {
    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    let rsp = client.send("SLOT:STATUS").unwrap_or_else(|e| {
        super::error_exit(&format!("Failed to read slot status: {}", e));
    });

    match parse_slot_status_line(&rsp) {
        Some(SlotStatus::Supported { bitmap, active }) => print_hosts(bitmap, active),
        Some(SlotStatus::Unsupported) => {
            println!("This device's firmware has no dual-host support (needs 1.6.12 or newer).");
            println!("Run 'immurok-cli fw check' to see whether an update is available.");
        }
        None => super::error_exit(&format!("Failed to read slot status: {}", rsp)),
    }
}

fn print_hosts(bitmap: u8, active: u8) {
    println!("Hosts");
    for slot in [1u8, 2u8] {
        let occupied = bitmap & (1 << (slot - 1)) != 0;
        // The active slot is the identity the device is presenting right now.
        // Only call it "this computer" when it is actually bound: the device
        // can sit on an EMPTY slot (it boots into one, or the host-switch
        // finger moved it there), and labelling that as this computer's slot
        // sent a user hunting for a pairing problem that did not exist —
        // their key was in the other slot the whole time.
        let suffix = match (slot == active, occupied) {
            (true, true) => " · active (this computer)",
            (true, false) => " · active — the device is presenting this empty slot",
            _ => "",
        };
        println!(
            "  {} Host {}   {}{}",
            if occupied { "\x1b[32m●\x1b[0m" } else { "○" },
            slot,
            if occupied { "bound" } else { "empty" },
            suffix
        );
    }
    println!();
    // Sitting on an empty slot means the firmware's pre-pair whitelist
    // refuses almost everything (fingerprints, keys, unpair), so say what to
    // do about it before the user runs into a wall of refusals.
    if bitmap & (1 << (active - 1)) == 0 && bitmap & 0x03 != 0 {
        println!(
            "\x1b[33mThe device is on an empty slot, so it refuses fingerprint, key and\x1b[0m"
        );
        println!("\x1b[33munpair commands. Touch the host-switch finger to return it to the\x1b[0m");
        println!("\x1b[33mbound slot, or pair this computer into the empty one.\x1b[0m");
        println!();
    }
    if bitmap & 0x03 == 0x03 {
        println!("Both slots are taken. Free one with 'immurok-cli unpair' (this computer)");
        println!("or 'immurok-cli unpair --slot N' (the other one, needs a fingerprint).");
    } else {
        println!("A free slot can be claimed by running 'immurok-cli pair' on another computer.");
    }
    println!("Switch between hosts by touching the host-switch finger (slot 5) on the device.");
}
