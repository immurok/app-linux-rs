# Linux 发行版打包与 GitHub 自动发布 — 设计稿

日期：2026-09-21
状态：设计已确认，待写实施计划

## 1. 目标与范围

把 app-linux-rs 从"只能 `make install`"变成"下载一个包就能装"，并由 GitHub Actions 自动出包：

- 支持 **Debian 12+ / Ubuntu 24.04+ / Fedora 43+ / Arch**，架构 **amd64 + arm64**。
- 产物只挂 **GitHub Release**（`.deb` / `.rpm` / `.pkg.tar.zst` × 2 架构 + `SHA256SUMS`）。不做 apt/dnf 仓库、AUR、COPR、PPA，不做 GPG 签名。
- 装完即用：daemon 自动启动、session-agent 对所有用户生效、GUI 自启动登记。用户需要做的只有配对（GUI 或 `immurok-cli pair`）和在 PAM 页启用钩子。
- **保守**：包不自动往 `/etc/pam.d/*` 插 `pam_immurok.so`，钩子由用户在 GUI/TUI 的 PAM 页 Install（`pkexec immurok-pam-helper add`，现有流程）。
- `make install`（源码安装到 `/usr/local`）保留，两条路线互斥且互相检测。

不在范围：Ubuntu 22.04（无 `python3-dbus-fast` 原生包，仍可 `make install` + pip）、GUI 拆包（授权弹窗 `immurok-auth-dialog` 本身就依赖 PyGObject + Gtk4 + Adw，拆了也省不掉 GTK）、legacy 用户级 daemon（`~/.immurok`）迁移（包没有"当前用户"上下文，由 GUI/TUI 的 Repair 走 `migrate-daemon <user>` 处理）。

## 2. 方案：一套二进制 + nfpm 出三种包

每个架构只在 **`debian:12` 容器**里构建一次：glibc 2.36 是四家目标里最老的，产物在 Fedora 43 / Arch 上都能跑；直接依赖的 soname（`libpam.so.0`、`libdbus-1.so.3`、`libsystemd.so.0`、`libgtk-4.so.1`、`libadwaita-1.so.0`）四家一致。GUI 用到的 libadwaita 组件最新到 `MessageDialog`（1.2），Debian 12 的 1.2.2 够用。

一份 `packaging/nfpm.yaml` 描述文件清单、依赖、maintainer 脚本，用 `overrides:` 按 `deb` / `rpm` / `archlinux` 分别写依赖包名，PAM 模块目录用 `contents` 条目的 `packager` 字段区分。除 PAM `.so` 外，`contents` 只引用 `packaging/stage.sh` 装配出的目录树。

放弃的备选：各家原生工具（cargo-deb + cargo-generate-rpm + makepkg）——三份清单要手工同步，且都按单 crate 思维设计，对 6 crate workspace + C 编译的 `.so` + python 脚本要硬拼 `assets`；额外维护 PKGBUILD 给未来 AUR——AUR 不做，YAGNI。

## 3. 文件布局与路径统一

### 3.1 包内路径（prefix `/usr`）

| 路径 | 内容 |
|---|---|
| `/usr/bin/` | `immurok-daemon` `immurok-cli` `imk` `immurok-session-agent` `immurok-gui` `immurok-auth-dialog` `immurok-pam-helper` `ble-notify-helper.py` |
| `/usr/lib/systemd/system/immurok-daemon.service` | 系统单元 |
| `/usr/lib/systemd/user/immurok-session-agent.service` | 用户单元 |
| `/usr/lib/sysusers.d/immurok.conf` | **新增** `u immurok - "immurok fingerprint daemon"`，替代 `useradd` |
| `/usr/lib/tmpfiles.d/immurok.conf` | 现有 |
| `/usr/share/dbus-1/system.d/immurok.conf` | BlueZ 授权（`make install` 装在 `/etc/dbus-1/system.d`，包用标准位置） |
| `/usr/share/dbus-1/services/com.immurok.Settings.service` | GUI D-Bus 激活 |
| `/usr/share/polkit-1/rules.d/49-immurok.rules` | 现有 |
| `/usr/share/polkit-1/actions/com.immurok.pam-helper.policy` | 由 `scripts/com.immurok.pam-helper.policy.in` 生成，`@HELPER_PATH@` = `/usr/bin/immurok-pam-helper` |
| `/usr/share/applications/com.immurok.Settings.desktop` | 现有 |
| `/etc/xdg/autostart/com.immurok.Settings.desktop` | 现有 `com.immurok.Settings.autostart.desktop`，改为系统级（对所有用户生效，不再写 `~/.config/autostart`）；标为 config 文件 |
| PAM 模块 | deb `/usr/lib/{x86_64,aarch64}-linux-gnu/security/pam_immurok.so`；rpm `/usr/lib64/security/`；arch `/usr/lib/security/` |
| `/usr/share/doc/immurok/` | README.md、CHANGELOG.md；LICENSE 按各格式惯例 |

