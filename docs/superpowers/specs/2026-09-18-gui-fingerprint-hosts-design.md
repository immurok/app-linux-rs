# Linux GUI 阶段三：指纹管理 + 双机管理 设计稿

日期：2026-09-18
状态：设计定稿，待写实施计划
前置：阶段一（`2026-09-14-gui-quickfill-design.md` §11 验收完成，分支 `worktree-linux-gui-phase1`，0.7.0）
参考实现：`app-macos/Sources/FingerprintView.swift`、`app-macos/Sources/DualHostView.swift`、`app-macos/Sources/FingerprintGateSheet.swift`；Linux 侧 `immurok-cli/src/tui/app.rs`（`action_enroll` / Hosts 菜单）、`immurok-cli/src/enroll_hint.rs`

---

## 1. 目标

让 `immurok-gui` 拥有与 macOS 一致的指纹管理和双机管理：

- 指纹：列出已登记槽位、登记（六步引导、门控验证、overlap 提示）、删除、本地命名、Test Fingerprint、登记「切换指纹」（槽 5）。
- 双机：Two Hosts 两张卡片，显示 Host 1 / Host 2 的绑定状态与「本机」标记；配对 / 解除本机；解绑另一台主机（门控）。

范围之外（明确不做）：
- Factory reset（macOS 没有；TUI 保留）。
- PAM / Firmware / Logs 页（另起计划）。
- TUI / CLI 对 Overlap 的专门文案（follow-up；本计划只改 `immurok-common` 的枚举）。
- 指纹名字同步到设备或另一台电脑；登记进度的推送通道（沿用轮询）。

## 2. 进程与信任边界

不变。GUI 仍是 daemon 的壳（同 uid、`SO_PEERCRED`），所有指纹门在设备上，长会话（登记、门控删除、解绑）由 daemon 阻塞执行，GUI 只等回复或轮询进度。指纹名字是纯本地展示数据，只存 `~/.config/immurok/gui.json`，不进 daemon、不进设备（与 macOS 存 UserDefaults 同理）。

## 3. 协议事实（2026-09-18 核对）

| 命令 | 回复 | 门控 | 超时 |
|---|---|---|---|
| `FP:LIST` | `OK:<bitmap>`（u8，bit N = 槽 N） | 否 | 默认 |
| `FP:ENROLL:<slot>` | `OK:ENROLL_STARTED`（过门后立刻返回，不等六次采集） | 有已登记指纹时是 | 40 s |
| `FP:STATUS` | `OK:IDLE` 或 `OK:<status>:<current>:<total>`（十进制原始码，**已透传 6**） | 否 | 默认 |
| `FP:ENROLL_CANCEL` | `OK:ENROLL_CANCELLED` | 否 | 默认 |
| `FP:DELETE:<slot>` | `OK:DELETED` | 有已登记指纹时是 | 40 s |
| `FP:VERIFY` | `OK:MATCH` / `OK:NO_MATCH` | 是 | 40 s |
| `SLOT:STATUS` | `OK:<bitmap>:<active>:<mine>`（bit0/1 = 槽 1/2；`mine`=0 表示未证明）或 `OK:UNSUPPORTED` | 否 | 默认 |
| `SLOT:CLEAR` | `OK:CLEARED` / `OK:CLEARED_UNCONFIRMED` / `OK:CLEARED_LOCAL_ONLY`；`ERROR:SLOT_CLEAR_REFUSED` | 否 | 默认 |
| `SLOT:CLEAR:<n>` | `OK:CLEARED` / `OK:CLEARED_SLOT_UNPAIRED`；`ERROR:INVALID_SLOT` / `DUAL_HOST_UNSUPPORTED` / `SLOT_CLEAR_FAILED:<e>` | 是（总是） | 40 s |
| `PAIR:START` | `OK:PAIRED` | 第二台主机时是 | 150 s |
| `PAIR:PROGRESS` | `OK:<IDLE\|WAIT_FP\|WAIT_BUTTON\|ECDH\|DONE\|FAILED>` | 否 | 默认 |

