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

/// How long an `imk run --agent` claim stays useful for log classification.
const AGENT_CLAIM_TTL_SECS: u64 = 3600;
use tracing::{info, warn};

use immurok_common::types::{DeviceStatus, EnrollEvent, PairProgress, PairingData};
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
    // Not yet wired to a caller; part of the planned worker control surface.
    #[allow(dead_code)]
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

#[derive(Debug, Clone)]
pub struct FpMatchEvent {
    // Carried for future per-page diagnostics; not read yet.
    #[allow(dead_code)]
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
    /// Set ONLY when the challenge-response HMAC genuinely verified against
    /// this host's shared key. Unlike `is_device_verified` — which is set
    /// optimistically on an unexpected answer, a timeout, or no pairing at
    /// all, for backwards compatibility — this is real proof, so it is the
    /// only sound basis for "the slot the device is presenting is MINE".
    /// 设备在 GET_STATUS 里自报「我没和你配对」。
    ///
    /// 设备被工厂复位（或本机对应的槽在别处被清掉）之后，主机的 pairing.json
    /// 还在，于是每个界面都显示 Paired: Yes 而所有认证静默失败。设备其实在每个
    /// 会话的第一条响应里就说了，这个标志把那句话保存下来供 UI 使用。
    pub device_reports_unpaired: AtomicBool,

    pub challenge_verified: AtomicBool,
    pub is_connected: AtomicBool,
    pub screen_locked: AtomicBool,
    pub fp_bitmap_stale: AtomicBool,

    // Enrollment progress (status, current, total) — updated by BLE, read by socket
    pub last_enroll_event: RwLock<Option<(u8, u8, u8)>>,

    /// Pairing progress, polled by the CLI/TUI over PAIR:PROGRESS.
    pub pair_progress: RwLock<PairProgress>,

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

    // 用户会话代理（immurok-session-agent）的出站通道。daemon 以系统用户运行、
    // 没有会话总线也没有显示，所有 UI 都得托它做。None = 没人订阅：那就没有
    // 窗口，认证本身不受影响 —— 触摸才是门。
    pub ui_out: RwLock<Option<mpsc::UnboundedSender<String>>>,
    /// 代理报上来的「用户点了取消」。broadcast 是因为 handle_auth 与
    /// handle_agent_approve 都要在各自的 select 里等这一件事。
    pub ui_cancel_tx: broadcast::Sender<()>,

    /// `imk run --agent` 声称过的进程：pid → (记录时刻, 命令)。
    ///
    /// 以前这是 ~/.immurok/markers/<pid> 文件。daemon 换成专用系统用户之后
    /// 那些文件它读不到（0600 归调用者），而放宽到 0644 会把 agent 执行的
    /// 命令文本泄露给同机其他用户。现在改由 AGENT_APPROVE 自己登记：pid 来
    /// 自 SO_PEERCRED，比任何人都能创建的文件更可信，也不用在 /run 下留一
    /// 个全局可写目录。
    ///
    /// 仍然只用于日志归类，不参与放行。
    agent_claims: RwLock<std::collections::HashMap<u32, (Instant, String)>>,

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

    // True exactly while the single BLE worker is parked in the
    // `send_fp_gated_inner` wait loop, i.e. while there IS a waiter on
    // `gate_cancel`. Callers must check it before `notify_one()`: Notify
    // stores a permit when nobody is waiting, so a cancel fired outside a
    // gate would silently kill the NEXT gate ("FP-gate cancelled" out of
    // nowhere). Set/cleared by a drop guard inside send_fp_gated_inner, so
    // every exit path (break, early return, error) clears it.
    pub gate_active: AtomicBool,

    // Fires when systemd-logind reports PrepareForSleep(false), i.e. the
    // machine just resumed from suspend. The BLE wait-for-device loop
    // listens for this to kick an active Device.Connect() — Linux BlueZ
    // does NOT auto-reconnect BLE LE devices the way it does classic
    // BR/EDR HID, so passively waiting for the device to reappear after
    // resume can take tens of seconds.
    pub resume_notify: Notify,

