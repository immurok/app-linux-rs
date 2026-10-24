# GUI 设置向导 — Linux 移植设计

2026-10-23 · 对应 immurok-gui 0.9.x（Unreleased）

上游设计：主仓库 `docs/setup-wizard-flow.md`（macOS 向导，v1.6.6）。本文只记录 Linux 端的落地方式和与 macOS 的差异；设计原则、示意图规范、文案规则以上游为准，改动时不要回退。

## 步骤

macOS 的 8 步在 Linux 上收成 6 步：屏幕解锁走 PAM（`gdm-password` / `sddm`），没有键盘模拟，所以「解锁密码」和「辅助功能」两步没有对应物。

| # | 步骤 | 完成条件 | 可跳过 | 备注 |
| --- | --- | --- | --- | --- |
| 1 | Welcome | — | — | 只在什么都没配过时从这页开始 |
| 2 | Install Check（按需） | daemon 可达，且 `services_to_repair` 为空 | 仅当 daemon 可达 | daemon 不可达时显示 `sudo systemctl start immurok-daemon`；缺 PAM 钩子时给「Install PAM Hooks」（pkexec） |
| 3 | Bluetooth | `STATUS` 报告 connected | 否 | 图示 + 「Open Bluetooth Settings」+ 等待转圈 |
| 4 | Link This Computer | `PAIR:STATUS` 为 PAIRED 且设备未报告 `device_unpaired` | 否 | 点击前不显示设备图 |
| 5 | Add Fingerprint | 认证槽至少 1 枚 | 是 | 复用 `enroll_dialog` |
| 6 | Done | — | — | 四行汇总：蓝牙 / 绑定 / 指纹 / PAM 钩子 |

轮询分两路：`STATUS` / `PAIR:STATUS` / `GET:SETTINGS` / pam.d 每秒一次；`FP:LIST` 单独跑，只在已连接且列表未知时发起，失败下个 tick 重试。原因：HID 刚连上时 daemon 的 `is_connected` 已为 true 而状态缓存尚未就绪，此时 `FP:LIST` 是一次真实 BLE 往返，可能阻塞很久；两者绑在一起会让蓝牙页卡在「等待连接」直到它返回。向导打开时的首次 `FP:LIST` 最多等 2 秒。

蓝牙页区分三种等待：daemon 不可达（红色错误）、`STATUS` 第 7 段 `link_up`=1 但 `connected`=0（「Bluetooth connected — the device is starting up…」，BlueZ 链路已连上、daemon 的 GATT 会话还在起）、都没有（「Waiting for the Bluetooth connection…」）。中间态过去能持续十几秒，原因和修法见 CHANGELOG（helper READY 顺序 / 就绪超时）。

与 macOS 相同的行为：侧栏打勾可回看；每秒轮询，当前步满足即自动前进；回看（Back / 点侧栏）暂停自动前进，直到再点 Next；启动跳到第一个未完成步，全部完成直接到 Done；「Later」写 `setup_completed` 并关窗。

与 macOS 不同：

- **弹出条件只看 `setup_completed`。** Linux 包刻意不动 `/etc/pam.d`，PAM 钩子缺失是用户的选择而不是损坏状态，所以不像 macOS 那样「PAM 缺失就再弹」。主窗口 header 菜单里的「Setup Wizard…」和 `immurok-gui --setup` 随时可以再打开。GUI 里点「Unlink」解绑本机会把 `setup_completed` 置回 false，下次打开主窗口向导自动回来；`immurok-cli unpair` 不碰 GUI 配置，不会触发。
- **「始终置顶」退化为「始终在主窗口之上」。** GTK 4 没有 keep-above，Wayland 上也没有全局置顶；向导窗口设为主窗口的 transient，合成器保证它叠在主窗口上方，点主窗口不会把它压下去。主窗口后于向导创建时（`--setup` 再开主窗口）由主窗口反向认领。系统蓝牙设置等其他应用的窗口仍可能盖住它。
- **安装检查把 daemon 掉线也算进去。** macOS 没有独立 daemon。
- **Install Check 是否插入在向导打开时决定一次**，之后步骤列表不再变动，避免侧栏在用户操作中途跳动。

踩过的坑：GUI 常驻实例（`--gapplication-service`）会接管之后所有启动，`make install` 换了二进制它还在跑旧代码——「Unlink 后向导没回来」的第一次报告就是这个（旧实例不重置 `setup_completed`）。现在 `make install` / 包 postinstall 用 `scripts/retire-gui.sh` 请它退出（新 `retire` action，固件升级中拒绝），`immurok-gui` 启动时发现主实例的 `/proc/<pid>/exe` 已是 `(deleted)` 也会先请它退出再接管。

