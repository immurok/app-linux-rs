# Linux 密码管理器解锁（AT-SPI 定向注入）设计稿

日期：2026-09-21
状态：**搁置（2026-09-21）**。探针（§9）做完当晚发现 1Password 8 / Bitwarden / KeePassXC ≥2.8 在 Linux 上都内置「系统认证解锁」，走 polkit → `/etc/pam.d/polkit-1`，而 Linux 端 `pam_immurok` 本来就装在 polkit-1——Bitwarden Flatpak 实测触摸即解锁（只需把 `com.bitwarden.desktop.policy` 装到 `/usr/share/polkit-1/actions/`）。本稿的 AT-SPI 注入方案因此对 v1 三个内置目标没有必要，仅作「不支持 polkit 系统认证的应用」的备选保留；§9 的探针结论（Chromium 无 EditableText、`GetAttributes` 踢门、flatpak 身份 = 代理进程 cgroup、Wayland 下 XTEST 无效、portal 可行）独立有效，将来做通用注入时直接复用。
对应 macOS 实现：`app-macos/Sources/AuthContextDetector.swift` / `AuthInjector.swift` / `InjectionWhitelist.swift`

---

## 1. 目标

把 macOS 端「触摸指纹 → 自动解锁 1Password / Bitwarden」搬到 Linux：用户锁定的密码管理器窗口出现在桌面上时，触摸设备一次，专用存储的主密码被**写进那个特定的密码框**并提交。

关键约束：
- **定向注入，不是键盘模拟**。密码只能进白名单进程里那一个密码框，不能「打到当前焦点」。Linux 上做到这一点的唯一通用手段是 AT-SPI2（辅助功能总线），X11 / Wayland 通吃，且绕过输入法和 Caps Lock。
- 主密码存主机（daemon 侧），触摸驱动。与「存设备、对话框驱动」方案比较后由用户拍板（2026-09-21）：更接近 macOS 手感，密码不占设备 keystore。
- v1 覆盖：1Password 8 桌面、Bitwarden 桌面、KeePassXC 三条内置，外加用户自定义条目。**浏览器扩展不做**（Chromium / Firefox 网页树两套、体量大，v1 稳定后再评估）。

不做的事：
- 不新增常驻进程。`immurok-session-agent` 已是用户会话里的无界面常驻服务（`systemctl --user`），本功能只是给它加一组 `PM:*` 消息分支。不加托盘图标。
- 不降级到键盘模拟打密码。`SetTextContents` 失败即失败（§5.3）。
- 不支持 AppImage 目标（挂载点用户可写，无法校验身份）。
- 不改固件。

## 2. 进程分工与信任边界

```
┌──────────────────────────────┐                    ┌──────────────────────────────────┐
│ immurok-daemon (User=immurok)│                    │ immurok-session-agent (用户会话)  │
│  pm_store.rs  密码存储(0600)  │  SUBSCRIBE:SESSION  │  pm/detector.rs  AT-SPI 找解锁框   │
│  裸触摸 → PM:MATCH 推送       │───────────────────►│  pm/identity.rs  校验目标进程       │
│  PM:REQUEST 校验 + 放出密码   │◄───────────────────│  pm/injector.rs  写值 + 提交       │
│  PM:SET/CLEAR/LIST 命令       │                    │  a11y_switch.rs  IsEnabled 开关    │
└──────────────────────────────┘                    └──────────────────────────────────┘
         ▲ 一连接一请求
┌────────┴─────────────┐
│ immurok-gui / -cli    │  设置密码、开关条目、测试检测
└──────────────────────┘
```

- daemon 是唯一持有密码的进程。密码文件在 daemon 的 state dir，0600、owner `immurok`，用户 uid 读不到——这是「存主机」相对「存设备」方案的全部安全增益。
- agent 拿到密码只在内存里停留到注入结束；不落盘、不进日志。
- GUI / CLI 只写不读（`PM:LIST` 不返回密码）。
- daemon 跑在 `ProtectHome=yes`、无 session bus 的系统用户下，够不着 a11y 总线（per-session D-Bus，地址从用户 session bus 的 `org.a11y.Bus.GetAddress` 拿），因此检测和注入必须在 agent 里做。
- **威胁模型明说**：同 uid 进程冒充 agent，可以在用户下一次裸触摸后从 daemon 骗到密码。这与 macOS「同 uid 可以 ptrace 1Password 本身」是同一层级，本方案不试图防御，只用两条限制收窄窗口（§6.4：3 s 窗口 + 单次消费）。真正的边界仍是设备上那次触摸。

