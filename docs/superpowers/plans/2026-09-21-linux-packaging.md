# Linux 发行版打包 + GitHub 自动发布 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 app-linux-rs 能出 `.deb` / `.rpm` / Arch `.pkg.tar.zst`（amd64 + arm64），由 GitHub Actions 在打 tag 时自动构建、在四个发行版容器里验证并挂到 Release；装完即用，只差配对和 PAM 页 Install。

**Architecture:** 每个架构只在 `debian:12` 容器里 `cargo build` 一次；`packaging/stage.sh` 把产物装配成 `/usr` 前缀的目录树并把 `lib.sh` 与四个 maintainer 脚本正文拼成独立文件；一份 `packaging/nfpm.yaml` 用 `overrides` 写三家依赖名、用 `contents[].packager` 区分 PAM 模块目录，nfpm 出三种包。CI 的 `verify` job 与本地 `make verify-pkg` 共用 `packaging/verify.sh` 在真实发行版容器里装/验/卸。`make install`（/usr/local）保留，与包互斥检测。

**Tech Stack:** bash / POSIX sh、nfpm 2.47.0、GitHub Actions（`ubuntu-24.04` + `ubuntu-24.04-arm` runner、`container:`）、docker（本地验证）、systemd sysusers/tmpfiles。

**Spec:** `docs/superpowers/specs/2026-09-21-linux-packaging-design.md`

## Global Constraints

- 目标发行版 **Debian 12+ / Ubuntu 24.04+ / Fedora 43+ / Arch**；架构 **amd64 + arm64**；不出 Ubuntu 22.04 包。
- 产物只挂 GitHub Release；不做仓库、AUR、GPG 签名。
- 包**不**自动装 PAM 钩子（不调 `immurok-pam-helper add`）；卸包时**必须**先 `immurok-pam-helper remove sudo polkit-1 gdm-password`。
- 包内规范路径 prefix `/usr`；`/usr/local/bin` 不得出现在任何打包文件里。
- maintainer 脚本用 **POSIX sh**（rpm 用 `/bin/sh` 跑 scriptlet，Debian 的 `/bin/sh` 是 dash）；`postinstall` 永不非零退出；`preinstall` 只在检测到 `/usr/local` 源码安装时 `exit 1`。
- 用户可见文案（脚本输出、README）**只用英文**（app-linux-rs 约定）；代码注释按仓库现状可用中文。
- 不改任何 Rust 源码；不 bump 版本号（`version.h` / Cargo 版本何时 bump 由用户定）。
- `sudo` / `pacman -U` / `git push` 等需要提权或 ssh 签名的命令用 `imk run --agent -- …` 包装。
- 每个 Task 结束都 commit（仓库根是 monorepo `imPress-v1`，在 `app-linux-rs/` 下操作，commit 前缀 `pkg(linux):`）。

---

### Task 1: 路径规范化（`/usr/local/bin` → `/usr/bin`）与 `install-root.sh` 的互斥检测

**Files:**
- Modify: `packaging/immurok-daemon.service:12`
- Modify: `packaging/immurok-session-agent.service:16`
- Modify: `packaging/com.immurok.Settings.service:3`
- Modify: `scripts/install-root.sh`（二进制安装段之前加检测；三个 unit/service 文件改为 sed 安装）

**Interfaces:**
- Produces: 三个文件以 `/usr/bin/` 为规范路径；`install-root.sh` 装到 `$BIN_DIR` 时替换。后续 `stage.sh` 直接复制这三个文件。

- [ ] **Step 1: 改三个文件的路径**

```bash
cd app-linux-rs
sed -i 's|/usr/local/bin/immurok-daemon|/usr/bin/immurok-daemon|' packaging/immurok-daemon.service
sed -i 's|/usr/local/bin/immurok-session-agent|/usr/bin/immurok-session-agent|' packaging/immurok-session-agent.service
sed -i 's|/usr/local/bin/immurok-gui|/usr/bin/immurok-gui|' packaging/com.immurok.Settings.service
grep -n '/usr/' packaging/immurok-daemon.service packaging/immurok-session-agent.service packaging/com.immurok.Settings.service
```

Expected: 三行分别是 `ExecStart=/usr/bin/immurok-daemon`、`ExecStart=/usr/bin/immurok-session-agent`、`Exec=/usr/bin/immurok-gui --gapplication-service`；`grep -rn '/usr/local' packaging/` 无输出。

- [ ] **Step 2: `install-root.sh` 加"已装包"检测**

在 `cd "$SRC" || exit 1` 之后、`step "binaries → $BIN_DIR"` 之前插入：

```bash
# 与发行版安装包互斥：包装在 /usr，源码装在 /usr/local，同机两份会互相遮
# （PATH、unit 覆盖、PAM .so 同路径）。发现 /usr/bin 的 daemon 归某个包管就停。
owned_by_package() {
    local f="$1"
    [ -e "$f" ] || return 1
    if command -v dpkg >/dev/null 2>&1 && dpkg -S "$f" >/dev/null 2>&1; then return 0; fi
    if command -v rpm >/dev/null 2>&1 && rpm -qf "$f" >/dev/null 2>&1; then return 0; fi
    if command -v pacman >/dev/null 2>&1 && pacman -Qo "$f" >/dev/null 2>&1; then return 0; fi
    return 1
}
if owned_by_package /usr/bin/immurok-daemon; then
    echo "immurok is installed from a distribution package (/usr/bin/immurok-daemon)."
    echo "Remove that package first (apt remove immurok / dnf remove immurok / pacman -R immurok),"
    echo "then run make install again. Pairing data in /var/lib/immurok is kept."
    exit 1
fi
```

- [ ] **Step 3: `install-root.sh` 三个文件改为 sed 安装**

把现有这两行：

```bash
install -Dm644 packaging/immurok-daemon.service "$SYSTEMD_SYSTEM_DIR/immurok-daemon.service"
```
和
```bash
install -Dm644 packaging/immurok-session-agent.service /etc/systemd/user/immurok-session-agent.service
```

分别替换为：

```bash
# unit 文件以 /usr/bin 为规范路径（发行版包用），源码安装替换成 $BIN_DIR
sed "s|/usr/bin/|$BIN_DIR/|" packaging/immurok-daemon.service > "$SYSTEMD_SYSTEM_DIR/immurok-daemon.service"
chmod 644 "$SYSTEMD_SYSTEM_DIR/immurok-daemon.service"
```
和
```bash
sed "s|/usr/bin/|$BIN_DIR/|" packaging/immurok-session-agent.service > /etc/systemd/user/immurok-session-agent.service
chmod 644 /etc/systemd/user/immurok-session-agent.service
```

GUI 段里已有的 `sed "s|/usr/local/bin/immurok-gui|$BIN_DIR/immurok-gui|" packaging/com.immurok.Settings.service` 改成 `sed "s|/usr/bin/|$BIN_DIR/|" packaging/com.immurok.Settings.service`。

- [ ] **Step 4: 验证 sed 结果与语法**

```bash
bash -n scripts/install-root.sh && echo SYNTAX_OK
for f in packaging/immurok-daemon.service packaging/immurok-session-agent.service packaging/com.immurok.Settings.service; do
  sed "s|/usr/bin/|/usr/local/bin/|" "$f" | grep -E '^(ExecStart|Exec)='
done
```

Expected: `SYNTAX_OK`，三行都以 `/usr/local/bin/` 开头。

- [ ] **Step 5: Commit**

```bash
git add packaging/immurok-daemon.service packaging/immurok-session-agent.service packaging/com.immurok.Settings.service scripts/install-root.sh
git commit -m "pkg(linux): unit/service 以 /usr/bin 为规范路径；make install 与发行版包互斥检测"
```

---

### Task 2: `sysusers.d` + `packaging/stage.sh` + `scripts/test-stage.sh`

**Files:**
- Create: `packaging/sysusers.d/immurok.conf`
- Create: `packaging/stage.sh`
- Create: `scripts/test-stage.sh`
- Modify: `.gitignore`（加 `/dist/`）

**Interfaces:**
- Produces: `packaging/stage.sh <outdir>` → `<outdir>/root/…`（包内文件树，不含 PAM `.so`）和 `<outdir>/scripts/{preinstall,postinstall,preremove,postremove}.sh`（Task 4 之前这四个文件由占位正文拼出；Task 4 补真实正文）。环境变量 `TARGET_DIR` 覆盖 `target/release`。
- Task 5 的 nfpm.yaml 引用 `dist/stage/root/usr/`、`dist/stage/root/etc/xdg/autostart/com.immurok.Settings.desktop`、`dist/stage/scripts/*.sh`。

- [ ] **Step 1: 写失败测试 `scripts/test-stage.sh`**

