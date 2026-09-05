# Linux PAM 信道加固：合理性评审 + 实施计划

日期：2026-09-04
对应设计稿：`docs/superpowers/specs/2026-09-03-pam-channel-hardening-design.md`（方案 A：daemon 特权分离；方案 B：可选 nonce/HMAC）
评审依据：本仓库当前实现（daemon / pam / cli 全量读过，引用行号为 2026-09-04 的 main）

---

## 1. 合理性结论

**方案 A 的方向是对的，必须做；但设计稿低估了 blast radius 约一倍。**

对的部分（复核通过）：

- 威胁模型准确。socket 是 `chmod 0o666`（`crates/immurok-daemon/src/socket.rs:38-43`），目录归用户，daemon 是用户级 systemd 单元、二进制在 `~/.local/bin`（`immurok-daemon.service:7`）。同 uid 进程停掉 daemon 再自己 `bind` 同名 socket，回 `OK` 即过——PAM 侧确实只看前两字节（`pam/pam_immurok.c:215-219`）。零交互过 sudo / polkit 属实。
- 「Linux 上 nonce/HMAC 单独用没有信任边界」判断正确。`shared_key` 就在 `~/.immurok/pairing.json` 0600（`crates/immurok-common/src/security.rs:159-199`），同 uid 可读；再加一把 `pam_key` 只是多读一个文件。方案 B 降级为可选是对的。
- 特权分离是 Linux 上唯一能建立边界的手段：AUTH 的可信性依赖 `shared_key` 的保密性，而 BLE 链路的加解密就在 daemon 里，所以「把 BLE + 密钥整体搬到专用系统用户」是必然结论，无法只搬 socket。
- 失败方向安全。`pam_immurok.so` 是 `auth sufficient`（`scripts/immurok-pam-helper:11`），迁移失败 / daemon 没起 → connect 失败 → `PAM_AUTH_ERR` → 落到密码，不会把用户锁在门外。
- BlueZ bond 由 bluetoothd（root）持有，换 daemon 运行身份不需要重新配对；D-Bus 侧自带 policy 授权 `immurok` 用户的做法比依赖各发行版默认策略稳。

需要改的部分见下节。**结论：按 A 做，但必须把「用户级会话代理（session agent）」从设计稿的「后续再说」提升为 P0 必做件**，否则迁移完成之日就是授权对话框、锁屏检测、SSH 接管、终端 spinner 集体失效之时。

---

## 2. 设计稿的缺口（按严重度）

### F1（高危，会打穿整机）：override 的因果判断是错的，且按稿改会引入整机故障

设计稿 §4.2 的论证是：「新版 polkit 的 agent-helper 是 `ProtectSystem=strict`（整个文件系统只读挂载），而 `connect()` 到 Unix socket 需要对 socket 文件的写权限，只读挂载下返回 EROFS。现有的 `BindPaths=/run/user` 正是为此存在」，据此要求把 override 改成 `ReadWritePaths=/run/immurok`。

**这两句都不成立。2026-09-04 在本机（Arch，systemd，polkit 127，polkit.service 与 polkit-agent-helper@.service 均为 `ProtectSystem=strict` + `ProtectHome=yes`）实测：**

| 实验 | 结果 |
|---|---|
| `ProtectSystem=strict` 下 `/run` 挂载选项 | `ro,nosuid,nodev,…` —— 确实只读 |
| `ProtectSystem=strict` + `ProtectHome=yes` 下 `connect()` 到 `/run/dbus/system_bus_socket`（就在这个只读 `/run` 里） | **connect OK** —— 只读挂载**不**阻止连接 unix socket |
| `ProtectHome=yes` 下访问 `/run/user/1000/` | `Permission denied`，`connect()` 得到 `EACCES(13)` |
| `ProtectSystem=strict` 下 `/run/user/1000` 是否可写 | 可写（用户单元里 `$XDG_RUNTIME_DIR` 被豁免） |

内核层面的原因：`sb_permission()` 里那条 EROFS 判断只对 `S_ISREG / S_ISDIR / S_ISLNK` 生效，**socket 类型被排除在外**；而只读 bind mount 是通过 `mnt_want_write` 一路把关的，socket 的 connect 路径根本不走它。

所以：

1. 现有 override 存在的真正原因是 **`ProtectHome=yes` 会把 `/run/user/<uid>` 整个挡掉**（这也正是 Makefile 里同时下发 `BindPaths=/run/user` 和 agent-helper 的 `ProtectHome=no` 的原因，`Makefile:73-79`）。
2. socket 一旦搬到 `/run/immurok`，`ProtectHome` 管不着它，`ProtectSystem=strict` 也不拦连接 —— **两个 override 应当整个删除，而不是改路径**。顺带把 agent-helper 的 `ProtectHome=no` 拿掉，等于把 polkit 的沙箱还原回发行版默认，是净收益。
3. 而设计稿提出的 `ReadWritePaths=/run/immurok` 不但没有必要，还会**主动制造**一个整机故障点：`RuntimeDirectory=` 的目录在服务停止时被 systemd 删除（实测确认），而 `ReadWritePaths=` 指向不存在的路径会让单元以 **`226/NAMESPACE`** 启动失败（实测确认）。即：immurok 一停 → polkit.service 起不来 → 整机图形授权、挂载、网络设置全废。

修正：