## 3. 主流程（触摸驱动）

```
设备 0x21 裸匹配
  → daemon on_fp_match 第 3 步：pm 总开关开 && 存在活的条目 && 屏幕未锁 && agent 在线
      → push_ui("PM:MATCH:<items json>")，记 pm_match_at = now
  → agent 收到 PM:MATCH → detector.detect(items)（预算 8000 节点 / 单进程 1.5 s / 总 5 s）
      → 命中 (item, field, window, pid) → identity.verify(pid, item) 通过
      → 回 "PM:REQUEST:<item-id>"
  → daemon 校验：now - pm_match_at ≤ 3 s、条目活的、来自 SUBSCRIBE:SESSION 连接本身
      → push_ui("PM:SECRET:<item-id>:<base64>")；pm_match_at 清零
      → 校验失败 push_ui("PM:DENY:<reason>")，不断连
  → agent injector.inject(field, secret) → submit(window, strategy)
      → 无论成败回 "PM:RESULT:<item-id>:OK|FAIL:<reason>"
  → daemon：OK 写 last_auth_flow（抑制 0x23 长按锁屏，同 PAM approve）；FAIL 视情况 NOTIFY（§8）
```

「活的条目」= 总开关 `settings.pm_unlock` 开 && 条目 `enabled` && 条目有密码，三者同时满足。

裸触摸的路由优先级：pending PAM > 锁屏解锁 > **PM 检测（新增）** > 忽略。不满足推送条件时走原有 `info!("no auth context, ignoring")` 分支，对没开这个功能的用户行为完全不变。

为什么 daemon 推「MATCH」让 agent 触摸时扫一次，而不是 agent 常驻监听焦点事件再上报「看到解锁框」：常驻监听要长期占一个 AT-SPI 事件订阅、每个焦点变化判一次；触摸驱动只在触摸那一刻扫一次，与 macOS 一致，daemon 也不用维护「最近上下文」状态。

## 4. daemon 侧

### 4.1 `pm_store.rs`

文件 `<state_dir>/pm.json`，0600，owner `immurok`。密码明文存：主机上没有比 daemon 用户 DAC 更强的信任根，`pairing.json` 里的 ECDH shared_key 更敏感也是同一保护级别，全盘加密是用户的事。写入用临时文件 + rename（同 `Settings::save`）。

```rust
struct PmItem {
    id: String,                  // 内置："1password" / "bitwarden" / "keepassxc"；自定义：uuid
    name: String,
    enabled: bool,
    builtin: bool,
    identities: Vec<Identity>,   // 内置项只读，加载时用 builtin_defaults() 刷新（同 macOS refreshingBuiltinIdentity）
    title_regex: Option<String>, // 仅自定义：对容器节点 Name 匹配，同进程多窗口时收窄
    submit: Submit,
    password: Option<String>,    // None = 未设置
}
```

内置默认（enabled 均 false）：

| id | submit | identities（实施时逐条核对，见 §9） |
|---|---|---|
| `1password` | `Enter` | `ExePath ["/opt/1Password/1password"]`、`Snap "1password"` |
| `bitwarden` | `DefaultButton` | `ExePath ["/opt/Bitwarden/bitwarden"]`、`Flatpak "com.bitwarden.desktop"`、`Snap "bitwarden"` |
| `keepassxc` | `DefaultButton` | `ExePath ["/usr/bin/keepassxc"]`、`Flatpak "org.keepassxc.KeePassXC"`、`Snap "keepassxc"` |

`Settings` 新增 `pm_unlock: bool`（默认 false），复用 `FEATURE:SET`。

### 4.2 socket 命令（一连接一请求，走现有 tier / authorize）

| 命令 | 门 | 说明 |
|---|---|---|
| `PM:LIST` | 无 | JSON 数组，不含密码，只含 `has_password` |
| `PM:SET:<id>:<base64>` | **有**：`ble_auth_request()`（AUTH_REQUEST，固件永远要新触摸） | 持久写入要主机侧门，同 Windows `PASS:SET`。base64 让 `:` / 换行安全通过行协议 |
| `PM:CLEAR:<id>` | 无 | 收缩攻击面的操作不设门 |
| `PM:ENABLE:<id>:0\|1` | 无 | 功能开关用户决定 |
| `PM:ADD:<json>` | 无 | 自定义条目 |
| `PM:REMOVE:<id>` | 无 | 内置项拒绝 `ERROR:BUILTIN` |
| `PM:TEST:<id>` | 无 | 让 agent 立刻跑一次 detect（不注入），回 `OK:found` / `ERROR:<reason>` |

