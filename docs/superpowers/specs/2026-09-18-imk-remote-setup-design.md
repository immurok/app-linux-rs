# `imk remote`：一条命令让远端主机用上本机 immurok 的认证

日期：2026-09-18 初稿
状态：**需求 + 设计稿，未实现，未评审**
姊妹稿：`2026-09-18-ssh-agent-forwarding-intent-design.md`（本机触摸的来源识别与用途声明）。两稿互不依赖：那份解决「A 上的触摸知不知道自己在批准什么」，本稿解决「用户能不能把 X 搭起来」。可以各自推进；都落地后体验才完整。
术语：**A** = 设备 BLE 连着、imk daemon 在跑、用户人在旁边的机器；**X** = 用户经 ssh 登录的远端主机。

## 1. 需求

用户在 A 上执行：

```bash
imk remote setup ssh  X     # 之后 X 上的 git / ssh / 提交签名用 A 的设备签名
imk remote setup sudo X     # 之后 X 上的 sudo / polkit 用 A 的指纹放行
imk remote status     X     # 看 X 上哪些已配好、公钥是否仍匹配
imk remote remove     X     # 撤掉 imk 在 A 和 X 上写的一切
```

之后用户在 A 上 `ssh X`，在 X 里：

| 操作 | 行为 |
|---|---|
| `git push` / `git pull` / `git clone`（SSH 远端）、`ssh` 跳第三台、`scp` / `rsync` | 签名请求回到 A，A 上摸一次 |
| `git commit -S`（SSH 签名） | 同上 |
| `sudo …`、图形提权（polkit） | 挑战回到 A，A 上摸一次；失败回落密码 |
| `imk run --agent -- …`（AI agent 用） | 姊妹稿方向 2：给这次签名贴上命令标签，A 的对话框能看到 X 上要跑什么 |
| `imk get imk://…`、OTP | **不支持**（§8） |

底层机制没有新东西：ssh agent 转发 + `pam_ssh_agent_auth`。本稿的价值全部在「一条命令、可验证、可回滚、可撤销」。

### 1.1 手工做要多少步（问题陈述）

| 场景 | A 上 | X 上 | 门槛 |
|---|---|---|---|
| git / ssh | 改 `~/.ssh/config`；确认 `SSH_AUTH_SOCK` | 无 | 低 |
| sudo | 导出公钥 | 装 PAM 模块（Arch 走 AUR）、放公钥、改 `/etc/pam.d/sudo`、改 sudoers | 高：动 PAM 和 sudoers，改错锁死 |
| 日常 | — | tmux 里 `SSH_AUTH_SOCK` 过期；提示出现在别的窗口 | 困惑 |

每台新 X 重复一遍。这是普通用户不会自己做的量。

### 1.2 成功标准

- 一台干净的 Ubuntu 24.04 / Fedora 44 / Arch 的 X，用户只输入上面两条命令和一次 X 的 sudo 密码，之后 §1 表格前四行全部成立。
- 任何一步失败，X 上不留半成品：sudo 仍按原样用密码工作。
- `remove` 之后 X 上 `sudo -k && sudo true` 走密码，`ssh-add -l` 报无 agent，`/etc/pam.d/sudo`、sudoers 和原来逐字节一致。
- 重复执行 `setup` 是幂等的，只报告「已配置」。

## 2. 非目标

- X 上读 imk 秘密（`imk get`、OTP）。它们走 daemon 的 PAM socket，不是 agent 协议，要另开通道，另议。
- 人不在 A 旁边的场景。
- 从第三台机器（手机、B 机器）登录 X 时的指纹。A 不在链路上，物理上不可能。
- X 是 macOS 或 Windows。X 只支持 Linux（systemd 发行版）。A 支持 Linux 和 macOS。
- 替代用户自己的 ssh 客户端配置（ProxyJump、多跳）。imk 只写 `ForwardAgent`，其余沿用用户的。多跳时中间跳板不需要也不应该开转发，只在最终 X 开，这点在 §4.1 处理。

## 3. 信任边界

- X 被视为**半信任**：它能在连接期间请求任意多次签名，但每次都要 A 上触摸。设备丢失、A 被攻破不在本稿范围。
- X 上公钥文件必须 root 所有、非 root 不可写。放用户家目录的话，X 上任何能写家目录的进程都能加自己的 key，用自己的 agent 过 sudo。
- `setup sudo` 以 root 身份改 X 的 PAM 与 sudoers。这条命令的每一处写入都必须「临时文件 → 校验 → 原地验证 → 原子替换」，且保留回滚副本。**这是发布前提，不是可选项。**
- setup 过程中经 ssh 传到 X 的只有公钥、二进制、配置文本。任何时候不传私钥、不传 `pairing.json`、不传 daemon 的任何状态。