- **默认做法：`make install` 不再下发任何 polkit override，`make uninstall` 负责把历史遗留的两个 `immurok.conf` 删掉。**
- 万一在某个发行版上实测发现确实连不上（SELinux 标签、或某发行版给 polkit 加了 `InaccessiblePaths` / `TemporaryFileSystem` 之类），才补 override，且**必须写成 `ReadWritePaths=-/run/immurok`**（`-` = 路径不存在则忽略），同时用 tmpfiles 让目录常驻：`/usr/lib/tmpfiles.d/immurok.conf` → `d /run/immurok 0755 immurok immurok -`，daemon 单元加 `RuntimeDirectoryPreserve=yes`。

### F2（高危，按稿实现会自锁）：PAM 侧目录属主判据写错了

设计稿 §4.2 要求 `/run/immurok` 「必须 root 所有」。但 `RuntimeDirectory=` + `User=immurok` 下，systemd 会把目录 chown 给 `immurok:immurok`。照稿写死 root 会 100% 拒绝。

判据应为：**属主是 root 或 `getpwnam("immurok")` 的 uid，且 group/other 均不可写**（`(st_mode & (S_IWGRP|S_IWOTH)) == 0`），`immurok` 用户不存在时直接 fail。

### F3（高危，功能整块消失）：授权对话框不是「通知」，是主链路

设计稿 §4.4 只提到 `notify-send`。实际情况是 daemon 自己 `spawn` 了一个 GTK4/Adw 的授权窗口：`spawn_auth_dialog()`（`socket.rs:492-525`）、`spawn_agent_dialog()`（`socket.rs:545+`），polkit / gdm / `imk run --agent` 三条路径都靠它（`socket.rs:394-401` 的 `is_graphical`）。它依赖 `WAYLAND_DISPLAY`/`DISPLAY`/`XAUTHORITY`/`DBUS_SESSION_BUS_ADDRESS`，靠继承用户会话环境工作。

`User=immurok` + `ProtectHome=yes` 之后：连不上用户的 Wayland/X，也进不了 `$XDG_RUNTIME_DIR`。窗口彻底起不来 → agent 模式看不到「在授权哪条命令」、polkit 无提示。这不是可降级的锦上添花，是 `imk run --agent` 的核心 UX。

必须新增用户级会话代理（见 §4 P2）。同时保留现有的失败语义：**对话框缺失 → 无 UI 但仍走触摸门；对话框被关掉/非零退出 → 拒绝**。这条方向是 fail-safe 的（同 uid 攻击者能压掉窗口只会导致拒绝，不会导致放行），迁移后不能反过来。

### F4（高危，锁屏解锁整条链断）：锁屏状态走的是 session bus

`screen.rs:38` 用 `Connection::session()` 订阅 `org.gnome.ScreenSaver` / `org.freedesktop.ScreenSaver` 的 `ActiveChanged`。系统 daemon 没有 session bus，`screen_locked` 永远是 false，`handle_fp_match` 的「锁屏 → loginctl unlock」分支（`coordinator.rs:244`）和长按锁屏的抑制逻辑（`coordinator.rs:352-360`）都失效。

另外两处设计稿没提：

- `unlock_screen()` / `lock_screen()` 调的是无参 `loginctl unlock-session`（`coordinator.rs:323, 374`），无参形式取调用进程自己的会话；系统 daemon 没有会话，必调失败。必须显式解析 session id（logind `ListSessions` / seat0 `ActiveSession`）再传参。
- polkit action 名写错了。logind 里没有 `org.freedesktop.login1.unlock-sessions`；解锁别人的会话走的是 **`org.freedesktop.login1.lock-sessions`**（同一个 action 覆盖 Lock/Unlock）。rules 文件按稿写会不生效。

替代实现：锁屏状态改用系统总线 logind 的 `Session.LockedHint` 属性 + `Lock`/`Unlock` 信号；GNOME/KDE 都会置位。对不上报 LockedHint 的锁屏（swaylock/i3lock）用 session agent 上报兜底。

### F5（中高危，语义变化没兜住）：单 daemon + 多用户，AUTH 的 user 字段完全不参与鉴权

`handle_auth` 只把 `user` 写进日志（`socket.rs:342-372`），从不校验。今天靠「每个用户一个 socket」天然隔离；改成全机一个 `/run/immurok/pam.sock` 之后，配对数据也从 per-user 变成 machine-level，于是：同机第二个用户跑 `sudo`，daemon 照样向设备发 AUTH，机主一次触摸就把 root 给了别人。

设计稿 §4.3 只限制了「谁能连（uid 0 / polkitd）」，没限制「给谁授权」。必须补：

- 配对时把 owner uid 落到 `/var/lib/immurok/owner`；
- `AUTH:user:service` 里的 user 解析成 uid，必须 == owner uid，否则 `DENY:NOT_OWNER`；
- 管理类命令的「活动会话」判据建议用 `sd_uid_get_state(uid) == active`（覆盖 SSH / 无 seat 的场景），而不是死盯 seat0 的 `ActiveSession`。

### F6（中，工作量被低估）：家目录耦合的清单不全

设计稿 §4.1 的表只列了 pairing/settings/socket/markers/fwupdate。实际还有：

