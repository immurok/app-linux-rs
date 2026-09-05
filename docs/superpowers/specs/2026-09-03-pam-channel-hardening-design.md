# Linux：PAM 信道加固（daemon 特权分离）

日期：2026-09-03 初稿 / 2026-09-04 按实现回写
状态：**已实现并合入 main。Arch、Fedora 44（SELinux Enforcing）、Ubuntu 24.04 三个发行版真机验证通过**
实施计划与逐条踩坑记录：`docs/superpowers/plans/2026-09-04-pam-channel-hardening-linux.md`
来源：macOS 端同名改动（imPress-v1 主仓库 `docs/superpowers/specs/2026-09-03-pam-channel-mac-design.md`）。**Linux 的信任模型和 macOS 不同，不能照抄**，原因见 §2。

> 本文是「实现后回写」版本。初稿有三处结论在真机上被推翻，见 §6 —— 那一节刻意保留，
> 免得下一个人重新推导出同样的错误。

## 1. 问题

`pam/pam_immurok.c` 以 root（sudo）或 polkitd 身份运行，连接 daemon 的 socket，收到以 `OK`
开头的两个字节就返回 `PAM_SUCCESS`。加固前这个 socket 是 `chmod 0666`、目录归用户所有、
daemon 是用户级 systemd 单元、二进制在用户可写的 `~/.local/bin`。于是任何以该用户身份运行
的进程都能：

```bash
systemctl --user stop immurok-daemon
rm /run/user/$UID/immurok/pam.sock && nc -lU /run/user/$UID/immurok/pam.sock   # 回 "OK"
```

零交互过 sudo 和 polkit。`gdm-password` 被 `fp_policy.h` 跳过，所以受影响的正是 sudo 与
polkit-1 两条提权路径。

daemon 侧原来的 `verify_peer_credentials` 只按 `SO_PEERCRED` 的 uid 放行 root / 自己 /
polkitd —— 同 uid 的攻击者天然通过。

## 2. 为什么不能照抄 macOS

macOS 方案：daemon 持一把随机 `pam_key`，PAM 发 nonce、只认 `OK:<HMAC>`；安全性依赖
「攻击者拿不到 App 钥匙串里的密钥」，钥匙串条目按代码签名做 ACL。

Linux 没有这一层：daemon 的全部秘密（`pairing.json` 里的 `shared_key`）就是一个 0600 文件，
**同用户任何进程都能读**；gnome-keyring 默认也不按进程隔离。把 `pam_key` 放到同样的地方，
攻击者多读一个文件就能自算 MAC。nonce/HMAC 在 Linux 上只能挡最笨的 `nc` 抢占，不构成信任
边界。

**结论：Linux 上唯一能建立边界的手段是特权分离。** 边界一旦建立，socket 抢占在物理上不
可能，PAM 现有的「回 OK 即通过」协议就是安全的，nonce/HMAC 降级为可选的纵深防御（§7）。

## 3. 信任边界

- **边界内**：daemon 进程（uid `immurok`）、`/var/lib/immurok`、`/run/immurok`、
  `/usr/local/bin` 下的二进制、`pam_immurok.so`。
- **边界外（重要）**：`immurok-session-agent`。它跑在用户会话里，同 uid 的攻击者可以杀掉
  它或伪造它 —— 换来的只是「没有弹窗」或「一次多余的取消」，永远变不成放行。
- 攻击者模型：桌面用户身份下的任意进程，不含 root。
- 不在范围：root 被攻破；BlueZ / 内核；固件侧签名（三端「选项 2」另议）。

## 4. 实现

### 4.1 运行身份与路径

| 项 | 加固前 | 现在 |
|---|---|---|
| 运行方式 | `systemctl --user` 单元 | 系统单元，`User=immurok`、`RuntimeDirectory`/`StateDirectory`/`LogsDirectory`、`ProtectHome=yes`、`ProtectSystem=strict`、`NoNewPrivileges=yes` |
| 二进制 | `~/.local/bin`（用户可写、可替换） | `/usr/local/bin`（root 所有） |
| PAM socket | `/run/user/<uid>/immurok/pam.sock` | `/run/immurok/pam.sock`，0666；目录 `immurok` 所有 0755 |
| SSH agent socket | `~/.immurok/agent.sock`（0600） | `/run/immurok/agent.sock`，0666 + 活动会话校验 |
| 配对/设置/缓存 | `~/.immurok/*.json` | `/var/lib/immurok/*`（0700 immurok） |
| 日志 | `~/.immurok/logs.txt` | `/var/log/immurok/daemon.log` + daemon 内存环形缓冲（§4.6） |
| 固件下载缓存 | `~/.immurok/fwupdate/` | **不动** —— 那是 CLI 侧的东西，OTA 是 CLI 读文件后 base64 经 socket 推给 daemon |

