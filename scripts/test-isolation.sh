#!/bin/bash
# 隔离验收 —— 以普通用户身份跑，每一条都必须失败。
#
# 特权分离要挡住的是「同一个用户下的任意进程」：停掉 daemon 再自己 bind 一个
# 回 OK 的假服务端，就能零交互过 sudo 和 polkit。这个脚本把那条路径上的每一
# 步都试一遍，任何一步成功都意味着边界没建起来。
#
# 用法: scripts/test-isolation.sh      （不要用 sudo 跑）
set -u

RUNTIME_DIR="${IMMUROK_RUNTIME_DIR:-/run/immurok}"
STATE_DIR="${IMMUROK_STATE_DIR:-/var/lib/immurok}"
BIN="${IMMUROK_BIN:-/usr/local/bin/immurok-daemon}"

if [ "$(id -u)" -eq 0 ]; then
    echo "别用 root 跑这个脚本 —— root 当然能做到这些，测的是普通用户做不到。"
    exit 2
fi

fail=0
# 期望失败：命令成功 = 边界破了
must_fail() {
    local label="$1"; shift
    if "$@" >/dev/null 2>&1; then
        echo "FAIL: $label —— 竟然成功了，隔离没有生效"
        fail=1
    else
        echo "PASS: $label 被拒绝"
    fi
}

echo "=== immurok 隔离验收（普通用户 $(id -un)）==="

if [ ! -S "$RUNTIME_DIR/pam.sock" ]; then
    echo "SKIP: $RUNTIME_DIR/pam.sock 不存在 —— daemon 没跑，或还没迁移"
    exit 2
fi

MAINPID=$(systemctl show -p MainPID --value immurok-daemon 2>/dev/null)

# 1. 抢 socket：删掉它就能自己 bind 一个假的
must_fail "删除 PAM socket"        rm -f "$RUNTIME_DIR/pam.sock"
# 2. 在目录里新建文件：daemon 没跑时抢先占位同样致命
must_fail "在 runtime 目录新建文件" touch "$RUNTIME_DIR/attacker.sock"
# 3. 杀 daemon：停掉它是抢 socket 的前置动作
if [ -n "$MAINPID" ] && [ "$MAINPID" != "0" ]; then
    must_fail "kill daemon 进程"    kill "$MAINPID"
else
    echo "SKIP: 拿不到 daemon 的 MainPID"
fi
# 4. 读配对密钥：拿到 shared_key 就能冒充主机
must_fail "读取 pairing.json"      cat "$STATE_DIR/pairing.json"
# 5. 改二进制：能替换它，前面几条挡住了也没意义
must_fail "改写 daemon 二进制"     sh -c "echo x >> '$BIN'"

# 6. 冒充 PAM：以普通用户身份连上去发 AUTH，daemon 必须按 uid 拒绝
if command -v python3 >/dev/null 2>&1; then
    resp=$(python3 - "$RUNTIME_DIR/pam.sock" <<'PY' 2>/dev/null
import socket, sys
s = socket.socket(socket.AF_UNIX)
try:
    s.connect(sys.argv[1])
    s.sendall(b"AUTH:" + __import__("getpass").getuser().encode() + b":sudo\n")
    s.settimeout(5)
    print(s.recv(256).decode(errors="replace").strip())
except Exception as e:
    print("ERROR:%s" % e)
PY
)
    case "$resp" in
        OK*) echo "FAIL: 普通用户直接发 AUTH 竟然拿到了 '$resp'"; fail=1 ;;
        *)   echo "PASS: 普通用户发 AUTH 被拒（$resp）" ;;
    esac
fi

echo
if [ "$fail" = "0" ]; then
    echo "全部通过：普通用户进程无法让 pam_immurok 返回成功，也读不到配对密钥。"
else
    echo "有断言失败 —— 隔离没有真正建立，别把这个版本当成已加固。"
fi
exit $fail