| 位置 | 现状 | 迁移后的问题 |
|---|---|---|
| `main.rs:20-30` | daemon 日志写 `$HOME/.immurok/logs.txt`，`expect("HOME not set")` | 系统用户 HOME=`/` → `create_dir_all` 失败 → **启动即 panic** |
| `cli/main.rs:94-100`、`tui/app.rs:1869` | CLI/TUI tail 同一个文件 | 路径失效，日志面板空白 |
| `ssh_config.rs:65-90` | daemon 直接改写 `~/.ssh/config` 注入 `IdentityAgent` | `ProtectHome=yes` 下写不了，`ssh_takeover` 开关变空转 |
| `ssh_agent.rs:527-626` | 读 `/proc/<pid>/fd/*` 找客户端 tty，往 `/dev/pts/N` 画 spinner | 跨 uid 读 fd 需 CAP_SYS_PTRACE、pts 是 `user:tty 0620` → 静默降级为无 spinner |
| `commands/keys.rs:24-60, 240-250` 与 **`tui/app.rs:633-657`** | CLI 与 TUI 都直接读 `~/.immurok/{ssh_keys,key_names}.json` | `/var/lib/immurok` 0700 → 读不到，两处都必须改走 socket（`LIST` / `GET:<cat>` 已存在，`socket.rs:1209-1290`） |
| `fwupdate/store.rs:34-45` | 固件下载缓存在 `~/.immurok/fwupdate`，**归 CLI 所有** | 设计稿要挪到 `/var/cache/immurok` 是搞反了：OTA 是 CLI 读文件后 base64 走 socket 推给 daemon（`fwupdate/push.rs:63-106`），daemon 从不碰这个目录。**保持在用户家目录不动。** |
| `ble.rs:1815-1832` | `find_helper_script()` 回退到源码树 | `ProtectHome` 下失效；helper 与 dialog 必须一起装进 `/usr/local/bin`，并加 `IMMUROK_HELPER_DIR` 覆盖供开发用 |

### F9（高，真机部署时发现）：对话框退出 == 用户取消，于是没有桌面就等于永远拒绝

`handle_agent_approve` 把「对话框进程退出」一律当作 Cancel（`socket.rs:1253-1261`）。系统 daemon 能 spawn `immurok-auth-dialog`，但它连不上显示服务器、瞬间非零退出 —— 于是每一次 `imk run --agent` 都被判成用户点了取消。方向是 fail-safe 的（拒绝而非放行），但整条 agent 授权路被堵死。

已修：`dialog_possible()` 检查 `WAYLAND_DISPLAY`/`DISPLAY`，系统 daemon 永远没有这两个变量 → 根本不 spawn，直接走无 UI 的触摸门。P2 的 session agent 接管后这个判据自然失效（届时由订阅者存在与否决定）。

### F10（高，真机部署时发现）：SSH agent socket 0600 把用户锁在外面

`ssh_agent::serve` 把 socket chmod 0600 且 `verify_peer_uid` 要求 `cred.uid == getuid()`。daemon 变成 `immurok` 之后，SSH 签名对真人完全不可用。已改为 0666 + 与管理命令同一套「活动会话 / owner」校验；socket 所在目录普通用户写不了，所以放开权限位不降低边界。

### F11（中，真机部署时发现）：`~/.ssh/config` 里的 IdentityAgent 指向迁移前的路径

`ssh_takeover` 写进去的是 `/run/user/<uid>/immurok/agent.sock`，迁移后该路径消失，git push / ssh 静默走不到指纹签名。迁移脚本目前不改它（daemon 在 `ProtectHome=yes` 下也写不了）——归 P2/T2.5 的 session agent。本机已手工改指，`~/.ssh/config.bak-immurok-migration` 是备份。

### F12（中，真机部署时发现）：装机脚本不能散着调 sudo

无 tty 环境下 sudo 的时间戳按 **ppid** 记（`timestamp_type` 在没有 tty 时退化），make 每条 recipe 都是新父进程 → 缓存完全不生效；而新 `.so` 一装上，在系统 daemon 起来之前指纹 sudo 必然是断的。两者叠加 = 安装跑到一半掉进密码提示。

已改：所有 root 步骤收进 `scripts/install-root.sh` / `uninstall-root.sh`，Makefile 只调一次 sudo。顺带 `enable --now` 改 `enable` + `restart`，否则重装新二进制时旧进程会一直跑着。

### F8（中，实现时发现）：bind 失败会让 daemon 以退出码 0 静默消失

`socket::serve` / `ssh_agent::serve` 绑定失败时只 `warn!` 后 `return`，而 `main()` 是 `tokio::select!`，任一分支返回就整体结束 —— 进程以 **exit 0** 退出，`Restart=on-failure` 根本不会触发。用户级单元时代这个问题被"反正用户会重登录"掩盖了；换成系统服务后，`/run/immurok` 里一个残留的 root 属主 socket 文件就能让指纹功能静默死亡（仍会回退密码，但没人会知道为什么）。

已修：两处改为 `error!` + `std::process::exit(1)`，让 systemd 真正重启。2026-09-04 实测退出码为 1。

### F7（低，但要记）：markers 目录降级为 1777 是可接受的

`classify_agent_marker`（`socket.rs:101-131`）确实只用于日志分类，不参与放行——搬到 `/run/immurok/markers`（1777, sticky）不引入新授权风险。但日志措辞要改成「claimed agent context」，避免以后有人误当成可信信号。

---

## 3. 路线取舍：先做一次决策

方案 A 的实际成本集中在「会话相关的东西全要拆成两半」（F3/F4/F6），而不是路径改名。开工前值得把设计稿 §3「不在范围」里那个「选项 2（固件侧签名）」拉回来正式比一次：

