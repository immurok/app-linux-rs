//! TUI application state and actions.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;

use crate::socket_client::DaemonClient;
use immurok_common::protocol;

/// Max lines retained in the in-TUI log viewer ring buffer.
pub const LOG_BUFFER_CAP: usize = 1000;
/// Lines moved per PgUp / PgDn keystroke.
pub const LOG_PAGE_STEP: usize = 20;

/// TUI interaction mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    EnrollSelect,
    DeleteSelect,
    /// Key management panel — SSH/OTP/API list + actions.
    Keys,
    /// SSH key generation: text-input for the key name.
    KeyGenInput,
    /// Confirm key deletion (y / n). Stores the pending category+index.
    KeyDeleteConfirm,
    /// Full-screen help overlay (read-only).
    Help,
    /// PAM service install/remove panel.
    Pam,
    /// In-TUI daemon log viewer (streams journalctl).
    Logs,
}

/// Which key category the Keys panel is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyTab {
    Ssh,
    Otp,
    Api,
}

impl KeyTab {
    pub fn label(self) -> &'static str {
        match self {
            KeyTab::Ssh => "SSH",
            KeyTab::Otp => "OTP",
            KeyTab::Api => "API",
        }
    }

    pub fn cat_byte(self) -> u8 {
        match self {
            KeyTab::Ssh => 0,
            KeyTab::Otp => 1,
            KeyTab::Api => 2,
        }
    }

    pub fn next(self) -> Self {
        match self {
            KeyTab::Ssh => KeyTab::Otp,
            KeyTab::Otp => KeyTab::Api,
            KeyTab::Api => KeyTab::Ssh,
        }
    }
}

/// SSH key entry shown in the TUI.
#[derive(Debug, Clone)]
pub struct SshKeyRow {
    pub index: u8,
    pub name: String,
    pub fingerprint: String,
    /// Base64-encoded SSH public key blob (for export/copy).
    pub blob_b64: String,
}

/// OTP/API name entry.
#[derive(Debug, Clone)]
pub struct NameKeyRow {
    pub index: u8,
    pub name: String,
}

/// One PAM service the panel can install/remove.
#[derive(Debug, Clone, Copy)]
pub struct PamService {
    pub display: &'static str,
    pub service: &'static str,
}

pub const PAM_SERVICES: &[PamService] = &[
    PamService { display: "sudo",         service: "sudo" },
    PamService { display: "polkit",       service: "polkit-1" },
    PamService { display: "screen (gdm)", service: "gdm-password" },
];

/// Sound preset cycled by [n]. Empty value = silent.
pub const SOUND_PRESETS: &[&str] = &["", "service-login", "complete", "bell", "message"];

/// TUI application state.
pub struct App {
    // Device state
    pub connected: bool,
    pub device_name: String,
    pub battery: u8,
    pub fw_version: String,
    pub paired: bool,
    pub fp_bitmap: u8,
    pub daemon_ok: bool,

    // Settings
    pub unlock_sudo: bool,
    pub unlock_polkit: bool,
    pub unlock_screen: bool,
    pub lock_screen: bool,
    pub unlock_sound: String,
    pub pam_sudo: bool,
    pub pam_polkit: bool,
    pub pam_screen: bool,

    // UI state
    pub mode: Mode,
    pub busy: bool,
    pub message: String,
    pub message_style: MessageStyle,

    // Enrollment progress (only meaningful while busy in Normal mode)
    pub enroll_active: bool,
    pub enroll_slot: u8,
    pub enroll_current: u8,
    pub enroll_total: u8,

    // Keys panel state
    pub key_tab: KeyTab,
    pub key_cursor: usize,
    pub ssh_keys: Vec<SshKeyRow>,
    pub otp_keys: Vec<NameKeyRow>,
    pub api_keys: Vec<NameKeyRow>,
    /// Pending text input (SSH key name during KeyGenInput).
    pub input_buf: String,
    /// Pending delete target (cat + index) while in KeyDeleteConfirm.
    pub pending_delete: Option<(KeyTab, u8)>,

    // PAM panel cursor
    pub pam_cursor: usize,

    // Logs panel state
    pub log_lines: VecDeque<String>,
    /// 0 = follow tail; >0 = number of lines above the tail the viewport
    /// is anchored at.
    pub log_scroll: usize,
    /// Owned journalctl child while the Logs panel is open. Kept here so
    /// that exiting the panel can `kill()` it deterministically.
    pub log_child: Option<Child>,

    // Background action results
    pub action_rx: mpsc::Receiver<ActionResult>,
    action_tx: mpsc::Sender<ActionResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageStyle {
    Dim,
    Green,
    Red,
    Yellow,
}

/// Result from a background action thread.
pub enum ActionResult {
    Refresh,
    Message(String, MessageStyle),
    /// Live enrollment progress update (current, total).
    EnrollProgress { current: u8, total: u8 },
    /// One line of streamed journalctl output for the Logs panel.
    LogLine(String),
    /// journalctl pipe closed (process exited or readers hit EOF).
    LogEnded,
    Done,
}

impl App {
    pub fn new() -> Self {
        let (action_tx, action_rx) = mpsc::channel();
        Self {
            connected: false,
            device_name: String::new(),
            battery: 0,
            fw_version: String::new(),
            paired: false,
            fp_bitmap: 0,
            daemon_ok: false,
            unlock_sudo: true,
            unlock_polkit: true,
            unlock_screen: true,
            lock_screen: false,
            unlock_sound: String::new(),
            pam_sudo: false,
            pam_polkit: false,
            pam_screen: false,
            mode: Mode::Normal,
            busy: false,
            message: "Ready".into(),
            message_style: MessageStyle::Dim,
            enroll_active: false,
            enroll_slot: 0,
            enroll_current: 0,
            enroll_total: 0,
            key_tab: KeyTab::Ssh,
            key_cursor: 0,
            ssh_keys: Vec::new(),
            otp_keys: Vec::new(),
            api_keys: Vec::new(),
            input_buf: String::new(),
            pending_delete: None,
            pam_cursor: 0,
            log_lines: VecDeque::with_capacity(LOG_BUFFER_CAP),
            log_scroll: 0,
            log_child: None,
            action_rx,
            action_tx,
        }
    }

