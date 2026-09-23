# Linux GUI 阶段三实施计划：指纹管理 + 双机管理

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 `immurok-gui` 加上与 macOS 一致的 Fingerprints 页（登记 / 删除 / 命名 / Test / 切换指纹）和 Device 页的 Two Hosts 分组（绑定状态 / 配对 / 解除 / 解绑另一台）。

**Architecture:** 指纹与双机的 socket 原语和登记轮询状态机全部放进共享 crate `immurok-client`（`fingerprint` / `enroll_session` / `hosts` / `enroll_hint`），纯逻辑可单测；`immurok-common` 只补 0x06 Overlap 常量与 `PairProgress::from_wire`。GUI 侧新增可复用的门控倒计时对话框、登记引导对话框、Fingerprints 页、HostsGroup，以及提前落地的 `settings_store.rs`（gui.json，存指纹名字）和共用错误映射 `errors.rs`。长会话在 std 线程跑，事件经 `async-channel` 回主线程；短请求沿用 `pages::run_blocking`。

**Tech Stack:** Rust 2021、gtk4-rs 0.9、libadwaita-rs 0.7、glib/gio 0.20、serde + serde_json、async-channel 2、tempfile（dev）；现有 `immurok-common`、`immurok-client`、`immurok-gui`。

**Spec:** `docs/superpowers/specs/2026-09-18-gui-fingerprint-hosts-design.md`

## Global Constraints

- 平台下限 Debian 12 / Ubuntu 22.04：`gtk4 = "0.9"`、`libadwaita = "0.7"`，**不开任何 `v4_*` / `v1_*` feature**；只用 libadwaita 1.0 控件；确认框用 `gtk::MessageDialog`（`pages::confirm`）。
- GTK 主线程禁止直接调用任何 socket 往返（`DaemonClient` 及 `immurok_client::{status,keys,fingerprint,hosts}` 的查询函数）：短请求走 `pages::run_blocking`，长会话走 `std::thread::spawn` + `async-channel`。
- `immurok-client` 保持同步 std，一请求一连接，不引入 tokio。
- 用户可见字符串一律英文（项目约定），动态文本进 toast / `ActionRow` 标题副标题前 `glib::markup_escape_text`；`gtk::ProgressBar` 的 text 是纯文本不转义。
- 指纹名字只存 `~/.config/immurok/gui.json`（0600），槽 5 固定 "Switch Host" 不可改、不落盘；解除本机配对成功后清空名字。
- 删除 / Test / 解绑另一台主机 / 登记全部走设备指纹门；Cancel 只发 `GATE:CANCEL` 或 `FP:ENROLL_CANCEL`，GUI 不能绕过。
- 不记录任何登记 / 验证结果到日志、不 `eprintln!` 用户数据。
- 不做：Factory reset、TUI/CLI 的 Overlap 文案、名字同步、推送通道。
- 本地私有仓库 commit 用中文；改动限于 `app-linux-rs/`。所有 crate 版本 0.7.0 → 0.8.0（Task 11），CHANGELOG 加条目。
- 每个任务的 `cargo build` / `cargo test` 必须 warning-free；GUI 手工验收在 Linux 开发机（GNOME Wayland）进行，需要指纹触摸或 sudo 的验收步骤由人完成、子代理只做启动检查。

---

## 文件结构

新建：
- `crates/immurok-client/src/fingerprint.rs` — `FpSlots`、`EnrollStatus`、`parse_fp_list/parse_fp_status`、`fp_list/enroll_start/fp_status/enroll_cancel/fp_delete/fp_verify`
- `crates/immurok-client/src/enroll_session.rs` — `EnrollProgress`、`Continue`、`Clock`、`drive`、`run_enrollment`
- `crates/immurok-client/src/hosts.rs` — `HostSlots`、`parse_host_slots`、`slot_status/clear_other_slot/clear_own_slot/pair_start/pair_progress/parse_pair_progress`
- `crates/immurok-client/src/enroll_hint.rs` — 从 `immurok-cli` 搬入（`git mv`）
- `crates/immurok-gui/src/settings_store.rs` — gui.json（含 `fingerprint_names`）
- `crates/immurok-gui/src/errors.rs` — `friendly(raw)`（从 `quickfill.rs` 搬出并扩展）
- `crates/immurok-gui/src/gate_dialog.rs` — `CountdownRing`、`run(...)`
- `crates/immurok-gui/src/enroll_dialog.rs` — `EnrollOutcome`、`run(parent, slot)`
- `crates/immurok-gui/src/pages/fingerprints.rs` — `FingerprintsPage`
- `crates/immurok-gui/src/pages/hosts.rs` — `HostsGroup`

修改：
- `crates/immurok-common/src/protocol.rs` — `ENROLL_OVERLAP`
- `crates/immurok-common/src/types.rs` — `EnrollEvent::Overlap`、`PairProgress::from_wire`
- `crates/immurok-client/src/lib.rs` — 声明四个新模块
- `crates/immurok-cli/src/main.rs` — `mod enroll_hint;` → `use immurok_client::enroll_hint;`
- `crates/immurok-gui/Cargo.toml` — serde / serde_json / async-channel / tempfile(dev)
- `crates/immurok-gui/src/main.rs` — 声明新模块
- `crates/immurok-gui/src/quickfill.rs` — 改用 `errors::friendly`，删掉本地 `friendly_error` 与其测试
- `crates/immurok-gui/src/pages/mod.rs` — 声明 `fingerprints`、`hosts`
- `crates/immurok-gui/src/pages/dashboard.rs` — 去掉 Pairing 行与 `wire_pair_button`，挂 `HostsGroup`，轮询加 `slot_status`
- `crates/immurok-gui/src/main_window.rs` — 挂 Fingerprints 页
- `README.md`、`CHANGELOG.md`、六个 `crates/*/Cargo.toml`、`Cargo.lock`
- `docs/superpowers/plans/2026-09-14-gui-quickfill-input.md` — 版本步进与 `settings_store.rs` 任务改为「扩展」的两行备注

---

### Task 1: `immurok-common` — Overlap 常量与 `PairProgress::from_wire`

**Files:**
- Modify: `crates/immurok-common/src/protocol.rs`（`ENROLL_FAILED` 常量附近）
- Modify: `crates/immurok-common/src/types.rs`（`EnrollEvent`、`PairProgress`）

**Interfaces:**
- Produces: `protocol::ENROLL_OVERLAP: u8 = 0x06`；`types::EnrollEvent::Overlap`；`types::PairProgress::from_wire(s: &str) -> Option<PairProgress>`

- [ ] **Step 1: 写测试**

在 `crates/immurok-common/src/types.rs` 末尾（已有 `#[cfg(test)] mod tests` 则追加到其中，否则新建）：

```rust
#[cfg(test)]
mod enroll_overlap_tests {
    use super::*;

    #[test]
    fn overlap_is_its_own_event_not_a_failure() {
        assert_eq!(EnrollEvent::from_notification(0x06, 2, 6), EnrollEvent::Overlap);
        assert_eq!(EnrollEvent::from_notification(0x04, 6, 6), EnrollEvent::Complete);
        assert_eq!(EnrollEvent::from_notification(0x7f, 0, 6), EnrollEvent::Failed);
    }

    #[test]
    fn pair_progress_wire_roundtrip() {
        for p in [
            PairProgress::Idle,
            PairProgress::WaitFp,
            PairProgress::WaitButton,
            PairProgress::Ecdh,
            PairProgress::Done,
            PairProgress::Failed,
        ] {
            assert_eq!(PairProgress::from_wire(p.as_wire()), Some(p));
        }
        assert_eq!(PairProgress::from_wire("BOGUS"), None);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p immurok-common enroll_overlap_tests`
Expected: 编译错误 `no variant named Overlap` / `no function from_wire`

- [ ] **Step 3: 实现**

`protocol.rs` 在 `pub const ENROLL_COMPLETE: u8 = 0x04;` 之后加：

```rust
/// Mode-1 enrollment: the new frame overlaps the previous one too much.
/// Not a failure — the user shifts the finger and presses again.
pub const ENROLL_OVERLAP: u8 = 0x06;
```

`types.rs` 的 `EnrollEvent`：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollEvent {
    Waiting,
    Captured { current: u8, total: u8 },
    Processing,
    LiftFinger,
    /// Frame too similar to the previous one; progress does not advance.
    Overlap,
    Complete,
    Failed,
}

impl EnrollEvent {
    pub fn from_notification(status: u8, current: u8, total: u8) -> Self {
        match status {
            0x00 => Self::Waiting,
            0x01 => Self::Captured { current, total },
            0x02 => Self::Processing,
            0x03 => Self::LiftFinger,
            0x04 => Self::Complete,
            0x06 => Self::Overlap,
            _ => Self::Failed,
        }
    }
}
```

`impl PairProgress` 里 `as_wire` 之后加：

```rust
    pub fn from_wire(s: &str) -> Option<Self> {
        Some(match s {
            "IDLE" => Self::Idle,
            "WAIT_FP" => Self::WaitFp,
            "WAIT_BUTTON" => Self::WaitButton,
            "ECDH" => Self::Ecdh,
            "DONE" => Self::Done,
            "FAILED" => Self::Failed,
            _ => return None,
        })
    }
```

- [ ] **Step 4: 全 workspace 编译 + 测试**

Run: `cargo build --workspace && cargo test -p immurok-common`
Expected: 通过；daemon 编译无 warning（`ble.rs` 的 `match ev` 只查 `Complete`，新分支不需要改）

- [ ] **Step 5: Commit**

```bash
git add crates/immurok-common/src/protocol.rs crates/immurok-common/src/types.rs
git commit -m "common: 登记 Overlap(0x06) 事件与 PairProgress::from_wire"
```

---

### Task 2: `enroll_hint` 搬入 `immurok-client`

**Files:**
- Move: `crates/immurok-cli/src/enroll_hint.rs` → `crates/immurok-client/src/enroll_hint.rs`
- Modify: `crates/immurok-client/src/lib.rs`
- Modify: `crates/immurok-cli/src/main.rs:4`

**Interfaces:**
- Produces: `immurok_client::enroll_hint::{step_hint(step: u8) -> &'static str, step_arrow(step: u8) -> &'static str}`（内容不变）

- [ ] **Step 1: 搬文件**

```bash
git mv crates/immurok-cli/src/enroll_hint.rs crates/immurok-client/src/enroll_hint.rs
```

- [ ] **Step 2: lib.rs 声明**

`crates/immurok-client/src/lib.rs` 的 `pub mod daemon;` 之后加一行 `pub mod enroll_hint;`（本任务只加这一行；Task 3–5 各自再加自己的模块）。

- [ ] **Step 3: CLI 改引用**

`crates/immurok-cli/src/main.rs` 把 `mod enroll_hint;` 改为：

```rust
// Enrollment step text now lives in immurok-client (shared with the GUI);
// re-exported under the old module name so `crate::enroll_hint::…` in
// commands/ and tui/ keeps working unchanged.
use immurok_client::enroll_hint;
```

`commands/fingerprint.rs` 与 `tui/widgets.rs` 里的 `crate::enroll_hint::…` 不动。

- [ ] **Step 4: 编译 + 测试**

Run: `cargo build --workspace && cargo test -p immurok-client -p immurok-cli`
Expected: 通过；`enroll_hint` 的 2 个测试现在在 `immurok-client` 下跑

- [ ] **Step 5: Commit**

```bash
git add crates/immurok-client crates/immurok-cli/src
git commit -m "client: enroll_hint 搬入 immurok-client，CLI 用别名引用"
```

---

### Task 3: `immurok-client::fingerprint` — 槽位与登记原语

**Files:**
- Create: `crates/immurok-client/src/fingerprint.rs`
- Modify: `crates/immurok-client/src/lib.rs`（加 `pub mod fingerprint;`）

**Interfaces:**
- Consumes: `crate::DaemonClient`、`crate::keys::GATE_TIMEOUT`、`immurok_common::protocol::{ENROLL_*, MAX_FINGERPRINT_SLOTS, SWITCH_FINGER_SLOT, TOTAL_FINGERPRINT_SLOTS}`
- Produces:
  - `pub const SWITCH_SLOT: u8`（= 5）、`pub const AUTH_SLOTS: u8`（= 5）
  - `pub struct FpSlots { pub bitmap: u8 }` + `is_enrolled(self, slot) -> bool`、`auth_slots(self) -> Vec<u8>`、`auth_count(self) -> usize`、`first_free_auth_slot(self) -> Option<u8>`、`switch_enrolled(self) -> bool`、`any(self) -> bool`
  - `pub fn parse_fp_list(line: &str) -> Option<FpSlots>`、`pub fn fp_list() -> Result<FpSlots, String>`
  - `pub enum EnrollStatus { Idle, Waiting { current: u8, total: u8 }, Captured { current: u8, total: u8 }, Processing, LiftFinger, Overlap, Complete, Failed }`（`Copy, PartialEq, Eq`）
  - `pub fn parse_fp_status(line: &str) -> Option<EnrollStatus>`、`pub fn fp_status() -> Result<EnrollStatus, String>`
  - `pub fn enroll_start(slot: u8) -> Result<(), String>`、`pub fn enroll_cancel()`、`pub fn fp_delete(slot: u8) -> Result<(), String>`、`pub fn fp_verify() -> Result<bool, String>`

- [ ] **Step 1: 写测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitmap_helpers() {
        let none = FpSlots { bitmap: 0 };
        assert!(!none.any());
        assert_eq!(none.first_free_auth_slot(), Some(0));
        assert_eq!(none.auth_count(), 0);

        let s = FpSlots { bitmap: 0b10_0101 }; // slots 0, 2, 5
        assert!(s.is_enrolled(0) && s.is_enrolled(2) && s.is_enrolled(5));
        assert!(!s.is_enrolled(1) && !s.is_enrolled(6));
        assert_eq!(s.auth_slots(), vec![0, 2]);
        assert_eq!(s.auth_count(), 2);
        assert!(s.switch_enrolled());
        assert_eq!(s.first_free_auth_slot(), Some(1));

        let full = FpSlots { bitmap: 0b01_1111 };
        assert_eq!(full.first_free_auth_slot(), None);
        assert!(!full.switch_enrolled());
    }

    #[test]
    fn parses_fp_list() {
        assert_eq!(parse_fp_list("OK:37"), Some(FpSlots { bitmap: 37 }));
        assert_eq!(parse_fp_list("ERROR:NOT_CONNECTED"), None);
        assert_eq!(parse_fp_list("OK:abc"), None);
    }

    #[test]
    fn parses_fp_status() {
        assert_eq!(parse_fp_status("OK:IDLE"), Some(EnrollStatus::Idle));
        assert_eq!(parse_fp_status("OK:0:0:6"), Some(EnrollStatus::Waiting { current: 0, total: 6 }));
        assert_eq!(parse_fp_status("OK:1:2:6"), Some(EnrollStatus::Captured { current: 2, total: 6 }));
        assert_eq!(parse_fp_status("OK:2:2:6"), Some(EnrollStatus::Processing));
        assert_eq!(parse_fp_status("OK:3:2:6"), Some(EnrollStatus::LiftFinger));
        assert_eq!(parse_fp_status("OK:6:2:6"), Some(EnrollStatus::Overlap));
        assert_eq!(parse_fp_status("OK:4:6:6"), Some(EnrollStatus::Complete));
        assert_eq!(parse_fp_status("OK:255:0:6"), Some(EnrollStatus::Failed));
        // Unknown code = failure, never silently "waiting".
        assert_eq!(parse_fp_status("OK:9:0:6"), Some(EnrollStatus::Failed));
        assert_eq!(parse_fp_status("ERROR:NOT_CONNECTED"), None);
        assert_eq!(parse_fp_status("OK:"), None);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p immurok-client fingerprint`
