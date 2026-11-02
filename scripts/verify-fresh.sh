#!/bin/bash
#
# Post-install self-check: is any process still running a binary that has
# since been replaced?
#
# Why this exists: immurok has three long-lived processes and each is replaced
# by a different mechanism — the daemon is a system unit (install-root.sh
# restarts it), the session agent is a user unit, and the GUI is a resident
# D-Bus single instance (retire-gui.sh asks it to exit). If any one of those
# does not take effect, the disk holds new code while old code keeps running,
# and nothing reports it: the symptom is "my change had no effect, the bug I
# fixed is still there", which is expensive to chase. The Makefile used to
# start the session agent with `systemctl --user enable --now`, and `--now` is
# a no-op on an already-running unit — so it could keep running a replaced
# binary across any number of `make install` runs.
#
# How it decides: process start time vs. binary mtime. Neither needs
# privileges (reading /proc/<pid>/exe for another user's process requires
# root, so it is not used).
#
# Usage: verify-fresh.sh [BIN_DIR]        default /usr/local/bin
set -u

BIN_DIR="${1:-/usr/local/bin}"

# A process that started before the binary was written (or in the same second)
# counts as stale. Treating the same second as stale is deliberately the
# conservative direction: when the binary was just written and the process
# happens to start within the same second, the order cannot be told apart —
# and one spurious warning is far better than missing a stale process.
started_before_binary() {
    local proc_start="$1" bin_mtime="$2"
    [ "$proc_start" -le "$bin_mtime" ]
}

# Three states: no process -> absent; all newer than the binary -> fresh;
# any one older -> stale.
# $1 = binary mtime, remaining args = process start times (all epoch seconds).
# "Any one older means stale" is the point: restarting one instance while
# missing another resident one still leaves the install half-applied.
component_state() {
    local bin_mtime="$1"; shift
    [ "$#" -eq 0 ] && { echo absent; return; }
    local t
    for t in "$@"; do
        if started_before_binary "$t" "$bin_mtime"; then
            echo stale
            return
        fi
    done
    echo fresh
}

# When sourced, expose the pure functions above and skip the main flow
# (used by test-verify-fresh.sh).
if [ "${IMMUROK_VERIFY_FRESH_SOURCE_ONLY:-}" = "1" ]; then
    return 0 2>/dev/null || exit 0
fi

# Processes whose argv[0] is exactly this path. All three components are
# started by absolute path (systemd ExecStart / the D-Bus service file), so an
# exact match is enough and cannot catch this script itself.
pids_for() {
    local exe="$1" d pid argv0
    for d in /proc/[0-9]*; do
        pid="${d#/proc/}"
        argv0="$(tr '\0' '\n' < "$d/cmdline" 2>/dev/null | head -1)" || continue
        [ "$argv0" = "$exe" ] && echo "$pid"
    done
}

# Start time = now - elapsed seconds. `etimes` is an integer and
# locale-independent, which makes it safer than parsing `lstart`.
proc_start_epoch() {
    local pid="$1" etimes now
    etimes="$(ps -o etimes= -p "$pid" 2>/dev/null | tr -d ' ')" || return 1
    [ -n "$etimes" ] || return 1
    now="$(date +%s)"
    echo $(( now - etimes ))
}

stale=0
check_one() {
    local name="$1" label="$2" fix="$3"
    local exe="$BIN_DIR/$name" bin_mtime pids starts pid t

    if [ ! -x "$exe" ]; then
        printf '  %-22s \033[90mnot installed, skipped\033[0m\n' "$label"
        return
    fi
    bin_mtime="$(stat -c %Y "$exe")"
    pids="$(pids_for "$exe")"
    starts=""
    for pid in $pids; do
        t="$(proc_start_epoch "$pid")" || continue
        starts="$starts $t"
    done

    case "$(component_state "$bin_mtime" $starts)" in
        absent)
            printf '  %-22s \033[90mnot running - the next start picks up the new binary\033[0m\n' "$label"
            ;;
        fresh)
            printf '  %-22s \033[32mOK: running the binary just installed\033[0m\n' "$label"
            ;;
        stale)
            printf '  %-22s \033[31mSTALE: pid%s is still running a replaced binary\033[0m\n' \
                "$label" "$(echo "$pids" | tr '\n' ',' | sed 's/,$//' | sed 's/^/ /')"
            printf '  %-22s   fix: %s\n' "" "$fix"
            stale=1
            ;;
    esac
}

echo "Freshness check (running processes vs. the binaries in $BIN_DIR):"
check_one immurok-daemon        "immurok-daemon"        "sudo systemctl restart immurok-daemon"
check_one immurok-session-agent "immurok-session-agent" "systemctl --user restart immurok-session-agent"
check_one immurok-gui           "immurok-gui"           "bash scripts/retire-gui.sh"

if [ "$stale" = "1" ]; then
    echo ""
    echo -e "\033[31mThe install itself finished, but the processes marked STALE above are still old code. Run the fix shown for each, or what you observe will still be the previous version's behaviour.\033[0m"
    exit 1
fi
exit 0
