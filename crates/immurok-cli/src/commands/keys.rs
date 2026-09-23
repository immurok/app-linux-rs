//! `immurok-cli key` — SSH/OTP/API key management subcommands.
//!
//! Keys are managed via the daemon socket which proxies BLE commands to the device.
//! SSH public keys and OTP/API key names are cached locally by the daemon.

use crate::socket_client::DaemonClient;
use immurok_client::keys::{add_otp, generate_ssh, import_ssh};
use immurok_client::keys_import::{base32_decode, build_key_add_cmd, parse_otp_import_file, KeyAddCat};

fn parse_category(cat: &str) -> &'static str {
    match cat.to_lowercase().as_str() {
        "ssh" => "ssh",
        "otp" => "otp",
        "api" => "api",
        _ => {
            super::error_exit("Invalid category. Must be: ssh, otp, api");
        }
    }
}

/// List keys in a category, from the cache the daemon maintains.
pub fn run_list(category: &str) {
    let cat = parse_category(category);

    match cat {
        "ssh" => {
            let entries = crate::socket_client::fetch_key_cache("ssh");
            if entries.is_empty() {
                println!("No SSH keys cached. Connect device to sync.");
                return;
            }
            println!("SSH keys:");
            for entry in &entries {
                let idx = entry["index"].as_u64().unwrap_or(0);
                let name = entry["name"].as_str().unwrap_or("-");
                let fp = entry["fingerprint"].as_str().unwrap_or("-");
                println!("  [{}] {} ({})", idx, name, fp);
            }
        }
        _ => {
            let entries = crate::socket_client::fetch_key_cache("names");
            let filtered: Vec<&serde_json::Value> = entries
                .iter()
                .filter(|e: &&serde_json::Value| e["category"].as_str() == Some(cat))
                .collect();
            if filtered.is_empty() {
                println!("No {} keys cached. Connect device to sync.", cat.to_uppercase());
                return;
            }
            println!("{} keys:", cat.to_uppercase());
            for entry in &filtered {
                let idx = entry["index"].as_u64().unwrap_or(0);
                let name = entry["name"].as_str().unwrap_or("-");
                println!("  [{}] {}", idx, name);
            }
        }
    }
}

/// Add a key interactively.
pub fn run_add(category: &str) {
    let cat = parse_category(category);

    if cat == "ssh" {
        super::error_exit(
            "SSH keys are generated on-device: use `immurok-cli key generate <name>` \
             or `immurok-cli key import <name> <file>`.",
        );
    }

    check_capacity_or_exit(cat);

    println!("Adding {} key (interactive).", cat.to_uppercase());

    // Read name
    eprint!("Key name: ");
    let mut name = String::new();
    std::io::stdin()
        .read_line(&mut name)
        .expect("Failed to read input");
    let name = name.trim();
    if name.is_empty() {
        super::error_exit("Name cannot be empty.");
    }

    // OTP entries carry a separate issuer/service field on the device.
    let service = if cat == "otp" {
        eprint!("Service / issuer (optional): ");
        let mut s = String::new();
        std::io::stdin()
            .read_line(&mut s)
            .expect("Failed to read input");
        s.trim().to_string()
    } else {
        String::new()
    };

    // Read the secret / value
    let prompt = if cat == "otp" {
        "TOTP secret (base32): "
    } else {
        "API key value: "
    };
    eprint!("{}", prompt);
    let mut s = String::new();
    std::io::stdin()
        .read_line(&mut s)
        .expect("Failed to read input");
    let s = s.trim().to_string();
    if s.is_empty() {
        super::error_exit("Secret cannot be empty.");
    }

    let cmd = match cat {
        "otp" => {
            let secret = match base32_decode(&s) {
                Some(b) if !b.is_empty() => b,
                _ => super::error_exit("Invalid base32 secret."),
            };
            if secret.len() > immurok_common::protocol::SECRET_LEN_OTP {
                super::error_exit(&format!(
                    "Secret too long: {} bytes decoded (device limit {}).",
                    secret.len(),
                    immurok_common::protocol::SECRET_LEN_OTP
                ));
            }
            build_key_add_cmd(KeyAddCat::Otp, name, &service, &secret)
        }
        "api" => {
            if s.len() > immurok_common::protocol::VALUE_LEN_API {
                super::error_exit(&format!(
                    "Value too long: {} bytes (device limit {}).",
                    s.len(),
                    immurok_common::protocol::VALUE_LEN_API
                ));
            }
            build_key_add_cmd(KeyAddCat::Api, name, "", s.as_bytes())
        }
        _ => unreachable!(),
    };

    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    println!("Touch the sensor to authorize the write…");
    let rsp = client.send(&cmd).unwrap_or_else(|e| {
        super::error_exit(&format!("Failed to add key: {}", e));
    });

    if rsp.starts_with("OK") {
        println!("\x1b[32m{} key '{}' added.\x1b[0m", cat.to_uppercase(), name);
    } else {
        eprintln!("Add key failed: {}", rsp);
    }
}

