#!/bin/sh
# postinstall — 全新安装与升级走同一条幂等路径。每步 best-effort，永不非零退出。
main() {
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
            # restart 而不是 start：也能在未运行时启动，且升级时替换旧二进制。
            run systemctl --user --machine="$u@" restart immurok-session-agent.service
        done
    fi

    cat <<'EOF'
immurok installed.
  1. Pair your device: open "immurok" from the app menu, or run: immurok-cli pair
  2. Enable fingerprint for sudo / polkit / login on the PAM page (or: immurok-cli pam install sudo)
  Daemon status: systemctl status immurok-daemon
EOF
    return 0
}
main "$@"