| | A：daemon 特权分离 | C：设备签名 nonce（原「选项 2」） |
|---|---|---|
| 做法 | daemon 跑 `immurok` 用户，密钥/socket 进系统路径 | daemon 留在用户会话；install 时 root 生成 `pam_verify_key`，一次性写进设备，副本存 `/etc/immurok`（root 0600）；PAM 发 nonce，设备签名，PAM 用 root-only 密钥验 |
| 挡住零交互提权 | 是 | 是（daemon 见不到 verify key，无法伪造） |
| 保护 `shared_key` | 是 | 否（仍在用户家目录，攻击者可冒充主机向设备发请求——但每次仍需真实触摸） |
| Linux 侧改动 | daemon/PAM/CLI/TUI/install + 新增 session agent，~10-14 人日 | PAM 加验签、install 加下发、daemon 加中继，~3-4 人日；会话架构零改动 |
| 其他 | 多用户语义要重定义；桌面通知/对话框/ssh 接管全要拆 | 需固件协议 + OTA 全量升级；三端同步；设备侧要有密钥槽 |

建议：

1. **如果固件协议这一版还能动**：C 的性价比明显更高，且 A 的残余风险（触摸劫持）它一样存在。可以先上 C 关掉「零交互提权」这个真洞，把 A 排到大版本。
2. **如果固件已冻结 / 不想再推一次 OTA**：按 A 做，但按 §4 的排期，session agent 与路径迁移同批上线，不允许「先迁移、UI 后补」——中间态是用户可感知的功能倒退。
3. 方案 B（nonce/HMAC）在 A 之后再做，价值只有「三端协议一致 + 目录权限配错时的兜底」，不排 P0。

下面的计划按 **路线 A** 展开（用户若选 C，另出一份短计划）。

---

## 4. 实施计划（路线 A，约 10-14 人日）

### P0 · 前提验证（0.5 人日，不写业务代码）

- [ ] T0.1 验证 F1 的结论在目标发行版上成立：**先把两个 `immurok.conf` override 删掉**，手工建 `/run/immurok` + socket，确认 `pkexec true` 能连通；再确认 immurok 停止后 `systemctl restart polkit` 正常。Arch 已于 2026-09-04 实测通过（polkit 127），Ubuntu / Fedora 各跑一遍。
- [ ] T0.2 验证跨 uid 连通性：`sudo -u nobody` 起一个监听 `/run/immurok/pam.sock` 的假服务端，`sudo` 与 `pkexec` 两条路径都能连上且拿到 `OK`（确认 SELinux/AppArmor 不拦；Fedora 单独跑一遍）。
- [ ] T0.3 验证 `immurok` 用户 + `bluetooth` 组 + 自带 D-Bus policy 下能完成 BlueZ 连接与 GATT notify（不登录图形会话也能连）。

**任一项不过就回到 §3 重新选路线。**

### P1 · 路径与身份骨架（2.5 人日）

- [x] T1.1 `immurok-common` 新增 `paths` 模块：`runtime_dir()`（`$RUNTIME_DIRECTORY` → `/run/immurok`）、`state_dir()`（`$STATE_DIRECTORY` → `/var/lib/immurok`）、`log_dir()`（`$LOGS_DIRECTORY` → `/var/log/immurok`）、`user_dir()`（CLI 侧仍是 `~/.immurok`，供 fwupdate 缓存/CLI 自身状态）。全部允许 `IMMUROK_*` 环境变量覆盖以便开发。单测覆盖三种回退。
- [x] T1.2 `security.rs:159-169` 的 `dirs_home()` 换成 `paths::state_dir()`；保留写文件 0600。
- [x] T1.3 `daemon/main.rs` 去掉全部 `HOME` 依赖：日志改写 `paths::log_dir()/daemon.log`（0640；同时保留 stderr → journald），`immurok_dir` 改 `state_dir()`。**这条修的是 F6 的启动 panic。**
- [x] T1.4 systemd 系统单元 `packaging/immurok-daemon.service`：`User/Group=immurok`、`SupplementaryGroups=bluetooth`、`RuntimeDirectory=immurok`、`RuntimeDirectoryPreserve=yes`、`StateDirectory=immurok`、`LogsDirectory=immurok`、`ProtectHome=yes`、`ProtectSystem=strict`、`NoNewPrivileges=yes`、`Restart=on-failure`。
- [x] T1.5 `packaging/tmpfiles.d/immurok.conf`（F1）、`packaging/dbus/immurok.conf`（org.bluez 授权）、`packaging/polkit/49-immurok.rules`（**action 名用 `org.freedesktop.login1.lock-sessions`**，见 F4）。
- [x] T1.6 PAM 模块：socket 路径常量化为 `/run/immurok/pam.sock`（去掉 `getpwnam` 拼 uid，`pam/pam_immurok.c:36-37, 101-110`）；连接前按 **F2 的正确判据** `lstat` 校验目录，失败 `PAM_AUTH_ERR` + `pam_syslog`。
- [x] T1.7 helper 落位（`IMMUROK_HELPER_DIR` 覆盖已加；装进 `/usr/local/bin` 归 P4）：`ble-notify-helper.py` / `immurok-auth-dialog` 装到 `/usr/local/bin`；`find_helper_script()` 增加 `IMMUROK_HELPER_DIR` 覆盖。

验收：手工建好用户与目录后，daemon 以 `immurok` 身份跑起来、BLE 连上、终端 `sudo` 触摸通过。