    /// Drain action results from background threads. Called every tick.
    /// Returns true if a refresh was requested.
    pub fn drain_actions(&mut self) -> bool {
        let mut needs_refresh = false;
        while let Ok(result) = self.action_rx.try_recv() {
            match result {
                ActionResult::Message(msg, style) => {
                    self.message = msg;
                    self.message_style = style;
                }
                ActionResult::Refresh => needs_refresh = true,
                ActionResult::EnrollProgress { current, total } => {
                    self.enroll_current = current;
                    self.enroll_total = total;
                }
                ActionResult::LogLine(line) => {
                    if self.log_lines.len() >= LOG_BUFFER_CAP {
                        self.log_lines.pop_front();
                    }
                    self.log_lines.push_back(line);
                }
                ActionResult::LogEnded => {
                    if self.mode == Mode::Logs {
                        if self.log_lines.len() >= LOG_BUFFER_CAP {
                            self.log_lines.pop_front();
                        }
                        self.log_lines
                            .push_back("[log stream ended]".to_string());
                    }
                }
                ActionResult::Done => {
                    self.busy = false;
                    self.enroll_active = false;
                }
            }
        }
        needs_refresh
    }

    /// Refresh state from daemon.
    pub fn refresh(&mut self) {
        self.drain_actions();

        // Each send() needs its own connection because the daemon
        // handles one request per connection then closes it.

        // STATUS
        if let Ok(rsp) = daemon_send("STATUS") {
            self.daemon_ok = true;
            let parts: Vec<&str> = rsp.split(':').collect();
            if parts.first() == Some(&"STATUS") && parts.len() >= 5 {
                self.connected = parts[1] == "1";
                self.device_name = parts[2].to_string();
                self.battery = parts[3].parse().unwrap_or(0);
                self.fw_version = parts[4].to_string();
            }
        } else {
            self.daemon_ok = false;
            self.connected = false;
            return;
        }

        // PAIR:STATUS
        if let Ok(rsp) = daemon_send("PAIR:STATUS") {
            self.paired = rsp.contains("PAIRED") && !rsp.contains("UNPAIRED");
        }

        // FP:LIST
        if self.connected {
            if let Ok(rsp) = daemon_send("FP:LIST") {
                let parts: Vec<&str> = rsp.split(':').collect();
                if parts.first() == Some(&"OK") && parts.len() > 1 {
                    self.fp_bitmap = parts[1].parse().unwrap_or(0);
                }
            }
        } else {
            self.fp_bitmap = 0;
        }

        // GET:SETTINGS
        if let Ok(rsp) = daemon_send("GET:SETTINGS") {
            let parts: Vec<&str> = rsp.split(':').collect();
            if parts.first() == Some(&"OK") {
                for part in &parts[1..] {
                    if let Some((k, v)) = part.split_once('=') {
                        match k {
                            "sudo" => self.unlock_sudo = v == "1",
                            "polkit" => self.unlock_polkit = v == "1",
                            "screen" => self.unlock_screen = v == "1",
                            "lock" => self.lock_screen = v == "1",
                            "sound" => self.unlock_sound = v.to_string(),
                            _ => {}
                        }
                    }
                }
            }
        }

        // PAM status
        self.pam_sudo = check_pam("sudo");
        self.pam_polkit = check_pam("polkit-1");
        self.pam_screen = check_pam("gdm-password");

        // Key cache (cheap — just JSON files maintained by the daemon)
        self.refresh_keys();
    }

    /// Reload key cache files (ssh_keys.json, key_names.json) into memory.
    pub fn refresh_keys(&mut self) {
        let home = std::env::var("HOME").unwrap_or_default();
        let immurok_dir = std::path::PathBuf::from(&home).join(protocol::IMMUROK_DIR);

        // SSH
        let ssh_path = immurok_dir.join(protocol::SSH_KEYS_FILE);
        let mut ssh_rows: Vec<SshKeyRow> = Vec::new();
        if let Ok(contents) = std::fs::read_to_string(&ssh_path) {
            let entries: Vec<serde_json::Value> =
                serde_json::from_str(&contents).unwrap_or_default();
            for e in &entries {
                let idx = e["index"].as_u64().unwrap_or(0) as u8;
                let name = e["name"].as_str().unwrap_or("-").to_string();
                let fp = e["fingerprint"].as_str().unwrap_or("").to_string();
                let blob = e["public_key_blob"].as_str().unwrap_or("").to_string();
                ssh_rows.push(SshKeyRow {
                    index: idx,
                    name,
                    fingerprint: fp,
                    blob_b64: blob,
                });
            }
        }
        self.ssh_keys = ssh_rows;

        // OTP + API share the same names file
        let names_path = immurok_dir.join(protocol::KEY_NAMES_FILE);
        let mut otp_rows: Vec<NameKeyRow> = Vec::new();
        let mut api_rows: Vec<NameKeyRow> = Vec::new();
        if let Ok(contents) = std::fs::read_to_string(&names_path) {
            let entries: Vec<serde_json::Value> =
                serde_json::from_str(&contents).unwrap_or_default();
            for e in &entries {
                let idx = e["index"].as_u64().unwrap_or(0) as u8;
                let name = e["name"].as_str().unwrap_or("-").to_string();
                let cat = e["category"].as_str().unwrap_or("");
                let row = NameKeyRow { index: idx, name };
                match cat {
                    "otp" => otp_rows.push(row),
                    "api" => api_rows.push(row),
                    _ => {}
                }
            }
        }
        self.otp_keys = otp_rows;
        self.api_keys = api_rows;

        // Clamp cursor if list shrank
        let len = self.current_key_len();
        if len == 0 {
            self.key_cursor = 0;
        } else if self.key_cursor >= len {
            self.key_cursor = len - 1;
        }
    }

