# Linux 图形配置 App + 热键 Quick-fill 设计稿

日期：2026-09-14
状态：设计定稿，待实施
对应计划：
- `docs/superpowers/plans/2026-09-14-gui-quickfill-core.md`（阶段一：共享客户端 crate、GUI 外壳、Dashboard / Keys 页、Quick-fill 面板 + 剪贴板输出）
- `docs/superpowers/plans/2026-09-14-gui-quickfill-input.md`（阶段二：全局热键、键盘注入后端、设置页）
- 阶段三（TUI 功能对齐：登记流程、PAM 页、Firmware 页、Logs 页、双机管理）在阶段一落地后另写计划

---

## 1. 目标

给 Linux 端做一个图形界面的配置 App，功能对齐 `immurok-cli tui`，并提供和 macOS `QuickFillPanel` 同款的体验：全局热键呼出面板 → 选一条 OTP → 触摸设备 → 6 位验证码自动填进当前焦点的输入框。

不做的事：
- 不把 BLE、密钥、配对逻辑搬进 GUI。GUI 是 daemon 的另一个壳，和 TUI 平级。
- 不做 uinput 虚拟键盘。portal / XTEST / wlroots 协议 / 剪贴板四层已覆盖所有桌面，uinput 需要 udev 规则且按 keycode 打字在非 QWERTY 布局下会打错，收益不值成本。
- 不做托盘常驻图标的强依赖。GNOME 默认没有托盘。托盘是可选项，阶段三再评估。

## 2. 进程与信任边界

```
┌──────────────────────────────┐   /run/immurok/pam.sock   ┌─────────────────────────┐
│ immurok-daemon (User=immurok)│◄──────────────────────────►│ immurok-gui (用户会话)   │
│  BLE / 密钥 / 指纹门 / 缓存    │   一连接一请求，文本行协议     │  Adw.Application         │
└──────────────┬───────────────┘                            │  com.immurok.Settings    │
               │ SUBSCRIBE:SESSION                          │  主窗口 + Quick-fill 面板 │
┌──────────────▼───────────────┐                            │  portal / xdotool / wtype│
│ immurok-session-agent (用户)  │                            │  剪贴板                  │
│  授权弹窗 / 通知 / ~/.ssh/config│                            └─────────────────────────┘
└──────────────────────────────┘
```

- GUI 与 daemon 的关系和 TUI 完全一样：同 uid 进程，走 `SO_PEERCRED` 校验，命令分级由 daemon 决定。GUI 不持久化任何秘密；TOTP 码只在内存里停留到注入完成。
- 指纹门在设备上。`GET:otp:<name>` 由 daemon 走 `ble_send_fp_gated`，GUI 只是等 `OK:<6位>` 或 `ERROR:*`。
- session-agent 不动。它管的是 daemon 主动发起的会话侧动作；Quick-fill 是用户主动发起的，归 GUI。

## 3. 代码结构

### 3.1 新增 crate `immurok-client`

把 `crates/immurok-cli/src/socket_client.rs` 整体抽出来，加一层类型化 API，TUI 和 GUI 共用：

| 模块 | 内容 |
|---|---|
| `daemon.rs` | 原 `socket_client.rs`：`DaemonClient::connect / send / send_with_timeout`、`fetch_key_cache`、`open_log_stream`、`probe_isolation` |
| `status.rs` | `DeviceStatus { connected, name, battery, fw_version }`，`parse_status_line`，`query_status()` |
| `keys.rs` | `KeyCategory { Ssh, Otp, Api }`、`KeyEntry { index, category, name, service }`、`parse_key_cache(ssh_json, names_json)`、`list_keys()`、`get_otp(name)`、`get_api(name)`、`cancel_gate()` |

约束：
- 保持同步阻塞（std），不引入 tokio。TUI 在 `thread::spawn` 里用，GUI 在 `gio::spawn_blocking` 里用。
- daemon 一个连接只处理一个请求，每个 API 调用内部自己 `connect()`。
- `get_otp` 读超时 40 s（指纹门 30 s + BLE 往返余量），其余沿用默认 60 s。
- immurok-cli 里 `mod socket_client;` 改为 `use immurok_client as socket_client;`，其余引用不动。

### 3.2 新增 crate `immurok-gui`