**2026-09-04 进度**：代码侧完成并冒烟通过 —— `env -u HOME IMMUROK_RUNTIME_DIR=… IMMUROK_STATE_DIR=… IMMUROK_LOG_DIR=… immurok-daemon` 能在**完全没有 `$HOME`** 的环境下启动、绑定两个 socket、state 目录落 0700、日志写进 log 目录，`immurok-cli status` 与 `imk list ssh` 经同一套解析连上。需要 root 的部分（建用户、装单元、真机 sudo/polkit）留在 P4/T0。

### P2 · 用户级会话代理 immurok-session-agent（3.5 人日，F3/F4/F6 的解药）

新 crate `crates/immurok-session-agent`，用户级 systemd 单元 `immurok-session-agent.service`（`WantedBy=graphical-session.target`）。它是唯一还留在会话里的组件，**不持有任何密钥，也不参与授权判定**。

- [x] T2.1 协议：agent 连 `/run/immurok/pam.sock` 发 `SUBSCRIBE:SESSION`，长连接接收事件（`UI:AUTH_DIALOG:<mode>[:cmd:timeout]`、`UI:DISMISS`、`NOTIFY:<text>`），上行发 `UI:CANCEL`、`SCREEN:LOCKED/UNLOCKED`、`SESSION:HELLO:<uid>`。daemon 侧按 SO_PEERCRED 记录订阅者 uid。
- [x] T2.2 daemon：`spawn_auth_dialog` / `spawn_agent_dialog` / `kill_auth_dialog`（`socket.rs:492-600`）改成向订阅者推事件；**无订阅者时行为等同今天「dialog 找不到」——继续走触摸门，不因缺 UI 而放行或拒绝**；收到 `UI:CANCEL` 等价于今天的非零退出（拒绝）。
- [x] T2.3 锁屏状态（F4）：`screen.rs` 改用系统总线 logind——订阅活动会话对象的 `Lock`/`Unlock` 信号 + 读 `LockedHint`；session agent 的 `SCREEN:*` 作为兜底来源（对不上报 LockedHint 的 swaylock/i3lock）。
- [x] T2.4 解锁/锁屏（F4）：`coordinator.rs:323, 374` 解析目标 session id 后调 `loginctl unlock-session <id>` / `lock-session <id>`；polkit rules 放行。
- [x] T2.5 `ssh_takeover`（F6）：`ssh_config.rs` 的 `~/.ssh/config` 读写整体搬到 session agent（daemon 只保存意图并在设置变更时推 `SSH_TAKEOVER:ON/OFF`）；socket 路径改 `/run/immurok/agent.sock`；处理旧 `~/.ssh/config` 里指向 `~/.immurok/agent.sock` 的历史块（`imk_main.rs:146` 已有类似兼容注释）。
- [x] T2.6 桌面通知走 agent（`notify-send` 或 libnotify），daemon 不再直接 spawn。
- [x] T2.7 tty spinner 降级说明（F6）—— 接受静默降级，记在 TESTING.md「仍然降级的部分」：`ssh_agent.rs:527` 起的 tty 探测在跨 uid 下会失败，接受静默降级；把「找不到 tty」从 debug 提到 info 一次性日志，避免以后当 bug 查。

验收：`imk run --agent -- sudo true` 弹窗正常、Cancel 生效、锁屏后触摸能解锁、`ssh_takeover` 开关能改到 `~/.ssh/config`。

**2026-09-04 真机**：代理订阅成功（`subscribed via /run/immurok/pam.sock`），
`systemd --user` 环境带 `WAYLAND_DISPLAY`/`DISPLAY`/会话总线，`imk run
--agent -- sudo id -un` 弹窗并经触摸通过；锁屏/解锁已确认正常。

### P3 · 授权模型（1.5 人日）

- [x] T3.1 `verify_peer_credentials`（`socket.rs:146-185`）改为按命令分级，实现设计稿 §4.3 的三档表；`AUTH:*` 仅 uid 0 / polkitd。
- [x] T3.2 owner 绑定（F5）：配对成功时写 `/var/lib/immurok/owner`；`handle_auth` 校验 `user` → uid == owner，否则 `DENY:NOT_OWNER`；日志记录。
- [x] T3.3 活动会话判定：优先 `sd_uid_get_state(uid) == "active"`（libsystemd，或直接 logind D-Bus `ListSessions` + `Active` 属性），管理类命令按此放行；实现放 `daemon/session.rs`，带单测（mock 输入）。
- [x] T3.4 marker 文件**整套删除**，改由 `AGENT_APPROVE` 在 daemon 内存里登记。原计划的「搬到 `/run/immurok/markers`（1777）」实做后才发现不成立：marker 是 `0600` 归调用者，daemon 换成 `immurok` 之后读不到；放宽到 0644 又会把 agent 执行的命令文本泄露给同机其他用户。改用 `SO_PEERCRED` 的 pid 登记后，凭据来自内核而非任何人都能创建的文件，`/run` 下也不再需要全局可写目录。（1777，daemon 只读）；`imk_main.rs:183-200` 同步；日志措辞改为 "claimed agent context"（F7）。

验收：另建一个测试用户，在其会话里跑 `sudo` 必须 `NOT_OWNER` 拒绝；`imk` 管理命令在非活动会话下被拒。

### P4 · 安装 / 迁移 / 卸载（2 人日）