路径解析集中在 `immurok-common/src/paths.rs`：`IMMUROK_*` 覆盖 → systemd 注入的
`RUNTIME_DIRECTORY`/`STATE_DIRECTORY`/`LOGS_DIRECTORY` → 编译期固定路径。daemon 与 CLI 共用
同一套，开发时用 `IMMUROK_*` 就能整套跑在临时目录里。**daemon 不再从 `$HOME` 派生任何东西**
（系统用户的 HOME 是 `/`，原来的 `create_dir_all("$HOME/.immurok")` 会直接 panic 在启动第一步）。

`SupplementaryGroups=bluetooth` **不能写进单元**：Arch 之类的发行版没有这个组，systemd 遇到
不存在的组是硬失败，单元直接起不来。改由 `immurok-pam-helper` 在组存在时 `usermod -aG`，
systemd 对 `User=` 会按 `/etc/group` 取补充组，效果相同。

### 4.2 PAM 模块

- socket 路径固定 `/run/immurok/pam.sock`（不再按 uid 拼）。
- 连接前 `lstat` 校验目录：**属主是 root 或 daemon uid，且 group/other 不可写**（判据是
  `pam/socket_trust.h`，带单测）。不能写成「必须 root 所有」——systemd 的 `RuntimeDirectory=`
  会把目录 chown 给服务用户，那样写 100% 自锁。`lstat` 而非 `stat`：路径上被塞符号链接时
  必须拒绝。
- `connect()` 改非阻塞 + 2s 上限。`SO_SNDTIMEO` **不覆盖 `connect()`**，而 AF_UNIX 在
  「对端在、accept 队列满」时会无限阻塞 —— 那是整条 PAM 路径上唯一没有上限的等待，且发生
  在按键/Ctrl+C 逃逸循环开始之前。
- `fp_policy.h`（gdm-password 跳过）与超时逻辑不变。
- **不下发任何 polkit override**，理由见 §6.1。

失败方向：模块是 `auth sufficient`，任何失败都只是这一行不生效，继续走 `pam_unix` 密码。

### 4.3 daemon 侧授权分级

`SO_PEERCRED` 的 uid 按**命令**分级，而不是按连接一刀切（socket 是全机一个了）：

| 命令 | 允许的对端 |
|---|---|
| `AUTH:*` | uid 0、polkitd |
| `STATUS`、`GET:INFO`、`GET:SETTINGS`、`PAIR:STATUS` | 任意本机用户 |
| 其余全部（`KEY:*`、`FP:*`、`PAIR:*`、`SLOT:*`、`SET:*`、`OTA:*`、`AGENT_APPROVE`、`SUBSCRIBE:*`…） | 当前在本机有**活动会话**的 uid，或 uid 0 |

- 活动会话判定走 `loginctl show-user <uid> -p State --value`，只认 `active`（`online` 会把
  「在另一个 VT 登录后走开的第二个用户」也算进来）。**不解析 `/run/systemd/users/<uid>`**
  —— 那个文件第一行就写着 "This is private data. Do not parse."。
- logind 不可用时回落到记录在案的 **owner uid**（`/var/lib/immurok/owner`），免得 logind
  出问题就把 CLI 彻底废掉。
- 拒绝时回 `DENY:NOT_AUTHORIZED` 而不是直接断连：断连在 CLI 那头只显示
  "Connection reset by peer"，什么信息都没有。

**owner 绑定**：配对成功时（以及迁移时由 helper）记下 owner uid，`handle_auth` 校验 `AUTH`
里的 `user` 字段解析出的 uid 必须等于 owner。socket 现在是全机一个，没有这一条的话，同机
第二个账号跑 sudo，机主的一次触摸就把 root 给了他。没有 owner 记录时只告警放行，不会把
单用户机器锁死。

### 4.4 需要会话才能做的事：`immurok-session-agent`

daemon 没有显示、没有会话总线、`ProtectHome=yes` 进不了家目录，所以凡是必须发生在会话里
的事都由一个用户级常驻小进程代办（单元装在 `/etc/systemd/user/`，挂 `default.target` 而
不是 `graphical-session.target`：纯 tty 登录同样需要 ssh_takeover 对账，而且不是所有 WM 都
会拉起 graphical-session.target）。

