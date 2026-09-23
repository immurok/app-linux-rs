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