Expected: 编译失败，缺 `FpSlots` 等

- [ ] **Step 3: 实现**

`crates/immurok-client/src/fingerprint.rs` 全文：

```rust
//! Fingerprint slots and enrollment primitives.
//!
//! Wire formats (`immurok-daemon/src/socket.rs`):
//!   FP:LIST            → `OK:<bitmap>`            (u8, bit N = slot N)
//!   FP:ENROLL:<slot>   → `OK:ENROLL_STARTED`      (after the FP-gate, if any;
//!                        the 6-frame capture continues on the device)
//!   FP:STATUS          → `OK:IDLE` | `OK:<status>:<current>:<total>`
//!                        (raw status code, see `protocol::ENROLL_*`)
//!   FP:ENROLL_CANCEL   → `OK:ENROLL_CANCELLED`
//!   FP:DELETE:<slot>   → `OK:DELETED`             (FP-gated when any finger exists)
//!   FP:VERIFY          → `OK:MATCH` | `OK:NO_MATCH` (FP-gated)

use immurok_common::protocol::{
    ENROLL_CAPTURED, ENROLL_COMPLETE, ENROLL_LIFT_FINGER, ENROLL_OVERLAP, ENROLL_PROCESSING,
    ENROLL_WAITING, MAX_FINGERPRINT_SLOTS, SWITCH_FINGER_SLOT, TOTAL_FINGERPRINT_SLOTS,
};

use crate::keys::GATE_TIMEOUT;
use crate::DaemonClient;

/// Slot reserved for the host-switch finger (firmware ≥ 1.6.4).
pub const SWITCH_SLOT: u8 = SWITCH_FINGER_SLOT;
/// Number of authentication slots (0..AUTH_SLOTS).
pub const AUTH_SLOTS: u8 = MAX_FINGERPRINT_SLOTS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FpSlots {
    pub bitmap: u8,
}

impl FpSlots {
    pub fn is_enrolled(self, slot: u8) -> bool {
        slot < TOTAL_FINGERPRINT_SLOTS && self.bitmap & (1 << slot) != 0
    }

    pub fn auth_slots(self) -> Vec<u8> {
        (0..AUTH_SLOTS).filter(|s| self.is_enrolled(*s)).collect()
    }

    pub fn auth_count(self) -> usize {
        self.auth_slots().len()
    }

    pub fn first_free_auth_slot(self) -> Option<u8> {
        (0..AUTH_SLOTS).find(|s| !self.is_enrolled(*s))
    }

    pub fn switch_enrolled(self) -> bool {
        self.is_enrolled(SWITCH_SLOT)
    }

    /// Any finger at all (auth or switch). The daemon gates enroll/delete
    /// behind a touch only when this is true.
    pub fn any(self) -> bool {
        self.bitmap & ((1 << TOTAL_FINGERPRINT_SLOTS) - 1) != 0
    }
}

pub fn parse_fp_list(line: &str) -> Option<FpSlots> {
    let bitmap = line.trim().strip_prefix("OK:")?.parse::<u8>().ok()?;
    Some(FpSlots { bitmap })
}

pub fn fp_list() -> Result<FpSlots, String> {
    let rsp = DaemonClient::connect()?.send("FP:LIST")?;
    match parse_fp_list(&rsp) {
        Some(s) => Ok(s),
        None => Err(rsp),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollStatus {
    Idle,
    Waiting { current: u8, total: u8 },
    Captured { current: u8, total: u8 },
    Processing,
    LiftFinger,
    /// Frame too similar to the previous one; shift the finger, no progress.
    Overlap,
    Complete,
    Failed,
}

pub fn parse_fp_status(line: &str) -> Option<EnrollStatus> {
    let parts: Vec<&str> = line.trim().split(':').collect();
    if parts.first() != Some(&"OK") {
        return None;
    }
    let second = *parts.get(1)?;
    if second == "IDLE" {
        return Some(EnrollStatus::Idle);
    }
    let status: u8 = second.parse().ok()?;
    let current: u8 = parts.get(2).and_then(|v| v.parse().ok()).unwrap_or(0);
    let total: u8 = parts.get(3).and_then(|v| v.parse().ok()).unwrap_or(6);
    Some(match status {
        ENROLL_WAITING => EnrollStatus::Waiting { current, total },
        ENROLL_CAPTURED => EnrollStatus::Captured { current, total },
        ENROLL_PROCESSING => EnrollStatus::Processing,
        ENROLL_LIFT_FINGER => EnrollStatus::LiftFinger,
        ENROLL_OVERLAP => EnrollStatus::Overlap,
        ENROLL_COMPLETE => EnrollStatus::Complete,
        _ => EnrollStatus::Failed,
    })
}

pub fn fp_status() -> Result<EnrollStatus, String> {
    let rsp = DaemonClient::connect()?.send("FP:STATUS")?;
    parse_fp_status(&rsp).ok_or_else(|| format!("unexpected FP:STATUS reply: {rsp}"))
}

/// Ask the device to start enrolling `slot`. Blocks through the FP-gate
/// (when fingers already exist) and returns once capture has begun; progress
/// is then polled with [`fp_status`].
pub fn enroll_start(slot: u8) -> Result<(), String> {
    let rsp = DaemonClient::connect()?.send_with_timeout(&format!("FP:ENROLL:{slot}"), GATE_TIMEOUT)?;
    if rsp == "OK:ENROLL_STARTED" {
        Ok(())
    } else {
        Err(rsp)
    }
}

/// Best effort: abort an enrollment (also unblocks a pending FP-gate wait).
pub fn enroll_cancel() {
    if let Ok(mut c) = DaemonClient::connect() {
        let _ = c.send("FP:ENROLL_CANCEL");
    }
}

pub fn fp_delete(slot: u8) -> Result<(), String> {
    let rsp = DaemonClient::connect()?.send_with_timeout(&format!("FP:DELETE:{slot}"), GATE_TIMEOUT)?;
    if rsp.starts_with("OK") {
        Ok(())
    } else {
        Err(rsp)
    }
}

/// One gated match attempt. `Ok(true)` = the touched finger matched.
pub fn fp_verify() -> Result<bool, String> {
    let rsp = DaemonClient::connect()?.send_with_timeout("FP:VERIFY", GATE_TIMEOUT)?;
    match rsp.as_str() {
        "OK:MATCH" => Ok(true),
        "OK:NO_MATCH" => Ok(false),
        _ => Err(rsp),
    }
}
```

`lib.rs` 加 `pub mod fingerprint;`。

- [ ] **Step 4: 跑测试**

Run: `cargo test -p immurok-client fingerprint`
Expected: 3 passed

- [ ] **Step 5: Commit**

```bash
git add crates/immurok-client/src/fingerprint.rs crates/immurok-client/src/lib.rs
git commit -m "client: 指纹槽位位图与 FP:ENROLL/STATUS/DELETE/VERIFY 原语"
```

---

### Task 4: `immurok-client::enroll_session` — 登记轮询驱动器

**Files:**
- Create: `crates/immurok-client/src/enroll_session.rs`
- Modify: `crates/immurok-client/src/lib.rs`（加 `pub mod enroll_session;`）

**Interfaces:**
- Consumes: `crate::fingerprint::{enroll_start, enroll_cancel, fp_list, fp_status, EnrollStatus, FpSlots}`
- Produces:
  - `pub enum EnrollProgress { GateWaiting, Started, Step { next_step: u8, captured: u8, total: u8 }, LiftFinger, Overlap, Processing, Complete, Failed(String) }`（`Clone, PartialEq, Eq`）
  - `pub enum Continue { Go, Stop }`（`Copy, PartialEq, Eq`）
  - `pub trait Clock { fn now(&self) -> Instant; fn sleep(&mut self, d: Duration); }`
  - `pub enum Outcome { Complete, Failed, Cancelled }`
  - `pub fn drive(slot, poll, bitmap, tick, clock) -> Outcome`（纯逻辑，可注入）
  - `pub fn run_enrollment(slot: u8, tick: impl FnMut(EnrollProgress) -> Continue)`（阻塞，真实 socket）
  - 常量 `POLL_INTERVAL = 150 ms`、`IDLE_BITMAP_CHECK = 3 s`、`ENROLL_TIMEOUT = 360 s`、`DEFAULT_TOTAL = 6`

