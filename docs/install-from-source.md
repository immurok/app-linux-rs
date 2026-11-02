# Building immurok from source

Only needed if there is no package for your distribution, or you want to run
a modified build. Packages are the supported path — see the
[README](../README.md).

> A package install and a source install cannot coexist: both ship
> `pam_immurok.so` and the polkit policy at the same paths, and `/usr/local`
> shadows `/usr`. Run `make uninstall` in your checkout before installing a
> package. Pairing data in `/var/lib/immurok` survives either way.

## Install dependencies

### Arch / Manjaro / EndeavourOS

```bash
sudo pacman -S --needed rust gcc pkgconf dbus pam bluez bluez-utils \
  gtk4 libadwaita python-gobject polkit python-dbus-fast

# gtk4/libadwaita above already include the dev headers Arch needs to build
# immurok-gui (optional — only needed to build the GUI, daemon/CLI/TUI don't
# need it)
```

### Fedora 43+

```bash
sudo dnf install rust cargo gcc pkgconf-pkg-config dbus-devel pam-devel \
  bluez bluez-libs \
  gtk4 libadwaita python3-gobject \
  python3-dbus-fast polkit \
  gtk4-devel libadwaita-devel   # optional: only needed to build immurok-gui
```

### Debian 12+ / Ubuntu 22.04+

```bash
sudo apt install gcc pkg-config libdbus-1-dev libpam0g-dev bluez \
  libgtk-4-1 libadwaita-1-0 python3-gi \
  policykit-1 \
  libgtk-4-dev libadwaita-1-dev   # optional: only needed to build immurok-gui

# Rust: the apt version is usually too old, prefer rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# dbus-fast: not always in apt, use pip
pip install --user dbus-fast
# Or on Debian 12+ (PEP 668 enforced), use pipx:
sudo apt install pipx && pipx install dbus-fast
```

> Ubuntu 24.04+ ships `python3-dbus-fast` in apt, so you can skip pip.

## Build

```bash
git clone https://github.com/immurok/app-linux-rs
cd app-linux-rs

make check-deps   # Preflight: lists every missing system component at once + the install command for your distro
make              # Equivalent to `make build pam` (build runs check-deps automatically first)
```

> `check-deps` fails fast on missing build deps (cargo / C compiler / PAM headers) and warns about missing
> runtime deps (`dbus_fast` / PyGObject+Gtk4 / bluez). `make` runs it automatically before building.

Artifacts:
- `target/release/immurok-daemon` — main daemon
- `target/release/immurok-cli` — interactive TUI + configuration / pairing CLI
- `target/release/imk` — agent command wrapper
- `pam/pam_immurok.so` — PAM module

> The first `cargo build` downloads dependencies and compiles ~200 crates: 10–30 minutes (depending on the machine).

## Install

```bash
make install
```

What this step does:

| File | Path | Needs sudo |
|------|------|------------|
| `immurok-daemon` / `imk` / `immurok-cli` | `/usr/local/bin/` | Yes |
| `immurok-auth-dialog` / `immurok-pam-helper` / `ble-notify-helper.py` | `/usr/local/bin/` | Yes |
| `pam_immurok.so` | `/usr/lib64/security/` (Fedora) / `/lib/x86_64-linux-gnu/security/` (Debian) / `/usr/lib/security/` (Arch) | Yes |
| PAM service config | `/etc/pam.d/sudo` / `/etc/pam.d/polkit-1` / `/etc/pam.d/gdm-password` | Yes |
| polkit policy | `/usr/share/polkit-1/actions/com.immurok.pam-helper.policy` | Yes |
| systemd system unit | `/etc/systemd/system/immurok-daemon.service` | Yes |
| session agent | `/etc/systemd/user/immurok-session-agent.service` | Yes |

> No `/etc/pam.d/gdm-password` (KDE / SDDM setups) is fine — the Makefile skips that entry. For login-screen fingerprint unlock on SDDM: `sudo immurok-pam-helper add sddm`.

After `make install` completes, the daemon should already be running:

```bash
systemctl status immurok-daemon
```