/// Delete a key.
pub fn run_delete(category: &str, index: u8) {
    let cat = parse_category(category);
    let cat_byte: u8 = match cat {
        "ssh" => 0,
        "otp" => 1,
        "api" => 2,
        _ => unreachable!(),
    };

    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    let cmd = format!("KEY:DELETE:{}:{}", cat_byte, index);
    let rsp = client.send(&cmd).unwrap_or_else(|e| {
        super::error_exit(&format!("Failed to delete key: {}", e));
    });

    if rsp.starts_with("OK") {
        println!("\x1b[32m{} key [{}] deleted.\x1b[0m", cat.to_uppercase(), index);
    } else {
        eprintln!("Delete failed: {}", rsp);
    }
}

/// Export an SSH public key in authorized_keys format.
pub fn run_export_ssh(index: u8) {
    let entries = crate::socket_client::fetch_key_cache("ssh");
    if entries.is_empty() {
        super::error_exit("No SSH keys cached. Connect device to sync.");
    }

    let entry = entries
        .iter()
        .find(|e: &&serde_json::Value| e["index"].as_u64() == Some(index as u64));

    match entry {
        Some(e) => {
            let name = e["name"].as_str().unwrap_or("immurok");
            let blob_b64 = e["public_key_blob"].as_str().unwrap_or("");
            if blob_b64.is_empty() {
                super::error_exit("No public key data for this entry.");
            }
            // Output in authorized_keys format
            println!("ecdsa-sha2-nistp256 {} {}", blob_b64, name);
        }
        None => {
            super::error_exit(&format!("SSH key index {} not found.", index));
        }
    }
}

/// Count cached entries for a category (post-most-recent sync). Used by
/// capacity guards before write operations — keystore-full conditions
/// previously failed silently on the device side. Mirrors macOS commit
/// 11d3f40.
fn cached_count(category: &str) -> usize {
    if category == "ssh" {
        return crate::socket_client::fetch_key_cache("ssh").len();
    }
    crate::socket_client::fetch_key_cache("names")
        .iter()
        .filter(|e| e["category"].as_str() == Some(category))
        .count()
}

/// Refuse the operation when the category is at firmware-defined capacity.
fn check_capacity_or_exit(category: &str) {
    let (current, max, label) = match category {
        "ssh" => (
            cached_count("ssh"),
            immurok_common::protocol::KEY_MAX_SSH as usize,
            "SSH",
        ),
        "otp" => (
            cached_count("otp"),
            immurok_common::protocol::KEY_MAX_OTP as usize,
            "OTP",
        ),
        "api" => (
            cached_count("api"),
            immurok_common::protocol::KEY_MAX_API as usize,
            "API",
        ),
        _ => return,
    };
    if current >= max {
        super::error_exit(&format!(
            "{} keystore is full ({}/{}). Delete an entry first: \
             `immurok-cli key delete {} <index>`.",
            label, current, max, category
        ));
    }
}

/// Generate an SSH keypair on device.
pub fn run_generate_ssh(name: &str) {
    check_capacity_or_exit("ssh");
    println!("Generating SSH keypair '{}' on device...", name);
    match generate_ssh(name) {
        Ok(()) => {
            println!("\x1b[32mSSH keypair '{}' generated.\x1b[0m", name);
            println!("Use 'immurok-cli key list ssh' to see the new key.");
        }
        Err(e) => eprintln!("Generate failed: {}", e),
    }
}

