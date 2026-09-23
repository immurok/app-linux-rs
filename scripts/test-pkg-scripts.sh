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
RUN: systemctl --user --machine=alice@ restart immurok-session-agent.service"
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
echo "$out" | grep '^RUN:' | grep -q 'systemctl' && r=present || r=ok
check "$r" ok "postinstall: no systemctl without systemd"
echo "$out" | grep -q 'systemd-sysusers' && r=ok || r=no
check "$r" ok "postinstall: sysusers still runs without systemd"
echo "$out" | grep -q 'immurok-cli pair' && r=ok || r=no
check "$r" ok "postinstall: hint still printed without systemd"

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

exit $fail
