#!/usr/bin/env bash
#
# immurok one-shot installer — README §1 (dependencies), §2 (build) and §3
# (install) as a single command, so the only steps left to do by hand are the
# ones that need the physical device: pairing and fingerprint enrollment.
#
#   ./scripts/install.sh                install dependencies, build, install
#   ./scripts/install.sh -y             don't ask before installing packages
#   ./scripts/install.sh --skip-deps    assume dependencies are already there
#   ./scripts/install.sh --dry-run      print every command, change nothing
#
# Package names are not duplicated here: they come from
# `check-deps.sh deps-cmd`, so the installer and the preflight cannot disagree
# about what a distro needs.
#
# Two separate sudo prompts are expected — one for the package manager, one for
# `make install` (which funnels all of its root work through a single
# scripts/install-root.sh call; see the comment at the top of that file).

set -euo pipefail

ASSUME_YES=0
SKIP_DEPS=0
DRY_RUN=0

while [ $# -gt 0 ]; do
  case "$1" in
    -y|--yes)    ASSUME_YES=1;;
    --skip-deps) SKIP_DEPS=1;;
    --dry-run)   DRY_RUN=1;;
    -h|--help)   sed -n '3,18p' "$0" | sed 's/^# \{0,1\}//'; exit 0;;
    *) echo "unknown option: $1  (try --help)" >&2; exit 2;;
  esac
  shift
done

# Building as root leaves a root-owned target/ that the next unprivileged build
# cannot write, and `make install` calls sudo itself where it actually needs it.
[ "$(id -u)" -eq 0 ] && { echo "Don't run this as root — it calls sudo where root is needed." >&2; exit 1; }

cd "$(dirname "$(readlink -f "$0")")/.."

BOLD=$'\033[1m'; DIM=$'\033[2m'; GREEN=$'\033[32m'; RED=$'\033[31m'; YELLOW=$'\033[33m'; OFF=$'\033[0m'
STEP=0
step() { STEP=$((STEP+1)); printf '\n%s=== [%d/4] %s ===%s\n' "$BOLD" "$STEP" "$1" "$OFF"; }
run()  {
  printf '%s$ %s%s\n' "$DIM" "$*" "$OFF"
  [ "$DRY_RUN" -eq 1 ] && return 0
  "$@"
}

# ── 1. dependencies ────────────────────────────────────────────────────────
step "Dependencies"
if [ "$SKIP_DEPS" -eq 1 ]; then
  echo "skipped (--skip-deps)"
else
  # Exit 1 means "distro not recognised" and is recoverable by hand; anything
  # else is check-deps.sh itself being broken or absent, which is not.
  rc=0
  DEPS_CMD=$(bash scripts/check-deps.sh deps-cmd) || rc=$?
  if [ "$rc" -eq 1 ]; then
    printf '%s!%s Unrecognised distribution — install the dependencies from README §1 by hand,\n' "$YELLOW" "$OFF"
    echo "  then re-run with --skip-deps."
  elif [ "$rc" -ne 0 ]; then
    echo "scripts/check-deps.sh failed (exit $rc)" >&2
    exit 1
  else
    echo "$DEPS_CMD"
    case "$DEPS_CMD" in
      *" -Syu "*) echo "${DIM}(-Syu also upgrades installed packages: Arch does not support partial upgrades.)${OFF}";;
    esac
    if [ "$ASSUME_YES" -eq 0 ] && [ "$DRY_RUN" -eq 0 ]; then
      read -r -p "Run this? [Y/n] " reply
      case "$reply" in [nN]*) echo "skipped"; DEPS_CMD="";; esac
    fi
    # Word splitting is the point: DEPS_CMD is a command line, not a filename.
    # shellcheck disable=SC2086
    [ -n "$DEPS_CMD" ] && run $DEPS_CMD
  fi
fi

# ── 2. preflight ───────────────────────────────────────────────────────────
# Runs even after a successful install above: it is the check that whatever the
# package manager did actually satisfies the build (an apt box still needs
# rustup, for instance).
step "Preflight"
if [ "$DRY_RUN" -eq 0 ]; then
  bash scripts/check-deps.sh all || exit 1
else
  echo "${DIM}(skipped in --dry-run)${OFF}"
fi

# ── 3. build ───────────────────────────────────────────────────────────────
step "Build"
echo "${DIM}First build downloads and compiles ~200 crates: 10-30 minutes.${OFF}"
run make all

# ── 4. install ─────────────────────────────────────────────────────────────
step "Install"
echo "${DIM}sudo is needed for the PAM module, the system unit and the immurok user.${OFF}"
run make install

[ "$DRY_RUN" -eq 1 ] && { echo; echo "--dry-run: nothing was changed."; exit 0; }

# ── verify ─────────────────────────────────────────────────────────────────
# Everything below is a report, not a gate: a fresh install is expected to be
# "running but unpaired", and nothing here can be fixed by failing the script.
printf '\n%s=== Verify ===%s\n' "$BOLD" "$OFF"
ok()   { printf '  %s✓%s %s\n' "$GREEN" "$OFF" "$1"; }
nope() { printf '  %s✗%s %s\n' "$RED" "$OFF" "$1"; VERIFY_FAILED=1; }
VERIFY_FAILED=0

systemctl is-active --quiet immurok-daemon \
  && ok "immurok-daemon running (system unit, user 'immurok')" \
  || nope "immurok-daemon not running — systemctl status immurok-daemon"

systemctl --user is-active --quiet immurok-session-agent \
  && ok "immurok-session-agent running (dialogs, notifications, ~/.ssh/config)" \
  || nope "immurok-session-agent not running — systemctl --user status immurok-session-agent"

command -v immurok-cli >/dev/null \
  && ok "immurok-cli on PATH ($(command -v immurok-cli))" \
  || nope "immurok-cli not on PATH — check that $(make -s print-bindir) is in it"

grep -q pam_immurok /etc/pam.d/sudo \
  && ok "PAM hook installed in /etc/pam.d/sudo" \
  || nope "no pam_immurok in /etc/pam.d/sudo — immurok-cli pam install sudo"

if [ "$VERIFY_FAILED" -eq 0 ]; then
  cat <<'NEXT'

=== Done ===
Remaining steps need the device in your hand:

  immurok-cli pair          # hold the device button until the LED blinks blue
  immurok-cli fp enroll 0   # touch the sensor 6 times
  immurok-cli set sudo on   # then: sudo -k && sudo whoami

Or do all of it from the TUI:  immurok-cli tui
NEXT
else
  printf '\n%sSome checks failed — see README §6 (Troubleshooting).%s\n' "$YELLOW" "$OFF"
  exit 1
fi
