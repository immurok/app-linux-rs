# SSH Takeover Toggle — Design

Date: 2026-07-31
Scope: `app-linux-rs/` only. No changes outside this directory.

## Goal

Add an **SSH** toggle to the Unlock panel of the Linux TUI. Turning it **ON**
routes all interactive SSH (and git-over-SSH) through the fingerprint device
key served by the daemon's SSH agent. Turning it **OFF** reverts SSH to the
user's system keys/agent. Both transitions prompt the user about the
consequences before applying.

Today none of this is automatic: the only place SSH is routed to the device
agent is inside the `imk run --agent --` wrapper, which forces `SSH_AUTH_SOCK`
for that one child process. Manual `ssh` / `git push` in the user's own shell
never touches the device agent unless the user hand-edits their environment or
ssh config. This feature makes that a one-switch operation.

All user-facing strings are English (Linux app convention).

## Decisions (settled during brainstorming)

- **Mechanism: managed block in `~/.ssh/config`** (chosen over `environment.d`
  and shell rc). Affects only SSH, takes effect immediately across all shells
  and terminals, is fully reversible, and leaves on-disk keys working as a
  fallback.
- **Do NOT force device-key-only.** No `IdentitiesOnly`. The device key is
  offered first (from `IdentityAgent`); on-disk keys remain as fallback so a
  host that has not authorized the device key still works and the user is never
  locked out.
- **Toggle key: `h`** (`s` / `o` / `k` already taken by sudo / polkit / screen).
- **Single writer: the daemon.** Consistent with the existing settings
  architecture (`SET:` → daemon `Settings` → persisted). The daemon knows
  `HOME` and the absolute agent socket path it actually bound.

## Managed block format

Written at the **top** of `~/.ssh/config` (first-match-wins in ssh_config means
a top block guarantees takeover of `IdentityAgent` for all hosts). The socket
path is the **resolved absolute path** the daemon bound (`$XDG_RUNTIME_DIR/immurok/agent.sock`,
falling back to `/run/user/<uid>/immurok/agent.sock`) — no reliance on ssh-side
environment variable expansion.

```
# >>> immurok managed — do not edit (toggle in immurok app) >>>
Host *
    IdentityAgent /run/user/1000/immurok/agent.sock
# <<< immurok managed <<<
```

Sentinels delimit the block for idempotent enable/replace and clean removal.

## Components

### 1. `immurok-common/src/protocol.rs`
- New request: `Request::SetSshTakeover(bool)`, parsed from `SET:SSH_TAKEOVER:<0|1>`
  alongside the existing `SET:UNLOCK_*` handling.
- `GET:SETTINGS` response string gains an `ssh=<0|1>` field.

### 2. `immurok-daemon/src/settings.rs`
- `Settings` gains `ssh_takeover: bool`, default `false`.
- Serialized/persisted with the existing settings file (serde default so old
  files load without the field).

### 3. `immurok-daemon/src/ssh_config.rs` (new module)
Pure file-manipulation unit, independently testable.
- `enable(home: &Path, agent_sock: &Path) -> Result<(), String>`: idempotently
  ensure the managed block (with the given socket path) sits at the top of
  `<home>/.ssh/config`. Create `~/.ssh` (0700) and `~/.ssh/config` (0600) if
  absent. If a block already exists, replace it (path may have changed).
- `disable(home: &Path) -> Result<(), String>`: remove the managed block by
  sentinels; leave the rest of the file untouched. No-op if absent or file
  missing.
- `is_enabled(home: &Path) -> bool`: block present?
- Block detection/removal is sentinel-based and preserves all user content
  outside the block. Atomic write (temp file + rename), mode 0600.
- Test-injectable HOME via argument (tests pass a temp dir), mirroring the
  `IMMUROK_*` env-injection style used by `scripts/test-pam-helper.sh`.

