# immurok-gui 视觉整形（方案 A）— 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 libadwaita HIG 内让 immurok-gui 有视觉锚点、语义一致的图标、更少的导航项、有引导的空状态和固定的主操作位置。

**Architecture:** 侧边栏左列变成 `Box`（设备卡 + 分隔线 + 页面列表），设备卡由 Dashboard 的 2 s 轮询喂数据（与 Features/Keys 同模式）；`FirmwarePage` 的根从 `PreferencesPage` 降为 `PreferencesGroup` 嵌进 Device 页；Keys / Fingerprints / PAM 各自把底部按钮搬到分组 header suffix，空态改为可激活行。

**Tech Stack:** gtk4-rs 0.9 / libadwaita-rs 0.7（只开 `v1_1`）。

**Spec:** `docs/superpowers/specs/2026-09-21-gui-visual-polish-design.md`

## Global Constraints

- API 下限 GTK 4.6 / libadwaita 1.1：不用 `adw::EntryRow` / `adw::MessageDialog` / `adw::ToolbarView` / `adw::Banner` / `gtk::FileDialog`；`PreferencesGroup::set_header_suffix` 是 1.1 可用。
- 不新增自定义 CSS provider；只用 libadwaita 自带样式类（`accent`、`dim-label`、`caption`、`title-4`、`flat`、`suggested-action`、`navigation-sidebar`、`card`）。
- 用户可见文案英文。
- 闭包不得捕获它所挂的控件本身；页面对象在闭包里持 `Weak`。
- 每个 Task：`cargo build -p immurok-gui 2>&1 | grep -E "^(warning|error)"` 为空，`cargo test --workspace` 全绿，提交信息中文、前缀 `gui:`。
- 工作目录 `app-linux-rs/`。目视验收用 Broadway（已安装实例占着单实例 app id）：`gtk4-broadwayd :5` 与 `dbus-run-session -- env GDK_BACKEND=broadway BROADWAY_DISPLAY=:5 ./target/debug/immurok-gui` 各自后台起，headless Chromium `--remote-debugging-port=9333` + Node 26 内置 WebSocket 走 CDP 点侧边栏行并 `Page.captureScreenshot`；结束 `pkill -f "debug/immurok-gui"; pkill -f "gtk4-broadwayd :5"`。截图放 `/tmp/claude-1000/`。

---

### Task 1: 侧边栏设备卡 + 图标统一 + 窗口尺寸

**Files:**
- Create: `crates/immurok-gui/src/sidebar.rs`
- Modify: `crates/immurok-gui/src/main.rs`（`mod sidebar;`）
- Modify: `crates/immurok-gui/src/main_window.rs`（左列结构、图标名、默认尺寸、`dashboard.set_sidebar`）
- Modify: `crates/immurok-gui/src/pages/dashboard.rs`（删 Device 分组三行；加 `set_sidebar` 喂数据）
- Modify: `crates/immurok-gui/data/icons/scalable/actions/immurok-fingerprint-symbolic.svg`（整文件替换）

**Interfaces:**
- Produces: `sidebar::SidebarStatus { pub fn new() -> Rc<Self>; pub fn widget(&self) -> &gtk::Widget; pub fn apply(&self, status: &DeviceStatus, paired: bool); pub fn apply_daemon_down(&self) }`；`DashboardPage::set_sidebar(&self, s: &Rc<SidebarStatus>)`（内部 `Weak`）。
- Consumes: `immurok_client::status::DeviceStatus { connected, name, battery: u8, fw_version, device_unpaired }`。

- [ ] **Step 1: `sidebar.rs`**

