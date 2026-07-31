# SSH Takeover Toggle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an SSH toggle to the Linux TUI's Unlock panel that, when ON, routes all interactive SSH through the daemon's fingerprint device-key agent, and when OFF reverts SSH to the system keys — applied by a managed block in `~/.ssh/config`.

**Architecture:** A new daemon module (`ssh_config`) idempotently writes/removes a sentinel-delimited `Host * / IdentityAgent <sock>` block at the top of `~/.ssh/config`. The existing settings pipeline (`SET:` command → daemon `Settings` → persisted) gains an `ssh_takeover` flag; the daemon applies the file effect on change and reconciles at startup. The TUI adds a confirm-gated toggle mirroring the existing `KeyDeleteConfirm` mode.

**Tech Stack:** Rust, tokio, serde, ratatui/crossterm (TUI). No new runtime dependencies (one dev-dependency: `tempfile`).

## Global Constraints

- Scope: `app-linux-rs/` only. Do not modify firmware, macOS app, PAM C code, or `certificate/`.
- All user-facing strings are English (Linux app convention — no localization).
- Managed block sentinels (exact, byte-for-byte):
  - BEGIN: `# >>> immurok managed — do not edit (toggle in immurok app) >>>`
  - END: `# <<< immurok managed <<<`
- Socket path written into the block is the resolved absolute path
  (`$XDG_RUNTIME_DIR/immurok/agent.sock`, fallback `/run/user/<uid>/immurok/agent.sock`);
  never rely on ssh-side env expansion.
- No `IdentitiesOnly` — on-disk keys remain a fallback.
- Toggle keybinding: `h` (Dashboard context).
- File permissions: `~/.ssh` = 0700, `~/.ssh/config` = 0600.
- Do not auto-commit beyond the per-task commits in this plan; the human runs/reviews.

---

### Task 1: `ssh_config` daemon module (file manipulation + tests)

Self-contained file-manipulation unit for the managed `~/.ssh/config` block. Fully unit-tested against a temp HOME.

**Files:**
- Create: `crates/immurok-daemon/src/ssh_config.rs`
- Modify: `crates/immurok-daemon/src/main.rs` (register `mod ssh_config;`)
- Modify: `crates/immurok-daemon/Cargo.toml` (add `tempfile` dev-dependency)

**Interfaces:**
- Produces:
  - `pub fn enable(home: &Path, agent_sock: &Path) -> std::io::Result<()>`
  - `pub fn disable(home: &Path) -> std::io::Result<()>`
  - `pub fn is_enabled(home: &Path) -> bool`
  - `pub fn apply(enabled: bool) -> std::io::Result<()>` (resolves HOME + agent socket from env, calls enable/disable)

- [ ] **Step 1: Add `tempfile` dev-dependency**

In `crates/immurok-daemon/Cargo.toml`, add at end of file:

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Write the module with the full test suite (tests first)**

Create `crates/immurok-daemon/src/ssh_config.rs`:

