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