- [ ] **Step 1: 写测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct FakeClock {
        now: Instant,
    }
    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            self.now
        }
        fn sleep(&mut self, d: Duration) {
            self.now += d;
        }
    }

    /// Runs `drive` over a scripted FP:STATUS sequence; once the script is
    /// exhausted every poll says IDLE. The bitmap query always reports
    /// `bitmap_after`. Returns the events the UI would see.
    fn script(
        polls: Vec<Result<EnrollStatus, String>>,
        bitmap_after: u8,
        stop_on: Option<EnrollProgress>,
    ) -> (Vec<EnrollProgress>, Outcome) {
        let mut polls: VecDeque<_> = polls.into();
        let mut events = Vec::new();
        let mut clock = FakeClock { now: Instant::now() };
        let outcome = drive(
            1,
            || polls.pop_front().unwrap_or(Ok(EnrollStatus::Idle)),
            || Ok(FpSlots { bitmap: bitmap_after }),
            &mut |ev| {
                let stop = stop_on.as_ref() == Some(&ev);
                events.push(ev);
                if stop {
                    Continue::Stop
                } else {
                    Continue::Go
                }
            },
            &mut clock,
        );
        (events, outcome)
    }

    #[test]
    fn happy_path_emits_steps_then_complete() {
        use EnrollStatus as S;
        let (events, outcome) = script(
            vec![
                Ok(S::Waiting { current: 0, total: 6 }),
                Ok(S::Captured { current: 1, total: 6 }),
                Ok(S::LiftFinger),
                Ok(S::Waiting { current: 1, total: 6 }),
                Ok(S::Captured { current: 2, total: 6 }),
                Ok(S::Processing),
                Ok(S::Complete),
            ],
            0,
            None,
        );
        assert_eq!(outcome, Outcome::Complete);
        assert_eq!(
            events,
            vec![
                EnrollProgress::Started,
                EnrollProgress::Step { next_step: 1, captured: 0, total: 6 },
                EnrollProgress::Step { next_step: 2, captured: 1, total: 6 },
                EnrollProgress::LiftFinger,
                EnrollProgress::Step { next_step: 2, captured: 1, total: 6 },
                EnrollProgress::Step { next_step: 3, captured: 2, total: 6 },
                EnrollProgress::Processing,
                EnrollProgress::Complete,
            ]
        );
    }

    #[test]
    fn repeated_identical_polls_are_not_repeated_events() {
        use EnrollStatus as S;
        let (events, _) = script(
            vec![
                Ok(S::Waiting { current: 0, total: 6 }),
                Ok(S::Waiting { current: 0, total: 6 }),
                Ok(S::Waiting { current: 0, total: 6 }),
                Ok(S::Complete),
            ],
            0,
            None,
        );
        assert_eq!(events.len(), 3); // Started, Step 1, Complete
    }

    #[test]
    fn overlap_does_not_advance_step() {
        use EnrollStatus as S;
        let (events, _) = script(
            vec![
                Ok(S::Captured { current: 2, total: 6 }),
                Ok(S::Overlap),
                Ok(S::Waiting { current: 2, total: 6 }),
                Ok(S::Complete),
            ],
            0,
            None,
        );
        assert_eq!(events[1], EnrollProgress::Step { next_step: 3, captured: 2, total: 6 });
        assert_eq!(events[2], EnrollProgress::Overlap);
        assert_eq!(events[3], EnrollProgress::Step { next_step: 3, captured: 2, total: 6 });
    }

    #[test]
    fn failed_status_ends_with_failed() {
        let (events, outcome) = script(vec![Ok(EnrollStatus::Failed)], 0, None);
        assert_eq!(outcome, Outcome::Failed);
        assert!(matches!(events.last(), Some(EnrollProgress::Failed(_))));
    }

    #[test]
    fn not_connected_is_reported_as_disconnect() {
        let (events, outcome) = script(vec![Err("ERROR:NOT_CONNECTED".into())], 0, None);
        assert_eq!(outcome, Outcome::Failed);
        assert_eq!(events.last(), Some(&EnrollProgress::Failed("Device disconnected".into())));
    }

    #[test]
    fn transient_poll_errors_are_skipped() {
        let (events, outcome) =
            script(vec![Err("Read failed: EAGAIN".into()), Ok(EnrollStatus::Complete)], 0, None);
        assert_eq!(outcome, Outcome::Complete);
        assert_eq!(events, vec![EnrollProgress::Started, EnrollProgress::Complete]);
    }

    #[test]
    fn stop_from_ui_cancels() {
        let (events, outcome) = script(
            vec![Ok(EnrollStatus::Waiting { current: 0, total: 6 }), Ok(EnrollStatus::Complete)],
            0,
            Some(EnrollProgress::Step { next_step: 1, captured: 0, total: 6 }),
        );
        assert_eq!(outcome, Outcome::Cancelled);
        assert_eq!(events.len(), 2); // Started, Step 1 — nothing after Stop
    }

    #[test]
    fn idle_falls_back_to_bitmap_after_three_seconds() {
        // Every poll says IDLE (e.g. the completion notification was missed);
        // the bitmap says slot 1 is enrolled → Complete.
        let (events, outcome) = script(vec![], 0b10, None);
        assert_eq!(outcome, Outcome::Complete);
        assert_eq!(events.last(), Some(&EnrollProgress::Complete));
    }

    #[test]
    fn times_out_after_enroll_timeout() {
        let (events, outcome) = script(vec![], 0, None);
        assert_eq!(outcome, Outcome::Failed);
        assert_eq!(events.last(), Some(&EnrollProgress::Failed("Enrollment timed out".into())));
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p immurok-client enroll_session`
Expected: 编译失败

- [ ] **Step 3: 实现**

`crates/immurok-client/src/enroll_session.rs` 全文：

```rust
//! Enrollment driver: `FP:ENROLL`, then poll `FP:STATUS` until the device
//! reports complete or failed.
//!
//! Same state machine as the TUI's `App::action_enroll`, written as a pure
//! function ([`drive`]) over injected poll / bitmap / clock functions so the
//! event sequence can be unit-tested. [`run_enrollment`] binds it to the real
//! socket and blocks the calling thread; the GUI runs it on a worker thread
//! and forwards each event to the main loop.
//!
//! The daemon caches only the *latest* enrollment notification, so two
//! consecutive polls that read the same tuple are the same event and are
//! reported once. (Two overlap rejects with no other event in between are
//! therefore indistinguishable — same limitation as the TUI.)

use std::time::{Duration, Instant};

use crate::fingerprint::{enroll_cancel, enroll_start, fp_list, fp_status, EnrollStatus, FpSlots};

pub const POLL_INTERVAL: Duration = Duration::from_millis(150);
/// While `FP:STATUS` keeps saying IDLE, re-read the bitmap this often in
/// case the completion notification was missed.
pub const IDLE_BITMAP_CHECK: Duration = Duration::from_secs(3);
pub const ENROLL_TIMEOUT: Duration = Duration::from_secs(360);
pub const DEFAULT_TOTAL: u8 = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrollProgress {
    /// `FP:ENROLL` is about to be sent; the device may first ask for an
    /// already-enrolled finger.
    GateWaiting,
    /// Capture has begun on the device.
    Started,
    /// The user should now press frame `next_step` (1-based); `captured`
    /// frames are already in.
    Step { next_step: u8, captured: u8, total: u8 },
    LiftFinger,
    /// Too similar to the previous frame — shift and press again.
    Overlap,
    Processing,
    Complete,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continue {
    Go,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Complete,
    Failed,
    Cancelled,
}

pub trait Clock {
    fn now(&self) -> Instant;
    fn sleep(&mut self, d: Duration);
}

struct RealClock;

impl Clock for RealClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
}

fn step_event(current: u8, total: u8) -> EnrollProgress {
    EnrollProgress::Step {
        next_step: current.saturating_add(1).min(total.max(1)),
        captured: current,
        total,
    }
}

/// Poll loop after `FP:ENROLL` has been accepted. Emits `Started` first.
pub fn drive(
    slot: u8,
    mut poll: impl FnMut() -> Result<EnrollStatus, String>,
    mut bitmap: impl FnMut() -> Result<FpSlots, String>,
    tick: &mut impl FnMut(EnrollProgress) -> Continue,
    clock: &mut impl Clock,
) -> Outcome {
    if tick(EnrollProgress::Started) == Continue::Stop {
        return Outcome::Cancelled;
    }
    let started = clock.now();
    let mut last: Option<EnrollStatus> = None;
    let mut idle_since: Option<Instant> = None;

    loop {
        if clock.now().duration_since(started) > ENROLL_TIMEOUT {
            tick(EnrollProgress::Failed("Enrollment timed out".into()));
            return Outcome::Failed;
        }
        clock.sleep(POLL_INTERVAL);

        let status = match poll() {
            Ok(s) => s,
            Err(e) if e.contains("NOT_CONNECTED") => {
                tick(EnrollProgress::Failed("Device disconnected".into()));
                return Outcome::Failed;
            }
            // A single failed poll (socket hiccup) is not the end of the world.
            Err(_) => continue,
        };

        if status == EnrollStatus::Idle {
            let now = clock.now();
            match idle_since {
                None => idle_since = Some(now),
                Some(t) if now.duration_since(t) >= IDLE_BITMAP_CHECK => {
                    idle_since = Some(now);
                    if let Ok(b) = bitmap() {
                        if b.is_enrolled(slot) {
                            tick(EnrollProgress::Complete);
                            return Outcome::Complete;
                        }
                    }
                }
                Some(_) => {}
            }
            continue;
        }
        idle_since = None;

        if last == Some(status) {
            continue;
        }
        last = Some(status);

        let cont = match status {
            EnrollStatus::Waiting { current, total } => tick(step_event(current, total)),
            EnrollStatus::Captured { current, total } => tick(step_event(current, total)),
            EnrollStatus::LiftFinger => tick(EnrollProgress::LiftFinger),
            EnrollStatus::Overlap => tick(EnrollProgress::Overlap),
            EnrollStatus::Processing => tick(EnrollProgress::Processing),
            EnrollStatus::Complete => {
                tick(EnrollProgress::Complete);
                return Outcome::Complete;
            }
            EnrollStatus::Failed => {
                tick(EnrollProgress::Failed("Enrollment failed".into()));
                return Outcome::Failed;
            }
            EnrollStatus::Idle => unreachable!("handled above"),
        };
        if cont == Continue::Stop {
            return Outcome::Cancelled;
        }
    }
}

/// Blocking end-to-end enrollment of `slot` against the real daemon.
/// `tick` is called on the calling thread for every event; returning
/// [`Continue::Stop`] aborts (an `FP:ENROLL_CANCEL` is sent).
pub fn run_enrollment(slot: u8, mut tick: impl FnMut(EnrollProgress) -> Continue) {
    if tick(EnrollProgress::GateWaiting) == Continue::Stop {
        return;
    }
    if let Err(e) = enroll_start(slot) {
        tick(EnrollProgress::Failed(e));
        return;
    }
    let mut clock = RealClock;
    if drive(slot, fp_status, fp_list, &mut tick, &mut clock) == Outcome::Cancelled {
        enroll_cancel();
    }
}
```

`lib.rs` 加 `pub mod enroll_session;`。

- [ ] **Step 4: 跑测试**

Run: `cargo test -p immurok-client enroll_session`
Expected: 9 passed（`idle_falls_back` 与 `times_out` 靠 FakeClock 推进，瞬间完成）

- [ ] **Step 5: Commit**

```bash
git add crates/immurok-client/src/enroll_session.rs crates/immurok-client/src/lib.rs
git commit -m "client: 登记轮询驱动器 run_enrollment，纯逻辑 drive 可单测"
```

---

### Task 5: `immurok-client::hosts` — 双机槽位与配对原语

**Files:**
- Create: `crates/immurok-client/src/hosts.rs`
- Modify: `crates/immurok-client/src/lib.rs`（加 `pub mod hosts;`）

**Interfaces:**
- Consumes: `immurok_common::dual_host::{parse_slot_status_line, parse_slot_owner, SlotStatus}`、`immurok_common::types::PairProgress`（Task 1 的 `from_wire`）、`crate::keys::GATE_TIMEOUT`
- Produces:
  - `pub struct HostSlots { pub supported: bool, pub slot1: bool, pub slot2: bool, pub active: u8, pub mine: Option<u8> }`（`Copy, PartialEq, Eq, Default`）+ `bound(self, n: u8) -> bool`、`both_bound(self) -> bool`、`pub fn other_of(n: u8) -> u8`
  - `pub fn parse_host_slots(line: &str) -> Option<HostSlots>`、`pub fn slot_status() -> Result<HostSlots, String>`
  - `pub fn clear_other_slot(n: u8) -> Result<(), String>`、`pub fn clear_own_slot() -> Result<(), String>`
  - `pub const PAIR_TIMEOUT: Duration = 150 s`、`pub fn pair_start() -> Result<(), String>`
  - `pub fn parse_pair_progress(line: &str) -> Option<PairProgress>`、`pub fn pair_progress() -> Result<PairProgress, String>`

- [ ] **Step 1: 写测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_with_owner() {
        let h = parse_host_slots("OK:3:1:2").unwrap();
        assert!(h.supported && h.slot1 && h.slot2);
        assert_eq!(h.active, 1);
        assert_eq!(h.mine, Some(2));
        assert!(h.both_bound());
        assert!(h.bound(1) && h.bound(2) && !h.bound(3));
    }

    #[test]
    fn owner_zero_or_missing_is_unproven() {
        assert_eq!(parse_host_slots("OK:1:1:0").unwrap().mine, None);
        assert_eq!(parse_host_slots("OK:1:1").unwrap().mine, None);
    }

    #[test]
    fn unsupported_firmware() {
        let h = parse_host_slots("OK:UNSUPPORTED").unwrap();
        assert!(!h.supported && !h.slot1 && !h.slot2);
        assert_eq!(h.mine, None);
    }

    #[test]
    fn rejects_errors() {
        assert_eq!(parse_host_slots("ERROR:NOT_CONNECTED"), None);
        assert_eq!(parse_host_slots("OK:3:9"), None);
    }

    #[test]
    fn other_slot() {
        assert_eq!(HostSlots::other_of(1), 2);
        assert_eq!(HostSlots::other_of(2), 1);
    }

    #[test]
    fn pair_progress_line() {
        assert_eq!(parse_pair_progress("OK:WAIT_BUTTON"), Some(PairProgress::WaitButton));
        assert_eq!(parse_pair_progress("OK:IDLE"), Some(PairProgress::Idle));
        assert_eq!(parse_pair_progress("ERROR:X"), None);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p immurok-client hosts`
Expected: 编译失败

- [ ] **Step 3: 实现**

```rust
//! Dual-host (two computers per device) slots and pairing.
//!
//! Wire formats (`immurok-daemon/src/socket.rs`, decoding helpers in
//! `immurok-common::dual_host`):
//!   SLOT:STATUS     → `OK:<bitmap>:<active>[:<mine>]` | `OK:UNSUPPORTED`
//!   SLOT:CLEAR      → `OK:CLEARED` | `OK:CLEARED_UNCONFIRMED` | `OK:CLEARED_LOCAL_ONLY`
//!                     (this host's own slot; ungated)
//!   SLOT:CLEAR:<n>  → `OK:CLEARED` | `OK:CLEARED_SLOT_UNPAIRED`
//!                     (the other host's slot; always FP-gated)
//!   PAIR:START      → `OK:PAIRED` (up to 150 s; FP-gated for a second host)
//!   PAIR:PROGRESS   → `OK:<IDLE|WAIT_FP|WAIT_BUTTON|ECDH|DONE|FAILED>`

use std::time::Duration;

use immurok_common::dual_host::{parse_slot_owner, parse_slot_status_line, SlotStatus};
use immurok_common::types::PairProgress;

use crate::keys::GATE_TIMEOUT;
use crate::DaemonClient;

pub const PAIR_TIMEOUT: Duration = Duration::from_secs(150);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HostSlots {
    /// False on firmware without dual-host support (`OK:UNSUPPORTED`).
    pub supported: bool,
    pub slot1: bool,
    pub slot2: bool,
    /// Slot the device is currently presenting (1 or 2; 0 if unsupported).
    pub active: u8,
    /// Slot the daemon has cryptographically proven to be THIS computer's.
    /// `None` = not proven (unpaired, older daemon, or verification failed).
    pub mine: Option<u8>,
}

impl HostSlots {
    pub fn bound(self, n: u8) -> bool {
        match n {
            1 => self.slot1,
            2 => self.slot2,
            _ => false,
        }
    }

    pub fn both_bound(self) -> bool {
        self.slot1 && self.slot2
    }

    pub fn other_of(n: u8) -> u8 {
        if n == 1 {
            2
        } else {
            1
        }
    }
}

pub fn parse_host_slots(line: &str) -> Option<HostSlots> {
    Some(match parse_slot_status_line(line)? {
        SlotStatus::Unsupported => HostSlots { supported: false, ..HostSlots::default() },
        SlotStatus::Supported { bitmap, active } => HostSlots {
            supported: true,
            slot1: bitmap & 0b01 != 0,
            slot2: bitmap & 0b10 != 0,
            active,
            mine: parse_slot_owner(line),
        },
    })
}

pub fn slot_status() -> Result<HostSlots, String> {
    let rsp = DaemonClient::connect()?.send("SLOT:STATUS")?;
    parse_host_slots(&rsp).ok_or_else(|| format!("unexpected SLOT:STATUS reply: {rsp}"))
}

fn ok_if_ok(rsp: String) -> Result<(), String> {
    if rsp.starts_with("OK") {
        Ok(())
    } else {
        Err(rsp)
    }
}

/// Unbind the *other* computer. The device asks for an enrolled finger first.
pub fn clear_other_slot(n: u8) -> Result<(), String> {
    let rsp = DaemonClient::connect()?.send_with_timeout(&format!("SLOT:CLEAR:{n}"), GATE_TIMEOUT)?;
    ok_if_ok(rsp)
}

/// Unpair this computer (clears our own slot; fingerprints and keys stay).
pub fn clear_own_slot() -> Result<(), String> {
    let rsp = DaemonClient::connect()?.send("SLOT:CLEAR")?;
    ok_if_ok(rsp)
}

/// Blocks until the user confirms on the device (button, plus a touch when
/// enrolling as the second host) or the daemon gives up.
pub fn pair_start() -> Result<(), String> {
    let rsp = DaemonClient::connect()?.send_with_timeout("PAIR:START", PAIR_TIMEOUT)?;
    if rsp == "OK:PAIRED" {
        Ok(())
    } else {
        Err(rsp)
    }
}

pub fn parse_pair_progress(line: &str) -> Option<PairProgress> {
    PairProgress::from_wire(line.trim().strip_prefix("OK:")?)
}

pub fn pair_progress() -> Result<PairProgress, String> {
    let rsp = DaemonClient::connect()?.send("PAIR:PROGRESS")?;
    parse_pair_progress(&rsp).ok_or_else(|| format!("unexpected PAIR:PROGRESS reply: {rsp}"))
}
```

`lib.rs` 加 `pub mod hosts;`。最终 `lib.rs` 的模块声明为：`daemon, enroll_hint, enroll_session, fingerprint, hosts, keys, status`。

- [ ] **Step 4: 跑测试**

Run: `cargo test -p immurok-client`
Expected: 全部通过（keys 5 + status 6 + fingerprint 3 + enroll_session 9 + hosts 6 + enroll_hint 2）

- [ ] **Step 5: Commit**

```bash
git add crates/immurok-client/src/hosts.rs crates/immurok-client/src/lib.rs
git commit -m "client: 双机槽位解析、SLOT:CLEAR、PAIR:START/PROGRESS 原语"
```

---

### Task 6: `immurok-gui` 依赖、`settings_store.rs`、`errors.rs`

**Files:**
- Modify: `crates/immurok-gui/Cargo.toml`
- Create: `crates/immurok-gui/src/settings_store.rs`
- Create: `crates/immurok-gui/src/errors.rs`
- Modify: `crates/immurok-gui/src/main.rs`（`mod errors; mod settings_store;`）
- Modify: `crates/immurok-gui/src/quickfill.rs`（改用 `crate::errors::friendly`，删本地 `friendly_error` 与其测试）

**Interfaces:**
- Consumes: `immurok_client::fingerprint::SWITCH_SLOT`
- Produces:
  - `settings_store::OutputChoice { Auto, Clipboard, Portal, Xdotool, Wtype }`（serde 小写；`ALL`、`label()`）——与阶段二计划 Task 2 完全一致
  - `settings_store::GuiSettings { output, portal_restore_token, x11_hotkey, fingerprint_names: BTreeMap<u8, String> }` + `SWITCH_NAME`、`fingerprint_name(&self, slot) -> String`、`set_fingerprint_name(&mut self, slot, name)`、`clear_fingerprint_names(&mut self)`
  - `settings_store::{path, load_from, save_to, load, save}`
  - `errors::friendly(raw: &str) -> String`

- [ ] **Step 1: Cargo.toml**

`[dependencies]` 追加：

```toml
serde = { version = "1", features = ["derive"] }
serde_json = "1"
async-channel = "2"
```

新增：

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: 写 settings_store 测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn defaults_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let s = load_from(&dir.path().join("gui.json"));
        assert_eq!(s, GuiSettings::default());
        assert_eq!(s.output, OutputChoice::Auto);
        assert_eq!(s.x11_hotkey, "ctrl+backslash");
        assert!(s.fingerprint_names.is_empty());
    }

    #[test]
    fn roundtrip_and_mode_0600() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("gui.json");
        let mut s = GuiSettings::default();
        s.output = OutputChoice::Xdotool;
        s.set_fingerprint_name(0, "Right thumb");
        save_to(&p, &s).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(load_from(&p), s);
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

    #[test]
    fn fingerprint_names() {
        let mut s = GuiSettings::default();
        assert_eq!(s.fingerprint_name(0), "Finger 1");
        assert_eq!(s.fingerprint_name(4), "Finger 5");
        assert_eq!(s.fingerprint_name(SWITCH_SLOT), GuiSettings::SWITCH_NAME);

        s.set_fingerprint_name(0, "  Right thumb ");
        assert_eq!(s.fingerprint_name(0), "Right thumb");
        // The switch slot never takes a name.
        s.set_fingerprint_name(SWITCH_SLOT, "nope");
        assert_eq!(s.fingerprint_name(SWITCH_SLOT), "Switch Host");
        assert!(!s.fingerprint_names.contains_key(&SWITCH_SLOT));
        // Blank clears.
        s.set_fingerprint_name(0, "   ");
        assert_eq!(s.fingerprint_name(0), "Finger 1");

        s.set_fingerprint_name(2, "Left index");
        s.clear_fingerprint_names();
        assert!(s.fingerprint_names.is_empty());
    }

    #[test]
    fn unknown_fields_are_ignored_and_missing_ones_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("gui.json");
        std::fs::write(&p, br#"{"output":"clipboard","future_field":1}"#).unwrap();
        let s = load_from(&p);
        assert_eq!(s.output, OutputChoice::Clipboard);
        assert_eq!(s.x11_hotkey, "ctrl+backslash");
    }
}
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p immurok-gui settings_store`
Expected: 编译失败（模块不存在）

- [ ] **Step 4: 实现 settings_store.rs**

```rust
//! Per-user GUI settings: `~/.config/immurok/gui.json`.
//!
//! Phase 2 (hotkeys / typing backends) reads `output`, `portal_restore_token`
//! and `x11_hotkey` from here; phase 3 adds the local fingerprint names.
//! Nothing in this file is a secret, but the portal token lets a same-uid
//! process skip a permission prompt, so the file is 0600 anyway.
//!
//! Fingerprint names are purely local (the device only knows slot numbers),
//! exactly like macOS keeps them in UserDefaults.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use immurok_client::fingerprint::SWITCH_SLOT;

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
            OutputChoice::Auto => "Automatic",
            OutputChoice::Clipboard => "Clipboard",
            OutputChoice::Portal => "Desktop portal",
            OutputChoice::Xdotool => "xdotool (X11)",
            OutputChoice::Wtype => "wtype (wlroots)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiSettings {
    pub output: OutputChoice,
    pub portal_restore_token: Option<String>,
    pub x11_hotkey: String,
    /// Slot → display name for authentication slots 0–4. Slot 5 (the
    /// host-switch finger) has a fixed name and is never stored.
    pub fingerprint_names: BTreeMap<u8, String>,
}

