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
