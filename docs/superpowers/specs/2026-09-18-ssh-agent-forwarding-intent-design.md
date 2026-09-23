# SSH agent 转发：远端签名请求的来源识别与用途声明

日期：2026-09-18 初稿
状态：**设计稿，未实现，未评审**
范围：协议部分（§4）平台无关，macOS / Linux 共用；实现部分（§5-§7）先做 Linux（`app-linux-rs`），macOS 在 §9 列出对应落点，另开计划跟进。
前置讨论：本稿来自「远程 agent 能否用本地 imk 里的私钥做 ssh / git」的讨论结论 —— 密码学层面 ssh-agent 转发已经解决，问题出在**本机那次触摸不知道自己在批准什么**。
姊妹稿：`2026-09-18-imk-remote-setup-design.md`（`imk remote setup ssh|sudo X`，把远端搭起来的那条命令）。两稿互不依赖。

## 1. 问题

imk 的 SSH 私钥在 CH592F 里，daemon 的 `agent.sock` 只是把摘要转成 BLE `KEY_SIGN`。
因此 `ssh -A host` 之后，远端主机上的 `git push` / `ssh` 都能经转发拿到签名，私钥不离开设备。
这是 ssh-agent 协议的本意，不需要新机制。

但当前 daemon 对转发来的请求有三处处理不当：

1. **无来源识别。** `ssh_agent.rs` 只用 `SO_PEERCRED` 校验 uid 是否有活动会话，然后拿 peer pid 找终端画 spinner。
   转发请求的 peer 是本机的 `ssh` 客户端进程，daemon 看不出「这是远端主机发起的」，spinner 还会画到用户正在看远端 shell 的那个终端上。
2. **触摸是盲的。** 请求体只有「用 key N 签这 N 字节」。本地 `imk run --agent` 有 `AGENT_APPROVE` 把命令文本送到对话框；远端没有任何等价通道。
   CLAUDE.md 的安全模型是「真正的边界只有设备上的那次触摸」，不知情的触摸等于没有边界。
3. **cooldown 给远端开了窗。** 固件 `FP_GATE_COOLDOWN_MS` 是 10 s rolling，AUTH cooldown 还会非对称放行 `KEY_SIGN`（`hidkbd.c:6383`）。
   用户为合法的 push 摸一下，被攻破的远端在 10 s 内可以再要任意多次签名，每次都续期。

## 2. 目标与非目标

目标：
- G1 daemon 能把签名请求分成四类（§5.1），每一类有明确的触摸策略和展示文案。
- G2 远端能通过转发的 agent 通道**声明用途**（命令、目录、主机），本机对话框展示后再触摸。
- G3 来自远端的签名每次都要新触摸，不吃 cooldown；声明过用途的命令可以在**有上限的预算**内多签几次。
- G4 不改固件、不改 ssh 客户端、不改远端 sshd；远端只需要放一个 `imk` 二进制。
- G5 触摸仍是唯一放行依据。声明文本只是给人看的，和现有 `classify_agent_claim` 一样属于「claimed」而非「verified」。

非目标：
- 远端 sudo 的**搭建**。远端装了 `pam_ssh_agent_auth` 之后，sudo 的 PAM 挑战就是一次普通的转发签名请求，本稿的四类分类和触摸策略自动覆盖它，`imk run --agent -- sudo …` 也能贴标签；怎么把它装好由姊妹稿负责。没装的远端，sudo 走密码，imk 不介入。
- 拓扑 C（Anthropic 云沙箱）需要的 relay。另议。
- OpenSSH 8.9+ 的 destination-constrained keys（`ssh-add -h`）。imk 不做 `ssh-add`，也没有 sshd 可查询。
- Windows 原生实现。WSL 与 Windows 服务侧为兼容本稿要满足的要求已写在主仓库 `docs/superpowers/specs/2026-09-06-wsl2-support-design.md` §12，实现顺序也在那里。
- 把「触摸」换成手机上的批准。人不在设备旁边这条路不成立，稿子不试图解决。

## 3. 拓扑与信任边界

```
本机（设备、daemon、用户在场）                       远端 dev box
┌──────────────────────────────┐                  ┌──────────────────────┐
│ CH592F ◄─BLE─► daemon        │   ssh -A / -R    │ sshd ─ $SSH_AUTH_SOCK │
│              agent.sock ◄────┼── ssh 客户端 ◄───┼── imk run --agent ─┐ │
│              session-agent   │   (peer pid P)   │       └─ git push ──┘ │
│              (对话框)         │                  │            └─ ssh ────┘ │
└──────────────────────────────┘                  └──────────────────────┘
```