impl Default for GuiSettings {
    fn default() -> Self {
        Self {
            output: OutputChoice::Auto,
            portal_restore_token: None,
            x11_hotkey: "ctrl+backslash".into(),
            fingerprint_names: BTreeMap::new(),
        }
    }
}

impl GuiSettings {
    pub const SWITCH_NAME: &'static str = "Switch Host";

    pub fn fingerprint_name(&self, slot: u8) -> String {
        if slot == SWITCH_SLOT {
            return Self::SWITCH_NAME.to_string();
        }
        self.fingerprint_names
            .get(&slot)
            .cloned()
            .unwrap_or_else(|| format!("Finger {}", slot + 1))
    }

    /// Blank names clear the entry; the switch slot is ignored.
    pub fn set_fingerprint_name(&mut self, slot: u8, name: &str) {
        if slot == SWITCH_SLOT {
            return;
        }
        let name = name.trim();
        if name.is_empty() {
            self.fingerprint_names.remove(&slot);
        } else {
            self.fingerprint_names.insert(slot, name.to_string());
        }
    }

    pub fn clear_fingerprint_names(&mut self) {
        self.fingerprint_names.clear();
    }
}

pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
            home.join(".config")
        });
    base.join("immurok").join("gui.json")
}

pub fn load_from(path: &Path) -> GuiSettings {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Atomic write (temp file + rename), mode 0600.
pub fn save_to(path: &Path, s: &GuiSettings) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        f.write_all(&json).map_err(|e| e.to_string())?;
    }
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

pub fn load() -> GuiSettings {
    load_from(&path())
}

pub fn save(s: &GuiSettings) -> Result<(), String> {
    save_to(&path(), s)
}
```

- [ ] **Step 5: 写 errors 测试 + 实现**

`crates/immurok-gui/src/errors.rs` 全文：

```rust
//! Daemon error strings → user-facing text.
//!
//! The literals come from `immurok-daemon/src/ble.rs` (`FP-gate timeout` /
//! `FP-gate cancelled` / `FP-gate failed: 0x..`) and `socket.rs`
//! (`ERROR:BUSY`, `ERROR:NOT_CONNECTED`, `ERROR:INVALID_SLOT`,
//! `ERROR:DUAL_HOST_UNSUPPORTED`, `ERROR:SLOT_CLEAR_REFUSED`, and the
//! `ERROR:<KIND>_FAILED:<reason>` wrappers). `Read failed: …` is our own
//! socket read timeout (`immurok-client/src/daemon.rs`).

const WRAPPERS: [&str; 7] = [
    "ERROR:ENROLL_FAILED:",
    "ERROR:DELETE_FAILED:",
    "ERROR:SLOT_CLEAR_FAILED:",
    "ERROR:OTP_FAILED:",
    "ERROR:READ_FAILED:",
    "ERROR:VERIFY_FAILED:",
    "ERROR:FACTORY_RESET_FAILED:",
];

fn map(s: &str) -> Option<&'static str> {
    Some(match s {
        r if r.contains("FP-gate timeout") || r.contains("Read failed") => {
            "Timed out waiting for a fingerprint touch."
        }
        r if r.contains("FP-gate cancelled") => "Cancelled",
        r if r.contains("FP-gate failed") => "Fingerprint didn't match after multiple attempts.",
        r if r.contains("NOT_CONNECTED") => "Device not connected",
        r if r.contains("BUSY") => "Device busy, try again",
        r if r.contains("INVALID_SLOT") => "Invalid slot",
        r if r.contains("DUAL_HOST_UNSUPPORTED") => "This firmware does not support two hosts",
        r if r.contains("SLOT_CLEAR_REFUSED") => "The device refused to clear the slot",
        r if r.contains("PAIRING_IN_PROGRESS") => "Pairing is already in progress",
        _ => return None,
    })
}

/// Best-effort translation; unknown strings pass through verbatim (with the
/// `ERROR:<KIND>_FAILED:` wrapper intact so the raw code stays visible).
pub fn friendly(raw: &str) -> String {
    if let Some(m) = map(raw) {
        return m.to_string();
    }
    for w in WRAPPERS {
        if let Some(inner) = raw.strip_prefix(w) {
            if let Some(m) = map(inner) {
                return m.to_string();
            }
        }
    }
    raw.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_outcomes() {
        assert_eq!(friendly("ERROR:OTP_FAILED:FP-gate timeout"), "Timed out waiting for a fingerprint touch.");
        assert_eq!(friendly("ERROR:READ_FAILED:FP-gate timeout"), "Timed out waiting for a fingerprint touch.");
        assert_eq!(friendly("ERROR:DELETE_FAILED:FP-gate timeout"), "Timed out waiting for a fingerprint touch.");
        assert_eq!(
            friendly("Read failed: Resource temporarily unavailable (os error 11)"),
            "Timed out waiting for a fingerprint touch."
        );
        assert_eq!(friendly("ERROR:ENROLL_FAILED:FP-gate cancelled"), "Cancelled");
        assert_eq!(friendly("ERROR:SLOT_CLEAR_FAILED:FP-gate failed: 0x21"), "Fingerprint didn't match after multiple attempts.");
        assert_eq!(friendly("ERROR:OTP_FAILED:FP-gate failed"), "Fingerprint didn't match after multiple attempts.");
    }

    #[test]
    fn plain_codes() {
        assert_eq!(friendly("ERROR:BUSY"), "Device busy, try again");
        assert_eq!(friendly("ERROR:NOT_CONNECTED"), "Device not connected");
        assert_eq!(friendly("ERROR:INVALID_SLOT"), "Invalid slot");
        assert_eq!(friendly("ERROR:DUAL_HOST_UNSUPPORTED"), "This firmware does not support two hosts");
        assert_eq!(friendly("ERROR:SLOT_CLEAR_REFUSED"), "The device refused to clear the slot");
    }

    #[test]
    fn unknown_passes_through_with_wrapper() {
        assert_eq!(friendly("ERROR:OTP_FAILED:0x21"), "ERROR:OTP_FAILED:0x21");
        assert_eq!(friendly("ERROR:ENROLL_FAILED:0x28"), "ERROR:ENROLL_FAILED:0x28");
    }
}
```

- [ ] **Step 6: quickfill.rs 改用共用映射**

- 顶部加 `use crate::errors::friendly;`
- `Err(e) => p.back_to_listing_with_error(&friendly_error(&e))` 改为 `&friendly(&e)`
- 删除 `fn friendly_error` 及其测试 `friendly_errors`（保留 `gate_timeout_constant_covers_device_gate`）；若 `GATE_TIMEOUT` 因此只在测试里用，保持现有的 `#[cfg(test)] use` 写法不变。

`main.rs` 的模块声明加 `mod errors;` 与 `mod settings_store;`（字母序）。`settings_store` 在 Task 9 之前没有调用者，会触发 dead_code：在 `main.rs` 里对该模块声明加 `#[allow(dead_code)] // consumed by pages::fingerprints (next task)` 并在 Task 9 移除该属性。

- [ ] **Step 7: 编译 + 测试**

Run: `cargo build -p immurok-gui && cargo test -p immurok-gui`
Expected: warning-free；settings_store 6 + errors 3 + filter 2 + quickfill 1 + cli 4 = 16 passed

- [ ] **Step 8: Commit**

```bash
git add crates/immurok-gui Cargo.lock
git commit -m "gui: gui.json 设置存储（含指纹名字）与共用错误映射 errors.rs"
```

---

### Task 7: `gate_dialog.rs` — 触摸等待对话框与倒计时环

**Files:**
- Create: `crates/immurok-gui/src/gate_dialog.rs`
- Modify: `crates/immurok-gui/src/main.rs`（`mod gate_dialog;`）

**Interfaces:**
- Consumes: `immurok_client::keys::cancel_gate`、`crate::pages::run_blocking`、`crate::errors::friendly`
- Produces:
  - `pub const GATE_SECS: f64 = 30.0`
  - `pub struct CountdownRing { pub area: gtk::DrawingArea, .. }` + `new() -> Self`、`start(&self)`、`stop(&self)`
  - `pub async fn run<T: Send + 'static>(parent: &impl IsA<gtk::Window>, title: &str, hint: &str, work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Option<Result<T, String>>`（`None` = 用户取消）

- [ ] **Step 1: 实现**

