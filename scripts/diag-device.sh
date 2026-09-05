#!/bin/bash
# 设备真实状态诊断 —— 不看本地推断，只看设备自己怎么回
#
# 存在的理由：`immurok-cli status` / `slot status` 的每一行都是按本地
# pairing.json 推断出来的。当设备实际坐在另一个（空的）host 槽上时，这些
# 展示会全部说「Connected / Paired: Yes / active (this computer)」，而所有
# 认证静默失败。唯一的真相在 BLE 的原始响应里。
#
# 做法：一边跟 daemon 日志，一边发几条无副作用的命令，把设备回的原始字节
# 抓出来判读。
#
# 用法: bash scripts/diag-device.sh
set -u

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
c_ok=$'\033[32m'; c_bad=$'\033[31m'; c_warn=$'\033[33m'; c_dim=$'\033[2m'; c_off=$'\033[0m'

echo "=== 1. 主机这边怎么说（全部是本地推断）==="
immurok-cli status 2>&1 | sed 's/^/  /'
echo
immurok-cli slot status 2>&1 | sed 's/^/  /'

echo
echo "=== 2. 设备那边怎么回（原始字节）==="
# 先挂上日志流，再发命令，避免漏掉
timeout 12 immurok-cli logs > "$TMP/log" 2>/dev/null &
LOGPID=$!
sleep 1.5
BASE=$(wc -l < "$TMP/log")

# 三条无副作用的探针：FP 列表（需认证）、槽状态（pre-pair 白名单内）、设备信息
immurok-cli fp list   >/dev/null 2>&1
sleep 0.8
immurok-cli slot status >/dev/null 2>&1
sleep 0.8
immurok-cli info      >/dev/null 2>&1
sleep 1.5
kill $LOGPID 2>/dev/null; wait $LOGPID 2>/dev/null

FRAMES=$(tail -n +$((BASE+1)) "$TMP/log" | grep -E 'BLE TX: cmd=|BLE RX: \[')
if [ -z "$FRAMES" ]; then
    echo "  ${c_warn}没有抓到任何 BLE 帧${c_off} —— 设备没连上，或 daemon 没在跑"
    exit 2
fi
echo "$FRAMES" | sed -E 's/^[0-9T:.-]+Z +INFO +/  /'

echo
echo "=== 3. 判读 ==="

verdict=0

# (a) NOT_PAIRED：响应第二字节 0xf2
if echo "$FRAMES" | grep -qE 'BLE RX: \[[0-9a-f]{2}f2'; then
    echo "  ${c_bad}设备回 0xF2 (SEC_ERR_NOT_PAIRED)${c_off}"
    echo "    含义：设备当前呈现的 host 槽是空的，固件的 pre-pair 白名单会拒绝一切"
    echo "    白名单外的命令（CHALLENGE / FP_LIST / KEY_*）全部失败，"
    echo "    白名单内的（SLOT_STATUS）却正常 —— 所以状态展示看起来一切正常。"
    echo "    ${c_warn}解法：触摸设备上的切换指纹（slot 5）切回本机，或 immurok-cli pair 重新配对${c_off}"
    verdict=1
else
    echo "  ${c_ok}没有 0xF2${c_off} —— 设备认为本机是已配对的那个槽"
fi

# 这里曾有一段「响应错位」判读，判据是「响应首字节 == 刚发的命令码」。
# 那个前提是错的，固件里格式因命令而异：
#   FP_LIST     rspBuf[0]=IMMUROK_RSP_OK    → 首字节是状态 0x00
#   SLOT_STATUS rspBuf[0]=IMMUROK_CMD_SLOT_STATUS → 回显命令码
#   CHALLENGE   rspBuf[0]=IMMUROK_CMD_CHALLENGE   → 回显命令码
# 于是它对每一次 FP_LIST 都必然误报。健康设备上实测报「已确认异常」——
# 一个会喊狼来了的检测器比没有更糟，删掉。上面的原始帧才是真正有用的东西，
# 判读需要逐命令的格式知识，不存在统一规则。

# (c) 主机说已配对、设备说没配对 —— 最容易骗人的组合
if immurok-cli status 2>/dev/null | grep -qE '^Paired:.*Yes' \
   && echo "$FRAMES" | grep -qE 'BLE RX: \[[0-9a-f]{2}f2'; then
    echo "  ${c_bad}状态展示与设备实况矛盾${c_off}：本机显示 Paired: Yes，设备却说未配对。"
    echo "    这正是「看起来一切正常、认证却全部静默失败」的来源。"
fi

echo
[ "$verdict" = "0" ] && echo "  ${c_ok}设备侧未见异常${c_off}" || echo "  ${c_bad}已确认异常，见上${c_off}"
exit $verdict
