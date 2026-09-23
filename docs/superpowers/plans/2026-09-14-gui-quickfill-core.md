# Linux GUI + Quick-fill 阶段一实施计划（core）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 抽出共享的 `immurok-client` crate，新建 `immurok-gui`（GTK4/libadwaita），做出 Dashboard / Keys 两页和热键可呼出的 Quick-fill 面板，输出先走剪贴板。

**Architecture:** GUI 是 daemon 的另一个壳，与 TUI 平级：同一条 `/run/immurok/pam.sock`，一连接一请求的文本行协议。所有 socket 调用在 `gio::spawn_blocking` 里跑，结果用 `glib::spawn_future_local` 回主线程更新控件。Quick-fill 三条呼出路径（portal 热键 / X11 抓键 / DE 绑命令）收口到一个 GAction `quick-fill`，本阶段只做命令触发。

**Tech Stack:** Rust 2021、gtk4-rs 0.9、libadwaita-rs 0.7、glib 0.20、gio 0.20、serde_json；现有 `immurok-common`。

**Spec:** `docs/superpowers/specs/2026-09-14-gui-quickfill-design.md`

## Global Constraints

- 运行平台下限 Debian 12 / Ubuntu 22.04：`gtk4 = "0.9"`、`libadwaita = "0.7"`，**不开任何 `v4_*` / `v1_*` feature**；只用 libadwaita 1.0 控件（`ApplicationWindow`、`HeaderBar`、`ViewStack`、`ViewSwitcherTitle`、`PreferencesPage/Group`、`ActionRow`、`Toast`、`ToastOverlay`、`StatusPage`）。确认框用 `gtk::MessageDialog`。
- `immurok-client` 保持同步 std 实现，不引入 tokio。
- daemon 一连接一请求：每个 API 内部自己 `DaemonClient::connect()`。
- GUI 主线程禁止直接调 `DaemonClient`。
- TOTP / API 值不写日志、不进任何持久化。
- 应用 id 固定 `com.immurok.Settings`；二进制名 `immurok-gui`。
- 本计划的 `cargo build -p immurok-gui` 和手工验收必须在 Linux 开发机执行；macOS 上只能跑 `immurok-client` 的单测。
- 本地私有仓库 commit 用中文；改动限于 `app-linux-rs/`。
- 所有 crate 版本一起从 0.6.0 升到 0.7.0（Task 9），CHANGELOG 加条目。

---

## 文件结构

新建：
- `crates/immurok-client/Cargo.toml`
- `crates/immurok-client/src/lib.rs` — 导出 `daemon`、`status`、`keys`
- `crates/immurok-client/src/daemon.rs` — 原 `socket_client.rs` 原样搬入
- `crates/immurok-client/src/status.rs` — `DeviceStatus`、`parse_status_line`、`query_status`、`Settings`、`parse_settings_line`、`query_settings`、`set_setting`
- `crates/immurok-client/src/keys.rs` — `KeyCategory`、`KeyEntry`、`parse_key_cache`、`list_keys`、`get_otp`、`get_api`、`cancel_gate`、`delete_key`
- `crates/immurok-gui/Cargo.toml`
- `crates/immurok-gui/src/main.rs` — Application、actions、命令行分发
- `crates/immurok-gui/src/cli.rs` — `Launch` 与 `parse_launch`
- `crates/immurok-gui/src/main_window.rs` — 主窗口 + ViewStack
- `crates/immurok-gui/src/pages/mod.rs`
- `crates/immurok-gui/src/pages/dashboard.rs`
- `crates/immurok-gui/src/pages/keys.rs`
- `crates/immurok-gui/src/quickfill.rs` — 面板窗口与状态机
- `crates/immurok-gui/src/output.rs` — `Output` 枚举、`inject`、剪贴板 30 s 清空
- `crates/immurok-gui/src/filter.rs` — 纯函数 `filter_entries`
- `packaging/com.immurok.Settings.desktop`
- `packaging/com.immurok.Settings.service`
- `packaging/com.immurok.Settings.autostart.desktop`

修改：
- `Cargo.toml`（workspace members 自动含 `crates/*`，无需改；确认）
- `crates/immurok-cli/Cargo.toml` — 加 `immurok-client` 依赖
- `crates/immurok-cli/src/main.rs` — `mod socket_client;` → `use immurok_client as socket_client;`
- 删除 `crates/immurok-cli/src/socket_client.rs`
- `Makefile` — GTK 探测、`--exclude immurok-gui`、自启动条目安装/卸载
- `scripts/install-root.sh` / `scripts/uninstall-root.sh` — 二进制 + .desktop + D-Bus service
- `scripts/check-deps.sh` — GTK 开发头 warn
- `README.md`、`CHANGELOG.md`

---

### Task 1: 新建 `immurok-client` crate，搬入 socket 客户端

**Files:**
- Create: `crates/immurok-client/Cargo.toml`
- Create: `crates/immurok-client/src/lib.rs`
- Create: `crates/immurok-client/src/daemon.rs`（内容 = 现有 `crates/immurok-cli/src/socket_client.rs`）

**Interfaces:**
- Produces: `immurok_client::DaemonClient { connect() -> Result<Self,String>, send(&mut self,&str) -> Result<String,String>, send_with_timeout(&mut self,&str,Duration) -> Result<String,String> }`、`immurok_client::fetch_key_cache(kind:&str) -> Vec<serde_json::Value>`、`immurok_client::open_log_stream() -> Result<UnixStream,String>`、`immurok_client::probe_isolation() -> Option<Isolation>`、`immurok_client::Isolation`

- [ ] **Step 1: 建 crate 骨架**

`crates/immurok-client/Cargo.toml`：

```toml
[package]
name = "immurok-client"
version = "0.6.0"
edition = "2021"
license = "Apache-2.0"
description = "Synchronous client for the immurok daemon socket, shared by the TUI and the GUI"

[dependencies]
immurok-common = { path = "../immurok-common" }
libc = "0.2"
serde_json = "1"
```

`crates/immurok-client/src/lib.rs`：

```rust
//! Shared client for the immurok daemon socket.
//!
//! The daemon answers one request per connection and closes it, so every
//! helper here opens its own connection. Everything is synchronous std I/O:
//! the TUI calls it from `thread::spawn`, the GUI from `gio::spawn_blocking`.

pub mod daemon;
pub mod keys;
pub mod status;

pub use daemon::{fetch_key_cache, open_log_stream, probe_isolation, DaemonClient, Isolation};
```

- [ ] **Step 2: 搬文件**

```bash
git mv crates/immurok-cli/src/socket_client.rs crates/immurok-client/src/daemon.rs
```

把 `daemon.rs` 顶部的 `//! Synchronous Unix socket client for communicating with the daemon.` 保留即可，内容不改。

- [ ] **Step 3: 先放两个空模块让 lib 编译**

`crates/immurok-client/src/status.rs` 与 `crates/immurok-client/src/keys.rs` 先各写一行 `//! filled in by the next tasks`。

- [ ] **Step 4: 编译**

Run: `cargo build -p immurok-client`
Expected: 成功（immurok-cli 此刻会编译失败，Task 2 修）

- [ ] **Step 5: Commit**

```bash
git add crates/immurok-client crates/immurok-cli/src
git commit -m "client: 新建 immurok-client crate，搬入 socket 客户端"
```

---

### Task 2: immurok-cli 改用 `immurok-client`

**Files:**
- Modify: `crates/immurok-cli/Cargo.toml`
- Modify: `crates/immurok-cli/src/main.rs:3-8`

**Interfaces:**
- Consumes: Task 1 的 `immurok_client::*`

- [ ] **Step 1: 加依赖**

`crates/immurok-cli/Cargo.toml` `[dependencies]` 里 `immurok-common = ...` 下一行加：

```toml
immurok-client = { path = "../immurok-client" }
```

- [ ] **Step 2: 换掉模块声明**

`crates/immurok-cli/src/main.rs` 把

```rust
mod socket_client;
```

改为

```rust
// The socket client lives in its own crate now (shared with immurok-gui).
// Re-exported under the old module name so `crate::socket_client::…` paths
// across commands/ and tui/ keep working unchanged.
use immurok_client as socket_client;
```

其他文件（`commands/*.rs`、`tui/app.rs`、`imk_main.rs`）里的 `crate::socket_client::…` 不动。

- [ ] **Step 3: 全 workspace 编译 + 测试**

Run: `cargo build --workspace && cargo test -p immurok-cli -p immurok-client`
Expected: 全部通过，无 warning 关于 unused import

- [ ] **Step 4: 手工回归（有 daemon 的 Linux 机）**

Run: `target/debug/immurok-cli status && target/debug/immurok-cli tui`
Expected: status 输出与之前一致；TUI Keys 页 `o` 取码流程正常

- [ ] **Step 5: Commit**

```bash
git add crates/immurok-cli
git commit -m "cli: 改用 immurok-client crate，socket_client 模块名保留为别名"
```

---

### Task 3: `immurok-client::status` — 设备状态与功能开关

**Files:**
- Modify: `crates/immurok-client/src/status.rs`

**Interfaces:**
- Produces:
  - `pub struct DeviceStatus { pub connected: bool, pub name: String, pub battery: u8, pub fw_version: String, pub device_unpaired: bool }`
  - `pub fn parse_status_line(line: &str) -> Option<DeviceStatus>`
  - `pub fn query_status() -> Result<DeviceStatus, String>`
  - `pub struct Settings { pub unlock_sudo: bool, pub unlock_polkit: bool, pub unlock_screen: bool, pub lock_screen: bool, pub ssh_takeover: bool }`
  - `pub fn parse_settings_line(line: &str) -> Option<Settings>`
  - `pub fn query_settings() -> Result<Settings, String>`
  - `pub enum SettingKey { UnlockSudo, UnlockPolkit, UnlockScreen, LockScreen, SshTakeover }` 带 `fn wire(self) -> &'static str`
  - `pub fn set_setting(key: SettingKey, on: bool) -> Result<(), String>`
  - `pub fn query_paired() -> Result<bool, String>`