```bash
#!/bin/bash
# packaging/stage.sh 的装配测试：用假二进制跑一遍，断言目录树、policy 替换、无 /usr/local 残留。
set -u
SRC="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fail=0
check() { if [ "$1" = "$2" ]; then echo "PASS: $3"; else echo "FAIL: $3 (got '$1' want '$2')"; fail=1; fi; }

# 假的 target/release：只要文件存在且可执行
mkdir -p "$TMP/target"
for b in immurok-daemon immurok-cli imk immurok-session-agent immurok-gui; do
    printf '#!/bin/sh\necho %s\n' "$b" > "$TMP/target/$b"; chmod 755 "$TMP/target/$b"
done

TARGET_DIR="$TMP/target" bash "$SRC/packaging/stage.sh" "$TMP/out" >/dev/null
check "$?" 0 "stage.sh exits 0"

R="$TMP/out/root"
for f in usr/bin/immurok-daemon usr/bin/immurok-cli usr/bin/imk usr/bin/immurok-session-agent usr/bin/immurok-gui \
         usr/bin/immurok-auth-dialog usr/bin/immurok-pam-helper usr/bin/ble-notify-helper.py \
         usr/lib/systemd/system/immurok-daemon.service usr/lib/systemd/user/immurok-session-agent.service \
         usr/lib/sysusers.d/immurok.conf usr/lib/tmpfiles.d/immurok.conf \
         usr/share/dbus-1/system.d/immurok.conf usr/share/dbus-1/services/com.immurok.Settings.service \
         usr/share/polkit-1/rules.d/49-immurok.rules usr/share/polkit-1/actions/com.immurok.pam-helper.policy \
         usr/share/applications/com.immurok.Settings.desktop etc/xdg/autostart/com.immurok.Settings.desktop \
         usr/share/doc/immurok/README.md usr/share/doc/immurok/CHANGELOG.md; do
    [ -f "$R/$f" ] && r=ok || r=missing
    check "$r" ok "file $f"
done

# 可执行位
[ -x "$R/usr/bin/immurok-pam-helper" ] && r=ok || r=no
check "$r" ok "immurok-pam-helper is executable"
# 非可执行文件不能带 x 位（nfpm tree 保留磁盘上的 mode）
[ -x "$R/usr/lib/systemd/system/immurok-daemon.service" ] && r=exec || r=ok
check "$r" ok "unit file is not executable"

# policy 替换
grep -q '<annotate key="org.freedesktop.policykit.exec.path">/usr/bin/immurok-pam-helper</annotate>' \
    "$R/usr/share/polkit-1/actions/com.immurok.pam-helper.policy" && r=ok || r=no
check "$r" ok "policy exec.path substituted"
grep -q '@HELPER_PATH@' "$R/usr/share/polkit-1/actions/com.immurok.pam-helper.policy" && r=leftover || r=ok
check "$r" ok "no @HELPER_PATH@ left"

# autostart 变体（--gapplication-service）落到 /etc/xdg/autostart
grep -q 'gapplication-service' "$R/etc/xdg/autostart/com.immurok.Settings.desktop" && r=ok || r=no
check "$r" ok "xdg autostart uses the service variant"

# 没有 /usr/local
n=$(grep -rl '/usr/local' "$R/usr/lib/systemd" "$R/usr/share/dbus-1" "$R/usr/share/polkit-1" 2>/dev/null | wc -l)
check "$n" 0 "no /usr/local in packaged units"

# sysusers 行
grep -q '^u immurok - ' "$R/usr/lib/sysusers.d/immurok.conf" && r=ok || r=no
check "$r" ok "sysusers creates user immurok"

# maintainer 脚本已拼装：每个都以 lib.sh 开头且以正文结尾
for s in preinstall postinstall preremove postremove; do
    [ -x "$TMP/out/scripts/$s.sh" ] && r=ok || r=missing
    check "$r" ok "scripts/$s.sh assembled + executable"
    grep -q 'pkg_phase()' "$TMP/out/scripts/$s.sh" && r=ok || r=no
    check "$r" ok "scripts/$s.sh contains lib.sh"
    sh -n "$TMP/out/scripts/$s.sh" && r=ok || r=syntax
    check "$r" ok "scripts/$s.sh is valid POSIX sh"
done

# PAM .so 不在树里（nfpm.yaml 按格式各自指定）
[ -e "$R/usr/lib/security/pam_immurok.so" ] && r=present || r=ok
check "$r" ok "PAM module is not in the tree"

exit $fail
```

- [ ] **Step 2: 运行确认失败**

Run: `bash scripts/test-stage.sh`
Expected: `stage.sh: No such file` 之类的错误，FAIL 多条，退出码 1。

- [ ] **Step 3: 写 `packaging/sysusers.d/immurok.conf`**

```
# 由 postinstall 的 systemd-sysusers 读取；替代 immurok-pam-helper 里的 useradd。
# daemon 以这个专用系统用户运行（特权分离，见 pam-channel-hardening 设计稿）。
u immurok - "immurok fingerprint daemon" - -
```

- [ ] **Step 4: 写占位的 `packaging/scripts/lib.sh` 与四个正文**（Task 3/4 会替换成真实内容；这里只让 stage.sh 有东西可拼）

```bash
mkdir -p packaging/scripts
cat > packaging/scripts/lib.sh <<'EOF'
#!/bin/sh
# placeholder — replaced in Task 3
pkg_phase() { echo fresh; }
EOF
for s in preinstall postinstall preremove postremove; do
    printf '#!/bin/sh\n# placeholder — replaced in Task 4\nexit 0\n' > packaging/scripts/$s.sh
done
```

- [ ] **Step 5: 写 `packaging/stage.sh`**

```bash
#!/bin/bash
# packaging/stage.sh — 把构建产物装配成安装包的目录树 + 拼好的 maintainer 脚本。
#
# 用法: packaging/stage.sh <outdir>
#   <outdir>/root/…      包内文件（prefix /usr）。PAM .so 不在这里——三家发行版
#                        的模块目录不同，由 nfpm.yaml 的 contents[].packager 各自指定。
#   <outdir>/scripts/…   lib.sh + 各脚本正文拼成的独立文件：包里没有 lib.sh 可 source，
#                        deb 的 preinst 甚至在解包之前就跑。
#
# 环境变量：TARGET_DIR（默认 target/release）——测试用假二进制时覆盖。
set -eu
OUT="${1:?usage: stage.sh <outdir>}"
HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="$(cd "$HERE/.." && pwd)"
TARGET_DIR="${TARGET_DIR:-$SRC/target/release}"
ROOT="$OUT/root"

rm -rf "$OUT"
mkdir -p "$ROOT" "$OUT/scripts"

# 二进制与辅助脚本 → /usr/bin（daemon / session-agent / client 都按"自己旁边"找辅助脚本）
for b in immurok-daemon immurok-cli imk immurok-session-agent immurok-gui; do
    install -Dm755 "$TARGET_DIR/$b" "$ROOT/usr/bin/$b"
done
for s in immurok-auth-dialog immurok-pam-helper ble-notify-helper.py; do
    install -Dm755 "$SRC/scripts/$s" "$ROOT/usr/bin/$s"
done

# systemd / sysusers / tmpfiles
install -Dm644 "$SRC/packaging/immurok-daemon.service"        "$ROOT/usr/lib/systemd/system/immurok-daemon.service"
install -Dm644 "$SRC/packaging/immurok-session-agent.service" "$ROOT/usr/lib/systemd/user/immurok-session-agent.service"
install -Dm644 "$SRC/packaging/sysusers.d/immurok.conf"       "$ROOT/usr/lib/sysusers.d/immurok.conf"
install -Dm644 "$SRC/packaging/tmpfiles.d/immurok.conf"       "$ROOT/usr/lib/tmpfiles.d/immurok.conf"

# D-Bus / polkit
install -Dm644 "$SRC/packaging/dbus/immurok.conf"            "$ROOT/usr/share/dbus-1/system.d/immurok.conf"
install -Dm644 "$SRC/packaging/com.immurok.Settings.service" "$ROOT/usr/share/dbus-1/services/com.immurok.Settings.service"
install -Dm644 "$SRC/packaging/polkit/49-immurok.rules"      "$ROOT/usr/share/polkit-1/rules.d/49-immurok.rules"
mkdir -p "$ROOT/usr/share/polkit-1/actions"
sed 's|@HELPER_PATH@|/usr/bin/immurok-pam-helper|' "$SRC/scripts/com.immurok.pam-helper.policy.in" \
    > "$ROOT/usr/share/polkit-1/actions/com.immurok.pam-helper.policy"
chmod 644 "$ROOT/usr/share/polkit-1/actions/com.immurok.pam-helper.policy"

# 桌面入口 + 系统级自启动（对所有用户生效，不再写 ~/.config/autostart）
install -Dm644 "$SRC/packaging/com.immurok.Settings.desktop"           "$ROOT/usr/share/applications/com.immurok.Settings.desktop"
install -Dm644 "$SRC/packaging/com.immurok.Settings.autostart.desktop" "$ROOT/etc/xdg/autostart/com.immurok.Settings.desktop"

# 文档
install -Dm644 "$SRC/README.md"    "$ROOT/usr/share/doc/immurok/README.md"
install -Dm644 "$SRC/CHANGELOG.md" "$ROOT/usr/share/doc/immurok/CHANGELOG.md"

# maintainer 脚本：lib.sh + 正文 → 独立文件
for s in preinstall postinstall preremove postremove; do
    { cat "$SRC/packaging/scripts/lib.sh"; echo; cat "$SRC/packaging/scripts/$s.sh"; } > "$OUT/scripts/$s.sh"
    chmod 755 "$OUT/scripts/$s.sh"
done

# 任何 /usr/local 残留都是错的（Task 1 把规范路径改成了 /usr/bin）
if grep -rl '/usr/local' "$ROOT/usr/lib/systemd" "$ROOT/usr/share/dbus-1" "$ROOT/usr/share/polkit-1" >/dev/null 2>&1; then
    echo "stage.sh: /usr/local leaked into packaged files" >&2
    exit 1
fi
echo "staged -> $OUT"
```

```bash
chmod 755 packaging/stage.sh
echo '/dist/' >> .gitignore
```

- [ ] **Step 6: 运行测试确认通过**

Run: `bash scripts/test-stage.sh`
Expected: 全部 PASS，退出码 0。

- [ ] **Step 7: Commit**

```bash
git add packaging/sysusers.d/immurok.conf packaging/stage.sh packaging/scripts scripts/test-stage.sh .gitignore
git commit -m "pkg(linux): stage.sh 装配包目录树 + sysusers.d；test-stage.sh"
```

---

### Task 3: `packaging/scripts/lib.sh`（阶段归一、systemd 探测、legacy 检测、best-effort 执行）+ `scripts/test-pkg-scripts.sh`

**Files:**
- Modify: `packaging/scripts/lib.sh`（替换 Task 2 的占位）
- Create: `scripts/test-pkg-scripts.sh`

**Interfaces:**
- Produces（POSIX sh 函数，Task 4 的四个脚本正文调用）：
  - `pkg_phase <script> [args…]` → 输出 `fresh|upgrade|remove|purge`；`<script>` ∈ `preinstall|postinstall|preremove|postremove`。
  - `have_systemd` → 退出码 0/1；`IMMUROK_PKG_FORCE_SYSTEMD=1|0` 覆盖。
  - `legacy_install_present` → 0 表示存在 `/usr/local` 源码安装；路径前缀 `IMMUROK_PKG_ROOT`。
  - `run cmd…` → 执行；失败只打 `immurok: warning: …` 到 stderr；`IMMUROK_PKG_DRYRUN=1` 时输出 `RUN: cmd…` 不执行。
  - `reload_dbus`、`logged_in_users`、`pkg_has_group <name>`：dryrun 下分别输出 `RUN: systemctl reload dbus`、读 `IMMUROK_PKG_FAKE_USERS`、查 `IMMUROK_PKG_FAKE_GROUPS`（空格分隔）。
  - 变量 `PKG_ROOT`（= `IMMUROK_PKG_ROOT`，默认空）。