```rust
//! Sidebar device card: the always-visible anchor for "what state is the
//! device in". Fed from the Dashboard's 2 s poll; owns no I/O.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::status::DeviceStatus;

pub struct SidebarStatus {
    root: gtk::Widget,
    icon: gtk::Image,
    name: gtk::Label,
    state: gtk::Label,
    battery_box: gtk::Box,
    battery_bar: gtk::LevelBar,
    battery_pct: gtk::Label,
    fw: gtk::Label,
}

impl SidebarStatus {
    pub fn new() -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        root.set_margin_top(14);
        root.set_margin_bottom(10);
        root.set_margin_start(14);
        root.set_margin_end(14);

        let icon = gtk::Image::from_icon_name("bluetooth-disconnected-symbolic");
        icon.set_pixel_size(32);
        icon.set_valign(gtk::Align::Start);
        root.append(&icon);

        let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
        col.set_hexpand(true);
        let name = gtk::Label::builder().label("immurok").xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
        name.add_css_class("title-4");
        let state = gtk::Label::builder().label("Connecting to daemon…").xalign(0.0).wrap(true).build();
        state.add_css_class("caption");
        state.add_css_class("dim-label");

        let battery_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        battery_box.set_visible(false);
        let battery_bar = gtk::LevelBar::builder().min_value(0.0).max_value(1.0).hexpand(true).valign(gtk::Align::Center).build();
        // Default LevelBar offsets: low < 0.25 → red, high < 0.75 → yellow, else green.
        let battery_pct = gtk::Label::builder().xalign(1.0).build();
        battery_pct.add_css_class("caption");
        battery_box.append(&battery_bar);
        battery_box.append(&battery_pct);

        let fw = gtk::Label::builder().label("").xalign(0.0).build();
        fw.add_css_class("caption");
        fw.add_css_class("dim-label");

        col.append(&name);
        col.append(&state);
        col.append(&battery_box);
        col.append(&fw);
        root.append(&col);

        Rc::new(Self { root: root.upcast(), icon, name, state, battery_box, battery_bar, battery_pct, fw })
    }

    pub fn widget(&self) -> &gtk::Widget {
        &self.root
    }

    pub fn apply(&self, s: &DeviceStatus, paired: bool) {
        self.name.set_text(if s.name.is_empty() { "immurok" } else { &s.name });
        let (text, icon, accent) = if s.device_unpaired {
            ("No longer paired with this host", "bluetooth-disconnected-symbolic", false)
        } else if s.connected {
            ("Connected", "bluetooth-active-symbolic", true)
        } else if paired {
            ("Paired, not connected", "bluetooth-disconnected-symbolic", false)
        } else {
            ("Not paired", "bluetooth-disabled-symbolic", false)
        };
        self.state.set_text(text);
        self.icon.set_icon_name(Some(icon));
        if accent {
            self.icon.add_css_class("accent");
        } else {
            self.icon.remove_css_class("accent");
        }
        self.battery_box.set_visible(s.connected);
        if s.connected {
            self.battery_bar.set_value(f64::from(s.battery.min(100)) / 100.0);
            self.battery_pct.set_text(&format!("{}%", s.battery.min(100)));
            self.fw.set_text(&format!("Firmware {}", s.fw_version));
        } else {
            self.fw.set_text("");
        }
        let _ = glib::markup_escape_text(""); // keep glib import used if nothing else does
    }

    pub fn apply_daemon_down(&self) {
        self.state.set_text("Daemon unavailable");
        self.icon.set_icon_name(Some("bluetooth-disabled-symbolic"));
        self.icon.remove_css_class("accent");
        self.battery_box.set_visible(false);
        self.fw.set_text("");
    }
}
```

（`glib` 只用于 `use gtk::glib;`——若无其他用途，删掉那行 `let _ = …` 和 `use gtk::glib;` 以免 unused warning；`gtk::pango` 需 `gtk4` 的 re-export，`gtk::pango::EllipsizeMode` 可用。）

`main.rs` 加 `mod sidebar;`（字母序，`mod settings_store;` 之后）。

- [ ] **Step 2: `main_window.rs`**

1. 默认尺寸 `default_width(900)`、`default_height(620)`。
2. 图标：`dashboard` → `"bluetooth-active-symbolic"`；`features` → `"preferences-other-symbolic"`；`pam` → `"security-high-symbolic"`；`keys`、`logs` 不变；`fingerprints` 仍 `finger_icon_name()`。（Firmware 项在 Task 2 删除，此处先不动。）
3. 左列：把 `let sidebar = build_sidebar(&stack);` 之后改为

