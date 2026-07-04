//! Coordinator — event routing hub for all daemon modules.
//!
//! All daemon modules communicate through this shared struct. It holds:
//! - Shared state (pairing, settings, device_status, verified flag, screen_locked flag)
//! - Channels for cross-module communication (BLE commands, FP match events, enroll events)
//! - PAM pre-authorization window management
//! - FP match routing: pending PAM → approve | screen locked → loginctl unlock | else → set pre-auth

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, Notify, RwLock};
use tracing::{info, warn};

use immurok_common::types::{DeviceStatus, EnrollEvent, PairingData};
use crate::settings::Settings;

/// Commands sent to ble.rs via channel
#[derive(Debug)]
pub enum BleCommand {
    SendCommand {
        cmd: u8,
        payload: Vec<u8>,
        reply: tokio::sync::oneshot::Sender<BleResult>,
    },
    SendFpGated {
        cmd: u8,
        payload: Vec<u8>,
        reply: tokio::sync::oneshot::Sender<BleResult>,
    },
    Pair {
        reply: tokio::sync::oneshot::Sender<BleResult>,
    },
    /// Re-sync key cache from device after generate/import/delete.
    SyncKeys {
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// AUTH_REQUEST: send 0x33, get WAIT_FP, wait for [00] success via auth_pending.
    AuthRequest {
        reply: tokio::sync::oneshot::Sender<Result<bool, String>>,
    },
    OtaWriteRead {
        data: Vec<u8>,
        timeout_ms: u64,
        reply: tokio::sync::oneshot::Sender<Result<Vec<u8>, String>>,
    },
    OtaWrite {
        data: Vec<u8>,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    Disconnect,
}

pub type BleResult = Result<(u8, Vec<u8>), String>;

/// Pre-auth window granted after a real auth flow (PAM approve / screen
/// unlock). Bound to a service set so an out-of-band PAM request from an
/// unrelated process can't ride the window. Mirrors the macOS 1.2.6
/// hardening (commit 2f26dbf): the previous "any-service" path was the
/// most exploitable surface — a stray fingerprint touch would silently
/// authorize the next sudo from any same-UID process.
struct PreAuth {
    deadline: Instant,
    services: HashSet<String>,
}

/// Services allowed to ride the pre-auth window armed after a screen unlock
/// or PAM-approve flow. Excludes `sudo` deliberately — sudo via terminal
/// goes through a fresh AUTH_REQUEST cycle (or future AGENT_APPROVE), not
/// pre-auth, since pre-auth has no command context.
const UNLOCK_FOLLOWUP_SERVICES: &[&str] = &["polkit-1", "login", "gdm-password"];

/// Play a freedesktop sound theme entry by name. Tries `canberra-gtk-play`
/// first (lighter, follows XDG sound theme), then falls back to `paplay`
/// with the explicit OGA path. Failure is silent — audible cue is a nice-
/// to-have, not load-bearing.
async fn play_unlock_sound(name: &str) {
    if tokio::process::Command::new("canberra-gtk-play")
        .arg("-i")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
    {
        return;
    }
    let path = format!("/usr/share/sounds/freedesktop/stereo/{}.oga", name);
    let _ = tokio::process::Command::new("paplay")
        .arg(&path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[derive(Debug, Clone)]
pub struct FpMatchEvent {
    pub page_id: u16,
}

/// In-progress FP-gate feedback, broadcast so the SSH agent can echo a hint
/// on the client's own terminal (which it otherwise has no way to reach).
/// Only the per-attempt mismatch is broadcast here; the final approve/deny
/// outcome already flows back as the gated command's result.
#[derive(Debug, Clone, Copy)]
pub enum FpGateEvent {
    /// A touch didn't match; `remaining` attempts left before the gate fails.
    Mismatch { remaining: u8 },
    /// The touch matched (device sent RSP_FP_GATE_APPROVED). The device now
    /// spends ~2 s computing the signature before the gated call returns, so
    /// this is the right moment to show "verified — signing…" — not when the
    /// signature finally lands.
    Approved,
}

pub struct Coordinator {
    // Shared state
    pub pairing: RwLock<Option<PairingData>>,
    pub settings: RwLock<Settings>,
    pub device_status: RwLock<Option<DeviceStatus>>,
    pub is_device_verified: AtomicBool,
    pub is_connected: AtomicBool,
    pub screen_locked: AtomicBool,
    pub fp_bitmap_stale: AtomicBool,

    // Enrollment progress (status, current, total) — updated by BLE, read by socket
    pub last_enroll_event: RwLock<Option<(u8, u8, u8)>>,

    // Cross-module channels
    pub ble_cmd_tx: mpsc::Sender<BleCommand>,
    pub fp_match_tx: broadcast::Sender<FpMatchEvent>,
    pub enroll_tx: broadcast::Sender<EnrollEvent>,
    pub fp_gate_tx: broadcast::Sender<FpGateEvent>,

    // PAM pending auth
    pending_pam: RwLock<Option<tokio::sync::oneshot::Sender<bool>>>,
    pre_auth: RwLock<Option<PreAuth>>,
    // Last time an FP match drove a real auth flow (PAM approve or screen
    // unlock). Used by handle_lock_request to suppress the 0x23 long-press
    // lock that fires 1.6s after every touch rising edge — a successful auth
    // where the user lingers on the pad would otherwise immediately re-lock.
    last_auth_flow: RwLock<Option<Instant>>,

    // Notify for auth-dialog kill
    pub auth_dialog_cancel: Notify,

    // Mirrors mac cancelGateAndRelease (cdd6b07). FP-gated commands
    // (ENROLL_START / DELETE_FP / KEY_COMMIT / KEY_SIGN / KEY_OTP_GET)
    // park the single BLE worker task inside send_fp_gated_inner while
    // waiting for the user to touch the sensor — any GATE_CANCEL queued
    // via BleCommand would sit behind that wait forever. Triggering this
    // Notify lets the gated inner-loop bail out and write GATE_CANCEL
    // straight to the helper, bypassing the command queue.
    pub gate_cancel: Notify,

    // Fires when systemd-logind reports PrepareForSleep(false), i.e. the
    // machine just resumed from suspend. The BLE wait-for-device loop
    // listens for this to kick an active Device.Connect() — Linux BlueZ
    // does NOT auto-reconnect BLE LE devices the way it does classic
    // BR/EDR HID, so passively waiting for the device to reappear after
    // resume can take tens of seconds.
    pub resume_notify: Notify,

    // Paths
    pub immurok_dir: std::path::PathBuf,
}

impl Coordinator {
    pub fn new(
        ble_cmd_tx: mpsc::Sender<BleCommand>,
        immurok_dir: std::path::PathBuf,
    ) -> Arc<Self> {
        let (fp_match_tx, _) = broadcast::channel(16);
        let (enroll_tx, _) = broadcast::channel(16);
        let (fp_gate_tx, _) = broadcast::channel(16);

        Arc::new(Self {
            pairing: RwLock::new(None),
            settings: RwLock::new(Settings::default()),
            device_status: RwLock::new(None),
            is_device_verified: AtomicBool::new(false),
            is_connected: AtomicBool::new(false),
            screen_locked: AtomicBool::new(false),
            fp_bitmap_stale: AtomicBool::new(false),
            last_enroll_event: RwLock::new(None),
            ble_cmd_tx,
            fp_match_tx,
            enroll_tx,
            fp_gate_tx,
            pending_pam: RwLock::new(None),
            pre_auth: RwLock::new(None),
            last_auth_flow: RwLock::new(None),
            auth_dialog_cancel: Notify::new(),
            gate_cancel: Notify::new(),
            resume_notify: Notify::new(),
            immurok_dir,
        })
    }

    /// Core FP match routing logic — 3-way: pending PAM → approve | screen locked → unlock | else → pre-auth
    pub async fn on_fp_match(&self, page_id: u16) {
        // Broadcast to all subscribers (TUI, socket status)
        let _ = self.fp_match_tx.send(FpMatchEvent { page_id });

        // 1. Pending PAM request? → approve
        if self.approve_pending_pam().await {
            info!("FP match → approved pending PAM request");
            *self.last_auth_flow.write().await = Some(Instant::now());
            // Screen unlock may trigger follow-up PAM requests (polkit, login)
            if self.screen_locked.load(Ordering::Relaxed) {
                self.set_pre_auth(
                    Duration::from_secs(immurok_common::protocol::PRE_AUTH_DURATION_SECS),
                    UNLOCK_FOLLOWUP_SERVICES,
                )
                .await;
            }
            return;
        }

        // 2. Screen locked + unlock enabled? → loginctl unlock-session
        let settings = self.settings.read().await;
        if self.screen_locked.load(Ordering::Relaxed) && settings.unlock_screen {
            drop(settings);
            info!("FP match → unlocking screen");
            *self.last_auth_flow.write().await = Some(Instant::now());
            self.unlock_screen().await;
            return;
        }

        // 3. No identified auth context — do NOT pre-authorize.
        //    Removed in alignment with macOS 1.2.6 (commit 2f26dbf): a stray
        //    fingerprint touch with no PAM/unlock target previously armed a
        //    10s any-service window — the most exploitable surface, since
        //    any same-UID process could ride it. Refuse to grant authority
        //    without explicit context. AGENT_APPROVE will be the future
        //    explicit-context entry for non-PAM commands.
        drop(settings);
        info!("FP match → no auth context, ignoring (no pre-auth armed)");
    }

    /// Arm a pre-auth window for `services` (case-insensitive match against
    /// the PAM service name in handle_auth). Replaces any prior window.
    pub async fn set_pre_auth(&self, duration: Duration, services: &[&str]) {
        let entry = PreAuth {
            deadline: Instant::now() + duration,
            services: services.iter().map(|s| s.to_lowercase()).collect(),
        };
        *self.pre_auth.write().await = Some(entry);
    }

    /// Check pre-auth window — true iff within deadline AND `service` is in
    /// the bound set. Does NOT consume: multiple in-set PAM requests within
    /// the window are all approved (e.g. polkit fires twice on unlock).
    pub async fn consume_pre_auth(&self, service: &str) -> bool {
        let pa = self.pre_auth.read().await;
        if let Some(ref entry) = *pa {
            if Instant::now() < entry.deadline
                && entry.services.contains(&service.to_lowercase())
            {
                return true;
            }
        }
        false
    }

    /// Try to register a pending PAM AUTH. Returns false if another AUTH is
    /// already in flight — caller should respond BUSY rather than overwrite
    /// the previous channel (which would orphan the first PAM request and
    /// route on_fp_match to the wrong sender).
    pub async fn try_set_pending_pam(
        &self,
        sender: tokio::sync::oneshot::Sender<bool>,
    ) -> bool {
        let mut pending = self.pending_pam.write().await;
        if pending.is_some() {
            return false;
        }
        *pending = Some(sender);
        true
    }

    async fn approve_pending_pam(&self) -> bool {
        let mut pending = self.pending_pam.write().await;
        if let Some(sender) = pending.take() {
            let _ = sender.send(true);
            return true;
        }
        false
    }

    pub async fn deny_pending_pam(&self) {
        let mut pending = self.pending_pam.write().await;
        if let Some(sender) = pending.take() {
            let _ = sender.send(false);
        }
    }

    async fn unlock_screen(&self) {
        // Audible cue BEFORE loginctl: there's a ~2s black-screen window
        // between fingerprint and the password being injected, and it's
        // helpful to know the daemon registered the touch even if the
        // screen hasn't redrawn yet. Mirrors macOS unlockSound (f114466).
        let sound = self.settings.read().await.unlock_sound.clone();
        if !sound.is_empty() {
            play_unlock_sound(&sound).await;
        }

        let result = tokio::process::Command::new("loginctl")
            .arg("unlock-session")
            .output()
            .await;
        match result {
            Ok(output) if output.status.success() => {
                info!("Screen unlocked via loginctl");
                self.set_pre_auth(
                    Duration::from_secs(immurok_common::protocol::PRE_AUTH_DURATION_SECS),
                    UNLOCK_FOLLOWUP_SERVICES,
                )
                .await;
            }
            Ok(output) => warn!("loginctl unlock-session failed: {:?}", output.status),
            Err(e) => warn!("Failed to run loginctl: {}", e),
        }
    }

    /// Routes a 0x23 long-press lock request from the device.
    /// Skips when:
    ///   - feature disabled (settings.lock_screen=false, default)
    ///   - screen already locked
    ///   - within LOCK_SUPPRESS_WINDOW of a real auth flow (firmware fires
    ///     LOCK_HOLD 1.6s after every touch rising edge regardless of
    ///     match outcome — without this guard a successful auth where the
    ///     user lingers on the pad would re-lock immediately)
    pub async fn handle_lock_request(&self) {
        let enabled = self.settings.read().await.lock_screen;
        if !enabled {
            info!("Lock request ignored: feature disabled (lock_screen=false)");
            return;
        }
        if self.screen_locked.load(Ordering::Relaxed) {
            info!("Lock request ignored: screen already locked");
            return;
        }
        if let Some(last) = *self.last_auth_flow.read().await {
            if last.elapsed()
                < Duration::from_secs(immurok_common::protocol::LOCK_SUPPRESS_WINDOW_SECS)
            {
                info!("Lock request ignored: recent auth flow (tail)");
                return;
            }
        }
        info!("Lock request: locking screen");
        // Locking invalidates any preceding pre-auth context.
        *self.pre_auth.write().await = None;
        self.lock_screen().await;
    }

    async fn lock_screen(&self) {
        let result = tokio::process::Command::new("loginctl")
            .arg("lock-session")
            .output()
            .await;
        match result {
            Ok(output) if output.status.success() => info!("Screen locked via loginctl"),
            Ok(output) => warn!("loginctl lock-session failed: {:?}", output.status),
            Err(e) => warn!("Failed to run loginctl: {}", e),
        }
    }

    /// Send a BLE command via the channel
    pub async fn ble_send(&self, cmd: u8, payload: Vec<u8>) -> BleResult {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.ble_cmd_tx
            .send(BleCommand::SendCommand {
                cmd,
                payload,
                reply: tx,
            })
            .await
            .map_err(|_| "BLE channel closed".to_string())?;
        rx.await.map_err(|_| "BLE reply dropped".to_string())?
    }

    /// Broadcast an in-progress FP-gate event (best-effort; no-op when nobody
    /// is subscribed, e.g. a sudo/PAM gate rather than an SSH sign).
    pub fn emit_fp_gate_event(&self, ev: FpGateEvent) {
        let _ = self.fp_gate_tx.send(ev);
    }

    /// Subscribe to in-progress FP-gate events for the duration of one gate.
    pub fn subscribe_fp_gate(&self) -> broadcast::Receiver<FpGateEvent> {
        self.fp_gate_tx.subscribe()
    }

    /// Send an FP-gated BLE command (device prompts for fingerprint before executing)
    pub async fn ble_send_fp_gated(&self, cmd: u8, payload: Vec<u8>) -> BleResult {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.ble_cmd_tx
            .send(BleCommand::SendFpGated {
                cmd,
                payload,
                reply: tx,
            })
            .await
            .map_err(|_| "BLE channel closed".to_string())?;
        rx.await.map_err(|_| "BLE reply dropped".to_string())?
    }

    /// Send AUTH_REQUEST and wait for fingerprint result (handles [00] directly).
    pub async fn ble_auth_request(&self) -> Result<bool, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.ble_cmd_tx
            .send(BleCommand::AuthRequest { reply: tx })
            .await
            .map_err(|_| "BLE channel closed".to_string())?;
        rx.await.map_err(|_| "BLE reply dropped".to_string())?
    }

    /// Write to OTA characteristic and poll-read response (with timeout).
    pub async fn ota_write_and_read(&self, data: Vec<u8>, timeout_ms: u64) -> Result<Vec<u8>, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.ble_cmd_tx
            .send(BleCommand::OtaWriteRead {
                data,
                timeout_ms,
                reply: tx,
            })
            .await
            .map_err(|_| "BLE channel closed".to_string())?;
        rx.await.map_err(|_| "BLE reply dropped".to_string())?
    }

    /// Write to OTA characteristic (fire and forget, no read).
    pub async fn ota_write(&self, data: Vec<u8>) -> Result<(), String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.ble_cmd_tx
            .send(BleCommand::OtaWrite {
                data,
                reply: tx,
            })
            .await
            .map_err(|_| "BLE channel closed".to_string())?;
        rx.await.map_err(|_| "BLE reply dropped".to_string())?
    }

    /// Trigger key cache re-sync from device.
    pub async fn sync_keys(&self) -> Result<(), String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.ble_cmd_tx
            .send(BleCommand::SyncKeys { reply: tx })
            .await
            .map_err(|_| "BLE channel closed".to_string())?;
        rx.await.map_err(|_| "BLE reply dropped".to_string())?
    }

    pub fn settings_path(&self) -> std::path::PathBuf {
        self.immurok_dir
            .join(immurok_common::protocol::SETTINGS_FILE)
    }

    pub fn pairing_path(&self) -> std::path::PathBuf {
        self.immurok_dir
            .join(immurok_common::protocol::PAIRING_FILE)
    }
}