tier：`PM:LIST` / `PM:TEST` 归查询级，其余与 `FEATURE:SET` 同级。日志不记 `PM:SET` 的参数。`PM:SET` 撞上设备门 BUSY 回 `ERROR:BUSY`，客户端套 gate dialog。

### 4.3 session 通道新增行

```
daemon → agent   PM:ENABLED:0|1               总开关（订阅时 + 变更时重发，同 SSH_TAKEOVER 的「订阅即对账」）
                 PM:MATCH:<items json>        裸触摸；items = 活的条目的 {id, identities, title_regex, submit}，不含密码
                 PM:SECRET:<id>:<base64>      放出密码
                 PM:DENY:<reason>             拒绝 REQUEST
                 PM:TEST:<items json>         设置页测试（单条目）
agent  → daemon  PM:REQUEST:<id>
                 PM:RESULT:<id>:OK|FAIL:<reason>
                 PM:TEST_RESULT:OK|FAIL:<reason>
```

`serve_session_agent` 目前只认 `UI:CANCEL`，加上述三个 agent → daemon 分支。`PM:TEST` 需要 daemon 把一次性连接的请求转给 agent 再等结果：用 oneshot 挂在 coordinator 上，超时 6 s 回 `ERROR:TIMEOUT`。

### 4.4 放行规则

`PM:REQUEST:<id>` 全部满足才放：
1. `pm_match_at` 存在且 `elapsed ≤ 3 s`——超过说明不是这次触摸引发的
2. `<id>` 是活的条目
3. 来自当前 `SUBSCRIBE:SESSION` 连接本身（普通一次性连接发 `PM:REQUEST` 一律拒绝）

放出后 `pm_match_at` 清零：**一次触摸只放一次密码**。

推 `PM:MATCH` 不写 `last_auth_flow`；`PM:RESULT:OK` 时写，保证注入成功后手仍在传感器上不会被 0x23 的 1.6 s 长按锁屏。

## 5. agent 侧

新增依赖 `atspi`（odilia 项目，基于 zbus，开 tokio feature；agent 已是 tokio current_thread）。

### 5.1 `pm/detector.rs` — 找解锁框

输入：`PM:MATCH` 带来的活条目列表。输出：`Option<Hit { item_id, field, window, pid, pidfd }>`。

对每条条目：
1. 从 Registry 的 desktop 节点列出所有 `Application` 子节点，`GetProcessID` 取 pid，按条目身份规则（§5.2）过滤候选进程。**先过滤再扫树**，不对非白名单进程发任何 AT-SPI 调用。
2. 对每个候选做有界 BFS（总预算 8000 节点、单进程 1.5 s）：
   - 记录路径上最近的 `Frame` / `Window` / `Dialog` 节点作为容器
   - 命中 `ROLE_PASSWORD_TEXT` 收进结果，不下钻
   - 结果 > 1 立即收手
3. **恰好一个密码框**才返回命中，容器是它最近的 window 节点。与 macOS `soleSecureFieldWithWindow` 相同的硬化判据：解锁框永远单密码框，「修改主密码」多框表单天然排除。
4. 密码框状态须含 `Visible` + `Showing` + `Sensitive`（Electron 会把已隐藏路由的 DOM 留在树里）。KeePassXC 解锁页旁的钥匙文件框是 `ROLE_TEXT`，不影响。
5. 自定义条目有 `title_regex` 时，容器 `Name` 须匹配。

多条目都命中取第一个，日志 warn。

### 5.2 `pm/identity.rs` — 校验目标进程

Linux 没有代码签名，身份规则是三选一枚举：

```rust
enum Identity {
    /// /proc/<pid>/exe 的 canonical path 必须等于其一，且文件 owner 为 root、
    /// 所在目录自 / 起每一级都不可被当前用户写（防 /opt/foo 被用户拥有）
    ExePath(Vec<PathBuf>),
    /// /proc/<pid>/root/.flatpak-info 的 [Application] name 必须等于
    Flatpak(String),
    /// /proc/<pid>/cgroup 含 "snap.<name>."
    Snap(String),
}
```