- [ ] **Step 1: 写测试**

在 `status.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_status_line() {
        let s = parse_status_line("STATUS:1:immurok IK-1:87:1.8.0:0").unwrap();
        assert!(s.connected);
        assert_eq!(s.name, "immurok IK-1");
        assert_eq!(s.battery, 87);
        assert_eq!(s.fw_version, "1.8.0");
        assert!(!s.device_unpaired);
    }

    #[test]
    fn parses_old_daemon_without_unpaired_flag() {
        let s = parse_status_line("STATUS:0:-:0:-").unwrap();
        assert!(!s.connected);
        assert!(!s.device_unpaired);
    }

    #[test]
    fn rejects_non_status_line() {
        assert!(parse_status_line("OK:PAIRED").is_none());
        assert!(parse_status_line("STATUS:1").is_none());
    }

    #[test]
    fn parses_settings() {
        let s = parse_settings_line("OK:sudo=1:polkit=0:screen=1:lock=0:ssh=1").unwrap();
        assert!(s.unlock_sudo);
        assert!(!s.unlock_polkit);
        assert!(s.unlock_screen);
        assert!(!s.lock_screen);
        assert!(s.ssh_takeover);
    }

    #[test]
    fn settings_rejects_error_reply() {
        assert!(parse_settings_line("ERROR:NOT_CONNECTED").is_none());
    }

    #[test]
    fn setting_key_wire_names_match_daemon() {
        assert_eq!(SettingKey::UnlockSudo.wire(), "UNLOCK_SUDO");
        assert_eq!(SettingKey::UnlockPolkit.wire(), "UNLOCK_POLKIT");
        assert_eq!(SettingKey::UnlockScreen.wire(), "UNLOCK_SCREEN");
        assert_eq!(SettingKey::LockScreen.wire(), "LOCK_SCREEN");
        assert_eq!(SettingKey::SshTakeover.wire(), "SSH_TAKEOVER");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p immurok-client status`
Expected: 编译错误 `cannot find function parse_status_line`

- [ ] **Step 3: 实现**

`status.rs` 全文：

```rust
//! Device status and feature toggles.
//!
//! Wire formats (see `immurok-common/src/socket_proto.rs` and
//! `immurok-daemon/src/socket.rs`):
//!   STATUS         → `STATUS:<connected 0/1>:<name>:<battery>:<fw>[:<device_unpaired 0/1>]`
//!   GET:SETTINGS   → `OK:sudo=1:polkit=0:screen=1:lock=0:ssh=1`
//!   SET:<KEY>:<0|1> → `OK:…`
//!   PAIR:STATUS    → `OK:PAIRED` / `OK:UNPAIRED`

use crate::DaemonClient;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceStatus {
    pub connected: bool,
    pub name: String,
    pub battery: u8,
    pub fw_version: String,
    /// The device itself says it is not paired with this host (factory
    /// reset / slot cleared). Older daemons omit the field.
    pub device_unpaired: bool,
}

pub fn parse_status_line(line: &str) -> Option<DeviceStatus> {
    let parts: Vec<&str> = line.trim().split(':').collect();
    if parts.first() != Some(&"STATUS") || parts.len() < 5 {
        return None;
    }
    Some(DeviceStatus {
        connected: parts[1] == "1",
        name: parts[2].to_string(),
        battery: parts[3].parse().unwrap_or(0),
        fw_version: parts[4].to_string(),
        device_unpaired: parts.get(5) == Some(&"1"),
    })
}

pub fn query_status() -> Result<DeviceStatus, String> {
    let rsp = DaemonClient::connect()?.send("STATUS")?;
    parse_status_line(&rsp).ok_or_else(|| format!("unexpected STATUS reply: {rsp}"))
}

pub fn query_paired() -> Result<bool, String> {
    let rsp = DaemonClient::connect()?.send("PAIR:STATUS")?;
    Ok(rsp.split(':').nth(1) == Some("PAIRED"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Settings {
    pub unlock_sudo: bool,
    pub unlock_polkit: bool,
    pub unlock_screen: bool,
    pub lock_screen: bool,
    pub ssh_takeover: bool,
}

pub fn parse_settings_line(line: &str) -> Option<Settings> {
    let mut parts = line.trim().split(':');
    if parts.next() != Some("OK") {
        return None;
    }
    let mut s = Settings::default();
    for part in parts {
        if let Some((k, v)) = part.split_once('=') {
            let on = v == "1";
            match k {
                "sudo" => s.unlock_sudo = on,
                "polkit" => s.unlock_polkit = on,
                "screen" => s.unlock_screen = on,
                "lock" => s.lock_screen = on,
                "ssh" => s.ssh_takeover = on,
                _ => {}
            }
        }
    }
    Some(s)
}

pub fn query_settings() -> Result<Settings, String> {
    let rsp = DaemonClient::connect()?.send("GET:SETTINGS")?;
    parse_settings_line(&rsp).ok_or_else(|| format!("unexpected GET:SETTINGS reply: {rsp}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKey {
    UnlockSudo,
    UnlockPolkit,
    UnlockScreen,
    LockScreen,
    SshTakeover,
}

impl SettingKey {
    pub fn wire(self) -> &'static str {
        match self {
            SettingKey::UnlockSudo => "UNLOCK_SUDO",
            SettingKey::UnlockPolkit => "UNLOCK_POLKIT",
            SettingKey::UnlockScreen => "UNLOCK_SCREEN",
            SettingKey::LockScreen => "LOCK_SCREEN",
            SettingKey::SshTakeover => "SSH_TAKEOVER",
        }
    }
}

pub fn set_setting(key: SettingKey, on: bool) -> Result<(), String> {
    let cmd = format!("SET:{}:{}", key.wire(), if on { 1 } else { 0 });
    let rsp = DaemonClient::connect()?.send(&cmd)?;
    if rsp.starts_with("OK") {
        Ok(())
    } else {
        Err(rsp)
    }
}
```

- [ ] **Step 4: 跑测试**

Run: `cargo test -p immurok-client status`
Expected: 6 passed

- [ ] **Step 5: Commit**

```bash
git add crates/immurok-client/src/status.rs
git commit -m "client: 类型化 STATUS / GET:SETTINGS / SET 解析"
```

---

### Task 4: `immurok-client::keys` — 密钥列表与 FP 门控读取

**Files:**
- Modify: `crates/immurok-client/src/keys.rs`

**Interfaces:**
- Produces:
  - `pub enum KeyCategory { Ssh, Otp, Api }` 带 `fn label(self) -> &'static str`（"SSH"/"OTP"/"API"）、`fn wire(self) -> &'static str`（"ssh"/"otp"/"api"）
  - `pub struct KeyEntry { pub index: u8, pub category: KeyCategory, pub name: String, pub service: String, pub ssh_pubkey_b64: String }`
  - `pub fn parse_key_cache(ssh: &[serde_json::Value], names: &[serde_json::Value]) -> Vec<KeyEntry>`（顺序：OTP、API、SSH，各组内按 index 升序）
  - `pub fn list_keys() -> Vec<KeyEntry>`
  - `pub fn get_otp(name: &str) -> Result<String, String>`（40 s 超时，返回 6 位数字）
  - `pub fn get_api(name: &str) -> Result<String, String>`
  - `pub fn ssh_public_key_line(entry: &KeyEntry) -> String`（`ecdsa-sha2-nistp256 <b64> <name>`）
  - `pub fn cancel_gate()`
  - `pub fn delete_key(category: KeyCategory, index: u8) -> Result<(), String>`
  - `pub const GATE_TIMEOUT: Duration = Duration::from_secs(40)`

