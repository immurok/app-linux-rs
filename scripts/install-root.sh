#!/bin/bash
#
# immurok 安装的 root 部分 —— 一次 sudo 跑完。
#
# 为什么不在 Makefile 里散着写十几条 sudo：装完新 pam_immurok.so 之后、
# 系统 daemon 起来之前，指纹 sudo 必然是断的（新模块连 /run/immurok，那时
# 还没人监听）。如果每条 recipe 各自 sudo，就会在中途掉进密码提示；而在没
# 有 tty 的环境里 sudo 的时间戳按 ppid 记，make 每条 recipe 都是新父进程，
# 缓存根本不生效。收敛成一次调用，认证只发生一次，且发生在换模块之前。
#
# 用法: install-root.sh <invoking-user> <src-dir> <pam-dir> <bin-dir>
set -u

USER_NAME="${1:?usage: install-root.sh <user> <src-dir> <pam-dir> <bin-dir>}"
SRC="${2:?}"
PAM_DIR="${3:?}"
BIN_DIR="${4:?}"

SYSTEMD_SYSTEM_DIR=/etc/systemd/system
TMPFILES_DIR=/etc/tmpfiles.d
DBUS_POLICY_DIR=/etc/dbus-1/system.d
POLKIT_RULES_DIR=/etc/polkit-1/rules.d
POLKIT_DIR=/usr/share/polkit-1/actions
HELPER="$BIN_DIR/immurok-pam-helper"

[ "$(id -u)" -eq 0 ] || { echo "install-root.sh must run as root"; exit 1; }
cd "$SRC" || exit 1

step() { echo "  → $*"; }

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

step "binaries → $BIN_DIR"
install -Dm755 target/release/immurok-daemon "$BIN_DIR/immurok-daemon"
install -Dm755 target/release/immurok-cli    "$BIN_DIR/immurok-cli"
install -Dm755 target/release/imk            "$BIN_DIR/imk"
install -Dm755 scripts/immurok-auth-dialog   "$BIN_DIR/immurok-auth-dialog"
install -Dm755 scripts/immurok-pam-helper    "$HELPER"
install -Dm755 scripts/ble-notify-helper.py  "$BIN_DIR/ble-notify-helper.py"
install -Dm755 target/release/immurok-session-agent "$BIN_DIR/immurok-session-agent"

# GUI 是可选构建产物（缺 GTK 开发头时 Makefile 会跳过）。
if [ -x target/release/immurok-gui ]; then
    mkdir -p /usr/local/share/applications /usr/local/share/dbus-1/services
    install -Dm755 target/release/immurok-gui "$BIN_DIR/immurok-gui"
    install -Dm644 packaging/com.immurok.Settings.desktop /usr/local/share/applications/com.immurok.Settings.desktop
    sed "s|/usr/bin/|$BIN_DIR/|" packaging/com.immurok.Settings.service \
        > /usr/local/share/dbus-1/services/com.immurok.Settings.service
    chmod 644 /usr/local/share/dbus-1/services/com.immurok.Settings.service
    update-desktop-database /usr/local/share/applications 2>/dev/null || true
fi

step "system integration files"
sed "s|@HELPER_PATH@|$HELPER|" scripts/com.immurok.pam-helper.policy.in \
    > "$POLKIT_DIR/com.immurok.pam-helper.policy"
chmod 644 "$POLKIT_DIR/com.immurok.pam-helper.policy"
# unit 文件以 /usr/bin 为规范路径（发行版包用），源码安装替换成 $BIN_DIR
sed "s|/usr/bin/|$BIN_DIR/|" packaging/immurok-daemon.service > "$SYSTEMD_SYSTEM_DIR/immurok-daemon.service"
chmod 644 "$SYSTEMD_SYSTEM_DIR/immurok-daemon.service"
install -Dm644 packaging/tmpfiles.d/immurok.conf "$TMPFILES_DIR/immurok.conf"
install -Dm644 packaging/dbus/immurok.conf "$DBUS_POLICY_DIR/immurok.conf"
install -Dm644 packaging/polkit/49-immurok.rules "$POLKIT_RULES_DIR/49-immurok.rules"
# 用户级单元装到 /etc/systemd/user/，各用户自己 enable（见 Makefile 的非 root 段）
sed "s|/usr/bin/|$BIN_DIR/|" packaging/immurok-session-agent.service > /etc/systemd/user/immurok-session-agent.service
chmod 644 /etc/systemd/user/immurok-session-agent.service
# BlueZ 的 policy 是 dbus 守护进程读的，reload 即可，不用重启 bluetoothd
systemctl reload dbus 2>/dev/null || systemctl reload dbus-broker 2>/dev/null || true

# PAM 模块换在最后一刻之前，且必须用临时文件 + 原子 rename：跑着这条命令
# 的 sudo 自己 mmap 着旧的 .so，原地覆盖会打烂它的代码页（pam_end 里
# dlclose 时 SIGSEGV）。
step "PAM module → $PAM_DIR"
install -Dm755 pam/pam_immurok.so "$PAM_DIR/pam_immurok.so.new"
mv -f "$PAM_DIR/pam_immurok.so.new" "$PAM_DIR/pam_immurok.so"

step "PAM service configs"
"$HELPER" add sudo
"$HELPER" add polkit-1
"$HELPER" add gdm-password

# 建系统用户、搬数据、停旧的用户级 daemon、起系统 daemon、删旧 polkit
# override —— 一步做完，中间不会出现两个 daemon 抢 BLE 的窗口。
# 短命的中间版本在这里放过一个全局可写的 marker 目录；agent 归类改成由
# AGENT_APPROVE 在内存里登记之后它就没用了，而 RuntimeDirectoryPreserve=yes
# 会一直把它留着。
rm -rf /run/immurok/markers

step "migrate to isolated daemon"
"$HELPER" migrate-daemon "$USER_NAME" || exit 1

echo "  ✓ root steps done"
