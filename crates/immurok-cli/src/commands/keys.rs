//! `immurok-cli key` — SSH/OTP/API key management subcommands.
//!
//! Keys are managed via the daemon socket which proxies BLE commands to the device.
//! SSH public keys and OTP/API key names are cached locally by the daemon.

use crate::socket_client::DaemonClient;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::SecretKey;

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

/// List keys in a category. Reads from daemon's local cache files.
pub fn run_list(category: &str) {
    let cat = parse_category(category);
    let home = std::env::var("HOME").unwrap_or_default();
    let immurok_dir = std::path::PathBuf::from(&home)
        .join(immurok_common::protocol::IMMUROK_DIR);

    match cat {
        "ssh" => {
            let ssh_path = immurok_dir.join(immurok_common::protocol::SSH_KEYS_FILE);
            match std::fs::read_to_string(&ssh_path) {
                Ok(contents) => {
                    let entries: Vec<serde_json::Value> =
                        serde_json::from_str(&contents).unwrap_or_default();
                    if entries.is_empty() {
                        println!("No SSH keys.");
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
                Err(_) => println!("No SSH keys cached. Connect device to sync."),
            }
        }
        _ => {
            let names_path = immurok_dir.join(immurok_common::protocol::KEY_NAMES_FILE);
            match std::fs::read_to_string(&names_path) {
                Ok(contents) => {
                    let entries: Vec<serde_json::Value> =
                        serde_json::from_str(&contents).unwrap_or_default();
                    let filtered: Vec<&serde_json::Value> = entries
                        .iter()
                        .filter(|e: &&serde_json::Value| {
                            e["category"].as_str() == Some(cat)
                        })
                        .collect();
                    if filtered.is_empty() {
                        println!("No {} keys.", cat.to_uppercase());
                        return;
                    }
                    println!("{} keys:", cat.to_uppercase());
                    for entry in &filtered {
                        let idx = entry["index"].as_u64().unwrap_or(0);
                        let name = entry["name"].as_str().unwrap_or("-");
                        println!("  [{}] {}", idx, name);
                    }
                }
                Err(_) => println!("No {} keys cached. Connect device to sync.", cat.to_uppercase()),
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

/// Category selector for [`build_key_add_cmd`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum KeyAddCat {
    Otp,
    Api,
}

/// Build the daemon socket command that adds one OTP/API entry, mirroring
/// the firmware entry layout (otp_entry_t = name[30]+service[30]+secret;
/// api_entry_t = name[32]+value). Shared by the CLI interactive flow and
/// the TUI Keys panel. `service` is OTP-only and ignored for API.
pub fn build_key_add_cmd(cat: KeyAddCat, name: &str, service: &str, secret: &[u8]) -> String {
    use immurok_common::protocol::{NAME_LEN_API, NAME_LEN_OTP, SERVICE_LEN_OTP};

    fn pad_field(text: &str, len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        let bytes = text.as_bytes();
        let n = bytes.len().min(len - 1);
        buf[..n].copy_from_slice(&bytes[..n]);
        buf
    }

    let (mut payload, verb) = match cat {
        KeyAddCat::Otp => {
            let mut p = pad_field(name, NAME_LEN_OTP);
            p.extend_from_slice(&pad_field(service, SERVICE_LEN_OTP));
            (p, "OTP_IMPORT")
        }
        KeyAddCat::Api => (pad_field(name, NAME_LEN_API), "API_IMPORT"),
    };
    payload.extend_from_slice(secret);
    format!("KEY:{}:{}", verb, hex::encode(&payload))
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
    let home = std::env::var("HOME").unwrap_or_default();
    let ssh_path = std::path::PathBuf::from(&home)
        .join(immurok_common::protocol::IMMUROK_DIR)
        .join(immurok_common::protocol::SSH_KEYS_FILE);

    let contents = match std::fs::read_to_string(&ssh_path) {
        Ok(c) => c,
        Err(_) => {
            super::error_exit("No SSH keys cached. Connect device to sync.");
        }
    };

    let entries: Vec<serde_json::Value> =
        serde_json::from_str(&contents).unwrap_or_default();

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
    let home = std::env::var("HOME").unwrap_or_default();
    let immurok_dir = std::path::PathBuf::from(&home)
        .join(immurok_common::protocol::IMMUROK_DIR);
    if category == "ssh" {
        let path = immurok_dir.join(immurok_common::protocol::SSH_KEYS_FILE);
        let contents = std::fs::read_to_string(&path).unwrap_or_default();
        let entries: Vec<serde_json::Value> = serde_json::from_str(&contents).unwrap_or_default();
        return entries.len();
    }
    let path = immurok_dir.join(immurok_common::protocol::KEY_NAMES_FILE);
    let contents = std::fs::read_to_string(&path).unwrap_or_default();
    let entries: Vec<serde_json::Value> = serde_json::from_str(&contents).unwrap_or_default();
    entries
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

    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    // Build name payload (16 bytes, null-padded)
    let mut name_buf = vec![0u8; 16];
    let name_bytes = name.as_bytes();
    let copy_len = name_bytes.len().min(15);
    name_buf[..copy_len].copy_from_slice(&name_bytes[..copy_len]);

    let hex_name = hex::encode(&name_buf);
    let cmd = format!("KEY:GENERATE:{}", hex_name);

    println!("Generating SSH keypair '{}' on device...", name);

    let rsp = client.send(&cmd).unwrap_or_else(|e| {
        super::error_exit(&format!("Failed to generate key: {}", e));
    });

    if rsp.starts_with("OK") {
        println!("\x1b[32mSSH keypair '{}' generated.\x1b[0m", name);
        println!("Use 'immurok-cli key list ssh' to see the new key.");
    } else {
        eprintln!("Generate failed: {}", rsp);
        eprintln!("Note: key generation requires daemon support (future implementation).");
    }
}

/// Import an existing SSH private key (ECDSA P-256 only) to the device.
pub fn run_import_ssh(name: &str, keyfile: &str) {
    check_capacity_or_exit("ssh");

    // 1. Read key file
    let key_data = match std::fs::read_to_string(keyfile) {
        Ok(d) => d,
        Err(e) => super::error_exit(&format!("Cannot read key file '{}': {}", keyfile, e)),
    };

    // 2. Parse the key to extract (privkey_32B, pubkey_64B)
    let (privkey, pubkey) = if key_data.contains("-----BEGIN OPENSSH PRIVATE KEY-----") {
        parse_openssh_key(&key_data)
    } else if key_data.contains("-----BEGIN EC PRIVATE KEY-----") {
        parse_sec1_pem(&key_data)
    } else {
        super::error_exit(
            "Unsupported key format. Expected OpenSSH or SEC1 PEM (ECDSA P-256).",
        );
    };

    assert_eq!(privkey.len(), 32);
    assert_eq!(pubkey.len(), 64);

    // 2b. Pub/priv pair self-consistency check (mirrors macOS commit
    //     863b1d1). Without this, a privkey from one keyfile + pubkey from
    //     another would import quietly and silently fail the first time
    //     ssh-agent tries to sign. We derive the expected pubkey from the
    //     parsed privkey and compare against what we parsed from the file.
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    let secret = match p256::SecretKey::from_slice(&privkey) {
        Ok(s) => s,
        Err(e) => {
            super::error_exit(&format!("Invalid private key: {}", e));
        }
    };
    let derived_pub = secret.public_key().to_encoded_point(false);
    let derived_bytes = derived_pub.as_bytes(); // 0x04 || x[32] || y[32] = 65B
    if derived_bytes.len() != 65 || derived_bytes[0] != 0x04 {
        super::error_exit("Failed to derive uncompressed public key from private key");
    }
    if derived_bytes[1..65] != pubkey[..] {
        super::error_exit(
            "Public/private key pair mismatch — the keyfile contains a privkey \
             that does not derive the embedded pubkey. Re-export the key with \
             `ssh-keygen -y -f <privkey>` to sanity-check.",
        );
    }

    // 3. Convert public key from big-endian to little-endian (device uses LE)
    let mut pubkey_le = [0u8; 64];
    // x component: first 32 bytes, reversed
    let (x_be, y_be) = pubkey.split_at(32);
    let mut x_le: Vec<u8> = x_be.to_vec();
    x_le.reverse();
    let mut y_le: Vec<u8> = y_be.to_vec();
    y_le.reverse();
    pubkey_le[..32].copy_from_slice(&x_le);
    pubkey_le[32..].copy_from_slice(&y_le);

    // 4. Build device format: name(16B) + pubkey_LE(64B) + privkey(32B) = 112 bytes
    let mut device_data = vec![0u8; 112];
    // Name: 16 bytes null-padded
    let name_bytes = name.as_bytes();
    let copy_len = name_bytes.len().min(15);
    device_data[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
    // Pubkey LE
    device_data[16..80].copy_from_slice(&pubkey_le);
    // Privkey LE (device stores all numbers in little-endian)
    let mut privkey_le = privkey.clone();
    privkey_le.reverse();
    device_data[80..112].copy_from_slice(&privkey_le);

    // 5. Send to daemon
    let hex_data = hex::encode(&device_data);
    let cmd = format!("KEY:IMPORT:{}", hex_data);

    println!("Importing SSH key '{}' to device...", name);

    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => super::error_exit(&e),
    };

    let rsp = client.send(&cmd).unwrap_or_else(|e| {
        super::error_exit(&format!("Failed to import key: {}", e));
    });

    if rsp.starts_with("OK") {
        println!("\x1b[32mSSH key '{}' imported successfully.\x1b[0m", name);
        println!("Use 'immurok-cli key list ssh' to see the imported key.");
    } else {
        eprintln!("Import failed: {}", rsp);
        std::process::exit(1);
    }
}

/// Parse an OpenSSH private key (ECDSA P-256 only, unencrypted).
/// Returns (privkey_32B, pubkey_64B) in big-endian.
fn parse_openssh_key(pem_text: &str) -> (Vec<u8>, Vec<u8>) {
    use base64::Engine;

    // Strip header/footer and decode base64
    let b64: String = pem_text
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<&str>>()
        .join("");

    let data = base64::engine::general_purpose::STANDARD
        .decode(&b64)
        .unwrap_or_else(|e| {
            super::error_exit(&format!("Invalid base64 in OpenSSH key: {}", e));
        });

    // Magic: "openssh-key-v1\0"
    let magic = b"openssh-key-v1\0";
    if data.len() < magic.len() || &data[..magic.len()] != magic {
        super::error_exit("Not a valid OpenSSH key (bad magic)");
    }
    let mut pos = magic.len();

    // Helper: read u32 big-endian
    let read_u32 = |data: &[u8], pos: &mut usize| -> u32 {
        if *pos + 4 > data.len() {
            super::error_exit("Truncated OpenSSH key (reading u32)");
        }
        let val = u32::from_be_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
        *pos += 4;
        val
    };

    // Helper: read SSH string (u32 length + data)
    let read_string = |data: &[u8], pos: &mut usize| -> Vec<u8> {
        let len = read_u32(data, pos) as usize;
        if *pos + len > data.len() {
            super::error_exit("Truncated OpenSSH key (reading string)");
        }
        let s = data[*pos..*pos + len].to_vec();
        *pos += len;
        s
    };

    // cipher_name
    let cipher = read_string(&data, &mut pos);
    if cipher != b"none" {
        super::error_exit("Encrypted OpenSSH keys are not supported. Decrypt first with: ssh-keygen -p -f <keyfile>");
    }

    // kdf_name
    let kdf = read_string(&data, &mut pos);
    if kdf != b"none" {
        super::error_exit("Encrypted OpenSSH keys are not supported.");
    }

    // kdf_options
    let _kdf_opts = read_string(&data, &mut pos);

    // num_keys
    let num_keys = read_u32(&data, &mut pos);
    if num_keys != 1 {
        super::error_exit(&format!("Expected 1 key, found {}", num_keys));
    }

    // Public key blob (skip)
    let _pubkey_blob = read_string(&data, &mut pos);

    // Private section
    let priv_section = read_string(&data, &mut pos);
    let mut ppos = 0;

    // check1, check2
    let check1 = read_u32(&priv_section, &mut ppos);
    let check2 = read_u32(&priv_section, &mut ppos);
    if check1 != check2 {
        super::error_exit("OpenSSH key check values don't match (corrupted or encrypted?)");
    }

    // key_type
    let key_type = read_string(&priv_section, &mut ppos);
    let key_type_str = String::from_utf8_lossy(&key_type);
    if key_type_str != "ecdsa-sha2-nistp256" {
        super::error_exit(&format!(
            "Unsupported key type '{}'. Only ecdsa-sha2-nistp256 is supported.",
            key_type_str
        ));
    }

    // curve
    let curve = read_string(&priv_section, &mut ppos);
    if curve != b"nistp256" {
        super::error_exit("Unsupported curve. Only nistp256 (P-256) is supported.");
    }

    // public_key: 65 bytes (04 || x || y)
    let pub_bytes = read_string(&priv_section, &mut ppos);
    if pub_bytes.len() != 65 || pub_bytes[0] != 0x04 {
        super::error_exit(&format!(
            "Invalid public key length {} (expected 65 bytes uncompressed)",
            pub_bytes.len()
        ));
    }
    let pubkey = pub_bytes[1..65].to_vec(); // x(32) + y(32)

    // private_key: 32 bytes (may be 33 with leading 0x00 mpint padding)
    let mut privkey = read_string(&priv_section, &mut ppos);
    if privkey.len() == 33 && privkey[0] == 0x00 {
        privkey = privkey[1..].to_vec();
    }
    if privkey.len() != 32 {
        super::error_exit(&format!(
            "Invalid private key length {} (expected 32 bytes)",
            privkey.len()
        ));
    }

    (privkey, pubkey)
}

/// Parse a SEC1 PEM private key (-----BEGIN EC PRIVATE KEY-----).
/// Returns (privkey_32B, pubkey_64B) in big-endian.
fn parse_sec1_pem(pem_text: &str) -> (Vec<u8>, Vec<u8>) {
    use base64::Engine;

    // Strip header/footer and decode base64
    let b64: String = pem_text
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<&str>>()
        .join("");

    let der = base64::engine::general_purpose::STANDARD
        .decode(&b64)
        .unwrap_or_else(|e| {
            super::error_exit(&format!("Invalid base64 in PEM: {}", e));
        });

    // Minimal DER parse: SEQUENCE { INTEGER(1), OCTET_STRING(privkey), [0]OID, [1]BIT_STRING(pubkey) }
    let mut pos = 0;

    // Read DER tag + length
    let read_tag_len = |data: &[u8], pos: &mut usize| -> (u8, usize) {
        if *pos >= data.len() {
            super::error_exit("Truncated DER data");
        }
        let tag = data[*pos];
        *pos += 1;
        if *pos >= data.len() {
            super::error_exit("Truncated DER length");
        }
        let len_byte = data[*pos];
        *pos += 1;
        let len = if len_byte & 0x80 == 0 {
            len_byte as usize
        } else {
            let num_bytes = (len_byte & 0x7F) as usize;
            if *pos + num_bytes > data.len() {
                super::error_exit("Truncated DER multi-byte length");
            }
            let mut l: usize = 0;
            for i in 0..num_bytes {
                l = (l << 8) | data[*pos + i] as usize;
            }
            *pos += num_bytes;
            l
        };
        (tag, len)
    };

    // Outer SEQUENCE
    let (tag, _seq_len) = read_tag_len(&der, &mut pos);
    if tag != 0x30 {
        super::error_exit("Invalid DER: expected SEQUENCE");
    }

    // INTEGER (version = 1)
    let (tag, int_len) = read_tag_len(&der, &mut pos);
    if tag != 0x02 {
        super::error_exit("Invalid DER: expected INTEGER (version)");
    }
    pos += int_len; // skip version value

    // OCTET STRING (private key)
    let (tag, oct_len) = read_tag_len(&der, &mut pos);
    if tag != 0x04 {
        super::error_exit("Invalid DER: expected OCTET STRING (private key)");
    }
    if oct_len != 32 {
        super::error_exit(&format!(
            "Invalid private key length {} (expected 32)",
            oct_len
        ));
    }
    let privkey = der[pos..pos + 32].to_vec();
    pos += 32;

    // Try to find public key in [1] context tag (0xA1)
    let mut pubkey: Option<Vec<u8>> = None;
    while pos < der.len() {
        let (tag, content_len) = read_tag_len(&der, &mut pos);
        if tag == 0xA1 {
            // BIT STRING inside context [1]
            if pos + content_len <= der.len() {
                let inner_start = pos;
                let (inner_tag, inner_len) = read_tag_len(&der, &mut pos);
                if inner_tag == 0x03 && inner_len >= 66 {
                    // BIT STRING: first byte is unused-bits count (0), then 04||x||y
                    if der[pos] == 0x00 && der[pos + 1] == 0x04 {
                        pubkey = Some(der[pos + 2..pos + 66].to_vec());
                    }
                }
                pos = inner_start + content_len;
            }
        } else {
            pos += content_len;
        }
    }

    // If no public key in file, derive from private key
    let pubkey = pubkey.unwrap_or_else(|| {
        let sk = SecretKey::from_slice(&privkey).unwrap_or_else(|e| {
            super::error_exit(&format!("Invalid P-256 private key: {}", e));
        });
        let pk = sk.public_key();
        let point = pk.to_encoded_point(false); // uncompressed: 04||x||y
        let bytes = point.as_bytes();
        if bytes.len() != 65 || bytes[0] != 0x04 {
            super::error_exit("Failed to derive uncompressed public key");
        }
        bytes[1..65].to_vec()
    });

    if pubkey.len() != 64 {
        super::error_exit(&format!(
            "Invalid public key length {} (expected 64)",
            pubkey.len()
        ));
    }

    (privkey, pubkey)
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

/// Decode a string in RFC 4648 base32 (uppercase A-Z + 2-7), stripping any
/// `=` padding. Returns None on illegal characters. Lowercase is mapped to
/// uppercase so common copy-paste from auth-app exports works without
/// normalization.
pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '=')
        .map(|c| c.to_ascii_uppercase())
        .collect();

    let mut out = Vec::with_capacity(cleaned.len() * 5 / 8);
    let mut bits: u32 = 0;
    let mut nbits: u8 = 0;
    for c in cleaned.chars() {
        let v: u32 = match c {
            'A'..='Z' => (c as u32) - ('A' as u32),
            '2'..='7' => (c as u32) - ('2' as u32) + 26,
            _ => return None,
        };
        bits = (bits << 5) | v;
        nbits += 5;
        if nbits >= 8 {
            nbits -= 8;
            out.push(((bits >> nbits) & 0xFF) as u8);
        }
    }
    Some(out)
}

/// Split an andOTP / otpauth `issuer` + `label` pair into the device entry's
/// separate (service, account-name) fields, each truncated to its firmware
/// field width on a char boundary. If the account is empty the service is
/// promoted to the name so the entry stays identifiable.
fn split_otp_fields(issuer: &str, label: &str) -> (String, String) {
    let issuer = issuer.trim();
    let label = label.trim();

    let (service, account) = if !issuer.is_empty() {
        (issuer.to_string(), label.to_string())
    } else if let Some((svc, acc)) = label.split_once(':') {
        (svc.trim().to_string(), acc.trim().to_string())
    } else {
        (String::new(), label.to_string())
    };

    let (service, account) = if account.is_empty() {
        (String::new(), service)
    } else {
        (service, account)
    };

    (
        truncate_utf8(&service, immurok_common::protocol::SERVICE_LEN_OTP - 1),
        truncate_utf8(&account, immurok_common::protocol::NAME_LEN_OTP - 1),
    )
}

/// Truncate to at most `max` bytes without splitting a UTF-8 sequence.
fn truncate_utf8(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut cut = max;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s[..cut].to_string()
}

/// One parsed OTP entry ready to ship to the daemon.
struct OtpEntry {
    name: String,
    service: String,
    secret: Vec<u8>,
}

/// Parse andOTP JSON backup: a plain array of objects with
/// `secret/issuer/label/type/algorithm/digits/period`. Defaults match the
/// macOS implementation: missing `type/algorithm/digits/period` → TOTP /
/// SHA1 / 6 / 30 (all "supported"). Returns (entries, skipped_count).
fn parse_andotp_json(data: &str) -> Option<(Vec<OtpEntry>, usize)> {
    let arr: Vec<serde_json::Value> = serde_json::from_str(data).ok()?;
    let mut entries = Vec::new();
    let mut skipped = 0;
    for v in arr {
        let kind = v.get("type").and_then(|x| x.as_str()).unwrap_or("TOTP").to_uppercase();
        let algo = v.get("algorithm").and_then(|x| x.as_str()).unwrap_or("SHA1").to_uppercase();
        let digits = v.get("digits").and_then(|x| x.as_u64()).unwrap_or(6);
        let period = v.get("period").and_then(|x| x.as_u64()).unwrap_or(30);
        if kind != "TOTP" || algo != "SHA1" || digits != 6 || period != 30 {
            skipped += 1;
            continue;
        }
        let secret_str = match v.get("secret").and_then(|x| x.as_str()) {
            Some(s) => s,
            None => {
                skipped += 1;
                continue;
            }
        };
        let secret = match base32_decode(secret_str) {
            Some(b) if !b.is_empty() => b,
            _ => {
                skipped += 1;
                continue;
            }
        };
        let issuer = v.get("issuer").and_then(|x| x.as_str()).unwrap_or("");
        let label = v.get("label").and_then(|x| x.as_str()).unwrap_or("");
        let (service, name) = split_otp_fields(issuer, label);
        if name.is_empty() {
            skipped += 1;
            continue;
        }
        entries.push(OtpEntry { name, service, secret });
    }
    Some((entries, skipped))
}

/// Parse CSV with one `otpauth://totp/...` URI per line (the format
/// `key export` writes). The first row is dropped if it looks like a
/// header (`name,...`). Anything that doesn't reduce to a usable
/// (name, base32 secret) triplet is silently dropped — the CSV branch is
/// looser than JSON since we already trust that the file is something we
/// produced.
fn parse_csv_otpauth(content: &str) -> Vec<OtpEntry> {
    let lines: Vec<&str> = content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    let start = if lines
        .first()
        .map(|l| l.to_lowercase().starts_with("name"))
        .unwrap_or(false)
    {
        1
    } else {
        0
    };

    let mut out = Vec::new();
    for line in &lines[start..] {
        let pos = match line.find("otpauth://") {
            Some(p) => p,
            None => continue,
        };
        let uri = &line[pos..];
        let entry = match parse_otpauth_uri(uri) {
            Some(e) => e,
            None => continue,
        };
        out.push(entry);
    }
    out
}

/// Extract (name, secret_bytes) from an `otpauth://totp/path?secret=...&issuer=...`
/// URI. Honors percent-encoding in the path. Returns None for non-TOTP
/// URIs or missing secret.
fn parse_otpauth_uri(uri: &str) -> Option<OtpEntry> {
    // We only need a permissive split: scheme://host/path?query
    let rest = uri.strip_prefix("otpauth://")?;
    // host is "totp" or "hotp" — only totp is supported on-device
    let (host, after_host) = rest.split_once('/')?;
    if !host.eq_ignore_ascii_case("totp") {
        return None;
    }
    let (path_raw, query_raw) = match after_host.split_once('?') {
        Some((p, q)) => (p, q),
        None => (after_host, ""),
    };
    let path = url_decode(path_raw);

    let mut secret: Option<String> = None;
    let mut issuer = String::new();
    let mut digits: u64 = 6;
    let mut period: u64 = 30;
    let mut algo = String::from("SHA1");
    for pair in query_raw.split('&') {
        let (k, v) = match pair.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        let decoded = url_decode(v);
        match k.to_lowercase().as_str() {
            "secret" => secret = Some(decoded),
            "issuer" => issuer = decoded,
            "digits" => digits = decoded.parse().unwrap_or(6),
            "period" => period = decoded.parse().unwrap_or(30),
            "algorithm" => algo = decoded.to_uppercase(),
            _ => {}
        }
    }

    if digits != 6 || period != 30 || algo != "SHA1" {
        return None;
    }

    let secret = secret?;
    let secret_bytes = base32_decode(&secret)?;
    if secret_bytes.is_empty() {
        return None;
    }

    let (service, name) = if !issuer.is_empty() {
        if let Some((_, acc)) = path.split_once(':') {
            // both issuer query AND service-prefixed path → prefer issuer for svc.
            split_otp_fields(&issuer, acc)
        } else {
            split_otp_fields(&issuer, &path)
        }
    } else {
        split_otp_fields("", &path)
    };

    if name.is_empty() {
        return None;
    }
    Some(OtpEntry { name, service, secret: secret_bytes })
}

/// Tiny percent-decoder for otpauth URI components — we don't need any of
/// the full URL crate's complexity here.
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Build the device payload for one OTP entry, mirroring otp_entry_t:
/// name[30] + service[30] (both null-filled) + secret bytes. Sent as
/// `KEY:OTP_IMPORT:<hex>` and the daemon stages + commits it via
/// KEY_WRITE + KEY_COMMIT (FP gated on the first commit, cooldown rides
/// the rest).
fn build_otp_entry_payload(entry: &OtpEntry) -> Vec<u8> {
    fn pad(text: &str, len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        let bytes = text.as_bytes();
        let n = bytes.len().min(len - 1);
        buf[..n].copy_from_slice(&bytes[..n]);
        buf
    }
    let mut buf = pad(&entry.name, immurok_common::protocol::NAME_LEN_OTP);
    buf.extend_from_slice(&pad(&entry.service, immurok_common::protocol::SERVICE_LEN_OTP));
    buf.extend_from_slice(&entry.secret);
    buf
}

/// `key import-otp <file>` — bulk-import OTP secrets from a CSV or JSON
/// file. Confirms with the user (showing skip count) before writing.
pub fn run_import_otp(file: &str) {
    let content = match std::fs::read_to_string(file) {
        Ok(c) => c,
        Err(e) => super::error_exit(&format!("Cannot read '{}': {}", file, e)),
    };

    let is_json = std::path::Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    let (entries, skipped) = if is_json {
        match parse_andotp_json(&content) {
            Some(v) => v,
            None => super::error_exit(
                "Unrecognized JSON format. Expected andOTP backup (array of \
                 {secret, issuer, label, type, algorithm, digits, period}).",
            ),
        }
    } else {
        (parse_csv_otpauth(&content), 0)
    };

    if entries.is_empty() {
        if skipped > 0 {
            super::error_exit(&format!(
                "All {} entries skipped: only standard TOTP / HMAC-SHA1 / 6-digit / 30-second supported.",
                skipped
            ));
        } else {
            super::error_exit(
                "No importable OTP entries found. CSV expects one otpauth:// URI per \
                 line; JSON expects andOTP backup.",
            );
        }
    }

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
        // Daemon closes the socket after each request, so reconnect per
        // entry. Loose enough — there are at most KEY_MAX_OTP=128 of these.
        let mut client = match DaemonClient::connect() {
            Ok(c) => c,
            Err(e) => super::error_exit(&e),
        };
        let payload = build_otp_entry_payload(entry);
        let cmd = format!("KEY:OTP_IMPORT:{}", hex::encode(&payload));
        match client.send(&cmd) {
            Ok(rsp) => {
                if rsp.starts_with("OK") {
                    imported += 1;
                    println!(
                        "  [{:>3}/{}] {} → {}",
                        i + 1,
                        entries.len(),
                        entry.name,
                        rsp.trim()
                    );
                } else {
                    eprintln!(
                        "  [{:>3}/{}] {} → FAILED: {}",
                        i + 1,
                        entries.len(),
                        entry.name,
                        rsp.trim()
                    );
                    eprintln!("Aborting (remaining entries not imported).");
                    break;
                }
            }
            Err(e) => {
                eprintln!(
                    "  [{:>3}/{}] {} → SOCKET ERROR: {}",
                    i + 1,
                    entries.len(),
                    entry.name,
                    e
                );
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