- [ ] **Step 1: 写测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merges_caches_otp_api_ssh_in_that_order() {
        let ssh = vec![json!({"index": 2, "name": "work", "fingerprint": "SHA256:x", "public_key_blob": "QUJD"})];
        let names = vec![
            json!({"index": 1, "category": "api", "name": "openai", "service": ""}),
            json!({"index": 3, "category": "otp", "name": "github", "service": "GitHub"}),
            json!({"index": 0, "category": "otp", "name": "aws", "service": "Amazon"}),
        ];
        let out = parse_key_cache(&ssh, &names);
        let names_in_order: Vec<&str> = out.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names_in_order, ["aws", "github", "openai", "work"]);
        assert_eq!(out[0].category, KeyCategory::Otp);
        assert_eq!(out[0].service, "Amazon");
        assert_eq!(out[3].category, KeyCategory::Ssh);
        assert_eq!(out[3].ssh_pubkey_b64, "QUJD");
    }

    #[test]
    fn skips_unknown_categories() {
        let names = vec![json!({"index": 0, "category": "weird", "name": "x", "service": ""})];
        assert!(parse_key_cache(&[], &names).is_empty());
    }

    #[test]
    fn ssh_line_is_authorized_keys_format() {
        let e = KeyEntry {
            index: 0,
            category: KeyCategory::Ssh,
            name: "work".into(),
            service: String::new(),
            ssh_pubkey_b64: "QUJD".into(),
        };
        assert_eq!(ssh_public_key_line(&e), "ecdsa-sha2-nistp256 QUJD work");
    }

    #[test]
    fn otp_reply_parsing() {
        assert_eq!(parse_secret_reply("OK:123456"), Ok("123456".to_string()));
        assert_eq!(parse_secret_reply("ERROR:BUSY"), Err("ERROR:BUSY".to_string()));
        assert_eq!(parse_secret_reply("DENY:GATE_TIMEOUT"), Err("DENY:GATE_TIMEOUT".to_string()));
    }

    #[test]
    fn category_wire_names() {
        assert_eq!(KeyCategory::Otp.wire(), "otp");
        assert_eq!(KeyCategory::Api.wire(), "api");
        assert_eq!(KeyCategory::Ssh.wire(), "ssh");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p immurok-client keys`
Expected: 编译失败，缺 `KeyEntry` 等

- [ ] **Step 3: 实现**

```rust
//! Key store access: list from the daemon's cache, read secrets through the
//! device's fingerprint gate.
//!
//! Wire formats (`immurok-daemon/src/socket.rs`):
//!   KEY:CACHE:ssh    → `OK:[{"index":0,"name":"…","fingerprint":"…","public_key_blob":"<b64>"}]`
//!   KEY:CACHE:names  → `OK:[{"index":0,"category":"otp"|"api","name":"…","service":"…"}]`
//!   GET:otp:<name>   → `OK:123456` after a fingerprint touch (30 s gate)
//!   GET:api:<name>   → `OK:<value>` after a fingerprint touch
//!   KEY:DELETE:<cat>:<idx> → `OK:DELETED`
//!   GATE:CANCEL      → `OK:…`

use std::time::Duration;

use crate::{fetch_key_cache, DaemonClient};

/// Fingerprint gate is 30 s on the device; leave room for the BLE round trip.
pub const GATE_TIMEOUT: Duration = Duration::from_secs(40);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCategory {
    Ssh,
    Otp,
    Api,
}

impl KeyCategory {
    pub fn label(self) -> &'static str {
        match self {
            KeyCategory::Ssh => "SSH",
            KeyCategory::Otp => "OTP",
            KeyCategory::Api => "API",
        }
    }

    pub fn wire(self) -> &'static str {
        match self {
            KeyCategory::Ssh => "ssh",
            KeyCategory::Otp => "otp",
            KeyCategory::Api => "api",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEntry {
    pub index: u8,
    pub category: KeyCategory,
    pub name: String,
    /// Issuer / service (OTP only; empty otherwise).
    pub service: String,
    /// Base64 public key blob (SSH only; empty otherwise).
    pub ssh_pubkey_b64: String,
}

pub fn parse_key_cache(ssh: &[serde_json::Value], names: &[serde_json::Value]) -> Vec<KeyEntry> {
    let mut otp = Vec::new();
    let mut api = Vec::new();
    for e in names {
        let category = match e["category"].as_str() {
            Some("otp") => KeyCategory::Otp,
            Some("api") => KeyCategory::Api,
            _ => continue,
        };
        let entry = KeyEntry {
            index: e["index"].as_u64().unwrap_or(0) as u8,
            category,
            name: e["name"].as_str().unwrap_or("-").to_string(),
            service: e["service"].as_str().unwrap_or("").to_string(),
            ssh_pubkey_b64: String::new(),
        };
        match category {
            KeyCategory::Otp => otp.push(entry),
            _ => api.push(entry),
        }
    }
    let mut sshv: Vec<KeyEntry> = ssh
        .iter()
        .map(|e| KeyEntry {
            index: e["index"].as_u64().unwrap_or(0) as u8,
            category: KeyCategory::Ssh,
            name: e["name"].as_str().unwrap_or("-").to_string(),
            service: String::new(),
            ssh_pubkey_b64: e["public_key_blob"].as_str().unwrap_or("").to_string(),
        })
        .collect();
    otp.sort_by_key(|e| e.index);
    api.sort_by_key(|e| e.index);
    sshv.sort_by_key(|e| e.index);
    otp.extend(api);
    otp.extend(sshv);
    otp
}

/// Everything the daemon has cached. No device round-trip; works offline.
pub fn list_keys() -> Vec<KeyEntry> {
    let ssh = fetch_key_cache("ssh");
    let names = fetch_key_cache("names");
    parse_key_cache(&ssh, &names)
}

fn parse_secret_reply(rsp: &str) -> Result<String, String> {
    match rsp.trim().strip_prefix("OK:") {
        Some(v) => Ok(v.to_string()),
        None => Err(rsp.trim().to_string()),
    }
}

fn get_gated(cat: KeyCategory, name: &str) -> Result<String, String> {
    let cmd = format!("GET:{}:{}", cat.wire(), name);
    let rsp = DaemonClient::connect()?.send_with_timeout(&cmd, GATE_TIMEOUT)?;
    parse_secret_reply(&rsp)
}

/// Six-digit TOTP for the named OTP entry. Blocks until the user touches the
/// sensor or the gate times out.
pub fn get_otp(name: &str) -> Result<String, String> {
    get_gated(KeyCategory::Otp, name)
}

/// Stored API value for the named entry. Same gate as OTP.
pub fn get_api(name: &str) -> Result<String, String> {
    get_gated(KeyCategory::Api, name)
}

pub fn ssh_public_key_line(entry: &KeyEntry) -> String {
    format!("ecdsa-sha2-nistp256 {} {}", entry.ssh_pubkey_b64, entry.name)
}

/// Best effort: abort a pending fingerprint gate. Errors are ignored — if the
/// daemon is gone there is nothing left to cancel.
pub fn cancel_gate() {
    if let Ok(mut c) = DaemonClient::connect() {
        let _ = c.send("GATE:CANCEL");
    }
}

pub fn delete_key(category: KeyCategory, index: u8) -> Result<(), String> {
    let cmd = format!("KEY:DELETE:{}:{}", category.wire(), index);
    let rsp = DaemonClient::connect()?.send_with_timeout(&cmd, GATE_TIMEOUT)?;
    if rsp.starts_with("OK") {
        Ok(())
    } else {
        Err(rsp)
    }
}
```

- [ ] **Step 4: 跑测试**

Run: `cargo test -p immurok-client`
Expected: keys 5 passed，status 6 passed

- [ ] **Step 5: Commit**

```bash
git add crates/immurok-client/src/keys.rs
git commit -m "client: 密钥缓存解析与 FP 门控读取 API"
```

---

### Task 5: `immurok-gui` 骨架：Application、actions、命令行分发

**Files:**
- Create: `crates/immurok-gui/Cargo.toml`
- Create: `crates/immurok-gui/src/main.rs`
- Create: `crates/immurok-gui/src/cli.rs`
- Create: `crates/immurok-gui/src/main_window.rs`（本任务先放空页面）
- Create: `crates/immurok-gui/src/pages/mod.rs`（空）
- Create: `crates/immurok-gui/src/quickfill.rs`（本任务只放 `pub fn open(app: &adw::Application)` 打印一行日志，Task 8 填实）
- Create: `crates/immurok-gui/src/output.rs`（空模块）
- Create: `crates/immurok-gui/src/filter.rs`（空模块）

**Interfaces:**
- Produces:
  - `cli::Launch { Main, QuickFill, Service }`、`cli::parse_launch(args: &[String]) -> Launch`
  - GAction `show`（开主窗口）、`quick-fill`（开面板），挂在 `adw::Application` 上
  - `main_window::MainWindow::present_for(app: &adw::Application)`（单例：已存在则 `present()`）

- [ ] **Step 1: Cargo.toml**

```toml
[package]
name = "immurok-gui"
version = "0.6.0"
edition = "2021"
license = "Apache-2.0"
description = "immurok graphical settings app and quick-fill panel"

[[bin]]
name = "immurok-gui"
path = "src/main.rs"

[dependencies]
immurok-client = { path = "../immurok-client" }
immurok-common = { path = "../immurok-common" }
gtk4 = "0.9"
libadwaita = "0.7"
glib = "0.20"
gio = "0.20"
```

不加任何 feature：Debian 12 的 GTK 4.8 / libadwaita 1.2 和 Ubuntu 22.04 的 GTK 4.6 / libadwaita 1.1 都要能跑。

- [ ] **Step 2: 写 `cli.rs` 的测试与实现**

```rust
//! Command-line entry parsing. Kept free of GTK so it can be unit-tested.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// Show the main settings window.
    Main,
    /// Open the quick-fill panel (hotkey path).
    QuickFill,
    /// Started by D-Bus activation / autostart: stay resident, no window.
    Service,
}

/// `args` includes argv[0].
pub fn parse_launch(args: &[String]) -> Launch {
    let mut launch = Launch::Main;
    for a in args.iter().skip(1) {
        match a.as_str() {
            "--quick-fill" => return Launch::QuickFill,
            "--gapplication-service" => launch = Launch::Service,
            _ => {}
        }
    }
    launch
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_args_is_main() {
        assert_eq!(parse_launch(&v(&["immurok-gui"])), Launch::Main);
    }

    #[test]
    fn quick_fill_flag() {
        assert_eq!(parse_launch(&v(&["immurok-gui", "--quick-fill"])), Launch::QuickFill);
    }

    #[test]
    fn service_flag() {
        assert_eq!(parse_launch(&v(&["immurok-gui", "--gapplication-service"])), Launch::Service);
    }

    #[test]
    fn quick_fill_wins_over_service() {
        assert_eq!(
            parse_launch(&v(&["immurok-gui", "--gapplication-service", "--quick-fill"])),
            Launch::QuickFill
        );
    }
}
```

- [ ] **Step 3: `main.rs`**

```rust
//! immurok-gui — graphical settings app + hotkey quick-fill panel.
//!
//! One `adw::Application` with id `com.immurok.Settings`. GApplication's
//! single-instance machinery does the rest: a second `immurok-gui
//! --quick-fill` forwards its command line to the running instance, which
//! fires the `quick-fill` action. Portal shortcuts (phase 2) and X11 key
//! grabs fire the same action, so every entry path converges here.

mod cli;
mod filter;
mod main_window;
mod output;
mod pages;
mod quickfill;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

pub const APP_ID: &str = "com.immurok.Settings";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    app.connect_startup(|app| {
        register_actions(app);
    });

    // HANDLES_COMMAND_LINE: this runs in the *primary* instance for both the
    // first launch and every forwarded invocation.
    app.connect_command_line(|app, cmdline| {
        let args: Vec<String> = cmdline
            .arguments()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        match cli::parse_launch(&args) {
            cli::Launch::QuickFill => app.activate_action("quick-fill", None),
            cli::Launch::Main => app.activate_action("show", None),
            cli::Launch::Service => {
                // Stay alive with no window so the hotkey path is instant.
                // The guard is intentionally leaked: it lives as long as the
                // process does.
                std::mem::forget(app.hold());
            }
        }
        0
    });

    app.run()
}

