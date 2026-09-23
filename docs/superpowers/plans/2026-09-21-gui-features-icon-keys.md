# immurok-gui Features 页 / 指纹图标 / Keys 页补全 — 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把功能开关拆成独立页、用自带 SVG 修好指纹图标、给 Keys 页补齐 添加/导入/容量/空状态/行内取码，同时把 OTP/SSH 解析代码从 CLI 下沉到 `immurok-client` 供三端共用。

**Architecture:** 解析与 payload 组装（纯函数，无 I/O）进 `immurok-client::keys_import` / `ssh_import`，发送函数进 `immurok-client::keys`；CLI/TUI 改为调用它们，行为不变。GUI 侧 Features 页复用 Dashboard 的 2 s 轮询结果；Keys 页新增 `+` 按钮 → `key_add_dialog.rs`；指纹图标走 gresource。

**Tech Stack:** Rust 2021、gtk4-rs 0.9 / libadwaita-rs 0.7（只开 `v1_1`）、glib-build-tools 0.20、p256 0.13、base64 0.22、serde_json。

**Spec:** `docs/superpowers/specs/2026-09-21-gui-features-icon-keys-design.md`

## Global Constraints

- 项目下限 Ubuntu 22.04 = GTK 4.6 / libadwaita 1.1。绑定 crate 只开 `libadwaita` 的 `v1_1` feature；禁止 `adw::EntryRow`、`adw::MessageDialog`、`adw::ToolbarView`、`gtk::FileDialog`。
- 用户可见文案只用英文（Linux 端约定）。
- 所有 daemon 往返走 `pages::run_blocking`，不得在 GTK 主线程上阻塞。
- 错误不吞：对话框/toast 里显示 daemon 或解析返回的原文（`glib::markup_escape_text` 转义后）。
- 每个 Task 结束 `cargo test --workspace` 全绿再提交；提交信息用中文，前缀 `client:` / `cli:` / `gui:`。
- 工作目录：`app-linux-rs/`（仓库根是上一级 `imPress-v1`，`git` 命令在哪里跑都行）。

---

### Task 1: `immurok-client::keys_import` — OTP 解析与 payload

**Files:**
- Create: `crates/immurok-client/src/keys_import.rs`
- Modify: `crates/immurok-client/src/lib.rs`（加 `pub mod keys_import;`）

**Interfaces:**
- Consumes: `immurok_common::protocol::{NAME_LEN_OTP, SERVICE_LEN_OTP, NAME_LEN_API, SECRET_LEN_OTP}`
- Produces:
  - `pub struct OtpEntry { pub name: String, pub service: String, pub secret: Vec<u8> }`
  - `pub fn base32_decode(s: &str) -> Option<Vec<u8>>`
  - `pub fn truncate_utf8(s: &str, max: usize) -> String`
  - `pub fn split_otp_fields(issuer: &str, label: &str) -> (String, String)` → `(service, name)`
  - `pub fn parse_otpauth_uri(uri: &str) -> Option<OtpEntry>`
  - `pub fn parse_csv_otpauth(content: &str) -> Vec<OtpEntry>`
  - `pub fn parse_andotp_json(data: &str) -> Option<(Vec<OtpEntry>, usize)>`
  - `pub fn parse_otp_import_file(path_hint: &str, content: &str) -> Result<(Vec<OtpEntry>, usize), String>`
  - `pub fn build_otp_entry_payload(entry: &OtpEntry) -> Vec<u8>`
  - `pub enum KeyAddCat { Otp, Api }`
  - `pub fn build_key_add_cmd(cat: KeyAddCat, name: &str, service: &str, secret: &[u8]) -> String`

- [ ] **Step 1: 写失败测试**

在 `crates/immurok-client/src/keys_import.rs` 末尾（文件先只放测试 + `use` 行也编不过，没关系，本步就是要它失败）：

```rust
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
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p immurok-client keys_import`
Expected: 编译错误 `unresolved import` / `cannot find function base32_decode`（`lib.rs` 还没注册模块也算失败）。

- [ ] **Step 3: 实现**

`crates/immurok-client/src/lib.rs` 在 `pub mod keys;` 后加：

```rust
pub mod keys_import;
```

`crates/immurok-client/src/keys_import.rs`（把测试块留在文件末尾）：