一条长连接，纯文本协议：

| 方向 | 消息 | 含义 |
|---|---|---|
| daemon → agent | `UI:DIALOG:AUTH` | 弹「触摸指纹」提示（polkit / gdm 路径） |
| daemon → agent | `UI:DIALOG:AGENT:<secs>:<cmd>` | agent 授权窗（命令 pill + 倒计时） |
| daemon → agent | `UI:DISMISS` | 收掉窗口 |
| daemon → agent | `NOTIFY:<text>` | 桌面通知 |
| daemon → agent | `SSH_TAKEOVER:ON\|OFF` | 对账 `~/.ssh/config`（每次订阅重推一次） |
| agent → daemon | `UI:CANCEL` | 用户点了取消/关窗 |

设计约束（安全相关，改的时候不要破坏）：

- **没有订阅者 = 没有 UI，认证照常**。推送失败一律按「无 UI」继续，绝不当成失败或同意。
- 对话框退出码区分「是我们 SIGTERM 关的（0）」和「用户关的（非 0）」；后者才是取消。
- 因此 UI 通道只能用来**拒绝**，永远不能作为放行依据。

锁屏状态与解锁也走 daemon（不经代理）：logind 的 `LockedHint`（系统总线信号做唤醒 + 30s
兜底轮询），锁屏/解锁用 `loginctl lock-session <id>` / `unlock-session <id>` 显式指定 owner
的图形会话 —— **无参形式作用于调用者自己的会话**，系统 daemon 没有会话，只会失败。polkit
rules 放行的 action 是 `org.freedesktop.login1.lock-sessions`：logind 里**没有**
`unlock-sessions` 这个 action，Lock/Unlock 共用同一个 id。

### 4.5 安装 / 迁移 / 卸载

- **所有 root 步骤收进 `scripts/install-root.sh`，`make install` 只调一次 `sudo`**。散着调
  sudo 在这里不成立：无 tty 时 sudo 的时间戳按 **ppid** 记，make 每条 recipe 都是新父进程，
  缓存完全不生效；而新 `.so` 一装上，在系统 daemon 起来之前指纹 sudo 必然是断的 —— 两者
  叠加就是安装跑到一半掉进密码提示。卸载同理（`uninstall-root.sh`）。
- PAM 模块必须**临时文件 + 原子 rename**，绝不原地覆盖：跑着这条命令的 sudo 自己 mmap 着
  旧的 `.so`，原地覆盖会打烂它的代码页（`pam_end` 里 `dlclose` 时 SIGSEGV）。
- 迁移（`immurok-pam-helper migrate-daemon <user>`，幂等）：停旧的用户级单元 → 建系统用户 →
  建 state 目录 → 搬数据 → 记 owner → 删旧 polkit override → `enable` + **`restart`**。
  一步做完，中间不留两个 daemon 抢 BLE 的窗口。`--now` 不够：它对已在运行的服务是空操作，
  重装新二进制时旧进程会一直跑着。
- 搬数据时**以目标用户身份读源文件**（`runuser -u <user> -- cat`），并拒绝符号链接源：源文件
  在用户可写的目录里，否则能诱导 root 把别的文件复制进 `/var/lib/immurok`。原件改名
  `.migrated` 而不是删除，回滚时用户能自己搬回去。
- pkexec 的 `exec.path` 必须指向 root 所有的路径。加固前它指向 `~/.local/bin/immurok-pam-helper`
  —— 用户可写，任何该用户的进程都能改写它，等一次 admin 授权就拿到 root。
- 卸载最后重启一次 polkit 并打印状态，作为「清掉 override 后 polkit 仍能起来」的验收项。

### 4.6 CLI / TUI

daemon 的 state 是 0700、日志是 0640，CLI/TUI 不再直接读文件：

- `KEY:CACHE:<ssh|names>` → `OK:<json>`，内容就是磁盘缓存原样。**放在连接性检查之前** ——
  缓存读取必须在设备不在时也能用，那才是缓存的意义。
- `SUBSCRIBE:LOG`：daemon 自持 500 行环形缓冲 + 实时广播（装成 tracing 的 writer，与写文件
  同一条路），先发历史再发实时。没有沿用「CLI 直接读日志文件、不行就回退 journalctl」：文件
  0640 读不到，改成世界可读会在多用户机器上公开每一次认证事件，而 journalctl 读系统单元要求
  用户在 wheel/adm/systemd-journal 组里，不能假定。