fn register_actions(app: &adw::Application) {
    let show = gio::SimpleAction::new("show", None);
    show.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| main_window::MainWindow::present_for(&app)
    ));
    app.add_action(&show);

    let quick = gio::SimpleAction::new("quick-fill", None);
    quick.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| quickfill::open(&app)
    ));
    app.add_action(&quick);

    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| app.quit()
    ));
    app.add_action(&quit);
    app.set_accels_for_action("app.quit", &["<Primary>q"]);
}
```

- [ ] **Step 4: 占位的 `main_window.rs` / `quickfill.rs` / 空模块**

`main_window.rs`：

```rust
//! Main settings window: header bar + view stack of pages.

use adw::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

pub struct MainWindow;

impl MainWindow {
    /// Present the (single) main window, creating it on first use.
    pub fn present_for(app: &adw::Application) {
        if let Some(existing) = app.windows().into_iter().find(|w| w.widget_name() == "main") {
            existing.present();
            return;
        }
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("immurok")
            .default_width(720)
            .default_height(520)
            .build();
        window.set_widget_name("main");

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&adw::HeaderBar::new());
        content.append(
            &adw::StatusPage::builder()
                .title("immurok")
                .description("Pages arrive in the next tasks.")
                .vexpand(true)
                .build(),
        );
        window.set_content(Some(&content));
        window.present();
    }
}
```

`quickfill.rs`：

```rust
//! Quick-fill panel (filled in by Task 8).

use libadwaita as adw;

pub fn open(_app: &adw::Application) {
    eprintln!("immurok-gui: quick-fill requested");
}
```

`pages/mod.rs`、`output.rs`、`filter.rs`：各一行 `//! filled in later`。

- [ ] **Step 5: 编译 + 单测（Linux）**

Run: `cargo build -p immurok-gui && cargo test -p immurok-gui`
Expected: 编译成功；cli 4 个测试通过

- [ ] **Step 6: 手工验证单实例转发**

终端 A：`target/debug/immurok-gui`（出现主窗口）
终端 B：`target/debug/immurok-gui --quick-fill`
Expected: 终端 B 立刻退出；终端 A 打印 `immurok-gui: quick-fill requested`；`Ctrl+Q` 关闭 A。

再试：`target/debug/immurok-gui --gapplication-service &`，然后 `target/debug/immurok-gui`（应在 service 实例上开窗口，`ps` 里只有一个 immurok-gui）。

- [ ] **Step 7: Commit**

```bash
git add crates/immurok-gui
git commit -m "gui: 新建 immurok-gui crate，单实例 Application 与 show/quick-fill action"
```

---

### Task 6: Dashboard 页：状态、开关、配对/解除

**Files:**
- Create: `crates/immurok-gui/src/pages/dashboard.rs`
- Modify: `crates/immurok-gui/src/pages/mod.rs`
- Modify: `crates/immurok-gui/src/main_window.rs`

**Interfaces:**
- Consumes: `immurok_client::status::{query_status, query_settings, query_paired, set_setting, SettingKey, DeviceStatus, Settings}`、`immurok_client::DaemonClient`
- Produces:
  - `pages::dashboard::DashboardPage { pub fn new(toasts: &adw::ToastOverlay) -> Self, pub fn widget(&self) -> &gtk::Widget, pub fn start_polling(&self, window: &adw::ApplicationWindow) }`
  - `pages::run_blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> impl Future<Output = Option<T>>`（`gio::spawn_blocking` 的薄封装，panic 时返回 None）

- [ ] **Step 1: `pages/mod.rs` 通用助手**

```rust
//! Pages of the main window. Every daemon round-trip goes through
//! [`run_blocking`] — a BLE-backed request can take seconds and must never
//! run on the GTK main thread.

pub mod dashboard;
pub mod keys;

use gtk4 as gtk;
use gtk::gio;

pub async fn run_blocking<T, F>(f: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    gio::spawn_blocking(f).await.ok()
}

/// Ask the user a yes/no question. Resolves to `true` on the affirmative
/// button. `gtk::MessageDialog` rather than `adw::MessageDialog`: the latter
/// is libadwaita 1.2 and Ubuntu 22.04 ships 1.1.
pub async fn confirm(parent: &impl glib::object::IsA<gtk::Window>, title: &str, body: &str, yes: &str) -> bool {
    use gtk::prelude::*;
    let dialog = gtk::MessageDialog::builder()
        .transient_for(parent)
        .modal(true)
        .message_type(gtk::MessageType::Question)
        .text(title)
        .secondary_text(body)
        .build();
    dialog.add_button("取消", gtk::ResponseType::Cancel);
    dialog.add_button(yes, gtk::ResponseType::Accept);
    let response = dialog.run_future().await;
    dialog.close();
    response == gtk::ResponseType::Accept
}

use gtk::glib;
```

`keys` 模块在 Task 7 建；本任务先建空文件 `pages/keys.rs` 写一行注释以便编译。

- [ ] **Step 2: `pages/dashboard.rs`**

```rust
//! Dashboard: device status, feature toggles, pair / unpair.
//!
//! Mirrors the TUI Dashboard minus enrollment (phase 3). State is polled
//! every 2 s while the window is visible; toggles write through immediately
//! and only flip the switch once the daemon has confirmed.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::status::{
    query_paired, query_settings, query_status, set_setting, DeviceStatus, SettingKey, Settings,
};
use immurok_client::DaemonClient;

use super::run_blocking;

pub struct DashboardPage {
    root: gtk::Widget,
    status_row: adw::ActionRow,
    battery_row: adw::ActionRow,
    fw_row: adw::ActionRow,
    pair_button: gtk::Button,
    switches: Vec<(SettingKey, gtk::Switch)>,
    toasts: adw::ToastOverlay,
    /// Set while a poll is in flight so a slow daemon does not pile up.
    polling: Rc<Cell<bool>>,
    paired: Rc<Cell<bool>>,
}

impl DashboardPage {
    pub fn new(toasts: &adw::ToastOverlay) -> Self {
        let page = adw::PreferencesPage::new();

        // ── Device ──
        let device = adw::PreferencesGroup::builder().title("设备").build();
        let status_row = adw::ActionRow::builder().title("状态").subtitle("正在连接 daemon…").build();
        let battery_row = adw::ActionRow::builder().title("电量").subtitle("-").build();
        let fw_row = adw::ActionRow::builder().title("固件").subtitle("-").build();
        let pair_button = gtk::Button::builder().label("配对").valign(gtk::Align::Center).build();
        let pair_row = adw::ActionRow::builder().title("配对").build();
        pair_row.add_suffix(&pair_button);
        device.add(&status_row);
        device.add(&battery_row);
        device.add(&fw_row);
        device.add(&pair_row);
        page.add(&device);

        // ── Features ──
        let features = adw::PreferencesGroup::builder().title("功能").build();
        let mut switches = Vec::new();
        for (key, title, subtitle) in [
            (SettingKey::UnlockSudo, "sudo 指纹授权", "终端 sudo 时触摸设备代替密码"),
            (SettingKey::UnlockPolkit, "系统授权（polkit）", "图形授权框触摸设备代替密码"),
            (SettingKey::UnlockScreen, "指纹解锁屏幕", "锁屏时触摸设备解锁"),
            (SettingKey::LockScreen, "长按锁屏", "长按传感器锁定屏幕"),
            (SettingKey::SshTakeover, "SSH agent 接管", "让 ssh 使用设备上的密钥"),
        ] {
            let sw = gtk::Switch::builder().valign(gtk::Align::Center).build();
            let row = adw::ActionRow::builder().title(title).subtitle(subtitle).build();
            row.add_suffix(&sw);
            row.set_activatable_widget(Some(&sw));
            features.add(&row);
            switches.push((key, sw));
        }
        page.add(&features);

        let this = Self {
            root: page.upcast(),
            status_row,
            battery_row,
            fw_row,
            pair_button,
            switches,
            toasts: toasts.clone(),
            polling: Rc::new(Cell::new(false)),
            paired: Rc::new(Cell::new(false)),
        };
        this.wire_switches();
        this.wire_pair_button();
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        &self.root
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    fn wire_switches(&self) {
        for (key, sw) in &self.switches {
            let key = *key;
            let toasts = self.toasts.clone();
            // Return Stop: we own the `state` property and flip it only after
            // the daemon confirms, so a failed write leaves the switch where
            // it was instead of lying.
            sw.connect_state_set(move |sw, wanted| {
                let sw = sw.clone();
                let toasts = toasts.clone();
                glib::spawn_future_local(async move {
                    match run_blocking(move || set_setting(key, wanted)).await {
                        Some(Ok(())) => sw.set_state(wanted),
                        Some(Err(e)) => {
                            toasts.add_toast(adw::Toast::new(&format!("设置失败：{e}")));
                            sw.set_active(!wanted);
                        }
                        None => sw.set_active(!wanted),
                    }
                });
                glib::Propagation::Stop
            });
        }
    }

    fn wire_pair_button(&self) {
        let button = self.pair_button.clone();
        let toasts = self.toasts.clone();
        let paired = self.paired.clone();
        button.connect_clicked(move |b| {
            let b = b.clone();
            let toasts = toasts.clone();
            let paired = paired.clone();
            glib::spawn_future_local(async move {
                b.set_sensitive(false);
                if paired.get() {
                    // Same y/n gate the TUI has: this clears THIS host's slot.
                    let win = b.root().and_then(|r| r.downcast::<gtk::Window>().ok());
                    let ok = match win {
                        Some(w) => super::confirm(&w, "解除与这台电脑的配对？", "指纹和密钥保留在设备上。", "解除配对").await,
                        None => false,
                    };
                    if ok {
                        b.set_label("正在解除…");
                        let r = run_blocking(|| DaemonClient::connect()?.send("SLOT:CLEAR")).await;
                        match r {
                            Some(Ok(rsp)) if rsp.starts_with("OK") => toasts.add_toast(adw::Toast::new("已解除配对")),
                            Some(Ok(rsp)) => toasts.add_toast(adw::Toast::new(&format!("解除失败：{rsp}"))),
                            Some(Err(e)) => toasts.add_toast(adw::Toast::new(&format!("解除失败：{e}"))),
                            None => {}
                        }
                    }
                } else {
                    b.set_label("配对中，按设备按键…");
                    toasts.add_toast(adw::Toast::new("请在设备上确认配对（最多 150 秒）"));
                    let r = run_blocking(|| {
                        DaemonClient::connect()?
                            .send_with_timeout("PAIR:START", Duration::from_secs(150))
                    })
                    .await;
                    match r {
                        Some(Ok(rsp)) if rsp == "OK:PAIRED" => toasts.add_toast(adw::Toast::new("配对成功")),
                        Some(Ok(rsp)) => toasts.add_toast(adw::Toast::new(&format!("配对失败：{rsp}"))),
                        Some(Err(e)) => toasts.add_toast(adw::Toast::new(&format!("配对失败：{e}"))),
                        None => {}
                    }
                }
                b.set_sensitive(true);
            });
        });
    }

    /// Poll every 2 s while `window` is visible. Call once after construction.
    pub fn start_polling(self: &Rc<Self>, window: &adw::ApplicationWindow) {
        let this = Rc::downgrade(self);
        let window = window.clone();
        // Fire once immediately, then on the timer.
        Self::poll_once(self);
        glib::timeout_add_local(Duration::from_secs(2), move || {
            let Some(this) = this.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if window.is_visible() {
                Self::poll_once(&this);
            }
            glib::ControlFlow::Continue
        });
    }

    fn poll_once(this: &Rc<Self>) {
        if this.polling.replace(true) {
            return;
        }
        let weak = Rc::downgrade(this);
        glib::spawn_future_local(async move {
            let result = run_blocking(|| {
                let status = query_status();
                let settings = status.as_ref().ok().and_then(|_| query_settings().ok());
                let paired = query_paired().unwrap_or(false);
                (status, settings, paired)
            })
            .await;
            let Some(this) = weak.upgrade() else { return };
            this.polling.set(false);
            match result {
                Some((Ok(status), settings, paired)) => this.apply(&status, settings.as_ref(), paired),
                Some((Err(e), _, _)) => this.apply_daemon_down(&e),
                None => {}
            }
        });
    }

    fn apply(&self, status: &DeviceStatus, settings: Option<&Settings>, paired: bool) {
        self.paired.set(paired);
        let state = if status.device_unpaired {
            "设备已不再与本机配对".to_string()
        } else if status.connected {
            format!("已连接 · {}", status.name)
        } else if paired {
            "已配对，未连接".to_string()
        } else {
            "未配对".to_string()
        };
        self.status_row.set_subtitle(&state);
        self.battery_row.set_subtitle(&if status.connected { format!("{}%", status.battery) } else { "-".into() });
        self.fw_row.set_subtitle(&if status.connected { status.fw_version.clone() } else { "-".into() });
        self.pair_button.set_label(if paired { "解除配对" } else { "配对" });
        self.pair_button.set_sensitive(status.connected || paired);

        if let Some(s) = settings {
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
    }

    fn apply_daemon_down(&self, err: &str) {
        self.status_row.set_subtitle(&format!("daemon 不可用：{err}"));
        self.battery_row.set_subtitle("-");
        self.fw_row.set_subtitle("-");
        self.pair_button.set_sensitive(false);
    }
}
```

