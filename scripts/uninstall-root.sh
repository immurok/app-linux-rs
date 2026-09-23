#!/bin/bash
#
# immurok 卸载的 root 部分 —— 同 install-root.sh，一次 sudo 跑完。
#
# 用法: uninstall-root.sh <pam-dir> <bin-dir> [--purge-state]
set -u

PAM_DIR="${1:?usage: uninstall-root.sh <pam-dir> <bin-dir> [--purge-state]}"
BIN_DIR="${2:?}"
PURGE="${3:---keep-state}"

SYSTEMD_SYSTEM_DIR=/etc/systemd/system
TMPFILES_DIR=/etc/tmpfiles.d
DBUS_POLICY_DIR=/etc/dbus-1/system.d
POLKIT_RULES_DIR=/etc/polkit-1/rules.d
POLKIT_DIR=/usr/share/polkit-1/actions
HELPER="$BIN_DIR/immurok-pam-helper"
LEGACY_OVERRIDE_DIRS="$SYSTEMD_SYSTEM_DIR/polkit.service.d $SYSTEMD_SYSTEM_DIR/polkit-agent-helper@.service.d"

[ "$(id -u)" -eq 0 ] || { echo "uninstall-root.sh must run as root"; exit 1; }

step() { echo "  → $*"; }

# PAM 配置先摘掉再删模块，否则中间会有一段「pam.d 引用了不存在的模块」
step "PAM service configs"
[ -x "$HELPER" ] && { "$HELPER" remove sudo; "$HELPER" remove polkit-1; "$HELPER" remove gdm-password; }

step "stop daemon / remove system user"
[ -x "$HELPER" ] && "$HELPER" purge-daemon "$PURGE"

step "system files"
rm -f "$PAM_DIR/pam_immurok.so"
rm -f "$POLKIT_DIR/com.immurok.pam-helper.policy"
rm -f "$TMPFILES_DIR/immurok.conf"
rm -f "$DBUS_POLICY_DIR/immurok.conf"
rm -f "$POLKIT_RULES_DIR/49-immurok.rules"
rm -rf /run/immurok
for d in $LEGACY_OVERRIDE_DIRS; do
    rm -f "$d/immurok.conf"
    rmdir "$d" 2>/dev/null
done
rm -f "$BIN_DIR/immurok-daemon" "$BIN_DIR/immurok-cli" "$BIN_DIR/imk"
rm -f "$BIN_DIR/immurok-auth-dialog" "$BIN_DIR/immurok-pam-helper"
rm -f "$BIN_DIR/immurok-session-agent"
rm -f /etc/systemd/user/immurok-session-agent.service
rm -f "$BIN_DIR/ble-notify-helper.py"
rm -f "$BIN_DIR/immurok-gui"
rm -f /usr/local/share/applications/com.immurok.Settings.desktop
rm -f /usr/local/share/dbus-1/services/com.immurok.Settings.service
systemctl daemon-reload 2>/dev/null

# override 清掉后重启一次 polkit，让沙箱回到发行版默认 —— 同时这就是
# 「卸载后 polkit 仍能起来」那条验收项。
step "restart polkit"
systemctl restart polkit 2>/dev/null
if systemctl is-active --quiet polkit; then
    echo "  ✓ polkit 正常"
else
    echo "  ⚠️  polkit 未运行 —— 检查 $SYSTEMD_SYSTEM_DIR/polkit*.d/"
fi