- [ ] **Step 1: 写失败测试 `scripts/test-pkg-scripts.sh`（第一部分：lib.sh）**

```bash
#!/bin/bash
# maintainer 脚本测试：lib.sh 的阶段归一 / systemd 探测 / legacy 检测 / dryrun，
# 以及 stage.sh 拼出的四个脚本在 dryrun 下的命令序列。无需 root。
set -u
SRC="$(cd "$(dirname "$0")/.." && pwd)"
LIB="$SRC/packaging/scripts/lib.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fail=0
check() { if [ "$1" = "$2" ]; then echo "PASS: $3"; else echo "FAIL: $3 (got '$1' want '$2')"; fail=1; fi; }

# ── pkg_phase：三种包格式的参数归一 ───────────────────────────────
phase() { sh -c ". '$LIB'; pkg_phase \"\$@\"" _ "$@"; }

# deb
check "$(phase preinstall install)"          fresh   "deb preinst install"
check "$(phase preinstall install 0.8.0)"    fresh   "deb preinst install <old> (reinstall after remove)"
check "$(phase preinstall upgrade 0.8.0)"    upgrade "deb preinst upgrade"
check "$(phase postinstall configure '')"    fresh   "deb postinst configure ''"
check "$(phase postinstall configure 0.8.0)" upgrade "deb postinst configure <old>"
check "$(phase preremove remove)"            remove  "deb prerm remove"
check "$(phase preremove upgrade 0.9.0)"     upgrade "deb prerm upgrade"
check "$(phase postremove remove)"           remove  "deb postrm remove"
check "$(phase postremove purge)"            purge   "deb postrm purge"
check "$(phase postremove upgrade 0.9.0)"    upgrade "deb postrm upgrade"
# rpm：$1 = 安装后剩余实例数
check "$(phase preinstall 1)"   fresh   "rpm %pre 1"
check "$(phase postinstall 2)"  upgrade "rpm %post 2"
check "$(phase preremove 0)"    remove  "rpm %preun 0"
check "$(phase preremove 1)"    upgrade "rpm %preun 1"
check "$(phase postremove 0)"   remove  "rpm %postun 0"
check "$(phase postremove 1)"   upgrade "rpm %postun 1"
# arch：版本串
check "$(phase postinstall 0.9.0-1)"          fresh   "arch post_install new"
check "$(phase postinstall 0.9.0-1 0.8.0-1)"  upgrade "arch post_upgrade new old"
check "$(phase preremove 0.9.0-1)"            remove  "arch pre_remove"
check "$(phase postremove 0.9.0-1)"           remove  "arch post_remove"

# ── have_systemd ─────────────────────────────────────────────────
r=$(sh -c ". '$LIB'; IMMUROK_PKG_FORCE_SYSTEMD=1 have_systemd && echo yes || echo no")
check "$r" yes "have_systemd forced on"
r=$(sh -c ". '$LIB'; IMMUROK_PKG_FORCE_SYSTEMD=0 have_systemd && echo yes || echo no")
check "$r" no "have_systemd forced off"
r=$(IMMUROK_PKG_ROOT="$TMP/empty" sh -c ". '$LIB'; have_systemd && echo yes || echo no")
check "$r" no "have_systemd: no /run/systemd/system under root"
mkdir -p "$TMP/sysd/run/systemd/system"
if command -v systemctl >/dev/null 2>&1; then   # 容器里没有 systemctl，这条只在有它的主机上有意义
    r=$(IMMUROK_PKG_ROOT="$TMP/sysd" sh -c ". '$LIB'; have_systemd && echo yes || echo no")
    check "$r" yes "have_systemd: /run/systemd/system present"
fi

# ── legacy_install_present ───────────────────────────────────────
r=$(IMMUROK_PKG_ROOT="$TMP/clean" sh -c ". '$LIB'; legacy_install_present && echo yes || echo no")
check "$r" no "legacy: clean system"
mkdir -p "$TMP/leg1/usr/local/bin"; touch "$TMP/leg1/usr/local/bin/immurok-daemon"
r=$(IMMUROK_PKG_ROOT="$TMP/leg1" sh -c ". '$LIB'; legacy_install_present && echo yes || echo no")
check "$r" yes "legacy: /usr/local/bin/immurok-daemon"
mkdir -p "$TMP/leg2/etc/systemd/system"; touch "$TMP/leg2/etc/systemd/system/immurok-daemon.service"
r=$(IMMUROK_PKG_ROOT="$TMP/leg2" sh -c ". '$LIB'; legacy_install_present && echo yes || echo no")
check "$r" yes "legacy: /etc/systemd/system/immurok-daemon.service"

# ── run：dryrun 打印，真实失败只 warn 且返回 0 ────────────────────
r=$(IMMUROK_PKG_DRYRUN=1 sh -c ". '$LIB'; run systemctl restart foo")
check "$r" "RUN: systemctl restart foo" "run: dryrun prints"
r=$(sh -c ". '$LIB'; run false 2>/dev/null; echo rc=\$?")
check "$r" "rc=0" "run: failure does not propagate"
r=$(sh -c ". '$LIB'; run false 2>&1 >/dev/null")
check "$r" "immurok: warning: 'false' failed" "run: failure warns"

# ── dryrun 钩子 ──────────────────────────────────────────────────
r=$(IMMUROK_PKG_DRYRUN=1 IMMUROK_PKG_FAKE_USERS="alice bob" sh -c ". '$LIB'; logged_in_users" | tr '\n' ' ')
check "$r" "alice bob " "logged_in_users: fake list"
r=$(IMMUROK_PKG_DRYRUN=1 IMMUROK_PKG_FAKE_GROUPS="wheel bluetooth" sh -c ". '$LIB'; pkg_has_group bluetooth && echo yes || echo no")
check "$r" yes "pkg_has_group: fake hit"
r=$(IMMUROK_PKG_DRYRUN=1 IMMUROK_PKG_FAKE_GROUPS="wheel" sh -c ". '$LIB'; pkg_has_group bluetooth && echo yes || echo no")
check "$r" no "pkg_has_group: fake miss"
r=$(IMMUROK_PKG_DRYRUN=1 sh -c ". '$LIB'; reload_dbus")
check "$r" "RUN: systemctl reload dbus" "reload_dbus: dryrun"

exit $fail
```

- [ ] **Step 2: 运行确认失败**

Run: `bash scripts/test-pkg-scripts.sh`
Expected: 大量 FAIL（占位 lib.sh 只有 `pkg_phase` 恒返回 fresh），退出码 1。

- [ ] **Step 3: 写 `packaging/scripts/lib.sh`**