- 条目 `identities` 任一满足即通过。
- AppImage 一律不认。
- pid 复用竞争：拿到 pid 先 `pidfd_open`，校验完再扫树，注入前 `pidfd_send_signal(0)` 确认仍是同一进程。
- 校验失败记 agent stderr + 回 `FAIL:identity`；daemon 只记 warn，不发通知（安全事件不给冒充者反馈）。

### 5.3 `pm/injector.rs` — 写值与提交

写值：`EditableText.SetTextContents(secret)`。不经过输入法、Caps Lock、焦点。失败（接口不存在 / 返回 false）直接 `FAIL:set_text`，**不降级到键盘模拟**——打到未知焦点比失败更糟。

提交策略（对齐 macOS `SubmitStrategy`，v1 两种）：

```rust
enum Submit {
    /// 容器子树里找 ROLE_PUSH_BUTTON，按名字优先级（"Unlock" / "Sign in" / "OK"，
    /// 不分大小写）挑一个，Action.DoAction(0)；找不到退到 Enter
    DefaultButton,
    /// 密码框 GrabFocus 后合成 Return：1Password 8 的提交箭头是无名 button，
    /// Electron 又不一定暴露 DoAction
    Enter,
}
```

`Enter` 的合成：先 `Accessible.GrabFocus()` 钉焦点到密码框（保证定向），再用 GUI 阶段二的键盘后端（portal / xdotool / wtype）发单个 Return keysym。**回车是唯一走键盘模拟的按键**，不含秘密。键盘后端代码从 `immurok-gui/src/output.rs` 抽到 `immurok-common`（或新 crate `immurok-input`），agent 与 GUI 共用；GUI 未编译（缺 GTK）时 agent 仍要能用，所以不能依赖 gtk4。quickfill 阶段二的键盘后端尚未落地（`output.rs` 目前只有剪贴板）；若本功能先做，只实现「发一个 Return keysym」的最小版本（portal `NotifyKeyboardKeysym` / `xdotool key Return` / `wtype -k Return`），不做通用打字。

注入后 300 ms 读 `Text.GetCharacterCount`：仍等于密码长度说明提交没生效，回 `FAIL:not_submitted`，不重试（重试等于要第二次触摸）。

### 5.4 `a11y_switch.rs` — IsEnabled 开关

Electron / Qt 默认不构建 a11y 树，只在 a11y 总线 `org.a11y.Status.IsEnabled` 为 true 时才暴露。**不碰 `ScreenReaderEnabled`**——GNOME 上置 true 会拉起 Orca。

- 收到 `PM:ENABLED:1`：读 `IsEnabled` 原值存 `~/.local/state/immurok/a11y_prev`（仅文件不存在时写，避免多次开关冲掉原值），置 true。
- 收到 `PM:ENABLED:0`：按 `a11y_prev` 还原，删文件。
- 检测时若候选进程的 Application 节点无子节点，回 `FAIL:no_tree`；daemon 转通知「请重启 <app>」——Chromium 只在启动时读这个开关。

副作用（设置页明写）：桌面上所有 Electron / Qt 应用开始暴露控件树，轻微内存开销；同 uid 进程可读它们的非密码文本——同 uid 本来就能 ptrace，不算新增边界。

## 6. 客户端

### 6.1 `immurok-client/src/pm.rs`

`PmItemView { id, name, enabled, builtin, has_password, submit, identities_summary }`、`list()`、`set_password(id, pw)`（读超时 40 s，同 `get_otp`）、`clear_password(id)`、`enable(id, bool)`、`add(custom)`、`remove(id)`、`test(id)`。同步阻塞，GUI 在 `spawn_blocking` 里用。

### 6.2 GUI：`pages/password_managers.rs`

