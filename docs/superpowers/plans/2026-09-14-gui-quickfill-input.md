# Linux GUI + Quick-fill 阶段二实施计划（input：热键与键盘注入）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 Quick-fill 面板能被全局热键呼出，并把取到的值直接打进当前焦点的输入框；剪贴板退为兜底。

**Architecture:** 在阶段一的 `immurok-gui` 上加四个模块：`session`（X11/Wayland 判定）、`settings_store`（`~/.config/immurok/gui.json`）、`output` 扩展三个打字后端（portal keysym / xdotool / wtype）+ `select_output` 选择逻辑、`hotkey`（GlobalShortcuts portal 与 X11 XGrabKey，都收口到 `quick-fill` action）。再加一个设置页展示当前生效的后端并允许强制指定。所有 portal 调用用 `ashpd`，在 `glib::spawn_future_local` 里 await。

**Tech Stack:** ashpd 0.11（默认 async-std 后端）、x11rb 0.13、async-channel 2、serde / serde_json、tempfile（dev）。

**Spec:** `docs/superpowers/specs/2026-09-14-gui-quickfill-design.md` §6、§7、§8

## Global Constraints

- 前置：阶段一计划（`2026-09-14-gui-quickfill-core.md`）全部完成并合入。
- 平台下限不变：gtk4 0.9 / libadwaita 0.7 无 feature；设置页只用 `Adw.ComboRow`（1.0）、`Adw.ActionRow`，不用 `Adw.EntryRow`（1.2）。
- 不做 uinput。
- keysym 映射：`0x20..=0x7e` 与 `0xa0..=0xff` 直接等于码点，其余 `0x0100_0000 | 码点`。
- 打字后端一律在面板关闭后等 150 ms 再注入；剪贴板在面板关闭前写。
- 注入失败必须自动降级剪贴板并通知，用户不能空手而归。
- 外部工具（xdotool / wtype）文本走 stdin，不进 argv。
- restore token 不是秘密，但 gui.json 仍 0600。
- ashpd 方法名以 docs.rs 上 0.11 版为准；本计划中的调用若与文档不一致，按文档改，语义不变。
- 手工验收要在 GNOME Wayland 48+ 与 KDE Plasma 6 Wayland 各跑一遍，X11 会话跑一遍（任一 DE 的 X11 session 即可）。
- 版本 0.9.0 → 0.10.0（Task 9；阶段三占用 0.8.0、阶段四占用 0.9.0）。

---

## 文件结构

新建：
- `crates/immurok-gui/src/session.rs` — `SessionKind`、`detect`、`detect_from`
- `crates/immurok-gui/src/settings_store.rs` — `GuiSettings`、`OutputChoice`、`load`、`save`、`path`
- `crates/immurok-gui/src/state.rs` — 进程级共享状态（settings、session、probes、hotkey 状态）
- `crates/immurok-gui/src/typer.rs` — 外部工具打字 `type_via_tool`、`tool_available`
- `crates/immurok-gui/src/portal.rs` — RemoteDesktop portal：`keysym_for`、`probe_remote_desktop`、`type_text`
- `crates/immurok-gui/src/hotkey.rs` — `HotkeyStatus`、`start`、X11 `parse_binding`
- `crates/immurok-gui/src/pages/settings.rs` — 设置页

修改：
- `crates/immurok-gui/Cargo.toml` — 新依赖
- `crates/immurok-gui/src/output.rs` — 三个新变体、`select_output`、`Probes`
- `crates/immurok-gui/src/quickfill.rs` — `open` 用 `state` 里选出的 `Output`
- `crates/immurok-gui/src/main.rs` — startup 时加载设置、探测、启动热键
- `crates/immurok-gui/src/main_window.rs` — 挂设置页
- `crates/immurok-gui/src/pages/mod.rs`
- `scripts/check-deps.sh`、`README.md`、`CHANGELOG.md`、`crates/*/Cargo.toml`

---

### Task 1: 依赖与 `session.rs`

**Files:**
- Modify: `crates/immurok-gui/Cargo.toml`
- Create: `crates/immurok-gui/src/session.rs`
- Modify: `crates/immurok-gui/src/main.rs`（加 `mod session;`）

**Interfaces:**
- Produces: `session::SessionKind { X11, Wayland, Unknown }`、`session::detect() -> SessionKind`、`session::detect_from(get: impl Fn(&str) -> Option<String>) -> SessionKind`

- [ ] **Step 1: 依赖**

`crates/immurok-gui/Cargo.toml` `[dependencies]` 追加：

```toml
ashpd = "0.11"
x11rb = "0.13"
async-channel = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: 测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + '_ {
        move |k| pairs.iter().find(|(kk, _)| *kk == k).map(|(_, v)| v.to_string())
    }

    #[test]
    fn wayland_by_session_type() {
        assert_eq!(detect_from(env(&[("XDG_SESSION_TYPE", "wayland")])), SessionKind::Wayland);
    }

    #[test]
    fn wayland_by_display_var_even_if_session_type_missing() {
        assert_eq!(detect_from(env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")])), SessionKind::Wayland);
    }

    #[test]
    fn x11_when_only_display() {
        assert_eq!(detect_from(env(&[("XDG_SESSION_TYPE", "x11"), ("DISPLAY", ":0")])), SessionKind::X11);
        assert_eq!(detect_from(env(&[("DISPLAY", ":1")])), SessionKind::X11);
    }

    #[test]
    fn unknown_on_tty() {
        assert_eq!(detect_from(env(&[("XDG_SESSION_TYPE", "tty")])), SessionKind::Unknown);
        assert_eq!(detect_from(env(&[])), SessionKind::Unknown);
    }
}
```

- [ ] **Step 3: 实现**

```rust
//! Which display server we are talking to. Decides both the hotkey and the
//! typing backend (spec §6, §7).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    X11,
    Wayland,
    Unknown,
}

impl SessionKind {
    pub fn label(self) -> &'static str {
        match self {
            SessionKind::X11 => "X11",
            SessionKind::Wayland => "Wayland",
            SessionKind::Unknown => "未知",
        }
    }
}

pub fn detect() -> SessionKind {
    detect_from(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
}

/// `WAYLAND_DISPLAY` wins over `XDG_SESSION_TYPE`: XWayland sessions export
/// `DISPLAY` too, and a stale `XDG_SESSION_TYPE=x11` from a login manager
/// must not route us to XTEST inside a Wayland compositor.
pub fn detect_from(get: impl Fn(&str) -> Option<String>) -> SessionKind {
    if get("WAYLAND_DISPLAY").is_some() {
        return SessionKind::Wayland;
    }
    match get("XDG_SESSION_TYPE").as_deref() {
        Some("wayland") => SessionKind::Wayland,
        Some("x11") => SessionKind::X11,
        _ if get("DISPLAY").is_some() => SessionKind::X11,
        _ => SessionKind::Unknown,
    }
}
```

- [ ] **Step 4: 跑测试**

Run: `cargo test -p immurok-gui session`
Expected: 4 passed

- [ ] **Step 5: Commit**

```bash
git add crates/immurok-gui
git commit -m "gui: 会话类型判定（X11 / Wayland）"
```

---

### Task 2: `settings_store.rs` — gui.json

**备注（2026-09-18）：** `settings_store.rs` 已由阶段三计划（`2026-09-18-gui-fingerprint-hosts.md` Task 6）以完全相同的结构创建并多了 `fingerprint_names` 字段；执行本任务时改为核对并补齐缺失项，不要重建文件。