```rust
//! Modal "touch the device" dialog: a 30 s countdown ring, a hint line and
//! Cancel. The daemon runs the fingerprint gate inside the blocking `work`
//! call; this dialog only visualises the wait and turns Cancel into
//! `GATE:CANCEL`. Reused by delete, Test Fingerprint and unbind-other-host;
//! the enrollment dialog embeds [`CountdownRing`] for its own gate page.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::keys::cancel_gate;

use crate::errors::friendly;
use crate::pages::run_blocking;

/// The device's fingerprint gate (`BLE_FP_GATE_TIMEOUT_SECS`).
pub const GATE_SECS: f64 = 30.0;

/// A ring that drains from full to empty over [`GATE_SECS`].
pub struct CountdownRing {
    pub area: gtk::DrawingArea,
    started: Rc<Cell<Option<Instant>>>,
}

impl CountdownRing {
    pub fn new() -> Self {
        let started: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
        let area = gtk::DrawingArea::builder()
            .content_width(72)
            .content_height(72)
            .halign(gtk::Align::Center)
            .build();
        let s = started.clone();
        area.set_draw_func(move |_, cr, w, h| {
            let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
            let r = (w.min(h) as f64 / 2.0) - 4.0;
            let frac = match s.get() {
                Some(t) => (1.0 - t.elapsed().as_secs_f64() / GATE_SECS).clamp(0.0, 1.0),
                None => 1.0,
            };
            cr.set_line_width(4.0);
            cr.set_source_rgba(0.5, 0.5, 0.5, 0.25);
            cr.arc(cx, cy, r, 0.0, std::f64::consts::TAU);
            let _ = cr.stroke();
            cr.set_source_rgba(0.21, 0.52, 0.89, 1.0);
            let start = -std::f64::consts::FRAC_PI_2;
            cr.arc(cx, cy, r, start, start + std::f64::consts::TAU * frac);
            let _ = cr.stroke();
        });
        Self { area, started }
    }

    /// (Re)start the countdown from full.
    pub fn start(&self) {
        self.started.set(Some(Instant::now()));
        let area = self.area.downgrade();
        let s = self.started.clone();
        glib::timeout_add_local(Duration::from_millis(100), move || {
            let Some(area) = area.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if s.get().is_none() {
                return glib::ControlFlow::Break;
            }
            area.queue_draw();
            glib::ControlFlow::Continue
        });
    }

    pub fn stop(&self) {
        self.started.set(None);
        self.area.queue_draw();
    }
}

impl Default for CountdownRing {
    fn default() -> Self {
        Self::new()
    }
}

/// Show the dialog, run `work` off the main thread, resolve when it returns
/// or the user cancels. On cancel a `GATE:CANCEL` is sent so the daemon
/// stops waiting for the touch.
pub async fn run<T: Send + 'static>(
    parent: &impl IsA<gtk::Window>,
    title: &str,
    hint: &str,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Option<Result<T, String>> {
    let window = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .resizable(false)
        .title(title)
        .default_width(360)
        .build();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(24);
    root.set_margin_bottom(24);
    root.set_margin_start(24);
    root.set_margin_end(24);

    let title_label = gtk::Label::builder().label(title).wrap(true).build();
    title_label.add_css_class("title-2");
    let ring = CountdownRing::new();
    let hint_label = gtk::Label::builder().label(hint).wrap(true).justify(gtk::Justification::Center).build();
    let cancel = gtk::Button::builder().label("Cancel").halign(gtk::Align::Center).build();

    root.append(&title_label);
    root.append(&ring.area);
    root.append(&hint_label);
    root.append(&cancel);
    window.set_child(Some(&root));

    let cancelled = Rc::new(Cell::new(false));
    let c = cancelled.clone();
    let w = window.clone();
    cancel.connect_clicked(move |_| {
        c.set(true);
        std::thread::spawn(cancel_gate);
        w.close();
    });
    let c = cancelled.clone();
    window.connect_close_request(move |_| {
        // Closing via the WM (Alt+F4) is a cancel too; the button path has
        // already set the flag and sent the cancel.
        if !c.get() {
            c.set(true);
            std::thread::spawn(cancel_gate);
        }
        glib::Propagation::Proceed
    });

    window.present();
    ring.start();

    let result = run_blocking(work).await;
    if cancelled.get() {
        // The daemon's reply to a cancelled gate is not interesting.
        return None;
    }
    ring.stop();
    let (text, hold) = match &result {
        Some(Ok(_)) => ("Verified, processing…".to_string(), 300),
        Some(Err(e)) => (friendly(e), 1500),
        None => ("Something went wrong.".to_string(), 1500),
    };
    hint_label.set_text(&text);
    cancel.set_sensitive(false);
    glib::timeout_future(Duration::from_millis(hold)).await;
    // Mark as handled so the close-request below does not send a stray
    // GATE:CANCEL for a gate that already finished.
    cancelled.set(true);
    window.close();
    Some(result.unwrap_or_else(|| Err("worker panicked".into())))
}
```

`main.rs` 加 `mod gate_dialog;`（Task 9 之前没有调用者：加 `#[allow(dead_code)]`，Task 9 移除）。

- [ ] **Step 2: 编译**

Run: `cargo build -p immurok-gui && cargo test -p immurok-gui`
Expected: warning-free

- [ ] **Step 3: Commit**

```bash
git add crates/immurok-gui/src/gate_dialog.rs crates/immurok-gui/src/main.rs
git commit -m "gui: 触摸等待对话框 gate_dialog（30 s 倒计时环 + GATE:CANCEL）"
```

---

### Task 8: `enroll_dialog.rs` — 六步引导登记对话框

**Files:**
- Create: `crates/immurok-gui/src/enroll_dialog.rs`
- Modify: `crates/immurok-gui/src/main.rs`（`mod enroll_dialog;`）

**Interfaces:**
- Consumes: `immurok_client::enroll_session::{run_enrollment, Continue, EnrollProgress}`、`immurok_client::fingerprint::{enroll_cancel, SWITCH_SLOT}`、`immurok_client::enroll_hint::{step_hint, step_arrow}`、`crate::gate_dialog::CountdownRing`、`crate::errors::friendly`
- Produces:
  - `pub enum EnrollOutcome { Enrolled, Failed, Cancelled }`
  - `pub async fn run(parent: &impl IsA<gtk::Window>, slot: u8) -> EnrollOutcome`

- [ ] **Step 1: 实现**

```rust
//! Guided enrollment dialog (spec §10).
//!
//! Page "gate": the device may first ask for an already-enrolled finger —
//! same ring as `gate_dialog`. Page "guide": six-step capture with the macOS
//! step titles / arrows, a progress bar, and an orange flash + shake when
//! the device rejects a frame as too similar (Overlap) — progress does not
//! advance on that. The enrollment itself runs on a worker thread
//! (`run_enrollment`); events arrive over an async channel.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::enroll_hint::{step_arrow, step_hint};
use immurok_client::enroll_session::{run_enrollment, Continue, EnrollProgress};
use immurok_client::fingerprint::{enroll_cancel, SWITCH_SLOT};

use crate::errors::friendly;
use crate::gate_dialog::CountdownRing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollOutcome {
    Enrolled,
    Failed,
    Cancelled,
}

const SHAKE_CSS: &str = r#"
@keyframes immurok-shake {
  0%   { transform: translateX(0); }
  25%  { transform: translateX(-8px); }
  50%  { transform: translateX(8px); }
  75%  { transform: translateX(-8px); }
  100% { transform: translateX(0); }
}
.enroll-card { border-radius: 12px; padding: 18px; }
.enroll-card.overlap {
  animation: immurok-shake 300ms ease-in-out;
  background-color: alpha(@warning_color, 0.25);
}
"#;

fn install_css_once() {
    thread_local! { static DONE: Cell<bool> = const { Cell::new(false) }; }
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_data(SHAKE_CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

struct Guide {
    card: gtk::Box,
    step_title: gtk::Label,
    arrow: gtk::Label,
    progress: gtk::ProgressBar,
    subtext: gtk::Label,
    error: gtk::Label,
}

impl Guide {
    fn build() -> Self {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 10);
        card.add_css_class("card");
        card.add_css_class("enroll-card");
        let step_title = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).build();
        step_title.add_css_class("title-2");
        let arrow = gtk::Label::builder().label("·").build();
        arrow.add_css_class("title-1");
        let progress = gtk::ProgressBar::builder().show_text(true).build();
        let subtext = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).build();
        let error = gtk::Label::builder().wrap(true).visible(false).justify(gtk::Justification::Center).build();
        error.add_css_class("error");
        card.append(&step_title);
        card.append(&arrow);
        card.append(&progress);
        card.append(&subtext);
        card.append(&error);
        Self { card, step_title, arrow, progress, subtext, error }
    }

    fn set_step(&self, next_step: u8, captured: u8, total: u8) {
        self.step_title.set_text(step_hint(next_step));
        let a = step_arrow(next_step);
        self.arrow.set_text(a);
        self.arrow.set_visible((2..=5).contains(&next_step));
        let total = total.max(1);
        self.progress.set_fraction(captured as f64 / total as f64);
        self.progress.set_text(Some(&format!("Captured ({captured}/{total})")));
    }

    /// Orange flash + shake; removing and re-adding the class restarts the
    /// CSS animation.
    fn shake(&self) {
        self.card.remove_css_class("overlap");
        let card = self.card.clone();
        glib::idle_add_local_once(move || {
            card.add_css_class("overlap");
            let card = card.clone();
            glib::timeout_add_local_once(Duration::from_millis(400), move || {
                card.remove_css_class("overlap");
            });
        });
    }
}

/// Run a full enrollment of `slot` in a modal dialog. Resolves when the
/// dialog closes.
pub async fn run(parent: &impl IsA<gtk::Window>, slot: u8) -> EnrollOutcome {
    install_css_once();

    let is_switch = slot == SWITCH_SLOT;
    let window = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .resizable(false)
        .title(if is_switch { "Add switch fingerprint" } else { "Add Fingerprint" })
        .default_width(420)
        .build();

    let stack = gtk::Stack::new();
    stack.set_margin_top(24);
    stack.set_margin_bottom(24);
    stack.set_margin_start(24);
    stack.set_margin_end(24);

    // ── gate page ──
    let gate = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let ring = CountdownRing::new();
    let gate_hint = gtk::Label::builder()
        .label("Verify with an enrolled finger")
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    gate.append(&ring.area);
    gate.append(&gate_hint);
    stack.add_named(&gate, Some("gate"));

    // ── guide page ──
    let guide = Guide::build();
    let subtitle = gtk::Label::builder()
        .label(if is_switch {
            "This finger will only switch between the two hosts."
        } else {
            "Press the sensor six times, shifting slightly each time."
        })
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    subtitle.add_css_class("dim-label");
    let guide_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    guide_box.append(&subtitle);
    guide_box.append(&guide.card);
    stack.add_named(&guide_box, Some("guide"));

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::Center);
    buttons.set_margin_bottom(18);
    let cancel = gtk::Button::builder().label("Cancel").build();
    let close = gtk::Button::builder().label("Close").visible(false).build();
    close.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&close);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&stack);
    root.append(&buttons);
    window.set_child(Some(&root));
    stack.set_visible_child_name("gate");

    // ── worker ──
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let (tx, rx) = async_channel::unbounded::<EnrollProgress>();
    {
        let flag = cancel_flag.clone();
        std::thread::spawn(move || {
            run_enrollment(slot, |ev| {
                let _ = tx.send_blocking(ev);
                if flag.load(Ordering::Relaxed) {
                    Continue::Stop
                } else {
                    Continue::Go
                }
            });
        });
    }

    // Cancel = flag for the poll loop + FP:ENROLL_CANCEL right away, which
    // also unblocks a gate wait inside `enroll_start`.
    let user_cancelled = Rc::new(Cell::new(false));
    let finished = Rc::new(Cell::new(false));
    {
        let flag = cancel_flag.clone();
        let uc = user_cancelled.clone();
        let w = window.clone();
        cancel.connect_clicked(move |_| {
            uc.set(true);
            flag.store(true, Ordering::Relaxed);
            std::thread::spawn(enroll_cancel);
            w.close();
        });
    }
    {
        let flag = cancel_flag.clone();
        let uc = user_cancelled.clone();
        let fin = finished.clone();
        window.connect_close_request(move |_| {
            if !fin.get() && !uc.get() {
                uc.set(true);
                flag.store(true, Ordering::Relaxed);
                std::thread::spawn(enroll_cancel);
            }
            glib::Propagation::Proceed
        });
    }
    {
        let w = window.clone();
        close.connect_clicked(move |_| w.close());
    }

    window.present();

    let mut outcome = EnrollOutcome::Cancelled;
    while let Ok(ev) = rx.recv().await {
        if user_cancelled.get() {
            break;
        }
        match ev {
            EnrollProgress::GateWaiting => {
                stack.set_visible_child_name("gate");
                ring.start();
            }
            EnrollProgress::Started => {
                ring.stop();
                stack.set_visible_child_name("guide");
                guide.set_step(1, 0, 6);
                guide.subtext.set_text("Place your finger on the sensor...");
            }
            EnrollProgress::Step { next_step, captured, total } => {
                guide.set_step(next_step, captured, total);
                guide.subtext.set_text(if captured == 0 {
                    "Place your finger on the sensor..."
                } else {
                    "Lift your finger, then press again..."
                });
            }
            EnrollProgress::LiftFinger => guide.subtext.set_text("Lift your finger, then press again..."),
            EnrollProgress::Overlap => {
                guide.subtext.set_text("Too similar — shift your finger and press again");
                guide.shake();
            }
            EnrollProgress::Processing => guide.subtext.set_text("Processing..."),
            EnrollProgress::Complete => {
                outcome = EnrollOutcome::Enrolled;
                break;
            }
            EnrollProgress::Failed(reason) => {
                if reason.contains("FP-gate cancelled") {
                    // Our own cancel racing the gate — not a failure.
                    outcome = EnrollOutcome::Cancelled;
                    break;
                }
                outcome = EnrollOutcome::Failed;
                let text = if reason.contains("disconnected") || reason.contains("NOT_CONNECTED") {
                    "Device disconnected before enrollment finished. Reconnect the device and try again.".to_string()
                } else if reason.contains("FP-gate") {
                    friendly(&reason)
                } else {
                    "Enrollment failed. Please try again.".to_string()
                };
                // The session is over: closing the window from here must
                // not send another FP:ENROLL_CANCEL.
                finished.set(true);
                ring.stop();
                stack.set_visible_child_name("guide");
                guide.error.set_text(&text);
                guide.error.set_visible(true);
                cancel.set_visible(false);
                close.set_visible(true);
                // Wait for the user to dismiss.
                let (done_tx, done_rx) = async_channel::bounded::<()>(1);
                {
                    let done_tx = done_tx.clone();
                    close.connect_clicked(move |_| {
                        let _ = done_tx.try_send(());
                    });
                }
                window.connect_close_request(move |_| {
                    let _ = done_tx.try_send(());
                    glib::Propagation::Proceed
                });
                let _ = done_rx.recv().await;
                break;
            }
        }
    }
    finished.set(true);
    window.close();
    outcome
}
```

