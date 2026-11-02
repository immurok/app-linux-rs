#!/bin/bash
# immurok-pam-helper 多服务测试（无需 root，用 IMMUROK_PAM_D 注入临时目录）
set -u
HELPER="$(dirname "$0")/immurok-pam-helper"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
export IMMUROK_PAM_D="$TMP"
export IMMUROK_SKIP_MODULE_CHECK=1

fail=0
check() { if [ "$1" = "$2" ]; then echo "PASS: $3"; else echo "FAIL: $3 (got '$1' want '$2')"; fail=1; fi; }

# 准备：sudo 已有 auth 栈、polkit-1 不存在（add 会建模板）
printf 'auth\tsufficient\tpam_unix.so\n' > "$TMP/sudo"

# 一次调用装两个服务
out=$("$HELPER" add sudo polkit-1)
echo "$out"
grep -q 'pam_immurok.so' "$TMP/sudo" && r=ok || r=no
check "$r" ok "sudo 行已插入"
grep -q 'pam_immurok.so' "$TMP/polkit-1" && r=ok || r=no
check "$r" ok "polkit-1 模板已建"

# 幂等：再装一次不应重复
"$HELPER" add sudo >/dev/null
n=$(grep -c 'pam_immurok.so' "$TMP/sudo")
check "$n" 1 "sudo 幂等（仅一行）"

# 非法服务不影响其余：foo 报错，sudo 仍成功
out=$("$HELPER" add foo sudo)
echo "$out" | grep -q 'ERROR:INVALID_SERVICE' && r=ok || r=no
check "$r" ok "非法服务报 ERROR"
echo "$out" | grep -q 'OK:.*(sudo)' && r=ok || r=no
check "$r" ok "非法服务不影响同批 sudo"

# remove 多服务
"$HELPER" remove sudo polkit-1 >/dev/null
grep -q 'pam_immurok.so' "$TMP/sudo" && r=no || r=ok
check "$r" ok "sudo 行已移除"

# 回归：模块不存在时只打一行带后缀错误，不再双重打印
EMPTY_MODDIR="$(mktemp -d)"
out=$(IMMUROK_SKIP_MODULE_CHECK="" IMMUROK_PAM_MODDIR="$EMPTY_MODDIR" "$HELPER" add sudo 2>&1)
count=$(echo "$out" | grep -c 'MODULE_NOT_INSTALLED')
check "$count" 1 "缺模块只输出一行 MODULE_NOT_INSTALLED"
echo "$out" | grep -q 'MODULE_NOT_INSTALLED(sudo)' && r=ok || r=no
check "$r" ok "缺模块错误行含服务后缀"
echo "$out" | grep -qE '^ERROR:MODULE_NOT_INSTALLED$' && r=no || r=ok
check "$r" ok "缺模块不输出裸行（无后缀）"
rmdir "$EMPTY_MODDIR"

# ── 厂商目录（/usr/lib/pam.d）回归 ─────────────────────────────
# Arch 的 gdm 等把 PAM 配置搬到了 /usr/lib/pam.d，/etc/pam.d 下那份只在用户
# 改过时才存在（升级时会被 pacman 留成 .pacsave）。PAM 的查找顺序是 /etc
# 优先、再 /usr/lib，所以 add 必须把厂商那份抄到 /etc 再插行；旧实现直接
# 报 FILE_NOT_FOUND，等于在这些机器上永远装不上登录屏。
VENDOR="$(mktemp -d)"
export IMMUROK_PAM_VENDOR_D="$VENDOR"
printf '#%%PAM-1.0\nauth       include     system-local-login\n' > "$VENDOR/gdm-password"
rm -f "$TMP/gdm-password"

out=$("$HELPER" add gdm-password)
echo "$out"
echo "$out" | grep -q 'ERROR' && r=no || r=ok
check "$r" ok "厂商目录兜底：add 不再报错"
grep -q 'pam_immurok.so' "$TMP/gdm-password" && r=ok || r=no
check "$r" ok "厂商副本已落到 /etc 并插入 immurok 行"
grep -q 'system-local-login' "$TMP/gdm-password" && r=ok || r=no
check "$r" ok "厂商原栈（include system-local-login）被保留"
n=$(grep -c 'pam_immurok.so' "$TMP/gdm-password")
check "$n" 1 "immurok 行只有一行"
head -1 "$TMP/gdm-password" | grep -q '^#%PAM-1.0' && r=ok || r=no
check "$r" ok "首行仍是 #%PAM-1.0"

# 幂等：再 add 一次不重复插行
"$HELPER" add gdm-password >/dev/null
n=$(grep -c 'pam_immurok.so' "$TMP/gdm-password")
check "$n" 1 "厂商副本上 add 幂等"

# remove：我们建的副本整份删掉，让厂商文件重新生效
"$HELPER" remove gdm-password >/dev/null
[ -e "$TMP/gdm-password" ] && r=no || r=ok
check "$r" ok "remove 删掉 immurok 建的副本（厂商文件恢复生效）"

# 用户自己维护的 /etc 文件不能被 remove 删掉，只删行
printf '#%%PAM-1.0\nauth       include     system-local-login\n' > "$TMP/gdm-password"
"$HELPER" add gdm-password >/dev/null
"$HELPER" remove gdm-password >/dev/null
[ -f "$TMP/gdm-password" ] && r=ok || r=no
check "$r" ok "非 immurok 创建的文件 remove 只删行、保留文件"
grep -q 'pam_immurok.so' "$TMP/gdm-password" && r=no || r=ok
check "$r" ok "非 immurok 创建的文件 immurok 行已删"

# 两边都没有文件时仍诚实报 FILE_NOT_FOUND
rm -f "$TMP/gdm-password" "$VENDOR/gdm-password"
out=$("$HELPER" add gdm-password)
echo "$out" | grep -q 'ERROR:FILE_NOT_FOUND(gdm-password)' && r=ok || r=no
check "$r" ok "/etc 与厂商目录都没有时报 FILE_NOT_FOUND"
rm -rf "$VENDOR"
unset IMMUROK_PAM_VENDOR_D

exit $fail
