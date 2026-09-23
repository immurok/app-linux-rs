//! Per-user GUI settings: `~/.config/immurok/gui.json`.
//!
//! Phase 2 (hotkeys / typing backends) reads `output`, `portal_restore_token`
//! and `x11_hotkey` from here; phase 3 adds the local fingerprint names.
//! Nothing in this file is a secret, but the portal token lets a same-uid
//! process skip a permission prompt, so the file is 0600 anyway.
//!
//! Fingerprint names are purely local (the device only knows slot numbers),
//! exactly like macOS keeps them in UserDefaults.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use immurok_client::fingerprint::SWITCH_SLOT;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OutputChoice {
    #[default]
    Auto,
    Clipboard,
    Portal,
    Xdotool,
    Wtype,
}

impl OutputChoice {
    #[allow(dead_code)] // used by phase 2 (hotkey / typing-backend settings page)
    pub const ALL: [OutputChoice; 5] = [
        OutputChoice::Auto,
        OutputChoice::Clipboard,
        OutputChoice::Portal,
        OutputChoice::Xdotool,
        OutputChoice::Wtype,
    ];

    #[allow(dead_code)] // used by phase 2 (hotkey / typing-backend settings page)
    pub fn label(self) -> &'static str {
        match self {
            OutputChoice::Auto => "Automatic",
            OutputChoice::Clipboard => "Clipboard",
            OutputChoice::Portal => "Desktop portal",
            OutputChoice::Xdotool => "xdotool (X11)",
            OutputChoice::Wtype => "wtype (wlroots)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiSettings {
    pub output: OutputChoice,
    pub portal_restore_token: Option<String>,
    pub x11_hotkey: String,
    /// Slot → display name for authentication slots 0–4. Slot 5 (the
    /// host-switch finger) has a fixed name and is never stored.
    pub fingerprint_names: BTreeMap<u8, String>,
}

impl Default for GuiSettings {
    fn default() -> Self {
        Self {
            output: OutputChoice::Auto,
            portal_restore_token: None,
            x11_hotkey: "ctrl+backslash".into(),
            fingerprint_names: BTreeMap::new(),
        }
    }
}

impl GuiSettings {
    pub const SWITCH_NAME: &'static str = "Switch Host";

    pub fn fingerprint_name(&self, slot: u8) -> String {
        if slot == SWITCH_SLOT {
            return Self::SWITCH_NAME.to_string();
        }
        self.fingerprint_names
            .get(&slot)
            .cloned()
            .unwrap_or_else(|| format!("Finger {}", slot + 1))
    }

    /// Blank names clear the entry; the switch slot is ignored.
    pub fn set_fingerprint_name(&mut self, slot: u8, name: &str) {
        if slot == SWITCH_SLOT {
            return;
        }
        let name = name.trim();
        if name.is_empty() {
            self.fingerprint_names.remove(&slot);
        } else {
            self.fingerprint_names.insert(slot, name.to_string());
        }
    }

    pub fn clear_fingerprint_names(&mut self) {
        self.fingerprint_names.clear();
    }
}

pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
            home.join(".config")
        });
    base.join("immurok").join("gui.json")
}

pub fn load_from(path: &Path) -> GuiSettings {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Atomic write (temp file + rename), mode 0600.
pub fn save_to(path: &Path, s: &GuiSettings) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        f.write_all(&json).map_err(|e| e.to_string())?;
    }
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

pub fn load() -> GuiSettings {
    load_from(&path())
}

pub fn save(s: &GuiSettings) -> Result<(), String> {
    save_to(&path(), s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn defaults_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let s = load_from(&dir.path().join("gui.json"));
        assert_eq!(s, GuiSettings::default());
        assert_eq!(s.output, OutputChoice::Auto);
        assert_eq!(s.x11_hotkey, "ctrl+backslash");
        assert!(s.fingerprint_names.is_empty());
    }

    #[test]
    fn roundtrip_and_mode_0600() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("gui.json");
        let mut s = GuiSettings { output: OutputChoice::Xdotool, ..Default::default() };
        s.set_fingerprint_name(0, "Right thumb");
        save_to(&p, &s).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(load_from(&p), s);
    }

    #[test]
    fn corrupt_file_falls_back_to_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("gui.json");
        std::fs::write(&p, b"{ not json").unwrap();
        assert_eq!(load_from(&p), GuiSettings::default());
    }

    #[test]
    fn output_choice_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&OutputChoice::Xdotool).unwrap(), "\"xdotool\"");
    }

    #[test]
    fn fingerprint_names() {
        let mut s = GuiSettings::default();
        assert_eq!(s.fingerprint_name(0), "Finger 1");
        assert_eq!(s.fingerprint_name(4), "Finger 5");
        assert_eq!(s.fingerprint_name(SWITCH_SLOT), GuiSettings::SWITCH_NAME);

        s.set_fingerprint_name(0, "  Right thumb ");
        assert_eq!(s.fingerprint_name(0), "Right thumb");
        // The switch slot never takes a name.
        s.set_fingerprint_name(SWITCH_SLOT, "nope");
        assert_eq!(s.fingerprint_name(SWITCH_SLOT), "Switch Host");
        assert!(!s.fingerprint_names.contains_key(&SWITCH_SLOT));
        // Blank clears.
        s.set_fingerprint_name(0, "   ");
        assert_eq!(s.fingerprint_name(0), "Finger 1");

        s.set_fingerprint_name(2, "Left index");
        s.clear_fingerprint_names();
        assert!(s.fingerprint_names.is_empty());
    }

    #[test]
    fn unknown_fields_are_ignored_and_missing_ones_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("gui.json");
        std::fs::write(&p, br#"{"output":"clipboard","future_field":1}"#).unwrap();
        let s = load_from(&p);
        assert_eq!(s.output, OutputChoice::Clipboard);
        assert_eq!(s.x11_hotkey, "ctrl+backslash");
    }
}
