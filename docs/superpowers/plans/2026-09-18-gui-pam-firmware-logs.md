# Linux GUI 阶段四实施计划：PAM / Firmware / Logs 页

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 `immurok-gui` 加上与 TUI 对等的 PAM 页（服务安装状态 / 安装 / 移除 / 修复 + 隔离横幅）、Firmware 页（联网检查、直连 / 两跳 / 续传更新、进度）、Logs 页（实时尾部、等级着色、上滚暂停），以及 Device 页的固件更新提示。

**Architecture:** 固件更新 I/O 层（`immurok-cli/src/fwupdate/`）整体搬入 `immurok-client::fwupdate`，PAM helper 调用逻辑提炼为 `immurok-client::pam`（不打印不退出的纯 API），CLI / TUI 改为别名或薄封装。GUI 三页各自一个模块；pkexec 与文件读取走 `run_blocking`，固件 `execute` 与日志读取各用一个 std 线程 + `async-channel` 回主线程。不新增任何 daemon 协议。

**Tech Stack:** Rust 2021、gtk4-rs 0.9、libadwaita-rs 0.7、glib/gio 0.20、async-channel 2、ureq 2 / sha2 / base64 / hex / serde / thiserror（随 fwupdate 迁入 immurok-client）；现有 `immurok-common`、`immurok-client`、`immurok-gui`。

**Spec:** `docs/superpowers/specs/2026-09-18-gui-pam-firmware-logs-design.md`

## Global Constraints

- 平台下限 Debian 12 / Ubuntu 22.04：`gtk4 = "0.9"`、`libadwaita = "0.7"`，**不开任何 `v4_*` / `v1_*` feature**；只用 libadwaita 1.0 控件；确认 / 提示框用 `gtk::MessageDialog`（`pages::confirm` 或同款）。
- GTK 主线程禁止任何阻塞调用：socket 往返、`/etc/pam.d` 文件读取、`pkexec`（阻塞到用户完成授权）一律 `pages::run_blocking`；固件 `execute` 与日志 `BufReader::lines()` 用 `std::thread::spawn` + `async-channel`。
- `immurok-client` 保持同步 std，不引入 tokio。
- pkexec 参数只含 `find_helper()` 的绝对路径、固定动作字（`add` / `remove`）与 `PAM_SERVICES` 中的服务名；`run_helper` 对其他输入直接返回错误，不执行。
- 固件包校验链（manifest sha256、`imfw::parse`、设备端签名 / 低电量拒绝）原样保留；`execute` 不可取消，更新期间主窗口 `close-request` 返回 `Stop`，Ctrl+Q 同样被挡。
- 用户可见字符串一律英文；动态文本进 toast / `ActionRow` 前 `glib::markup_escape_text`；`TextView` 用纯文本插入。
- 日志与固件结果不写盘、不 `eprintln!`；GUI 不导出日志。
- CLI / TUI 用户可见行为不变（`immurok-cli fw|pam|logs`、TUI 三页）。
- 本地私有仓库 commit 用中文；改动限于 `app-linux-rs/`。所有 crate 版本 0.8.0 → 0.9.0（Task 8），CHANGELOG 加条目；阶段二计划的版本备注改为 0.9.0 → 0.10.0。
- 每个任务 `cargo build --workspace` / `cargo test` / `cargo clippy` warning-free；手工验收在 Linux 开发机（GNOME Wayland）进行，需要 polkit 密码、真固件包或停 daemon 的步骤由人完成，子代理只做启动检查。

---

## 文件结构

新建：
- `crates/immurok-client/src/fwupdate/{mod,http,store,push,error}.rs` — 从 `immurok-cli` `git mv`
- `crates/immurok-client/src/pam.rs` — `PAM_SERVICES`、`service_status`、`services_to_repair`、`find_helper`、`classify_exit`、`run_helper`、`PamError`
- `crates/immurok-gui/src/pages/pam.rs` — `PamPage`
- `crates/immurok-gui/src/pages/firmware.rs` — `FirmwarePage`、`ProgressMerger`、`silent_check`、`FW_UPDATING`
- `crates/immurok-gui/src/pages/logs.rs` — `LogsPage`、`classify`、`lines_to_drop`

修改：
- `crates/immurok-client/Cargo.toml` — ureq / sha2 / base64 / hex / serde / thiserror
- `crates/immurok-client/src/lib.rs` — `pub mod fwupdate; pub mod pam;`
- `crates/immurok-cli/src/main.rs` — `mod fwupdate;` → `use immurok_client::fwupdate;`
- `crates/immurok-cli/src/commands/pam.rs` — 改为 `immurok_client::pam` 的薄封装
- `crates/immurok-cli/src/tui/mod.rs` — `run_pam_helper` 改调共享 API，删 `find_pam_helper`
- `crates/immurok-gui/src/errors.rs` — `fw_friendly`
- `crates/immurok-gui/src/pages/mod.rs` — 声明三个新页
- `crates/immurok-gui/src/pages/dashboard.rs` — 固件提示行 + `set_fw_hint`
- `crates/immurok-gui/src/main_window.rs` — 挂三页、`close-request` 守卫、静默检查
- `crates/immurok-gui/src/main.rs` — `quit` action 检查 `FW_UPDATING`
- `README.md`、`CHANGELOG.md`、六个 `crates/*/Cargo.toml`、`Cargo.lock`、`docs/superpowers/plans/2026-09-14-gui-quickfill-input.md`

---

### Task 1: `fwupdate` 搬入 `immurok-client`

**Files:**
- Move: `crates/immurok-cli/src/fwupdate/{mod,http,store,push,error}.rs` → `crates/immurok-client/src/fwupdate/`
- Modify: `crates/immurok-client/Cargo.toml`、`crates/immurok-client/src/lib.rs`、`crates/immurok-client/src/fwupdate/mod.rs:18`、`crates/immurok-client/src/fwupdate/push.rs:19-21`
- Modify: `crates/immurok-cli/src/main.rs:4`

**Interfaces:**
- Produces（内容不变，只换路径）：`immurok_client::fwupdate::{query_device_status, DeviceStatus, fetch_manifest_cached, prepare, execute, PreparedUpdate, Hop, HopSource, ProgressEvent, stage_label, wait_for_version, unix_now, MANDATORY_MIN_VERSION, BATTERY_MIN_PERCENT, RECONNECT_TIMEOUT_SECS}`、`fwupdate::store::FwStore`、`fwupdate::error::FwUpdateError`、`fwupdate::push::{OtaChannel, PushEvent, push_once, push_with_retry}`、`fwupdate::http::*`

- [ ] **Step 1: 搬文件**

```bash
mkdir -p crates/immurok-client/src/fwupdate
git mv crates/immurok-cli/src/fwupdate/mod.rs   crates/immurok-client/src/fwupdate/mod.rs
git mv crates/immurok-cli/src/fwupdate/http.rs  crates/immurok-client/src/fwupdate/http.rs
git mv crates/immurok-cli/src/fwupdate/store.rs crates/immurok-client/src/fwupdate/store.rs
git mv crates/immurok-cli/src/fwupdate/push.rs  crates/immurok-client/src/fwupdate/push.rs
git mv crates/immurok-cli/src/fwupdate/error.rs crates/immurok-client/src/fwupdate/error.rs
```

- [ ] **Step 2: 改两处内部引用**

`crates/immurok-client/src/fwupdate/mod.rs` 第 18 行 `use crate::socket_client::DaemonClient;` → `use crate::DaemonClient;`。
`crates/immurok-client/src/fwupdate/push.rs` 第 19、21 行的 `crate::socket_client::DaemonClient` → `crate::DaemonClient`。其余内容一字不改（`crate::fwupdate::…` 路径在新 crate 里同样成立）。

- [ ] **Step 3: 依赖与声明**

`crates/immurok-client/Cargo.toml` `[dependencies]` 追加（版本与 `immurok-cli/Cargo.toml` 相同）：

```toml
ureq = "2"
sha2 = "0.10"
base64 = "0.22"
hex = "0.4"
serde = { version = "1", features = ["derive"] }
thiserror = "2"
```

`crates/immurok-client/src/lib.rs` 加 `pub mod fwupdate;`（字母序，在 `enroll_session` 之后）。

`crates/immurok-cli/src/main.rs` 把 `mod fwupdate;` 改为：

```rust
// Firmware-update orchestration lives in immurok-client now (shared with
// immurok-gui); re-exported under the old module name so `crate::fwupdate::…`
// across commands/ and tui/ keeps working unchanged.
use immurok_client::fwupdate;
```

`commands/{fw,ota,status}.rs`、`tui/app.rs` 不动。

- [ ] **Step 4: 编译 + 测试**

Run: `cargo build --workspace && cargo test -p immurok-client -p immurok-cli`
Expected: 无 warning；`immurok-client` 测试从 32 增为 49（fwupdate 17 个随文件迁入），`immurok-cli` 从 29 减为 12。若 `immurok-cli` 因某个只被 fwupdate 使用的依赖（如 `ureq`）变为未使用，保留不动（cargo 不对未用依赖告警）。

- [ ] **Step 5: 手工回归**

Run: `target/debug/immurok-cli fw status && target/debug/immurok-cli fw check`
Expected: 输出与改动前一致（本机无网络时 `fw check` 报 manifest 拉取失败也属一致）。

- [ ] **Step 6: Commit**

```bash
git add crates/immurok-client crates/immurok-cli/src Cargo.lock
git commit -m "client: fwupdate 搬入 immurok-client，CLI 用别名引用"
```

---

### Task 2: `immurok-client::pam` 与 CLI / TUI 改用共享 API

