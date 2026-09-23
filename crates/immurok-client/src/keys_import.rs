//! OTP / API entry parsing and device payload layout, shared by CLI, TUI
//! and GUI. Pure functions — no daemon I/O here (that lives in `keys`).
//!
//! Firmware only supports standard TOTP / HMAC-SHA1 / 6-digit / 30-second.
//! Anything else (HOTP / STEAM / SHA256 / 7-8 digit / non-30s period) is
//! skipped and counted; callers show the skip count before writing.

use immurok_common::protocol::{NAME_LEN_API, NAME_LEN_OTP, SERVICE_LEN_OTP};

/// One parsed OTP entry ready to ship to the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtpEntry {
    pub name: String,
    pub service: String,
    pub secret: Vec<u8>,
}

/// Decode RFC 4648 base32 (A-Z, 2-7). `=` padding and whitespace are
/// stripped, lowercase is accepted. `None` on any other character.
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

/// Truncate to at most `max` bytes without splitting a UTF-8 sequence.
pub fn truncate_utf8(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut cut = max;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s[..cut].to_string()
}

/// Split an andOTP / otpauth `issuer` + `label` pair into the device's
/// separate (service, name) fields, each cut to its firmware width. An
/// empty account promotes the service to the name so the entry stays
/// identifiable.
pub fn split_otp_fields(issuer: &str, label: &str) -> (String, String) {
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
        truncate_utf8(&service, SERVICE_LEN_OTP - 1),
        truncate_utf8(&account, NAME_LEN_OTP - 1),
    )
}

/// Tiny percent-decoder for otpauth URI components.
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

/// `otpauth://totp/<label>?secret=…&issuer=…` → entry. `None` for HOTP,
/// non-default digits/period/algorithm, or a missing/invalid secret.
pub fn parse_otpauth_uri(uri: &str) -> Option<OtpEntry> {
    let rest = uri.trim().strip_prefix("otpauth://")?;
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
        let Some((k, v)) = pair.split_once('=') else { continue };
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

    let secret_bytes = base32_decode(&secret?)?;
    if secret_bytes.is_empty() {
        return None;
    }

    let (service, name) = if !issuer.is_empty() {
        match path.split_once(':') {
            // Both an issuer query and a service-prefixed path: the query wins.
            Some((_, acc)) => split_otp_fields(&issuer, acc),
            None => split_otp_fields(&issuer, &path),
        }
    } else {
        split_otp_fields("", &path)
    };
    if name.is_empty() {
        return None;
    }
    Some(OtpEntry { name, service, secret: secret_bytes })
}

/// The `secret=` query value of an otpauth URI, percent-decoded, without
/// validating it — what the GUI puts back into the Secret field after
/// auto-filling from a pasted URI.
pub fn otpauth_secret_field(uri: &str) -> Option<String> {
    let (_, query_raw) = uri.split_once('?')?;
    for pair in query_raw.split('&') {
        let Some((k, v)) = pair.split_once('=') else { continue };
        if k.eq_ignore_ascii_case("secret") {
            return Some(url_decode(v));
        }
    }
    None
}

/// CSV with one `otpauth://totp/…` URI per line. No header heuristic: any
/// line without an `otpauth://` URI (a header row included) simply has
/// nothing to find and is dropped.
pub fn parse_csv_otpauth(content: &str) -> Vec<OtpEntry> {
    content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .filter_map(|line| line.find("otpauth://").and_then(|p| parse_otpauth_uri(&line[p..])))
        .collect()
}