libadwaita 1.0 控件内完成（沿用 quickfill 稿 §3.3 版本下限），文案全英文：
- 顶部 `SwitchRow` 总开关 `Password manager unlock` + 说明：开启会把桌面辅助功能开关置 true，目标应用需重启一次；AppImage 不支持。
- `PreferencesGroup` 列条目：`ActionRow` 标题 + 副标题（`Password set` / `No password`），右侧 enabled `Switch`，末尾菜单：Set password / Clear password / Test detection / Remove（自定义才有）。
- Set password：`gtk::MessageDialog` 带 `PasswordEntry`，确认后套现有 `gate_dialog`（30 s 倒计时 + 取消 → `GATE:CANCEL`）。
- Add custom：名称、身份（下拉 Executable path / Flatpak app id + 文本框）、窗口标题正则（可选）、提交方式下拉。
- Test detection：Toast 显示结果；`FAIL:no_tree` 文案直接说 restart the app。

### 6.3 CLI：`immurok-cli pm`

`pm list` / `pm set <id>`（stdin 或交互提示读密码，不接受命令行参数）/ `pm clear <id>` / `pm enable|disable <id>` / `pm test <id>`。不进 TUI。

## 7. 错误处理与用户反馈

agent 每次 `PM:MATCH` 的终态都回 daemon；daemon 只在失败时 `NOTIFY:`（成功用户已看到 app 解锁）：

| agent 回报 | 通知 | 备注 |
|---|---|---|
| `FAIL:no_tree` | `<app>: accessibility tree is empty — restart the app` | Chromium 启动时读开关 |
| `FAIL:set_text` | `<app>: field does not accept injection` | 见 §9 探针 1 |
| `FAIL:not_submitted` | `<app>: password filled but not submitted` | 用户可手动回车 |
| `FAIL:timeout` | `<app>: detection timed out` | 整个 detect + inject > 5 s |
| `FAIL:identity` | 不通知，daemon warn | 安全事件 |
| 无命中 | 不通知，daemon debug | 裸触摸无上下文是常态 |

## 8. 测试

- **纯逻辑单测**（`cargo test --workspace`）：
  - `identity.rs`：tempdir 造 `/proc` 样板（exe 符号链接、`.flatpak-info`、cgroup 文本），覆盖 root-owned 通过 / 用户可写目录拒绝 / AppImage 拒绝 / flatpak id 匹配。
  - `detector.rs`：AT-SPI 访问抽成 trait `A11yTree`，内存树 mock 覆盖「恰好一个」「两个拒绝」「不可见排除」「预算耗尽」「title_regex」。
  - `pm_store.rs`：序列化、刷新内置项保留 enabled / password、内置 remove 拒绝、文件权限 0600。
  - daemon 放行规则：3 s 窗口、单次消费、非 session 连接拒绝——照 `coordinator.rs` 现有 `try_begin_pairing_*` 测试风格。
- **集成**：不自动化，靠 §10 验收。

## 9. 探针结果（2026-09-21 回填，GNOME 49 Wayland / at-spi2-core 2.60 / Flatpak 1.18）

探针代码：`atspi` 0.30 写的 ~250 行二进制 + RemoteDesktop portal 的 Python 脚本（scratchpad，不入库）。被测对象：Bitwarden flatpak（Chrome 150）、Antigravity /opt（Chrome 146）、Motrix /opt（Chrome 108）、自写 Qt 6.11 小程序（密码 QLineEdit + 默认按钮）。KeePassXC / 1Password 真机未装（sudo 门等设备），Qt 结论以小程序代表，KeePassXC 布局待验收时核对。

### 9.1 `EditableText.SetTextContents`（问题 1）

| 工具包 | 密码框接口集 | 结论 |
|---|---|---|
| Qt 6 | `Accessible | Action | Collection | Component | EditableText | Text` | **可用**：`SetTextContents("abc")` → `Ok(true)`，`CharacterCount`=3，`DoAction(Press)` 默认按钮后应用收到 "abc" |
| Chromium / Electron（146、150） | `Accessible | Action | Collection | Component | Document | Hyperlink | Text` | **没有 `EditableText`**，普通 `Entry` 也没有——Chromium 整体未实现 AtkEditableText。密码框 Action 只有 `activate` / `showContextMenu` |

→ 1Password 8 / Bitwarden 走不了纯 AT-SPI 写值，**只能键盘路径**（§9.4）。

### 9.2 让 Electron 暴露树（问题 2）——两道门，不是一道