```rust
        let status = crate::sidebar::SidebarStatus::new();
        let left = gtk::Box::new(gtk::Orientation::Vertical, 0);
        left.set_width_request(200);
        left.append(status.widget());
        left.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        left.append(&sidebar);
        sidebar.set_vexpand(true);
        dashboard.set_sidebar(&status);
        let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        body.append(&left);
        body.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        body.append(&stack);
```

   `build_sidebar` 里 `list.set_width_request(180)` 改为 200（或删掉，由 `left` 决定）。`set_data` 段加 `unsafe { window.set_data("sidebar-status", status) };`。

- [ ] **Step 3: `dashboard.rs`**

1. 删除 `// ── Device ──` 分组（`device` PreferencesGroup 与 `status_row` / `battery_row` / `fw_row` 三行）及对应字段、初始化、`apply()` / `apply_daemon_down()` 里对这三行的 `set_subtitle`。
2. 仿 `features` 加字段 `sidebar: RefCell<Option<Weak<crate::sidebar::SidebarStatus>>>`、`pub fn set_sidebar(&self, s: &Rc<…>)`、私有 `fn sidebar(&self)`；`apply()` 开头（`self.paired.set(paired)` 之后）加 `if let Some(s) = self.sidebar() { s.apply(status, paired); }`；`apply_daemon_down()` 加 `if let Some(s) = self.sidebar() { s.apply_daemon_down(); }`。
3. 文件头注释改为 `//! Dashboard: two hosts, firmware. Device status lives in the sidebar card (`sidebar.rs`) and feature toggles in `features.rs`; both are fed from this page's poll.`（Firmware 在 Task 2 才进来，注释可先写好。）

- [ ] **Step 4: 指纹图标重绘**

整文件替换 `crates/immurok-gui/data/icons/scalable/actions/immurok-fingerprint-symbolic.svg`：

```xml
<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><g fill="#2e3436"><path d="M1.70 5.66A6.95 6.95 0 0 1 14.30 5.66L12.58 6.47A5.05 5.05 0 0 0 3.42 6.47Z"/><circle cx="2.56" cy="6.06" r="0.95"/><circle cx="13.44" cy="6.06" r="0.95"/><path d="M4.01 14.29A6.95 6.95 0 0 1 1.47 10.98L3.25 10.33A5.05 5.05 0 0 0 5.10 12.74Z"/><circle cx="4.56" cy="13.51" r="0.95"/><circle cx="2.36" cy="10.65" r="0.95"/><path d="M3.45 8.60A4.55 4.55 0 0 1 12.55 8.60L10.65 8.60A2.65 2.65 0 0 0 5.35 8.60Z"/><circle cx="4.40" cy="8.60" r="0.95"/><circle cx="11.60" cy="8.60" r="0.95"/><path d="M3.45 8.60L3.45 11.00L5.35 11.00L5.35 8.60Z"/><circle cx="4.40" cy="8.60" r="0.95"/><circle cx="4.40" cy="11.00" r="0.95"/><path d="M10.65 8.60L10.65 13.20L12.55 13.20L12.55 8.60Z"/><circle cx="11.60" cy="8.60" r="0.95"/><circle cx="11.60" cy="13.20" r="0.95"/><path d="M5.85 8.60A2.15 2.15 0 0 1 10.15 8.60L8.25 8.60A0.25 0.25 0 0 0 7.75 8.60Z"/><circle cx="6.80" cy="8.60" r="0.95"/><circle cx="9.20" cy="8.60" r="0.95"/><path d="M5.85 8.60L5.85 14.00L7.75 14.00L7.75 8.60Z"/><circle cx="6.80" cy="8.60" r="0.95"/><circle cx="6.80" cy="14.00" r="0.95"/><path d="M8.25 8.60L8.25 10.80L10.15 10.80L10.15 8.60Z"/><circle cx="9.20" cy="8.60" r="0.95"/><circle cx="9.20" cy="10.80" r="0.95"/></g></svg>
```