/// andOTP JSON backup: a plain array of `{secret, issuer, label, type,
/// algorithm, digits, period}`. Missing fields default to TOTP / SHA1 / 6 /
/// 30. Returns `(entries, skipped)`; `None` if the text is not such an array.
pub fn parse_andotp_json(data: &str) -> Option<(Vec<OtpEntry>, usize)> {
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
        let secret = match v.get("secret").and_then(|x| x.as_str()).and_then(base32_decode) {
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

/// Parse an import file by extension: `.json` → andOTP, anything else →
/// CSV/otpauth lines. `Err` explains why nothing usable was found.
pub fn parse_otp_import_file(path_hint: &str, content: &str) -> Result<(Vec<OtpEntry>, usize), String> {
    let is_json = std::path::Path::new(path_hint)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("json"))
        .unwrap_or(false);
    let (entries, skipped) = if is_json {
        parse_andotp_json(content).ok_or_else(|| {
            "Unrecognized JSON format. Expected an andOTP backup (array of \
             {secret, issuer, label, type, algorithm, digits, period})."
                .to_string()
        })?
    } else {
        (parse_csv_otpauth(content), 0)
    };
    if entries.is_empty() {
        return Err(if skipped > 0 {
            format!(
                "All {} entries skipped: only standard TOTP / HMAC-SHA1 / 6-digit / 30-second supported.",
                skipped
            )
        } else {
            "No otpauth://totp entries found in the file.".to_string()
        });
    }
    Ok((entries, skipped))
}

fn pad_field(text: &str, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    let text = truncate_utf8(text, len - 1);
    let bytes = text.as_bytes();
    buf[..bytes.len()].copy_from_slice(bytes);
    buf
}

/// Device payload for one OTP entry, mirroring `otp_entry_t`:
/// name[30] + service[30] (NUL-filled) + secret bytes.
pub fn build_otp_entry_payload(entry: &OtpEntry) -> Vec<u8> {
    let mut buf = pad_field(&entry.name, NAME_LEN_OTP);
    buf.extend_from_slice(&pad_field(&entry.service, SERVICE_LEN_OTP));
    buf.extend_from_slice(&entry.secret);
    buf
}

/// Category selector for [`build_key_add_cmd`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAddCat {
    Otp,
    Api,
}