```rust
//! Manage the immurok-owned block in ~/.ssh/config that routes SSH through
//! the daemon's fingerprint-gated SSH agent (see docs spec 2026-07-31).
//!
//! enable() prepends a sentinel-delimited `Host * / IdentityAgent <sock>`
//! block; disable() removes it. Both preserve all user content outside the
//! block. The socket path is written as a resolved absolute path — ssh does
//! no env expansion for us.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const BEGIN: &str = "# >>> immurok managed — do not edit (toggle in immurok app) >>>";
const END: &str = "# <<< immurok managed <<<";

/// Managed block text (always ends with a trailing newline).
fn block(agent_sock: &Path) -> String {
    format!(
        "{BEGIN}\nHost *\n    IdentityAgent {}\n{END}\n",
        agent_sock.display()
    )
}

/// Remove any existing managed block from `content`, returning remaining user
/// lines joined by '\n' with surrounding blank lines trimmed.
fn strip_block(content: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut in_block = false;
    for line in content.lines() {
        if line.trim() == BEGIN {
            in_block = true;
            continue;
        }
        if in_block {
            if line.trim() == END {
                in_block = false;
            }
            continue;
        }
        out.push(line);
    }
    out.join("\n").trim_matches('\n').to_string()
}

fn config_path(home: &Path) -> PathBuf {
    home.join(".ssh").join("config")
}

fn write_atomic(path: &Path, content: &str) -> io::Result<()> {
    let tmp = path.with_extension("immurok-tmp");
    std::fs::write(&tmp, content)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Ensure the managed block (pointing at `agent_sock`) sits at the top of
/// `<home>/.ssh/config`. Idempotent: replaces an existing block. Creates
/// ~/.ssh (0700) and the config file (0600) if missing.
pub fn enable(home: &Path, agent_sock: &Path) -> io::Result<()> {
    let ssh_dir = home.join(".ssh");
    std::fs::create_dir_all(&ssh_dir)?;
    std::fs::set_permissions(&ssh_dir, std::fs::Permissions::from_mode(0o700))?;

    let path = config_path(home);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let body = strip_block(&existing);

    let mut content = block(agent_sock);
    if !body.is_empty() {
        content.push('\n');
        content.push_str(&body);
        content.push('\n');
    }
    write_atomic(&path, &content)
}

/// Remove the managed block. No-op if the file or block is absent. Preserves
/// all other content.
pub fn disable(home: &Path) -> io::Result<()> {
    let path = config_path(home);
    let existing = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !existing.contains(BEGIN) {
        return Ok(());
    }
    let body = strip_block(&existing);
    let content = if body.is_empty() {
        String::new()
    } else {
        format!("{body}\n")
    };
    write_atomic(&path, &content)
}

/// Whether the managed block is currently present.
pub fn is_enabled(home: &Path) -> bool {
    std::fs::read_to_string(config_path(home))
        .map(|s| s.contains(BEGIN))
        .unwrap_or(false)
}

/// Resolve the daemon's SSH agent socket path from the environment — same
/// XDG_RUNTIME_DIR (fallback /run/user/<uid>) convention as the daemon binds.
fn resolved_agent_sock() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }))
        });
    runtime
        .join("immurok")
        .join(immurok_common::protocol::AGENT_SOCKET_NAME)
}

/// Apply the toggle state using environment-resolved HOME + agent socket.
pub fn apply(enabled: bool) -> io::Result<()> {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "HOME not set"))?;
    if enabled {
        enable(&home, &resolved_agent_sock())
    } else {
        disable(&home)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn sock() -> PathBuf {
        PathBuf::from("/run/user/1000/immurok/agent.sock")
    }

    #[test]
    fn enable_on_missing_file_creates_block_at_top() {
        let home = tempfile::tempdir().unwrap();
        enable(home.path(), &sock()).unwrap();

        let cfg = home.path().join(".ssh").join("config");
        let content = std::fs::read_to_string(&cfg).unwrap();
        assert!(content.starts_with(BEGIN));
        assert!(content.contains("IdentityAgent /run/user/1000/immurok/agent.sock"));
        assert!(content.contains(END));
        // config is 0600
        let mode = std::fs::metadata(&cfg).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(is_enabled(home.path()));
    }

    #[test]
    fn enable_preserves_existing_user_content_below_block() {
        let home = tempfile::tempdir().unwrap();
        let ssh_dir = home.path().join(".ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();
        let cfg = ssh_dir.join("config");
        std::fs::write(&cfg, "Host myserver\n    HostName 1.2.3.4\n").unwrap();

        enable(home.path(), &sock()).unwrap();

        let content = std::fs::read_to_string(&cfg).unwrap();
        assert!(content.starts_with(BEGIN));
        assert!(content.contains("Host myserver"));
        assert!(content.contains("HostName 1.2.3.4"));
    }

    #[test]
    fn enable_is_idempotent_and_replaces_stale_path() {
        let home = tempfile::tempdir().unwrap();
        enable(home.path(), &PathBuf::from("/run/user/1000/immurok/agent.sock")).unwrap();
        enable(home.path(), &PathBuf::from("/run/user/1000/immurok/agent.sock")).unwrap();
        enable(home.path(), &PathBuf::from("/run/user/2000/immurok/agent.sock")).unwrap();

        let content = std::fs::read_to_string(home.path().join(".ssh").join("config")).unwrap();
        // exactly one block
        assert_eq!(content.matches(BEGIN).count(), 1);
        assert_eq!(content.matches(END).count(), 1);
        // reflects the latest path only
        assert!(content.contains("/run/user/2000/immurok/agent.sock"));
        assert!(!content.contains("/run/user/1000/immurok/agent.sock"));
    }

    #[test]
    fn disable_removes_block_keeps_user_content() {
        let home = tempfile::tempdir().unwrap();
        let ssh_dir = home.path().join(".ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();
        std::fs::write(ssh_dir.join("config"), "Host keep\n    User me\n").unwrap();

        enable(home.path(), &sock()).unwrap();
        disable(home.path()).unwrap();

        let content = std::fs::read_to_string(ssh_dir.join("config")).unwrap();
        assert!(!content.contains(BEGIN));
        assert!(!content.contains(END));
        assert!(content.contains("Host keep"));
        assert!(content.contains("User me"));
        assert!(!is_enabled(home.path()));
    }

    #[test]
    fn disable_on_missing_file_is_noop() {
        let home = tempfile::tempdir().unwrap();
        disable(home.path()).unwrap(); // must not error
        assert!(!is_enabled(home.path()));
    }

    #[test]
    fn socket_path_written_literally() {
        let home = tempfile::tempdir().unwrap();
        let odd = Path::new("/run/user/31337/immurok/agent.sock");
        enable(home.path(), odd).unwrap();
        let content = std::fs::read_to_string(home.path().join(".ssh").join("config")).unwrap();
        assert!(content.contains("IdentityAgent /run/user/31337/immurok/agent.sock"));
    }
}
```