    /// Length of the currently-selected key list.
    pub fn current_key_len(&self) -> usize {
        match self.key_tab {
            KeyTab::Ssh => self.ssh_keys.len(),
            KeyTab::Otp => self.otp_keys.len(),
            KeyTab::Api => self.api_keys.len(),
        }
    }

    /// Index of the key under cursor (only valid if list non-empty).
    pub fn current_key_index(&self) -> Option<u8> {
        match self.key_tab {
            KeyTab::Ssh => self.ssh_keys.get(self.key_cursor).map(|r| r.index),
            KeyTab::Otp => self.otp_keys.get(self.key_cursor).map(|r| r.index),
            KeyTab::Api => self.api_keys.get(self.key_cursor).map(|r| r.index),
        }
    }

    /// Name of the key under cursor.
    pub fn current_key_name(&self) -> Option<String> {
        match self.key_tab {
            KeyTab::Ssh => self.ssh_keys.get(self.key_cursor).map(|r| r.name.clone()),
            KeyTab::Otp => self.otp_keys.get(self.key_cursor).map(|r| r.name.clone()),
            KeyTab::Api => self.api_keys.get(self.key_cursor).map(|r| r.name.clone()),
        }
    }

    // ── Actions ─────────────────────────────────────────────

    pub fn auto_enroll(&mut self) {
        if !self.connected {
            self.set_msg("Device not connected", MessageStyle::Red);
            return;
        }
        // Find first empty slot
        let slot = (0..protocol::MAX_FINGERPRINT_SLOTS)
            .find(|i| self.fp_bitmap & (1 << i) == 0);
        match slot {
            Some(s) => self.action_enroll(s),
            None => self.set_msg("All fingerprint slots are full", MessageStyle::Red),
        }
    }

    pub fn enter_enroll_select(&mut self) {
        if !self.connected {
            self.set_msg("Device not connected", MessageStyle::Red);
            return;
        }
        if self.busy {
            return;
        }
        self.mode = Mode::EnrollSelect;
        self.set_msg(
            &format!(
                "Press 0-{} to choose enroll slot (E = first empty, Esc = cancel)",
                protocol::MAX_FINGERPRINT_SLOTS - 1
            ),
            MessageStyle::Yellow,
        );
    }

    pub fn enter_delete_select(&mut self) {
        if !self.connected {
            self.set_msg("Device not connected", MessageStyle::Red);
            return;
        }
        self.mode = Mode::DeleteSelect;
        self.set_msg(
            &format!(
                "Press 0-{} to choose slot to delete (Esc to cancel)",
                protocol::MAX_FINGERPRINT_SLOTS - 1
            ),
            MessageStyle::Yellow,
        );
    }

    pub fn cancel_select(&mut self) {
        self.mode = Mode::Normal;
        self.set_msg("Ready", MessageStyle::Dim);
    }

    pub fn action_pair(&mut self) {
        if self.busy {
            return;
        }
        if self.paired {
            self.set_msg("Already paired — unpair first.", MessageStyle::Yellow);
            return;
        }
        if !self.connected {
            self.set_msg("Device not connected", MessageStyle::Red);
            return;
        }

        self.busy = true;
        self.set_msg(
            "Pairing… press the device button within 30s",
            MessageStyle::Yellow,
        );

        let tx = self.action_tx.clone();
        thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let mut client = DaemonClient::connect()?;
                client.send("PAIR:START")
            })();