**Files:**
- Create: `crates/immurok-client/src/pam.rs`
- Modify: `crates/immurok-client/src/lib.rs`（`pub mod pam;`）
- Modify: `crates/immurok-cli/src/commands/pam.rs`
- Modify: `crates/immurok-cli/src/tui/mod.rs`（`find_pam_helper` 删除，`run_pam_helper` 改写）

**Interfaces:**
- Consumes: `immurok_common::pam::{pam_line_present, pam_line_present_in}`
- Produces:
  - `pub struct PamService { pub label: &'static str, pub service: &'static str, pub path: &'static str }`、`pub const PAM_SERVICES: [PamService; 3]`、`pub const PAM_DIR: &str = "/etc/pam.d"`、`pub fn is_known_service(s: &str) -> bool`
  - `pub struct PamServiceStatus { pub service, pub label, pub path, pub installed: bool }`、`pub fn service_status() -> Vec<PamServiceStatus>`、`pub fn service_status_in(dir: &Path) -> Vec<PamServiceStatus>`
  - `pub fn desired_services(sudo_on, polkit_on) -> Vec<&'static str>`、`pub fn services_to_repair(sudo_on, polkit_on) -> Vec<&'static str>`、`pub fn services_to_repair_in(dir, sudo_on, polkit_on) -> Vec<&'static str>`
  - `pub fn find_helper() -> Option<PathBuf>`、`pub fn helper_output_has_error(output: &str) -> bool`
  - `pub struct HelperReport { pub lines: Vec<String> }`、`pub enum PamError { HelperNotFound, NoPolkitAgent, AuthCancelled, HelperFailed { code: i32, lines: Vec<String> }, Spawn(String) }`（`Display`）
  - `pub fn classify_exit(code: Option<i32>, stdout: &str) -> Result<HelperReport, PamError>`、`pub fn run_helper(action: &str, services: &[&str]) -> Result<HelperReport, PamError>`

- [ ] **Step 1: 写测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_pam_dir(files: &[(&str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("immurok_pam_{}_{}", std::process::id(), files.len()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, body) in files {
            std::fs::write(dir.join(name), body).unwrap();
        }
        dir
    }

    #[test]
    fn service_table() {
        assert_eq!(PAM_SERVICES.len(), 3);
        assert_eq!(PAM_SERVICES[0].service, "sudo");
        assert_eq!(PAM_SERVICES[1].service, "polkit-1");
        assert_eq!(PAM_SERVICES[2].service, "gdm-password");
        assert_eq!(PAM_SERVICES[1].path, "/etc/pam.d/polkit-1");
        assert!(is_known_service("sudo") && !is_known_service("login"));
    }

    #[test]
    fn status_reads_each_file() {
        let dir = tmp_pam_dir(&[("sudo", "auth sufficient pam_immurok.so\n"), ("polkit-1", "auth include common-auth\n")]);
        let st = service_status_in(&dir);
        assert_eq!(st.len(), 3);
        assert!(st[0].installed && !st[1].installed && !st[2].installed);
        assert_eq!(st[2].label, "Login screen (gdm)");
    }

    #[test]
    fn repair_derives_from_toggles_and_missing_lines() {
        let dir = tmp_pam_dir(&[("sudo", "auth include common-auth\n"), ("polkit-1", "auth sufficient pam_immurok.so\n")]);
        assert_eq!(services_to_repair_in(&dir, true, true), vec!["sudo"]);
        assert_eq!(services_to_repair_in(&dir, false, true), Vec::<&str>::new());
        assert_eq!(services_to_repair_in(&dir, true, false), vec!["sudo"]);
        // gdm only when the file exists and lacks the line.
        let dir2 = tmp_pam_dir(&[("gdm-password", "auth include common-auth\n")]);
        assert_eq!(services_to_repair_in(&dir2, false, false), vec!["gdm-password"]);
    }

    #[test]
    fn error_lines() {
        assert!(helper_output_has_error("OK:ADDED(sudo)\nERROR:MODULE_NOT_INSTALLED(polkit-1)\n"));
        assert!(!helper_output_has_error("OK:ADDED(sudo)\n  OK:ALREADY_PRESENT(polkit-1)\n"));
    }

    #[test]
    fn exit_classification() {
        assert_eq!(
            classify_exit(Some(0), "OK:ADDED(sudo)\n"),
            Ok(HelperReport { lines: vec!["OK:ADDED(sudo)".into()] })
        );
        assert_eq!(
            classify_exit(Some(0), "ERROR:MODULE_NOT_INSTALLED(sudo)\n"),
            Err(PamError::HelperFailed { code: 0, lines: vec!["ERROR:MODULE_NOT_INSTALLED(sudo)".into()] })
        );
        assert_eq!(classify_exit(Some(126), ""), Err(PamError::AuthCancelled));
        assert_eq!(classify_exit(Some(127), ""), Err(PamError::NoPolkitAgent));
        assert_eq!(classify_exit(Some(1), "boom\n"), Err(PamError::HelperFailed { code: 1, lines: vec!["boom".into()] }));
        assert_eq!(classify_exit(None, ""), Err(PamError::HelperFailed { code: -1, lines: vec![] }));
    }

    #[test]
    fn run_helper_refuses_unknown_input_without_spawning() {
        assert_eq!(run_helper("nuke", &["sudo"]), Err(PamError::Spawn("invalid request".into())));
        assert_eq!(run_helper("add", &["login"]), Err(PamError::Spawn("invalid request".into())));
        assert_eq!(run_helper("add", &[]), Err(PamError::Spawn("invalid request".into())));
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p immurok-client pam`
Expected: 编译失败（模块不存在）

- [ ] **Step 3: 实现 `pam.rs`**

```rust
//! PAM configuration: which services carry the immurok auth line, and the
//! privileged helper that edits them.
//!
//! There is no socket command for this. State is read straight from
//! `/etc/pam.d/<service>` (`immurok_common::pam`), and edits go through
//! `pkexec <abs path>/immurok-pam-helper add|remove <svc...>` — polkit action
//! `com.immurok.pam-helper` (`auth_admin_keep`, GUI allowed, matched by the
//! helper's absolute path). The helper prints one `OK:…(svc)` or
//! `ERROR:…(svc)` line per service. pkexec itself exits 126 when the user
//! cancels the authentication and 127 when no polkit agent is available.
//!
//! Repair derivation mirrors `immurok-cli pam repair`: sudo / polkit-1 come
//! from the daemon's feature toggles; gdm-password is only touched when its
//! file already exists and lacks the line.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use immurok_common::pam::pam_line_present_in;

pub const PAM_DIR: &str = "/etc/pam.d";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PamService {
    pub label: &'static str,
    pub service: &'static str,
    pub path: &'static str,
}

pub const PAM_SERVICES: [PamService; 3] = [
    PamService { label: "sudo", service: "sudo", path: "/etc/pam.d/sudo" },
    PamService { label: "System authorization (polkit)", service: "polkit-1", path: "/etc/pam.d/polkit-1" },
    PamService { label: "Login screen (gdm)", service: "gdm-password", path: "/etc/pam.d/gdm-password" },
];

pub fn is_known_service(s: &str) -> bool {
    PAM_SERVICES.iter().any(|p| p.service == s)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PamServiceStatus {
    pub service: &'static str,
    pub label: &'static str,
    pub path: &'static str,
    pub installed: bool,
}

pub fn service_status_in(dir: &Path) -> Vec<PamServiceStatus> {
    PAM_SERVICES
        .iter()
        .map(|s| PamServiceStatus {
            service: s.service,
            label: s.label,
            path: s.path,
            installed: pam_line_present_in(dir, s.service),
        })
        .collect()
}

pub fn service_status() -> Vec<PamServiceStatus> {
    service_status_in(Path::new(PAM_DIR))
}

/// Services that should carry the line, derived from the daemon toggles.
pub fn desired_services(sudo_on: bool, polkit_on: bool) -> Vec<&'static str> {
    let mut v = Vec::new();
    if sudo_on {
        v.push("sudo");
    }
    if polkit_on {
        v.push("polkit-1");
    }
    v
}

/// Desired-but-missing services, plus gdm-password when its file exists and
/// lacks the line (best effort, never created).
pub fn services_to_repair_in(dir: &Path, sudo_on: bool, polkit_on: bool) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = desired_services(sudo_on, polkit_on)
        .into_iter()
        .filter(|s| !pam_line_present_in(dir, s))
        .collect();
    if dir.join("gdm-password").exists() && !pam_line_present_in(dir, "gdm-password") {
        v.push("gdm-password");
    }
    v
}

pub fn services_to_repair(sudo_on: bool, polkit_on: bool) -> Vec<&'static str> {
    services_to_repair_in(Path::new(PAM_DIR), sudo_on, polkit_on)
}

/// `immurok-pam-helper` next to the running binary, else on `PATH`.
pub fn find_helper() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let c = dir.join("immurok-pam-helper");
            if c.exists() {
                return Some(c);
            }
        }
    }
    let out = Command::new("which").arg("immurok-pam-helper").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if p.is_empty() {
        None
    } else {
        Some(PathBuf::from(p))
    }
}