- [ ] **Step 3: Register the module**

In `crates/immurok-daemon/src/main.rs`, add `mod ssh_config;` alongside the other `mod` declarations near the top of the file.

- [ ] **Step 4: Run the tests to verify they fail, then pass**

Run: `cargo test -p immurok-daemon ssh_config`
Expected: after Steps 1–3 compile, the six `ssh_config::tests::*` tests PASS. (If you split test-writing before implementation, they FAIL first with unresolved `enable`/`disable`/`is_enabled`.)

- [ ] **Step 5: Commit**

```bash
git add crates/immurok-daemon/src/ssh_config.rs crates/immurok-daemon/src/main.rs crates/immurok-daemon/Cargo.toml
git commit -m "feat(daemon): ssh_config module for ~/.ssh/config managed block"
```

---

### Task 2: `SetSshTakeover` protocol request + parse

Add the wire command `SET:SSH_TAKEOVER:<0|1>` to the socket protocol, mirroring the existing `SET:UNLOCK_*` variants. Independently testable via `parse_request`.

**Files:**
- Modify: `crates/immurok-common/src/socket_proto.rs` (Request enum ~line 81, doc table ~line 23, SET parse ~line 249, tests ~line 371)

**Interfaces:**
- Consumes: existing `parse_bool`, `Request` enum, `ParseError`.
- Produces: `Request::SetSshTakeover(bool)` variant; `parse_request("SET:SSH_TAKEOVER:1") == Request::SetSshTakeover(true)`.

- [ ] **Step 1: Write the failing parse test**

In `crates/immurok-common/src/socket_proto.rs`, in the existing `#[cfg(test)] mod`, add:

```rust
#[test]
fn parse_set_ssh_takeover() {
    assert_eq!(
        parse_request("SET:SSH_TAKEOVER:1").unwrap(),
        Request::SetSshTakeover(true)
    );
    assert_eq!(
        parse_request("SET:SSH_TAKEOVER:0").unwrap(),
        Request::SetSshTakeover(false)
    );
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p immurok-common parse_set_ssh_takeover`
Expected: FAIL — `Request::SetSshTakeover` does not exist.

- [ ] **Step 3: Add the enum variant**

In the `pub enum Request { ... }` (after `SetLockScreen(bool),` at ~line 98):

```rust
    SetSshTakeover(bool),
```

- [ ] **Step 4: Add the SET parse arm**

In the `"SET" => { ... match sub { ... }`, after the `"LOCK_SCREEN"` arm (~line 266):

```rust
                "SSH_TAKEOVER" => {
                    let v = parse_bool(&parts, 2, "SET:SSH_TAKEOVER", "value")?;
                    Ok(Request::SetSshTakeover(v))
                }
```

- [ ] **Step 5: Update the doc table**

In the module doc comment table (~line 26), after the `SET:LOCK_SCREEN` row add:

```text
//! | SET:SSH_TAKEOVER    | `SET:SSH_TAKEOVER:1` / `:0`        |
```

- [ ] **Step 6: Run tests to verify pass**

Run: `cargo test -p immurok-common`
Expected: PASS (new test + existing ones).

- [ ] **Step 7: Commit**

