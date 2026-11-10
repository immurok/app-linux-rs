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

# ── polkit-1 模板的 include 目标（Arch 死循环回归）────────────────
# 旧实现只认 password-auth / common-auth，探测不到就 fallback 回
# password-auth —— 而 Arch 根本没有这个文件（它叫 system-auth）。于是模板
# 四条 include 全部指向不存在的文件，PAM 报 "unable to open config"，
# 连 account 栈也打不开：指纹明明 Approved，polkit-agent-helper 的
# pam_acct_mgmt() 仍然失败，polkit 认成认证失败后反复弹框 → 死循环。
# 这组用例的核心不变量：模板里写的 include 目标必须真实存在。
IT_TMP="$(mktemp -d)"
IT_VENDOR="$(mktemp -d)"
export IMMUROK_PAM_D="$IT_TMP"
export IMMUROK_PAM_VENDOR_D="$IT_VENDOR"

include_target_of() { sed -n 's/^auth[[:space:]]*include[[:space:]]*//p' "$1" | head -1; }

# Arch：只有 system-auth，且厂商目录没有 polkit-1 → 模板必须用 system-auth
rm -f "$IT_TMP"/* "$IT_VENDOR"/*
printf 'auth\trequired\tpam_unix.so\n' > "$IT_TMP/system-auth"
out=$("$HELPER" add polkit-1)
check "$(include_target_of "$IT_TMP/polkit-1")" system-auth "Arch：模板 include system-auth"
[ -f "$IT_TMP/$(include_target_of "$IT_TMP/polkit-1")" ] && r=ok || r=no
check "$r" ok "Arch：include 目标文件真实存在"
n=$(grep -c 'include' "$IT_TMP/polkit-1")
check "$n" 4 "Arch：auth/account/password/session 四条 include 都在"
grep -q 'password-auth' "$IT_TMP/polkit-1" && r=no || r=ok
check "$r" ok "Arch：模板不含 password-auth"

# Fedora：password-auth 与 system-auth 并存 → 保持已验证的 password-auth
rm -f "$IT_TMP"/*
printf 'auth\trequired\tpam_unix.so\n' > "$IT_TMP/password-auth"
printf 'auth\trequired\tpam_unix.so\n' > "$IT_TMP/system-auth"
"$HELPER" add polkit-1 >/dev/null
check "$(include_target_of "$IT_TMP/polkit-1")" password-auth "Fedora：仍优先 password-auth"

# Debian/Ubuntu：只有 common-auth
rm -f "$IT_TMP"/*
printf 'auth\trequired\tpam_unix.so\n' > "$IT_TMP/common-auth"
"$HELPER" add polkit-1 >/dev/null
check "$(include_target_of "$IT_TMP/polkit-1")" common-auth "Debian：模板 include common-auth"

# 三个都探测不到 → 必须报错，且绝不留下一份坏配置
rm -f "$IT_TMP"/*
out=$("$HELPER" add polkit-1)
echo "$out" | grep -q 'ERROR:NO_PAM_INCLUDE_TARGET(polkit-1)' && r=ok || r=no
check "$r" ok "探测不到 include 目标时报 NO_PAM_INCLUDE_TARGET"
[ -e "$IT_TMP/polkit-1" ] && r=no || r=ok
check "$r" ok "探测失败不写出坏配置文件"

# 厂商目录有 polkit-1（Arch 的 polkit 包就自带）→ 抄厂商那份，别猜模板。
# 发行版自己写的 include 目标一定是对的。
rm -f "$IT_TMP"/* "$IT_VENDOR"/*
printf 'auth\trequired\tpam_unix.so\n' > "$IT_TMP/system-auth"
printf '#%%PAM-1.0\nauth       include      system-auth\naccount    include      system-auth\n' > "$IT_VENDOR/polkit-1"
out=$("$HELPER" add polkit-1)
echo "$out" | grep -q 'OK:ADDED_FROM_VENDOR(polkit-1)' && r=ok || r=no
check "$r" ok "厂商有 polkit-1 时抄副本而非套模板"
check "$(include_target_of "$IT_TMP/polkit-1")" system-auth "厂商副本保留其 include system-auth"
grep -q 'pam_immurok.so.*timeout=10' "$IT_TMP/polkit-1" && r=ok || r=no
check "$r" ok "厂商副本上 polkit-1 仍带 timeout=10"

# remove 把我们抄的那份整份删掉，让厂商文件重新生效
"$HELPER" remove polkit-1 >/dev/null
[ -e "$IT_TMP/polkit-1" ] && r=no || r=ok
check "$r" ok "remove 删掉 polkit-1 厂商副本"

rm -rf "$IT_TMP" "$IT_VENDOR"
unset IMMUROK_PAM_VENDOR_D

# ── 坏配置自愈（0.9.1–0.9.3 存量机器）────────────────────────────
# 0.9.1–0.9.3 在 Arch 上留下的 polkit-1 里 include 指向并不存在的
# password-auth，但 pam_immurok 行是在的 —— 于是 add 只回 ALREADY_PRESENT、
# Rust 侧也照样算「已安装」，光升级到修好的版本治不好。add 必须认出这种
# 自己造的坏配置并整份重建。
SH_TMP="$(mktemp -d)"
SH_VENDOR="$(mktemp -d)"
export IMMUROK_PAM_D="$SH_TMP"
export IMMUROK_PAM_VENDOR_D="$SH_VENDOR"

# 复刻 0.9.2 留下的坏文件
write_broken_polkit() {
    cat > "$SH_TMP/polkit-1" <<'BROKEN'
#%PAM-1.0
# Created by immurok
auth        sufficient    pam_immurok.so timeout=10
auth        include       password-auth
account     include       password-auth
password    include       password-auth
session     include       password-auth
BROKEN
}

# (1) 厂商有 polkit-1 → 重建成厂商副本
rm -f "$SH_TMP"/* "$SH_VENDOR"/*
printf 'auth\trequired\tpam_unix.so\n' > "$SH_TMP/system-auth"
printf '#%%PAM-1.0\nauth       include      system-auth\naccount    include      system-auth\n' > "$SH_VENDOR/polkit-1"
write_broken_polkit
out=$("$HELPER" add polkit-1)
echo "$out" | grep -q 'OK:REPAIRED(polkit-1)' && r=ok || r=no
check "$r" ok "自愈：坏配置被重建，报 REPAIRED"
check "$(include_target_of "$SH_TMP/polkit-1")" system-auth "自愈：include 改回 system-auth"
grep -q 'password-auth' "$SH_TMP/polkit-1" && r=no || r=ok
check "$r" ok "自愈：坏的 password-auth 已清掉"
n=$(grep -c 'pam_immurok\.so' "$SH_TMP/polkit-1")
check "$n" 1 "自愈：pam_immurok 行仍只有一行"
grep -q 'pam_immurok.so.*timeout=10' "$SH_TMP/polkit-1" && r=ok || r=no
check "$r" ok "自愈：timeout=10 保住"

# (2) 厂商没有 polkit-1 → 用模板重建
rm -f "$SH_TMP"/* "$SH_VENDOR"/*
printf 'auth\trequired\tpam_unix.so\n' > "$SH_TMP/system-auth"
write_broken_polkit
out=$("$HELPER" add polkit-1)
echo "$out" | grep -q 'OK:REPAIRED(polkit-1)' && r=ok || r=no
check "$r" ok "自愈：无厂商文件时用模板重建"
check "$(include_target_of "$SH_TMP/polkit-1")" system-auth "自愈：模板重建后 include system-auth"

# (3) 健康配置一个字都不许动
rm -f "$SH_TMP"/* "$SH_VENDOR"/*
printf 'auth\trequired\tpam_unix.so\n' > "$SH_TMP/system-auth"
"$HELPER" add polkit-1 >/dev/null          # 先建一份健康的
before=$(cat "$SH_TMP/polkit-1")
out=$("$HELPER" add polkit-1)
echo "$out" | grep -q 'OK:ALREADY_PRESENT(polkit-1)' && r=ok || r=no
check "$r" ok "健康配置仍回 ALREADY_PRESENT（不误判成坏）"
[ "$before" = "$(cat "$SH_TMP/polkit-1")" ] && r=ok || r=no
check "$r" ok "健康配置内容未被改动"

# (4) include 目标只在厂商目录里 → 不算坏（PAM 查找会落到 /usr/lib/pam.d）
rm -f "$SH_TMP"/* "$SH_VENDOR"/*
printf 'auth\trequired\tpam_unix.so\n' > "$SH_VENDOR/system-auth"
printf '#%%PAM-1.0\n# Created by immurok\nauth        sufficient    pam_immurok.so timeout=10\nauth        include       system-auth\n' > "$SH_TMP/polkit-1"
out=$("$HELPER" add polkit-1)
echo "$out" | grep -q 'OK:ALREADY_PRESENT(polkit-1)' && r=ok || r=no
check "$r" ok "include 目标只在厂商目录里不算坏"

# (5) 用户自己维护的文件（无 immurok 标记）即使 include 坏了也不重建
rm -f "$SH_TMP"/* "$SH_VENDOR"/*
printf 'auth\trequired\tpam_unix.so\n' > "$SH_TMP/system-auth"
printf '#%%PAM-1.0\nauth        sufficient    pam_immurok.so\nauth        include       nonexistent-stack\n' > "$SH_TMP/polkit-1"
before=$(cat "$SH_TMP/polkit-1")
out=$("$HELPER" add polkit-1)
echo "$out" | grep -q 'OK:ALREADY_PRESENT(polkit-1)' && r=ok || r=no
check "$r" ok "用户自己的文件不重建（只回 ALREADY_PRESENT）"
[ "$before" = "$(cat "$SH_TMP/polkit-1")" ] && r=ok || r=no
check "$r" ok "用户自己的文件内容未被动"

# (6) 重建不出来时不许删掉坏文件（删空会落到 /etc/pam.d/other，更糟）
rm -f "$SH_TMP"/* "$SH_VENDOR"/*
write_broken_polkit
before=$(cat "$SH_TMP/polkit-1")
out=$("$HELPER" add polkit-1)
echo "$out" | grep -q 'ERROR:CANNOT_REPAIR(polkit-1)' && r=ok || r=no
check "$r" ok "重建不出来时报 CANNOT_REPAIR"
[ -f "$SH_TMP/polkit-1" ] && r=ok || r=no
check "$r" ok "重建不出来时坏文件保留（不留空配置）"
[ "$before" = "$(cat "$SH_TMP/polkit-1")" ] && r=ok || r=no
check "$r" ok "重建不出来时坏文件内容未动"

# (7) Debian 的 @include 语法也要能认出坏目标
rm -f "$SH_TMP"/* "$SH_VENDOR"/*
printf 'auth\trequired\tpam_unix.so\n' > "$SH_TMP/common-auth"
printf '#%%PAM-1.0\n# Created by immurok\nauth        sufficient    pam_immurok.so timeout=10\n@include common-auth-gone\n' > "$SH_TMP/polkit-1"
out=$("$HELPER" add polkit-1)
echo "$out" | grep -q 'OK:REPAIRED(polkit-1)' && r=ok || r=no
check "$r" ok "@include 坏目标也能自愈"
check "$(include_target_of "$SH_TMP/polkit-1")" common-auth "@include 自愈后 include common-auth"

# (8) 泛化到其它服务：sudo 的厂商副本坏了也能自愈
rm -f "$SH_TMP"/* "$SH_VENDOR"/*
printf 'auth\trequired\tpam_unix.so\n' > "$SH_TMP/system-auth"
printf '#%%PAM-1.0\nauth       include      system-auth\n' > "$SH_VENDOR/sudo"
printf '#%%PAM-1.0\nauth        sufficient    pam_immurok.so\nauth       include      gone-auth\n# %s: copy of x, so the immurok auth line could be added.\n' "Created by immurok" > "$SH_TMP/sudo"
out=$("$HELPER" add sudo)
echo "$out" | grep -q 'OK:REPAIRED(sudo)' && r=ok || r=no
check "$r" ok "sudo 的坏厂商副本也能自愈"
check "$(include_target_of "$SH_TMP/sudo")" system-auth "sudo 自愈后 include system-auth"
grep -q 'pam_immurok.so timeout=10' "$SH_TMP/sudo" && r=no || r=ok
check "$r" ok "sudo 不带 timeout=10（只有 polkit-1 带）"

rm -rf "$SH_TMP" "$SH_VENDOR"
unset IMMUROK_PAM_VENDOR_D

exit $fail
