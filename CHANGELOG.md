# Changelog

All notable changes to the immurok Linux companion (daemon, CLI/TUI and PAM
module) are documented here. Versions follow the workspace crate version.

## Unreleased

### Added

- gui: sidebar navigation; Features moved to its own page; bundled fingerprint icon
- gui: sidebar device card, consistent icons, Firmware folded into Device, actionable empty states, header-placed actions
- gui(keys): add SSH (generate/import), OTP (single + file import), API entries; capacity and empty states; inline OTP code with Copy
- client: OTP/SSH parsing shared via immurok-client (keys_import / ssh_import); CLI/TUI use it
- **Distribution packages.** `.deb` (Debian 12+ / Ubuntu 24.04+), `.rpm`
  (Fedora 43+) and Arch `.pkg.tar.zst`, amd64 and arm64, built by GitHub
  Actions on every `v*` tag and attached to the release together with
  `SHA256SUMS`. One package installs the daemon, CLI/TUI, `imk`, the GTK
  settings app, the session agent and the PAM module; the daemon is started,
  the session agent enabled for every user and the settings app registered
  to autostart. Pairing and enabling the PAM hooks stay manual by design.
  `make package` builds the same three packages locally (needs nfpm and the
  GTK 4 / libadwaita dev headers), `make verify-pkg DISTRO=…` installs one in
  a distro container.
- `make install` and a package install now refuse to coexist (each detects
  the other); unit files reference `/usr/bin` and are rewritten for
  `/usr/local` at install time.

### Fixed

- **`imk run --agent` commands longer than ~500 bytes were reported as
  "rejected by user".** The daemon read a request into a 512-byte buffer;
  the tail stayed in the socket, where the approval handler's disconnect
  probe read it and cancelled the request before the dialog could wait for
  a touch. The buffer is now the protocol maximum (`MAX_REQUEST_BYTES`,
  64 KiB); a request that fills it answers `ERROR:TOO_LONG`, bytes after
  the request line answer `ERROR:PROTOCOL`, and the disconnect-probe path
  returns `ERROR:PROTOCOL` instead of `DENY:REJECTED`.
- **Multi-line agent commands showed only their first line in the approval
  dialog.** The socket protocol is line-based and `imk` sent the command
  raw, so the user approved `sudo bash -c ` with the body hidden. `imk` now
  escapes `\n`, `\r` and NUL to their visible two-character forms before
  sending, and refuses oversized commands up front with a hint to wrap a
  script file instead.

## 0.9.0 — 2026-09-18

### Added

- **PAM, Firmware and Logs pages in `immurok-gui`**, matching the TUI:
  per-service install state with pkexec-backed Install / Remove / Repair and
  the daemon isolation banner; update-server check, direct / two-hop /
  resumed plans and OTA progress (window locked while updating); live daemon
  log tail with level colouring and pause-on-scroll. The Device page shows a
  firmware-update hint after a silent 24 h-throttled check.
- `immurok-client`: `fwupdate` (moved from the CLI) and `pam` modules shared
  by CLI, TUI and GUI.

## 0.8.0 — 2026-09-18

### Added

- **Fingerprints page in `immurok-gui`.** Enroll with the six-step guided
  capture (the device's "too similar, shift your finger" reject is shown as
  a nudge instead of a failure), delete, rename (local names in
  `gui.json`), Test Fingerprint, and the host-switch finger (slot 5).
- **Two Hosts group on the Device page.** Both host slots with a "This
  computer" mark, Pair / Unpair, pairing progress text, and unbinding the
  other computer after one fingerprint touch.
- `immurok-client`: `fingerprint`, `enroll_session` (testable enrollment
  state machine), `hosts` and `enroll_hint` (moved from the CLI) modules.
- `immurok-common`: `EnrollEvent::Overlap` (0x06) and
  `PairProgress::from_wire`.

### Changed

- The GUI's error texts for fingerprint-gate outcomes now match the
  daemon's actual messages (timeout / cancelled / no match).

## 0.7.0 — 2026-09-18

### Added