**Files:**
- Create: `crates/immurok-gui/src/settings_store.rs`
- Modify: `crates/immurok-gui/src/main.rs`（加 `mod settings_store;`）

**Interfaces:**
- Produces:
  - `settings_store::OutputChoice { Auto, Clipboard, Portal, Xdotool, Wtype }`（serde 小写字符串）带 `fn label(self) -> &'static str`、`pub const ALL: [OutputChoice; 5]`
  - `settings_store::GuiSettings { pub output: OutputChoice, pub portal_restore_token: Option<String>, pub x11_hotkey: String }`，`Default` = Auto / None / "ctrl+backslash"
  - `settings_store::path() -> PathBuf`（`$XDG_CONFIG_HOME/immurok/gui.json` 或 `~/.config/immurok/gui.json`）
  - `settings_store::load_from(path: &Path) -> GuiSettings`（缺失 / 损坏 → Default）
  - `settings_store::save_to(path: &Path, s: &GuiSettings) -> Result<(), String>`（临时文件 + rename，0600）
  - `settings_store::load() -> GuiSettings`、`settings_store::save(&GuiSettings) -> Result<(), String>`

- [ ] **Step 1: 测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn defaults_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let s = load_from(&dir.path().join("gui.json"));
        assert_eq!(s.output, OutputChoice::Auto);
        assert_eq!(s.x11_hotkey, "ctrl+backslash");
        assert!(s.portal_restore_token.is_none());
    }

    #[test]
    fn roundtrip_and_mode_0600() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("gui.json");
        let s = GuiSettings {
            output: OutputChoice::Wtype,
            portal_restore_token: Some("tok".into()),
            x11_hotkey: "ctrl+alt+k".into(),
        };
        save_to(&p, &s).unwrap();
        assert_eq!(load_from(&p), s);
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn corrupt_file_falls_back_to_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("gui.json");
        std::fs::write(&p, b"{ not json").unwrap();
        assert_eq!(load_from(&p), GuiSettings::default());
    }

    #[test]
    fn output_choice_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&OutputChoice::Xdotool).unwrap(), "\"xdotool\"");
    }
}
```

- [ ] **Step 2: 实现**

```rust
//! Per-user GUI settings: `~/.config/immurok/gui.json`.
//!
//! Holds the output backend override, the RemoteDesktop portal restore token
//! and the X11 hotkey binding. Nothing secret, but written 0600 anyway.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OutputChoice {
    #[default]
    Auto,
    Clipboard,
    Portal,
    Xdotool,
    Wtype,
}

impl OutputChoice {
    pub const ALL: [OutputChoice; 5] = [
        OutputChoice::Auto,
        OutputChoice::Clipboard,
        OutputChoice::Portal,
        OutputChoice::Xdotool,
        OutputChoice::Wtype,
    ];

    pub fn label(self) -> &'static str {
        match self {
            OutputChoice::Auto => "自动",
            OutputChoice::Clipboard => "剪贴板",
            OutputChoice::Portal => "Portal（RemoteDesktop）",
            OutputChoice::Xdotool => "xdotool（X11）",
            OutputChoice::Wtype => "wtype（wlroots）",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiSettings {
    pub output: OutputChoice,
    pub portal_restore_token: Option<String>,
    pub x11_hotkey: String,
}

impl Default for GuiSettings {
    fn default() -> Self {
        Self {
            output: OutputChoice::Auto,
            portal_restore_token: None,
            x11_hotkey: "ctrl+backslash".into(),
        }
    }
}

pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("immurok").join("gui.json")
}

pub fn load_from(path: &Path) -> GuiSettings {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_to(path: &Path, s: &GuiSettings) -> Result<(), String> {
    let dir = path.parent().ok_or("settings path has no parent")?;
    fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| format!("open {}: {e}", tmp.display()))?;
        f.write_all(&json).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))
}

pub fn load() -> GuiSettings {
    load_from(&path())
}

pub fn save(s: &GuiSettings) -> Result<(), String> {
    save_to(&path(), s)
}
```

- [ ] **Step 3: 跑测试**

Run: `cargo test -p immurok-gui settings_store`
Expected: 4 passed

- [ ] **Step 4: Commit**

```bash
git add crates/immurok-gui
git commit -m "gui: gui.json 设置存储（输出方式 / portal token / X11 热键）"
```

---

### Task 3: `typer.rs` — xdotool / wtype 外部工具打字

**Files:**
- Create: `crates/immurok-gui/src/typer.rs`
- Modify: `crates/immurok-gui/src/main.rs`（加 `mod typer;`）

**Interfaces:**
- Produces:
  - `typer::tool_available(name: &str) -> bool`（PATH 查找）
  - `typer::type_via_tool(program: &str, args: &[&str], text: &str) -> Result<(), String>`（stdin 喂文本）
  - `typer::XDOTOOL_ARGS: &[&str] = &["type", "--clearmodifiers", "--delay", "12", "--file", "-"]`
  - `typer::WTYPE_ARGS: &[&str] = &["-d", "12", "-"]`

- [ ] **Step 1: 测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_tool_is_not_available() {
        assert!(!tool_available("immurok-definitely-not-a-tool"));
        assert!(tool_available("sh"));
    }

    #[test]
    fn stdin_reaches_the_tool() {
        // `cat` echoes stdin; a zero exit means the pipe worked end to end.
        assert!(type_via_tool("cat", &[], "123456").is_ok());
    }

    #[test]
    fn nonzero_exit_is_an_error_with_stderr() {
        let err = type_via_tool("sh", &["-c", "echo boom >&2; exit 3"], "x").unwrap_err();
        assert!(err.contains("boom"), "{err}");
    }

    #[test]
    fn missing_binary_is_an_error() {
        assert!(type_via_tool("immurok-definitely-not-a-tool", &[], "x").is_err());
    }
}
```

- [ ] **Step 2: 实现**

```rust
//! Typing through an external tool: `xdotool type` on X11, `wtype` on
//! wlroots. Both read the text from stdin so it never appears in argv.
//! Both map characters through keysyms, so keyboard layout is not our
//! problem.

use std::io::Write;
use std::process::{Command, Stdio};

pub const XDOTOOL_ARGS: &[&str] = &["type", "--clearmodifiers", "--delay", "12", "--file", "-"];
pub const WTYPE_ARGS: &[&str] = &["-d", "12", "-"];

pub fn tool_available(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| {
        let p = dir.join(name);
        p.is_file()
            && std::fs::metadata(&p)
                .map(|m| {
                    use std::os::unix::fs::PermissionsExt;
                    m.permissions().mode() & 0o111 != 0
                })
                .unwrap_or(false)
    })
}

pub fn type_via_tool(program: &str, args: &[&str], text: &str) -> Result<(), String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn {program}: {e}"))?;
    {
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        stdin.write_all(text.as_bytes()).map_err(|e| format!("write {program}: {e}"))?;
        // Drop closes the pipe so the tool sees EOF.
    }
    let out = child.wait_with_output().map_err(|e| format!("wait {program}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{program} exited {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}
```

- [ ] **Step 3: 跑测试**

Run: `cargo test -p immurok-gui typer`
Expected: 4 passed

- [ ] **Step 4: Commit**

```bash
git add crates/immurok-gui
git commit -m "gui: 外部工具打字（xdotool / wtype），文本走 stdin"
```

---

### Task 4: `portal.rs` — RemoteDesktop portal keysym 打字

**Files:**
- Create: `crates/immurok-gui/src/portal.rs`
- Modify: `crates/immurok-gui/src/main.rs`（加 `mod portal;`）

