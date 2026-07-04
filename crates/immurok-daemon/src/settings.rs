//! User feature toggles persisted to ~/.immurok/settings.json

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_true")]
    pub unlock_sudo: bool,
    #[serde(default = "default_true")]
    pub unlock_polkit: bool,
    #[serde(default = "default_true")]
    pub unlock_screen: bool,
    /// Long-press device button → lock screen via loginctl. Opt-in.
    #[serde(default)]
    pub lock_screen: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            unlock_sudo: true,
            unlock_polkit: true,
            unlock_screen: true,
            lock_screen: false,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(std::io::Error::other)?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}