## 术语切分

GUI 内不再出现 Pair / Unpair / Bind / Unbind：

| 层 | 用词 |
| --- | --- |
| 系统蓝牙（BlueZ，桌面蓝牙设置里点连接） | Bluetooth connection / Connect |
| App 层 ECDH 密钥交换 | Link / Link This Computer / Linked / Not linked / Unlink |
| 设备上的两个槽位 | Host 1 / Host 2 |

`pair` 仍是 daemon 的 wire 命令名（`PAIR:START` 等）和 CLI 子命令名（`immurok-cli pair`），不改。TUI 文案未动。

侧栏设备卡改成两行：`Bluetooth: connected / not connected` 和 `Link: linked / not linked / no longer linked / shown once connected`，tooltip 解释「蓝牙只是让电脑看见设备，还要绑定」。

## 绑定引导框 `link_guide.rs`

Device 页的「Link This Computer」和向导第 4 步打开的是同一个对话框。

- 打开前先 `SLOT:STATUS` 判断是否第二台主机（已有槽位被占且设备当前槽为空）；两步清单只在这种情况下画两行，首次绑定只画一行。判断必须在 `PAIR:START` 之前完成，否则 `WAIT_BUTTON` 可能先到、画错分支。
- `PAIR:START` 在 worker 线程阻塞，另开连接每 300 ms 轮询 `PAIR:PROGRESS`，`WAIT_FP` / `WAIT_BUTTON` / `ECDH` 分别点亮对应行并切换顶部示意图（顶面手指 / 底面按钮光环 / 定格）。
- daemon 没有 `PAIR:CANCEL`，设备侧状态机不受窗口影响，所以进行中 `close-request` 一律拦下，脚注写明「长按设备按钮，或等 30 秒自动取消」。
- 失败文案按分支区分（`failure_text`）：首配超时、第二主机超时（含二手设备恢复出厂方法）、NEEDS_RESET、连接参数、长按取消。失败后对话框留在屏幕上，带 Close 按钮；调用方只 toast 一句。

## 设备示意图 `device_illustration.rs`

cairo 在 `gtk::DrawingArea` 上绘制，画布 340×190，三个场景与 macOS 一一对应（蓝牙窗口简笔画 + 顶面蓝灯闪烁；底面按钮光环扩散；顶面传感器绿光 + 手指按压）。几何常量照抄 Swift 版（按钮偏移 0.25、拨键偏移 0.24、按钮直径 0.11，螺丝 0.36，圆盖 0.34），单元测试锁住「部件落在轮廓内」的不等式。

颜色取自 style context：前景 `color()`，背景 `window_bg_color`，强调色 `accent_color`（没有就退回 GNOME 蓝），深色模式自然跟随。文字不画进画布——依赖树里没有 pangocairo，而且真 Label 直接跟随主题字体——通过 `gtk::Fixed` 把一行动作指令和蓝牙窗口里的三个短标签叠在画布上。

动画用 33 ms 定时器重绘，只在 DrawingArea map 期间运行，unmap 自动停止；起动放到下一个 main loop 轮次，避免把对话框呈现时的位移一起循环（与 macOS 同样的处理）。

## 代码位置

| 文件 | 负责 |
| --- | --- |
| `src/setup_wizard.rs` | 向导窗口：`Snapshot`（纯数据，可测）、步骤表、完成判断、自动前进、六个页面、蓝牙设置启动器 |
| `src/link_guide.rs` | 绑定引导框、第二主机判断、失败文案 |
| `src/device_illustration.rs` | 三个场景的 cairo 示意图 |
| `src/sidebar.rs` | 设备卡两行状态 |
| `src/pages/hosts.rs` | Two Hosts 卡片改用 Link / Unlink，绑定走 `link_guide` |
| `src/settings_store.rs` | `setup_completed` |
| `src/main.rs` / `src/cli.rs` / `src/main_window.rs` | `app.setup` action、`--setup`、首启自动弹出、header 菜单 |

## 验证

- `cargo test -p immurok-gui`：步骤表、起始步、可跳过规则、第二主机判断、失败文案、示意图几何。
- 真机截图：用 `IMMUROK_SOCKET` 指向一个脚本化的假 daemon（回答 STATUS / PAIR:STATUS / FP:LIST / SLOT:STATUS / PAIR:PROGRESS / PAIR:START），在 Xwayland 下逐页截图，覆盖欢迎、蓝牙、绑定、引导框（首配 / 第二主机 / 超时）、指纹、完成、Device 页与深色模式。