- 每个远端进程连一次 `$SSH_AUTH_SOCK`，本机这边就是 `ssh` 客户端进程 P 新开一条到 `agent.sock` 的连接。**同一个 P 上的所有连接来自同一台远端主机**（ControlMaster 复用时也是同一主机）。这是来源识别的锚点。
- 攻击者模型：远端主机被完全攻破（root）。它能发任意签名请求、任意声明文本、任意次数。
  它做不到的：绕过触摸；在预算之外复用一次触摸；让本机相信它是本地进程（peer pid 是内核给的）。
- 本机同用户进程的攻击者不在本稿范围，沿用 `2026-09-03-pam-channel-hardening-design.md` 的结论。

## 4. 协议：`intent@immurok.com` 扩展（平台无关）

用 ssh-agent 协议的标准扩展消息（draft-miller-ssh-agent §4.7），OpenSSH 的 `ssh` / `sshd` 对 agent 通道是字节透传，扩展消息原样到达 daemon。

### 4.1 注册用途

```
byte    SSH_AGENTC_EXTENSION (27)
string  "intent@immurok.com"
string  payload
```

payload 内部仍用 SSH wire 编码：

| 字段 | 类型 | 说明 |
|---|---|---|
| version | uint32 | 固定 1 |
| command | string | 被包装的命令原文（argv 用空格拼接，UTF-8） |
| cwd | string | 远端工作目录 |
| host | string | 远端 `hostname`（自报） |
| user | string | 远端 `$USER`（自报） |
| max_signs | uint32 | 本次命令预计需要的签名次数，默认 1 |
| ttl_seconds | uint32 | 声明有效期，默认 60 |

响应：
- `SSH_AGENT_SUCCESS (6)`：已登记。
- `SSH_AGENT_EXTENSION_FAILURE (28)`：daemon 认识这个扩展但拒绝（字段非法、预算超上限、设备未连接）。
- `SSH_AGENT_FAILURE (5)`：对端不是 imk（普通 ssh-agent 对未知扩展的标准回应）。shim 以此判断「转发的不是 imk」。

daemon 侧上限：`max_signs ≤ 5`，`ttl_seconds ≤ 120`，`command` ≤ 4 KiB；超出即 28。

### 4.2 生命周期绑定

**声明的生命周期 = 发起它的那条连接的生命周期，再受 ttl 封顶。** shim 登记后不关闭这条连接，直到被包装的命令退出。daemon 在连接 EOF 时立刻撤销声明。这样：
- 命令结束，声明随之失效，不需要远端「记得」注销。
- ttl 只是兜底，防止 shim 被 `kill -9` 后连接仍被远端 sshd 挂着。
- 攻击者想复用声明，得先让 shim 活着，且预算仍然有限。

### 4.3 `query` 扩展

同时实现 OpenSSH 约定的 `query` 扩展，返回 `["intent@immurok.com"]`。不是必需，但让 `ssh-add` 一类工具能看到支持列表，排障时方便。

### 4.4 明确不做的

- 声明里不带 key 指纹、不带签名数据的预哈希。绑定到 key 意义不大（远端要签什么 key 由 ssh 客户端决定，都是本设备的 key），YAGNI。
- 不做「声明时触摸」。触摸放在签名时（§5.3），一次触摸批准一次具体签名，远端没签就不浪费触摸。
- 不做 `intent-wait@immurok.com`（shim 阻塞等待「已触摸 / 已拒绝」并在远端终端回显）。v1 远端只在失败时看到 ssh 自己的报错，见 §11 第 4 条。

## 5. daemon 行为（Linux 实现）

### 5.1 签名请求分类

`handle_sign_request` 入口按 peer pid P 依次判断，命中即停：

| 序 | 类别 | 判定 | 触摸策略 | 展示 |
|---|---|---|---|---|
| 1 | **declared**（远端已声明） | P 上有未过期、预算未耗尽的 intent | 首签：强制新触摸；同一 intent 后续签名在预算内吃 cooldown | 对话框：声明文本 + 可验证信息 |
| 2 | **forwarded-undeclared**（转发但没声明） | `/proc/P/comm == "ssh"` 且（P 启动 > 20 s 或 P 此前已有过签名） | **每次**强制新触摸 | 对话框：「undeclared forwarded request via ssh <host>」+ 可验证信息 |
| 3 | **local-agent**（本地 imk run） | `classify_agent_claim(P)` 命中（沿父链找 `AGENT_APPROVE`） | 现状不变 | 现状不变（终端 spinner） |
| 4 | **manual**（用户自己敲的） | 其余 | 现状不变 | 现状不变 |

