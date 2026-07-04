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

exit $fail
