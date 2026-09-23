#!/bin/sh
# preinstall — 只做一件事：拒绝装在有 make install（/usr/local）的机器上。
# 这是唯一故意非零退出的 maintainer 脚本。Arch 走不到这里（pacman 先按文件冲突
# 拒装 PAM .so / polkit policy），deb / rpm 靠它干净中止。
main() {
    phase=$(pkg_phase preinstall "$@")
    [ "$phase" = fresh ] || return 0
    if legacy_install_present; then
        cat >&2 <<'EOF'
immurok: a source install (make install -> /usr/local) is present on this system.
  Remove it first, from the checkout you installed from:
      make uninstall
  Pairing data in /var/lib/immurok is kept. After installing this package,
  re-enable the PAM hooks from the PAM page (or: immurok-cli pam install sudo).
EOF
        exit 1
    fi
}
main "$@"
