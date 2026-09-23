//! PAM configuration: which services carry the immurok auth line, and the
//! privileged helper that edits them.
//!
//! There is no socket command for this. State is read straight from
//! `/etc/pam.d/<service>` (`immurok_common::pam`), and edits go through
//! `pkexec <abs path>/immurok-pam-helper add|remove <svc...>` — polkit action
//! `com.immurok.pam-helper` (`auth_admin_keep`, GUI allowed, matched by the
//! helper's absolute path). The helper prints one `OK:…(svc)` or
//! `ERROR:…(svc)` line per service. pkexec itself exits 126 when the user
//! cancels the authentication and 127 when no polkit agent is available.
//!
//! Repair derivation mirrors `immurok-cli pam repair`: sudo / polkit-1 come
//! from the daemon's feature toggles; gdm-password is only touched when its
//! file already exists and lacks the line.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use immurok_common::pam::pam_line_present_in;

pub const PAM_DIR: &str = "/etc/pam.d";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PamService {
    pub label: &'static str,
    pub service: &'static str,
    pub path: &'static str,
}

pub const PAM_SERVICES: [PamService; 3] = [
    PamService { label: "sudo", service: "sudo", path: "/etc/pam.d/sudo" },
    PamService { label: "System authorization (polkit)", service: "polkit-1", path: "/etc/pam.d/polkit-1" },
    PamService { label: "Login screen (gdm)", service: "gdm-password", path: "/etc/pam.d/gdm-password" },
];

pub fn is_known_service(s: &str) -> bool {
    PAM_SERVICES.iter().any(|p| p.service == s)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PamServiceStatus {
    pub service: &'static str,
    pub label: &'static str,
    pub path: &'static str,
    pub installed: bool,
}

pub fn service_status_in(dir: &Path) -> Vec<PamServiceStatus> {
    PAM_SERVICES
        .iter()
        .map(|s| PamServiceStatus {
            service: s.service,
            label: s.label,
            path: s.path,
            installed: pam_line_present_in(dir, s.service),
        })
        .collect()
}

pub fn service_status() -> Vec<PamServiceStatus> {
    service_status_in(Path::new(PAM_DIR))
}

/// Services that should carry the line, derived from the daemon toggles.
pub fn desired_services(sudo_on: bool, polkit_on: bool) -> Vec<&'static str> {
    let mut v = Vec::new();
    if sudo_on {
        v.push("sudo");
    }
    if polkit_on {
        v.push("polkit-1");
    }
    v
}

/// Desired-but-missing services, plus gdm-password when its file exists and
/// lacks the line (best effort, never created).
pub fn services_to_repair_in(dir: &Path, sudo_on: bool, polkit_on: bool) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = desired_services(sudo_on, polkit_on)
        .into_iter()
        .filter(|s| !pam_line_present_in(dir, s))
        .collect();
    if dir.join("gdm-password").exists() && !pam_line_present_in(dir, "gdm-password") {
        v.push("gdm-password");
    }
    v
}

pub fn services_to_repair(sudo_on: bool, polkit_on: bool) -> Vec<&'static str> {
    services_to_repair_in(Path::new(PAM_DIR), sudo_on, polkit_on)
}

/// `immurok-pam-helper` next to the running binary, else on `PATH`.
pub fn find_helper() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let c = dir.join("immurok-pam-helper");
            if c.exists() {
                return Some(c);
            }
        }
    }
    let out = Command::new("which").arg("immurok-pam-helper").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if p.is_empty() {
        None
    } else {
        Some(PathBuf::from(p))
    }
}

