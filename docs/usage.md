# Using immurok on Linux

Setting the device up for the first time is covered in the
[README](../README.md) — this is the reference for everything after that: the
GUI's pages, the TUI, the one-shot CLI commands, firmware updates, daemon
management and password-manager unlock.

## Device setup

### The GUI

`immurok-gui` opens the graphical settings window (also in your app menu as
"immurok"). It stays resident after login so the quick-fill panel is one key
away: bind `immurok-gui --quick-fill` to a shortcut in your desktop's keyboard
settings, press it in any text field, pick an OTP, touch the device. Phase 1
copies the code to the clipboard (cleared after 30 s); direct typing arrives in
0.9.

On first launch a **setup wizard** opens: connect the device in your
desktop's Bluetooth settings, link this computer (press the device button;
a second computer first touches an enrolled finger), enroll a fingerprint.
Every step checks itself and moves on; "Later" dismisses it and the header
menu (or `immurok-gui --setup`) brings it back.

The **Fingerprints** page mirrors the macOS app: one card per enrolled
finger (click the name to rename it — names are stored locally in
`~/.config/immurok/gui.json`, the device only knows slot numbers), "+" to
enroll with the six-step guide, a hover-revealed delete button, "Test
Fingerprint", and — on firmware with two-host support — "Add switch
fingerprint" for the finger that only switches between your two computers.
The **Two Hosts** group on the Device page shows both host slots, marks this
computer, and lets you link / unlink here or unlink the other computer
(one touch of an enrolled finger on the device confirms it). "Link" is the
app-level key exchange; the Bluetooth connection itself is made in your
desktop's Bluetooth settings.

Clicking the firmware line in the sidebar device card reads the live
Bluetooth connection parameters from the device (interval, peripheral
latency, supervision timeout) and says whether the link is long enough for
linking and SSH signing. When linking fails, the error dialog reads these
parameters for you and, if the supervision timeout is under 3000 ms, says to
disconnect and reconnect the device in Bluetooth settings so they are
renegotiated.

The **PAM** page shows whether the immurok line is installed for sudo,
polkit and the login screen, with Install / Remove / Repair buttons (polkit
asks for your password). The **Firmware** page checks immurok.com for a
newer firmware and runs the OTA update with a progress bar — the window
refuses to close while an update is running. The **Logs** page tails the
daemon log live with error / warning colouring; scroll up to pause.

### The TUI

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
| Firmware | `5` or `U` | check for updates and install with a progress bar (see [Firmware updates](#firmware-updates)) |

The one-shot CLI subcommands below do the same things — use them for scripting.

### Pair the device

```bash
# Power on / hold the device button to enter pairing mode (LED slowly blinks blue)
immurok-cli pair               # or: press `p` in the TUI
# Confirm by pressing the device button within 30s
```

### Enroll a fingerprint

In the TUI just press `e` — it enrolls into the lowest empty slot. Or via CLI:

```bash
immurok-cli fp enroll 0        # slot 0
# Touch the sensor 6 times, following the position hints
# (if a fingerprint is already enrolled, verify with it first to authorize)
immurok-cli fp list            # view enrolled slots
```

5 slots are supported (0–4). To delete: `immurok-cli fp delete 0`.

### Enable features

Toggle directly on the TUI Dashboard (`s`/`o`/`k`/`L`), or via CLI:

```bash
immurok-cli set sudo on
immurok-cli set polkit on
immurok-cli set screen on          # screen unlock
immurok-cli set lock on            # long-press device button to lock the screen (optional)
immurok-cli settings               # view all settings
```

### Firmware updates

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

### Daemon management

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

### Password managers (1Password / Bitwarden / KeePassXC)

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

## Verify

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
