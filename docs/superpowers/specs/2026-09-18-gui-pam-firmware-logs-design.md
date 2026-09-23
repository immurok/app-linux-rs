# Linux GUI 阶段四：PAM / Firmware / Logs 页 设计稿

日期：2026-09-18
状态：设计定稿，待写实施计划
前置：阶段一（0.7.0）、阶段三（0.8.0，分支 `worktree-linux-gui-phase1`）
参考实现：`immurok-cli/src/tui/{app,widgets,mod}.rs` 的 PAM / Firmware / Logs 页，`immurok-cli/src/commands/{pam,fw}.rs`，`immurok-cli/src/fwupdate/*`，`scripts/immurok-pam-helper`

---

## 1. 目标与布局

把 TUI 剩下的三页搬到 GUI，与 TUI 功能对等：

- **PAM**：三个 PAM 服务（sudo / polkit-1 / gdm-password）的安装状态、安装 / 移除 / 一键修复，以及 daemon 隔离状态横幅。
- **Firmware**：联网检查新版本、直连 / 两跳 / 续传计划、带进度的 OTA 更新。
- **Logs**：daemon 日志实时尾部，等级着色，上滚暂停。

主窗口 `ViewStack` 变为 6 页：Device / Keys / Fingerprints / PAM / Firmware / Logs。Device 页顶部加一行固件提示（§6.5）。

不做：本地 `.imfw` 文件刷写；日志过滤 / 搜索 / 复制；daemon 重启按钮（CLI 的 `systemctl --user restart immurok-daemon` 在特权分离后已不适用，另行处理）；新增任何 daemon 协议。

## 2. 事实（2026-09-18 核对）

- **PAM 没有 socket 命令。** 状态由客户端直接读 `/etc/pam.d/{sudo,polkit-1,gdm-password}`（`immurok_common::pam::pam_line_present`）。修改通过 `pkexec <绝对路径>/immurok-pam-helper add|remove <svc...>`；polkit 动作 `com.immurok.pam-helper`（`auth_admin_keep`、`allow_gui=true`、按 `exec.path` 匹配），GUI 可原样复用。helper 每服务打印一行 `OK:...(svc)` 或 `ERROR:...(svc)`；pkexec 退出码 126 = 用户取消授权，127 = 无 polkit agent。修复派生规则：`services_to_repair(unlock_sudo, unlock_polkit)` 只派生 sudo / polkit-1；gdm-password 仅在文件已存在且缺行时尽力修。
- **固件更新全走 daemon socket，无特权。** 逻辑在 `immurok-cli/src/fwupdate/`（bin crate，GUI 不可引用）：`query_device_status()`（STATUS）、`fetch_manifest_cached(&store, force, now)`（`https://immurok.com/fw/manifest.json`，`IMMUROK_FW_MANIFEST_URL` 可覆盖，24 h 节流）、`prepare(&store, force) -> Option<PreparedUpdate { device_version, target_version, notes, hops, resumed }>`、`execute(&store, &prep, &mut progress)`（先下载并校验全部 hop，再逐 hop `push_with_retry` + `wait_for_version(60 s)`）、`ProgressEvent`、`stage_label`。常量 `BATTERY_MIN_PERCENT = 30`、`RECONNECT_TIMEOUT_SECS = 60`、`MANDATORY_MIN_VERSION = "1.6.0"`。`FwStore::open_default()` = `~/.immurok/fwupdate`。纯逻辑部分已在 `immurok-common::fwupdate::{manifest, planner, imfw, version}`。`execute` 无取消令牌。
- **Logs**：`immurok_client::open_log_stream()` 发 `SUBSCRIBE:LOG` 后返回 `UnixStream`；daemon 先发缓冲快照再实时推送，落后时注入 `… N log lines dropped (viewer too slow)`；无过滤参数；`shutdown(Both)` 结束。TUI 环形上限 1000 行。

