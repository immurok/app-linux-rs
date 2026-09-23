# immurok — Linux Companion App

The immurok Linux client, verified on **Arch / Fedora 43+ / Debian 12+ / Ubuntu 24.04+ / KDE & GNOME**. Ubuntu 22.04 has no packaged dbus-fast and is source-install only (see section 1). This document covers dependencies and install steps for the three distro families, plus common troubleshooting.

## Requirements

- Linux kernel 5.10+
- A Bluetooth adapter (BLE 4.2+)
- `systemd` user services (satisfied by default on virtually all mainstream desktop distros)
- PAM 1.4+, polkit 0.120+
- A desktop environment (GNOME / KDE / any DE running GTK4)

> ⚠️ This project does **not** support musl libc distros (Alpine / Void musl) — the PAM module depends on glibc.

## 0. Install from a package (recommended)

Prebuilt packages for **Debian 12+ / Ubuntu 24.04+ / Fedora 43+ / Arch**, amd64 and arm64,
are attached to every [GitHub Release](https://github.com/immurok/app-linux-rs/releases).

```bash
# Debian / Ubuntu
sudo apt install ./immurok_<version>-1_amd64.deb
# Fedora
sudo dnf install ./immurok-<version>-1.x86_64.rpm
# Arch
sudo pacman -U immurok-<version>-1-x86_64.pkg.tar.zst
```

The package starts `immurok-daemon`, enables the session agent for every user and registers
the settings app to autostart. Two things are left to you:

1. **Pair** — open *immurok* from the app menu, or run `immurok-cli pair`.
2. **Enable the PAM hooks** on the PAM page (or `immurok-cli pam install sudo`). The package
   deliberately does not touch `/etc/pam.d` on its own.

Upgrading is `apt install ./new.deb` / `dnf install ./new.rpm` / `pacman -U new.pkg.tar.zst`
again; the daemon is restarted for you.

> A package install and a source install (`make install`, section 3) cannot coexist: both
> ship `pam_immurok.so` and the polkit policy at the same paths, and `/usr/local` shadows
> `/usr`. If you installed from source before, run `make uninstall` in that checkout first
> — pairing data in `/var/lib/immurok` survives, only the PAM hooks need re-enabling.
> The package refuses to install while a source install is present (on Arch, `pacman`
> reports the file conflict instead).

Sections 1–3 below are for building from source.

## 1. Install dependencies

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

## 2. Build

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

## 3. Install

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

## 4. First-time setup

### 4.0 The GUI (optional)

`immurok-gui` opens the graphical settings window (also in your app menu as
"immurok"). It stays resident after login so the quick-fill panel is one key
away: bind `immurok-gui --quick-fill` to a shortcut in your desktop's keyboard
settings, press it in any text field, pick an OTP, touch the device. Phase 1
copies the code to the clipboard (cleared after 30 s); direct typing arrives in
0.9.

The **Fingerprints** page mirrors the macOS app: one card per enrolled
finger (click the name to rename it — names are stored locally in
`~/.config/immurok/gui.json`, the device only knows slot numbers), "+" to
enroll with the six-step guide, a hover-revealed delete button, "Test
Fingerprint", and — on firmware with two-host support — "Add switch
fingerprint" for the finger that only switches between your two computers.
The **Two Hosts** group on the Device page shows both host slots, marks this
computer, and lets you pair / unpair here or unbind the other computer
(one touch of an enrolled finger on the device confirms it).

The **PAM** page shows whether the immurok line is installed for sudo,
polkit and the login screen, with Install / Remove / Repair buttons (polkit
asks for your password). The **Firmware** page checks immurok.com for a
newer firmware and runs the OTA update with a progress bar — the window
refuses to close while an update is running. The **Logs** page tails the
daemon log live with error / warning colouring; scroll up to pause.

### 4.1 The TUI (recommended)

Everything below is also available from a single interactive terminal UI —
the recommended way to manage immurok:

```bash
immurok-cli tui
```

Number keys switch pages, `?` shows the full key reference, `q` quits.

| Page | Key | What you can do |
|------|-----|-----------------|
| Dashboard | `1` | pair/unpair (`p`/`u`), enroll (`e`, auto-picks the lowest empty slot), delete (`d`), verify (`v`), unlock toggles (`s`/`o`/`k`/`L`), recent-event feed |
| Keys | `2` | SSH / OTP / API keystore: add (`a`), delete (`d`), fetch OTP code (`o`), show SSH pubkey (`c`), show API value (`s`) |
| PAM | `3` | install / remove / repair the PAM hooks (`i`/`r`/`R`, pkexec prompts) |
| Logs | `4` | live-tail daemon logs with scrollback |
| Firmware | `5` or `U` | check for updates and install with a progress bar (see [4.5](#45-firmware-updates)) |

The one-shot CLI subcommands below do the same things — use them for scripting.

### 4.2 Pair the device

```bash
# Power on / hold the device button to enter pairing mode (LED slowly blinks blue)
immurok-cli pair               # or: press `p` in the TUI
# Confirm by pressing the device button within 30s
```

### 4.3 Enroll a fingerprint

In the TUI just press `e` — it enrolls into the lowest empty slot. Or via CLI:

```bash
immurok-cli fp enroll 0        # slot 0
# Touch the sensor 6 times, following the position hints
# (if a fingerprint is already enrolled, verify with it first to authorize)
immurok-cli fp list            # view enrolled slots
```

5 slots are supported (0–4). To delete: `immurok-cli fp delete 0`.

### 4.4 Enable features

Toggle directly on the TUI Dashboard (`s`/`o`/`k`/`L`), or via CLI:

```bash
immurok-cli set sudo on
immurok-cli set polkit on
immurok-cli set screen on          # screen unlock
immurok-cli set lock on            # long-press device button to lock the screen (optional)
immurok-cli settings               # view all settings
```

### 4.5 Firmware updates

Check and install official firmware from immurok.com:

```bash
immurok-cli fw check            # compare device firmware with the latest release
immurok-cli fw update           # download, verify and install (asks for confirmation)
immurok-cli fw update -y        # non-interactive
immurok-cli fw status           # last check result + interrupted-update resume state
```

Notes:

- Devices below 1.6.0 (old signing era) upgrade in two hops via a bridge
  package — handled automatically, including resume after an interruption.
- Requires ≥30% battery. Downloads are cached under `~/.immurok/fwupdate/`.
- The TUI (tab 5, or press `U` on the Dashboard) offers the same flow with
  a progress bar. `immurok-cli ota <file.imfw>` remains available for
  manually built packages.

### 4.6 Daemon management

```bash
sudo systemctl restart immurok-daemon   # restart the system daemon
```

`immurok-cli daemon restart` is not recommended right now: it still
targets the old per-user unit, not the system daemon installed by the
package (a Rust fix is pending). Use `systemctl` above instead.

Before the device is paired, only `fw`/`ota` (firmware update),
`pair`, `status` and `logs` are available — everything else exits with
a hint to pair first. The TUI opens normally but gates device-facing
actions the same way.

### 4.7 Password managers (1Password / Bitwarden / KeePassXC)

No extra immurok setup: all three apps offer "system authentication" unlock on
Linux, which goes through polkit → `/etc/pam.d/polkit-1` → `pam_immurok`. With
the PAM hook installed (§3) and the polkit toggle on (§4.4), a touch unlocks the
vault. Your master password never touches immurok.

Turn it on inside the app:

| App | Where |
|---|---|
| 1Password 8 | Settings → Security → **Unlock using system authentication service** |
| Bitwarden desktop | File → Settings → Security → **Unlock with system authentication** |
| KeePassXC ≥ 2.8 | Database unlock screen → **Quick Unlock** (polkit) |

Notes:

- You still log in with the master password once after starting the app;
  system authentication only covers subsequent *unlocks*. The polkit actions
  use `auth_self`, so every unlock asks for a touch.
- **Flatpak Bitwarden** can't install its polkit action from inside the
  sandbox — install it yourself (official instructions:
  <https://bitwarden.com/help/biometrics/>):

  ```bash
  curl -fsSL -o /tmp/com.bitwarden.Bitwarden.policy \
    https://raw.githubusercontent.com/bitwarden/clients/main/apps/desktop/resources/com.bitwarden.desktop.policy
  sudo install -o root -g root -m 0644 /tmp/com.bitwarden.Bitwarden.policy \
    /usr/share/polkit-1/actions/com.bitwarden.Bitwarden.policy
  # Fedora / SELinux only:
  sudo chcon system_u:object_r:usr_t:s0 /usr/share/polkit-1/actions/com.bitwarden.Bitwarden.policy
  ```

  The .deb / .rpm / AppImage builds ship the file themselves. KeePassXC's
  Quick Unlock policy is also a manual copy at the moment — see the
  KeePassXC release notes for your version.
- immurok does not ship these policy files: they belong to the apps and the
  native packages install the same paths.

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

- The daemon isn't running: `sudo systemctl start immurok-daemon`
- The device isn't connected: `immurok-cli status` should show `Status: Connected`
- PAM doesn't have immurok: `sudo grep pam_immurok /etc/pam.d/sudo`; if empty, run `immurok-cli pam install sudo` (a wrapper around `sudo immurok-pam-helper add sudo`)

### A password manager asks for its master password instead of a fingerprint

The app never reached immurok — it went through polkit and polkit fell back to
your login password.

- `immurok-cli pam check` must not report `polkit-1` missing; if it does, `immurok-cli pam install polkit-1`
- `immurok-cli settings` must show polkit on; if not, `immurok-cli set polkit on`
- Flatpak Bitwarden: the polkit action file must exist, see §4.7
- The "system authentication" option is off inside the app, or the app was
  started before the policy file was installed — restart it

### The polkit dialog doesn't appear

Source installs no longer write a polkit override (the daemon's socket lives in
`/run/immurok`, which polkit's default sandbox can already see). If a stale
`/etc/systemd/system/polkit.service.d/immurok.conf` from an old version is still
present, remove it and `sudo systemctl restart polkit`.

### BLE can't find the device

```bash
bluetoothctl scan le         # should list "immurok IK-1"
immurok-cli logs | grep BLE  # daemon log lives in /var/log/immurok (0640, daemon-owned);
                              # the CLI reads it over the socket, not the journal
```

### Device repeatedly disconnects/reconnects (`ATT error: 0x0e` in logs)

BlueZ cached stale GATT handles from an old firmware layout. Forget and re-pair once:

```bash
bluetoothctl remove <MAC>   # then: immurok-cli pair
```

> Do **not** work around this with a global `[GATT] Cache = no` in `/etc/bluetooth/main.conf` — it breaks reconnection for *all* BLE peripherals (mice, headphones). Keep the default `Cache = always`.

### `dbus-fast` import fails on Debian / Ubuntu

```bash
python3 -c 'import dbus_fast'   # should not error
# If you get ModuleNotFoundError:
pip install --user dbus-fast
# If installed via pipx, add the script path to the daemon user's PATH
```

Note that `ble-notify-helper.py` uses `#!/usr/bin/python3`, i.e. the system python (not a venv), so a `pip install --user` lands in `~/.local/lib/python3.X/site-packages` where the system python can find it.

### GTK dialog doesn't grab focus under Wayland

Intentional — it never steals keyboard focus and closes itself once the fingerprint passes. Alt+Tab to it if you need the Cancel button.

## 7. Uninstall

```bash
cd app-linux-rs
make uninstall
```

This stops the service and removes the PAM config and polkit policy, but **keeps**
`/var/lib/immurok` (pairing keys, settings, key caches).

To also purge that state, pass `PURGE=1` on the same run instead:

```bash
make uninstall PURGE=1
```

### Package install

```bash
sudo apt remove immurok      # or: sudo apt purge immurok   (also deletes /var/lib/immurok and /var/log/immurok)
sudo dnf remove immurok
sudo pacman -R immurok
```

Removing the package takes the PAM hooks out of `/etc/pam.d` and stops the daemon. On
Fedora and Arch the pairing data is kept; to wipe it run
`sudo immurok-pam-helper purge-daemon --purge-state` **before** removing the package, or
`sudo rm -rf /var/lib/immurok /var/log/immurok` afterwards.

## Notes per desktop environment

### GNOME (Fedora / Ubuntu Desktop)

Works out of the box. Screen unlock listens to the `org.gnome.ScreenSaver` D-Bus signal.

### KDE (Fedora KDE / Kubuntu)

- Install `libadwaita` (KDE doesn't pull it in by default), otherwise the dialog won't launch
- Screen lock listens to the freedesktop `org.freedesktop.ScreenSaver` interface, which KDE is compatible with
- Login-screen fingerprint unlock requires installing the PAM hook for `sddm`. Currently `immurok-cli pam install` only whitelists `sudo/gdm-password/polkit-1`; to add sddm, manually edit `/etc/pam.d/sddm` and insert before the first auth line: `auth sufficient pam_immurok.so`

### Sway / Hyprland and other wlroots compositors

GTK4 dialogs launch fine. Screen lock depends on your lockscreen (swaylock / hyprlock); these generally don't emit D-Bus signals, so fingerprint screen-unlock may not work — just fall back to the password stack.

## License

[Apache License 2.0](LICENSE) — including the PAM module in `pam/`. Device firmware is licensed separately under BSL 1.1.