/// The helper prints one line per service; any `ERROR:` line means failure.
pub fn helper_output_has_error(output: &str) -> bool {
    output.lines().any(|l| l.trim_start().starts_with("ERROR:"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperReport {
    /// Helper stdout, one trimmed line per service (`OK:…` / `ERROR:…`).
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PamError {
    HelperNotFound,
    /// pkexec exit 127: no polkit authentication agent is running.
    NoPolkitAgent,
    /// pkexec exit 126: the user dismissed the authentication dialog.
    AuthCancelled,
    /// The helper ran but reported a failure (non-zero exit, or `ERROR:` lines).
    HelperFailed { code: i32, lines: Vec<String> },
    Spawn(String),
}

impl fmt::Display for PamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PamError::HelperNotFound => write!(f, "immurok-pam-helper not found"),
            PamError::NoPolkitAgent => write!(f, "no polkit authentication agent is running"),
            PamError::AuthCancelled => write!(f, "authorization cancelled"),
            PamError::HelperFailed { code, lines } => {
                write!(f, "PAM helper failed (exit {code})")?;
                for l in lines {
                    write!(f, "\n{l}")?;
                }
                Ok(())
            }
            PamError::Spawn(e) => write!(f, "could not run pkexec: {e}"),
        }
    }
}

fn trimmed_lines(stdout: &str) -> Vec<String> {
    stdout.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
}

/// Pure classification of a pkexec + helper run.
pub fn classify_exit(code: Option<i32>, stdout: &str) -> Result<HelperReport, PamError> {
    let lines = trimmed_lines(stdout);
    match code {
        Some(0) if !helper_output_has_error(stdout) => Ok(HelperReport { lines }),
        Some(0) => Err(PamError::HelperFailed { code: 0, lines }),
        Some(126) => Err(PamError::AuthCancelled),
        Some(127) => Err(PamError::NoPolkitAgent),
        Some(c) => Err(PamError::HelperFailed { code: c, lines }),
        None => Err(PamError::HelperFailed { code: -1, lines }),
    }
}

/// Run `pkexec immurok-pam-helper <action> <services...>`. Blocks until the
/// user finishes (or cancels) the polkit prompt — never call on a UI thread.
/// Only `add` / `remove` and the services in [`PAM_SERVICES`] are accepted.
pub fn run_helper(action: &str, services: &[&str]) -> Result<HelperReport, PamError> {
    let valid = matches!(action, "add" | "remove")
        && !services.is_empty()
        && services.iter().all(|s| is_known_service(s));
    if !valid {
        return Err(PamError::Spawn("invalid request".into()));
    }
    let helper = find_helper().ok_or(PamError::HelperNotFound)?;
    let output = Command::new("pkexec")
        .arg(&helper)
        .arg(action)
        .args(services)
        .output()
        .map_err(|e| PamError::Spawn(e.to_string()))?;
    classify_exit(output.status.code(), &String::from_utf8_lossy(&output.stdout))
}
```

`lib.rs` 加 `pub mod pam;`。

- [ ] **Step 4: CLI 改薄封装**

`crates/immurok-cli/src/commands/pam.rs`：删除本文件里的 `desired_services`、`find_helper`、`helper_output_has_error`、`services_to_repair` 定义，顶部改为：

```rust
use crate::socket_client::DaemonClient;
use immurok_common::pam::pam_line_present;
pub use immurok_client::pam::{desired_services, helper_output_has_error, services_to_repair};
use immurok_client::pam::{find_helper, run_helper as run_helper_shared, PamError};
```

`run_helper` 改为（打印与退出码语义与原来一致）：

```rust
/// 经 pkexec 跑 immurok-pam-helper，一次处理多个服务。
pub fn run_helper(action: &str, services: &[&str]) {
    let helper = match find_helper() {
        Some(h) => h,
        None => {
            eprintln!("Error: immurok-pam-helper not found in PATH or next to this binary.");
            std::process::exit(1);
        }
    };
    println!("Running: pkexec {} {} {}", helper.display(), action, services.join(" "));
    match run_helper_shared(action, services) {
        Ok(report) => {
            for l in &report.lines {
                println!("{l}");
            }
            println!("\x1b[32mPAM {} done.\x1b[0m", action);
        }
        Err(PamError::HelperFailed { code, lines }) => {
            for l in &lines {
                println!("{l}");
            }
            if code == 0 {
                eprintln!("\x1b[31mPAM {} failed (see ERROR lines above).\x1b[0m", action);
            } else {
                eprintln!("\x1b[31mPAM helper failed (exit code: {})\x1b[0m", code);
            }
            std::process::exit(1);
        }
        Err(PamError::AuthCancelled) => {
            eprintln!("\x1b[31mPAM helper failed (exit code: 126)\x1b[0m");
            std::process::exit(1);
        }
        Err(PamError::NoPolkitAgent) => {
            eprintln!("\x1b[31mPAM helper failed (exit code: 127)\x1b[0m");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to run pkexec: {}", e);
            std::process::exit(1);
        }
    }
}
```

`run_check` / `run_repair` / `fetch_toggles` 不动。

`crates/immurok-cli/src/tui/mod.rs`：删除 `find_pam_helper`；`run_pam_helper` 改为：

```rust
/// Run the PAM helper via pkexec for one or more services. Returns true on success.
fn run_pam_helper(action: &str, services: &[&str]) -> bool {
    use immurok_client::pam::{find_helper, run_helper, PamError};
    let Some(helper) = find_helper() else {
        eprintln!("Error: immurok-pam-helper not found in PATH or next to this binary.");
        return false;
    };
    let svc_list = services.join(" ");
    println!("Running: pkexec {} {} {}", helper.display(), action, svc_list);
    let verb = if action == "add" { "install" } else { "remove" };
    match run_helper(action, services) {
        Ok(report) => {
            for l in &report.lines {
                println!("{l}");
            }
            println!("\x1b[32mPAM {} for '{}' succeeded.\x1b[0m", verb, svc_list);
            true
        }
        Err(PamError::HelperFailed { code, lines }) => {
            for l in &lines {
                println!("{l}");
            }
            if code == 0 {
                eprintln!("\x1b[31mPAM {} for '{}' failed (see ERROR lines above).\x1b[0m", verb, svc_list);
            } else {
                eprintln!("\x1b[31mPAM helper failed (exit code: {})\x1b[0m", code);
            }
            false
        }
        Err(PamError::AuthCancelled) => {
            eprintln!("\x1b[31mPAM helper failed (exit code: 126)\x1b[0m");
            false
        }
        Err(PamError::NoPolkitAgent) => {
            eprintln!("\x1b[31mPAM helper failed (exit code: 127)\x1b[0m");
            false
        }
        Err(e) => {
            eprintln!("Failed to run pkexec: {}", e);
            false
        }
    }
}
```

- [ ] **Step 5: 编译 + 测试**

Run: `cargo build --workspace && cargo test -p immurok-client pam && cargo test -p immurok-cli && cargo clippy -p immurok-client -p immurok-cli`
Expected: 无 warning；pam 6 passed；CLI 测试照旧。`target/debug/immurok-cli pam check` 输出与之前一致（不触发 pkexec）。

- [ ] **Step 6: Commit**

```bash
git add crates/immurok-client crates/immurok-cli/src
git commit -m "client: PAM 服务状态与 pkexec helper 调用 API，CLI/TUI 改用共享实现"
```

---

### Task 3: `errors::fw_friendly`

**Files:**
- Modify: `crates/immurok-gui/src/errors.rs`

**Interfaces:**
- Consumes: `immurok_client::fwupdate::error::FwUpdateError`
- Produces: `pub fn fw_friendly(e: &FwUpdateError) -> String`

- [ ] **Step 1: 写测试**（追加到 `errors.rs` 的 `tests` 模块）

```rust
    #[test]
    fn firmware_errors() {
        use immurok_client::fwupdate::error::FwUpdateError as E;
        assert_eq!(fw_friendly(&E::LowBattery), "Battery below 30 % — charge the device first");
        assert_eq!(
            fw_friendly(&E::ReconnectTimeout("x".into())),
            "Device did not come back after the update; power-cycle it and check again"
        );
        assert_eq!(fw_friendly(&E::ManifestFetch("dns".into())), "Could not reach the update server");
        assert_eq!(fw_friendly(&E::Download("404".into())), "Could not reach the update server");
        assert_eq!(fw_friendly(&E::ManifestSchema("bad".into())), "Update server returned an invalid manifest");
        assert!(fw_friendly(&E::Sha256Mismatch).starts_with("Firmware package rejected: "));
        assert!(fw_friendly(&E::SignatureRejected).starts_with("Firmware package rejected: "));
        assert_eq!(fw_friendly(&E::Preflight("device not connected".into())), E::Preflight("device not connected".into()).to_string());
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p immurok-gui errors`
Expected: 编译失败，缺 `fw_friendly`

- [ ] **Step 3: 实现**

在 `errors.rs` 末尾（测试模块之前）加：

```rust
/// Firmware-update errors (`immurok_client::fwupdate::error`) → user text.
pub fn fw_friendly(e: &immurok_client::fwupdate::error::FwUpdateError) -> String {
    use immurok_client::fwupdate::error::FwUpdateError as E;
    match e {
        E::LowBattery => "Battery below 30 % — charge the device first".into(),
        E::ReconnectTimeout(_) => {
            "Device did not come back after the update; power-cycle it and check again".into()
        }
        E::ManifestFetch(_) | E::Download(_) => "Could not reach the update server".into(),
        E::ManifestSchema(_) => "Update server returned an invalid manifest".into(),
        E::Sha256Mismatch | E::PackageInvalid(_) | E::HeaderRejected(_) | E::SignatureRejected => {
            format!("Firmware package rejected: {e}")
        }
        other => other.to_string(),
    }
}
```

- [ ] **Step 4: 跑测试**

Run: `cargo test -p immurok-gui errors`
Expected: 4 passed

- [ ] **Step 5: Commit**

```bash
git add crates/immurok-gui/src/errors.rs
git commit -m "gui: 固件更新错误映射 fw_friendly"
```

---

### Task 4: PAM 页

**Files:**
- Create: `crates/immurok-gui/src/pages/pam.rs`
- Modify: `crates/immurok-gui/src/pages/mod.rs`（`pub mod pam;`）
- Modify: `crates/immurok-gui/src/main_window.rs`（挂第四页）

**Interfaces:**
- Consumes: `immurok_client::pam::{run_helper, service_status, services_to_repair, HelperReport, PamError, PamServiceStatus}`、`immurok_client::status::query_settings`、`immurok_client::probe_isolation`、`pages::run_blocking`
- Produces: `pages::pam::PamPage { pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self>, pub fn widget(&self) -> &gtk::Widget }`（页首次映射时自动加载）

- [ ] **Step 1: 实现 `pages/pam.rs`**

```rust
//! PAM page (spec §5): isolation banner, per-service install state with
//! Install / Remove, and a one-shot Repair. Edits go through
//! `pkexec immurok-pam-helper` (polkit prompts); everything blocking runs
//! off the main thread.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::pam::{run_helper, service_status, services_to_repair, HelperReport, PamError, PamServiceStatus};
use immurok_client::probe_isolation;
use immurok_client::status::query_settings;

use super::run_blocking;

struct ServiceRow {
    service: &'static str,
    state: gtk::Label,
    button: gtk::Button,
    installed: Cell<bool>,
}

pub struct PamPage {
    root: adw::PreferencesPage,
    isolation: adw::ActionRow,
    rows: Vec<ServiceRow>,
    repair: gtk::Button,
    repair_note: gtk::Label,
    refresh: gtk::Button,
    toasts: adw::ToastOverlay,
    busy: Cell<bool>,
    loaded_once: Cell<bool>,
    to_repair: std::cell::RefCell<Vec<&'static str>>,
}

struct Snapshot {
    services: Vec<PamServiceStatus>,
    isolated: Option<bool>,
    to_repair: Vec<&'static str>,
}

fn snapshot() -> Snapshot {
    let services = service_status();
    let isolated = probe_isolation().map(|i| i.isolated);
    // Daemon unreachable → assume both toggles on (prompt rather than miss),
    // same as `immurok-cli pam repair`.
    let (sudo_on, polkit_on) = query_settings().map(|s| (s.unlock_sudo, s.unlock_polkit)).unwrap_or((true, true));
    let to_repair = services_to_repair(sudo_on, polkit_on);
    Snapshot { services, isolated, to_repair }
}

impl PamPage {
    pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self> {
        let root = adw::PreferencesPage::new();

        let daemon = adw::PreferencesGroup::builder().title("Daemon").build();
        let isolation = adw::ActionRow::builder().title("Isolation").subtitle("Checking…").build();
        daemon.add(&isolation);
        root.add(&daemon);

        let group = adw::PreferencesGroup::builder()
            .title("PAM services")
            .description("Installing or removing needs administrator authorization — polkit will prompt.")
            .build();
        let mut rows = Vec::new();
        for s in service_status() {
            let row = adw::ActionRow::builder().title(s.label).subtitle(s.path).build();
            let state = gtk::Label::new(Some("…"));
            state.add_css_class("dim-label");
            let button = gtk::Button::builder().label("Install").valign(gtk::Align::Center).build();
            row.add_suffix(&state);
            row.add_suffix(&button);
            group.add(&row);
            rows.push(ServiceRow { service: s.service, state, button, installed: Cell::new(false) });
        }
        root.add(&group);

        let actions = adw::PreferencesGroup::new();
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let repair = gtk::Button::builder().label("Repair").sensitive(false).build();
        let repair_note = gtk::Label::new(Some(""));
        repair_note.add_css_class("dim-label");
        let refresh = gtk::Button::builder().label("Refresh").build();
        bar.append(&repair);
        bar.append(&repair_note);
        bar.append(&refresh);
        actions.add(&bar);
        root.add(&actions);

        let this = Rc::new(Self {
            root,
            isolation,
            rows,
            repair,
            repair_note,
            refresh,
            toasts: toasts.clone(),
            busy: Cell::new(false),
            loaded_once: Cell::new(false),
            to_repair: std::cell::RefCell::new(Vec::new()),
        });

        for (i, r) in this.rows.iter().enumerate() {
            let weak = Rc::downgrade(&this);
            r.button.connect_clicked(move |_| {
                if let Some(p) = weak.upgrade() {
                    let row = &p.rows[i];
                    let action = if row.installed.get() { "remove" } else { "add" };
                    p.run(action, vec![row.service]);
                }
            });
        }
        let weak = Rc::downgrade(&this);
        this.repair.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                let svcs = p.to_repair.borrow().clone();
                if !svcs.is_empty() {
                    p.run("add", svcs);
                }
            }
        });
        let weak = Rc::downgrade(&this);
        this.refresh.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.load();
            }
        });
        // Load when the page first becomes visible (not at window build).
        let weak = Rc::downgrade(&this);
        this.root.connect_map(move |_| {
            if let Some(p) = weak.upgrade() {
                if !p.loaded_once.replace(true) {
                    p.load();
                }
            }
        });
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        for r in &self.rows {
            r.button.set_sensitive(!busy);
        }
        self.refresh.set_sensitive(!busy);
        self.repair.set_sensitive(!busy && !self.to_repair.borrow().is_empty());
    }

    fn load(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        self.set_busy(true);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let snap = run_blocking(snapshot).await;
            let Some(this) = weak.upgrade() else { return };
            if let Some(snap) = snap {
                this.apply(snap);
            }
            this.set_busy(false);
        });
    }

    fn apply(&self, snap: Snapshot) {
        let (text, class) = match snap.isolated {
            Some(true) => ("Isolated daemon — running as its own system user", "success"),
            Some(false) => (
                "NOT isolated — the daemon runs as your user; any of your processes can pass sudo. Run `make install`.",
                "error",
            ),
            None => ("Isolation unknown — daemon not reachable", "dim-label"),
        };
        for c in ["success", "error", "dim-label"] {
            self.isolation.remove_css_class(c);
        }
        self.isolation.add_css_class(class);
        self.isolation.set_subtitle(text);

        for (row, st) in self.rows.iter().zip(snap.services.iter()) {
            row.installed.set(st.installed);
            row.state.set_text(if st.installed { "Installed" } else { "Not installed" });
            row.button.set_label(if st.installed { "Remove" } else { "Install" });
        }

        self.repair_note.set_text(if snap.to_repair.is_empty() {
            "Nothing to repair"
        } else {
            ""
        });
        *self.to_repair.borrow_mut() = snap.to_repair;
    }

    fn run(self: &Rc<Self>, action: &'static str, services: Vec<&'static str>) {
        if self.busy.get() {
            return;
        }
        self.set_busy(true);
        let this = self.clone();
        glib::spawn_future_local(async move {
            let svcs = services.clone();
            let r = run_blocking(move || run_helper(action, &svcs)).await;
            match r {
                Some(Ok(report)) => this.report_ok(action, &report),
                Some(Err(e)) => this.report_err(e).await,
                None => {}
            }
            this.set_busy(false);
            this.load();
        });
    }

    fn report_ok(&self, action: &str, report: &HelperReport) {
        for line in &report.lines {
            // `OK:ADDED(sudo)` / `OK:ALREADY_PRESENT(polkit-1)` / `OK:REMOVED(sudo)` …
            let svc = line.rsplit('(').next().map(|s| s.trim_end_matches(')')).unwrap_or("");
            let text = if line.contains("ALREADY_PRESENT") {
                format!("Already present for {}", glib::markup_escape_text(svc))
            } else if action == "add" {
                format!("Installed for {}", glib::markup_escape_text(svc))
            } else {
                format!("Removed from {}", glib::markup_escape_text(svc))
            };
            self.toast(&text);
        }
    }

    async fn report_err(&self, e: PamError) {
        match e {
            PamError::AuthCancelled => self.toast("Authorization cancelled"),
            PamError::HelperFailed { lines, .. } => {
                let errs: Vec<&String> = lines.iter().filter(|l| l.starts_with("ERROR:")).collect();
                if errs.is_empty() {
                    self.toast("PAM helper failed");
                }
                for l in errs {
                    self.toast(&glib::markup_escape_text(l));
                }
            }
            PamError::NoPolkitAgent => {
                self.dialog(
                    "No authentication agent",
                    "No polkit authentication agent is running. Start your desktop's polkit agent, or run `immurok-cli pam install <service>` in a terminal.",
                )
                .await
            }
            PamError::HelperNotFound => {
                self.dialog("Helper not found", "immurok-pam-helper was not found. Re-run `make install`.").await
            }
            PamError::Spawn(s) => self.toast(&format!("Could not run pkexec: {}", glib::markup_escape_text(&s))),
        }
    }

    async fn dialog(&self, title: &str, body: &str) {
        let Some(win) = self.root.root().and_then(|r| r.downcast::<gtk::Window>().ok()) else { return };
        let dialog = gtk::MessageDialog::builder()
            .transient_for(&win)
            .modal(true)
            .message_type(gtk::MessageType::Error)
            .text(title)
            .secondary_text(body)
            .build();
        dialog.add_button("Close", gtk::ResponseType::Close);
        let _ = dialog.run_future().await;
        dialog.close();
    }
}
```

- [ ] **Step 2: 挂到主窗口**

`pages/mod.rs` 加 `pub mod pam;`。`main_window.rs` 在 fingerprints 页之后加：

```rust
        let pam = pages::pam::PamPage::new(&toasts);
        stack
            .add_titled(pam.widget(), Some("pam"), "PAM")
            .set_icon_name(Some("system-lock-screen-symbolic"));