## 3. `immurok-client` 新模块

### 3.1 `fwupdate/`（搬家）
`git mv crates/immurok-cli/src/fwupdate/{mod,http,store,push,error}.rs → crates/immurok-client/src/fwupdate/`。内容只改两处引用：`crate::socket_client::DaemonClient` → `crate::DaemonClient`。`immurok-client/Cargo.toml` 加 `ureq = "2"`、`sha2 = "0.10"`、`base64 = "0.22"`、`hex = "0.4"`、`serde = { version = "1", features = ["derive"] }`（版本与 CLI 相同）。`immurok-cli/src/main.rs` 的 `mod fwupdate;` 改为 `use immurok_client::fwupdate;`，`commands/{fw,ota,status}.rs`、`tui/app.rs` 的 `crate::fwupdate::` 路径不改。既有单测随文件搬家。

### 3.2 `pam.rs`（新建）
从 `immurok-cli/src/commands/pam.rs` 提炼可复用部分：

```rust
pub struct PamService { pub label: &'static str, pub service: &'static str, pub path: &'static str }
pub const PAM_SERVICES: [PamService; 3];   // sudo / polkit-1 / gdm-password（顺序同 TUI）
pub struct PamServiceStatus { pub service: &'static str, pub label: &'static str, pub path: &'static str, pub installed: bool }
pub fn service_status() -> Vec<PamServiceStatus>;                 // 读文件，不碰 daemon
pub fn desired_services(sudo_on: bool, polkit_on: bool) -> Vec<&'static str>;
pub fn services_to_repair(sudo_on: bool, polkit_on: bool) -> Vec<&'static str>;
pub fn find_helper() -> Option<PathBuf>;                           // 同目录优先，其次 PATH
pub fn helper_output_has_error(output: &str) -> bool;
pub struct HelperReport { pub lines: Vec<String> }                 // helper 每服务一行
pub enum PamError { HelperNotFound, NoPolkitAgent, AuthCancelled, HelperFailed { code: i32, lines: Vec<String> }, Spawn(String) }
pub fn run_helper(action: &str, services: &[&str]) -> Result<HelperReport, PamError>;
```

`run_helper` 只允许 `action ∈ {"add","remove"}`、`services ⊆ PAM_SERVICES`，否则返回 `Spawn("invalid request")`；退出码 126 → `AuthCancelled`，127 → `NoPolkitAgent`，其余非零 → `HelperFailed`；退出 0 但含 `ERROR:` 行 → `HelperFailed { code: 0, lines }`。CLI 的 `commands/pam.rs::run_helper` 与 TUI 的 `tui/mod.rs::run_pam_helper` 改为调用它再打印 / 退出，用户可见行为不变。

## 4. GTK 线程模型

- pkexec 阻塞到用户完成授权：在 `pages::run_blocking` 里跑。
- 固件 `execute` 是同步长任务：`std::thread::spawn`，进度经 `async-channel` 回主线程（与登记对话框同一手法）。
- 日志读线程阻塞在 `BufReader::lines()`：`std::thread::spawn`，逐行经 `async-channel` 送回；主线程每次 `recv` 后 `try_recv` 取尽再一次性追加。
- 文件状态读取（`service_status`、`probe_isolation`）也放 `run_blocking`。

## 5. PAM 页（`pages/pam.rs`）

- 隔离横幅（`adw::PreferencesGroup` 内一行带图标的 `ActionRow`，页显示与 Refresh 时查 `probe_isolation()`）：
  - 已隔离：绿色 "Isolated daemon — running as its own system user"
  - 未隔离：红色 "NOT isolated — the daemon runs as your user; any of your processes can pass sudo. Run `make install`."
  - 未知：灰色 "Isolation unknown — daemon not reachable"