/// The helper prints one line per service; any `ERROR:` line means failure.
pub fn helper_output_has_error(output: &str) -> bool {
    output.lines().any(|l| l.trim_start().starts_with("ERROR:"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperReport {
    /// Helper stdout, one trimmed line per service (`OK:…` / `ERROR:…`).
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PamError {
    HelperNotFound,
    /// pkexec exit 127: no polkit authentication agent is running.
    NoPolkitAgent,
    /// pkexec exit 126: the user dismissed the authentication dialog.
    AuthCancelled,
    /// The helper ran but reported a failure (non-zero exit, or `ERROR:` lines).
    HelperFailed { code: i32, lines: Vec<String> },
    Spawn(String),
}

impl fmt::Display for PamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PamError::HelperNotFound => write!(f, "immurok-pam-helper not found"),
            PamError::NoPolkitAgent => write!(f, "no polkit authentication agent is running"),
            PamError::AuthCancelled => write!(f, "authorization cancelled"),
            PamError::HelperFailed { code, lines } => {
                write!(f, "PAM helper failed (exit {code})")?;
                for l in lines {
                    write!(f, "\n{l}")?;
                }
                Ok(())
            }
            PamError::Spawn(e) => write!(f, "could not run pkexec: {e}"),
        }
    }
}

fn trimmed_lines(stdout: &str) -> Vec<String> {
    stdout.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
}

/// Pure classification of a pkexec + helper run.
pub fn classify_exit(code: Option<i32>, stdout: &str) -> Result<HelperReport, PamError> {
    let lines = trimmed_lines(stdout);
    match code {
        Some(0) if !helper_output_has_error(stdout) => Ok(HelperReport { lines }),
        Some(0) => Err(PamError::HelperFailed { code: 0, lines }),
        Some(126) => Err(PamError::AuthCancelled),
        Some(127) => Err(PamError::NoPolkitAgent),
        Some(c) => Err(PamError::HelperFailed { code: c, lines }),
        None => Err(PamError::HelperFailed { code: -1, lines }),
    }
}

/// Run `pkexec immurok-pam-helper <action> <services...>`. Blocks until the
/// user finishes (or cancels) the polkit prompt — never call on a UI thread.
/// Only `add` / `remove` and the services in [`PAM_SERVICES`] are accepted.
pub fn run_helper(action: &str, services: &[&str]) -> Result<HelperReport, PamError> {
    let valid = matches!(action, "add" | "remove")
        && !services.is_empty()
        && services.iter().all(|s| is_known_service(s));
    if !valid {
        return Err(PamError::Spawn("invalid request".into()));
    }
    let helper = find_helper().ok_or(PamError::HelperNotFound)?;
    let output = Command::new("pkexec")
        .arg(&helper)
        .arg(action)
        .args(services)
        .output()
        .map_err(|e| PamError::Spawn(e.to_string()))?;
    classify_exit(output.status.code(), &String::from_utf8_lossy(&output.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The returned `TempDir` owns the directory: keep the binding alive for as
    /// long as the path is used, it is removed on drop.
    fn tmp_pam_dir(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in files {
            std::fs::write(dir.path().join(name), body).unwrap();
        }
        dir
    }

    #[test]
    fn service_table() {
        assert_eq!(PAM_SERVICES.len(), 3);
        assert_eq!(PAM_SERVICES[0].service, "sudo");
        assert_eq!(PAM_SERVICES[1].service, "polkit-1");
        assert_eq!(PAM_SERVICES[2].service, "gdm-password");
        assert_eq!(PAM_SERVICES[1].path, "/etc/pam.d/polkit-1");
        assert!(is_known_service("sudo") && !is_known_service("login"));
    }

    #[test]
    fn status_reads_each_file() {
        let dir = tmp_pam_dir(&[("sudo", "auth sufficient pam_immurok.so\n"), ("polkit-1", "auth include common-auth\n")]);
        let st = service_status_in(dir.path());
        assert_eq!(st.len(), 3);
        assert!(st[0].installed && !st[1].installed && !st[2].installed);
        assert_eq!(st[2].label, "Login screen (gdm)");
    }

    #[test]
    fn repair_derives_from_toggles_and_missing_lines() {
        let dir = tmp_pam_dir(&[("sudo", "auth include common-auth\n"), ("polkit-1", "auth sufficient pam_immurok.so\n")]);
        assert_eq!(services_to_repair_in(dir.path(), true, true), vec!["sudo"]);
        assert_eq!(services_to_repair_in(dir.path(), false, true), Vec::<&str>::new());
        assert_eq!(services_to_repair_in(dir.path(), true, false), vec!["sudo"]);
        // gdm only when the file exists and lacks the line.
        let dir2 = tmp_pam_dir(&[("gdm-password", "auth include common-auth\n")]);
        assert_eq!(services_to_repair_in(dir2.path(), false, false), vec!["gdm-password"]);
    }

    #[test]
    fn error_lines() {
        assert!(helper_output_has_error("OK:ADDED(sudo)\nERROR:MODULE_NOT_INSTALLED(polkit-1)\n"));
        assert!(!helper_output_has_error("OK:ADDED(sudo)\n  OK:ALREADY_PRESENT(polkit-1)\n"));
    }

    #[test]
    fn exit_classification() {
        assert_eq!(
            classify_exit(Some(0), "OK:ADDED(sudo)\n"),
            Ok(HelperReport { lines: vec!["OK:ADDED(sudo)".into()] })
        );
        assert_eq!(
            classify_exit(Some(0), "ERROR:MODULE_NOT_INSTALLED(sudo)\n"),
            Err(PamError::HelperFailed { code: 0, lines: vec!["ERROR:MODULE_NOT_INSTALLED(sudo)".into()] })
        );
        assert_eq!(classify_exit(Some(126), ""), Err(PamError::AuthCancelled));
        assert_eq!(classify_exit(Some(127), ""), Err(PamError::NoPolkitAgent));
        assert_eq!(classify_exit(Some(1), "boom\n"), Err(PamError::HelperFailed { code: 1, lines: vec!["boom".into()] }));
        assert_eq!(classify_exit(None, ""), Err(PamError::HelperFailed { code: -1, lines: vec![] }));
    }

    #[test]
    fn run_helper_refuses_unknown_input_without_spawning() {
        assert_eq!(run_helper("nuke", &["sudo"]), Err(PamError::Spawn("invalid request".into())));
        assert_eq!(run_helper("add", &["login"]), Err(PamError::Spawn("invalid request".into())));
        assert_eq!(run_helper("add", &[]), Err(PamError::Spawn("invalid request".into())));
    }
}
