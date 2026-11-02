# Troubleshooting

## `make install` fails: `ERROR:NO_AUTH_LINE`

The `auth` line format in the PAM config wasn't recognized. The current helper supports both `^auth` and `^@include` styles. If your distro uses something else (rare), manually edit `/etc/pam.d/sudo` and add this before all auth lines:

```
auth        sufficient    pam_immurok.so
```

## `pam_immurok.so` not found

The PAM module was installed to the wrong directory. Check your distro's standard location:

```bash
find /usr/lib* /lib* -name 'pam_*.so' 2>/dev/null | head -5
# Use the first directory as the target and copy it there
sudo cp pam/pam_immurok.so /usr/lib64/security/   # use the path found above
```

## sudo asks for a password instead of popping the fingerprint dialog

- The daemon isn't running: `sudo systemctl start immurok-daemon`
- The device isn't connected: `immurok-cli status` should show `Status: Connected`
- PAM doesn't have immurok: `sudo grep pam_immurok /etc/pam.d/sudo`; if empty, run `immurok-cli pam install sudo` (a wrapper around `sudo immurok-pam-helper add sudo`)

## A password manager asks for its master password instead of a fingerprint

The app never reached immurok — it went through polkit and polkit fell back to
your login password.

- `immurok-cli pam check` must not report `polkit-1` missing; if it does, `immurok-cli pam install polkit-1`
- `immurok-cli settings` must show polkit on; if not, `immurok-cli set polkit on`
- Flatpak Bitwarden: the polkit action file must exist, see §4.7
- The "system authentication" option is off inside the app, or the app was
  started before the policy file was installed — restart it

## The polkit dialog doesn't appear

Source installs no longer write a polkit override (the daemon's socket lives in
`/run/immurok`, which polkit's default sandbox can already see). If a stale
`/etc/systemd/system/polkit.service.d/immurok.conf` from an old version is still
present, remove it and `sudo systemctl restart polkit`.

## BLE can't find the device

```bash
bluetoothctl scan le         # should list "immurok IK-1"
immurok-cli logs | grep BLE  # daemon log lives in /var/log/immurok (0640, daemon-owned);
                              # the CLI reads it over the socket, not the journal
```

## Device repeatedly disconnects/reconnects (`ATT error: 0x0e` in logs)

BlueZ cached stale GATT handles from an old firmware layout. Forget and re-pair once:

```bash
bluetoothctl remove <MAC>   # then: immurok-cli pair
```

> Do **not** work around this with a global `[GATT] Cache = no` in `/etc/bluetooth/main.conf` — it breaks reconnection for *all* BLE peripherals (mice, headphones). Keep the default `Cache = always`.

## `dbus-fast` import fails on Debian / Ubuntu

```bash
python3 -c 'import dbus_fast'   # should not error
# If you get ModuleNotFoundError:
pip install --user dbus-fast
# If installed via pipx, add the script path to the daemon user's PATH
```

Note that `ble-notify-helper.py` uses `#!/usr/bin/python3`, i.e. the system python (not a venv), so a `pip install --user` lands in `~/.local/lib/python3.X/site-packages` where the system python can find it.

## GTK dialog doesn't grab focus under Wayland

Intentional — it never steals keyboard focus and closes itself once the fingerprint passes. Alt+Tab to it if you need the Cancel button.

## The GUI still behaves like the previous build after `make install` (setup wizard doesn't come back, old wording, old bugs)

`immurok-gui` stays resident after login (`--gapplication-service`), and every
later `immurok-gui`, hotkey or app-menu launch is forwarded to that process. A
resident instance started before `make install` keeps running the old binary
until it exits. `make install` now asks it to quit (`scripts/retire-gui.sh`),
and a newer `immurok-gui` started from a terminal does the same when it finds
the resident instance running a replaced binary. For a resident instance older
than 0.9 — it has no `retire` action — stop it once by hand:

```bash
pkill -f 'immurok-gui --gapplication-service'
immurok-gui
```

## Notes per desktop environment

### GNOME (Fedora / Ubuntu Desktop)

Works out of the box. Screen unlock reads the session's `LockedHint` from logind on the
system bus (GNOME and KDE both set it); the daemon runs as its own system user and has no
session bus of its own.

### KDE (Fedora KDE / Kubuntu)

- Install `libadwaita` (KDE doesn't pull it in by default), otherwise the dialog won't launch
- Screen lock listens to the freedesktop `org.freedesktop.ScreenSaver` interface, which KDE is compatible with
- Login-screen (greeter) fingerprint is not implemented on any desktop: the module returns `PAM_IGNORE` for graphical login services on purpose, because they have no controlling tty and blocking there would stall password entry. Unlocking a *locked session* works everywhere and does not go through PAM at all — the daemon calls `loginctl unlock-session`.

### Sway / Hyprland and other wlroots compositors

GTK4 dialogs launch fine. Screen unlock depends on your lockscreen: swaylock and i3lock
never set logind's `LockedHint`, so they look permanently unlocked to the daemon and
fingerprint unlock does not fire — fall back to the password stack. `sudo` and polkit
authorization work normally.