```bash
git add crates/immurok-common/src/socket_proto.rs
git commit -m "feat(common): SET:SSH_TAKEOVER protocol request"
```

---

### Task 3: Daemon settings field, apply-on-set, startup reconcile

Persist `ssh_takeover`, apply the `ssh_config` file effect when it changes, expose it in `GET:SETTINGS`, and reconcile at startup.

**Files:**
- Modify: `crates/immurok-daemon/src/settings.rs` (add field)
- Modify: `crates/immurok-daemon/src/socket.rs` (dispatch ~285, `handle_set_setting` ~1219, `handle_get_settings` ~1238)
- Modify: `crates/immurok-daemon/src/main.rs` (startup reconcile, after settings load ~line 73)

**Interfaces:**
- Consumes: `Request::SetSshTakeover(bool)` (Task 2); `ssh_config::apply` (Task 1).
- Produces: `GET:SETTINGS` response gains `:ssh=<0|1>`; `Settings.ssh_takeover: bool`.

- [ ] **Step 1: Add the settings field**

In `crates/immurok-daemon/src/settings.rs`, add to the `Settings` struct (after `lock_screen`):

```rust
    /// Route interactive SSH through the fingerprint device key via a managed
    /// block in ~/.ssh/config. Opt-in.
    #[serde(default)]
    pub ssh_takeover: bool,
```

And in `impl Default for Settings`, add `ssh_takeover: false,` to the returned struct.

- [ ] **Step 2: Dispatch the request**

In `crates/immurok-daemon/src/socket.rs` dispatch match (after `Request::SetLockScreen(v) => ...` ~line 288):

```rust
        Request::SetSshTakeover(v) => handle_set_setting(coord, "ssh_takeover", v).await,
```

- [ ] **Step 3: Handle set + apply file effect**

In `handle_set_setting` (~line 1219), add the `"ssh_takeover"` key. It must (a) update the flag, (b) apply the `~/.ssh/config` change, and (c) persist — returning an error (without persisting) if the file write fails. Replace the match/save body so the ssh case applies before saving:

```rust
async fn handle_set_setting(coord: &Arc<Coordinator>, key: &str, value: bool) -> Response {
    {
        let mut settings = coord.settings.write().await;
        match key {
            "unlock_sudo" => settings.unlock_sudo = value,
            "unlock_polkit" => settings.unlock_polkit = value,
            "unlock_screen" => settings.unlock_screen = value,
            "lock_screen" => settings.lock_screen = value,
            "ssh_takeover" => {
                // Apply the ~/.ssh/config effect first; only persist if it
                // succeeds, so settings and config never diverge.
                if let Err(e) = crate::ssh_config::apply(value) {
                    warn!("Failed to apply ssh_takeover: {}", e);
                    return Response::Error(format!("SAVE_FAILED:{}", e));
                }
                settings.ssh_takeover = value;
            }
            _ => return Response::Error("UNKNOWN_KEY".into()),
        }
        if let Err(e) = settings.save(&coord.settings_path()) {
            warn!("Failed to save settings: {}", e);
            return Response::Error(format!("SAVE_FAILED:{}", e));
        }
    }
    info!("Setting {}={}", key, value);
    Response::Ok(String::new())
}
```

- [ ] **Step 4: Expose in GET:SETTINGS**

In `handle_get_settings` (~line 1238), extend the format string to append `ssh`:

```rust
    let msg = format!(
        "sudo={}:polkit={}:screen={}:lock={}:ssh={}",
        if s.unlock_sudo { "1" } else { "0" },
        if s.unlock_polkit { "1" } else { "0" },
        if s.unlock_screen { "1" } else { "0" },
        if s.lock_screen { "1" } else { "0" },
        if s.ssh_takeover { "1" } else { "0" },
    );
```

- [ ] **Step 5: Startup reconcile**

In `crates/immurok-daemon/src/main.rs`, after the settings are loaded into the coordinator (just after the `*s = user_settings;` block ~line 73, while `user_settings` is still in scope — capture the flag before the move if needed), reconcile the config file to the persisted intent:

```rust
    // Reconcile ~/.ssh/config with the persisted ssh_takeover intent — a
    // hand-edited config must not drift from the stored setting.
    if let Err(e) = ssh_config::apply(ssh_takeover_intent) {
        tracing::warn!("ssh_takeover reconcile failed: {}", e);
    }
```