注意 `set_state` + `set_active` 同时调：`connect_state_set` 返回 `Stop` 后 GTK 不再自动同步 `state`，两者都要设，否则开关显示与 `active` 不一致。

- [ ] **Step 3: 主窗口挂上 Dashboard**

`main_window.rs` 替换为：

```rust
//! Main settings window: header bar + view stack of pages.

use std::rc::Rc;

use adw::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

use crate::pages;

pub struct MainWindow;

impl MainWindow {
    pub fn present_for(app: &adw::Application) {
        if let Some(existing) = app.windows().into_iter().find(|w| w.widget_name() == "main") {
            existing.present();
            return;
        }
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("immurok")
            .default_width(720)
            .default_height(560)
            .build();
        window.set_widget_name("main");

        let toasts = adw::ToastOverlay::new();
        let stack = adw::ViewStack::new();

        let dashboard = Rc::new(pages::dashboard::DashboardPage::new(&toasts));
        stack
            .add_titled(dashboard.widget(), Some("dashboard"), "设备")
            .set_icon_name(Some("preferences-system-symbolic"));

        let switcher = adw::ViewSwitcherTitle::builder().stack(&stack).title("immurok").build();
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&switcher));

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header);
        content.append(&stack);
        stack.set_vexpand(true);
        toasts.set_child(Some(&content));
        window.set_content(Some(&toasts));

        dashboard.start_polling(&window);
        // Keep the page alive as long as the window; the poll closure only
        // holds a Weak so a closed window lets everything drop.
        unsafe { window.set_data("dashboard-page", dashboard) };

        window.present();
    }
}
```

- [ ] **Step 4: 编译并手工验收**

Run: `cargo build -p immurok-gui && target/debug/immurok-gui`
Expected:
- 「设备」组显示与 `immurok-cli status` 一致的连接状态、电量、固件。
- 切一个开关，`immurok-cli settings` 反映变化；把 daemon 停掉（`sudo systemctl stop immurok-daemon`）后切开关，开关弹回、出现 toast「设置失败」，状态行显示 daemon 不可用；起回 daemon 后 2 s 内恢复。

- [ ] **Step 5: Commit**

```bash
git add crates/immurok-gui
git commit -m "gui: Dashboard 页，状态轮询、功能开关、配对/解除"
```

---

### Task 7: Keys 页：三类列表、取码、看公钥、删除

**Files:**
- Create: `crates/immurok-gui/src/pages/keys.rs`
- Modify: `crates/immurok-gui/src/main_window.rs`

**Interfaces:**
- Consumes: `immurok_client::keys::{list_keys, get_otp, get_api, delete_key, ssh_public_key_line, KeyEntry, KeyCategory}`、`pages::{run_blocking, confirm}`
- Produces: `pages::keys::KeysPage { pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self>, pub fn widget(&self) -> &gtk::Widget, pub fn reload(self: &Rc<Self>) }`

- [ ] **Step 1: 实现**

```rust
//! Keys page: SSH / OTP / API entries from the daemon cache.
//!
//! Secrets (OTP code, API value) are shown in a toast for 30 s and nowhere
//! else — the TUI's `SecretMessage` rule. Delete goes through the device's
//! fingerprint gate, so the button shows a "touch" hint while waiting.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::keys::{
    delete_key, get_api, get_otp, list_keys, ssh_public_key_line, KeyCategory, KeyEntry,
};

use super::{confirm, run_blocking};

pub struct KeysPage {
    root: gtk::Widget,
    groups: Vec<(KeyCategory, adw::PreferencesGroup)>,
    rows: RefCell<Vec<adw::ActionRow>>,
    toasts: adw::ToastOverlay,
}

impl KeysPage {
    pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self> {
        let page = adw::PreferencesPage::new();
        let mut groups = Vec::new();
        for (cat, desc) in [
            (KeyCategory::Otp, "触摸设备后生成 6 位验证码"),
            (KeyCategory::Api, "触摸设备后显示存储的值"),
            (KeyCategory::Ssh, "公钥可直接复制，签名走 SSH agent"),
        ] {
            let g = adw::PreferencesGroup::builder().title(cat.label()).description(desc).build();
            page.add(&g);
            groups.push((cat, g));
        }
        let this = Rc::new(Self {
            root: page.upcast(),
            groups,
            rows: RefCell::new(Vec::new()),
            toasts: toasts.clone(),
        });
        this.reload();
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        &self.root
    }

    fn toast(&self, text: &str, secs: u32) {
        let t = adw::Toast::new(text);
        t.set_timeout(secs);
        self.toasts.add_toast(t);
    }

    pub fn reload(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let entries = run_blocking(list_keys).await.unwrap_or_default();
            if let Some(this) = weak.upgrade() {
                this.populate(entries);
            }
        });
    }

    fn populate(self: &Rc<Self>, entries: Vec<KeyEntry>) {
        for row in self.rows.borrow_mut().drain(..) {
            for (_, g) in &self.groups {
                g.remove(&row);
            }
        }
        for entry in entries {
            let Some((_, group)) = self.groups.iter().find(|(c, _)| *c == entry.category) else { continue };
            let subtitle = match entry.category {
                KeyCategory::Otp => entry.service.clone(),
                KeyCategory::Api => String::new(),
                KeyCategory::Ssh => "ecdsa-sha2-nistp256".to_string(),
            };
            let row = adw::ActionRow::builder().title(&entry.name).subtitle(&subtitle).build();

            let primary = gtk::Button::builder()
                .label(match entry.category {
                    KeyCategory::Otp => "取验证码",
                    KeyCategory::Api => "显示",
                    KeyCategory::Ssh => "复制公钥",
                })
                .valign(gtk::Align::Center)
                .build();
            let delete = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .valign(gtk::Align::Center)
                .tooltip_text("删除（需触摸设备）")
                .build();
            delete.add_css_class("flat");
            row.add_suffix(&primary);
            row.add_suffix(&delete);
            row.set_activatable_widget(Some(&primary));

            let this = Rc::downgrade(self);
            let e = entry.clone();
            primary.connect_clicked(move |b| {
                let Some(this) = this.upgrade() else { return };
                this.primary_action(b.clone(), e.clone());
            });
            let this = Rc::downgrade(self);
            let e = entry.clone();
            delete.connect_clicked(move |b| {
                let Some(this) = this.upgrade() else { return };
                this.delete_action(b.clone(), e.clone());
            });

            group.add(&row);
            self.rows.borrow_mut().push(row);
        }
    }

    fn primary_action(self: &Rc<Self>, button: gtk::Button, entry: KeyEntry) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            match entry.category {
                KeyCategory::Ssh => {
                    let line = ssh_public_key_line(&entry);
                    if let Some(display) = gtk::gdk::Display::default() {
                        display.clipboard().set_text(&line);
                    }
                    this.toast("公钥已复制", 3);
                }
                KeyCategory::Otp | KeyCategory::Api => {
                    let original = button.label().map(|l| l.to_string()).unwrap_or_default();
                    button.set_sensitive(false);
                    button.set_label("请触摸设备…");
                    let name = entry.name.clone();
                    let cat = entry.category;
                    let r = run_blocking(move || match cat {
                        KeyCategory::Otp => get_otp(&name),
                        _ => get_api(&name),
                    })
                    .await;
                    button.set_label(&original);
                    button.set_sensitive(true);
                    match r {
                        Some(Ok(value)) => this.toast(&format!("{}：{}", entry.name, value), 30),
                        Some(Err(e)) => this.toast(&format!("读取失败：{e}"), 5),
                        None => {}
                    }
                }
            }
        });
    }

    fn delete_action(self: &Rc<Self>, button: gtk::Button, entry: KeyEntry) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            let Some(win) = button.root().and_then(|r| r.downcast::<gtk::Window>().ok()) else { return };
            let ok = confirm(
                &win,
                &format!("删除 {} 「{}」？", entry.category.label(), entry.name),
                "此操作需要在设备上触摸确认，且无法撤销。",
                "删除",
            )
            .await;
            if !ok {
                return;
            }
            button.set_sensitive(false);
            this.toast("请触摸设备确认删除…", 30);
            let (cat, idx) = (entry.category, entry.index);
            let r = run_blocking(move || delete_key(cat, idx)).await;
            button.set_sensitive(true);
            match r {
                Some(Ok(())) => {
                    this.toast("已删除", 3);
                    // The daemon re-syncs its cache after a successful delete;
                    // give it a beat before re-reading.
                    glib::timeout_future(Duration::from_millis(500)).await;
                    this.reload();
                }
                Some(Err(e)) => this.toast(&format!("删除失败：{e}"), 5),
                None => {}
            }
        });
    }
}
```