- **`immurok-gui` — a GTK4 / libadwaita settings app.** Same daemon socket as
  the TUI, same one-request-per-connection protocol. Phase 1 ships the Device
  page (status, feature toggles, pair / unpair) and the Keys page (OTP / API
  reads through the device's fingerprint gate, SSH public key copy, delete).
- **Quick-fill panel.** `immurok-gui --quick-fill` (bind it to a key in your
  desktop's shortcut settings) pops a search list of your keys; pick one,
  touch the device, and the value lands in the clipboard with a notification
  and is cleared 30 s later. Typing straight into the focused field and
  portal-registered hotkeys follow in 0.8.
- `immurok-client` crate: the socket client shared by CLI, TUI and GUI.

### Changed

- `make` skips `immurok-gui` when GTK4 / libadwaita development headers are
  missing, so daemon-only installs need no new dependencies.

## 0.6.0 — 2026-09-05

### Added

- **Privilege separation — the daemon no longer runs as you.** It now runs as a
  dedicated system user (`immurok`) from a sandboxed systemd *system* unit, so a
  compromised user process can no longer read the pairing key, replace the
  binary, or squat on the socket path.
  - Every `$HOME`-derived path is gone. Runtime / state / log directories
    resolve as `IMMUROK_*` override → systemd `RUNTIME_DIRECTORY` /
    `STATE_DIRECTORY` / `LOGS_DIRECTORY` → compile-time default, shared by the
    daemon and the CLI (set `IMMUROK_*` to run the whole stack out of a scratch
    directory during development). `pairing.json`, settings and the key cache
    live in the state directory (`0700`); the log is `0640`.
  - The PAM channel moved from `/run/user/<uid>/…` to a fixed
    `/run/immurok/pam.sock`. The module `lstat`s the directory before
    connecting and requires it to be owned by root *or* the daemon user and not
    group/world-writable — `RuntimeDirectory=` chowns it to the service user, so
    demanding root ownership would lock us out 100% of the time.
  - Packaging: system unit, `tmpfiles.d`, a BlueZ D-Bus policy and a polkit rule
    for logind screen lock (`lock-sessions` — logind has no separate
    `unlock-sessions` action).
- **`immurok-session-agent` — a user-level agent for everything that must happen
  inside your session.** The system daemon has no display, no session bus and
  (with `ProtectHome=yes`) no home directory, so authorization dialogs, desktop
  notifications and `~/.ssh/config` management are now driven over a long-lived
  connection with a five-line text protocol (`UI:DIALOG:AUTH`,
  `UI:DIALOG:AGENT:<secs>:<cmd>`, `UI:DISMISS`, `NOTIFY:<text>`,
  `SSH_TAKEOVER:ON|OFF`, plus `UI:CANCEL` back). It holds no keys and takes no
  part in authorization: killing or spoofing it buys you a missing dialog or one
  spurious cancel, never an approval — the gate is the touch on the device. It
  binds to `default.target`, not `graphical-session.target`, so tty logins still
  reconcile SSH takeover.
- **Per-command authorization tiers**, replacing the old "peer uid == our uid"
  check that locked out the device's owner once the daemon changed identity.
  `AUTH` is accepted only from root/polkitd; read-only commands from any local
  user; everything else requires an active local session (via logind, falling
  back to a recorded owner uid when logind is unavailable). Rejections return
  `DENY:NOT_AUTHORIZED` instead of dropping the connection. The owner uid is
  persisted and checked against the `AUTH` user field — the socket is now
  machine-wide, so without that a second account's `sudo` could be approved by
  the owner's touch.
- **CLI/TUI read their data over the socket** now that the daemon's files are no
  longer readable by you: `KEY:CACHE:<ssh|names>` returns the on-disk cache
  verbatim (deliberately answered before the connectivity check — a cache that
  needs the device present is not a cache), and `SUBSCRIBE:LOG` streams a
  500-line ring buffer followed by live output, replacing the TUI's `tail -F`
  subprocess. `GET:INFO` gained `uid=` and `sock=`; `immurok-cli settings` and
  the TUI PAM tab show whether the daemon is actually isolated — in red and
  pinned to the top when it is not.
- **Acceptance scripts.**
  - `scripts/test-install.sh` — fetch, dependency pre-flight, build, install,
    per-assertion PASS/FAIL/SKIP. It refuses to run as root (half the assertions
    are "an unprivileged user cannot do this", and root passes them all), tells
    the four device states apart (`disconnected` / `unpaired` / `unverified` /
    `ready`) instead of emitting nonsense failures on an unpaired machine, and
    never reports "all passed" when something was skipped.
  - `scripts/test-isolation.sh` — six assertions, each of which *must* fail:
    delete the socket, squat in the runtime directory, kill the daemon, read
    `pairing.json`, modify the binary, and send `AUTH` straight down the socket.
  - `scripts/diag-device.sh` — dumps the device's raw reply frames instead of
    local inference, for the case where every local view says "Connected /
    Paired: Yes" while authentication silently fails.
- **Agent context for log attribution is registered in memory** by the daemon
  on `AGENT_APPROVE`, keyed by the `SO_PEERCRED` pid, replacing the `0600`
  marker files the daemon could no longer read. It is still only used to label
  log lines, never to grant anything — and it no longer requires a
  world-writable directory under `/run` or risks leaking the command text of
  `imk run --agent` to other local users.

### Fixed

- **A factory-reset device no longer fails silently everywhere while every
  screen says things are fine.** With the host's `pairing.json` still in place,
  the firmware's pre-pair whitelist rejects every authentication command with
  `0xF2` (`NOT_PAIRED`), so `sudo`/`pkexec`/`imk` fall back to a password while
  the UI keeps reporting `Paired: Yes`. The device said so twice and the host
  heard neither: the `paired` byte parsed out of `GET_STATUS` was read by
  nobody, and the `0xF2` replies were swallowed by the challenge's "unknown
  response → assume verified" compatibility branch.
  - The self-reported `paired` bit is now honoured: no challenge, no
    `is_device_verified` (which already gates key sync), and a desktop
    notification. **The session stays up** — re-pairing itself needs a live BLE
    session, and tearing it down here would leave the user permanently stuck.
  - `STATUS` gained a `device_unpaired` field (appended, so positional clients
    are unaffected); the CLI prints `No (device says so)` with the command to
    run, and the TUI shows a banner. The flag is cleared as soon as pairing
    succeeds, so the warning does not linger into the next reconnect.
- **`pair` no longer bounces you back with "Already paired. Unpair first"** when
  the device denies the pairing — in that state the local record is waste paper,
  and `status` was already telling you to run `pair`.
- **The challenge retries once before falling back to old-firmware
  compatibility.** Genuinely old firmware fails consistently; a misrouted
  response is a one-off. Both raw frames are logged.
- **The daemon waits for the BLE helper to be ready before sending commands.**
  It used to start writing ~49 ms before the helper attached to D-Bus: the first
  `GET_STATUS` write failed immediately and was skipped by an `if let Ok(rsp)`,
  the challenge failed the same way and landed in the "assuming verified" arm —
  a session that had never exchanged a single byte reported
  `is_device_verified=true`. The session now awaits `READY` (5 s cap) and ends
  itself for a reconnect otherwise, and a failed challenge ends the session
  instead of claiming verification.
- **`pkexec`'s `exec.path` no longer points into a user-writable directory.**
  The PAM helper was installed to `~/.local/bin`, where any process of that user
  could rewrite it and wait for one admin authorization to get root. Binaries
  now install to `/usr/local/bin`.
- **A socket bind failure exits with status 1.** Previously any `select!` branch
  returning ended `main()` with exit 0, so `Restart=on-failure` never fired and
  fingerprint auth died silently (falling back to passwords, with no explanation
  anywhere).
- **Screen-lock detection rewritten on logind's `LockedHint`** — the system
  daemon has no session bus, so the old path logged an `EACCES` every 5 seconds
  and touch-to-unlock silently did nothing. Lock/unlock now name the owner's
  graphical session explicitly; the argument-less form acts on the caller's own
  session, which a system daemon does not have.
- **The authorization dialog's exit is no longer read as "user cancelled".**
  Under the system daemon the GTK process necessarily exits at once (no
  display), which rejected every `imk run --agent`. With no
  `DISPLAY`/`WAYLAND_DISPLAY` no dialog is spawned and the touch-only gate is
  used.
- **The SSH agent socket no longer locks out its owner** — `0600` plus a
  same-uid check was unreachable once the daemon changed users. It is `0666`
  with the same session check as the management commands; the containing
  directory is not writable by ordinary users, so the permission bits do not
  widen the boundary.
- **The unit no longer sets `SupplementaryGroups=bluetooth`** — the group does
  not exist on Arch and systemd treats a missing group as a hard failure, so the
  unit would not start at all. The installer runs `usermod` when the group is
  present.
- **`immurok-cli settings` works before pairing.** It now also reports whether
  the daemon is isolated and how PAM is wired — purely local facts, and a fresh
  unpaired install is exactly when you want to see them. Writes are still gated.
- **PAM `connect()` is non-blocking with a 2 s cap.** `SO_SNDTIMEO` does not
  cover `connect()`, and `AF_UNIX` blocks indefinitely when the peer exists with
  a full accept queue — this was the only unbounded wait on the PAM path, and it
  sat before the Ctrl+C escape loop.

### Changed

- Workspace crates bumped to **0.6.0**.
- **`make install` installs a system unit and a dedicated user** instead of a
  user unit in `~/.local/bin`. All root steps are collected into
  `scripts/install-root.sh` and invoked with a single `sudo` — with no tty,
  sudo's timestamp is keyed by ppid and every make recipe is a new parent, so
  scattered `sudo` calls would drop into a password prompt halfway through an
  install that has just broken fingerprint `sudo`. The helper gained
  `migrate-daemon` / `purge-daemon`, which create the user, move the data, stop
  the old user-level daemon, start the system one and remove the stale polkit
  overrides in one step, leaving no window with two daemons fighting over BLE.
  `purge-daemon` keeps your data unless asked otherwise.
- **The polkit sandbox overrides are deleted rather than amended.** What blocked
  us was `ProtectHome=yes` (it masks all of `/run/user/<uid>`), not
  `ProtectSystem=strict`: connecting to a socket under a read-only `/run`
  succeeds, since the kernel's `EROFS` check applies to REG/DIR/LNK only. The
  previously specified `ReadWritePaths=/run/immurok` would actively break the
  machine — `RuntimeDirectory=` is removed when the service stops, and pointing
  a unit at a missing path fails it with 226/NAMESPACE, which would take polkit
  itself down.
- Verified end to end on **Arch**, **Fedora 44 with SELinux Enforcing**
  (no `.te` policy module needed — `policykit_auth_t` reaches the socket under
  `/run/immurok`) and **Ubuntu 24.04** (older polkit's setuid helper path, PAM
  modules in `/lib/x86_64-linux-gnu/security`, `common-auth` in the polkit-1
  template).

## 0.5.0 — 2026-08-05

### Added

- **Two-host support — pair one device with two computers.** A device can now be
  registered to a second host and switched between them by fingerprint. Daemon,
  CLI and TUI gained full dual-host awareness:
  - **Register a second host** with a registered fingerprint **plus** the device
    button — two physical-presence gates, no shared secret to type. The CLI/TUI
    shows a `(1/2) (2/2)` two-step prompt driven by the device's `PAIR:PROGRESS`
    events, which distinguish first-host pairing from second-host enrollment.
  - **Switch fingerprint (slot 5)** — enroll / delete / list it like any finger.
    It hands the device to the other host, which advertises a **distinct BLE
    identity per slot**, so macOS/Linux see each side as a separate peripheral.
  - **Cross-slot unpair** — clear the *other* host's slot from either machine.
    The device recycles that slot's SNV bond and rotates its BLE address
    (generation bump) so the removed host won't auto-reconnect; where the peer
    address is unknown it degrades gracefully. A follow-up prompt walks you
    through forgetting the stale device in your Bluetooth settings.

- **`slot` CLI commands** — `slot status`, per-slot `unpair`, and a separate
  whole-device `factory-reset`. Unpair no longer implies a factory reset: it
  clears the current slot, and full-device erase is its own command.

- **TUI Hosts panel (`H` menu).** Lists both host slots, marks *this computer*
  prominently, shows bound/empty state, and drives unpair by explicit slot
  selection. The fingerprint block now shows all 6 slots (5 fingers + switch).
  During enrollment you can choose an auth finger or the switch finger.

- **Socket protocol: `SLOT:*` and `FACTORY_RESET`.** New commands expose slot
  status / clear and whole-device reset; `PAIR:RESET` is redirected to clear the
  current slot. `DaemonClient` gained per-call read timeouts.

### Fixed

- **`unpair` no longer deadlocks when the device is parked on an unpaired
  slot.** An unsatisfiable pre-check trapped the flow; recovery paths
  (factory-reset / current-slot clear) were opened up for that state, and the
  single-host `unpair` pre-check no longer walks into an irreversible trap.
- **`unpair` wording corrected** in three places — going offline isn't a one-way
  door, the slot really is cleared, and old firmware still needs a
  factory-reset.
- **`0x34` / `0x03` device notifications no longer land in the command-reply
  channel** — they were occasionally decoded as generic command responses. Added
  a dedicated `classify_unpair_response` pure decoder.
- **Pairing progress hardened** — an atomic claim filters button notifications
  arriving outside the pairing window; the second-host button window gets an
  extra round.
- **Slot status no longer labels an empty active slot as "this computer."**
- **No startup black screen**, and the Hosts panel is no longer wiped by a
  transient BLE failure.
- TUI brought in line with the CLI: `u` requires confirmation, pairing matches
  exactly, and unpair is no longer blocked by a local gate.

### Changed

- Workspace crates bumped to **0.5.0**.
- TUI Hosts rows restyled to `▶ [✓] Host N: paired`; the device-location marker
  appears only when abnormal and stays within 44 columns.

## 0.4.0 — 2026-07-31

### Added

- **SSH takeover toggle.** The daemon can now route *all* SSH through the
  device-backed, fingerprint-gated SSH agent by managing a sentinel-delimited
  block in `~/.ssh/config`:

  ```
  # >>> immurok managed — do not edit (toggle in immurok app) >>>
  Host *
      IdentityAgent /run/user/<uid>/immurok/agent.sock
  # <<< immurok managed <<<
  ```

  Everything outside the block is preserved; the file is rewritten atomically
  with `0600` permissions, and an unterminated block is left untouched rather
  than being repaired destructively. The setting is persisted, applied at
  startup and reconciled against the on-disk config, so the file and the
  daemon state cannot drift apart.

  Toggle it with `h` in the TUI Unlock panel, or over the socket protocol with
  `SET:SSH_TAKEOVER:1` / `:0`. On-disk SSH keys stay in place as a fallback —
  turning the toggle off reverts SSH to your system keys/agent.

### Fixed

- **Ctrl+C now cancels the fingerprint wait for `sudo`, `ssh` and `git`.**
  Previously the wait ignored Ctrl+C and the device LED kept blinking until the
  request timed out (up to 40 s for PAM, 50 s for the SSH agent).
  - *PAM side:* `sudo` masks `SIGINT` for the duration of `pam_authenticate`, so
    the signal never reached the module. It is now received through a
    `signalfd` folded into the existing `select()` loop — deliberately not a
    `sigaction` handler, since a handler installed from a `dlopen`'d module
    becomes a dangling pointer once libpam `dlclose()`s it in `pam_end`.
    `signalfd` delivers `SIGINT` even while it is masked. On Ctrl+C the module
    cancels the wait and falls through to the password prompt (`pam_unix`).
  - *SSH agent side:* the signing loop only awaited the fingerprint gate and its
    deadline, so a client killed with Ctrl+C left the gate (and the device LED)
    open. The client socket is now watched for disconnect and the gate is
    cancelled directly.

- **Device fingerprint gate is released immediately on cancel.** The cancel path
  used to enqueue `GATE_CANCEL` through the BLE send queue, but the single BLE
  worker was parked inside the auth request waiting for the touch — so the
  queued command sat behind it and never reached the device (and blocked the
  cancelling task for the whole timeout). Cancellation now wakes the parked wait
  through the `auth_dialog_cancel` notify, which writes `GATE_CANCEL` straight to
  the device; the LED stops right away.

- **GNOME screen unlock no longer stalls a typed password.** On the
  `gdm-password` PAM stack, `pam_immurok` ran as `auth sufficient` and blocked in
  its wait loop; its keypress-cancel fallback reads `/dev/tty`, which the
  graphical unlock has no controlling terminal for, so the loop could not be
  cancelled and a typed password was stalled until the 40 s timeout or three
  denied scans. `pam_sm_authenticate` now returns `PAM_IGNORE` for
  `gdm-password` before doing any socket work, letting the stack fall through to
  `pam_unix`.

  Fingerprint unlock for this service is unaffected — it is handled out of band
  by the daemon via `loginctl unlock-session` on a proactive match. `sudo` and
  `polkit-1` still block on touch-to-auth; the decision is keyed on the service
  name rather than tty presence, so tty-less `polkit-1` does not regress. A side
  effect is that the daemon no longer spawns a GTK dialog for `gdm-password`,
  which also removes an input-grab conflict on the lock screen. Covered by a new
  C unit test (`make test` in `pam/`).

- **PAM module is installed via atomic rename.** `install`/`cp` overwrote
  `pam_immurok.so` in place while the running `sudo` still had that exact file
  mapped; mutating its bytes underneath corrupted the module's code pages and
  crashed it in `dlclose` at `pam_end` (SIGSEGV into a scrambled fini address).
  The build now writes a temp file and `mv -f`s it into place (same-directory
  atomic rename), so the inode a running process mapped stays intact and later
  authentications pick up the new one.

- **Long commands are fully readable in the agent authorization dialog.** The
  command is rendered as a word-wrapped pill of up to 5 lines (ellipsized past
  that) instead of a single truncated line; the window widened from 420 to
  560 px and the hard truncation limit went from 80 to 300 characters. The plain
  PAM fingerprint dialog layout is unchanged.

### Changed

- The TUI SSH takeover toggle applies immediately on `h` — consistent with the
  sudo/polkit/screen switches — instead of opening a y/n confirm dialog. The
  consequence is shown as a non-blocking note on the result line. Dropping the
  dialog also removed the refresh race the confirm flow had to guard against
  (no dialog can now stay open across the 2 s refresh).