**2026-09-04 已在 Arch + GNOME + polkit 127 真机跑通。** 实测记录见本节末尾。

- [x] T4.1 `Makefile install` 重写：`useradd --system --no-create-home --shell /usr/sbin/nologin -G bluetooth immurok`；二进制与 helper 装 `/usr/local/bin`；装系统单元 / tmpfiles / dbus policy / polkit rules；**删除**现有的两个 polkit override（`BindPaths=/run/user` 与 agent-helper 的 `ProtectHome=no`，`Makefile:73-79`），默认不下发任何替代 override（理由见 F1）。
- [x] T4.2 迁移：`~/.immurok/{pairing,settings,ssh_keys,key_names,keystore_digests}.json` → `/var/lib/immurok/`（`install -o immurok -g immurok -m 600`），原文件改名 `.migrated`；`systemctl --user disable --now immurok-daemon` → `systemctl enable --now immurok-daemon`。
- [x] T4.3 `scripts/immurok-pam-helper` 增加 `migrate-daemon` 子命令（走已有的 `pkexec` policy），供 TUI 一键迁移；补 `scripts/test-pam-helper.sh` 用例。
- [x] T4.4 `make uninstall` 反向：删单元/rules/policy/tmpfiles、`userdel immurok`、`/var/lib/immurok` 询问后删；**卸载后必须验证 polkit 仍能重启**。
- [x] T4.5 回滚路径文档化（`TESTING.md`）：`make uninstall` + 旧 tag 的 `make install`，`.migrated` 文件改回。

### P5 · CLI / TUI（1.5 人日）

- [x] T5.1 `socket_client.rs:19-25`、`imk_main.rs:245-265, 336-351` 路径改 `/run/immurok/*`，保留 `IMMUROK_SOCKET` 覆盖。
- [x] T5.2 `commands/keys.rs:24-60, 240-250` **与 `tui/app.rs:633-657`** 改走 socket（`LIST` / `GET:<cat>`），不再直读文件（F6）。
- [x] T5.3 日志入口：**改为由 daemon 通过 socket 推**，而不是原计划的「改读 `/var/log/immurok/daemon.log`，无权限时回退 journalctl」。理由：日志文件是 `immurok:immurok 0640`，CLI 读不到；改成世界可读会在多用户机器上公开每一次认证事件；而 journalctl 读系统单元又要求用户在 wheel/adm/systemd-journal 组里，不能假定。daemon 自持 500 行环形缓冲 + 实时广播，`SUBSCRIBE:LOG` 走与其他管理命令同一套活动会话校验。
- [x] T5.4 Pam tab 状态行（**一键迁移未做**：迁移入口就是 `make install`，在 TUI 里再包一层 pkexec 是多余的风险面；状态行直接写明该跑什么）：`GET:INFO` 附带 daemon uid 与 socket 路径；未隔离时置顶不可隐藏（英文文案，Linux 端不做本地化）。
- [x] T5.5 `immurok-cli settings` 增加 `isolation: ON/OFF`。

### 真机验收记录（2026-09-04，Arch / GNOME / polkit 127 / FW 1.7.9.334a）

迁移：`pairing/settings/ssh_keys/key_names/keystore_digests` 五个文件搬入
`/var/lib/immurok`（0600 immurok），原件留 `.migrated`；`owner=1000` 落盘；
旧 polkit override 已删；旧用户级单元与 `~/.local/bin` 二进制已清。

隔离断言（以普通用户身份，全部按预期失败）：

```
rm /run/immurok/pam.sock                → Permission denied
kill <daemon pid>                       → Operation not permitted
cat /var/lib/immurok/pairing.json       → Permission denied
touch /run/immurok/evil.sock            → Permission denied
echo x >> /usr/local/bin/immurok-daemon → permission denied
```

认证三条路径：

- `imk run --agent -- sudo id -un` → `AUTH approved via pre-auth` → `root`
- `sudo -k` 后纯 `sudo` → `AUTH approved via BLE: katsu`（真实触摸）
- `pkexec id -un` → `root`（验证 AUTH 分级对 polkitd 的放行）

其余：`immurok-cli status` / `imk` 正常；`ssh-add -l` 经 `/run/immurok/agent.sock`
列出 key；屏幕监视器 `Screen lock monitor started (logind LockedHint)`，本次
启动以来 0 次失败（旧版是每 5 秒一条 EACCES）。

### P6 · 测试与攻击复现（1.5 人日）