1. **挂到 a11y 总线**：Chromium `AtkUtilAuraLinux::ShouldEnableAccessibility()` 依次看环境变量 `ACCESSIBILITY_ENABLED` / `GNOME_ACCESSIBILITY` / `QT_ACCESSIBILITY`（=1 开 =0 关）→ session bus 上 `org.a11y.Bus` 的 `org.a11y.Status.IsEnabled` → GSettings `toolkit-accessibility`。**只在启动时判一次。**
   - 非沙箱 Electron（Antigravity）：`IsEnabled=true` 即可，实测通过 → §5.4 的 IsEnabled 开关对它有效。
   - **Flatpak 里的 Electron（Bitwarden）：`IsEnabled=true` 无效**（沙箱的 session-bus 代理拦掉了这条 Properties.Get，gsettings 又是沙箱内 keyfile），必须 `ACCESSIBILITY_ENABLED=1` 进沙箱环境，即 `flatpak override --user --env=ACCESSIBILITY_ENABLED=1 <app-id>`，且需重启一次。
   - `ScreenReaderEnabled` 不在 Chromium 的判断里，维持不碰。
2. **开网页树**：挂上总线后只有 `Application → Frame`（Frame 0 子节点），这是 Chromium 的渐进式 AXMode。Chromium 源码注释明说：AT 调 `AtkGetAttributes` / `AtkRefRelationSet` 是「Orca 在用」的信号，触发 `kAXModeBasic | kExtendedProperties`（含 `kWebContents`）。**实测：对 Frame 调一次 `Accessible.GetAttributes`，约 1–2 s 后整棵网页树出现**，Chrome 146 / 150、沙箱内外都成立，无需 `--force-renderer-accessibility`、无需重启。
   - Chrome 108（Motrix）没有这套逻辑，不管；Electron 那么老的密码管理器不存在。
   - 另一条路是让它扫 `/proc` 发现名为 `orca` 的进程（`DiscoverOrca()`），但 flatpak 有独立 pid namespace 看不到，且哨兵进程会让所有 Chromium 应用进 screen-reader 模式，弃。

→ §5.4 的「应用需重启」只剩第 1 道门；detector 在 BFS 之前对候选进程的顶层节点调一次 `GetAttributes` 做「踢门」，然后轮询 `ChildCount` 最多 2 s。

### 9.3 身份（问题 3）

- **Flatpak 的 a11y 总线名对应的 pid 是 `xdg-dbus-proxy`，不是应用**（flatpak 给沙箱代理 a11y 总线）。`/proc/<pid>/root/.flatpak-info` 因此不可用。可用的是代理进程的 cgroup：`/proc/<pid>/cgroup` = `.../app-flatpak-<app-id>-<instance>.scope`，再与 `$XDG_RUNTIME_DIR/.flatpak/<instance>/info` 的 `[Application] name=` 互相印证。同 uid 能伪造 systemd scope 名，与 §2 的威胁模型一致。
- 同一 pid 可能出现**多个 Application 节点**（Qt 应用带 gtk3 platformtheme 时多出一个 toolkit=gtk、0 子节点的壳）。按 pid 合并，取有子节点的那个。
- pid 从 `org.freedesktop.DBus.GetConnectionUnixProcessID(<bus name>)` 取（libatspi 同款），atspi crate 没封装，直接用 zbus fdo。
- 内置条目核对：
  - 1Password：AUR/.deb/.rpm 都装到 `/opt/1Password/1password` ✓；**Flathub 有 `com.onepassword.OnePassword`（8.12.x）**，补进 identities；snap `1password` 未核。
  - Bitwarden：`.deb` → `/opt/Bitwarden/bitwarden` ✓；Flathub `com.bitwarden.desktop` ✓（实测）；**Arch `extra/bitwarden` 用系统 `electron` 跑 `/usr/lib/bitwarden/app.asar`，`/proc/pid/exe` 是共享的 electron 二进制，ExePath 身份对它无意义**——不进内置项，用户可自定义（并接受其弱身份）。
  - KeePassXC：`/usr/bin/keepassxc`（发行版包）✓；Flathub `org.keepassxc.KeePassXC` ✓。
- Qt 裸 `QWidget` 顶层的 role 是 `Filler`（`QMainWindow` 才是 `Frame`）。容器规则改为「Application 的直接子节点」而非按 role 挑，Chromium 和 Qt 都成立，`Active` 状态也都在这一层。

### 9.4 提交与键盘（问题 4 及 Electron 替代路径）

