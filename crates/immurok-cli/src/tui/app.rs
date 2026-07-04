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

/// Top-level page shown in the content area. Switched with 1-4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Dashboard,
    Keys,
    Pam,
    Logs,
    Firmware,
}

impl Tab {
    pub const ALL: [Tab; 5] = [Tab::Dashboard, Tab::Keys, Tab::Pam, Tab::Logs, Tab::Firmware];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Dashboard => "Dashboard",
            Tab::Keys => "Keys",
            Tab::Pam => "PAM",
            Tab::Logs => "Logs",
            Tab::Firmware => "Firmware",
        }
    }

    pub fn hotkey(self) -> char {
        match self {
            Tab::Dashboard => '1',
            Tab::Keys => '2',
            Tab::Pam => '3',
            Tab::Logs => '4',
            Tab::Firmware => '5',
        }
    }
}

/// Modal interaction state layered on top of the active tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    DeleteSelect,
    /// Add-key text input (SSH: name only; OTP/API: name then secret/value).
    KeyInput,
    /// Confirm key deletion (y / n). Stores the pending category+index.
    KeyDeleteConfirm,
    /// Full-screen help overlay (read-only).
    Help,
}

/// Firmware page state machine (mirrors macOS FirmwareUpdateService.state).
#[derive(Debug, Clone)]
pub enum FwState {
    Idle,
    Checking,
    UpToDate,
    /// prepare() succeeded — details live in App::fw_prepared.
    Ready,
    Updating { stage: String, fraction: f64, hop: usize, hops: usize },
    Success(String),
    Failed(String),
}

/// Staged add-key input flow (Mode::KeyInput).
/// SSH: name only. API: name → value. OTP: name → service → secret.
#[derive(Debug, Clone)]
pub struct KeyAddFlow {
    pub cat: KeyTab,
    /// Set once the name stage is confirmed.
    pub name: Option<String>,
    /// Set once the OTP service stage is confirmed (may be empty).
    pub service: Option<String>,
}

/// Which field a KeyAddFlow is currently collecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyInputStage {
    Name,
    Service,
    Secret,
}

impl KeyAddFlow {
    pub fn stage(&self) -> KeyInputStage {
        if self.name.is_none() {
            KeyInputStage::Name
        } else if self.cat == KeyTab::Otp && self.service.is_none() {
            KeyInputStage::Service
        } else {
            KeyInputStage::Secret
        }
    }
}

/// One entry in the Dashboard event feed.
#[derive(Debug, Clone)]
pub struct EventEntry {
    pub time: String,
    pub text: String,
    pub style: MessageStyle,
}