            match result {
                Ok(rsp) if rsp.contains("PAIRED") => {
                    let _ = tx.send(ActionResult::Message(
                        "Pairing successful.".into(),
                        MessageStyle::Green,
                    ));
                }
                Ok(rsp) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Pairing failed: {}", rsp),
                        MessageStyle::Red,
                    ));
                }
                Err(e) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Pairing error: {}", e),
                        MessageStyle::Red,
                    ));
                }
            }
            let _ = tx.send(ActionResult::Refresh);
            let _ = tx.send(ActionResult::Done);
        });
    }

    pub fn action_unpair(&mut self) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.set_msg("Resetting pairing…", MessageStyle::Yellow);

        let tx = self.action_tx.clone();
        thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let mut client = DaemonClient::connect()?;
                client.send("PAIR:RESET")
            })();

            match result {
                Ok(rsp) if rsp.contains("RESET") => {
                    let _ = tx.send(ActionResult::Message(
                        "Pairing cleared.".into(),
                        MessageStyle::Green,
                    ));
                }
                Ok(rsp) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Unpair failed: {}", rsp),
                        MessageStyle::Red,
                    ));
                }
                Err(e) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Unpair error: {}", e),
                        MessageStyle::Red,
                    ));
                }
            }
            let _ = tx.send(ActionResult::Refresh);
            let _ = tx.send(ActionResult::Done);
        });
    }

    pub fn action_enroll(&mut self, slot: u8) {
        if self.busy {
            return;
        }
        self.mode = Mode::Normal;
        self.busy = true;
        self.enroll_active = true;
        self.enroll_slot = slot;
        self.enroll_current = 0;
        self.enroll_total = 12;
        self.set_msg(
            &format!("Enrolling slot {} — place finger on sensor…", slot),
            MessageStyle::Yellow,
        );

        let tx = self.action_tx.clone();
        thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let rsp = daemon_send(&format!("FP:ENROLL:{}", slot))?;
                if !rsp.contains("ENROLL_STARTED") {
                    return Err(format!("Start failed: {}", rsp));
                }

                let _ = tx.send(ActionResult::Message(
                    "Enrollment started — place finger…".into(),
                    MessageStyle::Yellow,
                ));

                // Poll FP:STATUS for enrollment progress (12 captures × 30s = 360s max)
                let mut last_event = 255u8;
                let mut last_current = 255u8;
                let mut bitmap_polls = 0u32;
                for _ in 0..2400 {
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    if let Ok(rsp) = daemon_send("FP:STATUS") {
                        let parts: Vec<&str> = rsp.split(':').collect();
                        if parts.first() != Some(&"OK") || parts.len() < 2 {
                            continue;
                        }
                        if parts[1] == "IDLE" {
                            // COMPLETE notification may have been missed;
                            // fall back to bitmap check every ~3s
                            bitmap_polls += 1;
                            if bitmap_polls % 20 == 0 {
                                if let Ok(list_rsp) = daemon_send("FP:LIST") {
                                    let lp: Vec<&str> = list_rsp.split(':').collect();
                                    if lp.first() == Some(&"OK") && lp.len() > 1 {
                                        if let Ok(bm) = lp[1].parse::<u8>() {
                                            if bm & (1 << slot) != 0 {
                                                let _ = tx.send(ActionResult::Message(
                                                    format!("Slot {} enrolled.", slot),
                                                    MessageStyle::Green,
                                                ));
                                                let _ = tx.send(ActionResult::Refresh);
                                                let _ = tx.send(ActionResult::Done);
                                                return Ok(());
                                            }
                                        }
                                    }
                                }
                            }
                            continue;
                        }
                        let event: u8 = match parts[1].parse() {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        let current: u8 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                        let total: u8 = parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(12);
                        let _ = tx.send(ActionResult::EnrollProgress { current, total });

                        if event == last_event && current == last_current {
                            continue;
                        }
                        last_event = event;
                        last_current = current;

                        match event {
                            0x00 => {
                                let _ = tx.send(ActionResult::Message(
                                    "Place finger on sensor…".into(),
                                    MessageStyle::Yellow,
                                ));
                            }
                            0x01 => {
                                let msg = if current < total {
                                    format!("Captured [{}/{}] — lift and press again", current, total)
                                } else {
                                    format!("Captured [{}/{}]", current, total)
                                };
                                let _ = tx.send(ActionResult::Message(msg, MessageStyle::Yellow));
                            }
                            0x02 => {
                                let _ = tx.send(ActionResult::Message(
                                    "Processing…".into(),
                                    MessageStyle::Yellow,
                                ));
                            }
                            0x03 => {} // already told user to lift in CAPTURED message
                            0x04 => {
                                let _ = tx.send(ActionResult::Message(
                                    format!("Slot {} enrolled.", slot),
                                    MessageStyle::Green,
                                ));
                                // Wait briefly for daemon to refresh bitmap via BLE,
                                // then trigger UI refresh
                                std::thread::sleep(std::time::Duration::from_millis(500));
                                let _ = tx.send(ActionResult::Refresh);
                                let _ = tx.send(ActionResult::Done);
                                return Ok(());
                            }
                            0xFF => {
                                return Err("Enrollment failed".into());
                            }
                            _ => {}
                        }
                    }
                }

                Err("Enrollment timed out".into())
            })();

            if let Err(e) = result {
                let _ = tx.send(ActionResult::Message(
                    format!("Enroll error: {}", e),
                    MessageStyle::Red,
                ));
                let _ = tx.send(ActionResult::Refresh);
                let _ = tx.send(ActionResult::Done);
            }
            // On success, Refresh+Done already sent inside the closure
        });
    }

    /// Cancel an in-flight enrollment.
    pub fn action_enroll_cancel(&mut self) {
        if !self.enroll_active {
            return;
        }
        // Best-effort — daemon will reply quickly; the polling thread
        // notices IDLE and emits Done on its own.
        thread::spawn(|| {
            let _ = DaemonClient::connect().and_then(|mut c| c.send("FP:ENROLL_CANCEL"));
        });
        self.set_msg("Enrollment cancelled.", MessageStyle::Yellow);
    }

    pub fn action_delete(&mut self, slot: u8) {
        if self.busy {
            return;
        }
        self.mode = Mode::Normal;
        self.busy = true;
        self.set_msg(
            &format!("Deleting slot {} — touch sensor to confirm", slot),
            MessageStyle::Yellow,
        );

        let tx = self.action_tx.clone();
        thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let mut client = DaemonClient::connect()?;
                client.send(&format!("FP:DELETE:{}", slot))
            })();

            match result {
                Ok(rsp) if rsp.contains("DELETED") => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Slot {} deleted.", slot),
                        MessageStyle::Green,
                    ));
                    // Wait for daemon to refresh bitmap via BLE
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                Ok(rsp) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Delete failed: {}", rsp),
                        MessageStyle::Red,
                    ));
                }
                Err(e) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Delete error: {}", e),
                        MessageStyle::Red,
                    ));
                }
            }
            let _ = tx.send(ActionResult::Refresh);
            let _ = tx.send(ActionResult::Done);
        });
    }

    pub fn action_verify(&mut self) {
        if self.busy {
            return;
        }
        if !self.connected {
            self.set_msg("Device not connected", MessageStyle::Red);
            return;
        }
        if !self.paired {
            self.set_msg("Device not paired", MessageStyle::Red);
            return;
        }

        self.busy = true;
        self.set_msg("Touch sensor to verify…", MessageStyle::Yellow);

        let tx = self.action_tx.clone();
        thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let mut client = DaemonClient::connect()?;
                client.send("FP:VERIFY")
            })();

            match result {
                Ok(rsp) if rsp.contains("MATCH") && !rsp.contains("NO_MATCH") => {
                    let _ = tx.send(ActionResult::Message(
                        "Fingerprint verified.".into(),
                        MessageStyle::Green,
                    ));
                }
                Ok(rsp) if rsp.contains("NO_MATCH") => {
                    let _ = tx.send(ActionResult::Message(
                        "Fingerprint did not match.".into(),
                        MessageStyle::Red,
                    ));
                }
                Ok(rsp) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Verify: {}", rsp),
                        MessageStyle::Red,
                    ));
                }
                Err(e) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Verify error: {}", e),
                        MessageStyle::Red,
                    ));
                }
            }
            let _ = tx.send(ActionResult::Done);
        });
    }

    pub fn action_toggle_sudo(&mut self) {
        self.toggle_setting("sudo", "UNLOCK_SUDO", self.unlock_sudo);
    }

    pub fn action_toggle_polkit(&mut self) {
        self.toggle_setting("polkit", "UNLOCK_POLKIT", self.unlock_polkit);
    }

    pub fn action_toggle_screen(&mut self) {
        self.toggle_setting("screen", "UNLOCK_SCREEN", self.unlock_screen);
    }

    pub fn action_toggle_lock(&mut self) {
        self.toggle_setting("lock", "LOCK_SCREEN", self.lock_screen);
    }

    /// Cycle [n] through SOUND_PRESETS.
    pub fn action_cycle_sound(&mut self) {
        if self.busy {
            return;
        }
        let cur_pos = SOUND_PRESETS
            .iter()
            .position(|s| *s == self.unlock_sound.as_str())
            .unwrap_or(0);
        let next = SOUND_PRESETS[(cur_pos + 1) % SOUND_PRESETS.len()].to_string();
        self.busy = true;

        let display = if next.is_empty() {
            "silent".to_string()
        } else {
            next.clone()
        };
        self.set_msg(
            &format!("Setting unlock sound → {}…", display),
            MessageStyle::Yellow,
        );

        let cmd = format!("SET:UNLOCK_SOUND:{}", next);
        let tx = self.action_tx.clone();
        thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let mut client = DaemonClient::connect()?;
                client.send(&cmd)
            })();
            match result {
                Ok(rsp) if rsp.starts_with("OK") => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Unlock sound: {}", display),
                        MessageStyle::Green,
                    ));
                }
                Ok(rsp) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Sound update failed: {}", rsp),
                        MessageStyle::Red,
                    ));
                }
                Err(e) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Sound update error: {}", e),
                        MessageStyle::Red,
                    ));
                }
            }
            let _ = tx.send(ActionResult::Refresh);
            let _ = tx.send(ActionResult::Done);
        });
    }

    fn toggle_setting(&mut self, name: &str, cmd_key: &str, current: bool) {
        if self.busy {
            return;
        }
        self.busy = true;

        let new_val = if current { "0" } else { "1" };
        let cmd = format!("SET:{}:{}", cmd_key, new_val);
        let name = name.to_string();
        let enabling = !current;

        self.set_msg(
            &format!(
                "Setting {} {}…",
                name,
                if enabling { "ON" } else { "OFF" }
            ),
            MessageStyle::Yellow,
        );

        let tx = self.action_tx.clone();
        thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let mut client = DaemonClient::connect()?;
                client.send(&cmd)
            })();

            let state = if enabling { "ON" } else { "OFF" };
            match result {
                Ok(rsp) if rsp.starts_with("OK") => {
                    let _ = tx.send(ActionResult::Message(
                        format!("{} → {}", name, state),
                        MessageStyle::Green,
                    ));
                }
                Ok(rsp) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Setting {} failed: {}", name, rsp),
                        MessageStyle::Red,
                    ));
                }
                Err(e) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Setting {} error: {}", name, e),
                        MessageStyle::Red,
                    ));
                }
            }
            let _ = tx.send(ActionResult::Refresh);
            let _ = tx.send(ActionResult::Done);
        });
    }

    pub fn action_info(&mut self) {
        let result = (|| -> Result<String, String> {
            let mut client = DaemonClient::connect()?;
            client.send("GET:INFO")
        })();

        match result {
            Ok(rsp) => {
                let parts: Vec<&str> = rsp.split(':').collect();
                if parts.first() == Some(&"OK") {
                    let mut lines = Vec::new();
                    for part in &parts[1..] {
                        if let Some((k, v)) = part.split_once('=') {
                            let label = match k {
                                "model" => "Model",
                                "fw" => "Firmware",
                                "battery" => "Battery",
                                "connected" => "Connected",
                                _ => k,
                            };
                            let display = if k == "battery" && v != "-1" {
                                format!("{}%", v)
                            } else if k == "connected" {
                                (if v == "1" { "Yes" } else { "No" }).to_string()
                            } else {
                                v.to_string()
                            };
                            lines.push(format!("{}: {}", label, display));
                        }
                    }
                    self.set_msg(&lines.join(" │ "), MessageStyle::Dim);
                } else {
                    self.set_msg(&format!("Info: {}", rsp), MessageStyle::Red);
                }
            }
            Err(e) => self.set_msg(&format!("Info error: {}", e), MessageStyle::Red),
        }
    }

    pub fn set_msg(&mut self, msg: &str, style: MessageStyle) {
        self.message = msg.to_string();
        self.message_style = style;
    }

    pub fn set_msg_dim(&mut self, msg: &str) {
        self.set_msg(msg, MessageStyle::Dim);
    }

    // ── Help overlay ──────────────────────────────────────────

    pub fn toggle_help(&mut self) {
        self.mode = match self.mode {
            Mode::Help => Mode::Normal,
            _ => Mode::Help,
        };
    }

    // ── PAM panel ─────────────────────────────────────────────

    pub fn enter_pam_mode(&mut self) {
        self.mode = Mode::Pam;
        if self.pam_cursor >= PAM_SERVICES.len() {
            self.pam_cursor = 0;
        }
        self.set_msg(
            "PAM services — [↑↓/jk] move  [i]nstall  [r]emove  [Esc] back",
            MessageStyle::Dim,
        );
    }

    pub fn pam_cursor_up(&mut self) {
        if self.pam_cursor > 0 {
            self.pam_cursor -= 1;
        }
    }

    pub fn pam_cursor_down(&mut self) {
        if self.pam_cursor + 1 < PAM_SERVICES.len() {
            self.pam_cursor += 1;
        }
    }

    /// Whether a given PAM service is currently installed.
    pub fn pam_is_installed(&self, idx: usize) -> bool {
        match PAM_SERVICES.get(idx).map(|s| s.service) {
            Some("sudo") => self.pam_sudo,
            Some("polkit-1") => self.pam_polkit,
            Some("gdm-password") => self.pam_screen,
            _ => false,
        }
    }

    /// Spawn the immurok-pam-helper via pkexec to install or remove PAM
    /// configuration. We *return* a TerminalHandoff request to the caller so
    /// the event loop can leave alternate screen first (pkexec prints a
    /// prompt to TTY).
    pub fn request_pam_action(&self, install: bool) -> Option<PamRequest> {
        let svc = PAM_SERVICES.get(self.pam_cursor)?;
        Some(PamRequest {
            action: if install { "add" } else { "remove" },
            service: svc.service,
        })
    }

    // ── Logs panel ────────────────────────────────────────────

    /// Spawn `journalctl -fu immurok-daemon` and stream its stdout into the
    /// in-TUI log ring buffer via the existing action channel.
    pub fn enter_logs_mode(&mut self) {
        // If already open (e.g. user double-pressed `l`), do nothing.
        if self.log_child.is_some() {
            self.mode = Mode::Logs;
            return;
        }

        let mut child = match Command::new("journalctl")
            .args([
                "--user",
                "-u",
                "immurok-daemon",
                "-n",
                "200",
                "-f",
                "--no-pager",
                "-o",
                "short-iso",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                self.set_msg(
                    &format!("Failed to start journalctl: {}", e),
                    MessageStyle::Red,
                );
                return;
            }
        };

        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                self.set_msg("journalctl produced no stdout", MessageStyle::Red);
                return;
            }
        };

        self.log_lines.clear();
        self.log_scroll = 0;
        self.mode = Mode::Logs;
        self.set_msg_dim(
            "Streaming daemon logs — [↑↓/jk] line  [PgUp/PgDn] page  [End] tail  [Esc] back",
        );

        let tx = self.action_tx.clone();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if tx.send(ActionResult::LogLine(l)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(ActionResult::LogEnded);
        });

        self.log_child = Some(child);
    }

    /// Kill the journalctl child (if any) and return to the Normal panel.
    pub fn exit_logs_mode(&mut self) {
        if let Some(mut child) = self.log_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // Drop the buffer too — re-entering Logs starts fresh and avoids
        // accumulating memory across sessions.
        self.log_lines.clear();
        self.log_scroll = 0;
        self.mode = Mode::Normal;
        self.set_msg_dim("Ready");
    }

    pub fn log_scroll_up(&mut self) {
        // Cap at buffer length so we don't scroll past the oldest line.
        if self.log_scroll < self.log_lines.len() {
            self.log_scroll += 1;
        }
    }

    pub fn log_scroll_down(&mut self) {
        if self.log_scroll > 0 {
            self.log_scroll -= 1;
        }
    }

    pub fn log_page_up(&mut self) {
        self.log_scroll = (self.log_scroll + LOG_PAGE_STEP).min(self.log_lines.len());
    }

    pub fn log_page_down(&mut self) {
        self.log_scroll = self.log_scroll.saturating_sub(LOG_PAGE_STEP);
    }

    pub fn log_jump_top(&mut self) {
        self.log_scroll = self.log_lines.len();
    }

    pub fn log_jump_bottom(&mut self) {
        self.log_scroll = 0;
    }

    /// Best-effort cleanup hook invoked on TUI shutdown.
    pub fn shutdown(&mut self) {
        if let Some(mut child) = self.log_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    pub fn after_pam_action(&mut self, req: &PamRequest, ok: bool) {
        let verb = if req.action == "add" { "installed" } else { "removed" };
        if ok {
            self.set_msg(
                &format!("PAM {} for {} succeeded.", verb, req.service),
                MessageStyle::Green,
            );
        } else {
            self.set_msg(
                &format!("PAM {} for {} failed (see terminal output above).", verb, req.service),
                MessageStyle::Red,
            );
        }
    }

    // ── Keys panel actions ───────────────────────────────────

    pub fn enter_keys_mode(&mut self) {
        self.mode = Mode::Keys;
        self.key_cursor = 0;
        self.refresh_keys();
        self.set_msg(
            "Keys — [Tab] tab  [↑↓/jk] move  [g]en  [d]el  [o]tp  [c]opy  [r]efresh  [Esc] back",
            MessageStyle::Dim,
        );
    }

    pub fn keys_next_tab(&mut self) {
        self.key_tab = self.key_tab.next();
        self.key_cursor = 0;
    }

    pub fn keys_set_tab(&mut self, tab: KeyTab) {
        self.key_tab = tab;
        self.key_cursor = 0;
    }

    pub fn keys_cursor_up(&mut self) {
        if self.key_cursor > 0 {
            self.key_cursor -= 1;
        }
    }

    pub fn keys_cursor_down(&mut self) {
        let len = self.current_key_len();
        if len > 0 && self.key_cursor + 1 < len {
            self.key_cursor += 1;
        }
    }

    /// `c` in SSH tab — render the selected key's authorized_keys line into the
    /// message area so the user can read it.
    pub fn action_key_show_pubkey(&mut self) {
        if self.key_tab != KeyTab::Ssh {
            self.set_msg("Public key view is SSH-only.", MessageStyle::Yellow);
            return;
        }
        let row = match self.ssh_keys.get(self.key_cursor) {
            Some(r) => r.clone(),
            None => {
                self.set_msg("No SSH key selected.", MessageStyle::Yellow);
                return;
            }
        };
        if row.blob_b64.is_empty() {
            self.set_msg(
                "No public key data cached for this entry.",
                MessageStyle::Red,
            );
            return;
        }
        let line = format!("ecdsa-sha2-nistp256 {} {}", row.blob_b64, row.name);
        self.set_msg(&line, MessageStyle::Green);
    }

    /// Enter SSH key generate flow — collects the key name then issues
    /// `KEY:GENERATE` once the user hits Enter.
    pub fn enter_key_gen_input(&mut self) {
        if self.key_tab != KeyTab::Ssh {
            self.set_msg(
                "Generate is SSH-only (use `immurok-cli key add` for OTP/API).",
                MessageStyle::Yellow,
            );
            return;
        }
        if !self.connected {
            self.set_msg("Device not connected", MessageStyle::Red);
            return;
        }
        if self.ssh_keys.len() >= protocol::KEY_MAX_SSH as usize {
            self.set_msg(
                &format!(
                    "SSH keystore full ({}/{}) — delete an entry first.",
                    self.ssh_keys.len(),
                    protocol::KEY_MAX_SSH
                ),
                MessageStyle::Red,
            );
            return;
        }
        self.input_buf.clear();
        self.mode = Mode::KeyGenInput;
        self.set_msg(
            "Enter SSH key name (max 15 chars). [Enter] confirm  [Esc] cancel",
            MessageStyle::Yellow,
        );
    }

    pub fn input_push_char(&mut self, c: char) {
        if self.input_buf.len() < 15 && !c.is_control() {
            self.input_buf.push(c);
        }
    }

    pub fn input_pop_char(&mut self) {
        self.input_buf.pop();
    }

    pub fn input_cancel(&mut self) {
        self.input_buf.clear();
        self.mode = Mode::Keys;
        self.set_msg("Cancelled.", MessageStyle::Dim);
    }

    pub fn input_submit_key_gen(&mut self) {
        let name = self.input_buf.trim().to_string();
        if name.is_empty() {
            self.set_msg("Name cannot be empty.", MessageStyle::Red);
            return;
        }
        self.input_buf.clear();
        self.mode = Mode::Keys;
        self.action_key_generate(name);
    }

    fn action_key_generate(&mut self, name: String) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.set_msg(
            &format!("Generating SSH keypair '{}' — touch sensor to authorize…", name),
            MessageStyle::Yellow,
        );

        // Build name payload (16 bytes, null-padded, copy ≤15 bytes).
        let mut name_buf = vec![0u8; 16];
        let nb = name.as_bytes();
        let copy_len = nb.len().min(15);
        name_buf[..copy_len].copy_from_slice(&nb[..copy_len]);
        let hex_name = hex::encode(&name_buf);
        let cmd = format!("KEY:GENERATE:{}", hex_name);

        let tx = self.action_tx.clone();
        thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let mut client = DaemonClient::connect()?;
                client.send(&cmd)
            })();
            match result {
                Ok(rsp) if rsp.starts_with("OK") => {
                    let _ = tx.send(ActionResult::Message(
                        format!("SSH keypair '{}' generated.", name),
                        MessageStyle::Green,
                    ));
                }
                Ok(rsp) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Generate failed: {}", rsp),
                        MessageStyle::Red,
                    ));
                }
                Err(e) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Generate error: {}", e),
                        MessageStyle::Red,
                    ));
                }
            }
            let _ = tx.send(ActionResult::Refresh);
            let _ = tx.send(ActionResult::Done);
        });
    }

    /// Begin delete-confirm flow on the selected key.
    pub fn enter_key_delete_confirm(&mut self) {
        if self.busy {
            return;
        }
        if !self.connected {
            self.set_msg("Device not connected", MessageStyle::Red);
            return;
        }
        let idx = match self.current_key_index() {
            Some(i) => i,
            None => {
                self.set_msg("No key selected.", MessageStyle::Yellow);
                return;
            }
        };
        let name = self.current_key_name().unwrap_or_else(|| "-".into());
        self.pending_delete = Some((self.key_tab, idx));
        self.mode = Mode::KeyDeleteConfirm;
        self.set_msg(
            &format!(
                "Delete {} key [{}] '{}'? [y] confirm  [n/Esc] cancel",
                self.key_tab.label(),
                idx,
                name
            ),
            MessageStyle::Yellow,
        );
    }

    pub fn cancel_key_delete(&mut self) {
        self.pending_delete = None;
        self.mode = Mode::Keys;
        self.set_msg("Cancelled.", MessageStyle::Dim);
    }

    pub fn confirm_key_delete(&mut self) {
        let (tab, idx) = match self.pending_delete.take() {
            Some(v) => v,
            None => {
                self.mode = Mode::Keys;
                return;
            }
        };
        self.mode = Mode::Keys;
        if self.busy {
            return;
        }
        self.busy = true;
        self.set_msg(
            &format!(
                "Deleting {} key [{}] — touch sensor to authorize…",
                tab.label(),
                idx
            ),
            MessageStyle::Yellow,
        );

        let cmd = format!("KEY:DELETE:{}:{}", tab.cat_byte(), idx);
        let label = tab.label();
        let tx = self.action_tx.clone();
        thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let mut client = DaemonClient::connect()?;
                client.send(&cmd)
            })();
            match result {
                Ok(rsp) if rsp.starts_with("OK") => {
                    let _ = tx.send(ActionResult::Message(
                        format!("{} key [{}] deleted.", label, idx),
                        MessageStyle::Green,
                    ));
                }
                Ok(rsp) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Delete failed: {}", rsp),
                        MessageStyle::Red,
                    ));
                }
                Err(e) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Delete error: {}", e),
                        MessageStyle::Red,
                    ));
                }
            }
            let _ = tx.send(ActionResult::Refresh);
            let _ = tx.send(ActionResult::Done);
        });
    }

    /// Request a fresh TOTP code for the selected OTP key. Routes through
    /// `GET:otp:<name>` which is FP-gated by the daemon.
    pub fn action_key_otp(&mut self) {
        if self.busy {
            return;
        }
        if self.key_tab != KeyTab::Otp {
            self.set_msg(
                "OTP code is only available on the OTP tab.",
                MessageStyle::Yellow,
            );
            return;
        }
        if !self.connected {
            self.set_msg("Device not connected", MessageStyle::Red);
            return;
        }
        let name = match self.current_key_name() {
            Some(n) => n,
            None => {
                self.set_msg("No OTP key selected.", MessageStyle::Yellow);
                return;
            }
        };

        self.busy = true;
        self.set_msg(
            &format!("Fetching OTP for '{}' — touch sensor to authorize…", name),
            MessageStyle::Yellow,
        );

        let cmd = format!("GET:otp:{}", name);
        let tx = self.action_tx.clone();
        thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let mut client = DaemonClient::connect()?;
                client.send(&cmd)
            })();
            match result {
                Ok(rsp) => {
                    let rsp = rsp.trim();
                    if let Some(code) = rsp.strip_prefix("OK:") {
                        let _ = tx.send(ActionResult::Message(
                            format!("OTP code for '{}': {}", name, code),
                            MessageStyle::Green,
                        ));
                    } else {
                        let _ = tx.send(ActionResult::Message(
                            format!("OTP failed: {}", rsp),
                            MessageStyle::Red,
                        ));
                    }
                }
                Err(e) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("OTP error: {}", e),
                        MessageStyle::Red,
                    ));
                }
            }
            let _ = tx.send(ActionResult::Done);
        });
    }
}

/// Returned by [`App::request_pam_action`] so the event loop can leave
/// alternate-screen mode before running pkexec.
#[derive(Debug, Clone, Copy)]
pub struct PamRequest {
    pub action: &'static str, // "add" | "remove"
    pub service: &'static str,
}

/// Send a single command via a fresh daemon connection.
/// The daemon handles one request per connection, so we must reconnect each time.
fn daemon_send(cmd: &str) -> Result<String, String> {
    let mut client = DaemonClient::connect()?;
    client.send(cmd)
}

fn check_pam(service: &str) -> bool {
    let path = format!("/etc/pam.d/{}", service);
    std::fs::read_to_string(path)
        .map(|c| c.contains("pam_immurok.so"))
        .unwrap_or(false)
}
