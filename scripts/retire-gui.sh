#!/bin/bash
#
# 让当前用户的常驻 immurok-gui（`--gapplication-service` 实例）退出。
#
# 为什么要有这一步：GApplication 单实例转发会把之后每一次 `immurok-gui`、
# 热键、应用菜单都交给仍在跑的那个进程。make install 换了磁盘上的二进制，
# 常驻进程却还是旧代码——新功能看不到、修掉的 bug 还在，而且没有任何提示。
# 退出之后下次启动由 D-Bus 自动拉起新二进制。
#
# 先走 App 自己的 `retire` action（固件升级中会拒绝，这是刻意的），没有这个
# action 的老版本才 pkill。
set -u

NAME=com.immurok.Settings
PATH_OBJ=/com/immurok/Settings

owned() {
    gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
        --method org.freedesktop.DBus.NameHasOwner "$NAME" 2>/dev/null | grep -q true
}
wait_gone() {
    for _ in $(seq 1 20); do
        owned || return 0
        sleep 0.2
    done
    return 1
}

command -v gdbus >/dev/null 2>&1 || exit 0
owned || exit 0

echo "  → 常驻 immurok-gui 还在跑旧二进制，请它退出"
if gdbus call --session --dest "$NAME" --object-path "$PATH_OBJ" \
        --method org.gtk.Actions.List 2>/dev/null | grep -q "'retire'"; then
    gdbus call --session --dest "$NAME" --object-path "$PATH_OBJ" \
        --method org.freedesktop.Application.ActivateAction retire '[]' '{}' >/dev/null 2>&1
    if wait_gone; then
        exit 0
    fi
    # 有 retire 却没退：固件升级中，App 自己拒绝了。别杀它。
    echo "  ⚠️  immurok-gui 拒绝退出（固件升级中？）——结束后请运行: pkill -f 'immurok-gui --gapplication-service'"
    exit 0
fi

# 老版本没有 retire action，只能结束进程。
pkill -u "$(id -u)" -f 'immurok-gui --gapplication-service' 2>/dev/null
if ! wait_gone; then
    echo "  ⚠️  immurok-gui 仍在运行，请手动: pkill -f 'immurok-gui --gapplication-service'"
fi
exit 0
