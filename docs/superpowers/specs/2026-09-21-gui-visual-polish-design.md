# immurok-gui 视觉整形（方案 A：HIG 内，不加自定义 CSS）

日期：2026-09-21
范围：`crates/immurok-gui`
前提：Ubuntu 22.04 下限 = GTK 4.6 / libadwaita 1.1，绑定 crate 只开 `v1_1`；不引入自定义 CSS provider（`enroll_dialog.rs` 已有的 shake CSS 除外）。

## 背景（设计评审结论）

功能齐全后整个 App 只有一种表达——灰色 `PreferencesGroup` + `ActionRow`。五个最伤的问题：没有视觉锚点（设备状态被拆成三行灰字）、侧边栏图标语义混乱（两个齿轮、一个扳手）、7 个导航项撑不起内容（Firmware 页两行）、空状态是一行灰字、每页主操作位置随意且 Fingerprints 底部按钮被裁切。

## A1. 侧边栏设备卡（视觉锚点）

新建 `sidebar.rs`：`SidebarStatus`，放在侧边栏 `ListBox` 上方（`main_window.rs` 的左列改成 `gtk::Box` 纵向：设备卡 → 分隔线 → `ListBox`）。

```
┌──────────────────┐
│  ⟡  immurok       │  ← bluetooth-active-symbolic 32px（已连接加 `accent` 类），设备名 title-4
│     Connected     │  ← 状态文字：Connected / Paired, not connected / Not paired / Daemon unavailable
│     ▮▮▮▮▮▯ 93%    │  ← gtk::LevelBar（0..1）+ 百分比 caption；未连接时隐藏
│     1.8.3.fc5a    │  ← 固件号 caption dim-label；未连接时 "-"
└──────────────────┘
```

接口：`SidebarStatus::new() -> Rc<Self>`、`widget()`、`apply(&DeviceStatus, paired: bool)`、`apply_daemon_down()`。数据来自 Dashboard 的 2 s 轮询（`DashboardPage::set_sidebar(&Rc<SidebarStatus>)`，与 `set_features` / `set_keys` 同模式）。

Device 页删掉 "Device" 分组（Status / Battery / Firmware 三行）——信息已在侧边栏常驻。

## A2. 图标语义统一

| 页 | 图标 |
|----|------|
| Device | `bluetooth-active-symbolic` |
| Features | `preferences-other-symbolic` |
| Keys | `dialog-password-symbolic`（不变） |
| Fingerprints | 自带 `immurok-fingerprint-symbolic`，**重绘**为 Adwaita 权重（1.9px 圆头描边转填充轮廓的指纹环，见 plan 里的 SVG） |
| PAM | `security-high-symbolic` |
| Logs | `utilities-terminal-symbolic`（不变） |

Firmware 不再是导航项（见 A3）。

## A3. Firmware 并入 Device 页

`FirmwarePage` 的根从 `PreferencesPage` 改为一个 `PreferencesGroup`（标题 "Firmware"），由 Dashboard `page.add()` 到 Two Hosts 之后。内容：

- header suffix：`Check again`（图标按钮 `view-refresh-symbolic`，flat；Failed 状态 tooltip "Retry check"）。
- 一行 `ActionRow`：title "Version"，subtitle = 设备版本；suffix 放 `Update` 按钮（`suggested-action`），**只在 `FwState::Ready` 时可见**。
- 状态行：`ActionRow` title 为状态文字（"Checking for updates…" / "Up to date" / "v1.9.0 available" / "Update complete — now on v…" / 错误文案），subtitle 放 release notes（有则显示）。
- 进度条 + "Do not power off" 警告：仅 Updating 时可见（保持现逻辑）。

删除：Dashboard 的 `fw_hint_group` / `set_fw_hint` / `connect_update_clicked`、`main_window.rs` 里的 `silent_check` 启动检查和 "firmware" 页跳转——`FirmwarePage` 自身 `connect_map` 进入 Device 页即检查。`firmware::updating()` 与关窗守卫不变。`latest_row` 删除（最新版本号并入状态行文案）。

## A4. 空状态与主操作位置

- **Keys**：空分组的占位行改为可激活的 `ActionRow`：prefix `list-add-symbolic` 图标，title "Add your first OTP entry"（按分类），subtitle 一句用途说明（复用分组 description 文案），`activatable=true`，激活 = `open_add(cat)`；分组 description 移除（说明进了占位行；有条目时分组不再重复说明）。header 的 `+` 保留。
- **Fingerprints**：`Add switch fingerprint` / `Test Fingerprint` / `Refresh` 从页底 `actions` 分组移到 "Fingerprints" 分组 header suffix 的横向 `Box`（Refresh 改图标按钮 `view-refresh-symbolic` flat，tooltip "Refresh"；其余保持文字按钮）。删掉 `actions` 分组。分组 description 缩短为 "Touch the sensor to unlock and authorize sudo."
- **PAM**：`Refresh` 改为 "PAM services" 分组 header suffix 图标按钮；`Repair` 按钮只在 `to_repair` 非空时可见（放在同一 header Box 里，`destructive-action` 去掉，用 `suggested-action`）；`repair_note`（"Nothing to repair"）删除；底部 `actions` 分组删除。
- **Device**：`Unpair` 留在 Host 卡内（按主机操作，位置合理）。

## A5. 尺寸

窗口默认 900×620；侧边栏宽 200（设备卡需要）。

## 验收

Broadway + CDP 截图六页（Device / Features / Keys / Fingerprints / PAM / Logs），核对：侧边栏设备卡随连接状态更新；无导航项 Firmware；Device 页 = Two Hosts + Firmware 组；Keys 空态可点；Fingerprints/PAM 无底部按钮且无裁切。真机：连接/断开设备看侧边栏卡切换。

## 不做

自定义主色 / 设备线稿 / 状态 chip（方案 B）；信息架构合并（方案 C）。