登记状态码（`protocol.rs`）：0 waiting、1 captured、2 processing、3 lift finger、4 complete、0xFF failed；固件 mode-1 的 **6 = overlap**（两次按压太相似，重新按压，不算失败）。槽位：0–4 认证指纹（`MAX_FINGERPRINT_SLOTS`=5），5 = 切换指纹（`SWITCH_FINGER_SLOT`，固件 ≥ 1.6.4 / daemon `SLOT:STATUS` 非 UNSUPPORTED 时可用）。门控超时 30 s，最多失败 3 次。

## 4. `immurok-common` 小改

- `protocol.rs`：加 `pub const ENROLL_OVERLAP: u8 = 0x06;`
- `types.rs`：`EnrollEvent` 加 `Overlap` 分支，`from_notification` 把 0x06 映射到它（其余未知码仍 → `Failed`）。加单测。
- daemon 只是编译通过，无行为变化（`FP:STATUS` 本就透传原始码；`enroll_tx` 广播在 daemon 内无订阅者）。

## 5. `immurok-client` 新模块

保持同步 std、一请求一连接的约定。

### 5.1 `fingerprint.rs`
- `pub struct FpSlots { pub bitmap: u8 }`：`is_enrolled(slot)`、`auth_count()`（槽 0–4）、`first_free_auth_slot() -> Option<u8>`、`switch_enrolled()`（槽 5）、`AUTH_SLOTS = 0..5`、`SWITCH_SLOT = 5`。
- `pub fn fp_list() -> Result<FpSlots, String>`。
- `pub fn enroll_start(slot: u8) -> Result<(), String>`：`FP:ENROLL:<slot>`，`GATE_TIMEOUT`，回复须为 `OK:ENROLL_STARTED`。
- `pub enum EnrollStatus { Idle, Waiting, Captured { current: u8, total: u8 }, Processing, LiftFinger, Overlap, Complete, Failed }` + `pub fn parse_fp_status(line: &str) -> Option<EnrollStatus>`（`OK:IDLE`；`OK:<n>:<c>:<t>`，n=6 → Overlap，未知 n → Failed）+ `pub fn fp_status() -> Result<EnrollStatus, String>`。
- `pub fn enroll_cancel()`（best effort，忽略错误）。
- `pub fn fp_delete(slot: u8) -> Result<(), String>`（`GATE_TIMEOUT`）。
- `pub fn fp_verify() -> Result<bool, String>`（`GATE_TIMEOUT`；`OK:MATCH` → true，`OK:NO_MATCH` → false）。

### 5.2 `enroll_session.rs`
阻塞式驱动器，把 TUI `action_enroll` 的轮询状态机搬成可测的纯逻辑：

```rust
pub enum EnrollProgress {
    GateWaiting,                                  // enroll_start 发出前
    Started,                                      // OK:ENROLL_STARTED
    Step { next_step: u8, captured: u8, total: u8 }, // 下一步该按第几帧（1..=6）
    LiftFinger,
    Overlap,                                      // 不推进 step
    Processing,
    Complete,
    Failed(String),                               // 含设备断开、超时、daemon 错误
}
pub enum Continue { Go, Stop }
pub fn run_enrollment(slot: u8, tick: impl FnMut(EnrollProgress) -> Continue);
```

行为：
- 先回调 `GateWaiting`，再 `enroll_start(slot)`；失败 → `Failed(err)` 结束。
- 成功 → `Started`，随后每 150 ms 新连接 `fp_status()`：状态变化才回调（Waiting/Captured → `Step`，映射 `next_step = current + 1`；LiftFinger；Overlap；Processing）；`Complete` / `Failed` 结束。
- IDLE 连续 ~3 s 时用 `fp_list()` 兜底：目标槽位已置位 → `Complete`。
- 总时长上限 360 s → `Failed("timed out")`。`fp_status` 回 `NOT_CONNECTED` → `Failed("device disconnected")`。
- 回调返回 `Stop` → 发 `enroll_cancel()` 并结束（不再回调）。
- 内部拆成 `drive(poll: impl FnMut() -> Result<EnrollStatus,String>, bitmap: impl FnMut() -> Result<FpSlots,String>, tick, clock)`，用假 poll 源单测事件序列。