- [x] T6.1 攻击脚本 `scripts/test-isolation.sh`（6 条断言，真机全过；用 root 跑会拒绝执行）（普通用户身份运行，全部必须失败）：`rm /run/immurok/pam.sock` → EACCES；`kill <daemon pid>` → EPERM；`cat /var/lib/immurok/pairing.json` → EACCES；`python3` 起假服务端抢 bind → EACCES；`sudo -S true </dev/null` 在假服务端存在时不得通过。
- [x] T6.2 Rust 单测：`paths` 回退、`session.rs` 活动会话判定、`socket_proto` 的 owner 校验分支。
- [x] T6.3 PAM C 单测：新增 `pam/test_socket_dir_check.c`，`mkdtemp` 造「目录归普通用户」「目录 root 0755」「目录 immurok 0755」「目录 0777」四种，只有中间两种放行（F2）。
- [x] T6.4 真机矩阵（用 `scripts/test-install.sh` 一键跑）：
  - **Arch + GNOME + polkit 127**：全套通过（2026-09-04，开发机，逐项见上）
  - **Fedora 44 + GNOME/Wayland + polkit 127 + SELinux Enforcing**：**20/20，0 跳过**
    （2026-09-04）。这条清掉了计划里唯一没排除的假设 —— `policykit_auth_t`
    能连 `/run/immurok` 下的 socket，`pkexec` 在 enforcing 下直接通过，**不需要
    任何 `.te` 策略模块**。
  - **Ubuntu 24.04（本地登录，未接设备）**：**15 PASS / 0 FAIL / 2 SKIP**
    （2026-09-05）。三处 Debian 系差异全过：PAM 目录 `/lib/x86_64-linux-gnu/security`、
    `polkit-1` 模板用 `common-auth`、旧版 polkit 的 setuid helper 路径；「重启 polkit
    后仍 active」同样通过。两个 SKIP 是设备相关（那台没接设备），设备交互已在
    Arch 与 Fedora 各验过一遍，与发行版无关。

    第一轮跑出过 1 个 FAIL（`CLI 报告 isolated`），查下来是脚本与 CLI 各自的问题，
    不是加固本身：`settings` 被配对闸门挡住（已把只读的 settings 放进白名单），
    以及那条断言用 `grep -q 'isolated'` 而未隔离时输出是 `NOT isolated`，同样匹配
    —— 一条不可能失败的断言。两处都已修。**这恰恰是这个脚本存在的意义：在一台
    全新未配对的机器上，它抓到了只在那种状态下才会暴露的问题。**

### P7 · 可选：方案 B（nonce + HMAC，1.5 人日）

仅在 A 全绿之后做；实现按设计稿 §5，复用 macOS 的 `hmac_sha256.c` 与测试向量。

---

## 5. 风险登记

| 风险 | 影响 | 处置 |
|---|---|---|
| polkit override 写坏 | 整机图形授权不可用 | 默认不下发 override（F1）；T0.1 前置验证 + 卸载后重启 polkit 的验收项 |
| Fedora SELinux 拦 `policykit_auth_t` → `var_run_t` socket connect | polkit 路径失效（sudo 仍可用） | T0.2 先验；必要时提供 `.te` 模块或退回只保 sudo |
| 迁移中断（拷了一半） | 设备"失联"，用户以为坏了 | 迁移做成幂等；`.migrated` 保留原文件；TUI 状态行显式提示未隔离 |
| 多用户机器上非活动会话不可用 | 设计如此 | 文档写清；`NOT_OWNER` 拒绝要有可读日志 |
| session agent 没起（纯 TTY 登录 / 精简 WM） | 无弹窗、无通知 | 保持「无 UI 也能触摸授权」，不得因缺 UI 放行或拒绝 |
| 非 systemd 发行版 | 不支持 | 现状已不支持，不算回归；`paths` 的固定路径回退保证可手动跑 |

## 6. 残余风险（做完 A 之后仍然存在）

- root 被攻破：无边界。
- `immurok` 用户被攻破：拿到 `shared_key`，可冒充主机；与今天持平。
- **触摸劫持**：同 uid 攻击者先发起 `sudo`，用户为自己的操作触摸 → 授权落到攻击者的请求上。`try_set_pending_pam` 的互斥（`socket.rs:406-411`）只保证不串台，不解决先来先得。无显示屏令牌下无解，需在文档里明确。
- 会话 UI 不可信：session agent 跑在用户会话里，同 uid 攻击者可以杀掉或伪造窗口。因此弹窗只能用来**拒绝**，永远不能作为放行依据——P2 的实现必须守住这条。

---

## 7. 升级兼容性：老用户单独升固件 / 单独升 App 会怎样

**底线要求：最坏只能退到"输入密码"，不允许出现无法鉴权后卡死、设备解绑、整机授权服务起不来。**

### 7.1 现有代码提供的四道保底（已逐条核实）

| 保底 | 证据 | 效果 |
|---|---|---|
| PAM 行是 `auth sufficient` | `scripts/immurok-pam-helper:11` | 模块任何失败（连不上/拒绝/解析错/`.so` 缺失 dlopen 失败）都只是这一行不生效，继续走 `pam_unix` 密码 |
| PAM 等待有硬性 wall-clock 上限 + 三种即时逃逸 | `pam/pam_immurok.c:168-232` | 从进入循环起算 `timeout_sec`（sudo 默认 40s、polkit-1 模板 `timeout=10`）；80ms 轮询；**任意按键 / Ctrl+C / 对端关闭连接**都立刻退出到密码 |
| 登录界面根本不进这条路 | `pam/fp_policy.h:31-34`，`pam_immurok.c:249-256` | `gdm-password` 在碰 socket 之前就 `PAM_IGNORE`，图形登录不可能被拖住 |
| 固件对未知命令一定回包 | `firmware/APP/hidkbd.c:6454-6458`（`default: rspBuf[0] = IMMUROK_RSP_UNKNOWN_CMD`） | 新 App 向老固件发新命令 → 立刻拿到 UNKNOWN_CMD，不会因"设备不吭声"而空等 5s BLE 超时 |

另外 daemon 侧 BLE 认证超时 30s（`protocol.rs:171`）短于 PAM 的 40s，所以"设备连着但没人碰"这条路径也是 daemon 先回 DENY，不会由 PAM 硬超时兜底。

### 7.2 唯一一处无上限等待（建议顺手修掉）

