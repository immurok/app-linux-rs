# immurok-gui：Features 独立页、指纹图标、Keys 页补全

日期：2026-09-21
范围：`crates/immurok-gui`、`crates/immurok-client`、`crates/immurok-cli`（仅解析代码搬迁）

## 背景

侧边栏布局（1dcc9cea）落地后暴露三个问题：

1. Device 页太长——设备状态、双机、5 个功能开关挤在一页。
2. Fingerprints 的侧边栏图标在 Fluent 图标主题下渲染为空：`finger_icon_name()` 用 `IconTheme::has_icon` 探测 `fingerprint-symbolic`（Breeze）/ `auth-fingerprint-symbolic`（Adwaita），探测通过但实际画不出来。
3. Keys 页只有 列表 / 取码·显示·复制公钥 / 删除，没有添加、导入、容量提示、空状态；OTP 码用 30 s toast 显示，不便复制。TUI/CLI 已有这些功能，但解析代码（base32、otpauth URI、andOTP JSON、CSV、OpenSSH/SEC1 私钥）全在 `immurok-cli/src/commands/keys.rs`，GUI 引用不到。

## A. Features 独立页

- 新建 `pages/features.rs`：

  ```rust
  pub struct FeaturesPage { root, switches: Vec<(SettingKey, gtk::Switch)>, toasts }
  impl FeaturesPage {
      pub fn new(toasts) -> Rc<Self>
      pub fn widget(&self) -> &gtk::Widget
      pub fn apply(&self, settings: &Settings)      // 回读开关状态
      pub fn set_daemon_available(&self, ok: bool)  // daemon 不可用时整页 insensitive
  }
  ```

  `dashboard.rs` 里的 Features 组、`wire_switches()`、`apply()` 中的开关回读段整体搬过去，逻辑不变（写入即时、daemon 确认后才翻开关、`state-set` 重入保护）。
- 不新增轮询：Dashboard 的 2 s 轮询已查回 `Settings`。`DashboardPage` 新增 `set_features(&self, Rc<FeaturesPage>)`（内部存 `Weak`），`apply()` / `apply_daemon_down()` 末尾转调。
- `main_window.rs` 侧边栏顺序：Device / Features / Keys / Fingerprints / PAM / Firmware / Logs；Features 图标 `emblem-system-symbolic`。

## B. 指纹图标

- 新增 `crates/immurok-gui/build.rs`，`glib-build-tools` 作 build-dependency，编译 `data/immurok.gresource.xml` → 资源前缀 `/com/immurok/Settings/icons/scalable/actions/immurok-fingerprint-symbolic.svg`。SVG 为自绘 16×16 symbolic（单色、`fill="currentColor"`），不依赖任何系统图标主题。
- `main.rs` 启动时 `gio::resources_register_include!("immurok.gresource")` 并对默认 display 的 `IconTheme` 调用 `add_resource_path("/com/immurok/Settings/icons")`。
- `finger_icon_name()` 直接返回 `"immurok-fingerprint-symbolic"`，去掉主题探测；`fingerprints.rs` 里的 40 px 大图标同样用它。
- Ubuntu 22.04 上 `glib-compile-resources` 来自 `libglib2.0-dev-bin`，是 `libgtk-4-dev` 的依赖；`Makefile` 的 `HAS_GTK_DEV` 判定不用改。

## C. 解析代码下沉到 `immurok-client`

新建 `crates/immurok-client/src/keys_import.rs`，从 `immurok-cli/src/commands/keys.rs` **移动**（非复制）以下项及其单元测试：

| 项 | 用途 |
|----|------|
| `base32_decode` | OTP secret |
| `OtpEntry`、`parse_otpauth_uri`、`parse_andotp_json`、`parse_csv_otpauth`、`split_otp_fields`、`url_decode`、`truncate_utf8` | OTP 单条 / 批量导入 |
| `build_otp_entry_payload`、`build_key_add_cmd`、`KeyAddCat` | 组装 `KEY:OTP_IMPORT` / `KEY:API_IMPORT` |
| `parse_openssh_key`、`parse_sec1_pem` + 112 字节 device payload 组装 | `KEY:IMPORT` |
| `KEY:GENERATE` 的 16 字节名字 payload | 设备上生成 SSH 密钥对 |

`p256`（`ecdh` feature）依赖随之加入 `immurok-client`；`immurok-cli` 里如果没有其他使用者则移除。

`keys.rs` 新增发送函数（全部走 `DaemonClient::connect()?.send_with_timeout(_, GATE_TIMEOUT)`，`OK*` → `Ok(())`，否则 `Err(响应原文)`）：