顺序有讲究：**转发判定必须排在本地 claim 之前**。用户本地 `imk run --agent -- ssh host` 之后，远端所有签名请求的 peer 都是那个被 claim 覆盖的 `ssh` 进程，若先查 claim 就会把远端 push 当成本地 agent 放行、吃 cooldown（§7 第二行）。

第 2 类的启发式说明：`ssh` 客户端自己登录用的签名发生在进程启动后几秒内，之后再来的请求只可能是转发。误判方向是安全的：用户网络慢、20 s 后才走到公钥认证，会被当成转发，代价是多一次触摸和一个多余的对话框，不会漏放。`ssh <host>` 里的 host 从 `/proc/P/cmdline` 取最后一个非选项参数，取不到就显示 pid。

P 的启动时间从 `/proc/P/stat` 第 22 字段换算；「此前已有过签名」用 coordinator 内一张 `pid → 签名次数` 表，pid 消失即清。

### 5.2 强制新触摸的实现

不改固件。利用固件两条既有规则：
- `CMD_AUTH_REQUEST` 无条件进指纹门（`hidkbd.c` AUTH 路径，`socket.rs:1292` 注释）。
- AUTH cooldown 非对称放行 `KEY_SIGN`（`hidkbd.c:6383`）。

所以「强制新触摸的签名」= 先 `ble_auth_request()`，成功后立刻 `ble_send_fp_gated(CMD_KEY_SIGN)`，后者吃刚设下的 AUTH cooldown，用户只摸一次。
第二个远端请求在 10 s 内到来时同样先走 `AUTH_REQUEST`，于是又要摸一次 —— rolling cooldown 对远端失效，这正是要的效果。

代价：多一次 BLE 往返（约 200-400 ms）。对 push/pull 无感。

指纹门同一时刻只能有一个（`try_set_pending_pam`）；第二个远端签名在前一个等待触摸期间到达，直接回 `SSH_AGENT_FAILURE`，不排队。远端 ssh 会报 agent 签名失败并回落到下一种认证方式，符合预期。

后续可选：固件在 `KEY_SIGN` 上加「不吃 cooldown」标志位，并入 CLAUDE.md 已记的待办「主机在 AUTH_REQUEST 时声明 ttl / 次数预算」。到那时 §5.2 退化为一条命令，行为不变。

### 5.3 declared 路径的对话框

复用 session-agent 的对话框通道，新增一种类型：

```
UI:DIALOG:REMOTE:30:<json>
```

json 字段（全部由 daemon 填，session-agent 只渲染）：

| 字段 | 来源 | 可信度 |
|---|---|---|
| `claimed.command` / `cwd` / `host` / `user` | intent | **claimed**，远端自报 |
| `verified.kind` | daemon 解析 `sign_data` | verified |
| `verified.detail` | 同上 | verified |
| `via` | `/proc/P/cmdline` 的 ssh 目标 | 本机观察 |
| `budget` | 「第 k / max_signs 次」 | 本机计数 |

`verified.kind` 的解析规则（`sign_data` 是 ssh 客户端构造的，远端改不了含义）：
- 以 `string session_id, byte 50, string user, string "ssh-connection", string "publickey" …` 开头 → `userauth`，detail = 认证用户名（如 `git`）。
- 以 `"SSHSIG"` 魔数开头 → `sshsig`，detail = namespace（git 提交签名为 `git`）。
- 以 `pam_ssh_agent_auth` 的挑战格式开头（模块拼的 buffer：随机 cookie + string user + string hostname + …，具体字段顺序实现时按上游源码核对）→ `pam-sudo`，detail = 远端用户名与主机名。这是远端 sudo / polkit 的签名。
- 其他 → `unknown`，detail 为长度。

UI 文案（Linux 端只用英文，见 memory `feedback_linux_english_only`）分两块并排，标题分别为 "Remote claims" 与 "Verified"。
声明文本与验证结果矛盾时（声明 `git push`，验证是认证用户 `root`）不做自动判定，只是并排摆出来让人看。

forwarded-undeclared 路径用同一对话框，`claimed` 为空，标题改 "Undeclared forwarded request"。

两种远端路径都**不**画终端 spinner。peer pid 的 tty 是用户看远端 shell 的那个终端，往里写会搅乱远端输出。