`pam_immurok.c:112-127`：socket 是阻塞模式，只设了 `SO_SNDTIMEO`，而 **`SO_SNDTIMEO` 不覆盖 `connect()`**。AF_UNIX 的 `connect()` 在"对端存在但 accept 队列打满"时会一直阻塞，此时按键/Ctrl+C 的逃逸逻辑还没开始跑。

现实触发条件苛刻（要有 128 个堆积连接、daemon 活着却不 accept），但它是全流程里唯一没有上限的等待。

- [x] T1.6b PAM 侧 `connect()` 改 `O_NONBLOCK` + `select` 限时 2s，超时即 `PAM_AUTH_ERR`。**这条与路线选择无关，建议独立先合。**

### 7.3 混版矩阵 · 路线 A（daemon 特权分离，不动固件）

| 组合 | 结果 | 是否可接受 |
|---|---|---|
| 单独升固件 | **零影响**。A 不改任何 BLE 命令与 EEPROM 结构 | ✅ |
| 单独升 App（新 `.so` + 新 daemon 一起装） | 正常 | ✅ |
| 新 `.so` + 老用户级 daemon（迁移中断） | `/run/immurok/pam.sock` 不存在 → `connect` 立刻 ENOENT → 立即回退密码，无等待 | ✅ |
| 老 `.so` + 新系统 daemon | 老 `.so` 连 `/run/user/<uid>/immurok/pam.sock`，同样立刻 ENOENT → 回退密码 | ✅ |
| 新系统 daemon 与老用户 daemon 同时在跑 | 两个进程抢同一条 GATT 通道，认证时灵时不灵；不会卡死（走 30s daemon 超时或按键逃逸），但体验很差 | ⚠️ 安装脚本必须先 `systemctl --user disable --now`；多用户机器上其他用户的旧单元不受影响，需在迁移提示里写明 |
| ~~polkit override 指向不存在的 `/run/immurok`~~ | 只有在照设计稿写 `ReadWritePaths=/run/immurok` 时才会发生（`226/NAMESPACE` → 整机图形授权全废）。按 F1 的修正**根本不下发 override**，这个故障点直接消失 | ✅ 已消除；若将来确需 override，必须带 `-` 前缀 |

**结论：按 F1 修正后（默认不下发 override），路线 A 不再有任何能造成「系统级故障」的点，全部混版组合都落在「回退密码」。**

### 7.4 混版矩阵 · 路线 C（设备签名）与方案 B（socket HMAC）

这两条都改协议，风险点从 PAM 转移到固件与下发：

| 组合 | 结果 | 处置 |
|---|---|---|
| 新 PAM 发四段 `AUTH:user:service:nonce` 给老 daemon | 老 daemon 的 `parse_request` 用 `splitn(10, ':')` 且只取 `parts[1..2]`（`socket_proto.rs:213-221`），**多余字段被静默忽略**，照常发起认证并回 `OK` → 新 PAM 因为没有 MAC 而拒绝 | 不卡死，但用户会经历"绿灯亮了、我也摸了、最后还是要密码"。**PAM 必须按策略密钥文件是否存在自判模式**：文件不在 = legacy 只认 `OK`；文件在 = 严格模式。这样"单独升 `.so`"不产生莫名拒绝 |
| 新 App/PAM + 老固件（不会签名） | 老固件回 `RSP_UNKNOWN_CMD`，立即失败 → 回退密码 | ✅ 前提是**新增新 opcode，绝不改 `0x33 AUTH_REQUEST` 的语义**，否则老 App 也会一起废掉 |
| 老 App + 新固件 | 新固件必须继续无条件响应老的 `0x33` | ✅ 同上，写进固件验收项 |
| **新固件改了 EEPROM 结构或 bump 了 magic** | `load_security_data` 只认 `STORAGE_MAGIC_V3` 且校验覆盖整个结构（`immurok_security.c:538-548`），任何布局变化或 magic 变更 → 返回 -1 → **设备直接当作未配对**；而且 keystore 与 security data 共用 Block 0（`immurok_security.c:584-597` 的读-改-写），一并受影响 | ❌ **这是路线 C 的头号风险，且现有设计稿完全没提。** 老用户单独 OTA 固件 = 解绑 + 需重新配对，远超"回退密码"的底线 |

好消息是结构里预留了空间：`storage_v3_t` 有 `padding[68]`（`immurok_security.c:57-64`）。

- [ ] 固件侧硬约束（若走 C）：新字段（如 `pam_verify_key[32]`）**只能从 `padding[68]` 里切**，`magic` 保持 `IMR3`、`sizeof` 保持 112B。老数据里这段是 0，读出来就是"未下发"，checksum 因字节未变而依然通过，老配对无损。
- [ ] 发版前回归：拿一台已配对的老设备（或老 EEPROM 镜像）刷新固件，验证 `paired=1` 与 keystore 条目全部存活。

### 7.5 无论走哪条路线都要写进验收单的项

- [ ] 断开设备 / 停掉 daemon / 删掉 `.so` 三种情况下，`sudo` 与 `pkexec true` 都在 1s 内落到密码提示。
- [ ] 图形 polkit 对话框（无 tty，逃不掉按键）最坏停顿 = `timeout=10`，实测确认。
- [ ] `gdm-password` 全程不受影响（改任何东西都要重跑这条）。
- [ ] 卸载 / daemon 停止后 `systemctl restart polkit` 成功。
- [ ] 老设备刷新固件后仍处于已配对状态、keystore 条目完整。