- [ ] **Step 2: 挂到主窗口**

`main_window.rs` 在 dashboard 之后加：

```rust
        let keys = pages::keys::KeysPage::new(&toasts);
        stack
            .add_titled(keys.widget(), Some("keys"), "密钥")
            .set_icon_name(Some("dialog-password-symbolic"));
```

并在 `unsafe { window.set_data("dashboard-page", dashboard) };` 后加 `unsafe { window.set_data("keys-page", keys) };`。

- [ ] **Step 3: 编译并手工验收**

Run: `cargo build -p immurok-gui && target/debug/immurok-gui`
Expected:
- 「密钥」页三组与 `immurok-cli key list otp/api/ssh` 一致。
- OTP 行「取验证码」→ 按钮变「请触摸设备…」→ 触摸 → toast 显示 6 位码，30 s 后消失。不触摸 → 30 s 后 toast「读取失败：DENY:GATE_TIMEOUT」或 daemon 的对应错误码。
- SSH 行「复制公钥」→ 粘贴出 `ecdsa-sha2-nistp256 … name`。
- 删除一条测试用 API key：确认框 → 触摸 → 列表刷新后该行消失。

- [ ] **Step 4: Commit**

```bash
git add crates/immurok-gui
git commit -m "gui: Keys 页，OTP/API 门控读取、SSH 公钥复制、删除"
```

---

### Task 8: Quick-fill 面板 + 剪贴板输出

**Files:**
- Modify: `crates/immurok-gui/src/filter.rs`
- Modify: `crates/immurok-gui/src/output.rs`
- Modify: `crates/immurok-gui/src/quickfill.rs`

**Interfaces:**
- Consumes: `immurok_client::keys::{list_keys, get_otp, get_api, ssh_public_key_line, cancel_gate, KeyEntry, KeyCategory}`
- Produces:
  - `filter::filter_entries(entries: &[KeyEntry], query: &str) -> Vec<KeyEntry>`
  - `output::Output { Clipboard }`、`impl Output { pub async fn inject(&self, app: &adw::Application, text: &str) -> Result<(), String>; pub fn needs_focus_return(&self) -> bool }`
  - `output::CLIPBOARD_CLEAR_AFTER: Duration = 30 s`
  - `quickfill::open(app: &adw::Application)`

- [ ] **Step 1: `filter.rs` 测试 + 实现**

```rust
//! Substring filter for the quick-fill list. Pure so it can be tested.

use immurok_client::keys::KeyEntry;

/// Case-insensitive substring match on name and service. Empty query keeps
/// everything. Order of `entries` is preserved.
pub fn filter_entries(entries: &[KeyEntry], query: &str) -> Vec<KeyEntry> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return entries.to_vec();
    }
    entries
        .iter()
        .filter(|e| e.name.to_lowercase().contains(&q) || e.service.to_lowercase().contains(&q))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use immurok_client::keys::KeyCategory;

    fn e(name: &str, service: &str) -> KeyEntry {
        KeyEntry {
            index: 0,
            category: KeyCategory::Otp,
            name: name.into(),
            service: service.into(),
            ssh_pubkey_b64: String::new(),
        }
    }

    #[test]
    fn empty_query_keeps_all() {
        let all = vec![e("aws", "Amazon"), e("gh", "GitHub")];
        assert_eq!(filter_entries(&all, "  ").len(), 2);
    }

    #[test]
    fn matches_name_or_service_case_insensitively() {
        let all = vec![e("aws", "Amazon"), e("gh", "GitHub")];
        assert_eq!(filter_entries(&all, "HUB").len(), 1);
        assert_eq!(filter_entries(&all, "AWS")[0].name, "aws");
        assert!(filter_entries(&all, "zzz").is_empty());
    }
}
```

- [ ] **Step 2: `output.rs`**

```rust
//! Where a fetched value goes. Phase 1 ships the clipboard only; phase 2 adds
//! the typing backends (portal / xdotool / wtype) as more variants.

use std::time::Duration;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

pub const CLIPBOARD_CLEAR_AFTER: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    Clipboard,
}

impl Output {
    /// Typing backends must run after the panel is gone and focus is back on
    /// the target window. The clipboard is the opposite: GNOME only lets a
    /// focused window write it, so it must run while the panel is still up.
    pub fn needs_focus_return(&self) -> bool {
        match self {
            Output::Clipboard => false,
        }
    }

    pub async fn inject(&self, app: &adw::Application, text: &str) -> Result<(), String> {
        match self {
            Output::Clipboard => copy_and_schedule_clear(app, text),
        }
    }
}

fn copy_and_schedule_clear(app: &adw::Application, text: &str) -> Result<(), String> {
    let display = gtk::gdk::Display::default().ok_or("no display")?;
    let clipboard = display.clipboard();
    clipboard.set_text(text);

    let note = gio::Notification::new("immurok");
    note.set_body(Some(&format!(
        "已复制到剪贴板，{} 秒后自动清除",
        CLIPBOARD_CLEAR_AFTER.as_secs()
    )));
    app.send_notification(Some("quick-fill"), &note);

    // Only clear if the clipboard still holds what we put there; the user
    // may have copied something else in the meantime.
    let ours = text.to_string();
    let app = app.clone();
    glib::spawn_future_local(async move {
        glib::timeout_future(CLIPBOARD_CLEAR_AFTER).await;
        if let Ok(Some(current)) = clipboard.read_text_future().await {
            if current.as_str() == ours {
                clipboard.set_text("");
            }
        }
        app.withdraw_notification("quick-fill");
    });
    Ok(())
}
```

- [ ] **Step 3: `quickfill.rs`**