| 文件 | 职责 |
|---|---|
| `main.rs` | `adw::Application`，id `com.immurok.Settings`，`HANDLES_COMMAND_LINE`；解析 `--quick-fill` / `--gapplication-service`；注册 `show` / `quick-fill` 两个 action |
| `cli.rs` | 纯函数 `parse_launch(args) -> Launch`，可单测 |
| `main_window.rs` | `Adw.ApplicationWindow` + `Adw.ViewStack`，挂各页 |
| `pages/dashboard.rs` | 设备状态、配对/解除、功能开关 |
| `pages/keys.rs` | SSH / OTP / API 三列表，取码、看公钥、删除 |
| `pages/settings.rs` | （阶段二）输出方式、热键状态、测试按钮 |
| `quickfill.rs` | Quick-fill 面板窗口和状态机 |
| `output.rs` | `enum Output { Clipboard, Portal, Xdotool, Wtype }` 与 `inject()`；阶段一只有 Clipboard |
| `poll.rs` | 2 s 一次的状态轮询，窗口不可见时暂停 |
| `session.rs` | （阶段二）X11 / Wayland 判定 |
| `hotkey.rs` | （阶段二）GlobalShortcuts portal + X11 XGrabKey |
| `settings_store.rs` | （阶段二）`~/.config/immurok/gui.json` |

GTK 线程模型：所有 socket 调用放 `gio::spawn_blocking`，结果用 `glib::spawn_future_local` 回到主线程更新控件。禁止在主线程直接调 `DaemonClient`，一次 BLE 往返能到秒级，会卡死界面。

### 3.3 版本下限

README 承诺 Debian 12+ / Ubuntu 22.04+。它们分别带 GTK 4.8 / libadwaita 1.2 和 GTK 4.6 / libadwaita 1.1。因此：
- `gtk4 = "0.9"`、`libadwaita = "0.7"`，都不开 `v4_*` / `v1_*` feature。
- 只用 libadwaita 1.0 的控件：`ApplicationWindow`、`HeaderBar`、`ViewStack`、`ViewSwitcherTitle`、`PreferencesPage/Group`、`ActionRow`、`Toast`、`ToastOverlay`、`StatusPage`。
- 不用 `Adw.MessageDialog`（1.2）、`Adw.ToolbarView` / `NavigationSplitView`（1.4）、`Adw.Dialog`（1.5）、`Gtk.AlertDialog`（4.10）。确认框用 `gtk::MessageDialog`。

## 4. 启动与单实例

- `.desktop`：`/usr/local/share/applications/com.immurok.Settings.desktop`，`DBusActivatable=true`。
- D-Bus 服务文件：`/usr/local/share/dbus-1/services/com.immurok.Settings.service`，`Exec=/usr/local/bin/immurok-gui --gapplication-service`。
- 自启动：`~/.config/autostart/com.immurok.Settings.desktop`，`Exec=immurok-gui --gapplication-service`，由 `make install` 复制。进程以 service 模式起来时调 `hold()` 常驻，不开窗口。
- `immurok-gui --quick-fill`：GApplication 检测到已有主实例，把命令行转发过去，主实例触发 `quick-fill` action。没有主实例时自己成为主实例并直接开面板。
- 因此三种呼出路径收敛到同一个 action：portal 快捷键信号、X11 抓键、DE 里绑定的命令。

## 5. Quick-fill 面板

状态机：

```
Idle ──(呼出)──► Listing ──(Enter 选中 OTP/API)──► Waiting(30s 倒计时) ──OK──► Delivering ──► Closed
                    │                                   │
                    │(Enter 选中 SSH)──► 直接投递公钥      │ERROR / 超时 / Esc(发 GATE:CANCEL)
                    │                                   ▼
                    └──(Esc / 失焦)──► Closed         红色提示 1.5s ──► Listing
```

- 数据来自 `list_keys()`（daemon 本地缓存，不触摸设备）。搜索框做子串过滤，大小写不敏感，匹配 name 和 service。
- 面板是独立 `gtk::Window`，`set_decorated(false)`，关闭即销毁，不复用。这是为了让合成器在面板消失后把焦点还给原窗口；Wayland 下 GUI 无法主动还焦点。
- `notify::is-active` 变 false 时自动关闭（点到别处即消失）。等待指纹期间不关闭：等待时用户可能去看设备。
- 投递流程：`Output::inject(code)`。剪贴板后端要在面板关闭前写（GNOME 只允许有焦点的窗口写剪贴板）；打字类后端要在面板关闭后等 150 ms 再注入。
- 面板关闭时若仍在 Waiting，发 `GATE:CANCEL`。
- 打字后端失败时自动降级到剪贴板并通知。

## 6. 输出后端（阶段二）

选择顺序由 `select_output(settings, session, probes)` 决定，用户可在设置页强制指定：

