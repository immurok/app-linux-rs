#!/bin/bash
# immurok-pam-helper migrate-daemon / purge-daemon 测试
#
# 全程无需 root：用 IMMUROK_* 钩子把系统用户创建、systemctl 与家目录都
# 换成临时替身，只验迁移逻辑本身（拷贝、幂等、软链拒绝、原件保留）。
set -u
HELPER="$(dirname "$0")/immurok-pam-helper"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

export IMMUROK_SKIP_USERADD=1
export IMMUROK_SKIP_SYSTEMCTL=1
export IMMUROK_HOME="$TMP/home"
export IMMUROK_STATE_DIR="$TMP/state"
export IMMUROK_SYSTEM_UNIT="$TMP/immurok-daemon.service"
export IMMUROK_LEGACY_OVERRIDES="$TMP/polkit.d/immurok.conf $TMP/helper.d/immurok.conf"

fail=0
check() { if [ "$1" = "$2" ]; then echo "PASS: $3"; else echo "FAIL: $3 (got '$1' want '$2')"; fail=1; fi; }

ME="$(id -un)"
mkdir -p "$IMMUROK_HOME/.immurok" "$TMP/polkit.d" "$TMP/helper.d"
echo '{"shared_key":"deadbeef"}' > "$IMMUROK_HOME/.immurok/pairing.json"
echo '{"unlock_sudo":true}'      > "$IMMUROK_HOME/.immurok/settings.json"
ln -s /etc/hostname "$IMMUROK_HOME/.immurok/ssh_keys.json"   # 软链必须被拒
touch "$IMMUROK_SYSTEM_UNIT"
printf '[Service]\nBindPaths=/run/user\n' > "$TMP/polkit.d/immurok.conf"
printf '[Service]\nProtectHome=no\n'      > "$TMP/helper.d/immurok.conf"

out=$("$HELPER" migrate-daemon "$ME")
echo "$out"

check "$(cat "$IMMUROK_STATE_DIR/pairing.json")" '{"shared_key":"deadbeef"}' "pairing.json 内容搬对"
check "$(stat -c '%a' "$IMMUROK_STATE_DIR/pairing.json")" 600 "pairing.json 落 0600"
check "$(stat -c '%a' "$IMMUROK_STATE_DIR")" 700 "state 目录 0700"
[ -f "$IMMUROK_STATE_DIR/settings.json" ] && r=ok || r=no
check "$r" ok "settings.json 一并搬走"

# 原件改名保留 —— 迁移出问题时用户能自己搬回去
[ -f "$IMMUROK_HOME/.immurok/pairing.json.migrated" ] && r=ok || r=no
check "$r" ok "原件改名为 .migrated"
[ -e "$IMMUROK_HOME/.immurok/pairing.json" ] && r=no || r=ok
check "$r" ok "原路径已让开"

# 软链：拒绝，且绝不落盘 —— 否则 root 会被诱导把任意可读文件搬进 state
echo "$out" | grep -q 'ERROR:SYMLINK_REFUSED(ssh_keys.json)' && r=ok || r=no
check "$r" ok "软链源被拒"
[ -e "$IMMUROK_STATE_DIR/ssh_keys.json" ] && r=no || r=ok
check "$r" ok "软链内容没有落进 state"

echo "$out" | grep -q 'OK:ABSENT(key_names.json)' && r=ok || r=no
check "$r" ok "缺失文件报 ABSENT 而不是失败"
echo "$out" | grep -q 'OK:LEGACY_OVERRIDES_REMOVED' && r=ok || r=no
check "$r" ok "旧 polkit override 被删"
[ -e "$TMP/polkit.d/immurok.conf" ] && r=no || r=ok
check "$r" ok "override 文件真的没了"

# 幂等：再跑一次不覆盖已迁移的数据
echo '{"shared_key":"REPLACED"}' > "$IMMUROK_HOME/.immurok/pairing.json"
out2=$("$HELPER" migrate-daemon "$ME")
echo "$out2" | grep -q 'OK:ALREADY_MIGRATED(pairing.json)' && r=ok || r=no
check "$r" ok "二次迁移报 ALREADY_MIGRATED"
check "$(cat "$IMMUROK_STATE_DIR/pairing.json")" '{"shared_key":"deadbeef"}' "已迁移的数据不被覆盖"

# 单元没装：必须报错而不是假装成功
rm -f "$IMMUROK_SYSTEM_UNIT"
out3=$("$HELPER" migrate-daemon "$ME"); rc=$?
echo "$out3" | grep -q 'ERROR:UNIT_NOT_INSTALLED' && r=ok || r=no
check "$r" ok "缺单元文件报 UNIT_NOT_INSTALLED"
check "$rc" 1 "缺单元文件退出码非零"

# purge：默认保留 state，显式才删
"$HELPER" purge-daemon --keep-state >/dev/null
[ -d "$IMMUROK_STATE_DIR" ] && r=ok || r=no
check "$r" ok "purge 默认保留 state"
"$HELPER" purge-daemon --purge-state >/dev/null
[ -d "$IMMUROK_STATE_DIR" ] && r=no || r=ok
check "$r" ok "--purge-state 才真删"

exit $fail