`main.rs` 加 `mod enroll_dialog;`（Task 9 之前无调用者：`#[allow(dead_code)]`，Task 9 移除）。

- [ ] **Step 2: 编译**

Run: `cargo build -p immurok-gui && cargo test -p immurok-gui`
Expected: warning-free

- [ ] **Step 3: Commit**

```bash
git add crates/immurok-gui/src/enroll_dialog.rs crates/immurok-gui/src/main.rs
git commit -m "gui: 六步引导登记对话框 enroll_dialog（门控页 + 引导页 + overlap 抖动）"
```

---

### Task 9: Fingerprints 页

**Files:**
- Create: `crates/immurok-gui/src/pages/fingerprints.rs`
- Modify: `crates/immurok-gui/src/pages/mod.rs`（`pub mod fingerprints;`）
- Modify: `crates/immurok-gui/src/main_window.rs`（挂第三页）
- Modify: `crates/immurok-gui/src/main.rs`（移除 Task 6–8 加的三个 `#[allow(dead_code)]`）

**Interfaces:**
- Consumes: `immurok_client::fingerprint::{fp_delete, fp_list, fp_verify, FpSlots, SWITCH_SLOT}`、`immurok_client::hosts::slot_status`、`immurok_client::status::query_status`、`crate::enroll_dialog::{run, EnrollOutcome}`、`crate::gate_dialog`、`crate::settings_store`、`crate::errors::friendly`、`pages::{confirm, run_blocking}`
- Produces: `pages::fingerprints::FingerprintsPage { pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self>, pub fn widget(&self) -> &gtk::Widget, pub fn start(self: &Rc<Self>, window: &adw::ApplicationWindow) }`

- [ ] **Step 1: 实现 `pages/fingerprints.rs`**

```rust
//! Fingerprints page (spec §8): one card per enrolled slot with a local,
//! editable name; "+" to enroll; slot 5 is the fixed "Switch Host" finger.
//! Delete / Test go through the device's fingerprint gate.
//!
//! Connection state is polled here (every 2 s while the page is showing)
//! independently of the Dashboard, so the page reloads when the device
//! comes and goes.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::fingerprint::{fp_delete, fp_list, fp_verify, FpSlots, SWITCH_SLOT};
use immurok_client::hosts::slot_status;
use immurok_client::status::query_status;

use crate::enroll_dialog::{self, EnrollOutcome};
use crate::errors::friendly;
use crate::gate_dialog;
use crate::settings_store;

use super::{confirm, run_blocking};

pub struct FingerprintsPage {
    /// "loading" | "disconnected" | "content"
    root: gtk::Stack,
    flow: gtk::FlowBox,
    add_switch: gtk::Button,
    test_button: gtk::Button,
    refresh_button: gtk::Button,
    toasts: adw::ToastOverlay,
    slots: Cell<FpSlots>,
    hosts_supported: Cell<bool>,
    connected: Cell<bool>,
    /// A gated / enrollment session is running: every action disabled,
    /// polling paused.
    busy: Cell<bool>,
    loading: Cell<bool>,
    have_data: Cell<bool>,
}

fn finger_icon_name() -> &'static str {
    let has = gtk::gdk::Display::default()
        .map(|d| gtk::IconTheme::for_display(&d).has_icon("fingerprint-symbolic"))
        .unwrap_or(false);
    if has {
        "fingerprint-symbolic"
    } else {
        "dialog-password-symbolic"
    }
}

impl FingerprintsPage {
    pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self> {
        // ── content ──
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .title("Fingerprints")
            .description("immurok lets you use your fingerprint to unlock this computer and authorize sudo.")
            .build();
        let flow = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .max_children_per_line(4)
            .min_children_per_line(2)
            .column_spacing(12)
            .row_spacing(12)
            .homogeneous(true)
            .build();
        group.add(&flow);
        page.add(&group);

        let actions = adw::PreferencesGroup::new();
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_halign(gtk::Align::Start);
        let add_switch = gtk::Button::builder().label("Add switch fingerprint").visible(false).build();
        let test_button = gtk::Button::builder().label("Test Fingerprint").build();
        let refresh_button = gtk::Button::builder().label("Refresh").build();
        buttons.append(&add_switch);
        buttons.append(&test_button);
        buttons.append(&refresh_button);
        actions.add(&buttons);
        page.add(&actions);

        // ── loading / disconnected ──
        let loading = gtk::Box::new(gtk::Orientation::Vertical, 12);
        loading.set_valign(gtk::Align::Center);
        let spinner = gtk::Spinner::new();
        spinner.set_spinning(true);
        loading.append(&spinner);
        loading.append(&gtk::Label::new(Some("Fetching fingerprint info from device...")));

        let disconnected = adw::StatusPage::builder()
            .icon_name("bluetooth-disabled-symbolic")
            .title("Device not connected")
            .description("Connect the device to manage fingerprints.")
            .build();

        let root = gtk::Stack::new();
        root.add_named(&loading, Some("loading"));
        root.add_named(&disconnected, Some("disconnected"));
        root.add_named(&page, Some("content"));
        root.set_visible_child_name("loading");

        let this = Rc::new(Self {
            root,
            flow,
            add_switch,
            test_button,
            refresh_button,
            toasts: toasts.clone(),
            slots: Cell::new(FpSlots::default()),
            hosts_supported: Cell::new(false),
            connected: Cell::new(false),
            busy: Cell::new(false),
            loading: Cell::new(false),
            have_data: Cell::new(false),
        });

        let weak = Rc::downgrade(&this);
        this.add_switch.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.add(SWITCH_SLOT);
            }
        });
        let weak = Rc::downgrade(&this);
        this.test_button.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.test();
            }
        });
        let weak = Rc::downgrade(&this);
        this.refresh_button.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.load();
            }
        });
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    /// Initial load, then a 2 s connection poll while the window is visible
    /// and this page is showing. Call once after construction.
    pub fn start(self: &Rc<Self>, window: &adw::ApplicationWindow) {
        self.load();
        let weak = Rc::downgrade(self);
        let window = window.downgrade();
        glib::timeout_add_local(Duration::from_secs(2), move || {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let Some(window) = window.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if window.is_visible() && this.root.is_mapped() && !this.busy.get() && !this.loading.get() {
                let weak = Rc::downgrade(&this);
                glib::spawn_future_local(async move {
                    let r = run_blocking(query_status).await;
                    let Some(this) = weak.upgrade() else { return };
                    match r {
                        Some(Ok(s)) if s.connected != this.connected.get() => this.load(),
                        Some(Err(_)) if this.connected.get() => this.load(),
                        _ => {}
                    }
                });
            }
            glib::ControlFlow::Continue
        });
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    fn window(&self) -> Option<gtk::Window> {
        self.root.root().and_then(|r| r.downcast::<gtk::Window>().ok())
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.update_buttons();
    }

    fn update_buttons(&self) {
        let slots = self.slots.get();
        let free = self.connected.get() && !self.busy.get() && !self.loading.get();
        self.add_switch.set_visible(self.hosts_supported.get() && !slots.switch_enrolled());
        self.add_switch.set_sensitive(free);
        self.test_button.set_sensitive(free && slots.any());
        self.refresh_button.set_sensitive(!self.busy.get() && !self.loading.get());
        // The "+" card is rebuilt by populate(); flip its sensitivity here.
        if let Some(add) = self.flow.last_child().and_then(|c| c.first_child()) {
            add.set_sensitive(free && slots.first_free_auth_slot().is_some());
        }
    }

    fn load(self: &Rc<Self>) {
        if self.loading.replace(true) {
            return;
        }
        if !self.have_data.get() {
            self.root.set_visible_child_name("loading");
        }
        self.update_buttons();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let r = run_blocking(|| {
                let slots = fp_list();
                let hosts = slot_status().map(|h| h.supported).unwrap_or(false);
                (slots, hosts)
            })
            .await;
            let Some(this) = weak.upgrade() else { return };
            this.loading.set(false);
            match r {
                Some((Ok(slots), hosts)) => {
                    this.connected.set(true);
                    this.have_data.set(true);
                    this.slots.set(slots);
                    this.hosts_supported.set(hosts);
                    this.populate();
                    this.root.set_visible_child_name("content");
                }
                Some((Err(e), _)) => {
                    this.connected.set(false);
                    this.have_data.set(false);
                    if !e.contains("NOT_CONNECTED") {
                        this.toast(&format!("Daemon unavailable: {}", glib::markup_escape_text(&e)));
                    }
                    this.root.set_visible_child_name("disconnected");
                }
                None => this.loading.set(false),
            }
            this.update_buttons();
        });
    }

    fn populate(self: &Rc<Self>) {
        while let Some(child) = self.flow.first_child() {
            self.flow.remove(&child);
        }
        let settings = settings_store::load();
        let slots = self.slots.get();
        for slot in slots.auth_slots() {
            let card = self.finger_card(slot, &settings.fingerprint_name(slot));
            self.flow.insert(&card, -1);
        }
        if slots.switch_enrolled() {
            let card = self.finger_card(SWITCH_SLOT, settings_store::GuiSettings::SWITCH_NAME);
            self.flow.insert(&card, -1);
        }
        self.flow.insert(&self.add_card(), -1);
        self.update_buttons();
    }

    fn finger_card(self: &Rc<Self>, slot: u8, name: &str) -> gtk::Widget {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
        card.add_css_class("card");
        card.set_margin_top(12);
        card.set_margin_bottom(12);
        card.set_margin_start(12);
        card.set_margin_end(12);
        let inner = gtk::Box::new(gtk::Orientation::Vertical, 6);
        inner.set_margin_top(14);
        inner.set_margin_bottom(14);
        inner.set_margin_start(14);
        inner.set_margin_end(14);
        card.append(&inner);

        let icon = gtk::Image::from_icon_name(finger_icon_name());
        icon.set_pixel_size(40);
        icon.add_css_class("accent");
        inner.append(&icon);

        // Name: label ↔ entry (inline rename), except for the switch slot.
        let name_stack = gtk::Stack::new();
        let label = gtk::Label::builder()
            .label(name)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(14)
            .build();
        let entry = gtk::Entry::builder().text(name).max_width_chars(14).build();
        name_stack.add_named(&label, Some("label"));
        name_stack.add_named(&entry, Some("entry"));
        name_stack.set_visible_child_name("label");
        inner.append(&name_stack);

        if slot == SWITCH_SLOT {
            let hint = gtk::Label::builder()
                .label("This finger only switches hosts — it never unlocks or authenticates")
                .wrap(true)
                .justify(gtk::Justification::Center)
                .build();
            hint.add_css_class("dim-label");
            hint.add_css_class("caption");
            inner.append(&hint);
        } else {
            let click = gtk::GestureClick::new();
            let (ns, en) = (name_stack.clone(), entry.clone());
            click.connect_released(move |_, _, _, _| {
                ns.set_visible_child_name("entry");
                en.grab_focus();
            });
            label.add_controller(click);

            let weak = Rc::downgrade(self);
            let (ns, lb) = (name_stack.clone(), label.clone());
            entry.connect_activate(move |e| {
                let text = e.text().to_string();
                if let Some(p) = weak.upgrade() {
                    let shown = p.rename(slot, &text);
                    lb.set_text(&shown);
                    e.set_text(&shown);
                }
                ns.set_visible_child_name("label");
            });
            let keys = gtk::EventControllerKey::new();
            let (ns, lb, en) = (name_stack.clone(), label.clone(), entry.clone());
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    en.set_text(&lb.text());
                    ns.set_visible_child_name("label");
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            entry.add_controller(keys);
            let focus = gtk::EventControllerFocus::new();
            let (ns, lb, en) = (name_stack.clone(), label.clone(), entry.clone());
            focus.connect_leave(move |_| {
                en.set_text(&lb.text());
                ns.set_visible_child_name("label");
            });
            entry.add_controller(focus);
        }

        // Hover-revealed delete button in the top-right corner.
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&card));
        let trash = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .visible(false)
            .tooltip_text("Delete (requires a touch on the device)")
            .build();
        trash.add_css_class("destructive-action");
        trash.add_css_class("circular");
        overlay.add_overlay(&trash);
        let motion = gtk::EventControllerMotion::new();
        let t = trash.clone();
        motion.connect_enter(move |_, _, _| t.set_visible(true));
        let t = trash.clone();
        motion.connect_leave(move |_| t.set_visible(false));
        overlay.add_controller(motion);

        let weak = Rc::downgrade(self);
        let name = name.to_string();
        trash.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.delete(slot, name.clone());
            }
        });

        overlay.upcast()
    }

    fn add_card(self: &Rc<Self>) -> gtk::Widget {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.set_margin_top(14);
        content.set_margin_bottom(14);
        content.set_margin_start(14);
        content.set_margin_end(14);
        let icon = gtk::Image::from_icon_name("list-add-symbolic");
        icon.set_pixel_size(40);
        content.append(&icon);
        content.append(&gtk::Label::new(Some("Add Fingerprint")));
        let button = gtk::Button::builder().child(&content).build();
        button.add_css_class("card");
        button.add_css_class("flat");
        button.set_margin_top(12);
        button.set_margin_bottom(12);
        button.set_margin_start(12);
        button.set_margin_end(12);
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                if let Some(slot) = p.slots.get().first_free_auth_slot() {
                    p.add(slot);
                }
            }
        });
        button.upcast()
    }

    /// Persist a new name; returns the name that is now displayed.
    fn rename(&self, slot: u8, text: &str) -> String {
        let mut s = settings_store::load();
        s.set_fingerprint_name(slot, text);
        if let Err(e) = settings_store::save(&s) {
            self.toast(&format!("Could not save the name: {}", glib::markup_escape_text(&e)));
        }
        s.fingerprint_name(slot)
    }

    fn add(self: &Rc<Self>, slot: u8) {
        if self.busy.get() {
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let Some(win) = this.window() else { return };
            if this.slots.get().any() {
                let ok = confirm(
                    &win,
                    "Add a New Fingerprint",
                    "The device first asks you to verify with an already-enrolled finger. After that, switch to the NEW finger.",
                    "Start",
                )
                .await;
                if !ok {
                    return;
                }
            }
            this.set_busy(true);
            let outcome = enroll_dialog::run(&win, slot).await;
            this.set_busy(false);
            match outcome {
                EnrollOutcome::Enrolled => {
                    this.toast("Fingerprint enrolled successfully!");
                    this.load();
                }
                EnrollOutcome::Failed => this.load(),
                EnrollOutcome::Cancelled => {}
            }
        });
    }

    fn delete(self: &Rc<Self>, slot: u8, name: String) {
        if self.busy.get() {
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let Some(win) = this.window() else { return };
            let body = if slot == SWITCH_SLOT {
                "After this you can no longer switch between computers by touch."
            } else {
                "This cannot be undone. You will need to enroll it again."
            };
            if !confirm(&win, &format!("Delete \"{name}\"?"), body, "Delete").await {
                return;
            }
            this.set_busy(true);
            let r = gate_dialog::run(&win, &format!("Delete \"{name}\""), "Verify with an enrolled finger", move || {
                fp_delete(slot)
            })
            .await;
            this.set_busy(false);
            match r {
                Some(Ok(())) => {
                    this.toast("Deleted");
                    let mut s = settings_store::load();
                    s.set_fingerprint_name(slot, "");
                    let _ = settings_store::save(&s);
                    this.load();
                }
                Some(Err(e)) => this.toast(&glib::markup_escape_text(&friendly(&e))),
                None => {}
            }
        });
    }

    fn test(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let Some(win) = this.window() else { return };
            this.set_busy(true);
            let r = gate_dialog::run(&win, "Test Fingerprint", "Touch the sensor with an enrolled finger", fp_verify).await;
            this.set_busy(false);
            match r {
                Some(Ok(true)) => this.toast("Fingerprint matched"),
                Some(Ok(false)) => this.toast("No match"),
                Some(Err(e)) => this.toast(&glib::markup_escape_text(&friendly(&e))),
                None => {}
            }
        });
    }
}
```