辅助脚本的查找**不用改代码**：daemon 找 `ble-notify-helper.py`（`ble.rs::find_helper_script`）、session-agent 找 `immurok-auth-dialog`（`main.rs::dialog_path`）、client 找 `immurok-pam-helper`（`pam.rs`）都是"先看自己可执行文件旁边"，`/usr/bin` 天然满足。`.desktop` 的 `Exec=immurok-gui` 走 PATH。

### 3.2 路径规范化

仓库里写死 `/usr/local/bin` 的只有三个文件：`packaging/immurok-daemon.service`、`packaging/immurok-session-agent.service`、`packaging/com.immurok.Settings.service`。改为以 **`/usr/bin` 为规范值**（包是主渠道）；`scripts/install-root.sh` 装到 `/usr/local` 时对这三个文件 `sed "s|/usr/bin/|$BIN_DIR/|"`（它已经对 D-Bus service 这么做，扩到三个）。

### 3.3 装配脚本 `packaging/stage.sh`

```
packaging/stage.sh <destdir> <pam-moddir>
```

从 `target/release/`、`scripts/`、`packaging/` 拼出 3.1 的目录树：复制二进制与脚本、`.policy.in` 替换、`.autostart.desktop` 改名放到 `etc/xdg/autostart/`。PAM `.so` 不进目录树，nfpm.yaml 里直接引用 `pam/pam_immurok.so` 写三条 `contents`，各带 `packager: deb|rpm|archlinux` 指到对应目录（nfpm 的 `Content.packager` 字段，已在源码确认）。三种格式共用同一次装配。

`make install` 路径**不动**（`install-root.sh` 逻辑保留，只加 sed 与 5.1 的检测）。两份文件清单都很短，各自由 `scripts/test-install.sh` 与 CI `verify` 兜底。

## 4. 包元数据、依赖、maintainer 脚本

### 4.1 元数据

- 包名 `immurok`；版本取 workspace crate 版本（CI 校验 tag `vX.Y.Z` == `crates/immurok-daemon/Cargo.toml` 的 `version`）；release `1`；license Apache-2.0；homepage `https://immurok.com`；maintainer immurok。
- 产物命名走各格式惯例：`immurok_0.9.0-1_amd64.deb` / `immurok_0.9.0-1_arm64.deb`、`immurok-0.9.0-1.x86_64.rpm` / `immurok-0.9.0-1.aarch64.rpm`、`immurok-0.9.0-1-x86_64.pkg.tar.zst` / `immurok-0.9.0-1-aarch64.pkg.tar.zst`，加 `SHA256SUMS`。
- 不签名：`apt install ./x.deb`、`dnf install ./x.rpm`、`pacman -U` 对本地未签名包都放行。

### 4.2 依赖

nfpm 不自动扫 shlib，按 `ldd` 实测手写。直接依赖：daemon → libdbus-1、libsystemd；GUI → gtk4、libadwaita；PAM `.so` → libpam；两个 python 脚本 → dbus-fast、PyGObject + Gtk4 + Adw。

| | deb | rpm | arch |
|---|---|---|---|
| BLE helper | `python3`, `python3-dbus-fast` | `python3`, `python3-dbus-fast` | `python`, `python-dbus-fast` |
| 授权弹窗 | `python3-gi`, `gir1.2-gtk-4.0`, `gir1.2-adw-1` | `python3-gobject`, `gtk4`, `libadwaita` | `python-gobject`, `gtk4`, `libadwaita` |
| 动态库 | `libgtk-4-1`, `libadwaita-1-0`, `libdbus-1-3`, `libsystemd0`, `libpam0g` | `dbus-libs`, `systemd-libs`, `pam` | `dbus`, `systemd-libs`, `pam` |
| 系统服务 | `bluez`, `polkitd \| policykit-1`, `pkexec`, `libnotify-bin`, `systemd`, `dbus` | `bluez`, `polkit`, `libnotify`, `systemd` | `bluez`, `polkit`, `libnotify` |

包名写错由 CI `verify` job 在真实发行版容器里安装时抓出来。

### 4.3 maintainer 脚本

`packaging/scripts/{preinstall,postinstall,preremove,postremove}.sh`，三格式共用一份，公共函数在 `packaging/scripts/lib.sh`：