```

并在 `set_data` 处加 `unsafe { window.set_data("pam-page", pam) };`。

- [ ] **Step 3: 编译 + 启动检查**

Run: `cargo build -p immurok-gui && cargo test -p immurok-gui && cargo clippy -p immurok-gui`
Expected: warning-free。`timeout 10 target/debug/immurok-gui 2>pam.log`，stderr 为空。人工验收：PAM 页三行状态与 `immurok-cli pam check` 一致；隔离横幅绿色；点 Install/Remove 弹 polkit 密码框；取消显示 "Authorization cancelled"；Repair 在无缺项时禁用并显示 "Nothing to repair"。

- [ ] **Step 4: Commit**

```bash
git add crates/immurok-gui
git commit -m "gui: PAM 页——隔离横幅、服务安装/移除/修复（pkexec helper）"
```

---

### Task 5: Firmware 页

**Files:**
- Create: `crates/immurok-gui/src/pages/firmware.rs`
- Modify: `crates/immurok-gui/src/pages/mod.rs`（`pub mod firmware;`）
- Modify: `crates/immurok-gui/src/main_window.rs`（挂第五页 + 更新中禁止关窗）
- Modify: `crates/immurok-gui/src/main.rs`（`quit` action 检查 `FW_UPDATING`）

**Interfaces:**
- Consumes: `immurok_client::fwupdate::{execute, prepare, query_device_status, stage_label, PreparedUpdate, ProgressEvent, store::FwStore, error::FwUpdateError}`、`crate::errors::fw_friendly`、`pages::run_blocking`
- Produces:
  - `pages::firmware::FW_UPDATING: AtomicBool`（更新进行中，全局只读标志）
  - `pages::firmware::ProgressMerger { pub fn new() -> Self, pub fn merge(&mut self, ev: &ProgressEvent) -> Merged }`、`pub struct Merged { pub stage: String, pub fraction: f64, pub hop: usize, pub hops: usize }`
  - `pages::firmware::FirmwarePage { pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self>, pub fn widget(&self) -> &gtk::Widget, pub fn check(self: &Rc<Self>) }`（页首次映射时自动 `check`）

- [ ] **Step 1: 写测试**（`firmware.rs` 末尾）

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use immurok_client::fwupdate::ProgressEvent as PE;

    #[test]
    fn stage_events_never_regress_within_a_hop() {
        let mut m = ProgressMerger::new();
        assert_eq!(m.merge(&PE::Stage { hop: 0, hops: 1, name: "erase" }).fraction, 0.0);
        let w = m.merge(&PE::Transfer { hop: 0, hops: 1, fraction: 0.6 });
        assert!((w.fraction - 0.6).abs() < 1e-9);
        assert_eq!(w.stage, "writing firmware");
        let s = m.merge(&PE::Stage { hop: 0, hops: 1, name: "end" });
        assert!((s.fraction - 0.6).abs() < 1e-9);
        assert_eq!(s.stage, "verifying + rebooting");
    }

    #[test]
    fn retry_resets_and_reconnect_fills() {
        let mut m = ProgressMerger::new();
        m.merge(&PE::Transfer { hop: 0, hops: 1, fraction: 0.9 });
        assert_eq!(m.merge(&PE::Stage { hop: 0, hops: 1, name: "retry" }).fraction, 0.0);
        let r = m.merge(&PE::Reconnect { hop: 0, hops: 1 });
        assert_eq!(r.fraction, 1.0);
        assert_eq!(r.stage, "waiting for device reboot");
    }

    #[test]
    fn two_hops_merge_and_display_one_based() {
        let mut m = ProgressMerger::new();
        let a = m.merge(&PE::Transfer { hop: 0, hops: 2, fraction: 1.0 });
        assert!((a.fraction - 0.5).abs() < 1e-9);
        assert_eq!((a.hop, a.hops), (1, 2));
        // Next hop starts from its own baseline, not the previous 1.0.
        let b = m.merge(&PE::Stage { hop: 1, hops: 2, name: "info" });
        assert!((b.fraction - 0.5).abs() < 1e-9);
        assert_eq!(b.hop, 2);
        let c = m.merge(&PE::Transfer { hop: 1, hops: 2, fraction: 0.5 });
        assert!((c.fraction - 0.75).abs() < 1e-9);
    }

    #[test]
    fn plan_text() {
        assert_eq!(plan_label(1, false, None), "Plan: direct (1 hop)");
        assert_eq!(plan_label(2, false, Some(("1.6.4", "1.8.0"))), "Plan: 2 hops (bridge 1.6.4 → 1.8.0)");
        assert_eq!(plan_label(1, true, None), "Plan: resume interrupted update");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p immurok-gui firmware`