```rust
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

/// CSV with one `otpauth://totp/…` URI per line. A first row starting with
/// `name` is treated as a header. Lines that do not parse are dropped.
pub fn parse_csv_otpauth(content: &str) -> Vec<OtpEntry> {
    let lines: Vec<&str> = content.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    let start = usize::from(lines.first().map(|l| l.to_lowercase().starts_with("name")).unwrap_or(false));
    lines[start..]
        .iter()
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
    let bytes = text.as_bytes();
    let n = bytes.len().min(len - 1);
    buf[..n].copy_from_slice(&bytes[..n]);
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
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p immurok-client keys_import`
Expected: `test result: ok. 9 passed`

- [ ] **Step 5: 提交**

```bash
git add crates/immurok-client/src/keys_import.rs crates/immurok-client/src/lib.rs
git commit -m "client: OTP 解析与 payload 组装下沉为 keys_import（CLI/TUI/GUI 共用）"
```

---

### Task 2: `immurok-client::ssh_import` — OpenSSH / SEC1 私钥解析

**Files:**
- Create: `crates/immurok-client/src/ssh_import.rs`
- Modify: `crates/immurok-client/Cargo.toml`（加 `p256 = { version = "0.13", features = ["ecdh"] }`）
- Modify: `crates/immurok-client/src/lib.rs`（加 `pub mod ssh_import;`）

**Interfaces:**
- Consumes: `base64`, `p256`（已在 workspace）
- Produces:
  - `pub fn parse_openssh_key(pem_text: &str) -> Result<(Vec<u8>, Vec<u8>), String>` → `(privkey_32_be, pubkey_64_be)`
  - `pub fn parse_sec1_pem(pem_text: &str) -> Result<(Vec<u8>, Vec<u8>), String>`
  - `pub fn build_ssh_name_payload(name: &str) -> Vec<u8>`（16 字节 NUL 填充，最多 15 字节）
  - `pub fn build_ssh_import_payload(name: &str, pem_text: &str) -> Result<Vec<u8>, String>`（112 字节：name[16] + pubkey_LE[64] + privkey_LE[32]，含公私钥一致性校验）

测试固件（P-256，未加密，同一把钥的两种编码；priv 以 `4a3f861d` 开头，pub.x 以 `4f3d540c` 开头，pub.y 以 `64fe15` 结尾）：

```
-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAaAAAABNlY2RzYS
1zaGEyLW5pc3RwMjU2AAAACG5pc3RwMjU2AAAAQQRPPVQM5IubdgjP7hppZgl8ohYUtKmb
bMKL25uMuJDsf5H4AdzUVC97r2FyzB0/N+nUilcaxdmRIz767UYGZP4VAAAAoLfyPGK38j
xiAAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBE89VAzki5t2CM/u
GmlmCXyiFhS0qZtswovbm4y4kOx/kfgB3NRUL3uvYXLMHT836dSKVxrF2ZEjPvrtRgZk/h
UAAAAgSj+GHWttlSNlX/iCTDDA3aED/6v67geoseeej7jdNRkAAAAHZml4dHVyZQE=
-----END OPENSSH PRIVATE KEY-----
```

```
-----BEGIN EC PRIVATE KEY-----
MHcCAQEEIEo/hh1rbZUjZV/4gkwwwN2hA/+r+u4HqLHnno+43TUZoAoGCCqGSM49
AwEHoUQDQgAETz1UDOSLm3YIz+4aaWYJfKIWFLSpm2zCi9ubjLiQ7H+R+AHc1FQv
e69hcswdPzfp1IpXGsXZkSM++u1GBmT+FQ==
-----END EC PRIVATE KEY-----
```

- [ ] **Step 1: 写失败测试**

`crates/immurok-client/src/ssh_import.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const OPENSSH: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAaAAAABNlY2RzYS
1zaGEyLW5pc3RwMjU2AAAACG5pc3RwMjU2AAAAQQRPPVQM5IubdgjP7hppZgl8ohYUtKmb
bMKL25uMuJDsf5H4AdzUVC97r2FyzB0/N+nUilcaxdmRIz767UYGZP4VAAAAoLfyPGK38j
xiAAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBE89VAzki5t2CM/u
GmlmCXyiFhS0qZtswovbm4y4kOx/kfgB3NRUL3uvYXLMHT836dSKVxrF2ZEjPvrtRgZk/h
UAAAAgSj+GHWttlSNlX/iCTDDA3aED/6v67geoseeej7jdNRkAAAAHZml4dHVyZQE=
-----END OPENSSH PRIVATE KEY-----
";

    const SEC1: &str = "-----BEGIN EC PRIVATE KEY-----
MHcCAQEEIEo/hh1rbZUjZV/4gkwwwN2hA/+r+u4HqLHnno+43TUZoAoGCCqGSM49
AwEHoUQDQgAETz1UDOSLm3YIz+4aaWYJfKIWFLSpm2zCi9ubjLiQ7H+R+AHc1FQv
e69hcswdPzfp1IpXGsXZkSM++u1GBmT+FQ==
-----END EC PRIVATE KEY-----
";

    #[test]
    fn both_encodings_parse_to_the_same_keypair() {
        let (p1, q1) = parse_openssh_key(OPENSSH).unwrap();
        let (p2, q2) = parse_sec1_pem(SEC1).unwrap();
        assert_eq!(p1, p2);
        assert_eq!(q1, q2);
        assert_eq!(p1.len(), 32);
        assert_eq!(q1.len(), 64);
        assert_eq!(&p1[..4], &[0x4a, 0x3f, 0x86, 0x1d]);
        assert_eq!(&q1[..4], &[0x4f, 0x3d, 0x54, 0x0c]);
        assert_eq!(&q1[61..], &[0x64, 0xfe, 0x15]);
    }

    #[test]
    fn import_payload_is_little_endian_112_bytes() {
        let p = build_ssh_import_payload("work", OPENSSH).unwrap();
        assert_eq!(p.len(), 112);
        assert_eq!(&p[..5], b"work\0");
        // pubkey.x LE: first byte is the last BE byte of x
        let (_, q) = parse_openssh_key(OPENSSH).unwrap();
        assert_eq!(p[16], q[31]);
        assert_eq!(p[16 + 63], q[32]); // y LE first byte at offset 48 … last at 79 = y[0]
        assert_eq!(p[80], 0x19); // priv LE first byte = last BE byte (…dd 35 19)
    }

    #[test]
    fn name_payload_truncates_to_15_bytes() {
        let n = build_ssh_name_payload("0123456789abcdefXYZ");
        assert_eq!(n.len(), 16);
        assert_eq!(&n[..15], b"0123456789abcde");
        assert_eq!(n[15], 0);
    }

    #[test]
    fn unsupported_inputs_give_specific_errors() {
        assert!(build_ssh_import_payload("x", "garbage").unwrap_err().contains("Unsupported key format"));
        assert!(parse_openssh_key("-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----")
            .unwrap_err()
            .contains("bad magic"));
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p immurok-client ssh_import`
Expected: 编译错误（模块/函数不存在）。

- [ ] **Step 3: 实现**

`Cargo.toml` `[dependencies]` 加：

```toml
p256 = { version = "0.13", features = ["ecdh"] }
```

`lib.rs` 加 `pub mod ssh_import;`。

`crates/immurok-client/src/ssh_import.rs`（测试块保留在末尾）：

```rust
//! ECDSA P-256 private key parsing for `KEY:IMPORT`. Two encodings:
//! OpenSSH (`-----BEGIN OPENSSH PRIVATE KEY-----`, unencrypted) and SEC1
//! PEM (`-----BEGIN EC PRIVATE KEY-----`). Pure functions; every failure is
//! an `Err(String)` the caller can show verbatim.

use base64::Engine;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::SecretKey;

fn pem_body_b64(pem_text: &str) -> Result<Vec<u8>, String> {
    let b64: String = pem_text.lines().filter(|l| !l.starts_with("-----")).collect::<Vec<_>>().join("");
    base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| format!("Invalid base64 in key file: {}", e))
}

fn read_u32(data: &[u8], pos: &mut usize) -> Result<u32, String> {
    if *pos + 4 > data.len() {
        return Err("Truncated OpenSSH key (reading u32)".into());
    }
    let v = u32::from_be_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    Ok(v)
}

fn read_string(data: &[u8], pos: &mut usize) -> Result<Vec<u8>, String> {
    let len = read_u32(data, pos)? as usize;
    if *pos + len > data.len() {
        return Err("Truncated OpenSSH key (reading string)".into());
    }
    let s = data[*pos..*pos + len].to_vec();
    *pos += len;
    Ok(s)
}

/// OpenSSH private key → `(privkey_32_be, pubkey_64_be)`. Only
/// `ecdsa-sha2-nistp256`, unencrypted.
pub fn parse_openssh_key(pem_text: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    let data = pem_body_b64(pem_text)?;
    let magic = b"openssh-key-v1\0";
    if data.len() < magic.len() || &data[..magic.len()] != magic {
        return Err("Not a valid OpenSSH key (bad magic)".into());
    }
    let mut pos = magic.len();

    if read_string(&data, &mut pos)? != b"none" {
        return Err("Encrypted OpenSSH keys are not supported. Decrypt first with: ssh-keygen -p -f <keyfile>".into());
    }
    if read_string(&data, &mut pos)? != b"none" {
        return Err("Encrypted OpenSSH keys are not supported.".into());
    }
    let _kdf_opts = read_string(&data, &mut pos)?;
    let num_keys = read_u32(&data, &mut pos)?;
    if num_keys != 1 {
        return Err(format!("Expected 1 key, found {}", num_keys));
    }
    let _pubkey_blob = read_string(&data, &mut pos)?;
    let priv_section = read_string(&data, &mut pos)?;

    let mut p = 0;
    let check1 = read_u32(&priv_section, &mut p)?;
    let check2 = read_u32(&priv_section, &mut p)?;
    if check1 != check2 {
        return Err("OpenSSH key check values don't match (corrupted or encrypted?)".into());
    }
    let key_type = read_string(&priv_section, &mut p)?;
    if key_type != b"ecdsa-sha2-nistp256" {
        return Err(format!(
            "Unsupported key type '{}'. Only ecdsa-sha2-nistp256 is supported.",
            String::from_utf8_lossy(&key_type)
        ));
    }
    if read_string(&priv_section, &mut p)? != b"nistp256" {
        return Err("Unsupported curve. Only nistp256 (P-256) is supported.".into());
    }
    let pub_bytes = read_string(&priv_section, &mut p)?;
    if pub_bytes.len() != 65 || pub_bytes[0] != 0x04 {
        return Err(format!("Invalid public key length {} (expected 65 bytes uncompressed)", pub_bytes.len()));
    }
    let pubkey = pub_bytes[1..65].to_vec();

    let mut privkey = read_string(&priv_section, &mut p)?;
    if privkey.len() == 33 && privkey[0] == 0x00 {
        privkey.remove(0);
    }
    if privkey.len() != 32 {
        return Err(format!("Invalid private key length {} (expected 32 bytes)", privkey.len()));
    }
    Ok((privkey, pubkey))
}

fn der_tag_len(data: &[u8], pos: &mut usize) -> Result<(u8, usize), String> {
    if *pos + 2 > data.len() {
        return Err("Truncated DER data".into());
    }
    let tag = data[*pos];
    let len_byte = data[*pos + 1];
    *pos += 2;
    let len = if len_byte & 0x80 == 0 {
        len_byte as usize
    } else {
        let n = (len_byte & 0x7F) as usize;
        if *pos + n > data.len() {
            return Err("Truncated DER multi-byte length".into());
        }
        let mut l = 0usize;
        for i in 0..n {
            l = (l << 8) | data[*pos + i] as usize;
        }
        *pos += n;
        l
    };
    Ok((tag, len))
}

/// SEC1 PEM (`EC PRIVATE KEY`) → `(privkey_32_be, pubkey_64_be)`. The
/// public key is derived from the private key when the file omits it.
pub fn parse_sec1_pem(pem_text: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    let der = pem_body_b64(pem_text)?;
    let mut pos = 0;

    let (tag, _) = der_tag_len(&der, &mut pos)?;
    if tag != 0x30 {
        return Err("Invalid DER: expected SEQUENCE".into());
    }
    let (tag, int_len) = der_tag_len(&der, &mut pos)?;
    if tag != 0x02 {
        return Err("Invalid DER: expected INTEGER (version)".into());
    }
    pos += int_len;
    let (tag, oct_len) = der_tag_len(&der, &mut pos)?;
    if tag != 0x04 {
        return Err("Invalid DER: expected OCTET STRING (private key)".into());
    }
    if oct_len != 32 || pos + 32 > der.len() {
        return Err(format!("Invalid private key length {} (expected 32)", oct_len));
    }
    let privkey = der[pos..pos + 32].to_vec();
    pos += 32;

    let mut pubkey: Option<Vec<u8>> = None;
    while pos < der.len() {
        let (tag, content_len) = der_tag_len(&der, &mut pos)?;
        if tag == 0xA1 && pos + content_len <= der.len() {
            let inner_start = pos;
            let (inner_tag, inner_len) = der_tag_len(&der, &mut pos)?;
            if inner_tag == 0x03 && inner_len >= 66 && pos + 66 <= der.len() && der[pos] == 0x00 && der[pos + 1] == 0x04 {
                pubkey = Some(der[pos + 2..pos + 66].to_vec());
            }
            pos = inner_start + content_len;
        } else {
            pos += content_len;
        }
    }

    let pubkey = match pubkey {
        Some(q) => q,
        None => derive_pubkey(&privkey)?,
    };
    Ok((privkey, pubkey))
}

fn derive_pubkey(privkey: &[u8]) -> Result<Vec<u8>, String> {
    let sk = SecretKey::from_slice(privkey).map_err(|e| format!("Invalid P-256 private key: {}", e))?;
    let point = sk.public_key().to_encoded_point(false);
    let bytes = point.as_bytes();
    if bytes.len() != 65 || bytes[0] != 0x04 {
        return Err("Failed to derive uncompressed public key".into());
    }
    Ok(bytes[1..65].to_vec())
}

/// `KEY:GENERATE` name payload: 16 bytes, NUL-padded, at most 15 bytes copied.
pub fn build_ssh_name_payload(name: &str) -> Vec<u8> {
    let mut buf = vec![0u8; 16];
    let nb = name.as_bytes();
    let n = nb.len().min(15);
    buf[..n].copy_from_slice(&nb[..n]);
    buf
}

/// `KEY:IMPORT` payload: name[16] + pubkey_LE[64] + privkey_LE[32] = 112
/// bytes (the device stores all numbers little-endian). Verifies the
/// private key derives the embedded public key before packing.
pub fn build_ssh_import_payload(name: &str, pem_text: &str) -> Result<Vec<u8>, String> {
    let (privkey, pubkey) = if pem_text.contains("-----BEGIN OPENSSH PRIVATE KEY-----") {
        parse_openssh_key(pem_text)?
    } else if pem_text.contains("-----BEGIN EC PRIVATE KEY-----") {
        parse_sec1_pem(pem_text)?
    } else {
        return Err("Unsupported key format. Expected OpenSSH or SEC1 PEM (ECDSA P-256).".into());
    };

    if derive_pubkey(&privkey)? != pubkey {
        return Err("Public/private key pair mismatch — the keyfile's private key does not derive \
                    its embedded public key. Re-export with `ssh-keygen -y -f <privkey>` to check."
            .into());
    }

    let mut out = build_ssh_name_payload(name);
    let (x, y) = pubkey.split_at(32);
    out.extend(x.iter().rev());
    out.extend(y.iter().rev());
    out.extend(privkey.iter().rev());
    debug_assert_eq!(out.len(), 112);
    Ok(out)
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p immurok-client ssh_import`
Expected: `4 passed`

- [ ] **Step 5: 提交**

```bash
git add crates/immurok-client/Cargo.toml crates/immurok-client/src/ssh_import.rs crates/immurok-client/src/lib.rs Cargo.lock
git commit -m "client: SSH 私钥解析（OpenSSH/SEC1）下沉为 ssh_import，错误改为 Result"
```

---

### Task 3: `immurok-client::keys` 发送函数 + CLI/TUI 切换

**Files:**
- Modify: `crates/immurok-client/src/keys.rs`
- Modify: `crates/immurok-cli/src/commands/keys.rs`（删除搬走的函数，改调用）
- Modify: `crates/immurok-cli/src/tui/app.rs:2178,2244-2248,2360-2370`

**Interfaces:**
- Consumes: Task 1/2 的 `build_key_add_cmd`、`build_otp_entry_payload`、`build_ssh_name_payload`、`build_ssh_import_payload`、`OtpEntry`、`KeyAddCat`
- Produces（`keys.rs`）:
  - `pub fn capacity(cat: KeyCategory) -> u8`
  - `pub fn generate_ssh(name: &str) -> Result<(), String>`
  - `pub fn import_ssh(name: &str, pem_text: &str) -> Result<(), String>`
  - `pub fn add_otp(entry: &OtpEntry) -> Result<(), String>`
  - `pub fn add_api(name: &str, value: &str) -> Result<(), String>`

- [ ] **Step 1: 写失败测试**

`crates/immurok-client/src/keys.rs` 的 `mod tests` 里追加：

```rust
    #[test]
    fn capacity_matches_protocol_limits() {
        use immurok_common::protocol::{KEY_MAX_API, KEY_MAX_OTP, KEY_MAX_SSH};
        assert_eq!(capacity(KeyCategory::Ssh), KEY_MAX_SSH);
        assert_eq!(capacity(KeyCategory::Otp), KEY_MAX_OTP);
        assert_eq!(capacity(KeyCategory::Api), KEY_MAX_API);
    }

    #[test]
    fn write_reply_ok_prefix_only() {
        assert_eq!(parse_write_reply("OK:GENERATED"), Ok(()));
        assert_eq!(parse_write_reply("OK"), Ok(()));
        assert_eq!(parse_write_reply("ERROR:KEYSTORE_FULL"), Err("ERROR:KEYSTORE_FULL".to_string()));
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p immurok-client keys::tests`
Expected: 编译错误 `cannot find function capacity` / `parse_write_reply`。

- [ ] **Step 3: 实现 `keys.rs` 新函数**

在 `delete_key` 之前加：

```rust
use crate::keys_import::{build_key_add_cmd, build_otp_entry_payload, KeyAddCat, OtpEntry};
use crate::ssh_import::{build_ssh_import_payload, build_ssh_name_payload};
use immurok_common::protocol::{KEY_MAX_API, KEY_MAX_OTP, KEY_MAX_SSH, SECRET_LEN_OTP, VALUE_LEN_API};

/// Firmware slot limit per category.
pub fn capacity(cat: KeyCategory) -> u8 {
    match cat {
        KeyCategory::Ssh => KEY_MAX_SSH,
        KeyCategory::Otp => KEY_MAX_OTP,
        KeyCategory::Api => KEY_MAX_API,
    }
}

fn parse_write_reply(rsp: &str) -> Result<(), String> {
    let rsp = rsp.trim();
    if rsp.starts_with("OK") {
        Ok(())
    } else {
        Err(rsp.to_string())
    }
}

fn send_write(cmd: &str) -> Result<(), String> {
    let rsp = DaemonClient::connect()?.send_with_timeout(cmd, GATE_TIMEOUT)?;
    parse_write_reply(&rsp)
}

/// Generate an ECDSA P-256 keypair on the device (`KEY:GENERATE`). The
/// name is cut to 15 bytes. Blocks on the fingerprint gate.
pub fn generate_ssh(name: &str) -> Result<(), String> {
    send_write(&format!("KEY:GENERATE:{}", hex::encode(build_ssh_name_payload(name))))
}

/// Import an existing P-256 private key (OpenSSH or SEC1 PEM text).
pub fn import_ssh(name: &str, pem_text: &str) -> Result<(), String> {
    let payload = build_ssh_import_payload(name, pem_text)?;
    send_write(&format!("KEY:IMPORT:{}", hex::encode(payload)))
}

/// Add one OTP entry. `entry.secret` is the decoded base32 secret.
pub fn add_otp(entry: &OtpEntry) -> Result<(), String> {
    if entry.secret.is_empty() {
        return Err("Secret cannot be empty.".into());
    }
    if entry.secret.len() > SECRET_LEN_OTP {
        return Err(format!("Secret too long: {} bytes decoded (device limit {}).", entry.secret.len(), SECRET_LEN_OTP));
    }
    send_write(&format!("KEY:OTP_IMPORT:{}", hex::encode(build_otp_entry_payload(entry))))
}

/// Add one API entry.
pub fn add_api(name: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("Value cannot be empty.".into());
    }
    if value.len() > VALUE_LEN_API {
        return Err(format!("Value too long: {} bytes (device limit {}).", value.len(), VALUE_LEN_API));
    }
    send_write(&build_key_add_cmd(KeyAddCat::Api, name, "", value.as_bytes()))
}
```

`immurok-client/Cargo.toml` 已有 `hex`；无需改。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p immurok-client`
Expected: 全部 `ok`，含新 2 个。

- [ ] **Step 5: CLI 切换到共享实现**

`crates/immurok-cli/src/commands/keys.rs`：

1. 删除整段 `KeyAddCat` / `build_key_add_cmd`（第 159-192 行）、`parse_openssh_key`、`parse_sec1_pem`、`base32_decode`、`split_otp_fields`、`truncate_utf8`、`OtpEntry`、`parse_andotp_json`、`parse_csv_otpauth`、`parse_otpauth_uri`、`url_decode`、`build_otp_entry_payload`（第 420-940 行），以及顶部 `use p256::…` 两行。
2. 顶部加：

```rust
use immurok_client::keys::{add_otp, generate_ssh, import_ssh};
use immurok_client::keys_import::{base32_decode, build_key_add_cmd, parse_otp_import_file, KeyAddCat, OtpEntry};
```

3. `run_add`：`build_key_add_cmd(...)` 调用不变（现在来自 import），其余逻辑不动。
4. `run_generate_ssh` 改为：

```rust
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
```

5. `run_import_ssh` 改为：

```rust
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
```

6. `run_import_otp`：文件读取后把「按扩展名解析 + 空结果报错」两段换成：

```rust
    let (entries, skipped) = match parse_otp_import_file(file, &content) {
        Ok(v) => v,
        Err(e) => super::error_exit(&e),
    };
```

  容量检查、确认提示保持不变；逐条发送循环里把 `DaemonClient::connect()` + `build_otp_entry_payload` + `client.send(&cmd)` 换成：

```rust
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
```

  `OtpEntry` 的 `use` 保留给类型注解（若编译器报 unused 则删）。

`crates/immurok-cli/src/tui/app.rs`：

- 第 2178 行 `crate::commands::keys::base32_decode` → `immurok_client::keys_import::base32_decode`
- 第 2244-2248 行 `crate::commands::keys::KeyAddCat` / `build_key_add_cmd` → `immurok_client::keys_import::{KeyAddCat, build_key_add_cmd}`（改成完整路径即可）
- `action_key_generate`：把「组 16 字节 name + `KEY:GENERATE` + `DaemonClient::connect()` + `send`」换成线程里调用 `immurok_client::keys::generate_ssh(&name)`：

```rust
        let tx = self.action_tx.clone();
        thread::spawn(move || {
            match immurok_client::keys::generate_ssh(&name) {
                Ok(()) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("SSH keypair '{}' generated.", name),
                        MessageStyle::Green,
                    ));
                }
                Err(e) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Generate failed: {}", e),
                        MessageStyle::Red,
                    ));
                }
            }
            let _ = tx.send(ActionResult::Refresh);
            let _ = tx.send(ActionResult::Done);
        });
```

- [ ] **Step 6: 编译 + 全量测试**

Run: `cargo build --workspace 2>&1 | grep -E "^(warning|error)" ; cargo test --workspace 2>&1 | grep "test result"`
Expected: 无 warning/error；所有 `test result: ok`。CLI 若报 `unused import` 就删掉对应 `use`。

- [ ] **Step 7: 提交**

```bash
git add crates/immurok-client/src/keys.rs crates/immurok-cli/src/commands/keys.rs crates/immurok-cli/src/tui/app.rs
git commit -m "client,cli: 添加/导入 key 的发送函数进 immurok-client，CLI/TUI 改为调用共享实现"
```

---

### Task 4: GUI — Features 独立页

**Files:**
- Create: `crates/immurok-gui/src/pages/features.rs`
- Modify: `crates/immurok-gui/src/pages/mod.rs`（加 `pub mod features;`）
- Modify: `crates/immurok-gui/src/pages/dashboard.rs`（移走 Features 组和 `wire_switches`，加 `set_features`）
- Modify: `crates/immurok-gui/src/main_window.rs`（新增页面、顺序）

**Interfaces:**
- Consumes: `immurok_client::status::{set_setting, SettingKey, Settings}`、`pages::run_blocking`
- Produces:
  - `pub struct FeaturesPage`；`pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self>`；`pub fn widget(&self) -> &gtk::Widget`；`pub fn apply(&self, s: &Settings)`；`pub fn set_daemon_available(&self, ok: bool)`
  - `DashboardPage::set_features(&self, page: &Rc<FeaturesPage>)`

- [ ] **Step 1: 新建 `pages/features.rs`**

```rust
//! Features page: the five daemon feature toggles. Toggles write through
//! immediately and only flip once the daemon has confirmed; state is fed
//! by the Dashboard's 2 s poll via [`FeaturesPage::apply`] — no second
//! poll loop.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::status::{set_setting, SettingKey, Settings};

use super::run_blocking;

pub struct FeaturesPage {
    root: gtk::Widget,
    switches: Vec<(SettingKey, gtk::Switch)>,
    toasts: adw::ToastOverlay,
}

impl FeaturesPage {
    pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self> {
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .title("Features")
            .description("Each toggle takes effect immediately.")
            .build();
        let mut switches = Vec::new();
        for (key, title, subtitle) in [
            (SettingKey::UnlockSudo, "sudo with fingerprint", "Touch the device instead of typing your password for sudo"),
            (SettingKey::UnlockPolkit, "System authorization (polkit)", "Touch the device for graphical authorization prompts"),
            (SettingKey::UnlockScreen, "Unlock screen", "Touch the device to unlock the lock screen"),
            (SettingKey::LockScreen, "Long-press to lock", "Long-press the sensor to lock the screen"),
            (SettingKey::SshTakeover, "SSH agent takeover", "Let ssh use the keys on the device"),
        ] {
            let sw = gtk::Switch::builder().valign(gtk::Align::Center).build();
            let row = adw::ActionRow::builder().title(title).subtitle(subtitle).build();
            row.add_suffix(&sw);
            row.set_activatable_widget(Some(&sw));
            group.add(&row);
            switches.push((key, sw));
        }
        page.add(&group);

        let this = Rc::new(Self { root: page.upcast(), switches, toasts: toasts.clone() });
        this.wire_switches();
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        &self.root
    }

    fn wire_switches(&self) {
        for (key, sw) in &self.switches {
            let key = *key;
            let toasts = self.toasts.clone();
            // Set while a `set_setting` write is in flight for this switch.
            let in_flight = Rc::new(Cell::new(false));
            // Return Stop: we own the `state` property and flip it only after
            // the daemon confirms, so a failed write leaves the switch where
            // it was instead of lying.
            sw.connect_state_set(move |sw, wanted| {
                // `gtk_switch_set_active` re-emits `state-set`, so our own
                // revert below and `apply()`'s external sync come back
                // through here. Without this guard a failing write reverts,
                // re-enters with the opposite `wanted`, writes again … a hot
                // loop of failed SETs and toasts.
                if wanted == sw.state() {
                    return glib::Propagation::Stop;
                }
                if in_flight.get() {
                    sw.set_active(sw.state());
                    return glib::Propagation::Stop;
                }
                in_flight.set(true);
                let sw = sw.clone();
                let toasts = toasts.clone();
                let in_flight = in_flight.clone();
                glib::spawn_future_local(async move {
                    match run_blocking(move || set_setting(key, wanted)).await {
                        Some(Ok(())) => sw.set_state(wanted),
                        Some(Err(e)) => {
                            toasts.add_toast(adw::Toast::new(&format!(
                                "Failed to save setting: {}",
                                glib::markup_escape_text(&e)
                            )));
                            sw.set_active(!wanted);
                        }
                        None => sw.set_active(!wanted),
                    }
                    in_flight.set(false);
                });
                glib::Propagation::Stop
            });
        }
    }

    /// Sync switches to what the daemon reports (called from the Dashboard poll).
    pub fn apply(&self, s: &Settings) {
        self.root.set_sensitive(true);
        for (key, sw) in &self.switches {
            let on = match key {
                SettingKey::UnlockSudo => s.unlock_sudo,
                SettingKey::UnlockPolkit => s.unlock_polkit,
                SettingKey::UnlockScreen => s.unlock_screen,
                SettingKey::LockScreen => s.lock_screen,
                SettingKey::SshTakeover => s.ssh_takeover,
            };
            // Setting `active` would re-enter state-set; we own `state`.
            if sw.state() != on {
                sw.set_state(on);
                sw.set_active(on);
            }
        }
    }

    /// Grey the page out while the daemon is unreachable.
    pub fn set_daemon_available(&self, ok: bool) {
        self.root.set_sensitive(ok);
    }
}
```

`pages/mod.rs` 的 `pub mod` 列表加 `pub mod features;`（按字母序放在 `dashboard` 之后）。

- [ ] **Step 2: 从 `dashboard.rs` 移走 Features**

1. 删除 `// ── Features ──` 到 `page.add(&features);` 整段（`new()` 内）；删除 `switches` 字段、`switches,` 初始化、`this.wire_switches();`、整个 `fn wire_switches`、`apply()` 里 `if let Some(s) = settings { … }` 块。
2. `use immurok_client::status::{…}` 去掉 `set_setting, SettingKey`（保留 `Settings` 供 `apply` 签名）；`use std::cell::Cell;` 仍被 `polling`/`paired` 用到，保留。
3. 结构体加字段 `features: RefCell<Option<Weak<super::features::FeaturesPage>>>`（`use std::cell::RefCell; use std::rc::Weak;`），初始化 `features: RefCell::new(None)`。
4. 加方法：

```rust
    /// The Features page shares this page's 2 s poll instead of running its own.
    pub fn set_features(&self, page: &Rc<super::features::FeaturesPage>) {
        *self.features.borrow_mut() = Some(Rc::downgrade(page));
    }

    fn features(&self) -> Option<Rc<super::features::FeaturesPage>> {
        self.features.borrow().as_ref().and_then(Weak::upgrade)
    }
```

5. `apply()` 末尾（原 `if let Some(s) = settings` 处）改为：

```rust
        if let Some(f) = self.features() {
            match settings {
                Some(s) => f.apply(s),
                None => f.set_daemon_available(false),
            }
        }
```

6. `apply_daemon_down()` 末尾加：

```rust
        if let Some(f) = self.features() {
            f.set_daemon_available(false);
        }
```

7. 文件头注释 `//! Dashboard: device status, feature toggles, pair / unpair.` 改为 `//! Dashboard: device status, two hosts, firmware hint. Feature toggles live in `features.rs` and are fed from this page's poll.`

- [ ] **Step 3: `main_window.rs` 挂页面**

在 `dashboard` 之后、`keys` 之前加：

```rust
        let features = pages::features::FeaturesPage::new(&toasts);
        stack
            .add_titled(features.widget(), Some("features"), "Features")
            .set_icon_name(Some("emblem-system-symbolic"));
        dashboard.set_features(&features);
```

`set_data` 段加 `unsafe { window.set_data("features-page", features) };`。

- [ ] **Step 4: 编译、测试、目视验收**

Run: `cargo build -p immurok-gui 2>&1 | grep -E "^(warning|error)"; cargo test -p immurok-gui 2>&1 | grep "test result"`
Expected: 无 warning；`ok`。

目视（已安装实例占着单实例，用 Broadway）：

```bash
(gtk4-broadwayd :5 &) ; sleep 1
(dbus-run-session -- env GDK_BACKEND=broadway BROADWAY_DISPLAY=:5 ./target/debug/immurok-gui &) ; sleep 3
chromium --headless=new --no-sandbox --disable-gpu --window-size=900,700 --virtual-time-budget=6000 \
  --screenshot=/tmp/claude-1000/gui-features.png http://127.0.0.1:8085
```

打开截图：侧边栏第二项是 Features；Device 页无开关；再用 CDP 点 Features 行截图确认 5 个开关且状态与 `immurok-cli settings` 输出一致。验收后 `pkill -f "debug/immurok-gui"; pkill -f "gtk4-broadwayd :5"`。

- [ ] **Step 5: 提交**

```bash
git add crates/immurok-gui/src/pages/features.rs crates/immurok-gui/src/pages/mod.rs crates/immurok-gui/src/pages/dashboard.rs crates/immurok-gui/src/main_window.rs
git commit -m "gui: 功能开关从 Device 页拆成独立 Features 页，共用 Dashboard 轮询"
```

---

### Task 5: GUI — 自带指纹 symbolic 图标（gresource）

**Files:**
- Create: `crates/immurok-gui/build.rs`
- Create: `crates/immurok-gui/data/immurok.gresource.xml`
- Create: `crates/immurok-gui/data/icons/scalable/actions/immurok-fingerprint-symbolic.svg`
- Modify: `crates/immurok-gui/Cargo.toml`（`[build-dependencies] glib-build-tools = "0.20"`）
- Modify: `crates/immurok-gui/src/main.rs`（注册资源 + 图标路径）
- Modify: `crates/immurok-gui/src/pages/fingerprints.rs:47-61`

**Interfaces:**
- Produces: 图标名 `immurok-fingerprint-symbolic`；`pages::fingerprints::finger_icon_name()` 返回它（签名不变）。

- [ ] **Step 1: 资源文件**

`crates/immurok-gui/data/immurok.gresource.xml`：

```xml
<?xml version="1.0" encoding="UTF-8"?>
<gresources>
  <gresource prefix="/com/immurok/Settings/icons">
    <file>scalable/actions/immurok-fingerprint-symbolic.svg</file>
  </gresource>
</gresources>
```

`crates/immurok-gui/data/icons/scalable/actions/immurok-fingerprint-symbolic.svg`（纯填充路径——GTK 对 symbolic 只重着色 `fill`，描边不会跟随前景色）：

```xml
<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><g fill="#2e3436"><path d="M1.05 5.97A7.4 7.4 0 0 1 14.95 5.97L13.83 6.38A6.2 6.2 0 0 0 2.17 6.38Z"/><path d="M2.88 7.60A5.2 5.2 0 0 1 13.12 7.60L11.94 7.81A4.0 4.0 0 0 0 4.06 7.81Z"/><path d="M12.89 10.28A5.2 5.2 0 0 1 5.40 13.00L6.00 11.96A4.0 4.0 0 0 0 11.76 9.87Z"/><path d="M5.40 10.00A3.0 3.0 0 1 1 9.50 11.10L8.90 10.06A1.8 1.8 0 1 0 6.44 9.40Z"/><path d="M13.67 13.26A7.4 7.4 0 0 1 5.47 15.45L5.88 14.33A6.2 6.2 0 0 0 12.75 12.49Z"/><path d="M3.24 14.17A7.4 7.4 0 0 1 0.71 9.78L1.89 9.58A6.2 6.2 0 0 0 4.01 13.25Z"/></g></svg>
```

- [ ] **Step 2: build.rs + Cargo**

`crates/immurok-gui/build.rs`：

```rust
fn main() {
    glib_build_tools::compile_resources(
        &["data"],
        "data/immurok.gresource.xml",
        "immurok.gresource",
    );
}
```

`Cargo.toml` 加：

```toml
[build-dependencies]
glib-build-tools = "0.20"
```

- [ ] **Step 3: 注册资源**

`main.rs` 的 `connect_startup` 闭包开头（`register_actions(app);` 之前）加：

```rust
        gio::resources_register_include!("immurok.gresource").expect("gresource compiled by build.rs");
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::IconTheme::for_display(&display).add_resource_path("/com/immurok/Settings/icons");
        }
```

`pages/fingerprints.rs` 把 `finger_icon_name` 整个函数换成：

```rust
/// Our own symbolic fingerprint glyph, bundled as a gresource
/// (`data/icons/…`) so it renders the same under every icon theme —
/// Fluent, for one, claims `fingerprint-symbolic` but draws nothing.
pub fn finger_icon_name() -> &'static str {
    "immurok-fingerprint-symbolic"
}
```

- [ ] **Step 4: 编译 + 验收**

Run: `cargo build -p immurok-gui 2>&1 | grep -E "^(warning|error)"`
Expected: 无输出（首次会下载 glib-build-tools；需要 `glib-compile-resources` 在 PATH，`pacman -Q glib2` / `apt list libglib2.0-dev-bin`）。

目视：同 Task 4 的 Broadway 流程截图，侧边栏 Fingerprints 行有指纹图标；再让用户在真实 GNOME + Fluent 主题下 `cargo run -p immurok-gui`（先 `pkill -f immurok-gui`）确认。深色/浅色主题各看一次（`GTK_THEME=Adwaita:dark` / 不带）确认重着色生效。

- [ ] **Step 5: 提交**

```bash
git add crates/immurok-gui/build.rs crates/immurok-gui/data crates/immurok-gui/Cargo.toml crates/immurok-gui/src/main.rs crates/immurok-gui/src/pages/fingerprints.rs Cargo.lock
git commit -m "gui: 指纹图标改用自带 gresource symbolic SVG，不再依赖图标主题"
```

---

### Task 6: GUI Keys — 容量标题、`+` 按钮、空状态、连接状态

**Files:**
- Modify: `crates/immurok-gui/Cargo.toml`（`libadwaita = { version = "0.7", features = ["v1_1"] }`）
- Modify: `crates/immurok-gui/src/pages/keys.rs`
- Modify: `crates/immurok-gui/src/pages/dashboard.rs`（轮询回调里转发 `connected`）
- Modify: `crates/immurok-gui/src/main_window.rs`（`dashboard.set_keys(&keys)`）

**Interfaces:**
- Consumes: `immurok_client::keys::capacity`
- Produces:
  - `KeysPage::set_connected(&self, connected: bool)`
  - `DashboardPage::set_keys(&self, page: &Rc<KeysPage>)`
  - `KeysPage` 内部：`add_buttons: Vec<(KeyCategory, gtk::Button)>`、`counts: RefCell<HashMap<KeyCategory, usize>>`、`connected: Cell<bool>`、`fn refresh_add_buttons(&self)`、`fn open_add(self: &Rc<Self>, cat: KeyCategory)`（本 Task 先只 toast "Not implemented"，Task 7 接对话框）

- [ ] **Step 1: 开 `v1_1`**

`crates/immurok-gui/Cargo.toml`：`libadwaita = { version = "0.7", features = ["v1_1"] }`。`cargo build -p immurok-gui` 应仍通过。

- [ ] **Step 2: 改 `keys.rs`**

结构体：

```rust
pub struct KeysPage {
    root: gtk::Widget,
    groups: Vec<(KeyCategory, adw::PreferencesGroup)>,
    add_buttons: Vec<(KeyCategory, gtk::Button)>,
    rows: RefCell<Vec<(adw::PreferencesGroup, adw::ActionRow)>>,
    counts: RefCell<HashMap<KeyCategory, usize>>,
    connected: Cell<bool>,
    toasts: adw::ToastOverlay,
}
```

（`use std::cell::{Cell, RefCell}; use std::collections::HashMap;`，`use immurok_client::keys::capacity;`）

`new()` 的分组循环改为：

```rust
        let mut groups = Vec::new();
        let mut add_buttons = Vec::new();
        for (cat, desc) in [
            (KeyCategory::Otp, "Generates a 6-digit code after you touch the device"),
            (KeyCategory::Api, "Shows the stored value after you touch the device"),
            (KeyCategory::Ssh, "Public key can be copied; signing goes through the SSH agent"),
        ] {
            let g = adw::PreferencesGroup::builder()
                .title(format!("{} (0/{})", cat.label(), capacity(cat)))
                .description(desc)
                .build();
            let add = gtk::Button::builder()
                .icon_name("list-add-symbolic")
                .valign(gtk::Align::Center)
                .tooltip_text(format!("Add {} entry", cat.label()))
                .build();
            add.add_css_class("flat");
            g.set_header_suffix(Some(&add));
            page.add(&g);
            groups.push((cat, g));
            add_buttons.push((cat, add));
        }
        let this = Rc::new(Self {
            root: page.upcast(),
            groups,
            add_buttons,
            rows: RefCell::new(Vec::new()),
            counts: RefCell::new(HashMap::new()),
            connected: Cell::new(false),
            toasts: toasts.clone(),
        });
        for (cat, btn) in &this.add_buttons {
            let cat = *cat;
            let weak = Rc::downgrade(&this);
            btn.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.open_add(cat);
                }
            });
        }
        this.refresh_add_buttons();
        this.reload();
        this
```

`populate()`：清空旧行后先统计，再按分类填充；没有条目的分类插一行占位（占位行也进 `rows` 以便下次清空）：

```rust
    fn populate(self: &Rc<Self>, entries: Vec<KeyEntry>) {
        for (group, row) in self.rows.borrow_mut().drain(..) {
            group.remove(&row);
        }
        let mut counts: HashMap<KeyCategory, usize> = HashMap::new();
        for e in &entries {
            *counts.entry(e.category).or_default() += 1;
        }
        for (cat, group) in &self.groups {
            let n = counts.get(cat).copied().unwrap_or(0);
            group.set_title(&format!("{} ({}/{})", cat.label(), n, capacity(*cat)));
            if n == 0 {
                let row = adw::ActionRow::builder()
                    .title(format!("No {} entries yet — press + to add one", cat.label()))
                    .build();
                row.add_css_class("dim-label");
                group.add(&row);
                self.rows.borrow_mut().push((group.clone(), row));
            }
        }
        *self.counts.borrow_mut() = counts;
        self.refresh_add_buttons();

        for entry in entries {
            // …现有的逐条建行代码原样保留…
        }
    }
```

新方法：

```rust
    /// Fed from the Dashboard poll; the add buttons need a live device.
    pub fn set_connected(&self, connected: bool) {
        if self.connected.replace(connected) != connected {
            self.refresh_add_buttons();
        }
    }

    fn refresh_add_buttons(&self) {
        let counts = self.counts.borrow();
        for (cat, btn) in &self.add_buttons {
            let n = counts.get(cat).copied().unwrap_or(0);
            let full = n >= capacity(*cat) as usize;
            let (sensitive, tip) = if !self.connected.get() {
                (false, "Device not connected".to_string())
            } else if full {
                (false, "Keystore full — delete an entry first".to_string())
            } else {
                (true, format!("Add {} entry", cat.label()))
            };
            btn.set_sensitive(sensitive);
            btn.set_tooltip_text(Some(&tip));
        }
    }

    fn open_add(self: &Rc<Self>, cat: KeyCategory) {
        // Replaced by the add dialog in the next task.
        self.toast(&format!("Add {}: not implemented yet", cat.label()), 3);
    }
```

- [ ] **Step 3: Dashboard → Keys 转发连接状态**

`dashboard.rs`：仿 `features` 加字段 `keys: RefCell<Option<Weak<super::keys::KeysPage>>>`、`pub fn set_keys(&self, page: &Rc<super::keys::KeysPage>)`、私有 `fn keys(&self) -> Option<Rc<…>>`；`apply()` 末尾加 `if let Some(k) = self.keys() { k.set_connected(status.connected); }`；`apply_daemon_down()` 末尾加 `if let Some(k) = self.keys() { k.set_connected(false); }`。

`main_window.rs` 在 `let keys = …` 之后加 `dashboard.set_keys(&keys);`。

- [ ] **Step 4: 编译 + 验收**

Run: `cargo build -p immurok-gui 2>&1 | grep -E "^(warning|error)"`
Expected: 无。

Broadway 截图 Keys 页：三组标题带 `(n/max)`；每组右上角 `+`；空分类显示占位行；设备已连接时 `+` 可点（点后 toast "not implemented"）。

- [ ] **Step 5: 提交**

```bash
git add crates/immurok-gui/Cargo.toml crates/immurok-gui/src/pages/keys.rs crates/immurok-gui/src/pages/dashboard.rs crates/immurok-gui/src/main_window.rs Cargo.lock
git commit -m "gui(keys): 分组显示容量、加 + 按钮与空状态，连接状态由 Dashboard 轮询同步"
```

---

### Task 7: GUI Keys — 添加对话框（SSH 生成/导入、OTP、API）

**Files:**
- Create: `crates/immurok-gui/src/key_add_dialog.rs`
- Modify: `crates/immurok-gui/src/main.rs`（`mod key_add_dialog;`）
- Modify: `crates/immurok-gui/src/pages/keys.rs`（`open_add` 接对话框）

**Interfaces:**
- Consumes: `immurok_client::keys::{add_api, add_otp, generate_ssh, import_ssh}`、`immurok_client::keys_import::{base32_decode, parse_otpauth_uri, OtpEntry}`、`pages::run_blocking`
- Produces: `pub async fn run(parent: &impl IsA<gtk::Window>, cat: KeyCategory) -> bool`（`true` = 有条目写入，调用方需 `reload()`）

- [ ] **Step 1: 写对话框**

`crates/immurok-gui/src/key_add_dialog.rs`：

```rust
//! "Add entry" dialog for the Keys page. One window per category:
//!   SSH — name; "Generate on device" or "Import from file…" (P-256 PEM)
//!   OTP — name / service / base32 secret; pasting an otpauth:// URI into
//!         the secret field fills all three
//!   API — name / value
//! Every write goes through the device's fingerprint gate: the primary
//! button turns into "Touch the device…" until the daemon answers. Errors
//! stay in the dialog (red label, daemon text verbatim) so the user can fix
//! the input and retry.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::keys::{add_api, add_otp, generate_ssh, import_ssh, KeyCategory};
use immurok_client::keys_import::{base32_decode, parse_otpauth_uri, OtpEntry};
use immurok_common::protocol::{NAME_LEN_API, NAME_LEN_OTP, NAME_LEN_SSH, SERVICE_LEN_OTP};

use crate::pages::run_blocking;

fn entry_row(group: &adw::PreferencesGroup, title: &str, max_bytes: Option<usize>, password: bool) -> gtk::Editable {
    let row = adw::ActionRow::builder().title(title).build();
    let editable: gtk::Editable = if password {
        gtk::PasswordEntry::builder().show_peek_icon(true).activates_default(true).valign(gtk::Align::Center).hexpand(true).build().upcast()
    } else {
        gtk::Entry::builder().activates_default(true).valign(gtk::Align::Center).hexpand(true).build().upcast()
    };
    if let Some(max) = max_bytes {
        // Firmware fields are byte-sized; GTK's max-length counts chars, so
        // clamp on change instead.
        let e = editable.clone();
        editable.connect_changed(move |_| {
            let text = e.text();
            if text.len() > max {
                let mut cut = max;
                while cut > 0 && !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                e.set_text(&text[..cut]);
                e.set_position(-1);
            }
        });
    }
    row.add_suffix(&editable);
    row.set_activatable_widget(Some(&editable));
    group.add(&row);
    editable
}

struct Shell {
    window: gtk::Window,
    group: adw::PreferencesGroup,
    error: gtk::Label,
    primary: gtk::Button,
    secondary: gtk::Button,
    cancel: gtk::Button,
}

fn shell(parent: &impl IsA<gtk::Window>, title: &str, primary: &str, secondary: Option<&str>) -> Shell {
    let window = gtk::Window::builder()
        .transient_for(parent)
        .destroy_with_parent(true)
        .modal(true)
        .resizable(false)
        .title(title)
        .default_width(460)
        .build();
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    page.add(&group);

    let error = gtk::Label::builder().wrap(true).visible(false).xalign(0.0).margin_start(24).margin_end(24).build();
    error.add_css_class("error");

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.set_margin_end(24);
    buttons.set_margin_bottom(18);
    buttons.set_margin_top(6);
    let cancel = gtk::Button::builder().label("Cancel").build();
    let secondary_btn = gtk::Button::builder().label(secondary.unwrap_or("")).visible(secondary.is_some()).build();
    let primary_btn = gtk::Button::builder().label(primary).build();
    primary_btn.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&secondary_btn);
    buttons.append(&primary_btn);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&page);
    root.append(&error);
    root.append(&buttons);
    window.set_child(Some(&root));
    window.set_default_widget(Some(&primary_btn));

    {
        let w = window.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.close();
            }
        });
    }
    Shell { window, group, error, primary: primary_btn, secondary: secondary_btn, cancel }
}

impl Shell {
    fn show_error(&self, text: &str) {
        self.error.set_text(text);
        self.error.set_visible(true);
    }
    fn busy(&self, on: bool, label_idle: &str) {
        self.primary.set_sensitive(!on);
        self.secondary.set_sensitive(!on);
        self.cancel.set_sensitive(!on);
        self.primary.set_label(if on { "Touch the device…" } else { label_idle });
        if on {
            self.error.set_visible(false);
        }
    }
}

fn name_of(e: &gtk::Editable) -> String {
    e.text().trim().to_string()
}

/// Run the dialog; resolves when it closes. `true` if something was written.
pub async fn run(parent: &impl IsA<gtk::Window>, cat: KeyCategory) -> bool {
    let written = Rc::new(Cell::new(false));
    let (done_tx, done_rx) = async_channel::bounded::<()>(1);

    let sh = match cat {
        KeyCategory::Ssh => shell(parent, "Add SSH key", "Generate on device", Some("Import from file…")),
        KeyCategory::Otp => shell(parent, "Add OTP entry", "Add", Some("Import from file…")),
        KeyCategory::Api => shell(parent, "Add API key", "Add", None),
    };
    let sh = Rc::new(sh);
    {
        let tx = done_tx.clone();
        sh.window.connect_close_request(move |_| {
            let _ = tx.try_send(());
            glib::Propagation::Proceed
        });
    }

    let name = entry_row(
        &sh.group,
        "Name",
        Some(match cat {
            KeyCategory::Ssh => NAME_LEN_SSH - 1,
            KeyCategory::Otp => NAME_LEN_OTP - 1,
            KeyCategory::Api => NAME_LEN_API - 1,
        }),
        false,
    );

    match cat {
        KeyCategory::Ssh => {
            sh.group.set_description(Some("ECDSA P-256. The private key never leaves the device."));
            // Generate
            {
                let (sh2, name, written, tx) = (sh.clone(), name.clone(), written.clone(), done_tx.clone());
                sh.primary.connect_clicked(move |_| {
                    let n = name_of(&name);
                    if n.is_empty() {
                        sh2.show_error("Name cannot be empty.");
                        return;
                    }
                    sh2.busy(true, "Generate on device");
                    let (sh3, written, tx) = (sh2.clone(), written.clone(), tx.clone());
                    glib::spawn_future_local(async move {
                        match run_blocking(move || generate_ssh(&n)).await {
                            Some(Ok(())) => {
                                written.set(true);
                                let _ = tx.try_send(());
                                sh3.window.close();
                            }
                            Some(Err(e)) => {
                                sh3.busy(false, "Generate on device");
                                sh3.show_error(&format!("Generate failed: {}", e));
                            }
                            None => sh3.busy(false, "Generate on device"),
                        }
                    });
                });
            }
            // Import
            {
                let (sh2, name, written, tx) = (sh.clone(), name.clone(), written.clone(), done_tx.clone());
                sh.secondary.connect_clicked(move |_| {
                    let n = name_of(&name);
                    if n.is_empty() {
                        sh2.show_error("Name cannot be empty.");
                        return;
                    }
                    let (sh3, written, tx) = (sh2.clone(), written.clone(), tx.clone());
                    glib::spawn_future_local(async move {
                        let Some(pem) = pick_text_file(&sh3.window, "Choose a private key file").await else { return };
                        sh3.busy(true, "Generate on device");
                        match run_blocking(move || import_ssh(&n, &pem)).await {
                            Some(Ok(())) => {
                                written.set(true);
                                let _ = tx.try_send(());
                                sh3.window.close();
                            }
                            Some(Err(e)) => {
                                sh3.busy(false, "Generate on device");
                                sh3.show_error(&format!("Import failed: {}", e));
                            }
                            None => sh3.busy(false, "Generate on device"),
                        }
                    });
                });
            }
        }
        KeyCategory::Otp => {
            sh.group.set_description(Some("TOTP, SHA-1, 6 digits, 30 s. Paste an otpauth:// URI into Secret to fill everything."));
            let service = entry_row(&sh.group, "Service", Some(SERVICE_LEN_OTP - 1), false);
            let secret = entry_row(&sh.group, "Secret", None, true);
            // otpauth:// auto-fill
            {
                let (name, service, secret2, sh2) = (name.clone(), service.clone(), secret.clone(), sh.clone());
                secret.connect_changed(move |_| {
                    let t = secret2.text();
                    if !t.trim_start().starts_with("otpauth://") {
                        return;
                    }
                    match parse_otpauth_uri(t.trim()) {
                        Some(e) => {
                            name.set_text(&e.name);
                            service.set_text(&e.service);
                            // Re-encode is lossy for odd lengths; keep the user's
                            // base32 by extracting it from the URI instead.
                            let raw = t.split("secret=").nth(1).and_then(|s| s.split('&').next()).unwrap_or("").to_string();
                            secret2.set_text(&raw);
                            sh2.error.set_visible(false);
                        }
                        None => sh2.show_error("Unsupported otpauth URI: only TOTP / SHA1 / 6 digits / 30 s."),
                    }
                });
            }
            {
                let (sh2, written, tx) = (sh.clone(), written.clone(), done_tx.clone());
                sh.primary.connect_clicked(move |_| {
                    let n = name_of(&name);
                    if n.is_empty() {
                        sh2.show_error("Name cannot be empty.");
                        return;
                    }
                    let Some(sec) = base32_decode(&secret.text()).filter(|b| !b.is_empty()) else {
                        sh2.show_error("Invalid base32 secret — check for typos.");
                        return;
                    };
                    let entry = OtpEntry { name: n, service: name_of(&service), secret: sec };
                    sh2.busy(true, "Add");
                    let (sh3, written, tx) = (sh2.clone(), written.clone(), tx.clone());
                    glib::spawn_future_local(async move {
                        match run_blocking(move || add_otp(&entry)).await {
                            Some(Ok(())) => {
                                written.set(true);
                                let _ = tx.try_send(());
                                sh3.window.close();
                            }
                            Some(Err(e)) => {
                                sh3.busy(false, "Add");
                                sh3.show_error(&format!("Add failed: {}", e));
                            }
                            None => sh3.busy(false, "Add"),
                        }
                    });
                });
            }
            // "Import from file…" is wired in Task 8; hide until then.
            sh.secondary.set_visible(false);
        }
        KeyCategory::Api => {
            let value = entry_row(&sh.group, "Value", None, true);
            let (sh2, written, tx) = (sh.clone(), written.clone(), done_tx.clone());
            sh.primary.connect_clicked(move |_| {
                let n = name_of(&name);
                if n.is_empty() {
                    sh2.show_error("Name cannot be empty.");
                    return;
                }
                let v = value.text().to_string();
                sh2.busy(true, "Add");
                let (sh3, written, tx) = (sh2.clone(), written.clone(), tx.clone());
                glib::spawn_future_local(async move {
                    match run_blocking(move || add_api(&n, &v)).await {
                        Some(Ok(())) => {
                            written.set(true);
                            let _ = tx.try_send(());
                            sh3.window.close();
                        }
                        Some(Err(e)) => {
                            sh3.busy(false, "Add");
                            sh3.show_error(&format!("Add failed: {}", e));
                        }
                        None => sh3.busy(false, "Add"),
                    }
                });
            });
        }
    }

    sh.window.present();
    let _ = done_rx.recv().await;
    written.get()
}

/// Native file chooser → file contents as text. `None` on cancel / read error
/// (the error is shown as a toast-free label by the caller if needed).
pub async fn pick_text_file(parent: &gtk::Window, title: &str) -> Option<String> {
    let chooser = gtk::FileChooserNative::new(Some(title), Some(parent), gtk::FileChooserAction::Open, Some("Open"), Some("Cancel"));
    chooser.set_modal(true);
    if chooser.run_future().await != gtk::ResponseType::Accept {
        return None;
    }
    let path = chooser.file()?.path()?;
    std::fs::read_to_string(path).ok()
}
```

`main.rs` 加 `mod key_add_dialog;`（字母序，在 `mod gate_dialog;` 之后）。

- [ ] **Step 2: `keys.rs` 接上**

`open_add` 换成：

```rust
    fn open_add(self: &Rc<Self>, cat: KeyCategory) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            let Some(win) = this.root.root().and_then(|r| r.downcast::<gtk::Window>().ok()) else { return };
            if crate::key_add_dialog::run(&win, cat).await {
                this.toast(&format!("{} entry added", cat.label()), 3);
                // The daemon re-syncs its cache after a successful write;
                // give it a beat before re-reading.
                glib::timeout_future(Duration::from_millis(500)).await;
                this.reload();
            }
        });
    }
```

- [ ] **Step 3: 编译 + 验收**

Run: `cargo build -p immurok-gui 2>&1 | grep -E "^(warning|error)"`
Expected: 无。若 `gtk::Editable` 的 `upcast()` 因 `PasswordEntry`/`Entry` 未实现 `IsA<Editable>` 报错，改为返回 `gtk::Widget` 并在需要时 `downcast_ref::<gtk::Editable>()`——实际两者都实现了 `Editable`，正常不会。

真机验收（用户操作，需要设备在线）：
1. Keys 页 SSH `+` → 输入 `test-gen` → Generate → 触摸 → 窗口关闭、toast、SSH 组多一条。
2. OTP `+` → 在 Secret 粘贴 `otpauth://totp/GitHub:alice?secret=JBSWY3DPEHPK3PXP&issuer=GitHub` → Name/Service 自动填、Secret 变为 `JBSWY3DPEHPK3PXP` → Add → 触摸。
3. API `+` → 名字 + 值 → Add → 触摸。
4. 任一对话框里输入空名字 → 红字 "Name cannot be empty."，不发请求。
5. 用 `immurok-cli key delete` 清掉测试条目。

- [ ] **Step 4: 提交**

```bash
git add crates/immurok-gui/src/key_add_dialog.rs crates/immurok-gui/src/main.rs crates/immurok-gui/src/pages/keys.rs
git commit -m "gui(keys): 添加对话框——SSH 设备生成/文件导入、OTP（含 otpauth 自动填充）、API"
```

---

### Task 8: GUI Keys — OTP 文件批量导入

**Files:**
- Modify: `crates/immurok-gui/src/key_add_dialog.rs`（OTP 分支接 secondary 按钮）

**Interfaces:**
- Consumes: `immurok_client::keys_import::parse_otp_import_file`、`immurok_client::keys::{add_otp, capacity, list_keys}`、`pages::confirm`、本文件 `pick_text_file`
- Produces: 无新公共接口

- [ ] **Step 1: 实现**

把 Task 7 OTP 分支末尾的 `sh.secondary.set_visible(false);` 换成：

```rust
            {
                let (sh2, written, tx) = (sh.clone(), written.clone(), done_tx.clone());
                sh.secondary.connect_clicked(move |_| {
                    let (sh3, written, tx) = (sh2.clone(), written.clone(), tx.clone());
                    glib::spawn_future_local(async move {
                        let chooser = gtk::FileChooserNative::new(
                            Some("Choose an OTP export (andOTP .json or otpauth .csv)"),
                            Some(&sh3.window),
                            gtk::FileChooserAction::Open,
                            Some("Open"),
                            Some("Cancel"),
                        );
                        chooser.set_modal(true);
                        if chooser.run_future().await != gtk::ResponseType::Accept {
                            return;
                        }
                        let Some(path) = chooser.file().and_then(|f| f.path()) else { return };
                        let content = match std::fs::read_to_string(&path) {
                            Ok(c) => c,
                            Err(e) => {
                                sh3.show_error(&format!("Cannot read file: {}", e));
                                return;
                            }
                        };
                        let (entries, skipped) = match parse_otp_import_file(&path.to_string_lossy(), &content) {
                            Ok(v) => v,
                            Err(e) => {
                                sh3.show_error(&e);
                                return;
                            }
                        };
                        // Capacity: count what the daemon has cached right now.
                        let used = run_blocking(|| immurok_client::keys::list_keys().into_iter().filter(|e| e.category == KeyCategory::Otp).count())
                            .await
                            .unwrap_or(0);
                        let free = (immurok_client::keys::capacity(KeyCategory::Otp) as usize).saturating_sub(used);
                        if entries.len() > free {
                            sh3.show_error(&format!(
                                "Cannot import {} entries: only {} OTP slots remaining. Delete some entries first.",
                                entries.len(),
                                free
                            ));
                            return;
                        }
                        let skip_note = if skipped > 0 {
                            format!(" ({} skipped: only TOTP / SHA1 / 6-digit / 30 s supported)", skipped)
                        } else {
                            String::new()
                        };
                        let ok = crate::pages::confirm(
                            &sh3.window,
                            &format!("Import {} OTP entr{}?", entries.len(), if entries.len() == 1 { "y" } else { "ies" }),
                            &format!("Touch the device when asked.{}", skip_note),
                            "Import",
                        )
                        .await;
                        if !ok {
                            return;
                        }
                        sh3.busy(true, "Add");
                        let total = entries.len();
                        let mut imported = 0;
                        let mut failure: Option<String> = None;
                        for (i, entry) in entries.into_iter().enumerate() {
                            sh3.primary.set_label(&format!("Importing {}/{}: {}", i + 1, total, entry.name));
                            match run_blocking(move || add_otp(&entry)).await {
                                Some(Ok(())) => imported += 1,
                                Some(Err(e)) => {
                                    failure = Some(e);
                                    break;
                                }
                                None => break,
                            }
                        }
                        if imported > 0 {
                            written.set(true);
                        }
                        match failure {
                            None if imported == total => {
                                let _ = tx.try_send(());
                                sh3.window.close();
                            }
                            None => {
                                sh3.busy(false, "Add");
                                sh3.show_error(&format!("Imported {}/{} entries.", imported, total));
                            }
                            Some(e) => {
                                sh3.busy(false, "Add");
                                sh3.show_error(&format!("Imported {}/{} entries; stopped at: {}", imported, total, e));
                            }
                        }
                    });
                });
            }
```

顶部 `use` 加 `use immurok_client::keys_import::parse_otp_import_file;`（并入已有那行）。

- [ ] **Step 2: 编译 + 验收**

Run: `cargo build -p immurok-gui 2>&1 | grep -E "^(warning|error)"`
Expected: 无。

真机：准备 `/tmp/claude-1000/otp.csv`：

```
otpauth://totp/Test:one?secret=JBSWY3DPEHPK3PXP&issuer=Test
otpauth://totp/Test:two?secret=JBSWY3DPEHPK3PXP&issuer=Test
otpauth://hotp/Test:skip?secret=JBSWY3DPEHPK3PXP
```

OTP `+` → Import from file… → 选它 → 确认框显示 "Import 2 OTP entries?" 且 skip 说明为 0（HOTP 行在 CSV 分支静默丢弃，属既有行为）→ Import → 第一条触摸、第二条走 10 s cooldown 免触摸 → 窗口关闭、OTP 组 +2。之后 `immurok-cli key delete otp <idx>` 清理。

- [ ] **Step 3: 提交**

```bash
git add crates/immurok-gui/src/key_add_dialog.rs
git commit -m "gui(keys): OTP 从 andOTP JSON / otpauth CSV 批量导入，带容量检查与进度"
```

---

### Task 9: GUI Keys — OTP 取码行内显示 + Copy

**Files:**
- Modify: `crates/immurok-gui/src/pages/keys.rs`（`primary_action` 的 OTP 分支；行构建时保留原 subtitle）
- Modify: `crates/immurok-gui/src/main_window.rs`（页面切走时清码）

**Interfaces:**
- Produces: `KeysPage::clear_codes(&self)`（公开，供 `main_window` 在 `visible-child` 变化时调用）
- 内部：`shown: RefCell<Vec<(adw::ActionRow, String /*orig subtitle*/, gtk::Button /*copy*/)>>`、`code_gen: Cell<u32>`（每次显示 +1；到期回调只在代数仍匹配时清除，避免持有 `SourceId` 在源已触发后 `remove()` panic）

- [ ] **Step 1: 实现**

结构体加字段 `shown: RefCell<Vec<(adw::ActionRow, String, gtk::Button)>>` 和 `code_gen: Cell<u32>`，初始化 `shown: RefCell::new(Vec::new())`、`code_gen: Cell::new(0)`。`populate()` 开头 `self.clear_codes();`（旧行即将被移除）。

新方法：

```rust
    /// Hide every inline OTP code (page switch, reload, expiry).
    pub fn clear_codes(&self) {
        self.code_gen.set(self.code_gen.get().wrapping_add(1));
        for (row, orig, copy) in self.shown.borrow_mut().drain(..) {
            row.set_subtitle(&orig);
            row.remove(&copy);
        }
    }

    /// Show `code` on `row` for 30 s with a Copy button.
    fn show_code(self: &Rc<Self>, row: &adw::ActionRow, code: &str) {
        self.clear_codes();
        let gen = self.code_gen.get();
        let orig = row.subtitle().map(|s| s.to_string()).unwrap_or_default();
        let pretty = if code.len() == 6 { format!("{} {}", &code[..3], &code[3..]) } else { code.to_string() };
        row.set_subtitle(&format!(
            "<span font_family=\"monospace\" size=\"x-large\" weight=\"bold\">{}</span>",
            glib::markup_escape_text(&pretty)
        ));
        let copy = gtk::Button::builder().icon_name("edit-copy-symbolic").valign(gtk::Align::Center).tooltip_text("Copy code").build();
        copy.add_css_class("flat");
        {
            let code = code.to_string();
            let this = Rc::downgrade(self);
            copy.connect_clicked(move |_| {
                if let Some(display) = gtk::gdk::Display::default() {
                    display.clipboard().set_text(&code);
                }
                if let Some(this) = this.upgrade() {
                    this.toast("Code copied", 2);
                }
            });
        }
        row.add_suffix(&copy);
        let this = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_secs(30), move || {
            if let Some(this) = this.upgrade() {
                if this.code_gen.get() == gen {
                    this.clear_codes();
                }
            }
        });
        self.shown.borrow_mut().push((row.clone(), orig, copy));
    }
```

`populate()` 里建行后，`primary.connect_clicked` 闭包多捕获 `row`（`let row = row.clone();`），并把 `this.primary_action(b.clone(), e.clone())` 改为 `this.primary_action(b.clone(), row.clone(), e.clone())`。

`primary_action` 签名改为 `fn primary_action(self: &Rc<Self>, button: gtk::Button, row: adw::ActionRow, entry: KeyEntry)`；`KeyCategory::Otp | KeyCategory::Api` 分支里的成功处理改为：

```rust
                    match r {
                        Some(Ok(value)) if entry.category == KeyCategory::Otp => this.show_code(&row, &value),
                        Some(Ok(value)) => this.toast(
                            &format!("{}: {}", glib::markup_escape_text(&entry.name), glib::markup_escape_text(&value)),
                            30,
                        ),
                        Some(Err(e)) => this.toast(&format!("Read failed: {}", glib::markup_escape_text(&e)), 5),
                        None => {}
                    }
```

`main_window.rs` 的 `build_sidebar` 之后（`stack` 已建）加：

```rust
        {
            let keys = Rc::downgrade(&keys);
            stack.connect_visible_child_name_notify(move |s| {
                if s.visible_child_name().as_deref() != Some("keys") {
                    if let Some(k) = keys.upgrade() {
                        k.clear_codes();
                    }
                }
            });
        }
```

（`keys` 是 `Rc<KeysPage>`，已在 `set_data` 前可用；这段要放在 `unsafe { window.set_data("keys-page", keys) }` 之前。）

- [ ] **Step 2: 编译 + 验收**

Run: `cargo build -p immurok-gui 2>&1 | grep -E "^(warning|error)"`
Expected: 无。

真机：OTP 条目 "Get code" → 触摸 → 行 subtitle 变大号等宽 `123 456` + Copy 按钮；点 Copy → 剪贴板有 6 位数字；切到别的页再回来 → 码已清；不切页 30 s 后自动清。

- [ ] **Step 3: 提交**

```bash
git add crates/immurok-gui/src/pages/keys.rs crates/immurok-gui/src/main_window.rs
git commit -m "gui(keys): OTP 取码改为行内大号显示 + Copy，30 s 或切页后清除"
```

---

### Task 10: 收尾验证

**Files:** 无新增

- [ ] **Step 1: 全量**

Run: `cargo build --workspace 2>&1 | grep -E "^(warning|error)"; cargo test --workspace 2>&1 | grep "test result"; cargo clippy --workspace 2>&1 | grep -E "^(warning|error)" | head`
Expected: 无 warning/error；全部 `ok`。

- [ ] **Step 2: CLI 回归（行为不变）**

```bash
./target/debug/immurok-cli key list ssh
./target/debug/immurok-cli key list otp
```

Expected: 与改前一致的列表输出。（`generate` / `import` 已在 GUI 验收里走过同一实现，不重复触摸。）

- [ ] **Step 3: 安装**

由用户执行 `make && make install`（含 sudo），然后在 GNOME + Fluent 下打开 immurok 确认：侧边栏 7 项、Fingerprints 有图标、Features 页开关可用、Keys 页 `+`/容量/空状态。

- [ ] **Step 4: 更新 CHANGELOG**

`CHANGELOG.md` 顶部 Unreleased 段加：

```
- gui: sidebar navigation; Features moved to its own page; bundled fingerprint icon
- gui(keys): add SSH (generate/import), OTP (single + file import), API entries; capacity and empty states; inline OTP code with Copy
- client: OTP/SSH parsing shared via immurok-client (keys_import / ssh_import); CLI/TUI use it
```

```bash
git add CHANGELOG.md
git commit -m "changelog: gui 侧边栏/Features 页/Keys 页补全"
```