- **参数归一**：deb `$1`=`install`/`upgrade`/`configure <old>`/`remove`/`purge`、rpm `$1`=`1`/`2`/`0`、arch `post_install <new>` / `post_upgrade <new> <old>` / `pre_remove <ver>`（版本串），统一成 `fresh` / `upgrade` / `remove` / `purge` 四种；判别顺序：deb 动词 → 纯数字（rpm）→ 其余按 arch，第二个参数存在即 upgrade。
- **Arch 的升级钩子要显式配**：nfpm 只把 `postinstall` 映射成 `post_install`，`post_upgrade` / `pre_upgrade` 来自 `archlinux.scripts.postupgrade|preupgrade`（源码确认）。nfpm.yaml 里 `postupgrade` 指向同一个 `postinstall.sh`，否则 Arch 升级后 daemon 不会重启。`preupgrade` 不配（legacy 检测只在全新安装有意义）。
- `have_systemd`：`/run/systemd/system` 不存在（chroot、容器）时跳过所有 `systemctl`。
- **postinstall 永不失败**：sysusers / tmpfiles / enable / restart / 起 session-agent 任何一步失败只打 warning 并提示 `systemctl status immurok-daemon`，不让包进入 deb 的"半配置"状态。`preinstall` 是唯一故意失败的地方（5.1）。

**preinstall**：5.1 的 legacy 检测。

**postinstall**（fresh 与 upgrade 同一路径，幂等）：
1. `systemd-sysusers /usr/lib/sysusers.d/immurok.conf`；`getent group bluetooth` 存在则 `usermod -aG bluetooth immurok`（与 `immurok-pam-helper ensure_daemon_user` 一致）。
2. `chown -R immurok:immurok /var/lib/immurok /var/log/immurok`（存在时）——修 5.2 的 uid 漂移；目录 0700，只有 daemon 用，安全。
3. `systemd-tmpfiles --create /usr/lib/tmpfiles.d/immurok.conf`。
4. `systemctl daemon-reload`；`systemctl reload dbus` 或 `dbus-broker`（BlueZ policy 生效，不重启 bluetoothd）。
5. `systemctl enable immurok-daemon` + **`restart`**（升级换掉旧进程；全新安装等于 start。同 `immurok-pam-helper start_system_daemon` 的理由）。
6. `systemctl --global enable immurok-session-agent.service`（对所有用户生效）。
7. 对当前已登录用户（`loginctl list-users`）best-effort `systemctl --user -M <user>@ start immurok-session-agent`，装完立刻有授权弹窗，不用重新登录。
8. 打印一行提示：打开 immurok（或 `immurok-cli pair`）配对；PAM 钩子在 PAM 页启用。

**preremove**（仅 remove / purge，升级跳过）：
1. `immurok-pam-helper remove sudo polkit-1 gdm-password`——趁 helper 与 `.so` 还在时把 `/etc/pam.d` 清干净，否则模块没了 PAM 会记错误日志。
2. `systemctl disable --now immurok-daemon`。
3. `systemctl --global disable immurok-session-agent`；对已登录用户 best-effort `--user stop`。

**postremove**：`daemon-reload`、`reload dbus`；deb `purge` 时删 `/var/lib/immurok` 与 `/var/log/immurok`。rpm / arch 没有 purge，状态保留，README 写 `sudo immurok-pam-helper purge-daemon --purge-state`（需在卸包前跑）或手动 `rm -rf`。`immurok` 系统用户按 Debian policy 不删。

## 5. 与 `make install`（/usr/local）的共存

### 5.1 冲突与检测

同机两种装法会互相踩：`/usr/local/bin` 在 PATH 前面（跑到旧 CLI/GUI）；`/etc/systemd/system/immurok-daemon.service` 遮住 `/usr/lib/systemd/system/` 的（跑旧 daemon）；`/usr/local/share` 的 desktop / D-Bus service 优先级更高；PAM `.so` 与 polkit `.policy` 路径**完全相同**——deb/rpm 静默覆盖，pacman 直接拒装（"exists in filesystem"）。

**决定：不自动清理，检测到就中止并给出明确指令。**

- `preinstall`：发现 `/usr/local/bin/immurok-daemon` 或 `/etc/systemd/system/immurok-daemon.service` → 打印"检测到源码安装（/usr/local），请先在原 checkout 里 `make uninstall`（配对数据 /var/lib/immurok 保留），再重新安装本包" → `exit 1`。deb/rpm 干净中止；Arch 走不到 preinstall（pacman 先查文件冲突），错误本身够明确，README 补同一句。
- 反方向：`install-root.sh` 发现 `/usr/bin/immurok-daemon` 属于某个包（`dpkg -S` / `rpm -qf` / `pacman -Qo` 任一命中）就中止，提示先卸包。

### 5.2 状态跨渠道存活

`make uninstall --keep-state`（`immurok-pam-helper purge-daemon --keep-state`）会 `userdel immurok` 但保留 `/var/lib/immurok`；之后包用 sysusers 重建的 `immurok` uid 可能不同，daemon 读不到 `pairing.json`。postinstall 第 2 步的 `chown -R` 修这个。配对与 `owner` 因此都能从源码安装迁到包；PAM 钩子被 `make uninstall` 摘掉了，装包后在 PAM 页重新 Install（postinstall 提示里写明）。