/// Daemon command adding one OTP/API entry (`KEY:OTP_IMPORT:<hex>` /
/// `KEY:API_IMPORT:<hex>`), mirroring the firmware entry layouts
/// (otp_entry_t = name[30]+service[30]+secret; api_entry_t = name[32]+value).
/// `service` is OTP-only and ignored for API.
pub fn build_key_add_cmd(cat: KeyAddCat, name: &str, service: &str, secret: &[u8]) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_decodes_rfc4648_and_tolerates_case_padding_whitespace() {
        assert_eq!(base32_decode("JBSWY3DPEHPK3PXP").unwrap(), b"Hello!\xde\xad\xbe\xef");
        assert_eq!(base32_decode("jbsw y3dp ehpk 3pxp====").unwrap(), b"Hello!\xde\xad\xbe\xef");
        assert!(base32_decode("JBSW1").is_none()); // '1' is not base32
    }

    #[test]
    fn otpauth_uri_with_issuer_and_percent_encoding() {
        let e = parse_otpauth_uri(
            "otpauth://totp/GitHub:alice%40example.com?secret=JBSWY3DPEHPK3PXP&issuer=GitHub",
        )
        .unwrap();
        assert_eq!(e.service, "GitHub");
        assert_eq!(e.name, "alice@example.com");
        assert_eq!(e.secret, b"Hello!\xde\xad\xbe\xef");
    }

    #[test]
    fn otpauth_uri_rejects_hotp_and_nonstandard_params() {
        assert!(parse_otpauth_uri("otpauth://hotp/x?secret=JBSWY3DP").is_none());
        assert!(parse_otpauth_uri("otpauth://totp/x?secret=JBSWY3DP&digits=8").is_none());
        assert!(parse_otpauth_uri("otpauth://totp/x?secret=JBSWY3DP&algorithm=SHA256").is_none());
        assert!(parse_otpauth_uri("otpauth://totp/x").is_none());
    }

    #[test]
    fn otpauth_secret_field_percent_decodes_and_handles_missing_secret() {
        assert_eq!(
            otpauth_secret_field("otpauth://totp/x?issuer=A&secret=JBSWY3DP%3D%3D"),
            Some("JBSWY3DP==".to_string())
        );
        assert_eq!(otpauth_secret_field("otpauth://totp/x?issuer=A"), None);
    }

    #[test]
    fn split_fields_promotes_service_when_account_empty_and_truncates() {
        assert_eq!(split_otp_fields("", "Acme"), (String::new(), "Acme".into()));
        assert_eq!(split_otp_fields("", "Acme:bob"), ("Acme".into(), "bob".into()));
        let long = "é".repeat(40); // 80 bytes
        let (_, name) = split_otp_fields("", &long);
        assert!(name.len() <= 29 && name.is_char_boundary(name.len()));
    }

    #[test]
    fn csv_skips_header_and_non_uri_lines() {
        let csv = "name,uri\nfoo,otpauth://totp/A?secret=JBSWY3DP\njunk\n";
        let v = parse_csv_otpauth(csv);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "A");

        // No header heuristic: a first data line whose entry name happens
        // to start with "name" must not be mistaken for a header row.
        let csv2 = "otpauth://totp/namecheap?secret=JBSWY3DP\n";
        let v2 = parse_csv_otpauth(csv2);
        assert_eq!(v2.len(), 1);
        assert_eq!(v2[0].name, "namecheap");
    }

    #[test]
    fn andotp_json_counts_unsupported_entries() {
        let json = r#"[
          {"secret":"JBSWY3DP","issuer":"A","label":"a","type":"TOTP"},
          {"secret":"JBSWY3DP","issuer":"B","label":"b","type":"HOTP"},
          {"secret":"JBSWY3DP","issuer":"C","label":"c","digits":8}
        ]"#;
        let (v, skipped) = parse_andotp_json(json).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(skipped, 2);
        assert!(parse_andotp_json("{}").is_none());
    }

    #[test]
    fn import_file_dispatches_on_extension() {
        let (v, _) = parse_otp_import_file("x.JSON", r#"[{"secret":"JBSWY3DP","label":"a"}]"#).unwrap();
        assert_eq!(v[0].name, "a");
        let (v, _) = parse_otp_import_file("x.csv", "otpauth://totp/B?secret=JBSWY3DP").unwrap();
        assert_eq!(v[0].name, "B");
        assert!(parse_otp_import_file("x.json", "not json").is_err());
        assert!(parse_otp_import_file("x.csv", "nothing here").is_err());
    }

    #[test]
    fn otp_payload_is_name30_service30_secret() {
        let e = OtpEntry { name: "n".into(), service: "s".into(), secret: vec![1, 2, 3] };
        let p = build_otp_entry_payload(&e);
        assert_eq!(p.len(), 30 + 30 + 3);
        assert_eq!(p[0], b'n');
        assert_eq!(p[30], b's');
        assert_eq!(&p[60..], &[1, 2, 3]);
    }

    #[test]
    fn key_add_cmd_layouts() {
        let otp = build_key_add_cmd(KeyAddCat::Otp, "n", "s", &[9]);
        assert!(otp.starts_with("KEY:OTP_IMPORT:"));
        assert_eq!(otp.len(), "KEY:OTP_IMPORT:".len() + (30 + 30 + 1) * 2);
        let api = build_key_add_cmd(KeyAddCat::Api, "n", "", b"v");
        assert!(api.starts_with("KEY:API_IMPORT:"));
        assert_eq!(api.len(), "KEY:API_IMPORT:".len() + (32 + 1) * 2);
    }

    #[test]
    fn pad_field_truncates_on_utf8_boundary() {
        // 40-byte UTF-8 string (20 × "é" = 20 × 2 bytes) at position 29 would split "é"
        // truncate_utf8 should stop at 28 bytes (14 complete "é" chars)
        let e = OtpEntry { name: "é".repeat(20), service: "s".into(), secret: vec![1] };
        let p = build_otp_entry_payload(&e);
        // NAME_LEN_OTP = 30: name field occupies p[0..30]
        // Name should be truncated to 28 bytes (14 complete 2-byte UTF-8 chars) + 1 NUL padding
        assert_eq!(p[28], 0); // Must have NUL at position 28 (last valid UTF-8 byte)
        assert_eq!(p[29], 0); // Position 29 is NUL padding
        // Verify the truncated name is valid UTF-8
        assert!(std::str::from_utf8(&p[..28]).is_ok());
        // And decodes to exactly 14 "é" chars
        assert_eq!(std::str::from_utf8(&p[..28]).unwrap(), "é".repeat(14));
    }
}