- 分组 "PAM services"，描述 "Installing or removing needs administrator authorization — polkit will prompt."；三行 `ActionRow`：标题 "sudo" / "System authorization (polkit)" / "Login screen (gdm)"，副标题为文件路径，后缀状态标签 "Installed" / "Not installed" + 按钮 "Install" / "Remove"（按当前状态二选一）。
- 底部按钮行："Repair"（`services_to_repair(query_settings)`；daemon 不可达按两开关都开处理，与 CLI 一致；无待修项时禁用并旁注 "Nothing to repair"）、"Refresh"。
- 动作：页级 `busy` → `run_blocking(run_helper(...))` → 结果：每条 `OK:` 行 toast "Installed for sudo" / "Removed from sudo" / "Already present for polkit-1"（按行首关键字映射，未识别则原文）；`HelperFailed` → 对 `ERROR:` 行逐条 toast（转义）；`NoPolkitAgent` → `MessageDialog` "No polkit authentication agent is running. Start your desktop's polkit agent, or run `immurok-cli pam install <service>` in a terminal."；`AuthCancelled` → toast "Authorization cancelled"；`HelperNotFound` → `MessageDialog` "immurok-pam-helper was not found. Re-run `make install`."。动作后重读状态。
- 状态刷新时机：页首次映射、每次动作后、Refresh；不轮询。

## 6. Firmware 页（`pages/firmware.rs`）

### 6.1 状态机（与 TUI 一致）
`Idle → Checking → UpToDate | Ready(prep) | Failed(e)`；`Ready → Updating { stage, fraction, hop, hops } → Success(v) | Failed(e)`。

### 6.2 显示
- 两行：`Device version`（`query_device_status().version`，未连接显示 "-"）、`Latest version`（`prep.target_version` 或 "-"）。
- Checking：spinner + "Checking for updates…"。
- UpToDate："✓ Firmware is up to date." + "Check again"。
- Ready："Plan: direct (1 hop)" / "Plan: 2 hops (bridge X → Y)" / "Plan: resume interrupted update"；可选 "Notes: …"；按钮 "Update"、"Check again"。
- Updating：stage 文本（`stage_label`），多跳时前缀 "hop N/M: "；`ProgressBar` 显示合并进度 `(hop-1+fraction)/hops`，只增不减（"retry" 阶段除外）；红字 "Do not power off the device."；页内按钮全部禁用。
- Success："✓ Update complete — device is now on v." + "Check again"。
- Failed："✗ " + 映射文案 + "Retry check"。

### 6.3 行为
- 进入页面时若状态为 Idle / Success / Failed → `prepare(&store, true)`；"Check again" 同。
- "Update" → 线程 `execute(&store, &prep, progress)`；`ProgressEvent` 经 channel 回主线程更新。
- 更新期间主窗口 `close-request` 返回 `Stop` 并 toast "Firmware update in progress — please wait."；Ctrl+Q 同样被挡（`quit` action 检查全局 `fw_updating` 标志）。
- `FwStore::open_default()` 与 CLI/TUI 共用，节流与续传状态一致。

### 6.4 错误映射（`errors.rs` 增加 `fw_friendly(&FwUpdateError) -> String`）
`LowBattery` → "Battery below 30 % — charge the device first"；`ReconnectTimeout` → "Device did not come back after the update; power-cycle it and check again"；`ManifestFetch` / `Download` → "Could not reach the update server"；`ManifestSchema` → "Update server returned an invalid manifest"；`Sha256Mismatch` / `PackageInvalid` / `HeaderRejected` / `SignatureRejected` → "Firmware package rejected: " + 明细；`Preflight(s)` / `Transfer{..}` / `Store(s)` → `Display` 原文。

### 6.5 Device 页提示
主窗口首次显示后在线程里跑一次静默检查（`fetch_manifest_cached(force=false)` 24 h 节流、`query_device_status`、`planner::plan`；任何错误静默）。结果：设备版本低于 `MANDATORY_MIN_VERSION` → "⚠ Firmware outdated (old signing era)"（红）；有可用更新 → "⬆ Firmware update available: vX"（黄）；否则不显示。提示行放 Dashboard 顶部（`Device` 分组之前），右侧 "Update" 按钮切换到 Firmware 页并触发检查。