## 4. `imk remote setup ssh X`

只在 A 上运行，不需要 X 的 root。

### 4.1 步骤

1. **本机自检**：daemon 在跑、设备已连接且已验证、`ssh-add -l` 能经 imk 的 agent socket 列出至少一把 key。任一不满足即退出并说明，不碰任何文件。
2. **解析 X**：跑 `ssh -G X`，取 `hostname` / `user` / `port` / `proxyjump`。用于后面所有 ssh 调用，也用于 `status` 判断「同一台」。
3. **写 A 的 ssh 配置**：不直接编辑 `~/.ssh/config`，而是：
   - 确保 `~/.ssh/config` 顶部有一行 `Include ~/.ssh/imk.d/*`（没有则插到第一行，`Include` 必须在任何 `Host` 块之前才是全局生效；有则不动）。
   - 写 `~/.ssh/imk.d/X`（0600）：
     ```
     # managed by imk remote — do not edit; run `imk remote remove X`
     Host X
         ForwardAgent yes
     ```
   - 多跳：`ProxyJump` 的中间主机不写 `ForwardAgent`。OpenSSH 的 `-J` 不在跳板上落 agent，转发只对最终目的地生效，所以只写 X 即可。
4. **安装远端 shim**：把与 A 上同版本的 `imk` Linux 静态二进制（按 X 的 `uname -m` 选 x86_64 / aarch64）经 `ssh X 'mkdir -p ~/.local/bin && cat > ~/.local/bin/imk.tmp && chmod +x … && mv …'` 放到 X。二进制从 A 的安装目录附带的 `remote/` 子目录取（release 打包时一并放入），不联网下载。
5. **写 X 的 shell 片段** `~/.config/imk/remote.sh`，并在 `~/.bashrc` / `~/.zshrc` 末尾加一行 `[ -r ~/.config/imk/remote.sh ] && . ~/.config/imk/remote.sh`（带 `# imk remote` 标记，remove 时按标记删）。片段内容：
   - 若 `$SSH_AUTH_SOCK` 不存在或不可连，且存在 `~/.imk-agent.sock` 链接可连，则改用它。
   - 每次交互登录（非 tmux 内）把当前 `$SSH_AUTH_SOCK` 软链到 `~/.imk-agent.sock`。
   这就是 tmux / screen 里 socket 过期的修法：长会话永远走那个链接，链接指向最近一次登录。
6. **端到端验证**：`ssh X 'ssh-add -l'`，输出里出现 A 的 key 指纹即成功。A 的终端此时不需要触摸（列 key 不过门）。
7. 在 A 记录 `~/.immurok/remote/X.json`：主机解析结果、写过的文件列表、时间、imk 版本。`status` 和 `remove` 依赖它。

### 4.2 幂等与失败

- 已存在 `~/.ssh/imk.d/X` 且内容一致 → 跳过；不一致（用户手改过）→ 提示并要求 `--force`。
- 第 4、5 步失败（X 上没有写权限、shell 不是 bash/zsh）→ 警告但**不算失败**：git/ssh 在这一步之前就已经可用，shim 和 shell 片段是增强。
- 第 6 步失败 → 回滚第 3 步的文件，报错退出。常见原因是 X 的 sshd `AllowAgentForwarding no`，错误信息要直接说这个。

## 5. `imk remote setup sudo X`

需要 X 的 sudo 密码一次（用户在 A 的终端输入，经 ssh 的 tty 转到 X 的 sudo）。

### 5.1 步骤

1. 先确保 §4 已完成（未完成就先跑一遍）。
2. **PAM 模块来源**（待确认 §10.1）。本稿选 **imk 自带**：release 里按 x86_64 / aarch64 附一份静态编译的 `pam_ssh_agent_auth.so`（上游 0.10.4+，含 ECDSA 支持），装到 X 的 `/usr/local/lib/security/`（不进发行版的 `/usr/lib/security`，避免和包管理器打架）。X 上已有发行版包时优先用发行版的，路径由 `status` 记录。
3. **公钥**：`ssh-add -L` 取 A 的全部 imk key，写 X 的 `/etc/security/imk_authorized_keys`（root:root 0644）。多把 key 全写，任一能签即可。
4. **PAM**：生成新的 `/etc/pam.d/sudo`：在第一行插入
   ```
   auth  sufficient  <模块路径>  file=/etc/security/imk_authorized_keys
   ```
   只动 `sudo`；`--polkit` 时同样处理 `/etc/pam.d/polkit-1`。**永远不碰** `system-auth` / `common-auth` / `password-auth`（`pam_immurok` 的部署教训，见 memory `feedback_pam_config`）。