`build.rs` 会重新编译 gresource（`cargo build` 自动；若未触发，`touch build.rs`）。

- [ ] **Step 5: 编译 + 目视 + 提交**

Build 无 warning；测试全绿。Broadway 截 Device 页：左上有设备卡（图标 / 名字 / Connected / 电量条 / Firmware x.y.z），侧边栏图标为蓝牙 / 滑杆 / 钥匙 / 指纹 / 盾 / 齿轮(Firmware, 下个 Task 删) / 终端；Device 页顶部直接是 Two Hosts。

```bash
git add crates/immurok-gui/src/sidebar.rs crates/immurok-gui/src/main.rs crates/immurok-gui/src/main_window.rs crates/immurok-gui/src/pages/dashboard.rs crates/immurok-gui/data/icons/scalable/actions/immurok-fingerprint-symbolic.svg
git commit -m "gui: 侧边栏设备卡作为状态锚点，图标语义统一，指纹图标重绘为 Adwaita 权重"
```

---

### Task 2: Firmware 并入 Device 页

**Files:**
- Modify: `crates/immurok-gui/src/pages/firmware.rs`（根改 `PreferencesGroup`，布局改行式，`latest_row` 删除）
- Modify: `crates/immurok-gui/src/pages/dashboard.rs`（嵌入 firmware 组；删 fw_hint 相关）
- Modify: `crates/immurok-gui/src/main_window.rs`（删 Firmware 导航项、`silent_check`、`connect_update_clicked` 跳转）

**Interfaces:**
- `FirmwarePage::new(toasts) -> Rc<Self>`、`widget() -> &gtk::Widget`（现在是 `PreferencesGroup`）保持；`updating()` 保持；`FwHint` / `silent_check` 若无其他调用者则删除（`grep -rn "silent_check\|FwHint" crates/immurok-gui/src`）。
- `DashboardPage::new(toasts, firmware: &Rc<FirmwarePage>)`——新增参数，Dashboard 把 `firmware.widget().downcast_ref::<adw::PreferencesGroup>()` `page.add()` 到 hosts 组之后（同 `HostsGroup` 的 downcast 写法）。

- [ ] **Step 1: `firmware.rs` 重排**

`new()`：

```rust
        let root = adw::PreferencesGroup::builder().title("Firmware").build();
        let check_btn = gtk::Button::builder().icon_name("view-refresh-symbolic").valign(gtk::Align::Center).tooltip_text("Check again").build();
        check_btn.add_css_class("flat");
        root.set_header_suffix(Some(&check_btn));

        let device_row = adw::ActionRow::builder().title("Version").subtitle("-").build();
        let update_btn = gtk::Button::builder().label("Update").valign(gtk::Align::Center).visible(false).build();
        update_btn.add_css_class("suggested-action");
        device_row.add_suffix(&update_btn);
        root.add(&device_row);

        let status_row = adw::ActionRow::builder().title("Checking for updates…").build();
        root.add(&status_row);

        let progress = gtk::ProgressBar::builder().visible(false).show_text(true).margin_top(6).build();
        let warning = gtk::Label::builder().label("Do not power off the device.").xalign(0.0).visible(false).build();
        warning.add_css_class("error");
        root.add(&progress);
        root.add(&warning);
```

结构体：`root: adw::PreferencesGroup`，删 `latest_row`、`status: gtk::Label`、`notes: gtk::Label`，加 `status_row: adw::ActionRow`。`widget()` 返回 `self.root.upcast_ref()`。

`render()`：