```rust
pub fn generate_ssh(name: &str) -> Result<(), String>
pub fn import_ssh(name: &str, pem_text: &str) -> Result<(), String>   // 解析失败 Err(具体格式错误)
pub fn add_otp(entry: &OtpEntry) -> Result<(), String>
pub fn add_api(name: &str, value: &str) -> Result<(), String>
pub fn parse_otp_import_file(path_hint: &str, content: &str) -> Result<(Vec<OtpEntry>, usize /*skipped*/), String>
pub fn capacity(cat: KeyCategory) -> u8   // KEY_MAX_SSH / OTP / API
```

CLI `run_generate_ssh` / `run_import_ssh` / `run_import_otp` / `run_add` 与 TUI `action_key_add` / `action_key_generate` 改为调用上述函数，用户可见行为与输出不变。

## D. Keys 页

### 列表
- 分组标题 `SSH (3/32)`、`OTP (12/128)`、`API (0/50)`；header 右侧 `+` 按钮（`PreferencesGroup::header_suffix`，libadwaita 1.1，`Cargo.toml` 开 `libadwaita` 的 `v1_1` feature——Ubuntu 22.04 的下限正是 1.1）。满员时禁用，tooltip "Keystore full — delete an entry first"；设备未连接时禁用，tooltip "Device not connected"。连接状态由 Dashboard 轮询经 `KeysPage::set_connected(bool)` 同步（与 A 的 `set_features` 同一路径）。
- 组内无条目：显示一行 `dim-label` 的 `ActionRow`，文案 `No <cat> entries yet — press + to add one`。

### 添加对话框（`key_add_dialog.rs`）
`adw::Window`，transient、modal、不可缩放；内容 `PreferencesPage` + 一组 `ActionRow`，每行 suffix 放 `gtk::Entry`（密码用 `gtk::PasswordEntry`）——`adw::EntryRow` 是 1.2，22.04 没有；底部 Cancel + 主按钮。提交时主按钮 insensitive、文字改 "Touch the device…"；成功则关窗、toast `"<cat> '<name>' added"`、`reload()`；失败保留窗口、把错误显示在对话框内红色 label（不吞错误码）。

| 分类 | 输入 | 主按钮 | 附加 |
|------|------|--------|------|
| SSH | Name（≤15 字节，超长按字节截断并提示） | Generate on device | "Import from file…"：`gtk::FileChooserNative`（`FileDialog` 是 4.10）选 OpenSSH/SEC1 私钥，仅 P-256 未加密；格式不符显示具体错误 |
| OTP | Name（≤29）、Service（≤29）、Secret（base32） | Add | Secret 框粘贴 `otpauth://` URI 时自动拆填三项；提交前 base32 校验；"Import from file…" 见下 |
| API | Name（≤31）、Value（`gtk::PasswordEntry`） | Add | — |

### OTP 批量导入
- 文件选择器 → `parse_otp_import_file` → `confirm()`：`Import N entries? (M skipped: only TOTP/SHA1/6-digit/30s supported)`；超容量直接报错不导入。
- 逐条 `add_otp`：第一条需触摸，后续在固件 10 s cooldown 内免触摸（daemon `KEY:OTP_IMPORT` 注释：FP gated on the first commit, cooldown rides the rest）；对话框主按钮显示 `Importing k/N: <name>` 进度；一条失败即停，报告 `Imported k/N`，然后 `reload()`。

### OTP 取码行内显示
- "Get code" 成功后不再 toast：该行 subtitle 改为 `123 456`（`monospace` + `title-3`），suffix 加 "Copy" 按钮；30 s 后或页面切走（`ViewStack` visible-child 变化）时恢复原 subtitle、移除 Copy。
- API 的 "Show" 保持 30 s toast（值长度不定）。

## E. 测试

- 移动到 `immurok-client` 的解析测试原样保留；新增：otpauth URI → 三字段拆填、`capacity` 与满员判断、SSH 名字字节截断、`parse_otp_import_file` 按扩展名分流。
- `cargo test --workspace`；CLI 行为不变由既有测试兜底。
- GUI 用 Broadway + 无头 Chromium CDP 截图验收：Features 页开关回读、Fingerprints 图标在 Fluent 主题下可见、Keys 三种添加、OTP 行内取码。

## API 版本约束

项目下限 Ubuntu 22.04 = GTK 4.6 / libadwaita 1.1。绑定 crate 只开到 `v1_1`，不得使用 `EntryRow`、`MessageDialog`（adw）、`ToolbarView`、`gtk::FileDialog` 等更新 API。

## 不做

- 条目重命名（固件无命令）；加密私钥、Ed25519 导入；Keys 页自己轮询。