### 5.3 `hosts.rs`
- `pub struct SlotStatus { pub supported: bool, pub slot1: bool, pub slot2: bool, pub active: u8, pub mine: Option<u8> }` + `parse_slot_status(line)`（`OK:UNSUPPORTED` → `supported=false`）+ `slot_status()`。
- `pub fn clear_other_slot(n: u8) -> Result<(), String>`（`SLOT:CLEAR:<n>`，`GATE_TIMEOUT`，`OK:CLEARED*` 均成功）。
- `pub fn clear_own_slot() -> Result<(), String>`（`SLOT:CLEAR`，`OK:*` 成功；现有 Dashboard 逻辑搬入）。
- `pub enum PairProgress { Idle, WaitFp, WaitButton, Ecdh, Done, Failed }` + `parse_pair_progress` + `pair_progress()`；`pub fn pair_start() -> Result<(), String>`（150 s，须回 `OK:PAIRED`）。

### 5.4 `enroll_hint.rs`
`step_hint(step)` / `step_arrow(step)` 从 `immurok-cli/src/enroll_hint.rs` `git mv` 到 `immurok-client/src/enroll_hint.rs`（测试同行），`immurok-cli/src/main.rs` 改为 `use immurok_client::enroll_hint;`，其他引用不动。

## 6. 本地设置文件 `settings_store.rs`

新建 `immurok-gui/src/settings_store.rs`，结构与阶段二计划（`2026-09-14-gui-quickfill-input.md` Task 2）**完全一致**：`OutputChoice`、`GuiSettings { output, portal_restore_token, x11_hotkey }`、`path()`、`load_from/save_to`（0600、损坏回默认）、`load/save`。本阶段追加字段：

```rust
#[serde(default)]
pub fingerprint_names: BTreeMap<u8, String>,   // 槽 0–4；槽 5 永远不存
```

助手：`fingerprint_name(&self, slot) -> String`（槽 5 → "Switch Host"；无记录 → "Finger {slot+1}"）、`set_fingerprint_name(slot, name)`（空白 → 删除记录）、`clear_fingerprint_names()`（解除本机配对成功后调用）。阶段二计划落地时改为在此文件加字段，不再新建。

## 7. GTK 线程模型

不变：短请求走 `pages::run_blocking`。长会话（`run_enrollment`）在 `std::thread::spawn` 里跑，事件经 `async-channel`（新增依赖 `async-channel = "2"`，阶段二同样需要）发回主线程；UI 用 `glib::spawn_future_local` 循环 `rx.recv().await` 刷新对话框；回调的 `Continue` 由一个 `Arc<AtomicBool>` cancel 标志决定。门控类阻塞调用（删除、验证、解绑）走 `run_blocking`，Cancel 另起线程发 `GATE:CANCEL`。

## 8. Fingerprints 页（`pages/fingerprints.rs`）

ViewStack 第三页，标题 "Fingerprints"，图标 `fingerprint-symbolic`（缺失时 `dialog-password-symbolic`）。

- 顶部 `PreferencesGroup`，description："immurok lets you use your fingerprint to unlock this computer and authorize sudo."
- 图标行：`gtk::FlowBox`，每个已登记槽一张卡片（`FingerCard`）：圆形指纹图标、名字标签；点名字变 `gtk::Entry` 内联编辑（Enter 保存到 gui.json，Esc / 失焦取消）；槽 5 固定 "Switch Host"，不可编辑，卡片下灰字 "This finger only switches hosts — it never unlocks or authenticates"；悬停时右上角出现红色 `user-trash-symbolic` 按钮。末尾一张 "+" 卡片 "Add Fingerprint"，在未连接 / 登记中 / 加载中 / 无空认证槽时禁用。
- 底部按钮行："Add switch fingerprint"（仅 `slot_status().supported && !switch_enrolled()` 时显示）、"Test Fingerprint"、"Refresh"。
- 状态：加载中 spinner + "Fetching fingerprint info from device..."；未连接 `StatusPage` "Device not connected"。
- 数据：`fp_list()` + `slot_status()`；在页首次显示、连接状态翻转、每次操作完成、点 Refresh 时刷新。连接状态由本页自己判断：窗口可见且本页为当前页时每 2 s `query_status()` 一次（非门控、单次往返），`connected` 翻转即重载；与 Dashboard 的轮询互不依赖。"Add Fingerprint" 登记 `first_free_auth_slot()`。页级 `busy: Cell<bool>` 在任何门控/登记会话进行中置位，禁用全部操作按钮，同时暂停本页轮询。