```rust
        let (title, subtitle, can_update, can_check, updating) = match &*st {
            FwState::Idle | FwState::Checking => ("Checking for updates…".to_string(), None, false, false, false),
            FwState::UpToDate => ("Up to date".to_string(), None, false, true, false),
            FwState::Ready(p) => {
                let bridge = p.hops.first().zip(p.hops.last()).map(|(a, b)| (a.version.as_str(), b.version.as_str()));
                (plan_label(p.hops.len(), p.resumed, bridge), p.notes.clone(), true, true, false)
            }
            FwState::Updating => ("Updating…".to_string(), None, false, false, true),
            FwState::Success(v) => (format!("Update complete — device is now on {v}."), None, false, true, false),
            FwState::Failed(e) => (format!("Could not check: {e}"), None, false, true, false),
        };
        self.status_row.set_title(&glib::markup_escape_text(&title));
        self.status_row.set_subtitle(&notes_or_empty);   // subtitle = release notes（markup-escape），无则 ""
        self.progress.set_visible(updating);
        self.warning.set_visible(updating);
        self.update_btn.set_visible(can_update);
        self.check_btn.set_sensitive(can_check);
        self.check_btn.set_tooltip_text(Some(if matches!(&*st, FwState::Failed(_)) { "Retry check" } else { "Check again" }));
```

`plan_label` 返回的文案保持（含版本号）。原来更新进度写 `self.status.set_text` 的地方改写 `status_row.set_title`（同样 escape）；设备版本仍写 `device_row.set_subtitle`。`connect_map` 挂在 `root` 上不变。

- [ ] **Step 2: Dashboard 嵌入 + 删 hint**

`DashboardPage::new(toasts: &adw::ToastOverlay, firmware: &Rc<FirmwarePage>)`；在 hosts 组 `page.add` 之后：

```rust
        page.add(firmware.widget().downcast_ref::<adw::PreferencesGroup>().expect("FirmwarePage::widget is a PreferencesGroup"));
```

删除 `fw_hint_group` / `fw_hint_row` / `fw_update_btn` 字段与构建、`set_fw_hint`、`connect_update_clicked`、`use super::firmware::FwHint`。

- [ ] **Step 3: `main_window.rs`**

`firmware` 在 `dashboard` **之前**创建（Dashboard 要用它）；删除 `stack.add_titled(firmware.widget(), Some("firmware"), "Firmware")…`；删除 `dashboard.connect_update_clicked(...)` 块和 `silent_check` 的 `spawn_future_local` 块；`set_data("firmware-page", firmware)` 保留（保活）。关窗守卫 `pages::firmware::updating()` 不变。

`firmware.rs` 里 `FwHint` / `silent_check` 若已无引用则删除（保留其依赖的 `check_plan` 等实际用到的函数）。

- [ ] **Step 4: 编译 + 目视 + 提交**

Build 无 warning（特别注意 dead_code）；测试全绿。Broadway：侧边栏 6 项无 Firmware；Device 页 = Two Hosts + Firmware 组（Version 行 + 状态行 + header 刷新图标）；`Update` 仅在有更新时出现（当前设备最新，应不可见）。

```bash
git add crates/immurok-gui/src/pages/firmware.rs crates/immurok-gui/src/pages/dashboard.rs crates/immurok-gui/src/main_window.rs
git commit -m "gui: Firmware 并入 Device 页，Update 仅在有更新时出现，导航减到 6 项"
```

---

### Task 3: 空状态与主操作位置（Keys / Fingerprints / PAM）

**Files:**
- Modify: `crates/immurok-gui/src/pages/keys.rs`
- Modify: `crates/immurok-gui/src/pages/fingerprints.rs`
- Modify: `crates/immurok-gui/src/pages/pam.rs`

- [ ] **Step 1: Keys 空态行**

`new()` 的分组去掉 `.description(desc)`；把 `desc` 文案保留在一个 `fn empty_hint(cat: KeyCategory) -> (&'static str, &'static str)` 里：

| cat | title | subtitle |
|-----|-------|----------|
| Otp | "Add your first OTP entry" | "Generates a 6-digit code after you touch the device" |
| Api | "Add your first API key" | "Shows the stored value after you touch the device" |
| Ssh | "Add your first SSH key" | "Public key can be copied; signing goes through the SSH agent" |