- `GET:INFO` 追加 `uid=` 与 `sock=`（追加在末尾，老解析器按前缀取值不受影响），
  `immurok-cli settings` 与 TUI 的 PAM tab 各显示一行隔离状态；未隔离时红色置顶且写明要跑
  `make install`。
- `imk run --agent` 的「agent 归类」改为 `AGENT_APPROVE` 时由 daemon 用 `SO_PEERCRED` 的 pid
  在内存里登记（TTL 1h，登记时清掉 `/proc/<pid>` 已消失的条目）。原来的
  `~/.immurok/markers/<pid>` 文件行不通：0600 归调用者，daemon 读不到；放宽到 0644 会把
  agent 执行的命令文本泄露给同机其他用户。仍然只用于日志归类，不参与放行。

### 4.7 绑定失败必须响亮

`socket::serve` / `ssh_agent::serve` 绑定失败时是 `error!` + `exit(1)`。原来只 `warn!` 后
`return`，而 `main()` 是 `tokio::select!` —— 任一分支返回就整体以 **exit 0** 结束，
`Restart=on-failure` 根本不触发，指纹功能会静默死掉（仍会回退密码，但没人知道为什么）。

## 5. 验收

`scripts/test-isolation.sh`，普通用户身份跑，**每条都必须失败**（用 root 跑脚本会拒绝执行）：

```
删除 PAM socket / 在 runtime 目录新建文件 / kill daemon / 读 pairing.json /
改写 daemon 二进制 / 直接连 socket 发 AUTH
```

最后一条验的是授权分级而不只是文件权限，期望 `DENY:NOT_AUTHORIZED`。

单测：`paths` 解析回退、`socket_trust.h` 的目录判据（含 sticky 0o1777、符号链接、daemon 用户
不存在）、logind State/LockedHint 解析、owner 文件解析、迁移脚本 17 条断言。

真机（2026-09-04，Arch/GNOME/polkit 127/FW 1.7.9.334a）：隔离 6/6；`imk run --agent` 预授权、
纯 sudo 触摸、`pkexec` 三条认证路径全通；取消路径 `agent command rejected by user`；锁屏解锁
正常；`ssh-add -l` 经新 socket 列出 key。

## 6. 与初版设计稿的差异（初稿错在哪）

### 6.1 polkit override：因果搞反了，而且按稿改会制造整机故障

初稿称「`ProtectSystem=strict` 把文件系统挂成只读，`connect()` 需要写权限所以返回 EROFS，
现有的 `BindPaths=/run/user` 正是为此存在」，据此要求把 override 改成
`ReadWritePaths=/run/immurok`。实测（Arch，polkit 127）：

- `strict` 下 `/run` 确实是 `ro` 挂载，但在 `strict` + `ProtectHome=yes` 下 `connect()` 到
  `/run/dbus/system_bus_socket` **成功** —— 只读挂载不阻止连接 unix socket。内核里
  `sb_permission()` 那条 EROFS 判断只对 `S_ISREG/S_ISDIR/S_ISLNK` 生效，**socket 被排除**；
  只读 bind mount 靠 `mnt_want_write` 把关，socket 的 connect 路径不走它。
- 真正挡住我们的是 **`ProtectHome=yes` 会把 `/run/user/<uid>` 整个屏蔽**（EACCES）。

所以 socket 搬到 `/run/immurok` 之后**两个 override 应当整个删除**，顺带把 agent-helper 的
`ProtectHome=no` 拿掉，等于把 polkit 沙箱还原成发行版默认。

而初稿的 `ReadWritePaths=/run/immurok` 会**主动制造**一个整机故障点：`RuntimeDirectory=` 的
目录在服务停止时被 systemd 删除，而 `ReadWritePaths=` 指向不存在的路径会让单元以
**`226/NAMESPACE`** 启动失败 —— immurok 一停，polkit 起不来，整机图形授权/挂载/设置全废。
若将来某个发行版实测确需 override，必须写成 `ReadWritePaths=-/run/immurok`（`-` = 路径不存在
则忽略）并配 tmpfiles 让目录常驻。

### 6.2 PAM 的目录判据写成了「必须 root 所有」

`RuntimeDirectory=` + `User=immurok` 下 systemd 会把目录 chown 给服务用户，照稿实现 100%
自锁。正确判据见 §4.2。

### 6.3 会话相关的东西被低估了一个数量级