## 9. 门控等待对话框（`gate_dialog.rs`）

可复用模态 `gtk::Window`（transient、不可缩放、不可关闭到后台）：标题、30 s 倒计时环（`gtk::DrawingArea` 画弧，随 100 ms 定时器递减）、提示文本、Cancel。

```rust
pub async fn run<T: Send + 'static>(
    parent: &impl IsA<gtk::Window>, title: &str, hint: &str,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Option<Result<T, String>>   // None = 用户取消
```

`work` 在 `run_blocking` 里跑；Cancel 另起线程发 `GATE:CANCEL`，对话框立即关闭并返回 `None`（后续到达的结果丢弃）。结果到达时文本切换："Verified, processing…"（`Ok`）/ 错误映射（§12）后短暂显示再关闭。四处共用：删除、Test Fingerprint、解绑另一台主机、以及登记对话框的门控页（§10 复用其绘制组件，不再弹第二个窗）。

## 10. 登记对话框（`enroll_dialog.rs`）

- 前置确认（仅已有指纹时）：`gtk::MessageDialog` "Add a New Fingerprint" / "The device first asks you to verify with an already-enrolled finger. After that, switch to the NEW finger." / Cancel / Start。
- 对话框内两页（`gtk::Stack`）：门控页（复用 §9 的环 + "Verify with an enrolled finger"）→ 收到 `Started` 切引导页。
- 引导页：大号步骤标题 `step_hint(next_step)`、方向箭头 `step_arrow`（步骤 2–5 显示，其余隐藏）、"Captured (n/6)" + `ProgressBar`、副文本（Waiting → "Place your finger on the sensor..."，LiftFinger → "Lift your finger, then press again..."，Processing → "Processing..."）、Cancel。
- Overlap：副文本 "Too similar — shift your finger and press again"，卡片加 CSS class `overlap`（内联 `CssProvider`，橙色背景闪 300 ms + 左右抖动 keyframes），步数与进度不变。
- Complete：关闭对话框，toast "Fingerprint enrolled successfully!"，若是新认证槽写入默认名 "Finger N"，刷新页面。
- Failed：对话框内红字（映射后的原因；断开 → "Device disconnected before enrollment finished. Reconnect the device and try again."；其余 → "Enrollment failed. Please try again."）+ Close。
- Cancel / 关窗：置 cancel 标志 → 驱动器发 `FP:ENROLL_CANCEL`；对话框立即关闭。
- 登记切换指纹：同一对话框，`slot = 5`，标题 "Add switch fingerprint"，副标题 "This finger will only switch between the two hosts."

## 11. Device 页的 Two Hosts 分组（`pages/hosts.rs` + 改 `pages/dashboard.rs`）

- Dashboard 删除现有 "Pairing" 行与 `wire_pair_button`；新增 `HostsGroup`（在 `pages/hosts.rs`）作为 "Two Hosts" `PreferencesGroup`，`dashboard.rs` 只负责把轮询结果（`DeviceStatus`、`paired`、`SlotStatus`）喂给它，保持每个文件 < 400 行。
- 分组内容：横向两张卡片 Host 1 / Host 2：`computer-symbolic` 图标（绑定时 accent 色）、"This computer" 标签（`mine == Some(n)` 才显示）、绿/灰圆点 + "Bound" / "Empty"。
- 特殊状态：`supported == false` → 一行 "This firmware does not support two hosts."，隐藏卡片；未连接 → "Device not connected. Host binding status will appear once connected."
- 按钮：
  - 本机槽已绑 → "Unpair"：确认框（现有文案）→ `clear_own_slot()` → 成功后 `clear_fingerprint_names()`、toast "Unpaired"。
  - 本机槽未绑（或未配对且 `mine` 未知）→ "Pair"：`pair_start()` 在 `run_blocking`，同时每 300 ms `pair_progress()` 把 WAIT_FP / WAIT_BUTTON / ECDH 显示为卡片下方进度文本（"Touch an enrolled finger on the device" / "Press the button on the device" / "Exchanging keys…"）；`pair_busy` 逻辑沿用。
  - 另一槽已绑 → "Unbind"：确认框 "Unbind Host N?" / "That computer will no longer be able to authenticate with this device until it pairs again. Requires one touch of an enrolled finger." → §9 门控对话框（标题 "Unbind Host N"，提示 "Touch a registered fingerprint on the device to confirm unbinding the other host."）→ `clear_other_slot(n)` → toast "Host N unbound"。
  - 另一槽为空 → 无按钮，灰字 "To fill this slot, open immurok on that computer and click Pair."
