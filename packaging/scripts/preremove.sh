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
