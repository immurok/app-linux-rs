# Changelog

All notable changes to the immurok Linux companion (daemon, CLI/TUI and PAM
module) are documented here. Versions follow the workspace crate version.

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
