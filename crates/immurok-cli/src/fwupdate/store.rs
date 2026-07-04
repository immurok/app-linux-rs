//! On-disk state under ~/.immurok/fwupdate/:
//!   last-check.json        24h-throttle record + cached manifest body
//!   cache/<sha256>.imfw    downloaded packages (sha256-named = dedup + tamper-evident)
//!   pending.json           two-hop resume state
//!   pending-target.imfw    final package copy for resume
//! All state writes are write-then-rename atomic.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::FwUpdateError;

pub const CHECK_INTERVAL_SECS: u64 = 24 * 3600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastCheck {
    pub checked_at: u64, // unix seconds
    pub manifest_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingHop {
    pub target_version: String,
    pub bridge_version: String,
}

pub struct FwStore {
    base: PathBuf,
}

impl FwStore {
    /// ~/.immurok/fwupdate — creates base + cache dirs.
    pub fn open_default() -> Result<Self, FwUpdateError> {
        let home =
            std::env::var("HOME").map_err(|_| FwUpdateError::Store("HOME not set".into()))?;
        Self::with_base(
            PathBuf::from(home)
                .join(immurok_common::protocol::IMMUROK_DIR)
                .join("fwupdate"),
        )
    }

    pub fn with_base(base: PathBuf) -> Result<Self, FwUpdateError> {
        fs::create_dir_all(base.join("cache"))
            .map_err(|e| FwUpdateError::Store(e.to_string()))?;
        Ok(Self { base })
    }

    fn last_check_path(&self) -> PathBuf {
        self.base.join("last-check.json")
    }
    fn pending_path(&self) -> PathBuf {
        self.base.join("pending.json")
    }
    pub fn pending_package_path(&self) -> PathBuf {
        self.base.join("pending-target.imfw")
    }

    // ── last-check ──────────────────────────────────────────────

    pub fn load_last_check(&self) -> Option<LastCheck> {
        let data = fs::read_to_string(self.last_check_path()).ok()?;
        serde_json::from_str(&data).ok()
    }

    pub fn save_last_check(&self, lc: &LastCheck) {
        if let Ok(json) = serde_json::to_vec(lc) {
            let _ = atomic_write(&self.last_check_path(), &json);
        }
    }

    /// Due when there is no record or the last check is ≥24h old.
    pub fn is_check_due(&self, now_unix: u64) -> bool {
        match self.load_last_check() {
            Some(lc) => now_unix.saturating_sub(lc.checked_at) >= CHECK_INTERVAL_SECS,
            None => true,
        }
    }

    // ── download cache ──────────────────────────────────────────

    pub fn cached_package(&self, sha256: &str) -> Option<PathBuf> {
        let p = self.base.join("cache").join(format!("{sha256}.imfw"));
        p.exists().then_some(p)
    }

    pub fn store_package(&self, sha256: &str, data: &[u8]) -> Result<PathBuf, FwUpdateError> {
        let p = self.base.join("cache").join(format!("{sha256}.imfw"));
        atomic_write(&p, data)?;
        Ok(p)
    }

    // ── pending hop (two-hop resume) ────────────────────────────

    /// Corrupt pending.json is treated as "no pending" and removed.
    pub fn load_pending(&self) -> Option<PendingHop> {
        let data = fs::read_to_string(self.pending_path()).ok()?;
        match serde_json::from_str(&data) {
            Ok(hop) => Some(hop),
            Err(_) => {
                // Corrupt state — drop it so we fall back to the full flow.
                self.clear_pending();
                None
            }
        }
    }

    pub fn save_pending(&self, hop: &PendingHop, package: &[u8]) -> Result<(), FwUpdateError> {
        atomic_write(&self.pending_package_path(), package)?;
        let json = serde_json::to_vec(hop).map_err(|e| FwUpdateError::Store(e.to_string()))?;
        atomic_write(&self.pending_path(), &json)
    }

    pub fn clear_pending(&self) {
        let _ = fs::remove_file(self.pending_path());
        let _ = fs::remove_file(self.pending_package_path());
    }
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<(), FwUpdateError> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data).map_err(|e| FwUpdateError::Store(e.to_string()))?;
    fs::rename(&tmp, path).map_err(|e| FwUpdateError::Store(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, FwStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = FwStore::with_base(dir.path().join("fwupdate")).unwrap();
        (dir, s)
    }

    #[test]
    fn check_due_when_empty_and_after_interval() {
        let (_d, s) = store();
        assert!(s.is_check_due(1_000_000));
        s.save_last_check(&LastCheck { checked_at: 1_000_000, manifest_json: "{}".into() });
        assert!(!s.is_check_due(1_000_000 + CHECK_INTERVAL_SECS - 1));
        assert!(s.is_check_due(1_000_000 + CHECK_INTERVAL_SECS));
        let lc = s.load_last_check().unwrap();
        assert_eq!(lc.checked_at, 1_000_000);
        assert_eq!(lc.manifest_json, "{}");
    }

    #[test]
    fn package_cache_roundtrip() {
        let (_d, s) = store();
        assert!(s.cached_package("abcd").is_none());
        let p = s.store_package("abcd", b"firmware-bytes").unwrap();
        assert_eq!(s.cached_package("abcd").unwrap(), p);
        assert_eq!(fs::read(&p).unwrap(), b"firmware-bytes");
    }

    #[test]
    fn pending_roundtrip_and_clear() {
        let (_d, s) = store();
        assert!(s.load_pending().is_none());
        let hop = PendingHop {
            target_version: "1.6.2".into(),
            bridge_version: "1.6.0".into(),
        };
        s.save_pending(&hop, b"final-pkg").unwrap();
        let loaded = s.load_pending().unwrap();
        assert_eq!(loaded.target_version, "1.6.2");
        assert_eq!(loaded.bridge_version, "1.6.0");
        assert_eq!(fs::read(s.pending_package_path()).unwrap(), b"final-pkg");
        s.clear_pending();
        assert!(s.load_pending().is_none());
        assert!(!s.pending_package_path().exists());
    }

    #[test]
    fn corrupt_pending_treated_as_none_and_removed() {
        let (_d, s) = store();
        fs::write(s.base.join("pending.json"), b"{not json").unwrap();
        assert!(s.load_pending().is_none());
        assert!(!s.base.join("pending.json").exists());
    }
}