### 5.4 预算与撤销

- intent 登记在 coordinator：`RwLock<HashMap<pid, Intent { fields, remaining, expires_at, conn_token }>>`。
- 每次 declared 签名成功后 `remaining -= 1`；到 0 后同一 P 的下一次请求落到第 2 类。
- 同一 P 再来一条新的 intent（远端连续跑两个命令）：**替换**旧的，不叠加预算。
- 连接 EOF、ttl 到期、设备断开（固件此时也清 cooldown，`hidkbd.c:5185`）三者任一发生即删除。
- 对话框取消 / 30 s 超时：回 `SSH_AGENT_FAILURE`，intent 不扣预算但也不撤销（用户可能只是没来得及摸）。

### 5.5 日志

沿用现有措辞：declared 记 `claimed remote intent: <host> "<command>"`；forwarded-undeclared 记 `forwarded sign request without intent via ssh <host>`。命令文本只进 info 日志不进对话框以外的地方；不记远端用户名以外的身份信息。

## 6. 远端 shim：`imk run --agent` 的远端模式

不新增二进制。`imk run --agent` 启动时按顺序决定模式：

1. 本机 daemon socket 可连 → 现有本地流程（`AGENT_APPROVE`，触摸后运行）。
2. 连不上 daemon 且 `$SSH_AUTH_SOCK` 已设 → **远端模式**：
   1. 连 `$SSH_AUTH_SOCK`，发 `intent@immurok.com`（command / cwd / hostname / $USER / `--signs N` / `--ttl S`）。
   2. 收到 6：保持这条连接，spawn 子进程（**不**覆盖 `SSH_AUTH_SOCK`，和本地模式相反），等退出，关连接，透传退出码。
   3. 收到 5：打印 `imk: no local daemon and $SSH_AUTH_SOCK is not an imk agent`，退出码沿用现有 `EXIT_GENERIC`，**不**运行命令。agent 看到这条会知道转发没接上，而不是静默跑出一个「密码认证失败」。
   4. 收到 28：打印 daemon 给的原因，退出，不运行命令。
3. 两者都没有 → 现有报错。

远端模式下 `--agent` 之外的子命令（`imk get` 等）保持现状：报「无 daemon」。远端读秘密不在本稿范围。

分发：`imk` 已是静态链接的 Rust 二进制，release 附 `x86_64` / `aarch64` musl 构建，远端 `install -m755 imk /usr/local/bin/`。
`imk-skill` 的 SKILL.md 加一段「远端主机也装 imk，包装方式不变」，agent 侧的规则不用改。

## 7. 错误处理与边界情况

| 情况 | 行为 |
|---|---|
| 远端没装 imk，直接 `git push` | 第 2 类：每次触摸 + "Undeclared" 对话框。可用，但吵；对话框本身就是提示「远端装 imk」 |
| 本地 `imk run --agent -- ssh host` 然后在远端跑 `git push` | 本地 claim 只覆盖本地 `ssh` 进程自己的登录签名（父链命中）。远端 push 的签名同样从 P 来，`classify_agent_claim(P)` 也会命中，若先查 claim 就落入第 3 类（local-agent）、吃 cooldown。**这正是 §5.1 把转发判定排在 claim 之前的原因**：P 的启动时间 / 签名计数先命中第 2 类，强制新触摸 |
| ControlMaster：多个远端 shell 共用一个 P | 全部归同一主机，intent 按 P 存，后来者替换先来者。可接受，多主机复用同一 master 不成立 |
| `ssh -R /tmp/imk.sock:/run/immurok/agent.sock` 而非 `-A` | peer 仍是本机 ssh 客户端，分类逻辑相同，无需特判 |
| 远端 `git pull`（fetch 一次连接） | 1 次签名，默认预算够 |
| 远端 `imk run --agent -- sudo …`（已装 `pam_ssh_agent_auth`） | PAM 挑战签名命中 intent → 第 1 类，对话框显示命令 + `pam-sudo` 验证信息。sudo 自己的时间戳缓存命中时不会有签名请求，也就没有触摸 |
| 远端 `git push` 到两个 remote / `rsync` 多次连接 | agent 自己传 `--signs 2`；超预算落第 2 类，多摸一次，不失败 |
| 对话框未开（session-agent 没跑） | 和 `AGENT_APPROVE` 一样：没有窗口，触摸仍是门；日志记 claimed 文本 |
| 设备未连接 | intent 回 28；签名沿用现有 5 s 等待重连逻辑 |
| 本机 macOS、远端 Linux | 协议一致；macOS daemon 未实现前，intent 回 5，shim 拒跑（§6 步骤 2.3）。这是过渡期的已知限制 |