**Interfaces:**
- Consumes: `settings_store::{load, save, GuiSettings}`
- Produces:
  - `portal::keysym_for(c: char) -> u32`
  - `portal::probe_remote_desktop() -> impl Future<Output = bool>`
  - `portal::type_text(text: &str) -> impl Future<Output = Result<(), String>>`（内部读写 restore token）

- [ ] **Step 1: 测试（纯函数部分）**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_printable_maps_to_itself() {
        assert_eq!(keysym_for('0'), 0x30);
        assert_eq!(keysym_for('9'), 0x39);
        assert_eq!(keysym_for('a'), 0x61);
        assert_eq!(keysym_for(' '), 0x20);
        assert_eq!(keysym_for('~'), 0x7e);
    }

    #[test]
    fn latin1_high_maps_to_itself() {
        assert_eq!(keysym_for('é'), 0xe9);
    }

    #[test]
    fn everything_else_is_unicode_keysym() {
        assert_eq!(keysym_for('€'), 0x0100_0000 | 0x20ac);
        assert_eq!(keysym_for('中'), 0x0100_0000 | 0x4e2d);
        // Controls are not printable: send them as unicode keysyms too rather
        // than as raw keysym 0x0a, which is not a valid X keysym.
        assert_eq!(keysym_for('\n'), 0x0100_0000 | 0x0a);
    }
}
```

- [ ] **Step 2: 实现**

```rust
//! XDG RemoteDesktop portal: the sanctioned way for a Wayland client to type
//! into another window. Keysyms, not keycodes — layout independent.
//!
//! First use pops the portal's permission dialog; we ask for
//! `PersistMode::ExplicitlyRevoked` and store the restore token in gui.json so
//! later runs are silent until the user revokes access in their desktop's
//! settings (GNOME: Settings → Apps → immurok).
//!
//! ashpd method names below follow 0.11. If docs.rs shows a different
//! signature (e.g. `start` taking `&WindowIdentifier`), adapt the call; the
//! flow is create_session → select_devices(keyboard, token, persist) → start
//! → notify_keyboard_keysym × N → session.close().

use ashpd::desktop::remote_desktop::{DeviceType, KeyState, RemoteDesktop};
use ashpd::desktop::PersistMode;

use crate::settings_store;

/// X11 keysym for a character (spec §6).
pub fn keysym_for(c: char) -> u32 {
    let cp = c as u32;
    if (0x20..=0x7e).contains(&cp) || (0xa0..=0xff).contains(&cp) {
        cp
    } else {
        0x0100_0000 | cp
    }
}

/// True when the portal backend implements RemoteDesktop at all. Reading a
/// property fails fast with "unknown interface" on backends that lack it
/// (xdg-desktop-portal-wlr, older COSMIC).
pub async fn probe_remote_desktop() -> bool {
    match RemoteDesktop::new().await {
        Ok(proxy) => proxy.available_device_types().await.is_ok(),
        Err(_) => false,
    }
}

pub async fn type_text(text: &str) -> Result<(), String> {
    let proxy = RemoteDesktop::new().await.map_err(|e| format!("portal: {e}"))?;
    let session = proxy.create_session().await.map_err(|e| format!("create_session: {e}"))?;

    let mut settings = settings_store::load();
    proxy
        .select_devices(
            &session,
            DeviceType::Keyboard.into(),
            settings.portal_restore_token.as_deref(),
            PersistMode::ExplicitlyRevoked,
        )
        .await
        .map_err(|e| format!("select_devices: {e}"))?;

    let started = proxy
        .start(&session, None)
        .await
        .map_err(|e| format!("start: {e}"))?
        .response()
        .map_err(|e| format!("start response: {e}"))?;

    // A fresh token arrives on every start; persist it so the next run is
    // silent. A stale token makes the portal ask again rather than fail.
    let new_token = started.restore_token().map(|t| t.to_string());
    if new_token != settings.portal_restore_token {
        settings.portal_restore_token = new_token;
        if let Err(e) = settings_store::save(&settings) {
            eprintln!("immurok-gui: could not persist portal token: {e}");
        }
    }

    for c in text.chars() {
        let ks = keysym_for(c) as i32;
        proxy
            .notify_keyboard_keysym(&session, ks, KeyState::Pressed)
            .await
            .map_err(|e| format!("keysym press: {e}"))?;
        proxy
            .notify_keyboard_keysym(&session, ks, KeyState::Released)
            .await
            .map_err(|e| format!("keysym release: {e}"))?;
    }

    let _ = session.close().await;
    Ok(())
}
```

- [ ] **Step 3: 编译 + 单测**

Run: `cargo build -p immurok-gui && cargo test -p immurok-gui portal`
Expected: 编译通过（若 ashpd 签名有差异，按 docs.rs 调整后再过）；3 passed

- [ ] **Step 4: Commit**

```bash
git add crates/immurok-gui
git commit -m "gui: RemoteDesktop portal keysym 打字，restore token 持久化"
```

---

### Task 5: `output.rs` 扩展 + `select_output` + `state.rs`

**Files:**
- Modify: `crates/immurok-gui/src/output.rs`
- Create: `crates/immurok-gui/src/state.rs`
- Modify: `crates/immurok-gui/src/quickfill.rs`（`open` 用 `state::current_output()`）
- Modify: `crates/immurok-gui/src/main.rs`（`mod state;`，startup 初始化）

**Interfaces:**
- Consumes: `session::SessionKind`、`settings_store::{GuiSettings, OutputChoice}`、`typer::*`、`portal::*`
- Produces:
  - `output::Output { Clipboard, Portal, Xdotool, Wtype }` 带 `fn label(self) -> &'static str`
  - `output::Probes { pub portal_remote_desktop: bool, pub xdotool: bool, pub wtype: bool }`
  - `output::select_output(choice: OutputChoice, session: SessionKind, probes: &Probes) -> Output`
  - `output::Output::inject(&self, app, text) -> Result<(), String>`（签名不变）
  - `state::init(app: &adw::Application) -> impl Future<Output = ()>`（加载设置、探测、写入全局）
  - `state::settings() -> GuiSettings`、`state::set_settings(GuiSettings)`（同时 save）
  - `state::session() -> SessionKind`、`state::probes() -> Probes`
  - `state::current_output() -> Output`
  - `state::hotkey_status() -> hotkey::HotkeyStatus`、`state::set_hotkey_status(HotkeyStatus)`（Task 6 定义类型，本任务先用 `String` 占位字段 `hotkey_status: RefCell<String>`，Task 6 换成枚举）

- [ ] **Step 1: 测试 `select_output`**