- [ ] **Step 2: 挂到主窗口**

`pages/mod.rs` 加 `pub mod fingerprints;`。`main_window.rs` 在 keys 页之后加：

```rust
        let fingerprints = pages::fingerprints::FingerprintsPage::new(&toasts);
        stack
            .add_titled(fingerprints.widget(), Some("fingerprints"), "Fingerprints")
            .set_icon_name(Some("fingerprint-symbolic"));
```

在 `dashboard.start_polling(&window);` 之后加 `fingerprints.start(&window);`，在两个 `set_data` 之后加 `unsafe { window.set_data("fingerprints-page", fingerprints) };`。

`main.rs` 移除 `settings_store` / `gate_dialog` / `enroll_dialog` 上的 `#[allow(dead_code)]`。

- [ ] **Step 3: 编译 + 启动检查**

Run: `cargo build -p immurok-gui && cargo test -p immurok-gui && cargo clippy -p immurok-gui`
Expected: warning-free。

启动：`timeout 15 target/debug/immurok-gui 2>fp.log`，stderr 为空（无 GTK-CRITICAL）。人工验收（真机）：Fingerprints 页显示与 `immurok-cli fp list` 一致的卡片；点名字可改名、重开窗口仍在；Add 走完六步；同位置连按出现橙闪抖动；删除走门控；Test 匹配 / 不匹配；断开设备后页面切到 "Device not connected"，重连 2 s 内恢复。

- [ ] **Step 4: Commit**

```bash
git add crates/immurok-gui
git commit -m "gui: Fingerprints 页——槽位卡片、改名、登记、删除、Test"
```

---

### Task 10: Device 页的 Two Hosts 分组

**Files:**
- Create: `crates/immurok-gui/src/pages/hosts.rs`
- Modify: `crates/immurok-gui/src/pages/mod.rs`（`pub mod hosts;`）
- Modify: `crates/immurok-gui/src/pages/dashboard.rs`

**Interfaces:**
- Consumes: `immurok_client::hosts::{clear_other_slot, clear_own_slot, pair_progress, pair_start, slot_status, HostSlots}`、`immurok_common::types::PairProgress`、`immurok_client::status::DeviceStatus`、`crate::gate_dialog`、`crate::settings_store`、`crate::errors::friendly`、`pages::{confirm, run_blocking}`
- Produces: `pages::hosts::HostsGroup { pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self>, pub fn widget(&self) -> &gtk::Widget, pub fn apply(&self, status: &DeviceStatus, paired: bool, slots: Option<HostSlots>), pub fn apply_daemon_down(&self) }`

- [ ] **Step 1: 实现 `pages/hosts.rs`**

```rust
//! "Two Hosts" group on the Device page (spec §11): one card per host slot,
//! Pair / Unpair on this computer's slot, gated Unbind on the other one.
//! Firmware without dual-host support (or a disconnected device) falls back
//! to a single Pair / Unpair button.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::hosts::{clear_other_slot, clear_own_slot, pair_progress, pair_start, HostSlots};
use immurok_client::status::DeviceStatus;
use immurok_common::types::PairProgress;

use crate::errors::friendly;
use crate::gate_dialog;
use crate::settings_store;

use super::{confirm, run_blocking};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Pair,
    Unpair,
    Unbind(u8),
}

struct HostCard {
    root: gtk::Box,
    icon: gtk::Image,
    badge: gtk::Label,
    state: gtk::Label,
    action: gtk::Button,
    other_hint: gtk::Label,
    pending: Cell<Option<Action>>,
}

impl HostCard {
    fn build(n: u8) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.add_css_class("card");
        root.set_hexpand(true);
        let inner = gtk::Box::new(gtk::Orientation::Vertical, 6);
        inner.set_margin_top(14);
        inner.set_margin_bottom(14);
        inner.set_margin_start(14);
        inner.set_margin_end(14);
        root.append(&inner);

        let icon = gtk::Image::from_icon_name("computer-symbolic");
        icon.set_pixel_size(36);
        let title = gtk::Label::new(Some(&format!("Host {n}")));
        title.add_css_class("heading");
        let badge = gtk::Label::builder().label("This computer").visible(false).build();
        badge.add_css_class("caption");
        badge.add_css_class("accent");
        let state = gtk::Label::new(Some("○ Empty"));
        state.add_css_class("dim-label");
        let action = gtk::Button::builder().visible(false).halign(gtk::Align::Center).build();
        let other_hint = gtk::Label::builder()
            .label("To fill this slot, open immurok on that computer and click Pair.")
            .wrap(true)
            .justify(gtk::Justification::Center)
            .visible(false)
            .build();
        other_hint.add_css_class("dim-label");
        other_hint.add_css_class("caption");
        inner.append(&icon);
        inner.append(&title);
        inner.append(&badge);
        inner.append(&state);
        inner.append(&action);
        inner.append(&other_hint);
        Self { root, icon, badge, state, action, other_hint, pending: Cell::new(None) }
    }

    fn show(&self, bound: bool, mine: bool, this_computer: bool) {
        self.badge.set_visible(this_computer);
        self.state.set_text(if bound { "● Bound" } else { "○ Empty" });
        if bound {
            self.icon.add_css_class("accent");
        } else {
            self.icon.remove_css_class("accent");
        }
        let action = match (mine, bound) {
            (true, true) => Some(Action::Unpair),
            (true, false) => Some(Action::Pair),
            (false, true) => None, // filled in by caller with the slot number
            (false, false) => None,
        };
        self.pending.set(action);
        self.action.set_visible(action.is_some());
        self.other_hint.set_visible(!mine && !bound);
        if let Some(a) = action {
            self.action.set_label(match a {
                Action::Pair => "Pair",
                Action::Unpair => "Unpair",
                Action::Unbind(_) => "Unbind",
            });
        }
    }

    fn show_unbind(&self, n: u8) {
        self.pending.set(Some(Action::Unbind(n)));
        self.action.set_label("Unbind");
        self.action.set_visible(true);
        self.other_hint.set_visible(false);
    }
}

pub struct HostsGroup {
    group: adw::PreferencesGroup,
    /// "simple" (label + one button) | "cards"
    stack: gtk::Stack,
    simple_label: gtk::Label,
    simple_button: gtk::Button,
    cards: [HostCard; 2],
    hint: gtk::Label,
    progress: gtk::Label,
    toasts: adw::ToastOverlay,
    paired: Cell<bool>,
    connected: Cell<bool>,
    slots: Cell<Option<HostSlots>>,
    busy: Cell<bool>,
    simple_pending: Cell<Option<Action>>,
}

impl HostsGroup {
    pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self> {
        let group = adw::PreferencesGroup::builder().title("Two Hosts").build();

        let simple = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let simple_label = gtk::Label::builder().wrap(true).xalign(0.0).build();
        simple_label.add_css_class("dim-label");
        let simple_button = gtk::Button::builder().label("Pair").halign(gtk::Align::Start).visible(false).build();
        simple.append(&simple_label);
        simple.append(&simple_button);

        let cards_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        cards_box.set_homogeneous(true);
        let cards = [HostCard::build(1), HostCard::build(2)];
        cards_box.append(&cards[0].root);
        cards_box.append(&cards[1].root);

        let stack = gtk::Stack::new();
        stack.add_named(&simple, Some("simple"));
        stack.add_named(&cards_box, Some("cards"));
        stack.set_visible_child_name("simple");

        let hint = gtk::Label::builder().wrap(true).xalign(0.0).build();
        hint.add_css_class("dim-label");
        hint.add_css_class("caption");
        let progress = gtk::Label::builder().wrap(true).xalign(0.0).visible(false).build();
        progress.add_css_class("accent");

        let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
        column.append(&stack);
        column.append(&progress);
        column.append(&hint);
        group.add(&column);

        let this = Rc::new(Self {
            group,
            stack,
            simple_label,
            simple_button,
            cards,
            hint,
            progress,
            toasts: toasts.clone(),
            paired: Cell::new(false),
            connected: Cell::new(false),
            slots: Cell::new(None),
            busy: Cell::new(false),
            simple_pending: Cell::new(None),
        });

        for (i, card) in this.cards.iter().enumerate() {
            let weak = Rc::downgrade(&this);
            card.action.connect_clicked(move |_| {
                if let Some(g) = weak.upgrade() {
                    if let Some(a) = g.cards[i].pending.get() {
                        g.run(a);
                    }
                }
            });
        }
        let weak = Rc::downgrade(&this);
        this.simple_button.connect_clicked(move |_| {
            if let Some(g) = weak.upgrade() {
                if let Some(a) = g.simple_pending.get() {
                    g.run(a);
                }
            }
        });
        this.simple_label.set_text("Connecting to daemon…");
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.group.upcast_ref()
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    fn window(&self) -> Option<gtk::Window> {
        self.group.root().and_then(|r| r.downcast::<gtk::Window>().ok())
    }

    /// Feed the Dashboard's poll result. Skipped while an operation is
    /// running so a tick cannot stomp the buttons mid-flight.
    pub fn apply(&self, status: &DeviceStatus, paired: bool, slots: Option<HostSlots>) {
        self.paired.set(paired);
        self.connected.set(status.connected);
        self.slots.set(slots);
        if self.busy.get() {
            return;
        }
        self.render();
    }

    pub fn apply_daemon_down(&self) {
        self.connected.set(false);
        self.slots.set(None);
        if self.busy.get() {
            return;
        }
        self.stack.set_visible_child_name("simple");
        self.simple_label.set_text("Daemon unavailable.");
        self.simple_button.set_visible(false);
        self.hint.set_text("");
    }

    fn render(&self) {
        let paired = self.paired.get();
        let connected = self.connected.get();
        let slots = self.slots.get();

        let simple_mode = |label: &str, action: Option<Action>| {
            self.stack.set_visible_child_name("simple");
            self.simple_label.set_text(label);
            self.simple_pending.set(action);
            self.simple_button.set_visible(action.is_some());
            self.simple_button.set_label(match action {
                Some(Action::Unpair) => "Unpair",
                _ => "Pair",
            });
            self.hint.set_text("");
        };

        if !connected {
            simple_mode(
                "Device not connected. Host binding status will appear once connected.",
                paired.then_some(Action::Unpair),
            );
            return;
        }
        let Some(slots) = slots else {
            simple_mode(
                "Host status unavailable.",
                Some(if paired { Action::Unpair } else { Action::Pair }),
            );
            return;
        };
        if !slots.supported {
            simple_mode(
                "This firmware does not support two hosts.",
                Some(if paired { Action::Unpair } else { Action::Pair }),
            );
            return;
        }

        // The slot this computer owns, or — before pairing — the slot the
        // device is presenting, which is where pairing will land.
        let mine_or_target = slots.mine.unwrap_or(slots.active);
        for (i, card) in self.cards.iter().enumerate() {
            let n = (i + 1) as u8;
            let bound = slots.bound(n);
            let mine = n == mine_or_target;
            let this_computer = slots.mine == Some(n) || (paired && slots.mine.is_none() && slots.active == n);
            card.show(bound, mine, this_computer);
            if !mine && bound {
                card.show_unbind(n);
            }
        }
        self.stack.set_visible_child_name("cards");
        self.hint.set_text(if slots.both_bound() {
            "Both host slots are in use. To swap one out, unbind it on that computer first."
        } else {
            "This device can be bound to up to two computers. Touch the switch fingerprint to move between them."
        });
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        for c in &self.cards {
            c.action.set_sensitive(!busy);
        }
        self.simple_button.set_sensitive(!busy);
        if !busy {
            self.render();
        }
    }

    fn run(self: &Rc<Self>, action: Action) {
        if self.busy.get() {
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let Some(win) = this.window() else { return };
            match action {
                Action::Unpair => this.unpair(&win).await,
                Action::Pair => this.pair().await,
                Action::Unbind(n) => this.unbind(&win, n).await,
            }
        });
    }

    async fn unpair(self: &Rc<Self>, win: &gtk::Window) {
        if !confirm(win, "Unpair from this computer?", "Fingerprints and keys stay on the device.", "Unpair").await {
            return;
        }
        self.set_busy(true);
        let r = run_blocking(clear_own_slot).await;
        match r {
            Some(Ok(())) => {
                self.toast("Unpaired");
                let mut s = settings_store::load();
                s.clear_fingerprint_names();
                let _ = settings_store::save(&s);
            }
            Some(Err(e)) => self.toast(&format!("Unpair failed: {}", glib::markup_escape_text(&friendly(&e)))),
            None => {}
        }
        self.set_busy(false);
    }

    async fn pair(self: &Rc<Self>) {
        self.set_busy(true);
        self.progress.set_text("Waiting for the device…");
        self.progress.set_visible(true);
        self.toast("Confirm pairing on the device (up to 150 s)");

        // Progress poller on a second connection (PAIR:START blocks its own).
        let stop = Rc::new(Cell::new(false));
        {
            let weak = Rc::downgrade(self);
            let stop = stop.clone();
            glib::spawn_future_local(async move {
                let mut last: Option<PairProgress> = None;
                while !stop.get() {
                    glib::timeout_future(Duration::from_millis(300)).await;
                    if stop.get() {
                        break;
                    }
                    let p = run_blocking(pair_progress).await;
                    let Some(this) = weak.upgrade() else { break };
                    if let Some(Ok(p)) = p {
                        if last != Some(p) {
                            last = Some(p);
                            this.progress.set_text(match p {
                                PairProgress::Idle => "Waiting for the device…",
                                PairProgress::WaitFp => "Touch an enrolled finger on the device",
                                PairProgress::WaitButton => "Press the button on the device",
                                PairProgress::Ecdh => "Exchanging keys…",
                                PairProgress::Done => "Paired",
                                PairProgress::Failed => "Pairing failed",
                            });
                        }
                    }
                }
            });
        }

        let r = run_blocking(pair_start).await;
        stop.set(true);
        match r {
            Some(Ok(())) => self.toast("Paired"),
            Some(Err(e)) => self.toast(&format!("Pairing failed: {}", glib::markup_escape_text(&friendly(&e)))),
            None => {}
        }
        self.progress.set_visible(false);
        self.set_busy(false);
    }

    async fn unbind(self: &Rc<Self>, win: &gtk::Window, n: u8) {
        let ok = confirm(
            win,
            &format!("Unbind Host {n}?"),
            "That computer will no longer be able to authenticate with this device until it pairs again. Requires one touch of an enrolled finger.",
            "Unbind",
        )
        .await;
        if !ok {
            return;
        }
        self.set_busy(true);
        let r = gate_dialog::run(
            win,
            &format!("Unbind Host {n}"),
            "Touch a registered fingerprint on the device to confirm unbinding the other host.",
            move || clear_other_slot(n),
        )
        .await;
        match r {
            Some(Ok(())) => self.toast(&format!("Host {n} unbound")),
            Some(Err(e)) => self.toast(&glib::markup_escape_text(&friendly(&e))),
            None => {}
        }
        self.set_busy(false);
    }
}
```