Expected: 编译失败

- [ ] **Step 3: 实现 `pages/firmware.rs`**

```rust
//! Firmware page (spec §6): check the update server, show the plan, run the
//! OTA update with merged progress. Same state machine as the TUI's Firmware
//! tab. `execute` cannot be cancelled, so while it runs the main window
//! refuses to close (`FW_UPDATING`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::fwupdate::error::FwUpdateError;
use immurok_client::fwupdate::store::FwStore;
use immurok_client::fwupdate::{execute, prepare, query_device_status, stage_label, PreparedUpdate, ProgressEvent};

use crate::errors::fw_friendly;

use super::run_blocking;

/// True while `execute` is running on the worker thread. The main window and
/// the `quit` action consult it to refuse closing mid-update.
pub static FW_UPDATING: AtomicBool = AtomicBool::new(false);

pub fn updating() -> bool {
    FW_UPDATING.load(Ordering::Relaxed)
}

// ── progress merging (same rules as the TUI) ──

#[derive(Debug, Clone, PartialEq)]
pub struct Merged {
    pub stage: String,
    /// Overall 0.0..=1.0 across all hops.
    pub fraction: f64,
    /// 1-based hop for display.
    pub hop: usize,
    pub hops: usize,
}

/// Within-hop fraction must never regress on `Stage` events (they would
/// snap the bar back to the hop baseline after `Transfer` reached ~1.0);
/// "retry" is the one exception since the push restarts from ERASE. Reset
/// when the hop index advances.
pub struct ProgressMerger {
    last_frac: f64,
    last_hop: usize,
}

impl Default for ProgressMerger {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressMerger {
    pub fn new() -> Self {
        Self { last_frac: 0.0, last_hop: usize::MAX }
    }

    pub fn merge(&mut self, ev: &ProgressEvent) -> Merged {
        let ev_hop = match ev {
            ProgressEvent::Stage { hop, .. } | ProgressEvent::Transfer { hop, .. } | ProgressEvent::Reconnect { hop, .. } => *hop,
        };
        if ev_hop != self.last_hop {
            self.last_hop = ev_hop;
            self.last_frac = 0.0;
        }
        let (stage, frac, hop, hops) = match ev {
            ProgressEvent::Stage { hop, hops, name } => {
                if *name == "retry" {
                    self.last_frac = 0.0;
                }
                (stage_label(name).to_string(), self.last_frac, *hop, *hops)
            }
            ProgressEvent::Transfer { hop, hops, fraction } => {
                self.last_frac = *fraction;
                ("writing firmware".to_string(), *fraction, *hop, *hops)
            }
            ProgressEvent::Reconnect { hop, hops } => {
                self.last_frac = 1.0;
                ("waiting for device reboot".to_string(), 1.0, *hop, *hops)
            }
        };
        let hops = hops.max(1);
        Merged { stage, fraction: (hop as f64 + frac) / hops as f64, hop: hop + 1, hops }
    }
}

fn plan_label(hops: usize, resumed: bool, bridge: Option<(&str, &str)>) -> String {
    if resumed {
        "Plan: resume interrupted update".into()
    } else if let Some((from, to)) = bridge.filter(|_| hops == 2) {
        format!("Plan: 2 hops (bridge {from} → {to})")
    } else {
        "Plan: direct (1 hop)".into()
    }
}

// ── page ──

enum FwState {
    Idle,
    Checking,
    UpToDate,
    Ready(PreparedUpdate),
    Updating,
    Success(String),
    Failed(String),
}

enum Msg {
    Progress(Merged),
    Done(Result<String, String>),
}

pub struct FirmwarePage {
    root: adw::PreferencesPage,
    device_row: adw::ActionRow,
    latest_row: adw::ActionRow,
    status: gtk::Label,
    notes: gtk::Label,
    progress: gtk::ProgressBar,
    warning: gtk::Label,
    update_btn: gtk::Button,
    check_btn: gtk::Button,
    toasts: adw::ToastOverlay,
    state: RefCell<FwState>,
    loaded_once: Cell<bool>,
}

impl FirmwarePage {
    pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self> {
        let root = adw::PreferencesPage::new();
        let versions = adw::PreferencesGroup::builder().title("Firmware").build();
        let device_row = adw::ActionRow::builder().title("Device version").subtitle("-").build();
        let latest_row = adw::ActionRow::builder().title("Latest version").subtitle("-").build();
        versions.add(&device_row);
        versions.add(&latest_row);
        root.add(&versions);

        let group = adw::PreferencesGroup::new();
        let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let status = gtk::Label::builder().xalign(0.0).wrap(true).build();
        let notes = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).build();
        notes.add_css_class("dim-label");
        let progress = gtk::ProgressBar::builder().visible(false).show_text(true).build();
        let warning = gtk::Label::builder().label("Do not power off the device.").xalign(0.0).visible(false).build();
        warning.add_css_class("error");
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let update_btn = gtk::Button::builder().label("Update").sensitive(false).build();
        update_btn.add_css_class("suggested-action");
        let check_btn = gtk::Button::builder().label("Check again").build();
        buttons.append(&update_btn);
        buttons.append(&check_btn);
        column.append(&status);
        column.append(&notes);
        column.append(&progress);
        column.append(&warning);
        column.append(&buttons);
        group.add(&column);
        root.add(&group);

        let this = Rc::new(Self {
            root,
            device_row,
            latest_row,
            status,
            notes,
            progress,
            warning,
            update_btn,
            check_btn,
            toasts: toasts.clone(),
            state: RefCell::new(FwState::Idle),
            loaded_once: Cell::new(false),
        });
        this.render();

        let weak = Rc::downgrade(&this);
        this.check_btn.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.check();
            }
        });
        let weak = Rc::downgrade(&this);
        this.update_btn.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.update();
            }
        });
        let weak = Rc::downgrade(&this);
        this.root.connect_map(move |_| {
            if let Some(p) = weak.upgrade() {
                if !p.loaded_once.replace(true) {
                    p.check();
                }
            }
        });
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    fn set_state(&self, s: FwState) {
        *self.state.borrow_mut() = s;
        self.render();
    }

    fn render(&self) {
        let st = self.state.borrow();
        let (text, notes, can_update, can_check, updating) = match &*st {
            FwState::Idle | FwState::Checking => ("Checking for updates…".to_string(), None, false, false, false),
            FwState::UpToDate => ("✓ Firmware is up to date.".to_string(), None, false, true, false),
            FwState::Ready(p) => {
                let bridge = p.hops.first().zip(p.hops.last()).map(|(a, b)| (a.version.as_str(), b.version.as_str()));
                (plan_label(p.hops.len(), p.resumed, bridge), p.notes.clone(), true, true, false)
            }
            FwState::Updating => (String::new(), None, false, false, true),
            FwState::Success(v) => (format!("✓ Update complete — device is now on {v}."), None, false, true, false),
            FwState::Failed(e) => (format!("✗ {e}"), None, false, true, false),
        };
        if !updating {
            self.status.set_text(&text);
        }
        self.notes.set_visible(notes.is_some());
        if let Some(n) = notes {
            self.notes.set_text(&format!("Notes: {n}"));
        }
        self.progress.set_visible(updating);
        self.warning.set_visible(updating);
        self.update_btn.set_sensitive(can_update);
        self.check_btn.set_sensitive(can_check);
    }

    pub fn check(self: &Rc<Self>) {
        if matches!(*self.state.borrow(), FwState::Checking | FwState::Updating) {
            return;
        }
        self.set_state(FwState::Checking);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let r = run_blocking(|| {
                let st = query_device_status().ok();
                let store = FwStore::open_default()?;
                let prep = prepare(&store, true)?;
                Ok::<_, FwUpdateError>((st, prep))
            })
            .await;
            let Some(this) = weak.upgrade() else { return };
            match r {
                Some(Ok((st, prep))) => {
                    let dev = st.as_ref().filter(|s| s.connected).map(|s| s.version.clone()).unwrap_or_else(|| "-".into());
                    this.device_row.set_subtitle(&glib::markup_escape_text(&dev));
                    match prep {
                        Some(p) => {
                            this.latest_row.set_subtitle(&glib::markup_escape_text(&p.target_version));
                            this.set_state(FwState::Ready(p));
                        }
                        None => {
                            this.latest_row.set_subtitle(&glib::markup_escape_text(&dev));
                            this.set_state(FwState::UpToDate);
                        }
                    }
                }
                Some(Err(e)) => this.set_state(FwState::Failed(fw_friendly(&e))),
                None => this.set_state(FwState::Failed("Check failed".into())),
            }
        });
    }

    fn update(self: &Rc<Self>) {
        let prep = match &*self.state.borrow() {
            FwState::Ready(p) => p.clone(),
            _ => return,
        };
        FW_UPDATING.store(true, Ordering::Relaxed);
        self.set_state(FwState::Updating);
        self.status.set_text("Starting…");
        self.progress.set_fraction(0.0);
        self.progress.set_text(Some("0 %"));

        let (tx, rx) = async_channel::unbounded::<Msg>();
        std::thread::spawn(move || {
            let target = prep.target_version.clone();
            let result = FwStore::open_default()
                .and_then(|store| {
                    let mut merger = ProgressMerger::new();
                    let tx_p = tx.clone();
                    let mut progress = |ev: ProgressEvent| {
                        let _ = tx_p.send_blocking(Msg::Progress(merger.merge(&ev)));
                    };
                    execute(&store, &prep, &mut progress)
                })
                .map(|_| target)
                .map_err(|e| fw_friendly(&e));
            let _ = tx.send_blocking(Msg::Done(result));
        });

        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(msg) = rx.recv().await {
                let Some(this) = weak.upgrade() else { break };
                match msg {
                    Msg::Progress(m) => {
                        let stage = if m.hops > 1 { format!("hop {}/{}: {}", m.hop, m.hops, m.stage) } else { m.stage };
                        this.status.set_text(&stage);
                        this.progress.set_fraction(m.fraction.clamp(0.0, 1.0));
                        this.progress.set_text(Some(&format!("{:.0} %", m.fraction * 100.0)));
                    }
                    Msg::Done(result) => {
                        FW_UPDATING.store(false, Ordering::Relaxed);
                        match result {
                            Ok(v) => {
                                this.device_row.set_subtitle(&glib::markup_escape_text(&v));
                                this.toasts.add_toast(adw::Toast::new("Firmware updated"));
                                this.set_state(FwState::Success(v));
                            }
                            Err(e) => this.set_state(FwState::Failed(e)),
                        }
                        break;
                    }
                }
            }
            FW_UPDATING.store(false, Ordering::Relaxed);
        });
    }
}

// ── silent startup check (Device page hint) ──

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FwHint {
    /// Device firmware predates `MANDATORY_MIN_VERSION` (old signing era).
    Outdated,
    /// A newer firmware is published.
    Available(String),
}

/// Blocking; honours the 24 h manifest throttle; any error → None.
pub fn silent_check() -> Option<FwHint> {
    use immurok_client::fwupdate::{fetch_manifest_cached, unix_now, MANDATORY_MIN_VERSION};
    use immurok_common::fwupdate::planner::{self, UpdatePlan};
    use immurok_common::fwupdate::version::{normalize_semver, FirmwareVersion};

    let st = query_device_status().ok()?;
    if !st.connected || st.version.is_empty() {
        return None;
    }
    let device = normalize_semver(&st.version);
    if let (Some(v), Some(min)) = (FirmwareVersion::parse(&device), FirmwareVersion::parse(MANDATORY_MIN_VERSION)) {
        if v < min {
            return Some(FwHint::Outdated);
        }
    }
    let store = FwStore::open_default().ok()?;
    let m = fetch_manifest_cached(&store, false, unix_now()).ok()?;
    match planner::plan(&device, &m.latest.version, m.latest.min_direct.as_deref()) {
        UpdatePlan::UpToDate | UpdatePlan::Unknown => None,
        _ => Some(FwHint::Available(m.latest.version.clone())),
    }
}
```

