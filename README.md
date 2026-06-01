# immurok — Linux Companion App

The immurok Linux client, verified on **Arch / Fedora 38+ / Debian 12+ (incl. Ubuntu 22.04+) / KDE & GNOME**. This document covers dependencies and install steps for the three distro families, plus common troubleshooting.

## Requirements

- Linux kernel 5.10+
- A Bluetooth adapter (BLE 4.2+)
- `systemd` user services (satisfied by default on virtually all mainstream desktop distros)
- PAM 1.4+, polkit 0.120+
- A desktop environment (GNOME / KDE / any DE running GTK4)

> ⚠️ This project does **not** support musl libc distros (Alpine / Void musl) — the PAM module depends on glibc.

## 1. Install dependencies

### Arch / Manjaro / EndeavourOS

```bash
sudo pacman -S --needed rust gcc pkgconf dbus pam bluez bluez-utils \
  gtk4 libadwaita python-gobject polkit libcanberra

# python-dbus-fast is in the AUR
yay -S python-dbus-fast
# Or skip the AUR and use pip:
pip install --user dbus-fast
```

### Fedora 38+

```bash
sudo dnf install rust cargo gcc pkgconf-pkg-config dbus-devel pam-devel \
  bluez bluez-libs \
  gtk4 libadwaita python3-gobject \
  python3-dbus-fast polkit libcanberra-gtk3
```

### Debian 12+ / Ubuntu 22.04+

```bash
sudo apt install gcc pkg-config libdbus-1-dev libpam0g-dev bluez \
  libgtk-4-1 libadwaita-1-0 python3-gi \
  policykit-1 libcanberra-gtk-module

# Rust: the apt version is usually too old, prefer rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# dbus-fast: not always in apt, use pip
pip install --user dbus-fast
# Or on Debian 12+ (PEP 668 enforced), use pipx:
sudo apt install pipx && pipx install dbus-fast
```

> Ubuntu 24.04+ ships `python3-dbus-fast` in apt, so you can skip pip.

## 2. Build

```bash
git clone https://github.com/immurok/app-linux-rs
cd app-linux-rs

make check-deps   # Preflight: lists every missing system component at once + the install command for your distro
make              # Equivalent to `make build pam` (build runs check-deps automatically first)
```

> `make check-deps` checks cargo / C compiler / PAM dev headers (required to build — missing ones make `make` fail outright),
> plus `dbus_fast` / PyGObject+Gtk4 / bluez (required at runtime — missing ones only warn; it still compiles, but the
> daemon will lack BLE / auth-dialog features). `make` wires this as a prerequisite of `build`, so missing packages no
> longer blow up cryptically halfway through compilation or while the daemon is running.

Artifacts:
- `target/release/immurok-daemon` — main daemon
- `target/release/immurok-cli` — configuration / pairing CLI
- `target/release/imk` — agent command wrapper
- `pam/pam_immurok.so` — PAM module

> The first `cargo build` downloads dependencies and compiles ~200 crates: 10–30 minutes (depending on the machine).

## 3. Install

```bash
make install
```

What this step does:

| File | Path | Needs sudo |
|------|------|------------|
| `immurok-daemon` / `imk` / `immurok-cli` | `~/.local/bin/` | No |
| `immurok-auth-dialog` / `immurok-pam-helper` / `ble-notify-helper.py` | `~/.local/bin/` | No |
| `pam_immurok.so` | `/usr/lib64/security/` (Fedora) / `/lib/x86_64-linux-gnu/security/` (Debian) / `/usr/lib/security/` (Arch) | Yes |
| PAM service config | `/etc/pam.d/sudo` / `/etc/pam.d/polkit-1` / `/etc/pam.d/gdm-password` | Yes |
| polkit policy | `/usr/share/polkit-1/actions/com.immurok.pam-helper.policy` | Yes |
| systemd polkit overrides | `/etc/systemd/system/polkit.service.d/immurok.conf` | Yes |
| systemd user service | `~/.config/systemd/user/immurok-daemon.service` | No |

> Not having GDM / no `/etc/pam.d/gdm-password` is common (KDE / Debian + SDDM). The Makefile tolerates this and skips that entry; if you want fingerprint unlock on the login screen, handle it yourself with something like `sudo immurok-pam-helper add sddm`.

After `make install` completes, the daemon should already be running:

```bash
systemctl --user status immurok-daemon
```

## 4. First-time setup

### 4.1 Pair the device

```bash
# Power on / hold the device button to enter pairing mode (LED slowly blinks blue)
immurok-cli pair
# Confirm by pressing the device button within 30s
```

### 4.2 Enroll a fingerprint

