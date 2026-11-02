#!/bin/bash
# Tests for verify-fresh.sh's decision logic (pure functions; needs no root
# and touches no real processes).
set -u
SRC="$(cd "$(dirname "$0")" && pwd)"
IMMUROK_VERIFY_FRESH_SOURCE_ONLY=1 . "$SRC/verify-fresh.sh"

fail=0
check() { if [ "$1" = "$2" ]; then echo "PASS: $3"; else echo "FAIL: $3 (got '$1' want '$2')"; fail=1; fi; }

# -- started_before_binary ------------------------------------------------
started_before_binary 100 200 && r=stale || r=fresh
check "$r" stale "process older than the binary is stale"
started_before_binary 300 200 && r=stale || r=fresh
check "$r" fresh "process newer than the binary is fresh"
# Binary replaced and process started within the same second: prefer a
# spurious warning over missing a stale process.
started_before_binary 200 200 && r=stale || r=fresh
check "$r" stale "same second counts as stale (conservative direction)"

# -- component_state ------------------------------------------------------
check "$(component_state 200)"         absent "no process running"
check "$(component_state 200 300 400)" fresh  "every process newer than the binary"
check "$(component_state 200 100)"     stale  "the only process is stale"
# The case that matters: install replaced the binary and restarted one
# instance, but another resident instance was missed.
check "$(component_state 200 300 100)" stale  "mixed ages count as stale (one stale is enough)"
check "$(component_state 200 100 300)" stale  "mixed ages, reversed order, still stale"

exit $fail