在 `output.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionKind;
    use crate::settings_store::OutputChoice;

    fn probes(portal: bool, xdotool: bool, wtype: bool) -> Probes {
        Probes { portal_remote_desktop: portal, xdotool, wtype }
    }

    #[test]
    fn auto_wayland_prefers_portal_then_wtype_then_clipboard() {
        assert_eq!(select_output(OutputChoice::Auto, SessionKind::Wayland, &probes(true, true, true)), Output::Portal);
        assert_eq!(select_output(OutputChoice::Auto, SessionKind::Wayland, &probes(false, true, true)), Output::Wtype);
        assert_eq!(select_output(OutputChoice::Auto, SessionKind::Wayland, &probes(false, true, false)), Output::Clipboard);
    }

    #[test]
    fn auto_x11_uses_xdotool_or_clipboard() {
        assert_eq!(select_output(OutputChoice::Auto, SessionKind::X11, &probes(true, true, true)), Output::Xdotool);
        assert_eq!(select_output(OutputChoice::Auto, SessionKind::X11, &probes(true, false, true)), Output::Clipboard);
    }

    #[test]
    fn auto_unknown_is_clipboard() {
        assert_eq!(select_output(OutputChoice::Auto, SessionKind::Unknown, &probes(true, true, true)), Output::Clipboard);
    }

    #[test]
    fn explicit_choice_is_honoured_when_available() {
        assert_eq!(select_output(OutputChoice::Wtype, SessionKind::Wayland, &probes(true, true, true)), Output::Wtype);
        assert_eq!(select_output(OutputChoice::Clipboard, SessionKind::Wayland, &probes(true, true, true)), Output::Clipboard);
    }

    #[test]
    fn explicit_choice_falls_back_to_clipboard_when_missing() {
        assert_eq!(select_output(OutputChoice::Xdotool, SessionKind::X11, &probes(false, false, false)), Output::Clipboard);
        assert_eq!(select_output(OutputChoice::Portal, SessionKind::Wayland, &probes(false, true, true)), Output::Clipboard);
    }

    #[test]
    fn typing_backends_need_focus_return_clipboard_does_not() {
        assert!(!Output::Clipboard.needs_focus_return());
        assert!(Output::Portal.needs_focus_return());
        assert!(Output::Xdotool.needs_focus_return());
        assert!(Output::Wtype.needs_focus_return());
    }
}
```

- [ ] **Step 2: 改 `output.rs`**

把枚举与 impl 替换为：

```rust
use crate::portal;
use crate::session::SessionKind;
use crate::settings_store::OutputChoice;
use crate::typer::{type_via_tool, WTYPE_ARGS, XDOTOOL_ARGS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    Clipboard,
    Portal,
    Xdotool,
    Wtype,
}

/// What the environment offers. Computed once at startup and again from the
/// settings page's "重新检测" button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Probes {
    pub portal_remote_desktop: bool,
    pub xdotool: bool,
    pub wtype: bool,
}

impl Output {
    pub fn label(self) -> &'static str {
        match self {
            Output::Clipboard => "剪贴板",
            Output::Portal => "Portal（RemoteDesktop）",
            Output::Xdotool => "xdotool（X11）",
            Output::Wtype => "wtype（wlroots）",
        }
    }

    pub fn needs_focus_return(&self) -> bool {
        !matches!(self, Output::Clipboard)
    }

    pub async fn inject(&self, app: &adw::Application, text: &str) -> Result<(), String> {
        match self {
            Output::Clipboard => copy_and_schedule_clear(app, text),
            Output::Portal => portal::type_text(text).await,
            Output::Xdotool => {
                let t = text.to_string();
                gio::spawn_blocking(move || type_via_tool("xdotool", XDOTOOL_ARGS, &t))
                    .await
                    .unwrap_or_else(|_| Err("xdotool worker panicked".into()))
            }
            Output::Wtype => {
                let t = text.to_string();
                gio::spawn_blocking(move || type_via_tool("wtype", WTYPE_ARGS, &t))
                    .await
                    .unwrap_or_else(|_| Err("wtype worker panicked".into()))
            }
        }
    }
}

/// Spec §6 priority table. Explicit choices are honoured only if the backend
/// is actually present; otherwise the clipboard, which always works.
pub fn select_output(choice: OutputChoice, session: SessionKind, probes: &Probes) -> Output {
    match choice {
        OutputChoice::Clipboard => Output::Clipboard,
        OutputChoice::Portal => if probes.portal_remote_desktop { Output::Portal } else { Output::Clipboard },
        OutputChoice::Xdotool => if probes.xdotool { Output::Xdotool } else { Output::Clipboard },
        OutputChoice::Wtype => if probes.wtype { Output::Wtype } else { Output::Clipboard },
        OutputChoice::Auto => match session {
            SessionKind::Wayland if probes.portal_remote_desktop => Output::Portal,
            SessionKind::Wayland if probes.wtype => Output::Wtype,
            SessionKind::X11 if probes.xdotool => Output::Xdotool,
            _ => Output::Clipboard,
        },
    }
}
```

`copy_and_schedule_clear` 与 `CLIPBOARD_CLEAR_AFTER` 保留原样。

- [ ] **Step 3: `state.rs`**

```rust
//! Process-wide state for the GUI: settings, session kind, backend probes,
//! hotkey status. Single-threaded by construction (GTK main thread), hence
//! `thread_local!` + `RefCell` rather than locks.

use std::cell::RefCell;

use libadwaita as adw;

use crate::output::{select_output, Output, Probes};
use crate::portal;
use crate::session::{self, SessionKind};
use crate::settings_store::{self, GuiSettings};
use crate::typer::tool_available;

struct State {
    settings: GuiSettings,
    session: SessionKind,
    probes: Probes,
    hotkey_status: String,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        settings: GuiSettings::default(),
        session: SessionKind::Unknown,
        probes: Probes::default(),
        hotkey_status: "未启动".to_string(),
    });
}

/// Load settings and probe the environment. Call once from `startup` before
/// any window opens; safe to call again to re-probe.
pub async fn init(_app: &adw::Application) {
    let settings = settings_store::load();
    let kind = session::detect();
    let portal_ok = if kind == SessionKind::Wayland { portal::probe_remote_desktop().await } else { false };
    let probes = Probes {
        portal_remote_desktop: portal_ok,
        xdotool: tool_available("xdotool"),
        wtype: tool_available("wtype"),
    };
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.settings = settings;
        s.session = kind;
        s.probes = probes;
    });
}

pub fn settings() -> GuiSettings {
    STATE.with(|s| s.borrow().settings.clone())
}

/// Update and persist. Returns the save error, if any; the in-memory copy is
/// updated regardless so the running session behaves as the user asked.
pub fn set_settings(new: GuiSettings) -> Result<(), String> {
    STATE.with(|s| s.borrow_mut().settings = new.clone());
    settings_store::save(&new)
}

pub fn session() -> SessionKind {
    STATE.with(|s| s.borrow().session)
}

pub fn probes() -> Probes {
    STATE.with(|s| s.borrow().probes)
}

pub fn current_output() -> Output {
    STATE.with(|s| {
        let s = s.borrow();
        select_output(s.settings.output, s.session, &s.probes)
    })
}

pub fn hotkey_status() -> String {
    STATE.with(|s| s.borrow().hotkey_status.clone())
}

pub fn set_hotkey_status(text: String) {
    STATE.with(|s| s.borrow_mut().hotkey_status = text);
}
```

- [ ] **Step 4: 接线**

`main.rs`：加 `mod portal; mod session; mod settings_store; mod state; mod typer;`。`connect_startup` 改为：

```rust
    app.connect_startup(|app| {
        register_actions(app);
        let app = app.clone();
        glib::spawn_future_local(async move {
            state::init(&app).await;
        });
    });
```

`quickfill.rs` 的 `open` 里 `Panel::build(app, Output::Clipboard)` 改为 `Panel::build(app, crate::state::current_output())`。

- [ ] **Step 5: 编译 + 单测 + 手工验收**

Run: `cargo test -p immurok-gui && cargo build -p immurok-gui`
Expected: output 6 个新测试通过，阶段一测试仍通过。

手工（GNOME Wayland 48+）：`target/debug/immurok-gui --gapplication-service &`，打开 gedit / 任意文本框并聚焦，另一终端 `target/debug/immurok-gui --quick-fill`，选 OTP，触摸。
Expected：第一次弹 portal 授权框（允许）→ 6 位数字出现在文本框里；`cat ~/.config/immurok/gui.json` 里有 `portal_restore_token`；第二次取码不再弹授权框。

手工（X11 会话，装了 xdotool）：同样操作，数字直接进文本框，无授权框。