/// Max entries retained in the Dashboard event feed.
pub const EVENT_BUFFER_CAP: usize = 200;

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

    pub fn prev(self) -> Self {
        match self {
            KeyTab::Ssh => KeyTab::Api,
            KeyTab::Otp => KeyTab::Ssh,
            KeyTab::Api => KeyTab::Otp,
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
    /// Issuer / service (OTP only; empty for API).
    pub service: String,
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

    // Firmware update state
    /// Set by the startup silent check / prepare when an update exists.
    pub fw_update_available: Option<String>,
    pub fw_state: FwState,
    pub fw_prepared: Option<crate::fwupdate::PreparedUpdate>,

    // Settings
    pub unlock_sudo: bool,
    pub unlock_polkit: bool,
    pub unlock_screen: bool,
    pub lock_screen: bool,
    pub pam_sudo: bool,
    pub pam_polkit: bool,
    pub pam_screen: bool,

    // UI state
    pub tab: Tab,
    pub mode: Mode,
    pub busy: bool,
    pub message: String,
    pub message_style: MessageStyle,
    /// Recent event feed shown on the Dashboard (newest at the back).
    pub events: VecDeque<EventEntry>,

    // Enrollment progress (only meaningful while busy in Normal mode)
    pub enroll_active: bool,
    /// True while the FP gate is pending — the device wants an *enrolled*
    /// finger to authorize before new-finger capture starts.
    pub enroll_gate: bool,
    pub enroll_slot: u8,
    pub enroll_current: u8,
    pub enroll_total: u8,

    // Keys panel state
    pub key_tab: KeyTab,
    pub key_cursor: usize,
    pub ssh_keys: Vec<SshKeyRow>,
    pub otp_keys: Vec<NameKeyRow>,
    pub api_keys: Vec<NameKeyRow>,
    /// Pending text input (key name / secret during KeyInput).
    pub input_buf: String,
    /// Active add-key flow while in KeyInput mode.
    pub key_add_flow: Option<KeyAddFlow>,
    /// Pending delete target (cat + index) while in KeyDeleteConfirm.
    pub pending_delete: Option<(KeyTab, u8)>,

    // PAM panel cursor
    pub pam_cursor: usize,

    // Logs panel state
    pub log_lines: VecDeque<String>,
    /// 0 = follow tail; >0 = number of lines above the tail the viewport
    /// is anchored at.
    pub log_scroll: usize,
    /// Owned `tail -F` child while the Logs panel is open. Kept here so
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
    /// Message shown in the footer only — never mirrored into the event
    /// feed (used for secrets like API key values).
    SecretMessage(String),
    /// FP gate passed — new-finger capture begins (clears the gate hint).
    EnrollStarted,
    /// Live enrollment progress update (current, total).
    EnrollProgress { current: u8, total: u8 },
    /// One line of streamed log-tail output for the Logs panel.
    LogLine(String),
    /// log-tail pipe closed (process exited or readers hit EOF).
    LogEnded,
    /// Startup silent check result: Some(version) if an update is available.
    FwSilentCheck(Option<String>),
    /// fw prepare finished (firmware page entered / re-checked).
    FwPrepared(Box<Result<Option<crate::fwupdate::PreparedUpdate>, String>>),
    /// Live firmware push progress.
    FwProgress { stage: String, fraction: f64, hop: usize, hops: usize },
    /// Firmware update finished: Ok(target version) / Err(message).
    FwFinished(Result<String, String>),
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
            fw_update_available: None,
            fw_state: FwState::Idle,
            fw_prepared: None,
            unlock_sudo: true,
            unlock_polkit: true,
            unlock_screen: true,
            lock_screen: false,
            pam_sudo: false,
            pam_polkit: false,
            pam_screen: false,
            tab: Tab::Dashboard,
            mode: Mode::Normal,
            busy: false,
            message: "Ready".into(),
            message_style: MessageStyle::Dim,
            events: VecDeque::with_capacity(EVENT_BUFFER_CAP),
            enroll_active: false,
            enroll_gate: false,
            enroll_slot: 0,
            enroll_current: 0,
            enroll_total: 0,
            key_tab: KeyTab::Ssh,
            key_cursor: 0,
            ssh_keys: Vec::new(),
            otp_keys: Vec::new(),
            api_keys: Vec::new(),
            input_buf: String::new(),
            key_add_flow: None,
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
                    self.set_msg(&msg, style);
                }
                ActionResult::SecretMessage(msg) => {
                    self.message = msg;
                    self.message_style = MessageStyle::Green;
                }
                ActionResult::Refresh => needs_refresh = true,
                ActionResult::EnrollStarted => {
                    self.enroll_gate = false;
                }
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
                    if self.tab == Tab::Logs {
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
                    self.enroll_gate = false;
                }
                ActionResult::FwSilentCheck(v) => {
                    self.fw_update_available = v;
                }
                ActionResult::FwPrepared(r) => match *r {
                    Ok(Some(prep)) => {
                        self.fw_update_available = Some(prep.target_version.clone());
                        self.fw_prepared = Some(prep);
                        self.fw_state = FwState::Ready;
                    }
                    Ok(None) => {
                        self.fw_update_available = None;
                        self.fw_state = FwState::UpToDate;
                    }
                    Err(e) => {
                        self.fw_state = FwState::Failed(e);
                    }
                },
                ActionResult::FwProgress { stage, fraction, hop, hops } => {
                    self.fw_state = FwState::Updating { stage, fraction, hop, hops };
                }
                ActionResult::FwFinished(r) => {
                    self.busy = false;
                    match r {
                        Ok(v) => {
                            self.fw_state = FwState::Success(v);
                            self.fw_update_available = None;
                            needs_refresh = true;
                        }
                        Err(e) => self.fw_state = FwState::Failed(e),
                    }
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
                            _ => {}
                        }
                    }
                }
            }
        }

        // PAM status
        self.pam_sudo = immurok_common::pam::pam_line_present("sudo");
        self.pam_polkit = immurok_common::pam::pam_line_present("polkit-1");
        self.pam_screen = immurok_common::pam::pam_line_present("gdm-password");

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
                let service = e["service"].as_str().unwrap_or("").to_string();
                let cat = e["category"].as_str().unwrap_or("");
                let row = NameKeyRow { index: idx, name, service };
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

    /// Enroll into the lowest empty slot. Slot picking is intentionally
    /// not exposed in the TUI — `immurok-cli fp enroll <slot>` still takes
    /// an explicit slot if ever needed.
    pub fn auto_enroll(&mut self) {
        if !self.guard_paired() {
            return;
        }
        if !self.connected {
            self.set_msg("Device not connected", MessageStyle::Red);
            return;
        }
        if self.busy {
            return;
        }
        let slot = (0..protocol::MAX_FINGERPRINT_SLOTS)
            .find(|i| self.fp_bitmap & (1 << i) == 0);
        match slot {
            Some(s) => self.action_enroll(s),
            None => self.set_msg("All fingerprint slots are full", MessageStyle::Red),
        }
    }

    pub fn enter_delete_select(&mut self) {
        if !self.guard_paired() {
            return;
        }
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
        if !self.guard_paired() {
            return;
        }
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
        // With fingerprints already enrolled the firmware FP-gates
        // ENROLL_START: an *enrolled* finger must authorize first.
        self.enroll_gate = self.fp_bitmap != 0;
        self.enroll_slot = slot;
        self.enroll_current = 0;
        self.enroll_total = 6;
        if self.enroll_gate {
            self.set_msg(
                &format!(
                    "Enrolling slot {} — verify with an enrolled finger to authorize…",
                    slot
                ),
                MessageStyle::Yellow,
            );
        } else {
            self.set_msg(
                &format!("Enrolling slot {} — place finger on sensor…", slot),
                MessageStyle::Yellow,
            );
        }

        let tx = self.action_tx.clone();
        thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let rsp = daemon_send(&format!("FP:ENROLL:{}", slot))?;
                if !rsp.contains("ENROLL_STARTED") {
                    return Err(format!("Start failed: {}", rsp));
                }

                let _ = tx.send(ActionResult::EnrollStarted);
                let _ = tx.send(ActionResult::Message(
                    "Enrollment started — place finger…".into(),
                    MessageStyle::Yellow,
                ));

                // Poll FP:STATUS for enrollment progress (6 captures × 30s = 180s max)
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
                            if bitmap_polls.is_multiple_of(20) {
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
                        let total: u8 = parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(6);
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
        if !self.guard_paired() {
            return;
        }
        if self.busy {
            return;
        }
        if !self.connected {
            self.set_msg("Device not connected", MessageStyle::Red);
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
        if !self.guard_paired() {
            return;
        }
        self.toggle_setting("sudo", "UNLOCK_SUDO", self.unlock_sudo);
    }

    pub fn action_toggle_polkit(&mut self) {
        if !self.guard_paired() {
            return;
        }
        self.toggle_setting("polkit", "UNLOCK_POLKIT", self.unlock_polkit);
    }

    pub fn action_toggle_screen(&mut self) {
        if !self.guard_paired() {
            return;
        }
        self.toggle_setting("screen", "UNLOCK_SCREEN", self.unlock_screen);
    }

    pub fn action_toggle_lock(&mut self) {
        if !self.guard_paired() {
            return;
        }
        self.toggle_setting("lock", "LOCK_SCREEN", self.lock_screen);
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
        if !self.guard_paired() {
            return;
        }
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

        // Mirror meaningful messages into the Dashboard event feed.
        // Skip idle chatter ("Ready", key-hint reminders) and duplicates.
        if msg.is_empty() || msg == "Ready" || style == MessageStyle::Dim {
            return;
        }
        if self.events.back().is_some_and(|e| e.text == msg) {
            return;
        }
        if self.events.len() >= EVENT_BUFFER_CAP {
            self.events.pop_front();
        }
        self.events.push_back(EventEntry {
            time: chrono::Local::now().format("%H:%M:%S").to_string(),
            text: msg.to_string(),
            style,
        });
    }

    pub fn set_msg_dim(&mut self, msg: &str) {
        self.set_msg(msg, MessageStyle::Dim);
    }

    /// Footer-only message that never enters the event feed — for input
    /// prompts and validation errors (they're guidance, not history).
    pub fn set_msg_prompt(&mut self, msg: &str, style: MessageStyle) {
        self.message = msg.to_string();
        self.message_style = style;
    }

    /// Pre-pair gate for device-facing TUI actions (mirrors the CLI's
    /// requires_pairing table). Returns false and shows a hint when the
    /// device is not paired yet.
    ///
    /// Intentionally checked BEFORE connected/busy in every gated action:
    /// pairing is the prerequisite path, so an unpaired user always gets
    /// the "pair first" hint regardless of connection state.
    pub fn guard_paired(&mut self) -> bool {
        if self.paired {
            return true;
        }
        self.set_msg("Pair the device first (press p).", MessageStyle::Yellow);
        false
    }

    // ── Help overlay ──────────────────────────────────────────

    pub fn toggle_help(&mut self) {
        self.mode = match self.mode {
            Mode::Help => Mode::Normal,
            _ => Mode::Help,
        };
    }

    // ── Tab switching ─────────────────────────────────────────

    /// Switch the active tab, running enter/leave side effects
    /// (log-tail child lifecycle, key cache refresh, cursor clamps).
    pub fn set_tab(&mut self, tab: Tab) {
        if self.tab == tab {
            return;
        }

        // Leave side effects
        if self.tab == Tab::Logs {
            self.stop_log_stream();
        }

        self.tab = tab;
        self.mode = Mode::Normal;

        // Enter side effects
        match tab {
            Tab::Dashboard => {}
            Tab::Keys => {
                self.refresh_keys();
                if self.key_cursor >= self.current_key_len().max(1) {
                    self.key_cursor = 0;
                }
            }
            Tab::Pam => {
                if self.pam_cursor >= PAM_SERVICES.len() {
                    self.pam_cursor = 0;
                }
            }
            Tab::Logs => self.start_log_stream(),
            Tab::Firmware => {}
        }
        self.set_msg_dim("Ready");
    }

    // ── PAM panel ─────────────────────────────────────────────

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
            services: vec![svc.service],
        })
    }

    /// 一键修复：按 daemon 开关派生目标里、当前缺失的服务。
    pub fn request_pam_repair(&self) -> Option<PamRequest> {
        let services =
            crate::commands::pam::services_to_repair(self.unlock_sudo, self.unlock_polkit);
        if services.is_empty() {
            return None;
        }
        Some(PamRequest { action: "add", services })
    }

    // ── Logs panel ────────────────────────────────────────────

    /// Tail `~/.immurok/logs.txt` (the daemon's tracing output — the journal
    /// only carries systemd start/stop lines) and stream its stdout into the
    /// in-TUI log ring buffer via the existing action channel. `-F` follows
    /// across truncation/rotation and waits for the file to appear.
    fn start_log_stream(&mut self) {
        if self.log_child.is_some() {
            return;
        }

        let home = std::env::var("HOME").unwrap_or_default();
        let log_path = std::path::PathBuf::from(&home)
            .join(protocol::IMMUROK_DIR)
            .join(protocol::LOG_FILE);

        let mut child = match Command::new("tail")
            .args(["-n", "200", "-F"])
            .arg(&log_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                self.set_msg(&format!("Failed to start tail: {}", e), MessageStyle::Red);
                return;
            }
        };

        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                self.set_msg("tail produced no stdout", MessageStyle::Red);
                return;
            }
        };

        self.log_lines.clear();
        self.log_scroll = 0;

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

    /// Kill the log-tail child (if any) and drop the buffer — re-entering
    /// Logs starts fresh and avoids accumulating memory across sessions.
    fn stop_log_stream(&mut self) {
        if let Some(mut child) = self.log_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.log_lines.clear();
        self.log_scroll = 0;
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
        let svc_list = req.services.join(", ");
        if ok {
            self.set_msg(
                &format!("PAM {} done: {}", verb, svc_list),
                MessageStyle::Green,
            );
        } else {
            self.set_msg(
                &format!("PAM {} for {} failed (see terminal output above).", verb, svc_list),
                MessageStyle::Red,
            );
        }
    }

    // ── Keys panel actions ───────────────────────────────────

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
        if !self.guard_paired() {
            return;
        }
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

    /// Enter the add-key flow for the current category.
    /// SSH: collects a name then issues `KEY:GENERATE` (on-device keypair).
    /// OTP: collects name + base32 secret → `KEY:OTP_IMPORT`.
    /// API: collects name + value → `KEY:API_IMPORT`.
    pub fn enter_key_add(&mut self) {
        if !self.guard_paired() {
            return;
        }
        if !self.connected {
            self.set_msg("Device not connected", MessageStyle::Red);
            return;
        }
        let (count, max) = match self.key_tab {
            KeyTab::Ssh => (self.ssh_keys.len(), protocol::KEY_MAX_SSH),
            KeyTab::Otp => (self.otp_keys.len(), protocol::KEY_MAX_OTP),
            KeyTab::Api => (self.api_keys.len(), protocol::KEY_MAX_API),
        };
        if count >= max as usize {
            self.set_msg(
                &format!(
                    "{} keystore full ({}/{}) — delete an entry first.",
                    self.key_tab.label(),
                    count,
                    max
                ),
                MessageStyle::Red,
            );
            return;
        }
        self.input_buf.clear();
        self.key_add_flow = Some(KeyAddFlow {
            cat: self.key_tab,
            name: None,
            service: None,
        });
        self.mode = Mode::KeyInput;
        let hint = match self.key_tab {
            KeyTab::Ssh => "Name the new SSH keypair (generated on-device).",
            KeyTab::Otp => "Name the OTP entry (secret comes next).",
            KeyTab::Api => "Name the API key (value comes next).",
        };
        self.set_msg_prompt(hint, MessageStyle::Yellow);
    }

    /// Max input length for the current KeyInput stage.
    pub fn input_max(&self) -> usize {
        match &self.key_add_flow {
            Some(f) => match f.stage() {
                // Firmware field widths minus the NUL terminator.
                KeyInputStage::Name => match f.cat {
                    KeyTab::Ssh => protocol::NAME_LEN_SSH - 1,
                    KeyTab::Otp => protocol::NAME_LEN_OTP - 1,
                    KeyTab::Api => protocol::NAME_LEN_API - 1,
                },
                KeyInputStage::Service => protocol::SERVICE_LEN_OTP - 1,
                KeyInputStage::Secret => 256,
            },
            None => 15,
        }
    }

    pub fn input_push_char(&mut self, c: char) {
        if self.input_buf.chars().count() < self.input_max() && !c.is_control() {
            self.input_buf.push(c);
        }
    }

    pub fn input_pop_char(&mut self) {
        self.input_buf.pop();
    }

    pub fn input_cancel(&mut self) {
        self.input_buf.clear();
        self.key_add_flow = None;
        self.mode = Mode::Normal;
        self.set_msg("Cancelled.", MessageStyle::Dim);
    }

    /// Enter pressed in KeyInput mode — advance the add flow one step.
    pub fn input_submit_key(&mut self) {
        let flow = match self.key_add_flow.clone() {
            Some(f) => f,
            None => {
                self.mode = Mode::Normal;
                return;
            }
        };
        let text = self.input_buf.trim().to_string();

        match flow.stage() {
            // ── Stage 1: name ─────────────────────────────
            KeyInputStage::Name => {
                if text.is_empty() {
                    self.set_msg_prompt("Name cannot be empty.", MessageStyle::Red);
                    return;
                }
                if flow.cat == KeyTab::Ssh {
                    // SSH needs no secret — generate right away.
                    self.input_buf.clear();
                    self.key_add_flow = None;
                    self.mode = Mode::Normal;
                    self.action_key_generate(text);
                    return;
                }
                self.key_add_flow = Some(KeyAddFlow {
                    name: Some(text),
                    ..flow
                });
                self.input_buf.clear();
                let hint = match flow.cat {
                    KeyTab::Otp => "Service / issuer — Enter alone to skip.",
                    KeyTab::Api => "Paste the API key value.",
                    KeyTab::Ssh => unreachable!(),
                };
                self.set_msg_prompt(hint, MessageStyle::Yellow);
            }

            // ── Stage 2 (OTP only): service / issuer ──────
            KeyInputStage::Service => {
                // Empty is fine — the field is optional on-device.
                self.key_add_flow = Some(KeyAddFlow {
                    service: Some(text),
                    ..flow
                });
                self.input_buf.clear();
                self.set_msg_prompt("Paste the TOTP secret (base32).", MessageStyle::Yellow);
            }

            // ── Stage 3: secret / value ───────────────────
            KeyInputStage::Secret => {
                if text.is_empty() {
                    self.set_msg_prompt("Secret cannot be empty.", MessageStyle::Red);
                    return;
                }
                let secret: Vec<u8> = match flow.cat {
                    KeyTab::Otp => {
                        match crate::commands::keys::base32_decode(&text) {
                            Some(b) if !b.is_empty() => {
                                if b.len() > protocol::SECRET_LEN_OTP {
                                    self.set_msg_prompt(
                                        &format!(
                                            "Secret too long: {} bytes decoded (limit {}).",
                                            b.len(),
                                            protocol::SECRET_LEN_OTP
                                        ),
                                        MessageStyle::Red,
                                    );
                                    return;
                                }
                                b
                            }
                            _ => {
                                self.set_msg_prompt(
                                    "Invalid base32 secret — check for typos.",
                                    MessageStyle::Red,
                                );
                                return;
                            }
                        }
                    }
                    KeyTab::Api => {
                        if text.len() > protocol::VALUE_LEN_API {
                            self.set_msg_prompt(
                                &format!(
                                    "Value too long: {} bytes (limit {}).",
                                    text.len(),
                                    protocol::VALUE_LEN_API
                                ),
                                MessageStyle::Red,
                            );
                            return;
                        }
                        text.into_bytes()
                    }
                    KeyTab::Ssh => unreachable!(),
                };
                let name = flow.name.clone().unwrap_or_default();
                let service = flow.service.clone().unwrap_or_default();
                self.input_buf.clear();
                self.key_add_flow = None;
                self.mode = Mode::Normal;
                self.action_key_add(flow.cat, name, service, secret);
            }
        }
    }

    /// Send the staged OTP/API add to the daemon (FP-gated on device).
    fn action_key_add(&mut self, cat: KeyTab, name: String, service: String, secret: Vec<u8>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.set_msg(
            &format!(
                "Adding {} key '{}' — touch sensor to authorize…",
                cat.label(),
                name
            ),
            MessageStyle::Yellow,
        );

        let add_cat = match cat {
            KeyTab::Otp => crate::commands::keys::KeyAddCat::Otp,
            KeyTab::Api => crate::commands::keys::KeyAddCat::Api,
            KeyTab::Ssh => unreachable!("SSH adds go through KEY:GENERATE"),
        };
        let cmd = crate::commands::keys::build_key_add_cmd(add_cat, &name, &service, &secret);
        let label = cat.label();
        let tx = self.action_tx.clone();
        thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let mut client = DaemonClient::connect()?;
                client.send(&cmd)
            })();
            match result {
                Ok(rsp) if rsp.starts_with("OK") => {
                    let _ = tx.send(ActionResult::Message(
                        format!("{} key '{}' added.", label, name),
                        MessageStyle::Green,
                    ));
                }
                Ok(rsp) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Add failed: {}", rsp),
                        MessageStyle::Red,
                    ));
                }
                Err(e) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Add error: {}", e),
                        MessageStyle::Red,
                    ));
                }
            }
            let _ = tx.send(ActionResult::Refresh);
            let _ = tx.send(ActionResult::Done);
        });
    }

    /// `s` in API tab — fetch and display the selected key's value via
    /// `GET:api:<name>` (FP-gated by the daemon).
    pub fn action_key_show_api(&mut self) {
        if !self.guard_paired() {
            return;
        }
        if self.busy {
            return;
        }
        if self.key_tab != KeyTab::Api {
            self.set_msg(
                "Show value is only available on the API tab.",
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
                self.set_msg("No API key selected.", MessageStyle::Yellow);
                return;
            }
        };

        self.busy = true;
        self.set_msg(
            &format!("Fetching '{}' — touch sensor to authorize…", name),
            MessageStyle::Yellow,
        );

        let cmd = format!("GET:api:{}", name);
        let tx = self.action_tx.clone();
        thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let mut client = DaemonClient::connect()?;
                client.send(&cmd)
            })();
            match result {
                Ok(rsp) => {
                    let rsp = rsp.trim();
                    if let Some(value) = rsp.strip_prefix("OK:") {
                        // SecretMessage: footer only, never the event feed.
                        let _ = tx.send(ActionResult::SecretMessage(format!(
                            "API key '{}': {}",
                            name, value
                        )));
                    } else {
                        let _ = tx.send(ActionResult::Message(
                            format!("Fetch failed: {}", rsp),
                            MessageStyle::Red,
                        ));
                    }
                }
                Err(e) => {
                    let _ = tx.send(ActionResult::Message(
                        format!("Fetch error: {}", e),
                        MessageStyle::Red,
                    ));
                }
            }
            let _ = tx.send(ActionResult::Done);
        });
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
        if !self.guard_paired() {
            return;
        }
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
        self.mode = Mode::Normal;
        self.set_msg("Cancelled.", MessageStyle::Dim);
    }

    pub fn confirm_key_delete(&mut self) {
        let (tab, idx) = match self.pending_delete.take() {
            Some(v) => v,
            None => {
                self.mode = Mode::Normal;
                return;
            }
        };
        self.mode = Mode::Normal;
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
        if !self.guard_paired() {
            return;
        }
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
                        // SecretMessage: footer only — expired codes have no
                        // business lingering in the event feed.
                        let _ = tx.send(ActionResult::SecretMessage(format!(
                            "OTP code for '{}': {}",
                            name, code
                        )));
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

    /// Startup silent check (24h-throttled; design doc §3). Never blocks the
    /// UI and never surfaces errors — failures just mean no hint.
    pub fn spawn_fw_silent_check(&self) {
        let tx = self.action_tx.clone();
        thread::spawn(move || {
            let result = (|| -> Option<String> {
                let store = crate::fwupdate::store::FwStore::open_default().ok()?;
                let m = crate::fwupdate::fetch_manifest_cached(
                    &store,
                    false,
                    crate::fwupdate::unix_now(),
                )
                .ok()?;
                let st = crate::fwupdate::query_device_status().ok()?;
                if !st.connected {
                    return None;
                }
                let device = immurok_common::fwupdate::version::normalize_semver(&st.version);
                use immurok_common::fwupdate::planner::{self, UpdatePlan};
                match planner::plan(&device, &m.latest.version, m.latest.min_direct.as_deref()) {
                    UpdatePlan::UpToDate | UpdatePlan::Unknown => None,
                    _ => Some(m.latest.version.clone()),
                }
            })();
            let _ = tx.send(ActionResult::FwSilentCheck(result));
        });
    }

    pub fn fw_updating(&self) -> bool {
        matches!(self.fw_state, FwState::Updating { .. })
    }

    /// Enter the Firmware page; kick off a check unless one is already
    /// running or its result is still current.
    pub fn fw_enter(&mut self) {
        self.set_tab(Tab::Firmware);
        if matches!(
            self.fw_state,
            FwState::Idle | FwState::Success(_) | FwState::Failed(_)
        ) {
            self.fw_recheck();
        }
    }

    /// Force a fresh check (bypasses the 24h throttle — mirrors the macOS
    /// window's onAppear force check).
    pub fn fw_recheck(&mut self) {
        if matches!(self.fw_state, FwState::Updating { .. } | FwState::Checking) {
            return;
        }
        self.fw_state = FwState::Checking;
        self.fw_prepared = None;
        let tx = self.action_tx.clone();
        thread::spawn(move || {
            let r = crate::fwupdate::store::FwStore::open_default()
                .and_then(|store| crate::fwupdate::prepare(&store, true))
                .map_err(|e| e.to_string());
            let _ = tx.send(ActionResult::FwPrepared(Box::new(r)));
        });
    }

    /// Start the push (Enter on a Ready page). Exit keys are blocked while
    /// this runs — see the key loop in tui/mod.rs.
    pub fn fw_start_update(&mut self) {
        if !matches!(self.fw_state, FwState::Ready) {
            return;
        }
        let Some(prep) = self.fw_prepared.clone() else { return };
        let hops = prep.hops.len();
        self.fw_state = FwState::Updating {
            stage: "starting".into(),
            fraction: 0.0,
            hop: 1,
            hops,
        };
        self.busy = true;
        let tx = self.action_tx.clone();
        thread::spawn(move || {
            let target = prep.target_version.clone();
            let result = crate::fwupdate::store::FwStore::open_default()
                .and_then(|store| {
                    // Within-hop fraction reached so far. `Stage` events must
                    // never regress it (they'd otherwise snap the bar back to
                    // the hop baseline after Transfer already reached ~1.0) —
                    // "retry" is the one genuine exception, since the push
                    // really does restart from ERASE. Reset when the hop index
                    // advances, or hop N+1's early Stage events would inherit
                    // hop N's 1.0 and briefly show the next hop as complete.
                    let mut last_frac: f64 = 0.0;
                    let mut last_hop = usize::MAX;
                    let mut progress = |ev: crate::fwupdate::ProgressEvent| {
                        use crate::fwupdate::ProgressEvent as PE;
                        let ev_hop = match ev {
                            PE::Stage { hop, .. }
                            | PE::Transfer { hop, .. }
                            | PE::Reconnect { hop, .. } => hop,
                        };
                        if ev_hop != last_hop {
                            last_hop = ev_hop;
                            last_frac = 0.0;
                        }
                        // Merged two-hop progress: base + p * weight (design §3).
                        let (stage, frac, hop, hops) = match ev {
                            PE::Stage { hop, hops, name } => {
                                if name == "retry" {
                                    last_frac = 0.0;
                                }
                                (
                                    crate::fwupdate::stage_label(name).to_string(),
                                    last_frac,
                                    hop,
                                    hops,
                                )
                            }
                            PE::Transfer { hop, hops, fraction } => {
                                last_frac = fraction;
                                ("writing firmware".to_string(), fraction, hop, hops)
                            }
                            PE::Reconnect { hop, hops } => {
                                last_frac = 1.0;
                                (
                                    "waiting for device reboot".to_string(),
                                    1.0,
                                    hop,
                                    hops,
                                )
                            }
                        };
                        let fraction = (hop as f64 + frac) / hops as f64;
                        let hop = hop + 1;
                        let _ = tx.send(ActionResult::FwProgress { stage, fraction, hop, hops });
                    };
                    crate::fwupdate::execute(&store, &prep, &mut progress)
                })
                .map(|_| target)
                .map_err(|e| e.to_string());
            let _ = tx.send(ActionResult::FwFinished(result));
        });
    }

    /// Old signing era (< 1.6.0) — dashboard/status soft warning.
    pub fn fw_outdated(&self) -> bool {
        use immurok_common::fwupdate::version::{normalize_semver, FirmwareVersion};
        if !self.connected || self.fw_version.is_empty() {
            return false;
        }
        match (
            FirmwareVersion::parse(&normalize_semver(&self.fw_version)),
            FirmwareVersion::parse(crate::fwupdate::MANDATORY_MIN_VERSION),
        ) {
            (Some(v), Some(min)) => v < min,
            _ => false,
        }
    }
}

/// Returned by [`App::request_pam_action`] so the event loop can leave
/// alternate-screen mode before running pkexec.
#[derive(Debug, Clone)]
pub struct PamRequest {
    pub action: &'static str, // "add" | "remove"
    pub services: Vec<&'static str>,
}

/// Send a single command via a fresh daemon connection.
/// The daemon handles one request per connection, so we must reconnect each time.
fn daemon_send(cmd: &str) -> Result<String, String> {
    let mut client = DaemonClient::connect()?;
    client.send(cmd)
}

