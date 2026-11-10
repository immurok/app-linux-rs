#!/bin/sh
# packaging/scripts/lib.sh — deb / rpm / archlinux 三种包共用的 maintainer 脚本函数。
#
# stage.sh 把它和 preinstall / postinstall / preremove / postremove 的正文拼成
# 独立文件；包里没有这个文件可 source。必须是 POSIX sh：rpm 用 /bin/sh 跑
# scriptlet，Debian 的 /bin/sh 是 dash；Arch 把内容原样塞进 .INSTALL 的函数体。
#
# 测试钩子（scripts/test-pkg-scripts.sh 用）：
#   IMMUROK_PKG_ROOT            路径前缀（legacy 检测、chown 目标、/run/systemd 探测）
#   IMMUROK_PKG_DRYRUN=1        run() 只打印 "RUN: …"，探测函数改读下面的假数据
#   IMMUROK_PKG_FORCE_SYSTEMD   1/0 覆盖 have_systemd
#   IMMUROK_PKG_FAKE_USERS      dryrun 下 logged_in_users 的返回（空格分隔）
#   IMMUROK_PKG_FAKE_GROUPS     dryrun 下 pkg_has_group 查的组列表（空格分隔）
#   IMMUROK_PKG_FAKE_GUI        1/0 覆盖 gui_service_running（dryrun 下默认 0）

PKG_ROOT="${IMMUROK_PKG_ROOT:-}"

# 归一化各格式传进来的参数 → fresh | upgrade | remove | purge
#   deb : preinst  install [old] | upgrade old
#         postinst configure [old]        （全新安装时 old 是空串）
#         prerm    remove | upgrade new | deconfigure …
#         postrm   remove | purge | upgrade new | …
#   rpm : $1 = 这次操作之后剩余的实例数
#         %pre/%post   1 = install, 2 = upgrade
#         %preun/%postun 0 = remove, 1 = upgrade
#   arch: post_install new | post_upgrade new old | pre_remove old | post_remove old
# 判别顺序：deb 动词 → 纯数字（rpm）→ 其余按 arch。
pkg_phase() {
    script="$1"; shift
    a1="${1:-}"; a2="${2:-}"
    case "$a1" in
        install)   echo fresh; return ;;
        configure) if [ -n "$a2" ]; then echo upgrade; else echo fresh; fi; return ;;
        upgrade|deconfigure|failed-upgrade|abort-*|disappear) echo upgrade; return ;;
        remove)    echo remove; return ;;
        purge)     echo purge; return ;;
    esac
    case "$a1" in
        ''|*[!0-9]*) ;;   # 不是纯数字 → arch
        *)
            case "$script" in
                preinstall|postinstall) if [ "$a1" -ge 2 ]; then echo upgrade; else echo fresh; fi ;;
                *)                      if [ "$a1" -ge 1 ]; then echo upgrade; else echo remove; fi ;;
            esac
            return ;;
    esac
    case "$script" in
        preinstall|postinstall) if [ -n "$a2" ]; then echo upgrade; else echo fresh; fi ;;
        *)                      echo remove ;;
    esac
}

# systemd 在跑才动 systemctl；chroot / 容器（无 /run/systemd/system）全部跳过。
have_systemd() {
    case "${IMMUROK_PKG_FORCE_SYSTEMD:-}" in
        1) return 0 ;;
        0) return 1 ;;
    esac
    [ -d "$PKG_ROOT/run/systemd/system" ] && command -v systemctl >/dev/null 2>&1
}

# best-effort 执行：失败只 warn。postinstall 永远不能非零退出，否则 deb 会把
# 包留在"半配置"状态。
run() {
    if [ "${IMMUROK_PKG_DRYRUN:-}" = "1" ]; then
        echo "RUN: $*"
        return 0
    fi
    "$@" || echo "immurok: warning: '$*' failed" >&2
    return 0
}

# BlueZ policy 是 dbus 守护进程读的，reload 即可；Arch / Fedora 是 dbus-broker，
# 它的单元别名成 dbus.service，两条都试。
reload_dbus() {
    if [ "${IMMUROK_PKG_DRYRUN:-}" = "1" ]; then
        echo "RUN: systemctl reload dbus"
        return 0
    fi
    systemctl reload dbus.service >/dev/null 2>&1 \
        || systemctl reload dbus-broker.service >/dev/null 2>&1 \
        || true
}

# 当前有会话的普通用户（uid >= 1000），用于装完立刻把 session-agent 拉起来。
logged_in_users() {
    if [ "${IMMUROK_PKG_DRYRUN:-}" = "1" ]; then
        for u in ${IMMUROK_PKG_FAKE_USERS:-}; do echo "$u"; done
        return 0
    fi
    loginctl list-users --no-legend 2>/dev/null | awk '$1 >= 1000 { print $2 }'
}

pkg_has_group() {
    if [ "${IMMUROK_PKG_DRYRUN:-}" = "1" ]; then
        for g in ${IMMUROK_PKG_FAKE_GROUPS:-}; do [ "$g" = "$1" ] && return 0; done
        return 1
    fi
    getent group "$1" >/dev/null 2>&1
}

# 常驻的 immurok-gui（--gapplication-service）在不在跑。
gui_service_running() {
    case "${IMMUROK_PKG_FAKE_GUI:-}" in
        1) return 0 ;;
        0) return 1 ;;
    esac
    # dryrun 下不读宿主机进程表 —— 否则在开着 immurok-gui 的机器上跑测试会
    # 多出一条 pkill，序列断言随宿主机状态飘（CI 容器里没 GUI 所以一直是绿的）。
    [ "${IMMUROK_PKG_DRYRUN:-}" = "1" ] && return 1
    pgrep -f 'immurok-gui --gapplication-service' >/dev/null 2>&1
}

# 自愈存量机器上的坏 PAM 配置：0.9.1–0.9.3 在 Arch 上写出的 polkit-1 里
# include 指向并不存在的 password-auth，auth 和 account 两条栈一起打不开，
# polkit 认证会死循环。
#
# 只碰已经带了 immurok 行的服务 —— 那是用户此前主动开过的，重建它不等于替他
# 开新功能，PAM 仍然是显式 opt-in。helper 的 add 对健康配置是空操作
# （ALREADY_PRESENT），所以这一步每次升级都跑也无妨。
#
# 为什么非得在包脚本里做：坏配置里 pam_immurok 行是在的，helper 只会回
# ALREADY_PRESENT、Rust 侧的 service_status 也照样算「已安装」，用户不会想到
# 去点修复 —— 光升级到修好的版本治不好存量机器。
repair_pam_configs() {
    helper="$PKG_ROOT/usr/bin/immurok-pam-helper"
    [ -x "$helper" ] || return 0
    for svc in sudo polkit-1; do
        f="$PKG_ROOT/etc/pam.d/$svc"
        [ -f "$f" ] || continue
        grep -q 'pam_immurok\.so' "$f" 2>/dev/null || continue
        run "$helper" add "$svc"
    done
    return 0
}

# make install 装到 /usr/local 的痕迹。两种装法同机会互相遮（PATH、unit 覆盖、
# PAM .so 与 polkit policy 同路径），所以 preinstall 见到就中止。
legacy_install_present() {
    [ -e "$PKG_ROOT/usr/local/bin/immurok-daemon" ] \
        || [ -e "$PKG_ROOT/etc/systemd/system/immurok-daemon.service" ]
}