5. **sudoers**：写 `/etc/sudoers.d/imk-remote`（0440）：
   ```
   Defaults env_keep += "SSH_AUTH_SOCK"
   ```
   落盘前 `visudo -cf <临时文件>`，不过就中止。
6. **备份**：原 `/etc/pam.d/sudo`（及 polkit-1）复制到 `/etc/pam.d/.imk-backup/sudo.<时间戳>`，路径记进 A 的 `X.json`。
7. **原子替换**：临时文件写在同一文件系统，`mv` 到位。
8. **原地验证**：在同一条 ssh 会话里跑 `sudo -k && sudo -n true`。此时 A 的终端出现指纹提示，用户摸一次。
   - 通过 → 完成，打印摘要。
   - 不通过（30 s 未摸 / 模块加载失败 / 签名验证失败）→ 用第 6 步的备份回滚第 4、5 步，删除 sudoers 片段，报错退出。回滚本身用本次会话仍然有效的 sudo 时间戳，不需要再输密码。
9. 提示用户：sudo 自己的时间戳缓存照常生效；想每次都摸，加 `--every-time` 让 sudoers 片段追加 `Defaults timestamp_timeout=0`（默认不加，尊重 X 原有策略）。

### 5.2 为什么不用 `pam_immurok`

`pam_immurok` 要求 daemon 在同一台机器，走特权分离后的 `pam.sock`，信任模型是「socket 在边界内」。隔着隧道用等于把边界交给 X。`pam_ssh_agent_auth` 只信任「签名验得过」，密钥在设备里，X 拿不到，这才是正确模型。

## 6. `imk remote status X` / `imk remote remove X`

`status`：读 A 的 `X.json`，再经 ssh 逐项核对：`imk.d/X` 存在且一致；X 上 `ssh-add -l` 可见 key；`imk_authorized_keys` 内容与当前 `ssh-add -L` 一致（设备重新生成 key 后会不一致，提示重跑 `setup sudo`）；PAM 行仍在首行；sudoers 片段仍在；shim 版本与 A 是否一致。每项一行 ✓ / ✗ / –。

`remove`：按 `X.json` 记录逆序删除。sudo 部分需要 X 的密码（PAM 一旦撤掉就不能再用指纹，所以先用指纹通过 sudo 拿到时间戳，再在同一会话里撤）。`--keep-ssh` 只撤 sudo。X 不可达时 `--local-only` 只清 A 侧并警告 X 上有残留。

不带 X 的 `imk remote status`：在 A 上列出所有配置过的主机；**在 X 上**（无 daemon、无 `X.json`）则做只读的本机自检：`SSH_AUTH_SOCK` 是否可连并列出 immurok key、`/etc/pam.d/sudo` 首行是否是 imk 的 PAM 行、`/etc/sudoers.d/imk-remote` 是否存在、shim 版本。这是 AI agent 在 X 上判断「sudo 会不会走指纹」的唯一入口（§9 第 2 条）。

## 7. 日常使用时的行为与限制

- 指纹提示（「Please verify your fingerprint...」）由 A 的 daemon 直接写进 A 上跑 `ssh X` 的那个终端，不是 X 打印的。同一窗口操作时看起来自然；从别的窗口 attach 到 X 的 tmux 时提示在原窗口。X 本身只看到卡住。姊妹稿 §11 第 4 条的 `intent-wait` 是解法，不在本稿。
- 每次 `ssh X` 必须从 A 发起。第三方登录 X 时上述功能全部静默失效，sudo 回落密码。
- 一次触摸后 10 s 内的后续签名吃固件 cooldown。姊妹稿方向 1 落地后，来自 X 的每次签名都要新触摸。
- A 是 macOS 时：§4 的 agent socket 路径和 `ssh-add -l` 检查走 macOS App 的 `SSHAgentServer`；其余步骤相同。

## 8. 与姊妹稿的接口

- 本稿第 §4.1 第 4 步装到 X 的 `imk` 就是姊妹稿 §6 的远端 shim。本稿只负责把它放过去；它连不上本地 daemon、看到 `SSH_AUTH_SOCK` 后的行为由姊妹稿定义。
- `pam_ssh_agent_auth` 签的是随机挑战，姊妹稿 §5.3 的 `verified.kind` 解析要加一个分支识别它的挑战格式（模块拼的 buffer 含 user、hostname、随机 cookie），否则显示 `unknown`。已在姊妹稿 §5.3 补记。
- X 上 `imk run --agent -- sudo …`：shim 登记 intent，sudo 的 PAM 挑战签名命中 intent → A 的对话框显示 X 上的命令。姊妹稿原 §2「远端 sudo 不在范围」已按此改写。

## 9. imk-skill（SKILL.md）更新清单

仓库 `immurok/imk-skill`，文件 `skills/using-imk/SKILL.md`。AI agent 只读这一份，不更新它就等于功能不存在。要改的地方：