Capture `ssh_takeover_intent` from `user_settings.ssh_takeover` **before** `user_settings` is moved into the coordinator (i.e. `let ssh_takeover_intent = user_settings.ssh_takeover;` immediately before the `let mut s = coord.settings.write().await; *s = user_settings;` lines).

- [ ] **Step 6: Build + test**

Run: `cargo build -p immurok-daemon && cargo test -p immurok-daemon`
Expected: builds clean; all tests pass (Task 1 tests still green).

- [ ] **Step 7: Commit**

```bash
git add crates/immurok-daemon/src/settings.rs crates/immurok-daemon/src/socket.rs crates/immurok-daemon/src/main.rs
git commit -m "feat(daemon): persist + apply + reconcile ssh_takeover setting"
```

---

### Task 4: TUI toggle — Unlock panel row, confirm dialog, keybinding

Add the `h` = ssh toggle to the Dashboard Unlock panel, gated by a consequence-confirm mode mirroring `KeyDeleteConfirm`, and reflect state from `GET:SETTINGS`.

**Files:**
- Modify: `crates/immurok-cli/src/tui/app.rs` (Mode enum ~53, App fields ~213/247, init ~315/340, GET parse ~474, new methods)
- Modify: `crates/immurok-cli/src/tui/mod.rs` (Dashboard key ~80, new Mode arm ~209)
- Modify: `crates/immurok-cli/src/tui/widgets.rs` (Unlock height ~232, `draw_unlock` ~371, help ~1048)

**Interfaces:**
- Consumes: `GET:SETTINGS` `ssh=` field (Task 3); existing `toggle_setting`, `guard_paired`, `set_msg`, `Mode`.
- Produces: `App::ssh_takeover`, `App::action_toggle_ssh`, `App::confirm_ssh_toggle`, `App::cancel_ssh_toggle`, `Mode::SshToggleConfirm`.

- [ ] **Step 1: Add the confirm Mode variant**

In `crates/immurok-cli/src/tui/app.rs`, in `pub enum Mode` (after `KeyDeleteConfirm,` ~line 59):

```rust
    /// Confirm SSH takeover toggle (y / n). Stores the pending target state.
    SshToggleConfirm,
```

- [ ] **Step 2: Add App state fields**

In the `App` struct, next to `unlock_screen` (~line 213):

```rust
    pub ssh_takeover: bool,
```

And next to `pending_delete` (~line 248):

```rust
    /// Pending SSH takeover target state while in SshToggleConfirm.
    pub pending_ssh_enable: Option<bool>,
```

In the `App` initializer, next to `unlock_screen: true,` (~line 317) add `ssh_takeover: false,`, and next to `pending_delete: None,` (~line 340) add `pending_ssh_enable: None,`.

- [ ] **Step 3: Parse `ssh=` from GET:SETTINGS**

In `crates/immurok-cli/src/tui/app.rs` (~line 477), add a match arm:

```rust
                            "ssh" => self.ssh_takeover = v == "1",
```

- [ ] **Step 4: Add the toggle + confirm methods**

Add near the other `action_toggle_*` methods (~line 1003):

```rust
    pub fn action_toggle_ssh(&mut self) {
        if !self.guard_paired() {
            return;
        }
        if self.busy {
            return;
        }
        let enabling = !self.ssh_takeover;
        self.pending_ssh_enable = Some(enabling);
        self.mode = Mode::SshToggleConfirm;
        let prompt = if enabling {
            "Route ALL SSH through the fingerprint device key. Hosts must have the device key authorized; on-disk keys stay as fallback. [y] confirm  [n/Esc] cancel"
        } else {
            "Revert SSH to your system keys/agent. [y] confirm  [n/Esc] cancel"
        };
        self.set_msg(prompt, MessageStyle::Yellow);
    }

    pub fn cancel_ssh_toggle(&mut self) {
        self.pending_ssh_enable = None;
        self.mode = Mode::Normal;
        self.set_msg("Cancelled.", MessageStyle::Dim);
    }

    pub fn confirm_ssh_toggle(&mut self) {
        self.pending_ssh_enable = None;
        self.mode = Mode::Normal;
        // toggle_setting derives the new value from the current flag.
        self.toggle_setting("ssh", "SSH_TAKEOVER", self.ssh_takeover);
    }
```

- [ ] **Step 5: Bind the key + confirm mode in mod.rs**