- 分组底部提示：两槽都占 → "Both host slots are in use. To swap one out, unbind it on that computer first."；否则 → "This device can be bound to up to two computers. Touch the switch fingerprint to move between them."
- 数据：Dashboard 现有 2 s 轮询在 `connected` 时追加 `slot_status()`（一次往返，非门控）。

## 12. 错误处理

- `immurok-gui/src/errors.rs`：`pub fn friendly(raw: &str) -> String`，把阶段一 `quickfill.rs` 的 `friendly_error` 搬来共用并扩展：`FP-gate timeout` → "Timed out waiting for a fingerprint touch."、`FP-gate cancelled` → "Cancelled"、`FP-gate failed` → "Fingerprint didn't match after multiple attempts."、`NOT_CONNECTED` → "Device not connected"、`BUSY` → "Device busy, try again"、`INVALID_SLOT` → "Invalid slot"、`DUAL_HOST_UNSUPPORTED` → "This firmware does not support two hosts"、`SLOT_CLEAR_REFUSED` → "The device refused to clear the slot"、`ENROLL_FAILED:` / `DELETE_FAILED:` / `SLOT_CLEAR_FAILED:` 前缀剥掉后递归映射，其余原样。
- 所有动态文本进 toast / 行标题前 `glib::markup_escape_text`。
- 页级 `busy` 防并发门控；daemon 不可达时 Fingerprints 页显示 "Daemon unavailable: …"。

## 13. 安全注意

- 指纹名字不是秘密，但 gui.json 仍 0600（阶段二约定）。
- 删除、解绑、Test 全部走设备门控，GUI 不能绕过；Cancel 只发 `GATE:CANCEL`。
- 登记会话中用户关闭主窗口：对话框随之关闭并发 `FP:ENROLL_CANCEL`。
- 不记录任何登记/验证结果到日志。

## 14. 测试

单测（`cargo test --workspace`）：
- `immurok-common`：`EnrollEvent::from_notification(0x06,…) == Overlap`。
- `immurok-client::fingerprint`：`parse_fp_status`（IDLE、`6:2:6`、未知码）、`FpSlots` 助手（空、满、只有槽 5）。
- `immurok-client::enroll_session::drive`：假 poll 序列 waiting→captured(1)→lift→captured(2)…→complete 产生的事件序列；overlap 不改 step；Failed 终止；`Stop` 回调触发 cancel；IDLE 兜底用位图判完成；360 s 超时。
- `immurok-client::hosts`：`parse_slot_status`（UNSUPPORTED、`mine=0`、两槽）、`parse_pair_progress`。
- `immurok-client::enroll_hint`：原测试。
- `immurok-gui::settings_store`：默认值、名字读写、槽 5 固定名、损坏文件回默认、0600。
- `immurok-gui::errors`：映射表。

手工验收（真机，GNOME Wayland）：
1. 登记一根认证指纹走完六步，卡片出现、默认名 "Finger N"；daemon 日志无秘密。
2. 同一位置连按两次：橙闪抖动、"Too similar…"、步数不变。
3. 登记中途 Cancel：对话框关闭，`immurok-cli logs` 有 ENROLL_CANCEL。
4. 删除一根：确认框 → 触摸 → 卡片消失；不触摸 30 s → 超时文案。
5. 改名后重开窗口名字还在；解除配对后名字清空。
6. Test Fingerprint：匹配 / 不匹配各一次。
7. 槽 5 空时 "Add switch fingerprint" 可见；登记后 Dashboard 提示语切换、卡片显示 "Switch Host" 不可改名。
8. 第二台电脑配对后 Two Hosts 两个 Bound、"This computer" 标在正确的槽；本机 Unbind 另一台（触摸）后变 Empty。
9. 旧固件设备：Fingerprints 页无槽 5、Two Hosts 显示不支持提示。
10. 未连接：两页均显示对应提示，按钮禁用。
