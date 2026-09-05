#!/bin/bash
# 并发探针：看 daemon 自己会不会报响应错乱
#
# 起因是一个**已被证伪**的假设：BleState 只有一个 pending_response（oneshot），
# send_command_inner 开头会 take() 掉上一个，看起来两条命令并发就会互相顶包。
# 实测不成立 —— socket 来的命令全部经 ble_cmd_tx 交给单个 BLE worker，本就串行，
# 并发 8 轮 daemon 一条错误都没报。
#
# 脚本保留的价值在于它的判据不含任何推测：只找 daemon 自己打出来的
# "response dropped" / "unexpected response" / "timeout: 0x.."。阴性结果本身
# 就是证据，将来改动 BLE 命令通道时可以拿它做回归。
#
# 用法: bash scripts/repro-response-mixup.sh [轮数，默认 10]
set -u

ROUNDS=${1:-10}
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
c_ok=$'\033[32m'; c_bad=$'\033[31m'; c_off=$'\033[0m'

immurok-cli status 2>/dev/null | grep -q 'Connected' || { echo "设备没连上，先连上再跑"; exit 2; }

echo "并发发命令 ${ROUNDS} 轮，不需要触摸传感器。"
timeout $((ROUNDS * 2 + 8)) immurok-cli logs > "$TMP/log" 2>/dev/null &
LOGPID=$!
sleep 1.5
BASE=$(wc -l < "$TMP/log")

for i in $(seq 1 "$ROUNDS"); do
    printf "\r  第 %d/%d 轮…" "$i" "$ROUNDS"
    # 两条命令同时发：它们会抢同一个 pending_response 槽
    immurok-cli fp list    >/dev/null 2>&1 &
    immurok-cli slot status >/dev/null 2>&1 &
    wait
    sleep 0.5
done
echo
sleep 2
kill $LOGPID 2>/dev/null; wait $LOGPID 2>/dev/null

NEW=$(tail -n +$((BASE+1)) "$TMP/log")
echo
echo "daemon 自己报的错（不是我推断的）："
HITS=$(echo "$NEW" | grep -E 'response dropped|unexpected response|timeout: 0x' | sed 's/^/  /')
if [ -n "$HITS" ]; then
    echo "$HITS"
    echo
    echo "${c_bad}复现成功${c_off} —— 单槽 pending_response 被并发命令抢占。"
    echo "  修法：给每条命令一个序号或独立的等待槽，响应按序号投递；"
    echo "  或者给整条 BLE 命令通道加互斥，同一时刻只允许一条在飞。"
    exit 1
fi
echo "  （无）"
echo
echo "${c_ok}本轮没有撞上${c_off} —— 加大轮数再试：bash $0 30"
echo "  注意：BLE 命令可能本来就被上层串行化了，若始终撞不上，说明并发这条路"
echo "  不是真正的触发条件，需要回到日志去找 GET_STATUS 那 1.5 秒里发生了什么。"
exit 0