### 5.3 文档

README 补"安装包 vs 从源码安装"两条路线与互斥说明；Arch 的 `python-dbus-fast` 已在 `extra`（README 说 AUR 的过时）；卸载章节补 rpm/arch 的状态清理命令。

## 6. CI 流水线

`app-linux-rs/.github/workflows/release.yml`（随 `scripts/sync-github.sh` rsync 到 `github.com/immurok/app-linux-rs`，仓库 public，`ubuntu-24.04-arm` runner 免费）。与 app-macos 同一套触发约定。

- **触发**：push tag `v*` → build + verify + release；`workflow_dispatch` → 只 build + verify，产物作 workflow artifact（dry run）。不在 push main 时跑。
- **前置检查**：tag 的 `X.Y.Z` == `crates/immurok-daemon/Cargo.toml` 的 version，且 `CHANGELOG.md` 存在 `## X.Y.Z` 段；否则失败。打 tag 前必须把 `## Unreleased` 改成版本号。

**job `build`**（矩阵）：

| arch | runner | container |
|---|---|---|
| amd64 | `ubuntu-24.04` | `debian:12` |
| arm64 | `ubuntu-24.04-arm` | `debian:12` |

步骤：apt 装构建依赖（`gcc pkg-config libdbus-1-dev libpam0g-dev libgtk-4-dev libadwaita-1-dev curl git`）→ rustup stable → cargo registry / target 缓存（按 arch 分 key）→ `cargo build --release --workspace --locked` → `cargo test --release --workspace --locked` + `make -C pam && make -C pam test` → `packaging/stage.sh` → 下载固定版本 nfpm → `nfpm package -p deb|rpm|archlinux` → 上传 3 个包为 artifact。

**job `verify`**（矩阵 `debian:12` / `ubuntu:24.04` / `fedora:43` / `archlinux:latest` × 2 arch）：下载对应包 → `apt install ./x.deb` / `dnf install ./x.rpm` / `pacman -U`（真实依赖解析）→ 断言关键文件存在、PAM `.so` 落在该发行版正确目录 → `immurok-cli --version`、`python3 -c 'import dbus_fast, gi'`、`immurok-pam-helper` 可执行 → `systemd-analyze verify` 两个 unit → 卸载一次，确认 preremove / postremove 在无 systemd 环境不报错。

**job `release`**（仅 tag，需 build + verify 全绿）：汇总 6 个包 + `SHA256SUMS` → `softprops/action-gh-release` 建 Release，正文取 CHANGELOG 对应段。只需 `GITHUB_TOKEN`。

## 7. 测试与验收

- **脚本层**：`scripts/test-pkg-scripts.sh` 对 `lib.sh` 的参数归一、`have_systemd`、legacy 检测做表驱动断言（deb `configure ""` / `configure 0.8.0` / `remove` / `purge`、rpm `1/2/0`、arch），风格同 `test-pam-helper.sh`；`stage.sh` 在本地 `target/release` 上跑一遍断言目录树与 `.policy` 替换。
- **本地复现 CI**：`make package`——本机有 nfpm 时把三种包出到 `dist/`，没有就提示安装方式。
- **集成层**：CI `verify` job（第 6 节）。
- **真机验收**（手动，写进 `TESTING.md`）：Arch 开发机 `make uninstall` → `pacman -U` 本机 `make package` 产物 → daemon 起来、配对数据存活（chown 生效）→ GUI 打开、PAM 页 Install → 指纹 sudo → `pacman -R` → `/etc/pam.d` 已清、`/var/lib/immurok` 保留。Debian / Fedora 的 systemd 路径（sysusers 建户、`--global enable`、已登录用户 session-agent 即时启动）在带 systemd 的容器（`systemd-nspawn` 或 `docker --privileged` + systemd 镜像）一次性手动过，不进 CI。

## 8. 涉及文件

新增：`packaging/nfpm.yaml`、`packaging/stage.sh`、`packaging/scripts/{lib,preinstall,postinstall,preremove,postremove}.sh`、`packaging/sysusers.d/immurok.conf`、`.github/workflows/release.yml`、`scripts/test-pkg-scripts.sh`。

修改：`packaging/immurok-daemon.service`、`packaging/immurok-session-agent.service`、`packaging/com.immurok.Settings.service`（`/usr/local/bin` → `/usr/bin`）；`scripts/install-root.sh`（sed 三个文件、包冲突检测）；`Makefile`（`make package`）；`README.md`、`TESTING.md`、`CHANGELOG.md`。

不改任何 Rust 源码。