1. **frontmatter `description`**：现在写的是「macOS BLE fingerprint companion」。补 Linux，并加触发条件「`SSH_AUTH_SOCK` 指向 ssh 转发 socket 且 `ssh-add -l` 列出的 key 注释含 `immurok`」，让 agent 在 X 上也能识别。
2. **新节「Remote hosts (forwarded agent)」**，放在 "Three patterns" 之后：
   - 说明 X 上没有 daemon、没有 `~/.immurok/`，但 `imk run --agent` 照样用，行为差异只有一条：指纹提示出现在用户 A 的终端，agent 在 X 上看不到任何提示，只看到命令在等，**不要**因为「没有输出」就中断或重试。
   - `sudo` 在 X 上是否走指纹取决于用户是否跑过 `imk remote setup sudo`；agent 用 `imk remote status`（X 上执行时是只读的本地探测：PAM 行、sudoers 片段、`ssh-add -l`）判断。没配的话 sudo 会要密码，agent 应告诉用户可以在 A 上跑 `imk remote setup sudo <this-host>`，而不是自己去改 PAM。
   - `--signs N`：一条命令要多次 SSH 连接（push 两个 remote、rsync 多次）时声明次数。
3. **"When NOT to wrap" 修正**：现有条目「ssh sessions without an attached macOS App — there's no human to fingerprint」不再成立，改为「ssh sessions **without a forwarded imk agent**（`ssh-add -l` 为空或报 no agent）」。
4. **"Discovery checklist"** 加远端分支：`which imk` 有、`~/.immurok/` 无、`echo $SSH_AUTH_SOCK` 形如 `/tmp/ssh-*/agent.*`、`ssh-add -l` 有 immurok key → 远端模式。
5. **"Troubleshooting"** 加三行：
   | 症状 | 原因 | agent 该做什么 |
   |---|---|---|
   | `imk: no local daemon and $SSH_AUTH_SOCK is not an imk agent` | 用户没从 A 带转发登录，或 A 不是 imk | 停止，告诉用户从 A 用 `ssh -A` 或先 `imk remote setup ssh` |
   | 命令卡住 30 s 后 ssh 报 `agent refused operation` / `Permission denied (publickey)` | 用户没在 A 上摸，或点了取消 | 视为拒绝，不重试 |
   | tmux 里突然要密码 | `SSH_AUTH_SOCK` 过期 | 提示用户 `source ~/.config/imk/remote.sh` 或重新登录；不要自己去猜 socket 路径 |
6. **"Security gotchas"** 加一条：X 上 `imk run --agent` 的命令文本会显示在 A 的对话框里，同样不要把秘密放在命令行。
7. 顶部一句话说明 imk 现在是 macOS / Linux 双端，避免 agent 在 Linux 上看到 `/Applications/immurok.app` 不存在就判定 imk 没装（现有 checklist 有这个误导）。

CLAUDE.md 的「Agent 工作约定」段加一行：「远端主机同样适用；见 imk-skill 的 Remote hosts 节」。

## 10. 待确认

1. **PAM 模块来源**：imk 自带静态 `.so`（本稿）vs 只用发行版包（Arch 用户得先装 AUR helper，「一条命令」在 Arch 上断掉）。自带的代价是要维护一个第三方 C 项目的构建和安全更新。
2. **`imk` 静态二进制随 A 的安装包分发**（本稿，不联网）vs setup 时从 GitHub release 下载（安装包小，但 setup 依赖网络和签名校验）。
3. `setup sudo` 默认是否包含 polkit-1。本稿默认不含，`--polkit` 开启。
4. X 上的 shell 片段是否值得做。它只解决 tmux 过期问题，但要动用户的 rc 文件；替代方案是只在文档里写一行说明。
5. A 是 macOS 时 `~/.ssh/imk.d` 与 `X.json` 的位置是否沿用 `~/.immurok/`。

## 11. 测试

- 三个发行版的 X 各跑一遍 §1.2 的成功标准；每台跑 `setup ssh → setup sudo → status → remove → status`，最后 diff `/etc/pam.d/sudo` 与 sudoers 目录。
- 故障注入：第 §5.1 第 8 步验证时不摸（超时）→ 确认回滚后 sudo 走密码；`visudo -c` 失败（人为写坏模板）→ 确认不落盘；X 上 `AllowAgentForwarding no` → 确认 §4 第 6 步给出明确原因。
- tmux：登录、开 tmux、断开、重新登录、attach、`git pull` → 不要密码。
- 多跳 `ProxyJump`：跳板上 `ssh-add -l` 报无 agent，X 上有。
- 设备重新生成 SSH key 后 `status` 报公钥不一致，`setup sudo` 重跑后恢复。