```bash
immurok-cli fp enroll 0        # slot 0
# Touch the sensor 12 times as prompted
immurok-cli fp list            # view enrolled slots
```

5 slots are supported (0–4). To delete: `immurok-cli fp delete 0`.

### 4.3 Enable features

```bash
immurok-cli set sudo true
immurok-cli set polkit true
immurok-cli set screen true        # screen unlock
immurok-cli set lock true          # long-press device button to lock the screen (optional)
immurok-cli settings               # view all settings
```

## 5. Verify

```bash
sudo -k && sudo whoami
# Should pop a GTK dialog or go straight to fingerprint (no re-prompt within the 10s cooldown)
```

If it works, after touching the sensor the terminal immediately prints `root` (the identity sudo elevated to), with no password prompt.

Test `imk run --agent`:

```bash
imk run --agent -- sudo systemctl restart NetworkManager
# Pops a single GTK dialog showing the wrapped command; approve with a fingerprint touch
```

## 6. Troubleshooting

### `make install` fails: `ERROR:NO_AUTH_LINE`

The `auth` line format in the PAM config wasn't recognized. The current helper supports both `^auth` and `^@include` styles. If your distro uses something else (rare), manually edit `/etc/pam.d/sudo` and add this before all auth lines:

```
auth        sufficient    pam_immurok.so
```

### `pam_immurok.so` not found

The PAM module was installed to the wrong directory. Check your distro's standard location:

```bash
find /usr/lib* /lib* -name 'pam_*.so' 2>/dev/null | head -5
# Use the first directory as the target and copy it there
sudo cp pam/pam_immurok.so /usr/lib64/security/   # use the path found above
```

### sudo asks for a password instead of popping the fingerprint dialog

- The daemon isn't running: `systemctl --user start immurok-daemon`
- The device isn't connected: `immurok-cli status` should show `connected=true verified=true`
- PAM doesn't have immurok: `sudo grep pam_immurok /etc/pam.d/sudo`; if empty, run `immurok-cli pam install sudo` (a wrapper around `sudo immurok-pam-helper add sudo`)

### The polkit dialog doesn't appear

```bash
# Check whether the polkit override took effect
systemctl show polkit | grep BindPaths
# Should show BindPaths=/run/user

# polkitd usually fails because ProtectHome=yes blocks access to /run/user
# The Makefile writes the override, but it needs systemctl daemon-reload + restart
sudo systemctl daemon-reload && sudo systemctl restart polkit
```

### BLE can't find the device

```bash
bluetoothctl scan le         # should list "immurok IK-1"
journalctl --user -u immurok-daemon | grep BLE
```

Device logs are in `~/.immurok/logs.txt`, **not** in the journal (the daemon writes its own file).

### `dbus-fast` import fails on Debian / Ubuntu

```bash
python3 -c 'import dbus_fast'   # should not error
# If you get ModuleNotFoundError:
pip install --user dbus-fast
# If installed via pipx, add the script path to the daemon user's PATH
```

Note that `ble-notify-helper.py` uses `#!/usr/bin/python3`, i.e. the system python (not a venv), so a `pip install --user` lands in `~/.local/lib/python3.X/site-packages` where the system python can find it.

### GTK dialog doesn't grab focus under Wayland

This is intentional — the dialog doesn't steal keyboard focus so it won't interrupt whatever command you're typing. It closes automatically once the fingerprint passes. If you can't click the Cancel button, switch focus to the dialog window (Alt+Tab).

## 7. Uninstall

```bash
cd app-linux-rs
make uninstall
```

This stops the service and removes the PAM config, polkit policy, and override, but **keeps** `~/.immurok/` (pairing keys, settings, logs).

To wipe everything:

```bash
rm -rf ~/.immurok
```

## Notes per desktop environment

### GNOME (Fedora / Ubuntu Desktop)

Works out of the box. Screen unlock listens to the `org.gnome.ScreenSaver` D-Bus signal.

### KDE (Fedora KDE / Kubuntu)

- Install `libadwaita` (KDE doesn't pull it in by default), otherwise the dialog won't launch
- Screen lock listens to the freedesktop `org.freedesktop.ScreenSaver` interface, which KDE is compatible with
- Login-screen fingerprint unlock requires installing the PAM hook for `sddm`. Currently `immurok-cli pam install` only whitelists `sudo/gdm-password/polkit-1`; to add sddm, manually edit `/etc/pam.d/sddm` and insert before the first auth line: `auth sufficient pam_immurok.so`

### Sway / Hyprland and other wlroots compositors

GTK4 dialogs launch fine. Screen lock depends on your lockscreen (swaylock / hyprlock); these generally don't emit D-Bus signals, so fingerprint screen-unlock may not work — just fall back to the password stack.