    // Set while logind is between PrepareForSleep(true) and (false). The BLE
    // wait loop must not fire active Device.Connect() in this window — a
    // pending LE create-connection left across the suspend boundary races the
    // kernel's hci resume re-init and can wedge the controller firmware.
    pub is_suspending: AtomicBool,

    // Paths
    pub state_dir: std::path::PathBuf,
}

impl Coordinator {
    pub fn new(
        ble_cmd_tx: mpsc::Sender<BleCommand>,
        state_dir: std::path::PathBuf,
    ) -> Arc<Self> {
        let (fp_match_tx, _) = broadcast::channel(16);
        let (enroll_tx, _) = broadcast::channel(16);
        let (fp_gate_tx, _) = broadcast::channel(16);

        Arc::new(Self {
            pairing: RwLock::new(None),
            settings: RwLock::new(Settings::default()),
            device_status: RwLock::new(None),
            is_device_verified: AtomicBool::new(false),
            device_reports_unpaired: AtomicBool::new(false),
            challenge_verified: AtomicBool::new(false),
            is_connected: AtomicBool::new(false),
            screen_locked: AtomicBool::new(false),
            fp_bitmap_stale: AtomicBool::new(false),
            last_enroll_event: RwLock::new(None),
            pair_progress: RwLock::new(PairProgress::Idle),
            ble_cmd_tx,
            fp_match_tx,
            enroll_tx,
            fp_gate_tx,
            pending_pam: RwLock::new(None),
            pre_auth: RwLock::new(None),
            last_auth_flow: RwLock::new(None),
            ui_out: RwLock::new(None),
            ui_cancel_tx: broadcast::channel(4).0,
            agent_claims: RwLock::new(std::collections::HashMap::new()),
            auth_dialog_cancel: Notify::new(),
            gate_cancel: Notify::new(),
            gate_active: AtomicBool::new(false),
            resume_notify: Notify::new(),
            is_suspending: AtomicBool::new(false),
            state_dir,
        })
    }

    pub async fn set_pair_progress(&self, p: PairProgress) {
        *self.pair_progress.write().await = p;
    }

    pub async fn pair_progress(&self) -> PairProgress {
        *self.pair_progress.read().await
    }