`pages/mod.rs` 加 `pub mod hosts;`。

- [ ] **Step 2: 改造 `pages/dashboard.rs`**

逐项修改（文件其余部分不动）：

1. 顶部 `use` 改为：

```rust
use immurok_client::hosts::{slot_status, HostSlots};
use immurok_client::status::{
    query_paired, query_settings, query_status, set_setting, DeviceStatus, SettingKey, Settings,
};

use super::hosts::HostsGroup;
use super::run_blocking;
```

（删除 `use immurok_client::DaemonClient;` 与 `use std::time::Duration;` 若不再使用——`start_polling` 仍用 `Duration`，保留它。）

2. `DashboardPage` 结构体：删除字段 `pair_button`、`pair_busy`；新增 `hosts: Rc<HostsGroup>`。

3. `new()`：删除 `pair_button` / `pair_row` 四行及 `device.add(&pair_row);`；在 `page.add(&device);` 之后加：

```rust
        // ── Two Hosts ──
        let hosts = HostsGroup::new(toasts);
        page.add(hosts.widget());
```

结构体初始化里去掉 `pair_button` / `pair_busy`，加 `hosts`；删除 `this.wire_pair_button();`。

4. 删除整个 `fn wire_pair_button`。

5. `poll_once` 的 `run_blocking` 闭包改为：

```rust
            let result = run_blocking(|| {
                let status = query_status();
                let settings = status.as_ref().ok().and_then(|_| query_settings().ok());
                let paired = query_paired().unwrap_or(false);
                let slots = match &status {
                    Ok(s) if s.connected => slot_status().ok(),
                    _ => None,
                };
                (status, settings, paired, slots)
            })
            .await;
```

`match result` 改为：

```rust
            match result {
                Some((Ok(status), settings, paired, slots)) => this.apply(&status, settings.as_ref(), paired, slots),
                Some((Err(e), _, _, _)) => this.apply_daemon_down(&e),
                None => {}
            }
```

6. `apply` 签名改为 `fn apply(&self, status: &DeviceStatus, settings: Option<&Settings>, paired: bool, slots: Option<HostSlots>)`；删除 `if !self.pair_busy.get() { … }` 三行；在 `self.fw_row.set_subtitle(...)` 之后加 `self.hosts.apply(status, paired, slots);`。

7. `apply_daemon_down`：把 `self.pair_button.set_sensitive(false);` 换成 `self.hosts.apply_daemon_down();`。

8. `toast()` 助手上的 `#[allow(dead_code)]` 保持不变（本任务不新增调用）。

- [ ] **Step 3: 编译 + 启动检查**

Run: `cargo build -p immurok-gui && cargo test -p immurok-gui && cargo clippy -p immurok-gui`
Expected: warning-free。

`timeout 15 target/debug/immurok-gui 2>hosts.log`，stderr 为空。人工验收（真机）：Device 页出现 Two Hosts 两张卡片，"This computer" 标在正确的槽，自己的槽显示 Unpair；另一槽为空时显示灰字提示无按钮；第二台电脑配对后另一槽变 Bound 并出现 Unbind，点 Unbind → 确认 → 触摸 → toast "Host N unbound"；解除配对后 gui.json 的 `fingerprint_names` 为空；旧固件设备显示 "This firmware does not support two hosts." 与单个 Pair/Unpair 按钮。

- [ ] **Step 4: Commit**

```bash
git add crates/immurok-gui
git commit -m "gui: Device 页 Two Hosts 分组——卡片、配对进度、解除、门控解绑"
```

---

### Task 11: 文档、版本、阶段二计划备注

**Files:**
- Modify: `README.md`（§4.0 The GUI）
- Modify: `CHANGELOG.md`
- Modify: 六个 `crates/*/Cargo.toml` 的 `version`、`Cargo.lock`
- Modify: `docs/superpowers/plans/2026-09-14-gui-quickfill-input.md`（两处备注）

**Interfaces:**
- Consumes: 无

- [ ] **Step 1: README**

`### 4.0 The GUI (optional)` 段末尾追加一段：

```markdown
The **Fingerprints** page mirrors the macOS app: one card per enrolled
finger (click the name to rename it — names are stored locally in
`~/.config/immurok/gui.json`, the device only knows slot numbers), "+" to
enroll with the six-step guide, a hover-revealed delete button, "Test
Fingerprint", and — on firmware with two-host support — "Add switch
fingerprint" for the finger that only switches between your two computers.
The **Two Hosts** group on the Device page shows both host slots, marks this
computer, and lets you pair / unpair here or unbind the other computer
(one touch of an enrolled finger on the device confirms it).
```

- [ ] **Step 2: CHANGELOG**

顶部加（日期填提交当天）：

```markdown
## 0.8.0 — 2026-09-XX

### Added

- **Fingerprints page in `immurok-gui`.** Enroll with the six-step guided
  capture (the device's "too similar, shift your finger" reject is shown as
  a nudge instead of a failure), delete, rename (local names in
  `gui.json`), Test Fingerprint, and the host-switch finger (slot 5).
- **Two Hosts group on the Device page.** Both host slots with a "This
  computer" mark, Pair / Unpair, pairing progress text, and unbinding the
  other computer after one fingerprint touch.
- `immurok-client`: `fingerprint`, `enroll_session` (testable enrollment
  state machine), `hosts` and `enroll_hint` (moved from the CLI) modules.
- `immurok-common`: `EnrollEvent::Overlap` (0x06) and
  `PairProgress::from_wire`.

### Changed

- The GUI's error texts for fingerprint-gate outcomes now match the
  daemon's actual messages (timeout / cancelled / no match).
```

- [ ] **Step 3: 版本**

六个 `crates/*/Cargo.toml`（common / daemon / cli / session-agent / client / gui）`version = "0.8.0"`；`cargo build --workspace` 刷新 `Cargo.lock`，随后 `cargo build --workspace --locked` 必须通过。

- [ ] **Step 4: 阶段二计划备注**

`docs/superpowers/plans/2026-09-14-gui-quickfill-input.md`：
- Global Constraints 里「版本 0.7.0 → 0.8.0（Task 9）」改为「版本 0.8.0 → 0.9.0（Task 9；阶段三已占用 0.8.0）」。
- Task 2 标题下加一行：「**备注（2026-09-18）：** `settings_store.rs` 已由阶段三计划（`2026-09-18-gui-fingerprint-hosts.md` Task 6）以完全相同的结构创建并多了 `fingerprint_names` 字段；执行本任务时改为核对并补齐缺失项，不要重建文件。」

- [ ] **Step 5: 全量验证**

Run: `cargo build --workspace --locked && cargo test --workspace && cargo clippy -p immurok-gui -p immurok-client`
Expected: 全绿、无 warning。`make` 产出 `target/release/immurok-gui`。

- [ ] **Step 6: Commit**

```bash
git add README.md CHANGELOG.md crates/*/Cargo.toml Cargo.lock docs/superpowers/plans/2026-09-14-gui-quickfill-input.md
git commit -m "gui: 指纹管理 + 双机管理落地，文档与版本 0.8.0"
```

---

## Self-Review

**Spec coverage**
- §1 目标 / 不做清单：Task 9（指纹）、Task 10（双机）；Factory reset 未出现；TUI/CLI Overlap 文案未动 ✓
- §3 协议事实：Task 3 / 5 的 doc comment 与实现逐条对应 ✓
- §4 `immurok-common` 小改：Task 1 ✓（`PairProgress::from_wire` 放在 common 而非 spec §5.3 写的 client，`as_wire` 在同处，更合理）
- §5.1–5.4 client 模块：Task 3 / 4 / 5 / 2 ✓；`HostSlots` 复用 `dual_host::parse_slot_status_line` + `parse_slot_owner` 而非重写解析 ✓
- §6 settings_store：Task 6，与阶段二结构一致 + `fingerprint_names`；阶段二计划备注在 Task 11 ✓
- §7 线程模型：登记走 `std::thread::spawn` + `async-channel`（Task 8）；门控与短请求走 `run_blocking`（Task 7 / 9 / 10）✓
- §8 Fingerprints 页：Task 9（卡片、内联改名、悬停删除、"+"、Add switch、Test、Refresh、loading / disconnected、自有 2 s 连接轮询、`busy`）✓
- §9 门控对话框：Task 7（环、Cancel → GATE:CANCEL、结果文案）✓；登记对话框复用 `CountdownRing`（Task 8）✓
- §10 登记对话框：Task 8（前置确认在 Task 9 `add()`；门控页 / 引导页；六步文案与箭头；overlap 抖动不推进；Complete / Failed / 断开文案；Cancel → `FP:ENROLL_CANCEL`；切换指纹标题）✓
- §11 Two Hosts：Task 10（卡片、This computer、Pair + `PAIR:PROGRESS` 轮询、Unpair + 清名字、Unbind 确认 + 门控、另一槽为空提示、底部提示、不支持 / 未连接 / 状态不可用三种简单模式）✓
- §12 错误处理：Task 6 `errors::friendly` + 各处 `markup_escape_text` + 页级 `busy` ✓
- §13 安全：无日志打印用户数据；Cancel 只发取消命令 ✓
- §14 测试：Task 1（common 2）、Task 3（3）、Task 4（9）、Task 5（6）、Task 2（enroll_hint 2）、Task 6（settings_store 6 + errors 3）；手工验收分布在 Task 9 / 10 Step 3 ✓

**Placeholder scan**：无 TBD / TODO；CHANGELOG 日期 `2026-09-XX` 明示「填提交当天」。

**Type consistency**
- `EnrollStatus::Waiting { current, total }`：Task 3 定义，Task 4 `drive` 匹配 ✓
- `EnrollProgress` 八个变体：Task 4 定义，Task 8 `match` 全覆盖 ✓
- `Continue::{Go, Stop}`：Task 4 / Task 8 ✓
- `HostSlots { supported, slot1, slot2, active, mine }` + `bound / both_bound / other_of`：Task 5 定义，Task 10 使用 ✓
- `PairProgress` 六变体：Task 1 `from_wire`，Task 5 解析，Task 10 文案 `match` 全覆盖 ✓
- `gate_dialog::run(parent, title, hint, work) -> Option<Result<T, String>>`：Task 7 定义，Task 9（`fp_delete`、`fp_verify`）与 Task 10（`clear_other_slot`）调用一致 ✓
- `enroll_dialog::run(parent, slot) -> EnrollOutcome`：Task 8 定义，Task 9 调用 ✓
- `HostsGroup::{new, widget, apply(status, paired, slots), apply_daemon_down()}`：Task 10 定义与 dashboard 改造一致 ✓
- `settings_store::{load, save, GuiSettings::{fingerprint_name, set_fingerprint_name, clear_fingerprint_names, SWITCH_NAME}}`：Task 6 定义，Task 9 / 10 使用 ✓
- `errors::friendly`：Task 6 定义，Task 7 / 8 / 9 / 10 使用 ✓
- `main.rs` 三个 `#[allow(dead_code)]`：Task 6 / 7 / 8 加，Task 9 统一移除 ✓