| 优先级 | 后端 | 条件 | 说明 |
|---|---|---|---|
| 1 | Portal | Wayland 且 `org.freedesktop.portal.RemoteDesktop` 存在 | `NotifyKeyboardKeysym` 逐字符，keysym 与布局无关；`persist_mode=ExplicitlyRevoked`，restore token 存 gui.json |
| 2 | Xdotool | X11 会话且 `xdotool` 在 PATH | `xdotool type --clearmodifiers --delay 12 --file -`，文本走 stdin |
| 3 | Wtype | Wayland 且 `wtype` 在 PATH | `wtype -d 12 -`，文本走 stdin；wlroots 的 `virtual-keyboard-unstable-v1` |
| 4 | Clipboard | 永远可用 | 写剪贴板，30 s 后若仍是我们写的内容就清空，发桌面通知 |

keysym 映射：`0x20..=0x7e` 和 `0xa0..=0xff` 直接等于码点，其余 `0x0100_0000 | 码点`。TOTP 只有数字，API key 是可打印 ASCII，这条规则够用。

## 7. 全局热键（阶段二）

| 层 | 条件 | 行为 |
|---|---|---|
| GlobalShortcuts portal | Wayland 且接口存在（GNOME 48+、Plasma 5.27+、Hyprland） | 绑定 id `quick-fill`，建议触发 `CTRL+backslash`；用户在 DE 设置里改 |
| X11 XGrabKey | X11 会话 | 默认 `ctrl+backslash`，gui.json 可改 |
| DE 绑定命令 | 任何桌面 | 设置页展示一行可复制的命令 `immurok-gui --quick-fill`，教用户在 DE 快捷键里绑 |

三条都通过 `quick-fill` action 收口。设置页只显示当前生效的是哪一层，不做录键控件。

## 8. 桌面覆盖（2026-09-14 核对）

| 桌面 | 热键 | 输出 |
|---|---|---|
| GNOME Wayland 48+ | portal | Portal |
| GNOME Wayland 43-47 | DE 绑命令 | Portal |
| KDE Plasma 6 Wayland | portal | Portal |
| Hyprland（新版 xdph） | portal | Portal |
| Sway / river / wlroots | DE 绑命令 | Wtype |
| COSMIC | DE 绑命令 | Portal |
| 任何 X11 会话 | XGrabKey | Xdotool |
| 以上皆不满足 | DE 绑命令 | Clipboard |

## 9. 安装与依赖

- 构建：`libgtk-4-dev` + `libadwaita-1-dev`（Debian）/ `gtk4-devel` + `libadwaita-devel`（Fedora）/ `gtk4` + `libadwaita`（Arch）。Makefile 用 `pkg-config --exists gtk4 libadwaita-1` 探测，缺失时 `--exclude immurok-gui` 继续构建其他 crate，check-deps 只 warn。
- 运行：GTK4 / libadwaita 已在 README 依赖列表里。`xdotool` / `wtype` 是可选运行依赖，check-deps warn。
- 二进制进 `/usr/local/bin`（root，和 daemon 同规则）。`.desktop` 与 D-Bus 服务文件进 `/usr/local/share/`，自启动条目进用户 `~/.config/autostart/`。
- 卸载时删除以上文件。

## 10. 安全注意

- TOTP 码不写日志、不进事件流。TUI 用 `SecretMessage` 的原因同样适用。
- Quick-fill 面板等待期间显示的是 name / service，不显示码。
- 剪贴板路径必须清空。30 s 后读回比对，仍是我们写的才清，避免误删用户后来复制的内容。
- portal restore token 不是秘密，但泄露后同 uid 进程可跳过授权弹窗注入按键。同 uid 本来就能做到，不算新增攻击面。
- daemon 侧已有触摸后 1.6 s 的 0x23 长按锁屏抑制（`coordinator.rs` `handle_lock_request`），Quick-fill 走的是同一条 fp-gated 路径，无需额外处理。阶段一验收要实测确认：取码后屏幕不锁。

## 11. 验收（阶段一）

在一台 Linux 开发机（GNOME 或 KDE Wayland）：
1. `make` 产出 `target/release/immurok-gui`；`cargo test --workspace` 全绿。
2. `immurok-cli tui` 行为无变化（Keys 页取码、Dashboard 开关）。
3. `immurok-gui` 打开主窗口，Dashboard 显示与 `immurok-cli status` 一致的状态；开关切换后 `immurok-cli settings` 反映变化。
4. Keys 页取 OTP：弹出触摸提示，触摸后显示 6 位码，30 s 后从界面消失。
5. 第二个终端跑 `immurok-gui --quick-fill`：主实例弹出面板；搜索、上下键、Enter 选 OTP、触摸、剪贴板里是 6 位码、收到通知、30 s 后剪贴板被清。
6. 取码后 5 s 内屏幕没有被锁。
7. 面板打开时点别处：面板消失。等待指纹时 Esc：daemon 日志里出现 GATE:CANCEL。