手工（Sway 或没有 xdotool 的 X11）：退回剪贴板并有通知。

- [ ] **Step 6: Commit**

```bash
git add crates/immurok-gui
git commit -m "gui: 输出后端选择（portal / xdotool / wtype / 剪贴板）与进程级状态"
```

---

### Task 6: `hotkey.rs` — GlobalShortcuts portal 与 X11 抓键

**Files:**
- Create: `crates/immurok-gui/src/hotkey.rs`
- Modify: `crates/immurok-gui/src/state.rs`（`hotkey_status` 改为 `HotkeyStatus`）
- Modify: `crates/immurok-gui/src/main.rs`（startup 后 `hotkey::start`）

**Interfaces:**
- Consumes: `state::{session, settings, set_hotkey_status}`、`session::SessionKind`
- Produces:
  - `hotkey::HotkeyStatus { Portal { trigger: String }, X11 { binding: String }, CommandOnly { reason: String } }` 带 `fn describe(&self) -> String`
  - `hotkey::start(app: &adw::Application)`（幂等：重复调用只重新报告状态）
  - `hotkey::parse_binding(s: &str) -> Option<(u16, u32)>`（X11 ModMask 位、keysym）
  - `pub const QUICK_FILL_COMMAND: &str = "immurok-gui --quick-fill"`

- [ ] **Step 1: 测试 `parse_binding`**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_backslash_default() {
        assert_eq!(parse_binding("ctrl+backslash"), Some((MOD_CONTROL, 0x5c)));
    }

    #[test]
    fn multiple_modifiers_and_letters() {
        assert_eq!(parse_binding("Ctrl+Alt+k"), Some((MOD_CONTROL | MOD_ALT, 0x6b)));
        assert_eq!(parse_binding("super+shift+F5"), Some((MOD_SUPER | MOD_SHIFT, 0xffc2)));
    }

    #[test]
    fn digits_space_and_grave() {
        assert_eq!(parse_binding("ctrl+7"), Some((MOD_CONTROL, 0x37)));
        assert_eq!(parse_binding("alt+space"), Some((MOD_ALT, 0x20)));
        assert_eq!(parse_binding("ctrl+grave"), Some((MOD_CONTROL, 0x60)));
    }

    #[test]
    fn rejects_no_modifier_or_unknown_key() {
        assert_eq!(parse_binding("backslash"), None);
        assert_eq!(parse_binding("ctrl+whatever"), None);
        assert_eq!(parse_binding(""), None);
    }
}
```

- [ ] **Step 2: 实现**

```rust
//! Global hotkey → `quick-fill` action.
//!
//! Three layers (spec §7). The portal and X11 layers are automatic; the
//! third — the user binding `immurok-gui --quick-fill` in their desktop's
//! shortcut settings — always works and is what the settings page shows when
//! neither automatic layer is available.

use std::cell::Cell;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use crate::session::SessionKind;
use crate::state;

pub const QUICK_FILL_COMMAND: &str = "immurok-gui --quick-fill";
const SHORTCUT_ID: &str = "quick-fill";
const PORTAL_PREFERRED_TRIGGER: &str = "CTRL+backslash";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyStatus {
    Portal { trigger: String },
    X11 { binding: String },
    CommandOnly { reason: String },
}

impl HotkeyStatus {
    pub fn describe(&self) -> String {
        match self {
            HotkeyStatus::Portal { trigger } => format!("由桌面管理（GlobalShortcuts portal）：{trigger}"),
            HotkeyStatus::X11 { binding } => format!("X11 全局抓键：{binding}"),
            HotkeyStatus::CommandOnly { reason } => {
                format!("{reason}。请在系统快捷键设置里绑定命令：{QUICK_FILL_COMMAND}")
            }
        }
    }
}

thread_local! {
    static STARTED: Cell<bool> = const { Cell::new(false) };
}

pub fn start(app: &adw::Application) {
    if STARTED.replace(true) {
        return;
    }
    match state::session() {
        SessionKind::Wayland => start_portal(app.clone()),
        SessionKind::X11 => start_x11(app.clone()),
        SessionKind::Unknown => state::set_hotkey_status(HotkeyStatus::CommandOnly {
            reason: "无法识别显示服务器".into(),
        }),
    }
}

// ── Portal ──────────────────────────────────────────────────

fn start_portal(app: adw::Application) {
    glib::spawn_future_local(async move {
        match run_portal(&app).await {
            Ok(()) => {}
            Err(e) => state::set_hotkey_status(HotkeyStatus::CommandOnly {
                reason: format!("GlobalShortcuts portal 不可用（{e}）"),
            }),
        }
    });
}

async fn run_portal(app: &adw::Application) -> Result<(), String> {
    use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
    use futures_util::StreamExt;

    let proxy = GlobalShortcuts::new().await.map_err(|e| e.to_string())?;
    let session = proxy.create_session().await.map_err(|e| e.to_string())?;
    let shortcuts = [NewShortcut::new(SHORTCUT_ID, "打开 immurok Quick-fill 面板")
        .preferred_trigger(PORTAL_PREFERRED_TRIGGER)];
    let bound = proxy
        .bind_shortcuts(&session, &shortcuts, None)
        .await
        .map_err(|e| e.to_string())?
        .response()
        .map_err(|e| e.to_string())?;
    let trigger = bound
        .shortcuts()
        .iter()
        .find(|s| s.id() == SHORTCUT_ID)
        .map(|s| s.trigger_description().to_string())
        .unwrap_or_else(|| "（未分配，请在桌面设置里指定）".into());
    state::set_hotkey_status(HotkeyStatus::Portal { trigger });

    let mut activated = proxy.receive_activated().await.map_err(|e| e.to_string())?;
    while let Some(ev) = activated.next().await {
        if ev.shortcut_id() == SHORTCUT_ID {
            app.activate_action("quick-fill", None);
        }
    }
    Err("portal 事件流结束".into())
}

// ── X11 ─────────────────────────────────────────────────────

pub const MOD_SHIFT: u16 = 1 << 0;
pub const MOD_CONTROL: u16 = 1 << 2;
pub const MOD_ALT: u16 = 1 << 3; // Mod1
pub const MOD_SUPER: u16 = 1 << 6; // Mod4
const MOD_LOCK: u16 = 1 << 1;
const MOD_NUMLOCK: u16 = 1 << 4; // Mod2

/// "ctrl+alt+k" → (X11 modifier mask, keysym). At least one modifier required.
pub fn parse_binding(s: &str) -> Option<(u16, u32)> {
    let mut mods: u16 = 0;
    let mut key: Option<u32> = None;
    for part in s.split('+').map(|p| p.trim()).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            "super" | "meta" | "win" => mods |= MOD_SUPER,
            other => {
                if key.is_some() {
                    return None;
                }
                key = Some(keysym_by_name(other)?);
            }
        }
    }
    if mods == 0 {
        return None;
    }
    key.map(|k| (mods, k))
}

fn keysym_by_name(name: &str) -> Option<u32> {
    match name {
        "backslash" => Some(0x5c),
        "space" => Some(0x20),
        "grave" => Some(0x60),
        "slash" => Some(0x2f),
        "minus" => Some(0x2d),
        "equal" => Some(0x3d),
        "semicolon" => Some(0x3b),
        "apostrophe" => Some(0x27),
        "comma" => Some(0x2c),
        "period" => Some(0x2e),
        "bracketleft" => Some(0x5b),
        "bracketright" => Some(0x5d),
        n if n.len() == 1 && n.as_bytes()[0].is_ascii_lowercase() => Some(n.as_bytes()[0] as u32),
        n if n.len() == 1 && n.as_bytes()[0].is_ascii_digit() => Some(n.as_bytes()[0] as u32),
        n if n.starts_with('f') => {
            let num: u32 = n[1..].parse().ok()?;
            (1..=12).contains(&num).then(|| 0xffbe + num - 1)
        }
        _ => None,
    }
}