- **`DoAction` 在 Electron 上有效**：Bitwarden 的 Unlock 按钮 `Action[0]=press`，`DoAction(0)` 后弹出 "Invalid master password"。Qt 是 `Action[0]=Press`。**两个工具包都给默认按钮打 `IsDefault` 状态**，`DefaultButton` 策略应先按 `IsDefault` 挑，名字匹配只做退路。
- **XTEST 在 GNOME Wayland 下到不了 XWayland 客户端**：`xdotool type`、AT-SPI `DeviceEventController.GenerateKeyboardEvent`（内部也是 XTEST）对 Bitwarden 均无效（连 Tab 都不动焦点）。quickfill 稿把 xdotool 列为「X11 会话」后端是对的，Wayland 下不能指望它。
- **RemoteDesktop portal 可用**：`CreateSession → SelectDevices(KEYBOARD, persist_mode=2) → Start`（首次弹系统授权框，之后凭 `restore_token` 静默）→ `NotifyKeyboardKeysym` 逐字符。实测打进 Bitwarden 密码框，`Text.CharacterCount` 随之变 3，`GetText` 对普通 Entry 能读回内容。`Component.GrabFocus` 在 Chromium 上返回 true 且字段获得 `Focused`，但**不会把窗口拉到前台**——窗口不是 `Active` 时打字会进别的窗口。

### 9.5 对 §5 的修订（待拍板）

1. 注入分两种写入策略，按字段接口集自动选：`EditableText` 有 → `SetTextContents`；没有（Chromium）→ portal 键盘。键盘路径的护栏：GrabFocus 后**立刻**重读字段 `Focused` 与容器 `Active`，任一不满足 → `FAIL:not_active`，不打字（用户点一下窗口再触摸）；打完 300 ms 内读 `CharacterCount`，≠ 密码长度 → 不提交、`FAIL:count_mismatch` 通知；portal 不可用（无 `org.freedesktop.portal.RemoteDesktop`）→ X11 会话退 xdotool，否则 `FAIL:no_keyboard`。
2. 「不降级到键盘模拟」改为「不降级到**无护栏的**键盘模拟」：Electron 目标在 v1 就是键盘路径，不是降级。
3. `PM:ENABLE` 一个 Flatpak 身份的条目时，agent 执行 `flatpak override --user --env=ACCESSIBILITY_ENABLED=1 <app-id>`（禁用时若是我们加的则 `--unset-env`），GUI 文案写明并提示重启该应用一次。
4. portal 授权：GUI 开总开关时即走一次 `Start`（用户当场点允许），`restore_token` 存 `~/.local/state/immurok/pm_portal_token`，agent 每次注入时凭 token 静默建会话。token 失效（portal 回非 0）→ `FAIL:portal_denied` 通知去设置页重新授权。
5. atspi crate 关掉 `p2p` feature（flatpak 内的 p2p socket 路径在宿主不可达，只产生噪音）。

## 10. 验收（GNOME Wayland + KDE Plasma 各一遍）

1. `cargo test --workspace` 全绿；`make` 产物含改动后的 agent；`systemctl --user restart immurok-session-agent` 后日志有 subscribed。
2. 设置页开总开关 → `busctl --user get-property org.a11y.Bus /org/a11y/bus org.a11y.Status IsEnabled` 为 true；关掉 → 还原为开启前的值。
3. 1Password 设密码时设备亮灯、触摸后 `pm list` 显示 `Password set`；`ls -l <state_dir>/pm.json` 是 `-rw------- immurok`；普通用户 `cat` 被拒。
4. 锁定 1Password，触摸 → 解锁；daemon 日志有 `PM:MATCH` → `PM:REQUEST` → `RESULT:OK`，无密码内容。Bitwarden、KeePassXC 各一次。
5. 硬化：打开「修改主密码」页触摸 → 不注入，日志 debug 无命中。把 1Password 二进制复制到 `~/fake/1password` 跑起来锁定后触摸 → `FAIL:identity` warn，不注入。
6. 关总开关后触摸 → daemon 日志回到 `no auth context, ignoring`。
7. 触摸后 5 s 内屏幕没被 0x23 锁。
8. 注入成功后 3 s 外再触摸一次 → 无事发生（单次消费 + 无上下文）。
9. `pm test 1password`：1Password 未启动回 `FAIL:not_found`，锁定时回 `OK:found`。
10. 目标 app 在开总开关之前已启动 → 触摸后收到「restart the app」通知；重启后正常。