### 4. `immurok-daemon/src/socket.rs`
- `handle_set_setting` handles `ssh_takeover`: update + persist `Settings`,
  then call `ssh_config::enable(home, agent_sock)` / `disable(home)`. On file
  error, return an error response (and do not leave settings/config diverged —
  persist only after the file op succeeds, or roll back the in-memory flag on
  failure).
- `handle_get_settings` appends `ssh=<0|1>`.
- The handler needs `HOME` and the absolute agent socket path; thread these via
  the `Coordinator` (add fields set at construction in `main.rs`) rather than
  re-deriving.

### 5. `immurok-daemon/src/main.rs`
- On startup, **reconcile**: call `ssh_config::enable`/`disable` to match the
  persisted `ssh_takeover`, so a hand-edited `~/.ssh/config` cannot drift from
  the stored intent.
- Provide `HOME` + resolved `agent_sock` to the coordinator for the socket
  handler to use.

### 6. `immurok-cli/src/tui/app.rs`
- `App` gains `ssh_takeover: bool` (default `false`).
- Parse `ssh=` from the `GET:SETTINGS` response (line ~474 area).
- `action_toggle_ssh()`: guard-paired, then open a confirm dialog (do not send
  `SET:` directly — confirm first).
- New `ConfirmState` variant (e.g. `SshTakeoverConfirm { enabling: bool }`),
  following the existing `KeyDeleteConfirm` pattern. On `y`, call
  `toggle_setting("ssh", "SSH_TAKEOVER", self.ssh_takeover)`. On `n`, dismiss.

### 7. `immurok-cli/src/tui/widgets.rs`
- `draw_unlock`: add `toggle_row("h", "ssh", app.ssh_takeover)`; bump the Unlock
  box height (`Constraint::Length(6)` → `7`) and the outer layout constraint.
- Add the confirm-dialog copy:
  - ON: `Route ALL SSH through the fingerprint device key. Hosts must have the device key authorized; on-disk keys stay as fallback. Continue? [y/n]`
  - OFF: `Revert SSH to your system keys/agent. Continue? [y/n]`
- Help line: add `h` → toggle ssh.

### 8. `immurok-cli/src/tui/mod.rs`
- Bind `KeyCode::Char('h')` → `app.action_toggle_ssh()` (Dashboard context,
  same guard as the other unlock toggles).
- Route `y`/`n` to the new confirm state alongside `KeyDeleteConfirm`.

## Data flow

```
TUI: press 'h'
  → action_toggle_ssh() → open SshTakeoverConfirm{enabling}
  → user presses 'y'
  → toggle_setting → SET:SSH_TAKEOVER:<0|1> over daemon socket
daemon: handle_set_setting("ssh_takeover", v)
  → ssh_config::enable(home, agent_sock)  (or disable)
  → persist Settings
  → OK
TUI: Refresh → GET:SETTINGS → parse ssh= → toggle reflects new state
```

## Error handling

- File op failure (`~/.ssh/config` unwritable): daemon returns
  `SAVE_FAILED:<detail>`; TUI shows it red; in-memory flag not committed so UI
  stays consistent with disk.
- Agent socket path unresolved: should never happen (daemon already bound it);
  reconcile/enable uses the same resolved path the agent listens on.
- Startup reconcile failure is logged (`warn!`) and non-fatal — the daemon
  still runs; the toggle just won't reflect until it can write.

## Testing

Unit tests in `ssh_config.rs` against a temp HOME:
- enable on missing file → creates `~/.ssh/config` (0600) with block at top.
- enable when block absent but file has user content → block prepended, user
  content preserved verbatim below.
- enable when block already present with a different socket path → block
  replaced, single block, user content intact.
- disable → block removed, user content intact; disable when absent → no-op.
- `is_enabled` reflects presence.
- socket path with a different uid is written literally (no env expansion).

## Out of scope

- macOS app, firmware, PAM, certificate — untouched.
- No `IdentitiesOnly` / device-key-only mode (decided against).
- No `environment.d` / shell rc mechanisms.
- Tamper-detection of user edits inside the block beyond startup reconcile.