    /// Claim the pairing slot: succeeds only when no attempt is in flight,
    /// and resets progress to Idle as it claims. Check and set happen under
    /// one write guard so two concurrent PAIR:START connections cannot both
    /// see an idle state and race.
    pub async fn try_begin_pairing(&self) -> bool {
        let mut p = self.pair_progress.write().await;
        if matches!(
            *p,
            PairProgress::WaitFp | PairProgress::WaitButton | PairProgress::Ecdh
        ) {
            return false;
        }
        *p = PairProgress::Idle;
        true
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

    /// The owner's graphical session id.
    ///
    /// `loginctl unlock-session` with no argument acts on *the caller's own*
    /// session — which a system daemon does not have, so it could only ever
    /// fail. The target has to be named explicitly, and unlocking someone
    /// else's session is what the polkit rule we ship allows.
    async fn target_session(&self) -> Option<String> {
        let uid = crate::session::owner_uid()?;
        crate::session::graphical_session_id(uid).await
    }

    async fn unlock_screen(&self) {
        let Some(session_id) = self.target_session().await else {
            warn!("Cannot unlock: no graphical session for the device owner");
            return;
        };
        let result = tokio::process::Command::new("loginctl")
            .arg("unlock-session")
            .arg(&session_id)
            .output()
            .await;
        match result {
            Ok(output) if output.status.success() => {
                info!("Screen unlocked via loginctl (session {})", session_id);
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
        let Some(session_id) = self.target_session().await else {
            warn!("Cannot lock: no graphical session for the device owner");
            return;
        };
        let result = tokio::process::Command::new("loginctl")
            .arg("lock-session")
            .arg(&session_id)
            .output()
            .await;
        match result {
            Ok(output) if output.status.success() => {
                info!("Screen locked via loginctl (session {})", session_id)
            }
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

    /// Remember that `pid` ran a command through `imk run --agent`.
    pub async fn record_agent_claim(&self, pid: u32, command: &str) {
        let mut claims = self.agent_claims.write().await;
        // Drop entries whose process is gone: pids get reused, and a stale
        // claim would mislabel whatever inherits the number.
        claims.retain(|p, _| std::path::Path::new(&format!("/proc/{}", p)).exists());
        claims.insert(pid, (Instant::now(), command.to_string()));
    }

    /// The command `pid` claimed, if it did and the claim is still fresh.
    pub async fn agent_claim(&self, pid: u32) -> Option<String> {
        let claims = self.agent_claims.read().await;
        claims
            .get(&pid)
            .filter(|(at, _)| at.elapsed() < Duration::from_secs(AGENT_CLAIM_TTL_SECS))
            .map(|(_, cmd)| cmd.clone())
    }

    /// Push one line to the session agent.
    ///
    /// `false` means nobody is subscribed. Callers must treat that as "no UI
    /// available" and carry on — never as a failure, and never as consent.
    pub async fn push_ui(&self, line: impl Into<String>) -> bool {
        match self.ui_out.read().await.as_ref() {
            Some(tx) => tx.send(line.into()).is_ok(),
            None => false,
        }
    }

    pub fn settings_path(&self) -> std::path::PathBuf {
        self.state_dir
            .join(immurok_common::protocol::SETTINGS_FILE)
    }

    /// Cancel the fingerprint gate the BLE worker is parked in, if any.
    /// Returns true when the notification was actually delivered.
    ///
    /// The `gate_active` check is the whole point: `Notify::notify_one()`
    /// with no waiter stores a permit, and that permit would cancel the
    /// NEXT gate — a "FP-gate cancelled" out of nowhere. Callers that also
    /// have a queued-BleCommand fallback should use it only when this
    /// returns false.
    pub fn cancel_active_gate(&self) -> bool {
        if self.gate_active.load(Ordering::SeqCst) {
            self.gate_cancel.notify_one();
            true
        } else {
            false
        }
    }

    // Kept for symmetry with settings_path / external tooling; unused internally.
    #[allow(dead_code)]
    pub fn pairing_path(&self) -> std::path::PathBuf {
        self.state_dir
            .join(immurok_common::protocol::PAIRING_FILE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_coordinator() -> Arc<Coordinator> {
        let (tx, _rx) = mpsc::channel(1);
        Coordinator::new(tx, std::path::PathBuf::from("/tmp/immurok-test"))
    }

    #[tokio::test]
    async fn try_begin_pairing_claims_when_idle() {
        let coord = new_coordinator();
        assert_eq!(coord.pair_progress().await, PairProgress::Idle);
        assert!(coord.try_begin_pairing().await);
        assert_eq!(coord.pair_progress().await, PairProgress::Idle);
    }

    #[tokio::test]
    async fn try_begin_pairing_claims_from_terminal_states() {
        let coord = new_coordinator();
        for terminal in [PairProgress::Done, PairProgress::Failed] {
            coord.set_pair_progress(terminal).await;
            assert!(coord.try_begin_pairing().await);
            // Claiming resets progress back to Idle.
            assert_eq!(coord.pair_progress().await, PairProgress::Idle);
        }
    }

    #[tokio::test]
    async fn cancel_active_gate_stores_no_permit_when_idle() {
        let coord = new_coordinator();
        // No gate running: nothing is signalled, and crucially no permit is
        // left behind for the next gate to trip over.
        assert!(!coord.cancel_active_gate());
        let leaked = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            coord.gate_cancel.notified(),
        )
        .await;
        assert!(leaked.is_err(), "an idle GATE:CANCEL must not arm the next gate");

        // Gate running: the waiting BLE worker is woken.
        coord.gate_active.store(true, Ordering::SeqCst);
        assert!(coord.cancel_active_gate());
        let woken = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            coord.gate_cancel.notified(),
        )
        .await;
        assert!(woken.is_ok(), "an active gate must receive the cancel");
    }

    #[tokio::test]
    async fn try_begin_pairing_rejects_while_in_flight() {
        let coord = new_coordinator();
        for in_flight in [PairProgress::WaitFp, PairProgress::WaitButton, PairProgress::Ecdh] {
            coord.set_pair_progress(in_flight).await;
            assert!(!coord.try_begin_pairing().await);
            // Rejected claim must not disturb the in-flight state.
            assert_eq!(coord.pair_progress().await, in_flight);
        }
    }
}