```rust
//! Quick-fill panel: hotkey → pick an entry → touch → value delivered.
//!
//! State machine (spec §5):
//!   Listing ──Enter(OTP/API)──► Waiting(30 s) ──OK──► Delivering ──► closed
//!      │  └─Enter(SSH)──► Delivering (no touch)          │ error / Esc → red hint, back to Listing
//!      └─ Esc / focus lost ──► closed
//!
//! The window is undecorated, created fresh on every open and destroyed on
//! close so the compositor hands focus back to the previous window — under
//! Wayland we cannot do that ourselves.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::gdk;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::keys::{
    cancel_gate, get_api, get_otp, list_keys, ssh_public_key_line, KeyCategory, KeyEntry,
    GATE_TIMEOUT,
};

use crate::filter::filter_entries;
use crate::output::Output;

const PANEL_NAME: &str = "quick-fill";
/// Delay between the panel closing and a typing backend firing — long
/// enough for the compositor to restore focus to the previous window.
const FOCUS_RETURN_DELAY: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Listing,
    Waiting,
    Delivering,
}

struct Panel {
    window: gtk::Window,
    app: adw::Application,
    search: gtk::SearchEntry,
    list: gtk::ListBox,
    progress: gtk::ProgressBar,
    hint: gtk::Label,
    all: RefCell<Vec<KeyEntry>>,
    shown: RefCell<Vec<KeyEntry>>,
    phase: Cell<Phase>,
    output: Output,
}

pub fn open(app: &adw::Application) {
    // A second hotkey press while the panel is up just re-focuses it.
    if let Some(w) = app.windows().into_iter().find(|w| w.widget_name() == PANEL_NAME) {
        w.present();
        return;
    }
    let panel = Panel::build(app, Output::Clipboard);
    panel.window.present();
    panel.load();
}

impl Panel {
    fn build(app: &adw::Application, output: Output) -> Rc<Self> {
        let window = gtk::Window::builder()
            .application(app)
            .decorated(false)
            .resizable(false)
            .default_width(480)
            .title("immurok quick-fill")
            .build();
        window.set_widget_name(PANEL_NAME);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.set_margin_top(8);
        root.set_margin_bottom(8);
        root.set_margin_start(8);
        root.set_margin_end(8);

        let search = gtk::SearchEntry::builder().placeholder_text("搜索 OTP / API / SSH…").build();
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        list.add_css_class("boxed-list");
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .propagate_natural_height(true)
            .max_content_height(320)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let progress = gtk::ProgressBar::builder().visible(false).build();
        let hint = gtk::Label::builder().visible(false).xalign(0.0).build();
        hint.add_css_class("error");

        root.append(&search);
        root.append(&scroller);
        root.append(&progress);
        root.append(&hint);
        window.set_child(Some(&root));

        let panel = Rc::new(Self {
            window: window.clone(),
            app: app.clone(),
            search,
            list,
            progress,
            hint,
            all: RefCell::new(Vec::new()),
            shown: RefCell::new(Vec::new()),
            phase: Cell::new(Phase::Listing),
            output,
        });
        panel.wire();
        panel
    }

    fn wire(self: &Rc<Self>) {
        // Search filters live.
        let weak = Rc::downgrade(self);
        self.search.connect_search_changed(move |_| {
            if let Some(p) = weak.upgrade() {
                p.refill();
            }
        });

        // Enter in the search box == activate the selected row.
        let weak = Rc::downgrade(self);
        self.search.connect_activate(move |_| {
            if let Some(p) = weak.upgrade() {
                p.activate_selected();
            }
        });

        let weak = Rc::downgrade(self);
        self.list.connect_row_activated(move |_, _| {
            if let Some(p) = weak.upgrade() {
                p.activate_selected();
            }
        });

        // Esc closes; Up/Down move the selection even while the entry has focus.
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(p) = weak.upgrade() else { return glib::Propagation::Proceed };
            match key {
                gdk::Key::Escape => {
                    p.close();
                    glib::Propagation::Stop
                }
                gdk::Key::Up => {
                    p.move_selection(-1);
                    glib::Propagation::Stop
                }
                gdk::Key::Down => {
                    p.move_selection(1);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        self.window.add_controller(keys);

        // Clicking elsewhere dismisses the panel — except while we are waiting
        // for a touch, when the user may well be looking at the device.
        let weak = Rc::downgrade(self);
        self.window.connect_is_active_notify(move |w| {
            let Some(p) = weak.upgrade() else { return };
            if !w.is_active() && p.phase.get() == Phase::Listing {
                p.close();
            }
        });

        // Closing mid-wait aborts the device gate.
        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |_| {
            if let Some(p) = weak.upgrade() {
                if p.phase.get() == Phase::Waiting {
                    std::thread::spawn(cancel_gate);
                }
            }
            glib::Propagation::Proceed
        });

        // Keep the Rc alive exactly as long as the window.
        unsafe { self.window.set_data("panel", self.clone()) };
    }

    fn load(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let entries = gtk::gio::spawn_blocking(list_keys).await.unwrap_or_default();
            if let Some(p) = weak.upgrade() {
                *p.all.borrow_mut() = entries;
                p.refill();
            }
        });
    }

    fn refill(&self) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let query = self.search.text().to_string();
        let shown = filter_entries(&self.all.borrow(), &query);
        for e in &shown {
            let subtitle = match e.category {
                KeyCategory::Otp => if e.service.is_empty() { "OTP".to_string() } else { e.service.clone() },
                KeyCategory::Api => "API".to_string(),
                KeyCategory::Ssh => "SSH 公钥".to_string(),
            };
            let row = adw::ActionRow::builder().title(&e.name).subtitle(&subtitle).activatable(true).build();
            self.list.append(&row);
        }
        *self.shown.borrow_mut() = shown;
        if let Some(first) = self.list.row_at_index(0) {
            self.list.select_row(Some(&first));
        }
    }

    fn move_selection(&self, delta: i32) {
        let n = self.shown.borrow().len() as i32;
        if n == 0 {
            return;
        }
        let cur = self.list.selected_row().map(|r| r.index()).unwrap_or(0);
        let next = (cur + delta).clamp(0, n - 1);
        if let Some(row) = self.list.row_at_index(next) {
            self.list.select_row(Some(&row));
        }
    }

    fn activate_selected(self: &Rc<Self>) {
        if self.phase.get() != Phase::Listing {
            return;
        }
        let idx = match self.list.selected_row() {
            Some(r) => r.index() as usize,
            None => return,
        };
        let entry = match self.shown.borrow().get(idx) {
            Some(e) => e.clone(),
            None => return,
        };
        match entry.category {
            KeyCategory::Ssh => self.deliver(ssh_public_key_line(&entry)),
            KeyCategory::Otp | KeyCategory::Api => self.wait_for_touch(entry),
        }
    }

    fn wait_for_touch(self: &Rc<Self>, entry: KeyEntry) {
        self.phase.set(Phase::Waiting);
        self.search.set_sensitive(false);
        self.list.set_sensitive(false);
        self.hint.set_visible(false);
        self.progress.set_visible(true);
        self.progress.set_fraction(1.0);
        self.progress.set_text(Some(&format!("请触摸设备读取「{}」…", entry.name)));
        self.progress.set_show_text(true);

        // Countdown bar: 1.0 → 0.0 over the device's 30 s gate.
        let started = Instant::now();
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(100), move || {
            let Some(p) = weak.upgrade() else { return glib::ControlFlow::Break };
            if p.phase.get() != Phase::Waiting {
                return glib::ControlFlow::Break;
            }
            let left = 1.0 - started.elapsed().as_secs_f64() / 30.0;
            p.progress.set_fraction(left.max(0.0));
            glib::ControlFlow::Continue
        });

        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let name = entry.name.clone();
            let cat = entry.category;
            let result = gtk::gio::spawn_blocking(move || match cat {
                KeyCategory::Otp => get_otp(&name),
                _ => get_api(&name),
            })
            .await
            .unwrap_or_else(|_| Err("worker panicked".into()));
            let Some(p) = weak.upgrade() else { return };
            match result {
                Ok(value) => p.deliver(value),
                Err(e) => p.back_to_listing_with_error(&friendly_error(&e)),
            }
        });
    }

    fn back_to_listing_with_error(self: &Rc<Self>, msg: &str) {
        self.phase.set(Phase::Listing);
        self.progress.set_visible(false);
        self.search.set_sensitive(true);
        self.list.set_sensitive(true);
        self.hint.set_text(msg);
        self.hint.set_visible(true);
        self.search.grab_focus();
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            if let Some(p) = weak.upgrade() {
                p.hint.set_visible(false);
            }
        });
    }

    fn deliver(self: &Rc<Self>, value: String) {
        self.phase.set(Phase::Delivering);
        let app = self.app.clone();
        let output = self.output;
        let window = self.window.clone();
        glib::spawn_future_local(async move {
            let result = if output.needs_focus_return() {
                window.close();
                glib::timeout_future(FOCUS_RETURN_DELAY).await;
                output.inject(&app, &value).await
            } else {
                let r = output.inject(&app, &value).await;
                window.close();
                r
            };
            if let Err(e) = result {
                // Last resort so the user is never left with nothing.
                let _ = Output::Clipboard.inject(&app, &value).await;
                eprintln!("immurok-gui: {output:?} failed ({e}); fell back to clipboard");
            }
        });
    }

    fn close(&self) {
        self.window.close();
    }
}

fn friendly_error(raw: &str) -> String {
    match raw {
        r if r.contains("GATE_TIMEOUT") || r.contains("Read failed") => "超时，未检测到触摸".into(),
        r if r.contains("GATE_REJECTED") || r.contains("DENY") => "设备拒绝".into(),
        r if r.contains("BUSY") => "设备正忙，稍后再试".into(),
        r if r.contains("NOT_CONNECTED") => "设备未连接".into(),
        r => r.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_errors() {
        assert_eq!(friendly_error("DENY:GATE_TIMEOUT"), "超时，未检测到触摸");
        assert_eq!(friendly_error("ERROR:BUSY"), "设备正忙，稍后再试");
        assert_eq!(friendly_error("ERROR:NOT_CONNECTED"), "设备未连接");
        assert_eq!(friendly_error("ERROR:OTP_FAILED:0x21"), "ERROR:OTP_FAILED:0x21");
    }

    #[test]
    fn gate_timeout_constant_covers_device_gate() {
        assert!(GATE_TIMEOUT >= Duration::from_secs(30));
    }
}
```

- [ ] **Step 4: 编译 + 单测**

Run: `cargo build -p immurok-gui && cargo test -p immurok-gui`
Expected: filter 2 个、quickfill 2 个、cli 4 个测试通过

- [ ] **Step 5: 手工验收（spec §11 第 5-7 条）**

终端 A：`target/debug/immurok-gui --gapplication-service`
终端 B：`target/debug/immurok-gui --quick-fill`
Expected：
- 面板出现、搜索框有焦点、第一行已选中。输入两个字母，列表过滤。上下键移动选择。
- Enter 选一条 OTP → 进度条从满开始下降、文字「请触摸设备读取「…」…」→ 触摸 → 面板消失、桌面通知「已复制到剪贴板，30 秒后自动清除」→ 粘贴出 6 位数字 → 30 s 后粘贴为空。
- 等待期间按 Esc → 面板关闭；`immurok-cli logs | grep GATE` 里出现 GATE:CANCEL。
- 等待期间不触摸 → 约 30 s 后红字「超时，未检测到触摸」1.5 s 后消失，回到列表。
- 列表态点桌面别处 → 面板消失。
- 取码后 5 s 内屏幕没有锁定（验证 daemon 的 0x23 抑制对这条路径生效）。
- Enter 选一条 SSH → 无需触摸，剪贴板里是公钥行。

- [ ] **Step 6: Commit**

```bash
git add crates/immurok-gui
git commit -m "gui: Quick-fill 面板，搜索/选择/触摸等待/剪贴板输出 30s 自清"
```

---

### Task 9: 打包、安装、文档、版本