若 `FirmwareVersion` 未实现 `PartialOrd`（TUI 的 `fw_outdated` 用了 `v < min`，说明已实现），按现状即可。

- [ ] **Step 4: 主窗口与 quit 守卫**

`pages/mod.rs` 加 `pub mod firmware;`。`main_window.rs`：

- 挂页：

```rust
        let firmware = pages::firmware::FirmwarePage::new(&toasts);
        stack
            .add_titled(firmware.widget(), Some("firmware"), "Firmware")
            .set_icon_name(Some("software-update-available-symbolic"));
```

- `window.present()` 之前加 close-request 守卫（本任务只有固件一项，Task 7 会在同一 handler 里加日志流关闭）：

```rust
        {
            let toasts = toasts.clone();
            window.connect_close_request(move |_| {
                if pages::firmware::updating() {
                    toasts.add_toast(adw::Toast::new("Firmware update in progress — please wait."));
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
```

（`main_window.rs` 需要 `use gtk::glib;`。）并 `unsafe { window.set_data("firmware-page", firmware) };`。

`main.rs` 的 `quit` action 闭包开头加：

```rust
            if pages::firmware::updating() {
                return; // the main window's close-request guard explains why
            }
```

- [ ] **Step 5: 编译 + 测试 + 启动检查**

Run: `cargo build -p immurok-gui && cargo test -p immurok-gui && cargo clippy -p immurok-gui`
Expected: warning-free；firmware 4 个测试通过。`timeout 10 target/debug/immurok-gui 2>fw.log` stderr 为空。人工验收：Firmware 页显示设备版本；有网时 Check 显示 UpToDate 或计划；设备有旧固件时 Update 走完（进度条只增不减、"Do not power off"、更新中关窗被挡、Ctrl+Q 无效）；断网时 Failed 显示 "Could not reach the update server"。

- [ ] **Step 6: Commit**

```bash
git add crates/immurok-gui
git commit -m "gui: Firmware 页——检查、计划、OTA 更新进度，更新中禁止关窗"
```

---

### Task 6: Device 页固件更新提示

**Files:**
- Modify: `crates/immurok-gui/src/pages/dashboard.rs`
- Modify: `crates/immurok-gui/src/main_window.rs`

**Interfaces:**
- Consumes: `pages::firmware::{silent_check, FwHint}`
- Produces: `DashboardPage::set_fw_hint(&self, hint: Option<FwHint>)`、`DashboardPage::connect_update_clicked(&self, f: impl Fn() + 'static)`

- [ ] **Step 1: Dashboard 提示行**

`pages/dashboard.rs`：
- `use super::firmware::FwHint;`
- 结构体加字段 `fw_hint_group: adw::PreferencesGroup`、`fw_hint_row: adw::ActionRow`、`fw_update_btn: gtk::Button`。
- `new()` 里在 `// ── Device ──` 之前建：

```rust
        // ── Firmware hint (hidden until the silent startup check says so) ──
        let fw_hint_group = adw::PreferencesGroup::builder().visible(false).build();
        let fw_hint_row = adw::ActionRow::builder().title("Firmware update available").build();
        let fw_update_btn = gtk::Button::builder().label("Update").valign(gtk::Align::Center).build();
        fw_update_btn.add_css_class("suggested-action");
        fw_hint_row.add_suffix(&fw_update_btn);
        fw_hint_group.add(&fw_hint_row);
        page.add(&fw_hint_group);
```