`populate()` 的 `n == 0` 分支改为：

```rust
                let (t, s) = empty_hint(*cat);
                let row = adw::ActionRow::builder().title(t).subtitle(s).activatable(true).build();
                row.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
                let weak = Rc::downgrade(self);
                let cat = *cat;
                row.connect_activated(move |_| {
                    if let Some(this) = weak.upgrade() {
                        this.open_add(cat);
                    }
                });
                group.add(&row);
                self.rows.borrow_mut().push((group.clone(), row));
```

（去掉 `dim-label`；行本身用默认样式，前缀 `+` 图标表达可添加。设备未连接时 `open_add` 会打开对话框但写入必然失败——在 `open_add` 开头加 `if !self.connected.get() { self.toast("Device not connected", 3); return; }`。）

- [ ] **Step 2: Fingerprints 操作进 header**

`new()`：删除 `actions` 分组与其 `buttons` Box；改为

```rust
        let header_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let add_switch = gtk::Button::builder().label("Add switch fingerprint").valign(gtk::Align::Center).visible(false).build();
        let test_button = gtk::Button::builder().label("Test").valign(gtk::Align::Center).tooltip_text("Verify with an enrolled finger").build();
        let refresh_button = gtk::Button::builder().icon_name("view-refresh-symbolic").valign(gtk::Align::Center).tooltip_text("Refresh").build();
        refresh_button.add_css_class("flat");
        header_actions.append(&add_switch);
        header_actions.append(&test_button);
        header_actions.append(&refresh_button);
        group.set_header_suffix(Some(&header_actions));
```

分组 description 改为 `"Touch the sensor to unlock and authorize sudo."`。其余（`update_buttons` 对三个按钮的 sensitive 控制）不变。

- [ ] **Step 3: PAM 操作进 header**

删除 `actions` 分组、`bar`、`repair_note` 字段与所有对它的 `set_text`；改为

```rust
        let header_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let repair = gtk::Button::builder().label("Repair").valign(gtk::Align::Center).visible(false).build();
        repair.add_css_class("suggested-action");
        let refresh = gtk::Button::builder().icon_name("view-refresh-symbolic").valign(gtk::Align::Center).tooltip_text("Refresh").build();
        refresh.add_css_class("flat");
        header_actions.append(&repair);
        header_actions.append(&refresh);
        group.set_header_suffix(Some(&header_actions));   // group = "PAM services"
```

原来 `self.repair.set_sensitive(!busy && !to_repair.is_empty())` 处改为 `self.repair.set_visible(!to_repair.is_empty()); self.repair.set_sensitive(!busy);`；原来给 `repair_note` 写 "Nothing to repair" / "N services need repair" 的地方：前者删掉，后者改为 `repair.set_tooltip_text(Some(&format!(…)))`。

- [ ] **Step 4: 编译 + 目视 + 提交**

Build 无 warning；测试全绿。Broadway 截 Keys（三组空态行带 `+` 前缀，可点）、Fingerprints（header 右侧 Test + 刷新图标，页底无按钮、无裁切）、PAM（header 右侧刷新图标，Repair 不可见因无需修复，页底无按钮）。

```bash
git add crates/immurok-gui/src/pages/keys.rs crates/immurok-gui/src/pages/fingerprints.rs crates/immurok-gui/src/pages/pam.rs
git commit -m "gui: Keys 空态改为可点行，Fingerprints/PAM 操作按钮进分组 header"
```

---

### Task 4: 收尾

- [ ] **Step 1:** `cargo build --workspace` 无 warning；`cargo test --workspace` 全绿；`cargo clippy -p immurok-gui` 不新增 warning。
- [ ] **Step 2:** Broadway 六页截图存 `/tmp/claude-1000/polish-*.png`，逐页核对 spec「验收」清单。
- [ ] **Step 3:** `CHANGELOG.md` Unreleased 段加一行：`- gui: sidebar device card, consistent icons, Firmware folded into Device, actionable empty states, header-placed actions`；提交 `changelog: gui 视觉整形`。
