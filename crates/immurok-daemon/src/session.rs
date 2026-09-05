//! Who is allowed to talk to the daemon.
//!
//! Before privilege separation this was trivial: the socket lived in the
//! user's own runtime directory, so "same uid as me" was the whole answer.
//! The daemon now runs as `immurok` and serves the entire machine from
//! /run/immurok, so that test would lock out precisely the person the device
//! belongs to. The replacement: management commands are served to whoever is
//! actually logged in at this machine right now, and to root.

use std::path::Path;

use immurok_common::paths;

/// `loginctl show-user <uid> -p State --value` output → is that user active?
///
/// Split out from the subprocess call so the parsing is testable. logind
/// reports `active` for the session in the foreground, `online` for a user
/// with sessions that are not current, `closing`/`lingering` on the way out.
/// Only `active` counts: "online" would include a second user who logged in
/// on another VT and walked away.
fn parse_user_state(out: &str) -> bool {
    out.trim() == "active"
}

/// Whether `uid` has an active login session on this machine.
///
/// `None` means we could not tell (no loginctl, logind not running) — the
/// caller decides what to do rather than getting a silent "no".
pub async fn uid_is_active(uid: u32) -> Option<bool> {
    let out = tokio::process::Command::new("loginctl")
        .arg("show-user")
        .arg(uid.to_string())
        .arg("-p")
        .arg("State")
        .arg("--value")
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        // Non-zero means logind has no such user (nobody logged in as them),
        // which is a definite "not active" rather than an unknown.
        return Some(false);
    }
    Some(parse_user_state(&String::from_utf8_lossy(&out.stdout)))
}

/// The uid the device is paired for, recorded at migration/pairing time.
///
/// Used as a fallback when logind cannot be reached, and to check that a PAM
/// `AUTH` request is for the owner rather than for some other local account
/// that happens to be able to run sudo.
pub fn owner_uid() -> Option<u32> {
    read_owner(&paths::state_dir().join("owner"))
}

fn read_owner(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// `loginctl show-user <uid> -p Display --value` → the user's graphical
/// session id, empty when they have none (tty-only login).
fn parse_session_id(out: &str) -> Option<String> {
    let id = out.trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

/// The owner's graphical session id, for `loginctl lock-session` /
/// `unlock-session`.
///
/// The daemon has no session of its own now, so the no-argument form of those
/// commands (which acts on the caller's session) can only fail.
pub async fn graphical_session_id(uid: u32) -> Option<String> {
    let out = tokio::process::Command::new("loginctl")
        .arg("show-user")
        .arg(uid.to_string())
        .arg("-p")
        .arg("Display")
        .arg("--value")
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_session_id(&String::from_utf8_lossy(&out.stdout))
}

/// Whether that session's screen is locked, per logind's `LockedHint`.
///
/// GNOME and KDE both set it when their lock screen engages. Compositors that
/// do not (swaylock, i3lock) report `no` while locked — same blind spot the
/// old screensaver-signal approach had for them.
pub async fn session_locked(session_id: &str) -> Option<bool> {
    let out = tokio::process::Command::new("loginctl")
        .arg("show-session")
        .arg(session_id)
        .arg("-p")
        .arg("LockedHint")
        .arg("--value")
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_locked_hint(&String::from_utf8_lossy(&out.stdout))
}

fn parse_locked_hint(out: &str) -> Option<bool> {
    match out.trim() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

/// Record `uid` as the device owner, unless one is already on record.
///
/// Never overwrites: re-pairing from a second account on a shared machine
/// must not silently hand the device over. Clearing it is an explicit
/// uninstall/factory-reset action.
pub fn record_owner(uid: u32) {
    let path = paths::state_dir().join("owner");
    if path.exists() {
        return;
    }
    if let Err(e) = std::fs::write(&path, format!("{}\n", uid)) {
        tracing::warn!("cannot record owner uid at {}: {}", path.display(), e);
    }
}

/// Resolve a username to a uid (for checking the `user` field of AUTH).
pub fn uid_of_user(name: &str) -> Option<u32> {
    let c_name = std::ffi::CString::new(name).ok()?;
    let pw = unsafe { libc::getpwnam(c_name.as_ptr()) };
    if pw.is_null() {
        None
    } else {
        Some(unsafe { (*pw).pw_uid })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_active_counts() {
        assert!(parse_user_state("active\n"));
        assert!(parse_user_state("active"));
        // A user logged in on another VT who is not the current session.
        assert!(!parse_user_state("online\n"));
        assert!(!parse_user_state("closing\n"));
        assert!(!parse_user_state("lingering\n"));
        assert!(!parse_user_state(""));
    }

    #[test]
    fn session_id_and_locked_hint_parsing() {
        assert_eq!(parse_session_id("3\n"), Some("3".to_string()));
        // A user with no graphical session (tty login) → logind prints nothing.
        assert_eq!(parse_session_id("\n"), None);
        assert_eq!(parse_session_id(""), None);

        assert_eq!(parse_locked_hint("yes\n"), Some(true));
        assert_eq!(parse_locked_hint("no\n"), Some(false));
        // Unknown/absent property must not be read as "unlocked".
        assert_eq!(parse_locked_hint(""), None);
        assert_eq!(parse_locked_hint("whatever"), None);
    }

    #[test]
    fn owner_file_parsing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owner");

        assert_eq!(read_owner(&path), None, "missing file → no owner");

        std::fs::write(&path, "1000\n").unwrap();
        assert_eq!(read_owner(&path), Some(1000));

        std::fs::write(&path, "  1000  ").unwrap();
        assert_eq!(read_owner(&path), Some(1000), "whitespace tolerated");

        std::fs::write(&path, "katsu").unwrap();
        assert_eq!(read_owner(&path), None, "a name is not a uid — refuse it");
    }
}