- 方法：

```rust
    pub fn set_fw_hint(&self, hint: Option<FwHint>) {
        match hint {
            None => self.fw_hint_group.set_visible(false),
            Some(FwHint::Outdated) => {
                self.fw_hint_row.set_title("⚠ Firmware outdated (old signing era)");
                self.fw_hint_row.set_subtitle("Update to keep unlocking and key operations working.");
                self.fw_hint_row.add_css_class("error");
                self.fw_hint_group.set_visible(true);
            }
            Some(FwHint::Available(v)) => {
                self.fw_hint_row.set_title(&format!("⬆ Firmware update available: v{}", glib::markup_escape_text(&v)));
                self.fw_hint_row.set_subtitle("");
                self.fw_hint_row.remove_css_class("error");
                self.fw_hint_group.set_visible(true);
            }
        }
    }

    pub fn connect_update_clicked(&self, f: impl Fn() + 'static) {
        self.fw_update_btn.connect_clicked(move |_| f());
    }
```

- [ ] **Step 2: 主窗口接线**

`main_window.rs` 在 `window.present();` 之前：

```rust
        {
            let stack = stack.clone();
            dashboard.connect_update_clicked(move || stack.set_visible_child_name("firmware"));
        }
        {
            let dashboard = Rc::downgrade(&dashboard);
            glib::spawn_future_local(async move {
                let hint = pages::run_blocking(pages::firmware::silent_check).await.flatten();
                if let Some(d) = dashboard.upgrade() {
                    d.set_fw_hint(hint);
                }
            });
        }
```

（`stack` 是 `adw::ViewStack`，GObject clone 是引用；Dashboard 页由窗口 `set_data` 持有，这里用 `Weak`。）

- [ ] **Step 3: 编译 + 验收**

Run: `cargo build -p immurok-gui && cargo clippy -p immurok-gui`
Expected: warning-free。人工验收：设备有可用更新时 Device 页顶部出现黄字提示，点 Update 跳到 Firmware 页；固件低于 1.6.0 时红字提示；否则无提示行。

- [ ] **Step 4: Commit**

```bash
git add crates/immurok-gui
git commit -m "gui: Device 页固件更新提示（启动静默检查，24 h 节流）"
```

---

### Task 7: Logs 页

**Files:**
- Create: `crates/immurok-gui/src/pages/logs.rs`
- Modify: `crates/immurok-gui/src/pages/mod.rs`（`pub mod logs;`）
- Modify: `crates/immurok-gui/src/main_window.rs`（挂第六页；close-request 里关闭日志流）

**Interfaces:**
- Consumes: `immurok_client::open_log_stream() -> Result<UnixStream, String>`、`pages::run_blocking`
- Produces:
  - `pages::logs::Level { Error, Warn, Dim, Plain }`、`pub fn classify(line: &str) -> Level`、`pub const LOG_CAP: usize = 1000`、`pub fn lines_to_drop(line_count: usize, cap: usize) -> usize`
  - `pages::logs::LogsPage { pub fn new() -> Rc<Self>, pub fn widget(&self) -> &gtk::Widget, pub fn shutdown(&self) }`（页首次映射时自动连接）

- [ ] **Step 1: 写测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_like_the_tui() {
        assert_eq!(classify("2026-09-18 ERROR ble: gone"), Level::Error);
        assert_eq!(classify("ERROR: x"), Level::Error);
        assert_eq!(classify("[ERROR] x"), Level::Error);
        assert_eq!(classify("thread 'main' panicked at"), Level::Error);
        assert_eq!(classify("2026 WARN  slow"), Level::Warn);
        assert_eq!(classify("[WARN] y"), Level::Warn);
        assert_eq!(classify("… 12 log lines dropped (viewer too slow)"), Level::Warn);
        assert_eq!(classify("2026 DEBUG poll"), Level::Dim);
        assert_eq!(classify("2026 TRACE poll"), Level::Dim);
        assert_eq!(classify("2026 INFO ok"), Level::Plain);
    }

    #[test]
    fn ring_trim() {
        assert_eq!(lines_to_drop(999, 1000), 0);
        assert_eq!(lines_to_drop(1000, 1000), 0);
        assert_eq!(lines_to_drop(1003, 1000), 3);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p immurok-gui logs`
Expected: 编译失败

- [ ] **Step 3: 实现 `pages/logs.rs`**

```rust
//! Logs page (spec §7): live tail of the daemon log over `SUBSCRIBE:LOG`,
//! level colouring, pause-on-scroll. A reader thread blocks on the socket
//! and forwards lines over a channel; the main loop appends them in batches
//! and keeps at most `LOG_CAP` lines in the buffer.

use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::open_log_stream;

use super::run_blocking;

pub const LOG_CAP: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
    Dim,
    Plain,
}

/// Same substring rules as the TUI's `log_line_style`; the daemon's
/// "… N log lines dropped" marker counts as a warning.
pub fn classify(line: &str) -> Level {
    if line.contains(" ERROR ") || line.contains("ERROR:") || line.contains("[ERROR]") || line.contains(" panicked") {
        Level::Error
    } else if line.contains(" WARN ") || line.contains("WARN:") || line.contains("[WARN]") || line.contains("log lines dropped") {
        Level::Warn
    } else if line.contains(" DEBUG ") || line.contains(" TRACE ") {
        Level::Dim
    } else {
        Level::Plain
    }
}

pub fn lines_to_drop(line_count: usize, cap: usize) -> usize {
    line_count.saturating_sub(cap)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamState {
    Connecting,
    Live,
    Paused(usize),
    Closed,
}

pub struct LogsPage {
    root: gtk::Box,
    badge: gtk::Label,
    jump: gtk::Button,
    reconnect: gtk::Button,
    view: gtk::TextView,
    buffer: gtk::TextBuffer,
    scroller: gtk::ScrolledWindow,
    state: Cell<StreamState>,
    stream: RefCell<Option<UnixStream>>,
    loaded_once: Cell<bool>,
}

impl LogsPage {
    pub fn new() -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.set_margin_top(12);
        root.set_margin_bottom(12);
        root.set_margin_start(12);
        root.set_margin_end(12);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let badge = gtk::Label::new(Some("● Connecting…"));
        badge.set_hexpand(true);
        badge.set_xalign(0.0);
        let jump = gtk::Button::builder().label("Jump to latest").sensitive(false).build();
        let reconnect = gtk::Button::builder().label("Reconnect").sensitive(false).build();
        header.append(&badge);
        header.append(&jump);
        header.append(&reconnect);

        let buffer = gtk::TextBuffer::new(None);
        let table = buffer.tag_table();
        let error = gtk::TextTag::builder().name("error").foreground("#e01b24").weight(700).build();
        let warn = gtk::TextTag::builder().name("warn").foreground("#e5a50a").build();
        let dim = gtk::TextTag::builder().name("dim").foreground("#77767b").build();
        table.add(&error);
        table.add(&warn);
        table.add(&dim);
        let view = gtk::TextView::builder()
            .buffer(&buffer)
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::None)
            .left_margin(6)
            .right_margin(6)
            .build();
        let scroller = gtk::ScrolledWindow::builder().child(&view).vexpand(true).build();

        root.append(&header);
        root.append(&scroller);

        let this = Rc::new(Self {
            root,
            badge,
            jump,
            reconnect,
            view,
            buffer,
            scroller,
            state: Cell::new(StreamState::Connecting),
            stream: RefCell::new(None),
            loaded_once: Cell::new(false),
        });

        let weak = Rc::downgrade(&this);
        this.jump.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.jump_to_end();
            }
        });
        let weak = Rc::downgrade(&this);
        this.reconnect.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.connect();
            }
        });
        // Scrolling up pauses; scrolling back to the bottom resumes.
        let weak = Rc::downgrade(&this);
        this.scroller.vadjustment().connect_value_changed(move |_| {
            if let Some(p) = weak.upgrade() {
                if p.at_bottom() {
                    if let StreamState::Paused(_) = p.state.get() {
                        p.set_state(StreamState::Live);
                    }
                }
            }
        });
        let weak = Rc::downgrade(&this);
        this.root.connect_map(move |_| {
            if let Some(p) = weak.upgrade() {
                if !p.loaded_once.replace(true) {
                    p.connect();
                }
            }
        });
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    /// Close the socket so the reader thread ends. Safe to call twice.
    pub fn shutdown(&self) {
        if let Some(s) = self.stream.borrow_mut().take() {
            let _ = s.shutdown(Shutdown::Both);
        }
    }

    fn set_state(&self, s: StreamState) {
        self.state.set(s);
        let text = match s {
            StreamState::Connecting => "● Connecting…".to_string(),
            StreamState::Live => "● Live".to_string(),
            StreamState::Paused(n) => format!("⏸ Paused ({n} new lines)"),
            StreamState::Closed => "● Stream closed".to_string(),
        };
        self.badge.set_text(&text);
        self.jump.set_sensitive(matches!(s, StreamState::Paused(_)));
        self.reconnect.set_sensitive(matches!(s, StreamState::Closed));
    }

    fn at_bottom(&self) -> bool {
        let adj = self.scroller.vadjustment();
        adj.value() + adj.page_size() >= adj.upper() - 1.0
    }

    fn jump_to_end(&self) {
        let mut end = self.buffer.end_iter();
        self.view.scroll_to_iter(&mut end, 0.0, false, 0.0, 1.0);
        self.set_state(StreamState::Live);
    }

    fn connect(self: &Rc<Self>) {
        self.shutdown();
        self.set_state(StreamState::Connecting);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let opened = run_blocking(open_log_stream).await;
            let Some(this) = weak.upgrade() else { return };
            let stream = match opened {
                Some(Ok(s)) => s,
                _ => {
                    this.set_state(StreamState::Closed);
                    return;
                }
            };
            let reader = match stream.try_clone() {
                Ok(r) => r,
                Err(_) => {
                    this.set_state(StreamState::Closed);
                    return;
                }
            };
            *this.stream.borrow_mut() = Some(stream);
            this.set_state(StreamState::Live);

            let (tx, rx) = async_channel::unbounded::<String>();
            std::thread::spawn(move || {
                for line in BufReader::new(reader).lines() {
                    match line {
                        Ok(l) => {
                            if tx.send_blocking(l).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                // Dropping `tx` closes the channel → the UI shows "Stream closed".
            });

            let weak = Rc::downgrade(&this);
            drop(this);
            glib::spawn_future_local(async move {
                while let Ok(first) = rx.recv().await {
                    let mut batch = vec![first];
                    while let Ok(more) = rx.try_recv() {
                        batch.push(more);
                    }
                    let Some(this) = weak.upgrade() else { return };
                    this.append(&batch);
                }
                if let Some(this) = weak.upgrade() {
                    this.stream.borrow_mut().take();
                    this.set_state(StreamState::Closed);
                }
            });
        });
    }

    fn append(&self, lines: &[String]) {
        let follow = self.at_bottom() && !matches!(self.state.get(), StreamState::Paused(_));
        for line in lines {
            let mut end = self.buffer.end_iter();
            let start_off = end.offset();
            self.buffer.insert(&mut end, line);
            self.buffer.insert(&mut end, "\n");
            let tag = match classify(line) {
                Level::Error => Some("error"),
                Level::Warn => Some("warn"),
                Level::Dim => Some("dim"),
                Level::Plain => None,
            };
            if let Some(t) = tag {
                let s = self.buffer.iter_at_offset(start_off);
                let e = self.buffer.end_iter();
                self.buffer.apply_tag_by_name(t, &s, &e);
            }
        }
        let drop_n = lines_to_drop(self.buffer.line_count() as usize, LOG_CAP + 1);
        if drop_n > 0 {
            let mut a = self.buffer.start_iter();
            let mut b = self.buffer.iter_at_line(drop_n as i32).unwrap_or_else(|| self.buffer.start_iter());
            self.buffer.delete(&mut a, &mut b);
        }
        if follow {
            let mut end = self.buffer.end_iter();
            self.view.scroll_to_iter(&mut end, 0.0, false, 0.0, 1.0);
        } else {
            let n = match self.state.get() {
                StreamState::Paused(n) => n,
                _ => 0,
            };
            self.set_state(StreamState::Paused(n + lines.len()));
        }
    }
}
```

说明：`TextBuffer::line_count()` 把末尾换行后的空行也算一行，所以裁剪阈值用 `LOG_CAP + 1`。`iter_at_line` 在 gtk4-rs 0.9 返回 `Option<TextIter>`。

- [ ] **Step 4: 挂到主窗口与关闭联动**

`pages/mod.rs` 加 `pub mod logs;`。`main_window.rs`：

```rust
        let logs = pages::logs::LogsPage::new();
        stack
            .add_titled(logs.widget(), Some("logs"), "Logs")
            .set_icon_name(Some("utilities-terminal-symbolic"));