In `crates/immurok-cli/src/tui/mod.rs`, in the `Tab::Dashboard` match (after `KeyCode::Char('k') => app.action_toggle_screen(),` ~line 82):

```rust
                                KeyCode::Char('h') => app.action_toggle_ssh(),
```

And add a new top-level `Mode` arm after `app::Mode::KeyDeleteConfirm => { ... }` (~line 209):

```rust
                    app::Mode::SshToggleConfirm => match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                            app.confirm_ssh_toggle();
                        }
                        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                            app.cancel_ssh_toggle();
                        }
                        _ => {}
                    },
```

- [ ] **Step 6: Render the toggle row + resize the box + help**

In `crates/immurok-cli/src/tui/widgets.rs`:

Bump the Unlock box height (~line 232):

```rust
            Constraint::Length(7),         // Unlock toggles
```

Add the row in `draw_unlock` (after the `long-press lock` row ~line 375):

```rust
        toggle_row("h", "ssh", app.ssh_takeover),
```

Update the Dashboard help line (~line 1048):

```rust
        Line::from(vec![key("s o k L h"), Span::raw("Toggle sudo / polkit / screen / long-press lock / ssh")]),
```

- [ ] **Step 7: Build + test**

Run: `cargo build -p immurok-cli && cargo test --workspace`
Expected: builds clean; whole workspace test suite passes.

- [ ] **Step 8: Commit**

```bash
git add crates/immurok-cli/src/tui/app.rs crates/immurok-cli/src/tui/mod.rs crates/immurok-cli/src/tui/widgets.rs
git commit -m "feat(tui): SSH takeover toggle in Unlock panel with confirm"
```

---

### Task 5: Manual end-to-end verification

No code — a smoke checklist run by the human against a real daemon/device. Document the outcome.

- [ ] **Step 1: Build + install**

Run: `make install` (or `cargo build --release` then the install step per README). Restart the daemon: `systemctl --user restart immurok-daemon`.

- [ ] **Step 2: Toggle ON**

In the TUI Dashboard, press `h`; confirm the consequence prompt appears; press `y`. Verify:
- `~/.ssh/config` now begins with the managed block pointing at the actual agent socket (`cat ~/.ssh/config`, check path matches `echo $XDG_RUNTIME_DIR/immurok/agent.sock`).
- `ssh -G github.com | grep -i identityagent` shows the immurok socket.
- The Unlock panel shows `h  ssh  ● on`.

- [ ] **Step 3: Toggle OFF**

Press `h`, confirm OFF prompt, press `y`. Verify the block is gone from `~/.ssh/config`, user content (if any) intact, panel shows `○ off`.

- [ ] **Step 4: Reconcile**

With the toggle ON, manually delete the block from `~/.ssh/config`, then `systemctl --user restart immurok-daemon`. Verify the block is restored on startup.

- [ ] **Step 5: Record results**

Note pass/fail for each step. If any fail, file follow-up before considering the feature done.

---

## Self-Review

**Spec coverage:**
- Unlock panel SSH toggle → Task 4 ✓
- ON routes all ssh via device key (managed block, Host *) → Tasks 1, 3 ✓
- OFF reverts to system keys (block removed) → Tasks 1, 3 ✓
- Confirm prompt on both ON and OFF → Task 4 (Mode::SshToggleConfirm, both prompts) ✓
- Mechanism A (~/.ssh/config managed block, absolute socket path, no IdentitiesOnly, 0600/0700) → Task 1 ✓
- Keybinding `h` → Task 4 ✓
- Single writer = daemon → Tasks 1, 3 (TUI only sends SET:) ✓
- Startup reconcile → Task 3 ✓
- Tests against temp HOME → Task 1 ✓
- English-only strings → Tasks 1, 4 (all copy is English) ✓
- Scope app-linux-rs only → all files under app-linux-rs ✓

**Placeholder scan:** none — every step has concrete code/commands.

**Type consistency:** `ssh_config::{enable,disable,is_enabled,apply}` signatures consistent across Tasks 1/3. `Request::SetSshTakeover(bool)` consistent Tasks 2/3. `App::{action_toggle_ssh,confirm_ssh_toggle,cancel_ssh_toggle}` + `Mode::SshToggleConfirm` + `pending_ssh_enable` consistent Task 4. GET:SETTINGS `ssh=` produced in Task 3 Step 4, consumed in Task 4 Step 3.
