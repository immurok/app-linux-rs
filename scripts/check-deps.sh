#!/usr/bin/env bash
#
# immurok dependency preflight — run before `make build` / `make install` so
# every missing system component is listed up front with the install command
# for your distro, instead of failing cryptically halfway through cargo, the
# C compile, or the running daemon.
#
# Usage: check-deps.sh [build|all]       (default: all)
#   build  check build-critical items only; exit 1 if any are missing
#   all    build-critical + runtime items; build missing -> exit 1,
#          runtime missing -> warn only
#
# Environment:
#   CARGO=<path>   override cargo path (the Makefile passes the detected one)
#   CC=<compiler>  override C compiler (default: gcc)
#
# Checked items:
#   build:   cargo / C compiler / PAM dev header (security/pam_modules.h)
#   runtime: python3 / dbus_fast (BLE helper) / PyGObject+Gtk4+Adw (auth
#            dialog) / bluez

MODE="${1:-all}"
CARGO="${CARGO:-cargo}"
CC="${CC:-gcc}"

# Each helper's interpreter (matches the script shebang)
PY_ENV="python3"          # ble-notify-helper.py: #!/usr/bin/env python3
PY_SYS="/usr/bin/python3" # immurok-auth-dialog:  #!/usr/bin/python3
[ -x "$PY_SYS" ] || PY_SYS="python3"

# ── Detect package manager (for install hints) ──
if   command -v dnf    >/dev/null 2>&1; then PM="dnf";    INSTALL="sudo dnf install"
elif command -v apt    >/dev/null 2>&1; then PM="apt";    INSTALL="sudo apt install"
elif command -v pacman >/dev/null 2>&1; then PM="pacman"; INSTALL="sudo pacman -S --needed"
else PM="unknown"; INSTALL="(install with your package manager)"
fi

# Per-distro package names
pkg() {
  case "$1:$PM" in
    rust:dnf)        echo "rust cargo";;
    rust:apt)        echo "(install via rustup: https://rustup.rs)";;
    rust:pacman)     echo "rust";;
    cc:dnf|cc:apt|cc:pacman) echo "gcc";;
    pkgconfig:dnf)   echo "pkgconf-pkg-config";;
    pkgconfig:apt)   echo "pkg-config";;
    pkgconfig:pacman) echo "pkgconf";;
    dbus:dnf)        echo "dbus-devel";;
    dbus:apt)        echo "libdbus-1-dev";;
    dbus:pacman)     echo "dbus";;
    pam:dnf)         echo "pam-devel";;
    pam:apt)         echo "libpam0g-dev";;
    pam:pacman)      echo "pam";;
    dbusfast:dnf)    echo "python3-dbus-fast";;
    dbusfast:apt)    echo "python3-dbus-fast  (or: pip install --user dbus-fast)";;
    dbusfast:pacman) echo "python-dbus-fast  (AUR; or: pip install --user dbus-fast)";;
    gtk:dnf)         echo "python3-gobject gtk4 libadwaita";;
    gtk:apt)         echo "python3-gi gir1.2-gtk-4.0 gir1.2-adw-1";;
    gtk:pacman)      echo "python-gobject gtk4 libadwaita";;
    bluez:dnf)       echo "bluez";;
    bluez:apt)       echo "bluez";;
    bluez:pacman)    echo "bluez bluez-utils";;
    *) echo "?";;
  esac
}

FATAL=0
WARN=0
ok()   { printf '  \033[32m✓\033[0m %s\n' "$1"; }
bad()  { printf '  \033[31m✗\033[0m %-26s missing -> %s %s\n' "$1" "$INSTALL" "$(pkg "$2")"; }
warn() { printf '  \033[33m!\033[0m %-26s missing -> %s %s\n' "$1" "$INSTALL" "$(pkg "$2")"; }

echo "=== immurok dependency preflight (package manager: $PM) ==="
echo "[build-critical]"

# cargo (command -v handles PATH, [ -x ] handles an absolute path)
if command -v "$CARGO" >/dev/null 2>&1 || [ -x "$CARGO" ]; then
  ok "cargo  ($CARGO)"
else
  bad "cargo" rust; FATAL=1
fi

# C compiler
if command -v "$CC" >/dev/null 2>&1; then
  ok "C compiler  ($CC)"
else
  bad "C compiler ($CC)" cc; FATAL=1
fi

# pkg-config (or pkgconf) — libdbus-sys (pulled in by bluer) uses it to
# locate system libdbus-1 at build time
PKGCONFIG=""
if command -v pkg-config >/dev/null 2>&1; then PKGCONFIG="pkg-config"
elif command -v pkgconf >/dev/null 2>&1; then PKGCONFIG="pkgconf"
fi
if [ -n "$PKGCONFIG" ]; then
  ok "pkg-config  ($PKGCONFIG)"
else
  bad "pkg-config" pkgconfig; FATAL=1
fi

# libdbus-1 dev — bluer -> libdbus-sys links against it; the canonical check
# is exactly what libdbus-sys's build script runs
if [ -n "$PKGCONFIG" ] && "$PKGCONFIG" --exists dbus-1 >/dev/null 2>&1; then
  ok "libdbus-1 dev  (dbus-1.pc)"
else
  bad "libdbus-1 dev" dbus; FATAL=1
fi

# PAM dev header — a compile test is the most reliable, path-independent check
if echo '#include <security/pam_modules.h>' | "$CC" -fsyntax-only -xc - >/dev/null 2>&1; then
  ok "PAM dev header  (security/pam_modules.h)"
else
  bad "PAM dev header" pam; FATAL=1
fi

if [ "$MODE" = build ]; then
  echo ""
  if [ "$FATAL" -ne 0 ]; then echo "✗ Build dependencies missing — install them before make."; exit 1; fi
  echo "✓ Build dependencies satisfied."; exit 0
fi

echo "[runtime  (used once the daemon runs; missing -> warn only, builds fine but features degraded)]"

# python3
if command -v "$PY_ENV" >/dev/null 2>&1; then
  ok "python3"
else
  warn "python3" gtk; WARN=1
fi

# dbus_fast — without it ble-notify-helper.py exits immediately and BLE
# disconnect-reconnects in a loop
if "$PY_ENV" -c 'import dbus_fast' >/dev/null 2>&1; then
  ok "python dbus_fast  (BLE notify helper)"
else
  warn "python dbus_fast" dbusfast; WARN=1
fi

# PyGObject + Gtk4 + Adw — immurok-auth-dialog (agent approval popup)
if "$PY_SYS" -c 'import gi; gi.require_version("Gtk","4.0"); gi.require_version("Adw","1"); from gi.repository import Gtk, Adw' >/dev/null 2>&1; then
  ok "PyGObject + Gtk4 + libadwaita  (auth dialog)"
else
  warn "PyGObject/Gtk4/Adw" gtk; WARN=1
fi

# bluez runtime
if command -v bluetoothctl >/dev/null 2>&1; then
  ok "bluez  (bluetoothctl)"
else
  warn "bluez" bluez; WARN=1
fi

echo ""
if [ "$FATAL" -ne 0 ]; then echo "✗ Build dependencies missing — install them before make."; exit 1; fi
if [ "$WARN" -ne 0 ]; then
  echo "⚠ Build deps OK (make will work), but runtime deps are missing — the daemon will run with degraded features (BLE / approval popup). Install them."
else
  echo "✓ All dependencies satisfied."
fi
exit 0