## 8. 测试

单元（`immurok-daemon`）：
- 扩展消息编解码：合法 payload、字段超限、version ≠ 1、截断。
- `sign_data` 解析：构造 userauth / sshsig / 随机字节三种输入。
- 分类函数：注入伪造的 `/proc` 读取（comm、starttime、cmdline）与 claim / intent 表，覆盖 §5.1 四类及其判定顺序。
- 预算与撤销：递减、替换、EOF 撤销、ttl 到期。

集成（真机 + 一台 VM，手工清单写进实施计划）：
1. `ssh -A vm`，远端 `imk run --agent -- git push` → 对话框显示声明 + `userauth git`，摸一次，push 成功。
2. 同上但远端直接 `git push`（不包装）→ "Undeclared" 对话框，摸一次成功。
3. 摸完立刻再 push 一次 → 再要触摸（cooldown 对远端失效）。
4. `--signs 2` 跑一条需要两次连接的命令 → 一次触摸。
5. 远端 `kill -9` shim 后立刻 push → 落第 2 类。
6. 本地 `imk run --agent -- ssh vm` 进入远端 shell 后 push → 仍要新触摸（§5.1 判定顺序生效）。
7. 远端是普通 `ssh-agent`（对照）→ shim 报「不是 imk agent」且不运行命令。
8. 对话框点 Cancel → 远端 ssh 收到 agent 失败，回落密码提示。

## 9. macOS 对应落点

只列文件，不展开：
- `SSHAgentServer.swift`：扩展消息解析、分类、`AUTH_REQUEST` 前置。peer pid 用 `LOCAL_PEERPID`，进程启动时间与 cmdline 用 `proc_pidinfo` / `sysctl KERN_PROCARGS2`。
- `AuthCallerClassifier.swift`：已有父链遍历，加启动时间判据。
- `AgentGateOverlaySession.swift` / `AuthRequestOverlay.swift`：新增 REMOTE 对话框，双栏 claimed / verified。
- `imk` CLI 的远端模式与 Linux 共用同一 crate，不需要 macOS 特有改动。

## 10. 实施切分

建议两个计划，可独立合入：

1. **方向 1：来源识别 + 强制新触摸**（§5.1 第 2 类、§5.2、§5.3 的 undeclared 变体）。不依赖远端任何改动，合入即对现有用户生效。
2. **方向 2：intent 扩展 + shim 远端模式**（§4、§5.1 第 1 类、§5.4、§6）。依赖 1 的对话框和分类骨架。

## 11. 待确认

1. 远端 shim 用同一个 `imk` 二进制（本稿方案）还是独立更小的程序？本稿倾向前者，分发和 skill 文档都不用分叉。
2. forwarded-undeclared 是「每次触摸放行」（本稿）还是提供配置项直接拒绝？拒绝更严，但远端没装 imk 时用户会完全用不了。
3. 「20 s」启发式阈值是否可接受，还是希望做成配置项？
4. 是否要做 §4.4 否掉的 `intent-wait` 扩展让远端终端也能看到「waiting for fingerprint / verified」？本稿不做，远端只在失败时看到 ssh 的报错。
5. macOS 跟进是紧接着做还是等 Linux 验证过再说？§7 最后一行的过渡期限制取决于这个。

## 12. 被否掉的方案

- **daemon 跑 `ssh -G <args>` 解析 `ForwardAgent`**：能准确判断转发是否开启，但 daemon 以 `immurok` 系统用户跑，读不到用户的 `~/.ssh/config`，结果不可信。
- **声明时触摸、签名时吃 cooldown**（照抄本地 `AGENT_APPROVE`）：本地那样做是因为 sudo 的 `AUTH_REQUEST` 没有任何展示通道，只能预先批准。远端只有签名，签名本身就是可展示的时刻；预先批准反而让「声明了但没签」白摸一次，且预算窗口从签名前就开始跑。
- **在 intent 里绑定 key 或数据哈希**：远端要签什么 key 由 ssh 客户端按 `IdentityFile` 决定，shim 拿不到；哈希更是签名时才有。绑定不了，去掉。
- **改固件加 `KEY_SIGN` 标志位作为首选**：可行且更干净，但 `AUTH_REQUEST` 前置已经能达到同样效果且不需要设备升级；留作后续与 ttl/预算待办一起做。