初稿 §4.4 只提到「桌面通知若有，先做没有通知也能工作」。实际断掉的是一整层，而且都是真机
部署时才暴露的：

| 东西 | 加固后的表现 |
|---|---|
| 授权对话框（daemon 直接 spawn GTK） | 连不上显示服务器 → 瞬间非零退出 → 被判成「用户取消」→ **每次 `imk run --agent` 都被拒** |
| 锁屏状态（session bus 的 ScreenSaver 信号） | 系统 daemon 没有 session bus → 每 5 秒一条 EACCES 刷日志，触摸解锁静默失效 |
| `loginctl unlock-session`（无参） | 作用于调用者自己的会话，系统 daemon 没有会话 → 必失败 |
| `~/.ssh/config` 的 `IdentityAgent` | daemon 写不了；且旧值指向已消失的 `/run/user/<uid>/…`，git push / ssh 静默走不到指纹签名 |
| SSH agent socket（0600 + 同 uid 校验） | 真人完全用不了 |
| CLI/TUI 的 peer 校验（「对端 uid == 自己 uid」） | 把设备的主人挡在了外面 |
| 日志文件、key 缓存文件 | CLI/TUI 读不到 |
| `imk` 的 0600 marker 文件 | daemon 读不到，且不能放宽（会泄露命令文本） |

**教训**：换权限模型时，凡是「daemon 与用户同 uid」这个前提下写对的代码，都要逐条重新审视 ——
它们不会报错，只会静默失效。这一类问题这次一共踩到八个，设计稿一个都没预见到。这也是为什么
后来改成「先把安装脚本做出来上真机」，而不是按顺序把 P2/P3 做完再说。

### 6.4 `/var/cache/immurok` 那条搞反了

固件下载缓存归 CLI（用户身份下载后经 socket 推给 daemon），daemon 从不碰它，应该留在家目录。

## 7. 可选：nonce + HMAC（原方案 B）

在特权分离之上再做，价值是三端协议一致、PAM 的 C 代码可与 macOS 共用。安全增益有限：daemon
被攻破（以 `immurok` 身份）时也拿得到 `pam_key`，所以对「daemon 被攻破」无增益；只对「有人把
socket 目录权限配错」是一道兜底。要点与 macOS spec §5 一致，不在此重复。

## 8. 兼容性

| 项 | 结论 |
|---|---|
| systemd 发行版 | 全部支持 `RuntimeDirectory`/`StateDirectory`/`User=`；无新增依赖 |
| 非 systemd | 现状的 `systemctl --user` 单元本就不支持，不是回归；`paths` 的固定路径回退保证可手动跑 |
| BlueZ | bond 本就是系统级，换 daemon 身份不需要重新配对；自带 D-Bus policy 显式授权 `immurok` 用户，开机未登录即可连接 |
| `bluetooth` 组 | **不能假定存在**（Arch 就没有），见 §4.1 |
| polkit 新版 / 旧版 | 都不需要 override，见 §6.1 |
| SELinux（Fedora） | **已验证**：Fedora 44 / polkit 127 / SELinux **Enforcing** / Wayland+GNOME 下全套验收 20/20 通过，`pkexec` 路径不需要任何 `.te` 策略模块（`policykit_auth_t` 能连 `/run/immurok` 下的 socket） |
| 屏幕解锁 | 依赖 logind `LockedHint`；swaylock/i3lock 不上报，与加固前的 ScreenSaver 信号方案有同样的盲区 |
| python `dbus_fast` | 必须是**系统级**可见（`ProtectHome=yes` 下用户 `pip --user` 装的看不到）。系统 daemon 从 systemd 拿到默认 PATH → `/usr/bin/python3` |
| 多用户 / 快速切换 | 按 logind 活动会话服务；非活动会话的用户用不了设备（设计如此） |
| 不可变系统 | `/usr/local`、`/etc` 可写即可装；NixOS 需单独 module |

## 9. 残余风险

- root 被攻破：无边界可言。
- `immurok` 用户被攻破（daemon 漏洞）：攻击者拿到 `shared_key`，可伪装设备；与加固前持平。
- **触摸劫持**：同 uid 攻击者先发起 `sudo`，用户为自己的操作触摸 → 授权落到攻击者的请求上。
  `try_set_pending_pam` 的互斥只保证不串台，不解决先来先得。无显示屏令牌下无解。
- **会话 UI 不可信**：见 §3 与 §4.4 —— 只能用来拒绝，不能用来放行。
- 多用户机器上非活动会话的用户无法使用设备（设计如此）。
