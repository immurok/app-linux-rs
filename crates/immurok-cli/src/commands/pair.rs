//! `immurok-cli pair` / `immurok-cli unpair` — pairing management.

use crate::socket_client::DaemonClient;

pub fn run_pair() {
    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    // Check current pairing status
    let pair_rsp = client.send("PAIR:STATUS").unwrap_or_default();
    if pair_rsp.contains("PAIRED") && !pair_rsp.contains("UNPAIRED") {
        eprintln!("Already paired. Unpair first with: immurok-cli unpair");
        std::process::exit(1);
    }

    println!("Starting pairing... Press the device button within 30s to confirm.");

    // PAIR:START can take up to ~45s (35s button-wait window + handshake)
    let rsp = client.send("PAIR:START").unwrap_or_else(|e| {
        super::error_exit(&format!("Pairing failed: {}", e));
    });

    if rsp.contains("PAIRED") {
        println!("\x1b[32mPairing successful!\x1b[0m");
    } else {
        eprintln!("\x1b[31mPairing failed: {}\x1b[0m", rsp);
        std::process::exit(1);
    }
}

pub fn run_unpair() {
    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    println!("Resetting pairing data...");

    let rsp = client.send("PAIR:RESET").unwrap_or_else(|e| {
        super::error_exit(&format!("Unpair failed: {}", e));
    });

    if rsp.contains("RESET") {
        println!("\x1b[32mPairing data cleared.\x1b[0m");
    } else {
        eprintln!("\x1b[31mUnpair failed: {}\x1b[0m", rsp);
        std::process::exit(1);
    }
}