```sh
#!/bin/sh
# packaging/scripts/lib.sh — deb / rpm / archlinux 三种包共用的 maintainer 脚本函数。
#
# stage.sh 把它和 preinstall / postinstall / preremove / postremove 的正文拼成
# 独立文件；包里没有这个文件可 source。必须是 POSIX sh：rpm 用 /bin/sh 跑
# scriptlet，Debian 的 /bin/sh 是 dash；Arch 把内容原样塞进 .INSTALL 的函数体。
#
# 测试钩子（scripts/test-pkg-scripts.sh 用）：
#   IMMUROK_PKG_ROOT            路径前缀（legacy 检测、chown 目标、/run/systemd 探测）
#   IMMUROK_PKG_DRYRUN=1        run() 只打印 "RUN: …"，探测函数改读下面的假数据
#   IMMUROK_PKG_FORCE_SYSTEMD   1/0 覆盖 have_systemd
#   IMMUROK_PKG_FAKE_USERS      dryrun 下 logged_in_users 的返回（空格分隔）
#   IMMUROK_PKG_FAKE_GROUPS     dryrun 下 pkg_has_group 查的组列表（空格分隔）

PKG_ROOT="${IMMUROK_PKG_ROOT:-}"

# 归一化各格式传进来的参数 → fresh | upgrade | remove | purge
#   deb : preinst  install [old] | upgrade old
#         postinst configure [old]        （全新安装时 old 是空串）
#         prerm    remove | upgrade new | deconfigure …
#         postrm   remove | purge | upgrade new | …
#   rpm : $1 = 这次操作之后剩余的实例数
#         %pre/%post   1 = install, 2 = upgrade
#         %preun/%postun 0 = remove, 1 = upgrade
#   arch: post_install new | post_upgrade new old | pre_remove old | post_remove old
# 判别顺序：deb 动词 → 纯数字（rpm）→ 其余按 arch。
pkg_phase() {
    script="$1"; shift
    a1="${1:-}"; a2="${2:-}"
    case "$a1" in
        install)   echo fresh; return ;;
        configure) if [ -n "$a2" ]; then echo upgrade; else echo fresh; fi; return ;;
        upgrade|deconfigure|failed-upgrade|abort-*|disappear) echo upgrade; return ;;
        remove)    echo remove; return ;;
        purge)     echo purge; return ;;
    esac
    case "$a1" in
        ''|*[!0-9]*) ;;   # 不是纯数字 → arch
        *)
            case "$script" in
                preinstall|postinstall) if [ "$a1" -ge 2 ]; then echo upgrade; else echo fresh; fi ;;
                *)                      if [ "$a1" -ge 1 ]; then echo upgrade; else echo remove; fi ;;
            esac
            return ;;
    esac
    case "$script" in
        preinstall|postinstall) if [ -n "$a2" ]; then echo upgrade; else echo fresh; fi ;;
        *)                      echo remove ;;
    esac
}

# systemd 在跑才动 systemctl；chroot / 容器（无 /run/systemd/system）全部跳过。
have_systemd() {
    case "${IMMUROK_PKG_FORCE_SYSTEMD:-}" in
        1) return 0 ;;
        0) return 1 ;;
    esac
    [ -d "$PKG_ROOT/run/systemd/system" ] && command -v systemctl >/dev/null 2>&1
}

# best-effort 执行：失败只 warn。postinstall 永远不能非零退出，否则 deb 会把
# 包留在"半配置"状态。
run() {
    if [ "${IMMUROK_PKG_DRYRUN:-}" = "1" ]; then
        echo "RUN: $*"
        return 0
    fi
    "$@" || echo "immurok: warning: '$*' failed" >&2
    return 0
}

# BlueZ policy 是 dbus 守护进程读的，reload 即可；Arch / Fedora 是 dbus-broker，
# 它的单元别名成 dbus.service，两条都试。
reload_dbus() {
    if [ "${IMMUROK_PKG_DRYRUN:-}" = "1" ]; then
        echo "RUN: systemctl reload dbus"
        return 0
    fi
    systemctl reload dbus.service >/dev/null 2>&1 \
        || systemctl reload dbus-broker.service >/dev/null 2>&1 \
        || true
}

# 当前有会话的普通用户（uid >= 1000），用于装完立刻把 session-agent 拉起来。
logged_in_users() {
    if [ "${IMMUROK_PKG_DRYRUN:-}" = "1" ]; then
        for u in ${IMMUROK_PKG_FAKE_USERS:-}; do echo "$u"; done
        return 0
    fi
    loginctl list-users --no-legend 2>/dev/null | awk '$1 >= 1000 { print $2 }'
}

pkg_has_group() {
    if [ "${IMMUROK_PKG_DRYRUN:-}" = "1" ]; then
        for g in ${IMMUROK_PKG_FAKE_GROUPS:-}; do [ "$g" = "$1" ] && return 0; done
        return 1
    fi
    getent group "$1" >/dev/null 2>&1
}

# make install 装到 /usr/local 的痕迹。两种装法同机会互相遮（PATH、unit 覆盖、
# PAM .so 与 polkit policy 同路径），所以 preinstall 见到就中止。
legacy_install_present() {
    [ -e "$PKG_ROOT/usr/local/bin/immurok-daemon" ] \
        || [ -e "$PKG_ROOT/etc/systemd/system/immurok-daemon.service" ]
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `bash scripts/test-pkg-scripts.sh`
Expected: 全部 PASS，退出码 0。同时 `bash scripts/test-stage.sh` 仍全 PASS。

- [ ] **Step 5: Commit**

```bash
git add packaging/scripts/lib.sh scripts/test-pkg-scripts.sh
git commit -m "pkg(linux): maintainer 脚本公共库 lib.sh（阶段归一/systemd 探测/legacy 检测）+ 测试"
```

---

### Task 4: 四个 maintainer 脚本正文 + dryrun 序列测试

**Files:**
- Modify: `packaging/scripts/preinstall.sh`、`postinstall.sh`、`preremove.sh`、`postremove.sh`（替换占位）
- Modify: `scripts/test-pkg-scripts.sh`（追加第二部分）

**Interfaces:**
- Consumes: Task 3 的 lib.sh 函数；Task 2 的 `stage.sh` 拼装。
- Produces: `dist/stage/scripts/*.sh`，供 Task 5 nfpm.yaml 引用。脚本正文以 `main "$@"` 结尾（Arch 把整段塞进 `function post_install() { … }`，`$@` 就是函数参数）。

- [ ] **Step 1: 在 `scripts/test-pkg-scripts.sh` 的 `exit $fail` 之前追加序列测试**

```bash
# ── 拼装后的四个脚本：dryrun 下的命令序列 ─────────────────────────
mkdir -p "$TMP/target"
for b in immurok-daemon immurok-cli imk immurok-session-agent immurok-gui; do
    printf '#!/bin/sh\n' > "$TMP/target/$b"; chmod 755 "$TMP/target/$b"
done
TARGET_DIR="$TMP/target" bash "$SRC/packaging/stage.sh" "$TMP/stage" >/dev/null
S="$TMP/stage/scripts"

# 一个空的假根，带 systemd 与 /var/lib/immurok
FR="$TMP/fakeroot"; mkdir -p "$FR/run/systemd/system" "$FR/var/lib/immurok" "$FR/usr/bin"
printf '#!/bin/sh\necho "helper $*"\n' > "$FR/usr/bin/immurok-pam-helper"; chmod 755 "$FR/usr/bin/immurok-pam-helper"

# FORCE_SYSTEMD=1：序列断言不依赖主机有没有 systemctl（CI 在 debian:12 容器里跑这个测试）
dry() { IMMUROK_PKG_DRYRUN=1 IMMUROK_PKG_ROOT="$FR" IMMUROK_PKG_FORCE_SYSTEMD=1 IMMUROK_PKG_FAKE_USERS="alice" IMMUROK_PKG_FAKE_GROUPS="bluetooth" sh "$@" 2>&1; }

# postinstall fresh：完整序列
out=$(dry "$S/postinstall.sh" configure '')
check "$?" 0 "postinstall exits 0"
expected="RUN: systemd-sysusers immurok.conf
RUN: usermod -aG bluetooth immurok
RUN: chown -R immurok:immurok $FR/var/lib/immurok
RUN: systemd-tmpfiles --create immurok.conf
RUN: systemctl daemon-reload
RUN: systemctl reload dbus
RUN: systemctl enable immurok-daemon.service
RUN: systemctl restart immurok-daemon.service
RUN: systemctl --global enable immurok-session-agent.service
RUN: systemctl --user --machine=alice@ start immurok-session-agent.service"
check "$(echo "$out" | grep '^RUN:')" "$expected" "postinstall fresh: command sequence"
echo "$out" | grep -q 'immurok-cli pair' && r=ok || r=no
check "$r" ok "postinstall prints pairing hint"
echo "$out" | grep -qi 'PAM' && r=ok || r=no
check "$r" ok "postinstall prints PAM hint"

# postinstall 无 bluetooth 组：跳过 usermod
out=$(IMMUROK_PKG_DRYRUN=1 IMMUROK_PKG_ROOT="$FR" IMMUROK_PKG_FORCE_SYSTEMD=1 IMMUROK_PKG_FAKE_GROUPS="" sh "$S/postinstall.sh" 1 2>&1)
echo "$out" | grep -q 'usermod' && r=present || r=ok
check "$r" ok "postinstall: no usermod without bluetooth group"

# postinstall 无 systemd（容器）：没有任何 systemctl，但 sysusers/tmpfiles 照跑
NOSYS="$TMP/nosys"; mkdir -p "$NOSYS"
out=$(IMMUROK_PKG_DRYRUN=1 IMMUROK_PKG_ROOT="$NOSYS" sh "$S/postinstall.sh" 0.9.0-1 2>&1)
echo "$out" | grep -q 'systemctl' && r=present || r=ok
check "$r" ok "postinstall: no systemctl without systemd"
echo "$out" | grep -q 'systemd-sysusers' && r=ok || r=no
check "$r" ok "postinstall: sysusers still runs without systemd"

# preinstall：legacy 存在 → exit 1 + 提示；upgrade 不检测
LEG="$TMP/legroot"; mkdir -p "$LEG/usr/local/bin"; touch "$LEG/usr/local/bin/immurok-daemon"
out=$(IMMUROK_PKG_ROOT="$LEG" sh "$S/preinstall.sh" install 2>&1); rc=$?
check "$rc" 1 "preinstall: legacy install aborts"
echo "$out" | grep -q 'make uninstall' && r=ok || r=no
check "$r" ok "preinstall: tells user to make uninstall"
IMMUROK_PKG_ROOT="$LEG" sh "$S/preinstall.sh" upgrade 0.8.0 >/dev/null 2>&1; rc=$?
check "$rc" 0 "preinstall: upgrade skips legacy check"
IMMUROK_PKG_ROOT="$FR" sh "$S/preinstall.sh" install >/dev/null 2>&1; rc=$?
check "$rc" 0 "preinstall: clean system passes"

# preremove remove：先摘 PAM，再停 daemon / session-agent；upgrade 什么都不做
out=$(dry "$S/preremove.sh" remove)
expected="RUN: $FR/usr/bin/immurok-pam-helper remove sudo polkit-1 gdm-password
RUN: systemctl disable --now immurok-daemon.service
RUN: systemctl --global disable immurok-session-agent.service
RUN: systemctl --user --machine=alice@ stop immurok-session-agent.service"
check "$(echo "$out" | grep '^RUN:')" "$expected" "preremove remove: command sequence"
out=$(dry "$S/preremove.sh" upgrade 0.9.0)
check "$(echo "$out" | grep -c '^RUN:')" 0 "preremove upgrade: no-op"
out=$(dry "$S/preremove.sh" 1)
check "$(echo "$out" | grep -c '^RUN:')" 0 "preremove rpm upgrade: no-op"

# postremove：remove 只 reload；purge 还删状态；upgrade 无操作
out=$(dry "$S/postremove.sh" remove)
expected="RUN: systemctl daemon-reload
RUN: systemctl reload dbus"
check "$(echo "$out" | grep '^RUN:')" "$expected" "postremove remove: reload only"
out=$(dry "$S/postremove.sh" purge)
echo "$out" | grep -q "RUN: rm -rf $FR/var/lib/immurok $FR/var/log/immurok" && r=ok || r=no
check "$r" ok "postremove purge: removes state"
out=$(dry "$S/postremove.sh" upgrade 0.9.0)
check "$(echo "$out" | grep -c '^RUN:')" 0 "postremove upgrade: no-op"
```

- [ ] **Step 2: 运行确认失败**

Run: `bash scripts/test-pkg-scripts.sh`
Expected: 前半 PASS，序列测试 FAIL，退出码 1。

- [ ] **Step 3: 写 `packaging/scripts/preinstall.sh`**

```sh
#!/bin/sh
# preinstall — 只做一件事：拒绝装在有 make install（/usr/local）的机器上。
# 这是唯一故意非零退出的 maintainer 脚本。Arch 走不到这里（pacman 先按文件冲突
# 拒装 PAM .so / polkit policy），deb / rpm 靠它干净中止。
main() {
    phase=$(pkg_phase preinstall "$@")
    [ "$phase" = fresh ] || return 0
    if legacy_install_present; then
        cat >&2 <<'EOF'
immurok: a source install (make install -> /usr/local) is present on this system.
  Remove it first, from the checkout you installed from:
      make uninstall
  Pairing data in /var/lib/immurok is kept. After installing this package,
  re-enable the PAM hooks from the PAM page (or: immurok-cli pam install).
EOF
        exit 1
    fi
}
main "$@"
```

- [ ] **Step 4: 写 `packaging/scripts/postinstall.sh`**

```sh
#!/bin/sh
# postinstall — 全新安装与升级走同一条幂等路径。每步 best-effort，永不非零退出。
main() {
    phase=$(pkg_phase postinstall "$@")

    # 专用系统用户。systemd-sysusers 不需要 systemd 在跑，chroot 里也能建。
    run systemd-sysusers immurok.conf
    # BlueZ 授权来自我们的 D-Bus policy；有些发行版另外按组放行，两条都占上。
    if pkg_has_group bluetooth; then
        run usermod -aG bluetooth immurok
    fi
    # make uninstall --keep-state 会 userdel 但留下 /var/lib/immurok；sysusers 重建
    # 的 uid 可能不同。目录 0700 只有 daemon 用，无条件收归。
    for d in /var/lib/immurok /var/log/immurok; do
        [ -d "$PKG_ROOT$d" ] && run chown -R immurok:immurok "$PKG_ROOT$d"
    done
    run systemd-tmpfiles --create immurok.conf

    if have_systemd; then
        run systemctl daemon-reload
        reload_dbus
        run systemctl enable immurok-daemon.service
        # restart 而不是 --now：升级时 --now 对已在运行的服务是空操作，旧进程会一直跑。
        run systemctl restart immurok-daemon.service
        # 用户级单元对所有用户启用；对已登录的用户立刻拉起，不用重新登录。
        run systemctl --global enable immurok-session-agent.service
        for u in $(logged_in_users); do
            run systemctl --user --machine="$u@" start immurok-session-agent.service
        done
    fi

    cat <<'EOF'
immurok installed.
  1. Pair your device: open "immurok" from the app menu, or run: immurok-cli pair
  2. Enable fingerprint for sudo / polkit / login on the PAM page (or: immurok-cli pam install)
  Daemon status: systemctl status immurok-daemon
EOF
    return 0
}
main "$@"
```

- [ ] **Step 5: 写 `packaging/scripts/preremove.sh`**

```sh
#!/bin/sh
# preremove — 只在真正卸载时跑（升级跳过）。先摘 PAM 再停服务：模块文件一删，
# /etc/pam.d 里残留的 pam_immurok.so 行会让每次认证记一条错误日志。
main() {
    phase=$(pkg_phase preremove "$@")
    [ "$phase" = upgrade ] && return 0

    helper="$PKG_ROOT/usr/bin/immurok-pam-helper"
    if [ -x "$helper" ]; then
        run "$helper" remove sudo polkit-1 gdm-password
    fi
    if have_systemd; then
        run systemctl disable --now immurok-daemon.service
        run systemctl --global disable immurok-session-agent.service
        for u in $(logged_in_users); do
            run systemctl --user --machine="$u@" stop immurok-session-agent.service
        done
    fi
    return 0
}
main "$@"
```

- [ ] **Step 6: 写 `packaging/scripts/postremove.sh`**

```sh
#!/bin/sh
# postremove — 让 systemd / dbus 忘掉我们；deb purge 时连状态一起删。
# rpm / arch 没有 purge：状态保留，README 写手动清理方式。
# immurok 系统用户按 Debian policy 不删。
main() {
    phase=$(pkg_phase postremove "$@")
    [ "$phase" = upgrade ] && return 0

    if have_systemd; then
        run systemctl daemon-reload
        reload_dbus
    fi
    if [ "$phase" = purge ]; then
        run rm -rf "$PKG_ROOT/var/lib/immurok" "$PKG_ROOT/var/log/immurok"
    fi
    return 0
}
main "$@"
```

- [ ] **Step 7: 运行测试确认通过**

Run: `bash scripts/test-pkg-scripts.sh && bash scripts/test-stage.sh`
Expected: 全部 PASS。若 `postinstall fresh: command sequence` 失败，逐行 diff `echo "$out" | grep '^RUN:'` 与期望——常见原因是 `chown` 那行的路径前缀或 `usermod` 顺序。

- [ ] **Step 8: Commit**

```bash
git add packaging/scripts scripts/test-pkg-scripts.sh
git commit -m "pkg(linux): preinstall/postinstall/preremove/postremove 正文 + dryrun 序列测试"
```

---

### Task 5: `nfpm.yaml` + `pkg-env.sh` + `make package`，本地出三种包并检查内容

**Files:**
- Create: `packaging/nfpm.yaml`
- Create: `packaging/pkg-env.sh`
- Modify: `Makefile`（`package` target、`.PHONY`）

**Interfaces:**
- Consumes: `dist/stage/`（Task 2/4）、`pam/pam_immurok.so`、`LICENSE`。
- Produces: `dist/immurok_<v>_<deb-arch>.deb`、`dist/immurok-<v>-1.<rpm-arch>.rpm`、`dist/immurok-<v>-1-<rpm-arch>.pkg.tar.zst`。环境变量约定：`VERSION`、`NFPM_ARCH`（`amd64|arm64`）、`DEB_TRIPLET`（`x86_64-linux-gnu|aarch64-linux-gnu`），由 `packaging/pkg-env.sh` 计算（`PKG_MACHINE` 可覆盖 `uname -m`）。

- [ ] **Step 1: 安装 nfpm 到本机（一次性）**

```bash
mkdir -p ~/.local/bin
curl -sL https://github.com/goreleaser/nfpm/releases/download/v2.47.0/nfpm_2.47.0_Linux_x86_64.tar.gz | tar -xz -C ~/.local/bin nfpm
~/.local/bin/nfpm --version
```

Expected: `nfpm version v2.47.0`（`~/.local/bin` 需在 PATH）。

- [ ] **Step 2: 写 `packaging/pkg-env.sh`**

```bash
# packaging/pkg-env.sh — 供 Makefile / CI source，算出 nfpm.yaml 需要的变量。
# 用法: set -a; . packaging/pkg-env.sh; set +a
#   VERSION      workspace crate 版本（以 immurok-daemon 为准，六个 crate 同号）
#   NFPM_ARCH    nfpm 的 GOARCH 风格架构名，它会自己翻成 deb amd64/arm64、rpm/arch x86_64/aarch64
#   DEB_TRIPLET  Debian 多架构目录名（PAM 模块落在 /usr/lib/<triplet>/security）
# 必须从仓库根目录 source（Makefile 与 CI 都是）：POSIX sh 里被 source 的文件拿不到
# 自己的路径，所以按 CWD 找 Cargo.toml。PKG_MACHINE 可覆盖 uname -m（测试用）。
[ -f crates/immurok-daemon/Cargo.toml ] || { echo "pkg-env.sh: source me from the repo root" >&2; return 1 2>/dev/null || exit 1; }
VERSION=$(grep -m1 '^version' crates/immurok-daemon/Cargo.toml | cut -d'"' -f2)
case "${PKG_MACHINE:-$(uname -m)}" in
    x86_64)        NFPM_ARCH=amd64; DEB_TRIPLET=x86_64-linux-gnu ;;
    aarch64|arm64) NFPM_ARCH=arm64; DEB_TRIPLET=aarch64-linux-gnu ;;
    *) echo "pkg-env.sh: unsupported machine $(uname -m)" >&2; return 1 2>/dev/null || exit 1 ;;
esac
```

验证（在 `app-linux-rs/` 下）：`sh -c 'set -a; . packaging/pkg-env.sh; set +a; echo $VERSION $NFPM_ARCH $DEB_TRIPLET'` → `0.9.0 amd64 x86_64-linux-gnu`；`PKG_MACHINE=aarch64 sh -c '…'` → `0.9.0 arm64 aarch64-linux-gnu`；`cd /tmp && sh -c '. …/pkg-env.sh'` → 报 `source me from the repo root`。

- [ ] **Step 3: 写 `packaging/nfpm.yaml`**

```yaml
# packaging/nfpm.yaml — 一份清单出 deb / rpm / archlinux（设计稿 §2、§4）。
#
# 从仓库根目录执行，先装配：
#   packaging/stage.sh dist/stage
#   set -a; . packaging/pkg-env.sh; set +a
#   nfpm package -f packaging/nfpm.yaml -p deb -t dist/     # rpm / archlinux 同理
# 或直接 `make package`。
#
# 依赖按 ldd 实测手写（nfpm 不扫 shlib）：daemon → libdbus-1 / libsystemd；
# GUI → gtk4 / libadwaita；PAM .so → libpam；ble-notify-helper.py → dbus-fast；
# immurok-auth-dialog → PyGObject + Gtk4 + Adw。包名写错由 CI verify 在真实
# 发行版容器里安装时抓出来。
name: immurok
arch: ${NFPM_ARCH}
platform: linux
version: ${VERSION}
release: "1"
section: admin
priority: optional
maintainer: immurok <support@immurok.com>
vendor: immurok
homepage: https://immurok.com
license: Apache-2.0
description: |
  Wireless fingerprint key companion for Linux.
  BLE daemon, PAM module, CLI/TUI, GTK settings app and the imk agent
  command wrapper for the immurok fingerprint key.

contents:
  - src: dist/stage/root/usr/
    dst: /usr/
    type: tree
  # 系统级自启动：用户删了它就是禁用，升级不复活
  - src: dist/stage/root/etc/xdg/autostart/com.immurok.Settings.desktop
    dst: /etc/xdg/autostart/com.immurok.Settings.desktop
    type: config|noreplace
  - src: LICENSE
    dst: /usr/share/licenses/immurok/LICENSE
    type: license
  # PAM 模块目录三家不同（设计稿 §3.1）
  - src: pam/pam_immurok.so
    dst: /usr/lib/${DEB_TRIPLET}/security/pam_immurok.so
    packager: deb
  - src: pam/pam_immurok.so
    dst: /usr/lib64/security/pam_immurok.so
    packager: rpm
  - src: pam/pam_immurok.so
    dst: /usr/lib/security/pam_immurok.so
    packager: archlinux

scripts:
  preinstall: dist/stage/scripts/preinstall.sh
  postinstall: dist/stage/scripts/postinstall.sh
  preremove: dist/stage/scripts/preremove.sh
  postremove: dist/stage/scripts/postremove.sh

archlinux:
  packager: immurok <support@immurok.com>
  scripts:
    # nfpm 只把 postinstall 映射成 post_install；升级钩子要显式给，否则 Arch
    # 升级后 daemon 不会重启（设计稿 §4.3）。preupgrade 不配：legacy 检测只在
    # 全新安装有意义。
    postupgrade: dist/stage/scripts/postinstall.sh

rpm:
  summary: Wireless fingerprint key companion for Linux
  group: Applications/System

overrides:
  deb:
    depends:
      - python3
      - python3-dbus-fast
      - python3-gi
      - gir1.2-gtk-4.0
      - gir1.2-adw-1
      - libgtk-4-1
      - libadwaita-1-0
      - libdbus-1-3
      - libsystemd0
      - libpam0g
      - bluez
      - polkitd | policykit-1
      - libnotify-bin
      - systemd
      - dbus
  rpm:
    depends:
      - python3
      - python3-dbus-fast
      - python3-gobject
      - gtk4
      - libadwaita
      - dbus-libs
      - systemd-libs
      - pam
      - bluez
      - polkit
      - libnotify
      - systemd
  archlinux:
    depends:
      - python
      - python-dbus-fast
      - python-gobject
      - gtk4
      - libadwaita
      - dbus
      - systemd-libs
      - pam
      - bluez
      - polkit
      - libnotify
```

- [ ] **Step 4: Makefile 加 `package` target**

在 `.PHONY` 行加 `package verify-pkg`；在 `clean:` 之前加：

```make
# ── 发行版安装包 ──────────────────────────────────────────────────
# 出 deb / rpm / archlinux 三种包到 dist/（设计稿 docs/superpowers/specs/2026-09-21-linux-packaging-design.md）。
# 需要 nfpm：https://github.com/goreleaser/nfpm/releases，解开后把二进制放进 PATH。
# GUI 是包的一部分，没有 gtk4/libadwaita 开发头就不出包。
DIST_DIR = dist
NFPM ?= nfpm

package: build pam
	@[ -n "$(HAS_GTK_DEV)" ] || { echo "✗ package needs immurok-gui: install gtk4/libadwaita dev headers"; exit 1; }
	@command -v $(NFPM) >/dev/null 2>&1 || { echo "✗ nfpm not found: https://github.com/goreleaser/nfpm/releases (put the binary in ~/.local/bin)"; exit 1; }
	bash packaging/stage.sh $(DIST_DIR)/stage
	@set -e; set -a; . packaging/pkg-env.sh; set +a; \
	for f in deb rpm archlinux; do $(NFPM) package -f packaging/nfpm.yaml -p $$f -t $(DIST_DIR)/; done
	@ls -1 $(DIST_DIR)/*.deb $(DIST_DIR)/*.rpm $(DIST_DIR)/*.pkg.tar.zst
```

`clean:` 追加一行 `rm -rf $(DIST_DIR)`。

- [ ] **Step 5: 本地出包**

Run: `make package`
Expected: 末尾列出 `dist/immurok_0.9.0_amd64.deb`、`dist/immurok-0.9.0-1.x86_64.rpm`、`dist/immurok-0.9.0-1-x86_64.pkg.tar.zst`。若 nfpm 报 `field packager … invalid` 之类的 schema 错误，对照 `nfpm jsonschema` 输出修 yaml 键名。

- [ ] **Step 6: 检查包内容**

```bash
dpkg-deb -c dist/immurok_0.9.0_amd64.deb | grep -E 'pam_immurok|immurok-daemon$|autostart|LICENSE'
dpkg-deb -e dist/immurok_0.9.0_amd64.deb /tmp/deb-ctl && cat /tmp/deb-ctl/control && head -3 /tmp/deb-ctl/postinst && cat /tmp/deb-ctl/conffiles; rm -rf /tmp/deb-ctl
bsdtar -tf dist/immurok-0.9.0-1.x86_64.rpm | grep -E 'pam_immurok|lib64'
bsdtar -tf dist/immurok-0.9.0-1-x86_64.pkg.tar.zst | grep -E 'pam_immurok|\.INSTALL|\.PKGINFO'
bsdtar -xOf dist/immurok-0.9.0-1-x86_64.pkg.tar.zst .INSTALL | grep -E '^function'
bsdtar -xOf dist/immurok-0.9.0-1-x86_64.pkg.tar.zst .PKGINFO | grep -E '^(depend|pkgver|arch)'
```

Expected：
- deb：`./usr/lib/x86_64-linux-gnu/security/pam_immurok.so`；`-rwxr-xr-x … ./usr/bin/immurok-daemon`；`-rw-r--r-- … ./usr/lib/systemd/system/immurok-daemon.service`；`control` 的 `Depends:` 含 `polkitd | policykit-1`；`conffiles` 是 `/etc/xdg/autostart/com.immurok.Settings.desktop`；`postinst` 第一行 `#!/bin/sh`。
- rpm：`./usr/lib64/security/pam_immurok.so`。
- arch：`usr/lib/security/pam_immurok.so`；`.INSTALL` 含 `function pre_install`、`post_install`、`post_upgrade`、`pre_remove`、`post_remove` 五个；`.PKGINFO` 有 `depend = python-dbus-fast` 等、`pkgver = 0.9.0-1`、`arch = x86_64`。

- [ ] **Step 7: Commit**

```bash
git add packaging/nfpm.yaml packaging/pkg-env.sh Makefile
git commit -m "pkg(linux): nfpm.yaml + make package（deb/rpm/archlinux 三种包）"
```

---

### Task 6: `packaging/verify.sh` + `make verify-pkg`，在四个发行版容器里本地装/验/卸

**Files:**
- Create: `packaging/verify.sh`
- Modify: `Makefile`（`verify-pkg` target）

**Interfaces:**
- Produces: `packaging/verify.sh <dist-dir>`，容器内以 root 运行，退出码 0 = 全过；最后一行 `ALL PASS (<id> <machine>)`。CI 的 verify job（Task 7）与 `make verify-pkg DISTRO=…` 都调它。

- [ ] **Step 1: 写 `packaging/verify.sh`**

```sh
#!/bin/sh
# packaging/verify.sh — 在目标发行版容器里以 root 执行：装包 → 断言 → 卸包 → 断言。
#
# 用法: verify.sh <dist-dir>   目录里放 make package / CI 的产物，按 /etc/os-release 自动挑。
#
# 容器里没有 systemd 在跑，maintainer 脚本会跳过 systemctl；这里验证的是：
#   - 依赖包名在该发行版真实存在（apt/dnf/pacman 解析）
#   - 文件落点、PAM .so 目录、policy 替换、无 /usr/local 残留
#   - sysusers 建户、python 运行依赖可 import、unit 文件通过 systemd-analyze verify
#   - 脚本在无 systemd 环境不报错，卸载干净
set -eu
DIST="${1:?usage: verify.sh <dist-dir>}"
. /etc/os-release

fail() { echo "FAIL: $*" >&2; exit 1; }
ok()   { echo "PASS: $*"; }

case "$(uname -m)" in
    x86_64)  DEB_ARCH=amd64; RPM_ARCH=x86_64;  PAM_DEB=/usr/lib/x86_64-linux-gnu/security ;;
    aarch64) DEB_ARCH=arm64; RPM_ARCH=aarch64; PAM_DEB=/usr/lib/aarch64-linux-gnu/security ;;
    *) fail "unsupported machine $(uname -m)" ;;
esac

case "$ID" in
    debian|ubuntu)
        PKG=$(ls "$DIST"/immurok_*_"$DEB_ARCH".deb | head -1)
        PAM_DIR=$PAM_DEB
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq --no-install-recommends "$PKG"
        REMOVE="apt-get remove -y -qq immurok"
        ;;
    fedora)
        PKG=$(ls "$DIST"/immurok-*."$RPM_ARCH".rpm | head -1)
        PAM_DIR=/usr/lib64/security
        dnf install -y -q "$PKG"
        REMOVE="dnf remove -y -q immurok"
        ;;
    arch)
        PKG=$(ls "$DIST"/immurok-*-"$RPM_ARCH".pkg.tar.zst | head -1)
        PAM_DIR=/usr/lib/security
        pacman -Sy --noconfirm >/dev/null
        pacman -U --noconfirm "$PKG"
        REMOVE="pacman -R --noconfirm immurok"
        ;;
    *) fail "unsupported distro $ID" ;;
esac
ok "installed $(basename "$PKG") on $ID $(uname -m)"

for f in /usr/bin/immurok-daemon /usr/bin/immurok-cli /usr/bin/imk /usr/bin/immurok-session-agent \
         /usr/bin/immurok-gui /usr/bin/immurok-auth-dialog /usr/bin/immurok-pam-helper /usr/bin/ble-notify-helper.py \
         /usr/lib/systemd/system/immurok-daemon.service /usr/lib/systemd/user/immurok-session-agent.service \
         /usr/lib/sysusers.d/immurok.conf /usr/lib/tmpfiles.d/immurok.conf \
         /usr/share/dbus-1/system.d/immurok.conf /usr/share/dbus-1/services/com.immurok.Settings.service \
         /usr/share/polkit-1/rules.d/49-immurok.rules /usr/share/polkit-1/actions/com.immurok.pam-helper.policy \
         /usr/share/applications/com.immurok.Settings.desktop /etc/xdg/autostart/com.immurok.Settings.desktop \
         /usr/share/licenses/immurok/LICENSE "$PAM_DIR/pam_immurok.so"; do
    [ -e "$f" ] || fail "missing $f"
done
ok "all files present (PAM -> $PAM_DIR)"

grep -q '>/usr/bin/immurok-pam-helper<' /usr/share/polkit-1/actions/com.immurok.pam-helper.policy \
    || fail "policy exec.path not substituted"
if grep -q '/usr/local' /usr/lib/systemd/system/immurok-daemon.service \
        /usr/lib/systemd/user/immurok-session-agent.service \
        /usr/share/dbus-1/services/com.immurok.Settings.service; then
    fail "/usr/local leaked into unit files"
fi
getent passwd immurok >/dev/null || fail "sysusers did not create user immurok"
ok "policy substituted, no /usr/local, user immurok exists"

/usr/bin/immurok-cli --version | grep -q immurok-cli || fail "immurok-cli --version"
/usr/bin/imk --version | grep -q imk || fail "imk --version"
python3 -c 'import dbus_fast, gi' || fail "python runtime deps (dbus_fast / gi)"
python3 -c 'import gi; gi.require_version("Gtk", "4.0"); gi.require_version("Adw", "1"); from gi.repository import Gtk, Adw' \
    || fail "Gtk4 / Adw typelibs"
ok "binaries run, python deps import"

systemd-analyze verify /usr/lib/systemd/system/immurok-daemon.service || fail "systemd-analyze verify (system unit)"
ok "system unit verifies"

$REMOVE
[ -e /usr/bin/immurok-daemon ] && fail "binary still present after remove"
[ -e "$PAM_DIR/pam_immurok.so" ] && fail "PAM module still present after remove"
ok "removed cleanly"
echo "ALL PASS ($ID $(uname -m))"
```

```bash
chmod 755 packaging/verify.sh
```

- [ ] **Step 2: Makefile 加 `verify-pkg`**

在 `package` target 之后加：

```make
# 在发行版容器里装/验/卸 dist/ 里的包——本地复现 CI 的 verify job。
#   make verify-pkg DISTRO=debian:12   （ubuntu:24.04 / fedora:43 / archlinux:latest）
DISTRO ?= debian:12
verify-pkg:
	docker run --rm -v "$$(pwd)/$(DIST_DIR):/dist:ro" -v "$$(pwd)/packaging/verify.sh:/verify.sh:ro" \
		$(DISTRO) sh /verify.sh /dist
```

- [ ] **Step 3: 四个发行版跑一遍**

```bash
for d in debian:12 ubuntu:24.04 fedora:43 archlinux:latest; do
  echo "=== $d"; make verify-pkg DISTRO=$d 2>&1 | tail -15 || break
done
```

Expected: 每个都以 `ALL PASS (<id> x86_64)` 结束。典型失败与处理：
- `E: Unable to locate package gir1.2-adw-1` 之类 → 依赖包名错，改 `nfpm.yaml` 的对应 override，`make package` 重出。
- Fedora `nothing provides python3-dbus-fast` → 查 `dnf repoquery python3-dbus-fast`，确认 Fedora 43 仓库名。
- `systemd-analyze` 不存在 → 该发行版 `systemd` 包没被依赖拉进来，把 `systemd` 加进对应 `depends`。
- pacman `error: failed to prepare transaction (could not satisfy dependencies)` → 看它列出的包名。
- Arch 容器 `pacman -U` 报 `signature` 之类 → 是仓库 keyring 过期，先 `pacman -Sy archlinux-keyring`（加进 verify.sh 的 arch 分支）。

- [ ] **Step 4: Commit**

```bash
git add packaging/verify.sh Makefile
git commit -m "pkg(linux): verify.sh 在发行版容器里装/验/卸；make verify-pkg"
```

---

### Task 7: GitHub Actions `release.yml`

**Files:**
- Create: `.github/workflows/release.yml`

**Interfaces:**
- Consumes: `packaging/stage.sh`、`packaging/pkg-env.sh`、`packaging/nfpm.yaml`、`packaging/verify.sh`、`pam/Makefile`。
- Produces: tag `v*` → GitHub Release 挂 6 个包 + `SHA256SUMS`；`workflow_dispatch` → 只出 artifact。

- [ ] **Step 1: 写 `.github/workflows/release.yml`**

```yaml
name: Release

# 打 tag 发版：  git tag v0.9.0 && git push origin v0.9.0
# 手动 dry run（Actions → Release → Run workflow）：只构建 + 验证，产物作 artifact，不发 Release。
#
# 一次构建（debian:12，glibc 2.36 是目标里最老的）→ nfpm 出 deb / rpm / archlinux
# → 在四个发行版容器里真实安装验证 → 挂 Release。
# 设计稿：docs/superpowers/specs/2026-09-21-linux-packaging-design.md
on:
  push:
    tags: ['v*']
  workflow_dispatch:

permissions:
  contents: write

env:
  NFPM_VERSION: 2.47.0

jobs:
  preflight:
    runs-on: ubuntu-24.04
    outputs:
      version: ${{ steps.ver.outputs.version }}
    steps:
      - uses: actions/checkout@v6
      - name: Version from Cargo.toml must match the tag; CHANGELOG must have the section
        id: ver
        run: |
          set -euo pipefail
          V=$(grep -m1 '^version' crates/immurok-daemon/Cargo.toml | cut -d'"' -f2)
          echo "version=$V" >> "$GITHUB_OUTPUT"
          if [ "${{ github.event_name }}" = "push" ]; then
            TAG="${GITHUB_REF_NAME#v}"
            [ "$TAG" = "$V" ] || { echo "tag v$TAG != Cargo version $V"; exit 1; }
          fi
          grep -q "^## $V" CHANGELOG.md || { echo "CHANGELOG.md has no '## $V' section (rename 'Unreleased' before tagging)"; exit 1; }
          echo "version $V ok"

  build:
    needs: preflight
    strategy:
      fail-fast: false
      matrix:
        include:
          - arch: amd64
            runner: ubuntu-24.04
          - arch: arm64
            runner: ubuntu-24.04-arm
    runs-on: ${{ matrix.runner }}
    container: debian:12
    steps:
      - name: Build dependencies
        run: |
          apt-get update -qq
          DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
            build-essential pkg-config libdbus-1-dev libpam0g-dev libgtk-4-dev libadwaita-1-dev \
            curl ca-certificates git
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
        with:
          key: ${{ matrix.arch }}
      - name: Build + test
        run: |
          set -euo pipefail
          cargo build --release --workspace --locked
          cargo test --release --workspace --locked
          make -C pam
          make -C pam test
          bash scripts/test-stage.sh
          bash scripts/test-pkg-scripts.sh
      - name: nfpm
        run: |
          set -euo pipefail
          case "${{ matrix.arch }}" in amd64) A=x86_64 ;; arm64) A=arm64 ;; esac
          curl -sL "https://github.com/goreleaser/nfpm/releases/download/v${NFPM_VERSION}/nfpm_${NFPM_VERSION}_Linux_${A}.tar.gz" \
            | tar -xz -C /usr/local/bin nfpm
          nfpm --version
      - name: Package
        run: |
          set -euo pipefail
          bash packaging/stage.sh dist/stage
          set -a; . packaging/pkg-env.sh; set +a
          for f in deb rpm archlinux; do nfpm package -f packaging/nfpm.yaml -p $f -t dist/; done
          ls -l dist/*.deb dist/*.rpm dist/*.pkg.tar.zst
      - uses: actions/upload-artifact@v7
        with:
          name: packages-${{ matrix.arch }}
          path: |
            dist/*.deb
            dist/*.rpm
            dist/*.pkg.tar.zst
          if-no-files-found: error

  verify:
    needs: build
    strategy:
      fail-fast: false
      matrix:
        arch: [amd64, arm64]
        image: ['debian:12', 'ubuntu:24.04', 'fedora:43', 'archlinux:latest']
        exclude:
          # 官方 archlinux 镜像只有 x86_64（Arch Linux ARM 是另一个项目）
          - arch: arm64
            image: 'archlinux:latest'
    runs-on: ${{ matrix.arch == 'arm64' && 'ubuntu-24.04-arm' || 'ubuntu-24.04' }}
    container: ${{ matrix.image }}
    steps:
      - uses: actions/checkout@v6
      - uses: actions/download-artifact@v7
        with:
          name: packages-${{ matrix.arch }}
          path: dist
      - name: Install / assert / remove
        run: sh packaging/verify.sh dist

  release:
    if: github.event_name == 'push'
    needs: [preflight, build, verify]
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v6
      - uses: actions/download-artifact@v7
        with:
          pattern: packages-*
          path: dist
          merge-multiple: true
      - name: Checksums + release notes
        run: |
          set -euo pipefail
          cd dist && sha256sum *.deb *.rpm *.pkg.tar.zst > SHA256SUMS && cat SHA256SUMS && cd ..
          V="${{ needs.preflight.outputs.version }}"
          awk -v v="$V" '/^## /{p = ($2 == v)} p' CHANGELOG.md > notes.md
          cat notes.md
      - uses: softprops/action-gh-release@v3
        with:
          name: v${{ needs.preflight.outputs.version }}
          body_path: notes.md
          files: |
            dist/*.deb
            dist/*.rpm
            dist/*.pkg.tar.zst
            dist/SHA256SUMS
```

- [ ] **Step 2: 本地静态检查**

```bash
python3 -c "import yaml,sys; d=yaml.safe_load(open('.github/workflows/release.yml')); print(list(d['jobs']))"
```

Expected: `['preflight', 'build', 'verify', 'release']`。（`pip install --user pyyaml` 若缺。）另外确认 `scripts/sync-github.sh` 的 rsync 不排除 `.github/`：`grep -n exclude ../scripts/sync-github.sh | grep -i github` 应无输出。

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "pkg(linux): GitHub Actions release.yml——tag 触发构建/验证/发 Release，dispatch 为 dry run"
```

---

### Task 8: 文档（README / TESTING / CHANGELOG）

**Files:**
- Modify: `README.md`（新增"Install from a package"章节放在 "1. Install dependencies" 之前；修 Arch dbus-fast 过时说明；"3. Install" 表格改为 `/usr/local` 现状；"7. Uninstall" 补包的卸载与状态清理；互斥说明）
- Modify: `TESTING.md`（末尾加"安装包验收"一节）
- Modify: `CHANGELOG.md`（`## Unreleased` 下加 `### Added` 条目）

- [ ] **Step 1: README 新增章节**（插在 `## 1. Install dependencies` 之前）

```markdown
## 0. Install from a package (recommended)

Prebuilt packages for **Debian 12+ / Ubuntu 24.04+ / Fedora 43+ / Arch**, amd64 and arm64,
are attached to every [GitHub Release](https://github.com/immurok/app-linux-rs/releases).

```bash
# Debian / Ubuntu
sudo apt install ./immurok_<version>_amd64.deb
# Fedora
sudo dnf install ./immurok-<version>-1.x86_64.rpm
# Arch
sudo pacman -U immurok-<version>-1-x86_64.pkg.tar.zst
```

The package starts `immurok-daemon`, enables the session agent for every user and registers
the settings app to autostart. Two things are left to you:

1. **Pair** — open *immurok* from the app menu, or run `immurok-cli pair`.
2. **Enable the PAM hooks** on the PAM page (or `immurok-cli pam install`). The package
   deliberately does not touch `/etc/pam.d` on its own.

Upgrading is `apt install ./new.deb` / `dnf install ./new.rpm` / `pacman -U new.pkg.tar.zst`
again; the daemon is restarted for you.

> A package install and a source install (`make install`, section 3) cannot coexist: both
> ship `pam_immurok.so` and the polkit policy at the same paths, and `/usr/local` shadows
> `/usr`. If you installed from source before, run `make uninstall` in that checkout first
> — pairing data in `/var/lib/immurok` survives, only the PAM hooks need re-enabling.
> The package refuses to install while a source install is present (on Arch, `pacman`
> reports the file conflict instead).

Sections 1–3 below are for building from source.
```

- [ ] **Step 2: README 其余修正**

- 第 3 行 `verified on **Arch / Fedora 38+ / Debian 12+ (incl. Ubuntu 22.04+)` 改为 `verified on **Arch / Fedora 43+ / Debian 12+ / Ubuntu 24.04+**`；紧接一句 `Ubuntu 22.04 has no packaged dbus-fast and is source-install only (see section 1).`
- Arch 依赖段：删掉 `# python-dbus-fast is in the AUR` / `yay -S python-dbus-fast` / pip 三行，把 `python-dbus-fast` 加进 `pacman -S --needed …` 那行。
- "3. Install" 的表格：`~/.local/bin/` 两行改为 `/usr/local/bin/`（Needs sudo: Yes）；`systemd user service` 行改为 `systemd system unit | /etc/systemd/system/immurok-daemon.service | Yes` 并加一行 `session agent | /etc/systemd/user/immurok-session-agent.service | Yes`；删 `systemd polkit overrides` 行；`systemctl --user status immurok-daemon` 改为 `systemctl status immurok-daemon`。
- "7. Uninstall" 末尾追加：

```markdown
### Package install

```bash
sudo apt remove immurok      # or: sudo apt purge immurok   (also deletes /var/lib/immurok)
sudo dnf remove immurok
sudo pacman -R immurok
```

Removing the package takes the PAM hooks out of `/etc/pam.d` and stops the daemon. On
Fedora and Arch the pairing data is kept; to wipe it run
`sudo immurok-pam-helper purge-daemon --purge-state` **before** removing the package, or
`sudo rm -rf /var/lib/immurok /var/log/immurok` afterwards.
```

- [ ] **Step 3: TESTING.md 末尾追加**

```markdown
## 安装包验收（deb / rpm / arch）

CI 已在四个发行版容器里装/验/卸（`packaging/verify.sh`，本地 `make verify-pkg DISTRO=…` 同款）。
容器里没有 systemd 在跑，下面这些只能在真机或带 systemd 的容器（`systemd-nspawn` /
`docker run --privileged` + systemd 镜像）验证：

### Arch 开发机（每次改 packaging/ 都跑）

1. `make uninstall`（保留 /var/lib/immurok），确认 `/usr/local/bin/immurok-daemon` 不存在。
2. `make package && sudo pacman -U dist/immurok-*-x86_64.pkg.tar.zst`。
3. `systemctl is-active immurok-daemon` → active；`ls -ln /var/lib/immurok` 属主是新的 immurok uid（postinstall 的 chown）；`journalctl -u immurok-daemon -n 20` 里已连上设备（配对数据存活）。
4. `systemctl --user is-active immurok-session-agent` → active（postinstall 对已登录用户直接拉起，未重新登录）。
5. 打开 immurok（app menu）→ PAM 页 Install → `sudo -k; sudo true` 走指纹。
6. 升级路径：`sudo pacman -U` 同一个包 → daemon 被 restart（`systemctl show -p ExecMainStartTimestamp immurok-daemon` 变了），PAM 行仍在。
7. `sudo pacman -R immurok` → `grep pam_immurok /etc/pam.d/sudo /etc/pam.d/polkit-1` 无输出；daemon 停；`/var/lib/immurok` 保留。

### Debian / Fedora（带 systemd 的容器，一次性）

`sysusers` 建户、`systemctl --global enable`、`preinstall` 对 `/usr/local` 遗留的中止（touch 一个 `/usr/local/bin/immurok-daemon` 再 `apt install ./x.deb` 应失败并打印 make uninstall 提示）。
```

- [ ] **Step 4: CHANGELOG `## Unreleased` 下加**

```markdown
### Added

- **Distribution packages.** `.deb` (Debian 12+ / Ubuntu 24.04+), `.rpm`
  (Fedora 43+) and Arch `.pkg.tar.zst`, amd64 and arm64, built by GitHub
  Actions on every `v*` tag and attached to the release together with
  `SHA256SUMS`. One package installs the daemon, CLI/TUI, `imk`, the GTK
  settings app, the session agent and the PAM module; the daemon is started,
  the session agent enabled for every user and the settings app registered
  to autostart. Pairing and enabling the PAM hooks stay manual by design.
  `make package` builds the same three packages locally (needs nfpm),
  `make verify-pkg DISTRO=…` installs one in a distro container.
- `make install` and a package install now refuse to coexist (each detects
  the other); unit files reference `/usr/bin` and are rewritten for
  `/usr/local` at install time.
```

- [ ] **Step 5: Commit**

```bash
git add README.md TESTING.md CHANGELOG.md
git commit -m "docs(linux): 安装包安装/卸载/验收说明；README 依赖与路径修正"
```

---

### Task 9: Arch 开发机真机验收

**Files:** 无改动（除非发现问题）。

前提：本机当前是 `make install` 装的（`/usr/local/bin/immurok-daemon` 存在）。所有提权命令走 `imk run --agent`。

- [ ] **Step 1: 卸掉源码安装**

```bash
ls -l /usr/local/bin/immurok-daemon && ls -ln /var/lib/immurok
imk run --agent -- make uninstall
ls /usr/local/bin/immurok-daemon 2>&1; ls -ln /var/lib/immurok
```

Expected: 卸载后 `immurok-daemon` 不存在；`/var/lib/immurok` 仍在，`pairing.json` / `owner` 属主是一个数字 uid（用户已被 userdel）。

- [ ] **Step 2: 装包**

```bash
make package
imk run --agent -- pacman -U --noconfirm dist/immurok-0.9.0-1-x86_64.pkg.tar.zst
```

Expected: 输出末尾有 `immurok installed.` 与两条 next steps；无 `warning:`。

- [ ] **Step 3: 断言**

```bash
systemctl is-active immurok-daemon
systemctl --user is-active immurok-session-agent
id immurok; ls -ln /var/lib/immurok        # 属主 = id immurok 的 uid
journalctl -u immurok-daemon -n 30 --no-pager | grep -iE 'connected|paired|BLE helper ready'
systemctl is-enabled immurok-daemon; systemctl --global is-enabled immurok-session-agent
ls /etc/xdg/autostart/com.immurok.Settings.desktop
```

Expected: 两个 `active`；属主一致；日志里 BLE helper ready + 连上设备；`enabled` × 2。

- [ ] **Step 4: PAM Install + 指纹 sudo**

打开 immurok GUI → PAM 页 → Install（pkexec 弹窗）。然后：

```bash
grep -n pam_immurok /etc/pam.d/sudo /etc/pam.d/polkit-1
sudo -k; imk run --agent -- sudo true && echo SUDO_OK
```

Expected: 两个文件各一行；触摸指纹后 `SUDO_OK`。

- [ ] **Step 5: 升级路径**

```bash
T1=$(systemctl show -p ExecMainStartTimestamp --value immurok-daemon)
imk run --agent -- pacman -U --noconfirm dist/immurok-0.9.0-1-x86_64.pkg.tar.zst
T2=$(systemctl show -p ExecMainStartTimestamp --value immurok-daemon)
[ "$T1" != "$T2" ] && echo RESTARTED
grep -c pam_immurok /etc/pam.d/sudo
```

Expected: `RESTARTED`（post_upgrade 生效）；`1`（PAM 行未动）。

- [ ] **Step 6: 卸载**

```bash
imk run --agent -- pacman -R --noconfirm immurok
grep pam_immurok /etc/pam.d/sudo /etc/pam.d/polkit-1; echo "grep rc=$?"
systemctl is-active immurok-daemon; ls /var/lib/immurok
```

Expected: `grep rc=1`（PAM 行已清）；`inactive`；`/var/lib/immurok` 保留。

- [ ] **Step 7: 恢复开发机（二选一）**

日常开发继续用包：`imk run --agent -- pacman -U --noconfirm dist/immurok-0.9.0-1-x86_64.pkg.tar.zst` 再在 PAM 页 Install。或回到源码安装：`imk run --agent -- make install`。

发现的任何问题按 Task 4/5/6 的测试补 case 再修，单独 commit。

---

### Task 10: 同步到 GitHub 并跑一次 workflow_dispatch dry run

- [ ] **Step 1: 同步**

```bash
cd /home/katsu/Documents/Projects/imPress-v1
imk run --agent -- ./scripts/sync-github.sh app-linux-rs -m "packaging: deb/rpm/arch packages + release workflow"
```

Expected: 推送成功；`https://github.com/immurok/app-linux-rs/tree/main/.github/workflows` 能看到 `release.yml`。

- [ ] **Step 2: 手动触发 dry run**

```bash
imk run --agent -- gh workflow run release.yml -R immurok/app-linux-rs
sleep 60; gh run list -R immurok/app-linux-rs -w release.yml -L 1
```

然后 `gh run watch -R immurok/app-linux-rs <run-id>`。

Expected: `preflight` 过（当前 Cargo 版本 0.9.0 且 CHANGELOG 有 `## 0.9.0`）；`build` 两个架构过；`verify` 7 格过；`release` 因 `if: github.event_name == 'push'` 跳过。artifact `packages-amd64` / `packages-arm64` 各含 3 个包。

若 `ubuntu-24.04-arm` runner 排队不动或不可用：确认仓库 public；否则先把 arm64 从矩阵里注释掉、记 TODO。

- [ ] **Step 3: 下载 arm64 artifact 抽查**

```bash
gh run download -R immurok/app-linux-rs <run-id> -n packages-arm64 -D /tmp/claude-1000/-home-katsu-Documents-Projects-imPress-v1/bc7c5f08-2067-4540-9906-f97bbced44a3/scratchpad/arm64
dpkg-deb -c …/arm64/immurok_0.9.0_arm64.deb | grep pam_immurok
```

Expected: `./usr/lib/aarch64-linux-gnu/security/pam_immurok.so`。

正式发版（打 `v0.9.x` tag）由用户决定时机，不在本计划内。