/// Import an existing SSH private key (ECDSA P-256 only) to the device.
pub fn run_import_ssh(name: &str, keyfile: &str) {
    check_capacity_or_exit("ssh");
    let key_data = match std::fs::read_to_string(keyfile) {
        Ok(d) => d,
        Err(e) => super::error_exit(&format!("Cannot read key file '{}': {}", keyfile, e)),
    };
    println!("Importing SSH key '{}' to device...", name);
    match import_ssh(name, &key_data) {
        Ok(()) => {
            println!("\x1b[32mSSH key '{}' imported successfully.\x1b[0m", name);
            println!("Use 'immurok-cli key list ssh' to see the imported key.");
        }
        Err(e) => {
            eprintln!("Import failed: {}", e);
            std::process::exit(1);
        }
    }
}

// ── OTP bulk import ─────────────────────────────────────────
//
// Mirrors macOS commit dcf72af (1.20 build 373). Two source formats:
//   - CSV: one `otpauth://totp/...?secret=...&issuer=...` URI per line
//   - JSON: andOTP backup (plain JSON array of entries) — see
//     https://github.com/andOTP/andOTP
//
// Firmware only supports standard TOTP / HMAC-SHA1 / 6-digit / 30-second.
// Anything else (HOTP / STEAM / SHA256 / 7-8 digit / non-30s period) is
// skipped and counted; the user is told the skip count before confirming.

/// `key import-otp <file>` — bulk-import OTP secrets from a CSV or JSON
/// file. Confirms with the user (showing skip count) before writing.
pub fn run_import_otp(file: &str) {
    let content = match std::fs::read_to_string(file) {
        Ok(c) => c,
        Err(e) => super::error_exit(&format!("Cannot read '{}': {}", file, e)),
    };

    let (entries, skipped) = match parse_otp_import_file(file, &content) {
        Ok(v) => v,
        Err(e) => super::error_exit(&e),
    };

    let cached = cached_count("otp");
    let max = immurok_common::protocol::KEY_MAX_OTP as usize;
    let remaining = max.saturating_sub(cached);
    if entries.len() > remaining {
        super::error_exit(&format!(
            "Cannot import {} entries: only {} OTP slots remaining (max {}). \
             Delete some entries first.",
            entries.len(),
            remaining,
            max
        ));
    }

    let skip_note = if skipped > 0 {
        format!(
            " ({} skipped: only standard TOTP / HMAC-SHA1 / 6-digit / 30-second supported)",
            skipped
        )
    } else {
        String::new()
    };

    eprint!(
        "Import {} OTP entr{}{} — touch the sensor when prompted. Continue? [y/N] ",
        entries.len(),
        if entries.len() == 1 { "y" } else { "ies" },
        skip_note,
    );
    let mut ans = String::new();
    if std::io::stdin().read_line(&mut ans).is_err() {
        return;
    }
    if !matches!(ans.trim().to_lowercase().as_str(), "y" | "yes") {
        println!("Cancelled.");
        return;
    }

    let mut imported = 0;
    for (i, entry) in entries.iter().enumerate() {
        match add_otp(entry) {
            Ok(()) => {
                imported += 1;
                println!("  [{:>3}/{}] {} → OK", i + 1, entries.len(), entry.name);
            }
            Err(e) => {
                eprintln!("  [{:>3}/{}] {} → FAILED: {}", i + 1, entries.len(), entry.name, e);
                eprintln!("Aborting (remaining entries not imported).");
                break;
            }
        }
    }

    println!(
        "\x1b[32mImported {}/{} OTP entries.\x1b[0m{}",
        imported,
        entries.len(),
        skip_note,
    );
}

/// Get a TOTP code for an OTP key.
pub fn run_otp(index: u8) {
    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    let cmd = format!("KEY:OTP:{}", index);
    let rsp = client.send(&cmd).unwrap_or_else(|e| {
        super::error_exit(&format!("Failed to get OTP: {}", e));
    });

    if let Some(code) = rsp.strip_prefix("OK:") {
        println!("{}", code);
    } else {
        eprintln!("OTP failed: {}", rsp);
        std::process::exit(1);
    }
}