## 7. Logs 页（`pages/logs.rs`）

- 头部：状态徽标 "● Live" / "⏸ Paused (N new lines)" / "● Stream closed"；按钮 "Jump to latest"（Paused 时可用）、"Reconnect"（closed 时可用）。
- 主体：只读等宽 `TextView`（`monospace`、不可编辑、不换行），`TextTag`：`error`（红粗：含 ` ERROR ` / `ERROR:` / `[ERROR]` / ` panicked`）、`warn`（黄：` WARN ` / `WARN:` / `[WARN]`，以及 daemon 的 "… log lines dropped" 行）、`dim`（` DEBUG ` / ` TRACE `）。分类抽成纯函数 `classify(line) -> Level`，规则与 TUI `log_line_style` 相同。
- 环形上限 1000 行：`TextBuffer` 行数超出时删除最早的行。
- 自动滚动：仅当垂直 `Adjustment` 在底部（`value + page_size >= upper - 1.0`）时跟随；否则计数新行并显示 Paused；"Jump to latest" / End 键滚到底并清零。
- 生命周期：页首次映射时 `open_log_stream()`，读线程逐行发 channel；主线程批量追加；主窗口 `close-request`（非固件更新中）时对 socket `shutdown(Both)`；流 EOF / 错误 → "Stream closed"。

## 8. 错误处理与安全

- pkexec 参数只含 `find_helper()` 的绝对路径、固定动作字、`PAM_SERVICES` 中的服务名；不接受用户输入。
- 固件包校验链（manifest sha256、`imfw::parse` 结构、设备端签名与低电量拒绝）原样保留，GUI 无绕过路径；`execute` 不可取消，更新中禁止关窗。
- 日志内容遵循 daemon 既有「不记秘密」规则；GUI 不落盘、不导出。
- 所有动态文本进 toast / `ActionRow` 前 `glib::markup_escape_text`；`TextView` 用纯文本插入。
- 用户可见字符串全部英文。

## 9. 测试

单测：
- `immurok-client::pam`：`helper_output_has_error`；`services_to_repair` 四种开关组合（用 `pam_line_present_in(dir, svc)` 注入临时目录）；`PAM_SERVICES` 路径表；helper 输出解析成 `HelperReport`；退出码 → `PamError` 映射（用假命令模拟不可行则只测纯函数）。
- `immurok-client::fwupdate`：既有测试随搬家继续通过。
- `immurok-gui`：`logs::classify`；环形裁剪函数（输入 N 行 → 保留最后 1000）；`firmware` 的进度合并函数（hop / fraction / retry 回退）与状态→文案函数；`fw_friendly` 映射。

手工验收（真机，GNOME Wayland）：
1. PAM 页三行状态与 `immurok-cli pam check` 一致；Install / Remove 各一次弹 polkit 密码框；取消授权显示 "Authorization cancelled"；Repair 在缺行时可用。
2. 未隔离环境（`IMMUROK_SOCKET` 指向用户级 daemon 或日志模拟）横幅变红；daemon 停掉横幅变灰。
3. Firmware：设备有旧版本时 Check 显示计划，Update 走完并显示新版本（有条件时含两跳）；更新中关窗被挡；电量低于 30 % 提示；断网时 Failed 文案为 "Could not reach the update server"。
4. Device 页出现更新提示，点 Update 跳到 Firmware 页。
5. Logs：打开即见历史尾部并实时滚动；上滚显示 Paused 与计数；Jump to latest 恢复；停 daemon 后显示 Stream closed，起回后 Reconnect 成功。
6. TUI / CLI 回归：`immurok-cli fw check`、`pam check`、`logs`、TUI Firmware/PAM/Logs 页行为不变。
