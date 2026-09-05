//! Where the daemon keeps its sockets, persistent state and logs.
//!
//! The daemon runs as the dedicated system user `immurok` from a systemd
//! system unit, which injects `RUNTIME_DIRECTORY` / `STATE_DIRECTORY` /
//! `LOGS_DIRECTORY`. Every resolver falls back to the fixed system path, so a
//! hand-started daemon (development, or a distro without systemd) still lands
//! where the PAM module and the CLI look for it. The `IMMUROK_*` overrides win
//! over both so a developer can run an unprivileged daemon out of a scratch
//! directory.
//!
//! These are deliberately *machine-level* paths: the daemon runs with
//! `ProtectHome=yes` and must never derive anything from `$HOME`.
//! [`user_dir`] is the one exception — per-user CLI state (firmware download
//! cache) stays in the invoking user's home, where the CLI can write it.

use std::path::PathBuf;

use crate::protocol;

/// First non-empty entry of a systemd-style path list.
///
/// systemd hands `RUNTIME_DIRECTORY` and friends as a colon-separated list
/// when a unit declares more than one directory. We only ever declare one, but
/// splitting keeps us honest if that changes.
fn first_entry(value: &str) -> Option<&str> {
    value.split(':').map(str::trim).find(|s| !s.is_empty())
}

/// Resolve a directory: explicit override, then what systemd injected, then
/// the compiled-in system path.
fn resolve(override_var: Option<&str>, systemd_var: Option<&str>, fallback: &str) -> PathBuf {
    override_var
        .and_then(first_entry)
        .or_else(|| systemd_var.and_then(first_entry))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(fallback))
}

fn env(var: &str) -> Option<String> {
    std::env::var(var).ok()
}

/// Runtime directory holding the sockets (`/run/immurok`).
pub fn runtime_dir() -> PathBuf {
    resolve(
        env("IMMUROK_RUNTIME_DIR").as_deref(),
        env("RUNTIME_DIRECTORY").as_deref(),
        protocol::SYSTEM_RUNTIME_DIR,
    )
}

/// Persistent state — pairing, settings, key caches (`/var/lib/immurok`, 0700).
pub fn state_dir() -> PathBuf {
    resolve(
        env("IMMUROK_STATE_DIR").as_deref(),
        env("STATE_DIRECTORY").as_deref(),
        protocol::SYSTEM_STATE_DIR,
    )
}

/// Daemon log directory (`/var/log/immurok`).
pub fn log_dir() -> PathBuf {
    resolve(
        env("IMMUROK_LOG_DIR").as_deref(),
        env("LOGS_DIRECTORY").as_deref(),
        protocol::SYSTEM_LOG_DIR,
    )
}

/// PAM / CLI socket. `IMMUROK_SOCKET` overrides the whole path (not just the
/// directory) because that is what a developer running two daemons wants.
pub fn pam_socket() -> PathBuf {
    match env("IMMUROK_SOCKET").as_deref().and_then(first_entry) {
        Some(p) => PathBuf::from(p),
        None => runtime_dir().join(protocol::PAM_SOCKET_NAME),
    }
}

/// Fingerprint-gated SSH agent socket.
pub fn agent_socket() -> PathBuf {
    match env("IMMUROK_AGENT_SOCKET").as_deref().and_then(first_entry) {
        Some(p) => PathBuf::from(p),
        None => runtime_dir().join(protocol::AGENT_SOCKET_NAME),
    }
}

/// Daemon log file.
pub fn daemon_log() -> PathBuf {
    log_dir().join(protocol::DAEMON_LOG_FILE)
}

/// Per-user CLI state (`~/.immurok`). `None` when `$HOME` is unset — which is
/// the normal case inside the daemon, and why nothing in the daemon may call
/// this.
pub fn user_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(protocol::IMMUROK_DIR))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_wins_over_systemd() {
        assert_eq!(
            resolve(Some("/scratch/run"), Some("/run/immurok"), "/fallback"),
            PathBuf::from("/scratch/run")
        );
    }

    #[test]
    fn systemd_wins_over_fallback() {
        assert_eq!(
            resolve(None, Some("/run/immurok"), "/fallback"),
            PathBuf::from("/run/immurok")
        );
    }

    #[test]
    fn fallback_when_nothing_set() {
        assert_eq!(resolve(None, None, "/fallback"), PathBuf::from("/fallback"));
    }

    /// An exported-but-empty variable must not win — that is how a shell
    /// leaves `IMMUROK_STATE_DIR=` and it would otherwise resolve to "".
    #[test]
    fn empty_values_are_ignored() {
        assert_eq!(resolve(Some(""), Some(""), "/fallback"), PathBuf::from("/fallback"));
        assert_eq!(resolve(Some("   "), None, "/fallback"), PathBuf::from("/fallback"));
    }

    #[test]
    fn takes_first_of_a_systemd_list() {
        assert_eq!(
            resolve(None, Some("/run/immurok:/run/immurok-extra"), "/fallback"),
            PathBuf::from("/run/immurok")
        );
    }
}