**Files:**
- Create: `packaging/com.immurok.Settings.desktop`
- Create: `packaging/com.immurok.Settings.service`
- Create: `packaging/com.immurok.Settings.autostart.desktop`
- Modify: `Makefile`
- Modify: `scripts/install-root.sh:31-38`、`scripts/uninstall-root.sh:42-43`
- Modify: `scripts/check-deps.sh`
- Modify: `README.md`、`CHANGELOG.md`
- Modify: 五个 `crates/*/Cargo.toml` 的 `version`

**Interfaces:**
- Consumes: `target/release/immurok-gui`

- [ ] **Step 1: 桌面文件**

`packaging/com.immurok.Settings.desktop`：

```ini
[Desktop Entry]
Type=Application
Name=immurok
Comment=Fingerprint key settings and quick-fill
Exec=immurok-gui
Icon=dialog-password
Terminal=false
Categories=Settings;Security;
DBusActivatable=true
StartupNotify=true
```

`packaging/com.immurok.Settings.service`：

```ini
[D-BUS Service]
Name=com.immurok.Settings
Exec=/usr/local/bin/immurok-gui --gapplication-service
```

`packaging/com.immurok.Settings.autostart.desktop`：

```ini
[Desktop Entry]
Type=Application
Name=immurok quick-fill
Comment=Keeps the immurok quick-fill panel one hotkey away
Exec=immurok-gui --gapplication-service
Terminal=false
NoDisplay=true
X-GNOME-Autostart-enabled=true
```

- [ ] **Step 2: Makefile**

在 `CARGO :=` 定义之后加：

```make
# GUI 需要 GTK4 + libadwaita 的开发头。缺就跳过 immurok-gui，其他 crate 照常构建，
# daemon / CLI 用户不被拖着装一套图形依赖。
HAS_GTK_DEV := $(shell pkg-config --exists gtk4 libadwaita-1 2>/dev/null && echo 1)
CARGO_EXCLUDE := $(if $(HAS_GTK_DEV),,--exclude immurok-gui)
```

`build:` 目标改为：

```make
build: check-deps
	$(CARGO) build --release --workspace $(CARGO_EXCLUDE)
	@[ -n "$(HAS_GTK_DEV)" ] || echo "⚠️  未找到 gtk4/libadwaita 开发头，已跳过 immurok-gui（装 libgtk-4-dev libadwaita-1-dev 后重跑 make）"
```

`install:` 的 `systemctl --user enable --now immurok-session-agent.service` 之后加：

```make
	@# ── 用户级：GUI 自启动（常驻，热键秒开）。二进制不存在时跳过 ──
	@if [ -x $(BIN_DIR)/immurok-gui ]; then \
		mkdir -p $(HOME)/.config/autostart; \
		install -m644 packaging/com.immurok.Settings.autostart.desktop $(HOME)/.config/autostart/com.immurok.Settings.desktop; \
		echo "✓ GUI 自启动已登记（$(HOME)/.config/autostart/com.immurok.Settings.desktop）"; \
	fi
```

`uninstall:` 的用户级段加：

```make
	-rm -f $(HOME)/.config/autostart/com.immurok.Settings.desktop
```

- [ ] **Step 3: install-root.sh / uninstall-root.sh**

`install-root.sh` 在 `install -Dm755 target/release/immurok-session-agent …` 之后加：

```bash
# GUI 是可选构建产物（缺 GTK 开发头时 Makefile 会跳过）。
if [ -x target/release/immurok-gui ]; then
    install -Dm755 target/release/immurok-gui "$BIN_DIR/immurok-gui"
    install -Dm644 packaging/com.immurok.Settings.desktop /usr/local/share/applications/com.immurok.Settings.desktop
    sed "s|/usr/local/bin/immurok-gui|$BIN_DIR/immurok-gui|" packaging/com.immurok.Settings.service \
        > /usr/local/share/dbus-1/services/com.immurok.Settings.service
    chmod 644 /usr/local/share/dbus-1/services/com.immurok.Settings.service
    update-desktop-database /usr/local/share/applications 2>/dev/null || true
fi
```

前面要先 `mkdir -p /usr/local/share/applications /usr/local/share/dbus-1/services`。

`uninstall-root.sh` 第 42 行附近加：

```bash
rm -f "$BIN_DIR/immurok-gui"
rm -f /usr/local/share/applications/com.immurok.Settings.desktop
rm -f /usr/local/share/dbus-1/services/com.immurok.Settings.service
```

- [ ] **Step 4: check-deps.sh**

在包名映射表（`gtk:dnf)` 那一组）旁加：

```bash
    gtkdev:dnf)      echo "gtk4-devel libadwaita-devel";;
    gtkdev:apt)      echo "libgtk-4-dev libadwaita-1-dev";;
    gtkdev:pacman)   echo "gtk4 libadwaita";;
```

在 PyGObject 检查之后加：

```bash
# GTK4 + libadwaita 开发头 — immurok-gui（可选；缺则 Makefile 跳过 GUI）
if pkg-config --exists gtk4 libadwaita-1 2>/dev/null; then
  ok "gtk4 + libadwaita dev headers  (immurok-gui)"
else
  warn "gtk4/libadwaita dev headers (GUI 将被跳过)" gtkdev; WARN=1
fi
```

- [ ] **Step 5: 版本 + CHANGELOG + README**

五个 `crates/*/Cargo.toml`（common / daemon / cli / session-agent / client / gui）`version = "0.7.0"`。

`CHANGELOG.md` 顶部加：

```markdown
## 0.7.0 — 2026-09-XX

### Added

- **`immurok-gui` — a GTK4 / libadwaita settings app.** Same daemon socket as
  the TUI, same one-request-per-connection protocol. Phase 1 ships the Device
  page (status, feature toggles, pair / unpair) and the Keys page (OTP / API
  reads through the device's fingerprint gate, SSH public key copy, delete).
- **Quick-fill panel.** `immurok-gui --quick-fill` (bind it to a key in your
  desktop's shortcut settings) pops a search list of your keys; pick one,
  touch the device, and the value lands in the clipboard with a notification
  and is cleared 30 s later. Typing straight into the focused field and
  portal-registered hotkeys follow in 0.8.
- `immurok-client` crate: the socket client shared by CLI, TUI and GUI.

### Changed

- `make` skips `immurok-gui` when GTK4 / libadwaita development headers are
  missing, so daemon-only installs need no new dependencies.
```

`README.md`：
- 第 1 节三家发行版的装包命令各加开发头（`libgtk-4-dev libadwaita-1-dev` / `gtk4-devel libadwaita-devel` / 已含 `gtk4 libadwaita`），注明「可选，装了才编 GUI」。
- 第 4 节 TUI 表格之前加 4.0 小节：

```markdown
### 4.0 The GUI (optional)

`immurok-gui` opens the graphical settings window (also in your app menu as
"immurok"). It stays resident after login so the quick-fill panel is one key
away: bind `immurok-gui --quick-fill` to a shortcut in your desktop's keyboard
settings, press it in any text field, pick an OTP, touch the device. Phase 1
copies the code to the clipboard (cleared after 30 s); direct typing arrives in
0.8.
```

- [ ] **Step 6: 全量构建、安装、验收**

Run: `make && make install`
Expected:
- 产物含 `target/release/immurok-gui`；`/usr/local/bin/immurok-gui` 存在；`~/.config/autostart/com.immurok.Settings.desktop` 存在。
- 注销再登录：`pgrep -a immurok-gui` 显示 `--gapplication-service` 实例。
- 应用菜单出现「immurok」，点击开主窗口，`pgrep` 仍只有一个进程。
- `gdbus call --session --dest com.immurok.Settings --object-path /com/immurok/Settings --method org.freedesktop.Application.ActivateAction quick-fill [] {}` 弹出面板（这是 D-Bus 激活路径，阶段二的 portal 也走它）。
- `make uninstall` 后以上文件全部消失。

- [ ] **Step 7: Commit**

```bash
git add Makefile scripts packaging README.md CHANGELOG.md crates/*/Cargo.toml Cargo.lock
git commit -m "gui: 打包安装（desktop / D-Bus service / 自启动），GTK 缺失时跳过；版本 0.7.0"
```

---

## Self-Review

**Spec coverage**
- §3.1 client crate：Task 1-4 ✓
- §3.2 文件结构：Task 5-8 覆盖 main / cli / main_window / pages / quickfill / output / filter；`poll.rs` 合并进了 `dashboard.rs`（轮询只有 Dashboard 用，单独文件是过度拆分），`settings.rs` / `session.rs` / `hotkey.rs` / `settings_store.rs` 属阶段二 ✓
- §3.3 版本下限：Global Constraints + Cargo.toml 无 feature ✓
- §4 启动与单实例：Task 5（HANDLES_COMMAND_LINE、hold）、Task 9（desktop / service / autostart）✓
- §5 面板状态机：Task 8，含失焦关闭、Waiting 不关闭、GATE:CANCEL、剪贴板先写后关、打字后端先关后等 150 ms ✓
- §6 输出后端：本阶段仅 Clipboard，含 30 s 比对清空 ✓；其余阶段二
- §9 安装依赖：Task 9 ✓
- §10 安全：秘密只进 toast / 剪贴板，不落日志 ✓；0x23 抑制在 Task 8 验收 ✓
- §11 验收 1-7：Task 2/6/7/8/9 的验收步骤逐条对应 ✓

**Placeholder scan**：无 TBD / TODO；Task 5 的 quickfill 占位明确写了「Task 8 填实」并给了完整占位代码。

**Type consistency**
- `KeyEntry` 字段 `index/category/name/service/ssh_pubkey_b64` 在 Task 4、7、8 一致 ✓
- `run_blocking` 返回 `Option<T>`，Task 6/7 的 `match` 都处理了 `None` ✓
- `Output::inject(&self, app, text) -> Result<(), String>` 与 Task 8 调用一致 ✓
- `SettingKey::wire()` 与 daemon `SET:<KEY>` 一致（socket_proto.rs 表）✓
- `GATE_TIMEOUT` 在 keys.rs 定义、quickfill.rs 引用 ✓
