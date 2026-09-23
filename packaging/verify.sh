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
fail() { echo "FAIL: $*" >&2; exit 1; }
ok()   { echo "PASS: $*"; }

DIST="${1:?usage: verify.sh <dist-dir>}"
DIST=$(cd "$DIST" && pwd) || fail "dist dir not found: $1"
. /etc/os-release

case "$(uname -m)" in
    x86_64)  DEB_ARCH=amd64; RPM_ARCH=x86_64;  PAM_DEB=/usr/lib/x86_64-linux-gnu/security ;;
    aarch64) DEB_ARCH=arm64; RPM_ARCH=aarch64; PAM_DEB=/usr/lib/aarch64-linux-gnu/security ;;
    *) fail "unsupported machine $(uname -m)" ;;
esac

case "$ID" in
    debian|ubuntu)
        PKG=$(ls "$DIST"/immurok_*_"$DEB_ARCH".deb 2>/dev/null | head -1)
        [ -n "$PKG" ] || fail "no matching package for $ID/$(uname -m) in $DIST"
        PAM_DIR=$PAM_DEB
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq --no-install-recommends "$PKG"
        REMOVE="apt-get remove -y -qq immurok"
        ;;
    fedora)
        PKG=$(ls "$DIST"/immurok-*."$RPM_ARCH".rpm 2>/dev/null | head -1)
        [ -n "$PKG" ] || fail "no matching package for $ID/$(uname -m) in $DIST"
        PAM_DIR=/usr/lib64/security
        dnf install -y -q "$PKG"
        REMOVE="dnf remove -y -q immurok"
        ;;
    arch)
        PKG=$(ls "$DIST"/immurok-*-"$RPM_ARCH".pkg.tar.zst 2>/dev/null | head -1)
        [ -n "$PKG" ] || fail "no matching package for $ID/$(uname -m) in $DIST"
        PAM_DIR=/usr/lib/security
        pacman -Syu --noconfirm >/dev/null
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

systemd-analyze --user verify /usr/lib/systemd/user/immurok-session-agent.service 2>/dev/null && ok "user unit verifies" \
    || echo "WARN: systemd-analyze --user verify unavailable in this container (non-fatal)"

$REMOVE
[ -e /usr/bin/immurok-daemon ] && fail "binary still present after remove"
[ -e "$PAM_DIR/pam_immurok.so" ] && fail "PAM module still present after remove"
ok "removed cleanly"
echo "ALL PASS ($ID $(uname -m))"