fn start_x11(app: adw::Application) {
    let binding = state::settings().x11_hotkey;
    let Some((mods, keysym)) = parse_binding(&binding) else {
        state::set_hotkey_status(HotkeyStatus::CommandOnly {
            reason: format!("X11 热键「{binding}」无法解析"),
        });
        return;
    };

    let (tx, rx) = async_channel::unbounded::<()>();
    let status_binding = binding.clone();
    std::thread::Builder::new()
        .name("immurok-x11-hotkey".into())
        .spawn(move || {
            if let Err(e) = x11_grab_loop(mods, keysym, tx) {
                eprintln!("immurok-gui: x11 hotkey: {e}");
            }
        })
        .map_err(|e| e.to_string())
        .ok();

    state::set_hotkey_status(HotkeyStatus::X11 { binding: status_binding });
    glib::spawn_future_local(async move {
        while rx.recv().await.is_ok() {
            app.activate_action("quick-fill", None);
        }
        state::set_hotkey_status(HotkeyStatus::CommandOnly {
            reason: "X11 抓键线程退出".into(),
        });
    });
}

fn x11_grab_loop(mods: u16, keysym: u32, tx: async_channel::Sender<()>) -> Result<(), String> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, GrabMode, ModMask};
    use x11rb::protocol::Event;

    let (conn, screen_num) = x11rb::connect(None).map_err(|e| e.to_string())?;
    let setup = conn.setup();
    let root = setup.roots[screen_num].root;
    let min = setup.min_keycode;
    let count = setup.max_keycode - min + 1;
    let mapping = conn
        .get_keyboard_mapping(min, count)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?;
    let per = mapping.keysyms_per_keycode as usize;
    let keycode = mapping
        .keysyms
        .chunks(per)
        .position(|syms| syms.contains(&keysym))
        .map(|i| min + i as u8)
        .ok_or_else(|| format!("keysym 0x{keysym:x} not on this keyboard"))?;

    // Grab with every Lock / NumLock combination so the hotkey works
    // regardless of those toggles.
    for extra in [0u16, MOD_LOCK, MOD_NUMLOCK, MOD_LOCK | MOD_NUMLOCK] {
        conn.grab_key(
            true,
            root,
            ModMask::from(mods | extra),
            keycode,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .map_err(|e| e.to_string())?
        .check()
        .map_err(|e| format!("grab_key: {e} (already bound by another app?)"))?;
    }
    conn.flush().map_err(|e| e.to_string())?;

    loop {
        match conn.wait_for_event().map_err(|e| e.to_string())? {
            Event::KeyPress(_) => {
                if tx.send_blocking(()).is_err() {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
}
```

`Cargo.toml` 再加 `futures-util = "0.3"`（`StreamExt`）。

- [ ] **Step 3: `state.rs` 改类型**

`hotkey_status: String` → `hotkey_status: crate::hotkey::HotkeyStatus`，初始值 `HotkeyStatus::CommandOnly { reason: "尚未启动".into() }`；`hotkey_status()` 返回 `HotkeyStatus`，`set_hotkey_status(HotkeyStatus)`。

- [ ] **Step 4: 接线**

`main.rs` startup 的 future 改为：

```rust
        glib::spawn_future_local(async move {
            state::init(&app).await;
            hotkey::start(&app);
        });
```

加 `mod hotkey;`。

- [ ] **Step 5: 编译 + 单测 + 手工验收**

Run: `cargo test -p immurok-gui hotkey && cargo build -p immurok-gui`
Expected: 4 passed；编译通过（ashpd 的 `bind_shortcuts` / `receive_activated` / `trigger_description` 若命名不同，按 docs.rs 0.11 调整）

手工：
- GNOME 48+ / Plasma 6：`immurok-gui --gapplication-service`，首次弹「immurok 想注册快捷键」→ 允许 → 按 Ctrl+\ 面板弹出；GNOME 设置 → 应用 → immurok 里能看到并改这条快捷键。
- X11：`immurok-gui --gapplication-service`，按 Ctrl+\ 面板弹出；开 CapsLock 再按仍弹出；把 gui.json 的 `x11_hotkey` 改成 `ctrl+alt+k` 重启进程，新键生效。
- Sway：进程起来后 `journalctl --user -t immurok-gui` 或 stderr 无 panic；面板由 `bindsym $mod+o exec immurok-gui --quick-fill` 呼出。

- [ ] **Step 6: Commit**

```bash
git add crates/immurok-gui
git commit -m "gui: 全局热键（GlobalShortcuts portal + X11 XGrabKey），收口 quick-fill action"
```

---

### Task 7: 设置页

**Files:**
- Create: `crates/immurok-gui/src/pages/settings.rs`
- Modify: `crates/immurok-gui/src/pages/mod.rs`（`pub mod settings;`）
- Modify: `crates/immurok-gui/src/main_window.rs`

**Interfaces:**
- Consumes: `state::*`、`hotkey::{HotkeyStatus, QUICK_FILL_COMMAND}`、`settings_store::{OutputChoice, GuiSettings}`、`output::Output`
- Produces: `pages::settings::SettingsPage { pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self>, pub fn widget(&self) -> &gtk::Widget }`

- [ ] **Step 1: 实现**

```rust
//! Settings page: which output backend is in effect, how the hotkey is
//! wired, and a test field to prove the whole chain works without a device.

use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use crate::hotkey::{HotkeyStatus, QUICK_FILL_COMMAND};
use crate::output::Output;
use crate::settings_store::OutputChoice;
use crate::state;

pub struct SettingsPage {
    root: gtk::Widget,
    effective_row: adw::ActionRow,
    hotkey_row: adw::ActionRow,
    x11_entry: gtk::Entry,
    test_entry: gtk::Entry,
    toasts: adw::ToastOverlay,
}

impl SettingsPage {
    pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self> {
        let page = adw::PreferencesPage::new();

        // ── Output ──
        let out = adw::PreferencesGroup::builder()
            .title("Quick-fill 输出")
            .description("取到的验证码如何送到当前输入框。「自动」按会话类型挑最合适的后端，不可用时退回剪贴板。")
            .build();
        let model = gtk::StringList::new(&OutputChoice::ALL.iter().map(|c| c.label()).collect::<Vec<_>>());
        let combo = adw::ComboRow::builder().title("输出方式").model(&model).build();
        let current = state::settings().output;
        combo.set_selected(OutputChoice::ALL.iter().position(|c| *c == current).unwrap_or(0) as u32);
        let effective_row = adw::ActionRow::builder().title("当前生效").build();
        let redetect = gtk::Button::builder().label("重新检测").valign(gtk::Align::Center).build();
        effective_row.add_suffix(&redetect);
        let test_entry = gtk::Entry::builder()
            .placeholder_text("点「测试」后 3 秒内把光标放回这里")
            .valign(gtk::Align::Center)
            .hexpand(true)
            .build();
        let test_button = gtk::Button::builder().label("测试").valign(gtk::Align::Center).build();
        let test_row = adw::ActionRow::builder().title("测试输出").build();
        test_row.add_suffix(&test_entry);
        test_row.add_suffix(&test_button);
        out.add(&combo);
        out.add(&effective_row);
        out.add(&test_row);
        page.add(&out);

        // ── Hotkey ──
        let hk = adw::PreferencesGroup::builder()
            .title("全局热键")
            .description("任何桌面都可以在系统快捷键设置里把下面这条命令绑到一个键上。")
            .build();
        let hotkey_row = adw::ActionRow::builder().title("状态").build();
        let cmd_row = adw::ActionRow::builder().title("呼出命令").subtitle(QUICK_FILL_COMMAND).build();
        let copy = gtk::Button::builder().icon_name("edit-copy-symbolic").valign(gtk::Align::Center).build();
        copy.add_css_class("flat");
        cmd_row.add_suffix(&copy);
        let x11_entry = gtk::Entry::builder()
            .text(&state::settings().x11_hotkey)
            .valign(gtk::Align::Center)
            .build();
        let x11_row = adw::ActionRow::builder()
            .title("X11 热键")
            .subtitle("形如 ctrl+backslash、ctrl+alt+k；回车保存，重启 immurok-gui 生效")
            .build();
        x11_row.add_suffix(&x11_entry);
        hk.add(&hotkey_row);
        hk.add(&cmd_row);
        hk.add(&x11_row);
        page.add(&hk);

        let this = Rc::new(Self {
            root: page.upcast(),
            effective_row,
            hotkey_row,
            x11_entry,
            test_entry,
            toasts: toasts.clone(),
        });
        this.refresh();

        // Combo → settings
        let weak = Rc::downgrade(&this);
        combo.connect_selected_notify(move |c| {
            let Some(p) = weak.upgrade() else { return };
            let choice = OutputChoice::ALL[c.selected() as usize];
            let mut s = state::settings();
            s.output = choice;
            if let Err(e) = state::set_settings(s) {
                p.toast(&format!("保存失败：{e}"));
            }
            p.refresh();
        });

        // Re-probe
        let weak = Rc::downgrade(&this);
        redetect.connect_clicked(move |b| {
            let Some(p) = weak.upgrade() else { return };
            let b = b.clone();
            glib::spawn_future_local(async move {
                b.set_sensitive(false);
                if let Some(app) = b.root().and_then(|r| r.downcast::<gtk::Window>().ok()).and_then(|w| w.application()) {
                    if let Ok(app) = app.downcast::<adw::Application>() {
                        state::init(&app).await;
                    }
                }
                b.set_sensitive(true);
                p.refresh();
            });
        });

        // Copy the command
        let weak = Rc::downgrade(&this);
        copy.connect_clicked(move |_| {
            let Some(p) = weak.upgrade() else { return };
            if let Some(d) = gtk::gdk::Display::default() {
                d.clipboard().set_text(QUICK_FILL_COMMAND);
            }
            p.toast("命令已复制");
        });

        // X11 binding
        let weak = Rc::downgrade(&this);
        this.x11_entry.connect_activate(move |e| {
            let Some(p) = weak.upgrade() else { return };
            let text = e.text().to_string();
            if crate::hotkey::parse_binding(&text).is_none() {
                p.toast("无法解析这个热键");
                return;
            }
            let mut s = state::settings();
            s.x11_hotkey = text;
            match state::set_settings(s) {
                Ok(()) => p.toast("已保存，重启 immurok-gui 后生效"),
                Err(err) => p.toast(&format!("保存失败：{err}")),
            }
        });

        // Test: give the user 3 s to focus the field, then inject "123456".
        let weak = Rc::downgrade(&this);
        test_button.connect_clicked(move |b| {
            let Some(p) = weak.upgrade() else { return };
            let b = b.clone();
            glib::spawn_future_local(async move {
                let Some(app) = b.root().and_then(|r| r.downcast::<gtk::Window>().ok()).and_then(|w| w.application()).and_then(|a| a.downcast::<adw::Application>().ok()) else { return };
                let output = state::current_output();
                p.test_entry.set_text("");
                p.test_entry.grab_focus();
                b.set_sensitive(false);
                for i in (1..=3).rev() {
                    b.set_label(&format!("{i}…"));
                    glib::timeout_future(Duration::from_secs(1)).await;
                }
                let r = output.inject(&app, "123456").await;
                b.set_label("测试");
                b.set_sensitive(true);
                match (output, r) {
                    (Output::Clipboard, Ok(())) => p.toast("已写入剪贴板（Ctrl+V 粘贴验证）"),
                    (_, Ok(())) => {
                        glib::timeout_future(Duration::from_millis(300)).await;
                        if p.test_entry.text() == "123456" {
                            p.toast(&format!("{} 正常", output.label()));
                        } else {
                            p.toast(&format!("{} 没有打进输入框，检查焦点或权限", output.label()));
                        }
                    }
                    (_, Err(e)) => p.toast(&format!("{} 失败：{e}", output.label())),
                }
            });
        });

        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        &self.root
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    pub fn refresh(&self) {
        let probes = state::probes();
        let avail = format!(
            "portal {} · xdotool {} · wtype {}",
            if probes.portal_remote_desktop { "✓" } else { "✗" },
            if probes.xdotool { "✓" } else { "✗" },
            if probes.wtype { "✓" } else { "✗" },
        );
        self.effective_row.set_subtitle(&format!(
            "{}（{} 会话）\n{avail}",
            state::current_output().label(),
            state::session().label()
        ));
        let status = state::hotkey_status();
        self.hotkey_row.set_subtitle(&status.describe());
        self.x11_entry.set_sensitive(matches!(status, HotkeyStatus::X11 { .. }) || state::session() == crate::session::SessionKind::X11);
    }
}
```

- [ ] **Step 2: 挂到主窗口**

`main_window.rs` 在 keys 之后加：

```rust
        let settings = pages::settings::SettingsPage::new(&toasts);
        stack
            .add_titled(settings.widget(), Some("settings"), "设置")
            .set_icon_name(Some("emblem-system-symbolic"));
        unsafe { window.set_data("settings-page", settings) };
```

- [ ] **Step 3: 编译 + 手工验收**

Run: `cargo build -p immurok-gui && target/debug/immurok-gui`
Expected：
- 设置页「当前生效」与会话对应（GNOME Wayland：Portal ✓；X11：xdotool）。
- 改「输出方式」为「剪贴板」→ `gui.json` 的 `output` 变 `clipboard`，「当前生效」立即变剪贴板；Quick-fill 走剪贴板。
- 「测试」→ 倒计时 3 秒 → 输入框里出现 123456 → toast「… 正常」。
- 热键状态行与 Task 6 验收一致；「呼出命令」复制按钮能复制。
- X11 会话：热键框输入 `ctrl+alt+k` 回车 → toast「已保存」→ 重启后 Ctrl+Alt+K 呼出。

- [ ] **Step 4: Commit**

```bash
git add crates/immurok-gui
git commit -m "gui: 设置页（输出方式 / 重新检测 / 测试注入 / 热键状态与命令）"
```

---

### Task 8: 注入失败降级的通知文案 + 面板提示

**Files:**
- Modify: `crates/immurok-gui/src/quickfill.rs`（`deliver` 的降级分支）
- Modify: `crates/immurok-gui/src/output.rs`（`notify` 助手）

**Interfaces:**
- Produces: `output::notify(app: &adw::Application, body: &str)`

- [ ] **Step 1: 抽通知助手**

`output.rs` 加：

```rust
pub fn notify(app: &adw::Application, body: &str) {
    let note = gio::Notification::new("immurok");
    note.set_body(Some(body));
    app.send_notification(Some("quick-fill"), &note);
}
```

`copy_and_schedule_clear` 里原来构造 `gio::Notification` 的三行改为调用 `notify(app, &format!("已复制到剪贴板，{} 秒后自动清除", CLIPBOARD_CLEAR_AFTER.as_secs()))`。

- [ ] **Step 2: 降级分支说清楚发生了什么**

`quickfill.rs` `deliver` 里的

```rust
            if let Err(e) = result {
                let _ = Output::Clipboard.inject(&app, &value).await;
                eprintln!("immurok-gui: {output:?} failed ({e}); fell back to clipboard");
            }
```

改为

```rust
            if let Err(e) = result {
                eprintln!("immurok-gui: {} failed ({e}); fell back to clipboard", output.label());
                match Output::Clipboard.inject(&app, &value).await {
                    Ok(()) => crate::output::notify(
                        &app,
                        &format!("{} 注入失败，已改为复制到剪贴板（30 秒后清除）", output.label()),
                    ),
                    Err(e2) => crate::output::notify(&app, &format!("输出失败：{e}；剪贴板也失败：{e2}")),
                }
            }
```

注意剪贴板兜底在面板已关闭后执行，GNOME Wayland 下可能因无焦点而静默失败，所以第二个分支的通知是必要的。

- [ ] **Step 3: 编译 + 手工验收**

在 X11 会话把 `output` 强制为 `xdotool` 然后 `chmod -x $(which xdotool)`（测完改回）→ 取码 → 通知「xdotool 注入失败，已改为复制到剪贴板」→ 粘贴出 6 位数字。

- [ ] **Step 4: Commit**

```bash
git add crates/immurok-gui
git commit -m "gui: 注入失败自动降级剪贴板并通知"
```

---

### Task 9: 依赖检查、文档、版本

**Files:**
- Modify: `scripts/check-deps.sh`
- Modify: `README.md`、`CHANGELOG.md`
- Modify: `crates/*/Cargo.toml`（0.7.0 → 0.8.0）

- [ ] **Step 1: check-deps 可选运行依赖**

包名映射加：

```bash
    xdotool:dnf)     echo "xdotool";;
    xdotool:apt)     echo "xdotool";;
    xdotool:pacman)  echo "xdotool";;
    wtype:dnf)       echo "wtype";;
    wtype:apt)       echo "wtype";;
    wtype:pacman)    echo "wtype";;
```

GTK 开发头检查之后加：

```bash
# Quick-fill 打字后端（可选）。Wayland 上 GNOME/KDE 走 portal 不需要；X11 要 xdotool；Sway 等 wlroots 要 wtype。
if command -v xdotool >/dev/null 2>&1; then ok "xdotool  (quick-fill typing on X11)"; else warn "xdotool (X11 上 quick-fill 只能走剪贴板)" xdotool; fi
if command -v wtype   >/dev/null 2>&1; then ok "wtype    (quick-fill typing on wlroots)"; else warn "wtype (Sway/river 上 quick-fill 只能走剪贴板)" wtype; fi
```

这两条只 warn，不置 `WARN=1`（它们是纯可选的）。

- [ ] **Step 2: README 4.0 小节替换为**

```markdown
### 4.0 The GUI (optional)

`immurok-gui` opens the graphical settings window (also in your app menu as
"immurok"). It stays resident after login so the quick-fill panel is one key
away: press the hotkey in any text field, pick an OTP, touch the device, and
the code is typed for you.

| Desktop | Hotkey | Typing |
|---|---|---|
| GNOME 48+ / KDE Plasma 6 / Hyprland (Wayland) | registered automatically; change it in your desktop's shortcut settings (default Ctrl+\) | RemoteDesktop portal, one permission prompt on first use |
| Older GNOME, COSMIC, Sway and other wlroots | bind `immurok-gui --quick-fill` in your desktop's shortcut settings | portal where available, otherwise `wtype` |
| Any X11 session | grabbed automatically (default Ctrl+\, editable on the Settings page) | `xdotool` |
| Anything else | bind `immurok-gui --quick-fill` | clipboard, cleared after 30 s |

The Settings page shows which backend is in effect and has a test field.
`xdotool` / `wtype` are optional runtime dependencies; `make check-deps`
tells you if they are missing.
```

- [ ] **Step 3: CHANGELOG**

```markdown
## 0.8.0 — 2026-09-XX

### Added

- **Quick-fill types the code for you.** On Wayland it goes through the XDG
  RemoteDesktop portal (keysyms, so any keyboard layout works; one permission
  prompt, then silent via a persisted restore token). X11 uses `xdotool`,
  wlroots compositors `wtype`. The clipboard is now the fallback, with a
  notification explaining why when it kicks in.
- **Global hotkey.** Registered through the GlobalShortcuts portal on desktops
  that have it (GNOME 48+, Plasma 5.27+, Hyprland) and grabbed directly on X11.
  Everywhere else the Settings page shows the command to bind. All three paths
  fire the same `quick-fill` action.
- Settings page: output backend override, environment re-probe, test field,
  hotkey status, editable X11 binding. Stored in `~/.config/immurok/gui.json`.
```

- [ ] **Step 4: 版本**

六个 `crates/*/Cargo.toml` 的 `version = "0.8.0"`。

- [ ] **Step 5: 全量构建、测试、三桌面验收**

Run: `cargo test --workspace && make && make install`
Expected: 全绿；spec §8 表里 GNOME Wayland 48+、KDE Plasma 6 Wayland、任一 X11 会话三行各按 Task 5/6/7 的手工步骤验收一遍，结果记入 `TESTING.md` 新增的「阶段 7：GUI / Quick-fill」小节（每行一条 PASS/FAIL）。

- [ ] **Step 6: Commit**

```bash
git add scripts/check-deps.sh README.md CHANGELOG.md TESTING.md crates/*/Cargo.toml Cargo.lock
git commit -m "gui: 阶段二收尾（可选依赖检查、文档、桌面覆盖表）；版本 0.8.0"
```

---

## Self-Review

**Spec coverage**
- §6 输出后端优先级表：Task 5 `select_output` + 测试逐行对应 ✓；keysym 规则：Task 4 ✓；restore token 持久化：Task 4 ✓；外部工具 stdin：Task 3 ✓；失败降级 + 通知：Task 8 ✓；剪贴板先写 / 打字后等 150 ms：沿用阶段一 `deliver` 的 `needs_focus_return` 分支，Task 5 把三个打字后端标为 true ✓
- §7 热键三层：Task 6（portal、X11）+ Task 7（命令展示与复制）✓；`quick-fill` action 收口 ✓；不做录键控件 ✓（X11 绑定是文本框）
- §8 桌面覆盖：Task 9 README 表 + TESTING 验收 ✓
- §9 可选依赖 check-deps：Task 9 ✓
- §10 安全：文本不进 argv（Task 3）、gui.json 0600（Task 2）✓
- 不做 uinput：Global Constraints ✓

**Placeholder scan**：无 TBD / TODO。ashpd 方法名的「按 docs.rs 调整」是明确的验证动作，不是留白。

**Type consistency**
- `OutputChoice::ALL` 顺序与设置页 `StringList` 一致（Task 2 定义，Task 7 使用 `position`）✓
- `Probes` 三字段在 Task 5 定义、`state::init` 填充、Task 7 `refresh` 展示 ✓
- `HotkeyStatus` 在 Task 6 定义；Task 5 的 `state.rs` 先用 `String`，Task 6 Step 3 明确改类型；Task 7 用枚举 ✓
- `Output::inject(&self, app, text) -> Result<(), String>` 签名阶段一到阶段二不变 ✓
- `MOD_*` 常量在 Task 6 定义并被其测试引用 ✓