```

Task 5 加的 `close-request` 守卫改为：

```rust
        {
            let toasts = toasts.clone();
            let logs = Rc::downgrade(&logs);
            window.connect_close_request(move |_| {
                if pages::firmware::updating() {
                    toasts.add_toast(adw::Toast::new("Firmware update in progress — please wait."));
                    return glib::Propagation::Stop;
                }
                if let Some(l) = logs.upgrade() {
                    l.shutdown();
                }
                glib::Propagation::Proceed
            });
        }
```

并 `unsafe { window.set_data("logs-page", logs) };`。

- [ ] **Step 5: 编译 + 测试 + 启动检查**

Run: `cargo build -p immurok-gui && cargo test -p immurok-gui && cargo clippy -p immurok-gui`
Expected: warning-free；logs 2 个测试通过。`timeout 10 target/debug/immurok-gui 2>logs.log` stderr 为空（Logs 页非默认页，连接只在首次切换时发生）。人工验收：切到 Logs 页立即看到历史尾部并实时滚动；上滚显示 Paused 与计数；Jump to latest 恢复；停 daemon 后显示 Stream closed，起回后 Reconnect 成功；关窗后 `ss -xp | grep pam.sock` 无 GUI 残留连接。

- [ ] **Step 6: Commit**

```bash
git add crates/immurok-gui
git commit -m "gui: Logs 页——SUBSCRIBE:LOG 实时尾部、等级着色、上滚暂停"
```

---

### Task 8: 文档、版本 0.9.0、阶段二计划备注

**Files:**
- Modify: `README.md`（§4.0 The GUI）
- Modify: `CHANGELOG.md`
- Modify: 六个 `crates/*/Cargo.toml`、`Cargo.lock`
- Modify: `docs/superpowers/plans/2026-09-14-gui-quickfill-input.md`

**Interfaces:**
- Consumes: 无

- [ ] **Step 1: README**

`### 4.0 The GUI (optional)` 段末尾追加：

```markdown
The **PAM** page shows whether the immurok line is installed for sudo,
polkit and the login screen, with Install / Remove / Repair buttons (polkit
asks for your password). The **Firmware** page checks immurok.com for a
newer firmware and runs the OTA update with a progress bar — the window
refuses to close while an update is running. The **Logs** page tails the
daemon log live with error / warning colouring; scroll up to pause.
```

- [ ] **Step 2: CHANGELOG**（日期填提交当天）

```markdown
## 0.9.0 — 2026-09-XX

### Added

- **PAM, Firmware and Logs pages in `immurok-gui`**, matching the TUI:
  per-service install state with pkexec-backed Install / Remove / Repair and
  the daemon isolation banner; update-server check, direct / two-hop /
  resumed plans and OTA progress (window locked while updating); live daemon
  log tail with level colouring and pause-on-scroll. The Device page shows a
  firmware-update hint after a silent 24 h-throttled check.
- `immurok-client`: `fwupdate` (moved from the CLI) and `pam` modules shared
  by CLI, TUI and GUI.
```

- [ ] **Step 3: 版本**

六个 `crates/*/Cargo.toml` `version = "0.9.0"`；`cargo build --workspace` 刷新 `Cargo.lock`，随后 `cargo build --workspace --locked` 必须通过。

- [ ] **Step 4: 阶段二计划备注**

`docs/superpowers/plans/2026-09-14-gui-quickfill-input.md` Global Constraints 里「版本 0.8.0 → 0.9.0（Task 9；阶段三已占用 0.8.0）」改为「版本 0.9.0 → 0.10.0（Task 9；阶段三占用 0.8.0、阶段四占用 0.9.0）」。

- [ ] **Step 5: 全量验证**

Run: `cargo build --workspace --locked && cargo test --workspace && cargo clippy -p immurok-gui -p immurok-client -p immurok-cli`
Expected: 全绿、无 warning；`make` 产出 `target/release/immurok-gui`。

- [ ] **Step 6: Commit**

```bash
git add README.md CHANGELOG.md crates/*/Cargo.toml Cargo.lock docs/superpowers/plans/2026-09-14-gui-quickfill-input.md
git commit -m "gui: PAM / Firmware / Logs 页落地，文档与版本 0.9.0"
```

---

## Self-Review

**Spec coverage**
- §1 布局 6 页：Task 4 / 5 / 7 各挂一页 ✓；Device 页提示：Task 6 ✓；不做清单：无本地 .imfw、无过滤、无重启按钮 ✓
- §3.1 fwupdate 搬家：Task 1 ✓；§3.2 pam 模块与 CLI/TUI 薄封装：Task 2 ✓
- §4 线程模型：pkexec / 文件读取 `run_blocking`（Task 4）、固件线程 + channel（Task 5）、日志线程 + channel 批量追加（Task 7）✓
- §5 PAM 页各元素、文案、错误分支（NoPolkitAgent / AuthCancelled / HelperNotFound / HelperFailed）：Task 4 ✓；Repair 派生与 CLI 一致（daemon 不可达按两开关都开）✓
- §6 Firmware 状态机、文案、进度合并、关窗守卫、Ctrl+Q、错误映射（Task 3）、§6.5 提示（Task 6）✓
- §7 Logs 徽标、着色规则、1000 行环、暂停/恢复、生命周期与关窗联动：Task 7 ✓
- §8 安全：`run_helper` 输入白名单（Task 2）、校验链未动、无落盘、转义 ✓
- §9 测试：pam 6、errors +1、firmware 4、logs 2、fwupdate 17 随迁 ✓；手工验收在 Task 4/5/6/7 Step 里 ✓

**Placeholder scan**：无 TBD / TODO；CHANGELOG 日期 `XX` 明示「填提交当天」。

**Type consistency**
- `PamError` 五变体：Task 2 定义，CLI/TUI 封装与 Task 4 `report_err` 全覆盖 ✓
- `HelperReport { lines }`：Task 2 / Task 4 ✓
- `run_blocking` 返回 `Option<T>`：Task 4/5/6/7 都处理 `None` ✓
- `ProgressEvent` 三变体（`hop`/`hops`/`name`/`fraction`）：Task 5 `merge` 与 fwupdate 定义一致 ✓
- `FwHint::{Outdated, Available(String)}`：Task 5 定义、Task 6 使用 ✓
- `pages::firmware::updating()`：Task 5 定义，main_window / main.rs / Task 7 使用 ✓
- `LogsPage::shutdown`：Task 7 定义并在 close-request 使用 ✓
- `open_log_stream() -> Result<UnixStream, String>`：现有签名 ✓
