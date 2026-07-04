//! BLE GATT client — scan -> connect -> serve commands -> reconnect on disconnect.
//!
//! Ported from app-linux/immurok/ble.py.
//! Uses `bluer` for device discovery, `zbus` ObjectManager for one-time GATT path
//! discovery, and a Python dbus-fast helper subprocess (`ble-notify-helper.py`) for
//! ALL runtime GATT I/O (CMD write, OTA read/write, RSP notifications).
//!
//! This avoids the notification delivery bug caused by zbus — it cannot reliably
//! receive asynchronous BLE PropertiesChanged signals (like FP match notifications
//! that arrive seconds after a command). The Python dbus-fast helper works correctly.
//!
//! Key design: the helper subprocess owns a single D-Bus connection. A spawned reader
//! task reads its stdout and routes lines to either `notify_tx` (NOTIFY: lines) or
//! `response_rx` (WRITE_OK, READ_OK, etc.). Command functions drain notifications
//! via `notify_rx` while awaiting their response.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};
use zbus::zvariant::OwnedValue;

use immurok_common::protocol::*;
use immurok_common::security;
use immurok_common::types::{EnrollEvent, PairingData};

use crate::coordinator::{BleCommand, BleResult, Coordinator};
use crate::keystore;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

// ── Main run loop ───────────────────────────────────────────────────────────

pub async fn run(coordinator: Arc<Coordinator>, mut cmd_rx: mpsc::Receiver<BleCommand>) {
    let mut backoff_secs = BLE_RECONNECT_INTERVAL_SECS;
    loop {
        let session_start = tokio::time::Instant::now();
        match connect_and_serve(&coordinator, &mut cmd_rx).await {
            Ok(()) => info!("BLE session ended normally"),
            Err(e) => warn!("BLE session error: {}", e),
        }
        coordinator.is_connected.store(false, Ordering::Relaxed);
        coordinator.is_device_verified.store(false, Ordering::Relaxed);
        *coordinator.device_status.write().await = None;

        // Backoff before retrying. A GATT discovery failure (transient BlueZ
        // state where Connected=true but the GATT tree hasn't repopulated yet)
        // makes connect_and_serve fail almost instantly, which without backoff
        // would re-spawn the helper hundreds of times per second, flood the log
        // file, and keep BlueZ saturated long enough that it never recovers.
        //
        // A session that survived BLE_SESSION_STABLE_SECS was real work (normal
        // connect → later disconnect) → reset to the fast base so a normal drop
        // reconnects promptly. A session that died almost immediately is the
        // degenerate state above → grow the backoff 1→2→4…→ceiling so the
        // stuck case idles quietly instead of spinning.
        if session_start.elapsed() >= Duration::from_secs(BLE_SESSION_STABLE_SECS) {
            backoff_secs = BLE_RECONNECT_INTERVAL_SECS;
        } else {
            warn!("BLE session failed fast (<{}s) — backoff {}s", BLE_SESSION_STABLE_SECS, backoff_secs);
        }
        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
        // Arm the next level AFTER sleeping, so the first fast failure still
        // only waits the base interval; a stable session already reset it.
        backoff_secs = (backoff_secs * 2).min(BLE_RECONNECT_BACKOFF_MAX_SECS);

        info!("Waiting for device reconnection...");
        wait_for_device_connected(&coordinator).await;
    }
}

// ── Per-connection mutable state ────────────────────────────────────────────

struct BleState {
    /// Oneshot for the next command response (non-special notifications resolve this).
    pending_response: Option<oneshot::Sender<Vec<u8>>>,
    /// FP-gate wait state.
    gate_pending: bool,
    gate_tx: Option<oneshot::Sender<GateResult>>,
    /// AUTH_REQUEST wait state.
    auth_pending: bool,
    auth_tx: Option<oneshot::Sender<bool>>,
    /// Failure counter (shared across gate/auth).
    auth_failures: u8,
    /// Pairing FP-gate mode: ACK 0x21 without HMAC verification.
    pair_fp_gate: bool,
    /// Pairing button-wait state: route [0x34, status] notifications.
    pair_button_pending: bool,
    /// Sent on PAIR_BUTTON_TIMEOUT (0x00) / PAIR_BUTTON_CANCELLED (0x02).
    /// PAIR_BUTTON_CONFIRMED (0x01) is informational only — the actual
    /// PAIR_INIT completion still arrives via pending_response as
    /// [0x30][pubkey:33B] once the device finishes ECDH.
    pair_button_tx: Option<oneshot::Sender<u8>>,
}

impl BleState {
    fn new() -> Self {
        Self {
            pending_response: None,
            gate_pending: false,
            gate_tx: None,
            auth_pending: false,
            auth_tx: None,
            auth_failures: 0,
            pair_fp_gate: false,
            pair_button_pending: false,
            pair_button_tx: None,
        }
    }
}

type GateResult = (bool, Option<u8>);

// ── Helper subprocess I/O ───────────────────────────────────────────────────

/// Communication handle for the Python dbus-fast helper subprocess.
/// All runtime GATT I/O goes through this.
struct HelperIO {
    stdin: tokio::process::ChildStdin,
    /// Channel for command responses (WRITE_OK, WRITE_ERR, READ_OK, READ_ERR).
    response_rx: mpsc::Receiver<String>,
}

impl HelperIO {
    /// Write a byte slice to the CMD characteristic via the helper.
    async fn cmd_write(&mut self, data: &[u8]) -> Result<(), String> {
        let line = format!("CMD_WRITE:{}\n", hex::encode(data));
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("helper stdin write failed: {}", e))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("helper stdin flush failed: {}", e))?;
        self.wait_write_ok().await
    }

    /// Write a byte slice to the OTA characteristic via the helper.
    async fn ota_write(&mut self, data: &[u8]) -> Result<(), String> {
        let line = format!("OTA_WRITE:{}\n", hex::encode(data));
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("helper stdin write failed: {}", e))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("helper stdin flush failed: {}", e))?;
        self.wait_write_ok().await
    }

    /// Read the OTA characteristic via the helper.
    async fn ota_read(&mut self) -> Result<Vec<u8>, String> {
        self.stdin
            .write_all(b"OTA_READ\n")
            .await
            .map_err(|e| format!("helper stdin write failed: {}", e))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("helper stdin flush failed: {}", e))?;
        self.wait_read_response().await
    }

    /// Wait for WRITE_OK or WRITE_ERR from the response channel (with timeout).
    async fn wait_write_ok(&mut self) -> Result<(), String> {
        match tokio::time::timeout(Duration::from_secs(5), self.response_rx.recv()).await {
            Ok(Some(line)) => {
                if line == "WRITE_OK" {
                    Ok(())
                } else if let Some(err) = line.strip_prefix("WRITE_ERR:") {
                    Err(format!("write failed: {}", err))
                } else {
                    Err(format!("unexpected response: {}", line))
                }
            }
            Ok(None) => Err("helper response channel closed".to_string()),
            Err(_) => Err("write response timeout".to_string()),
        }
    }

    /// Wait for READ_OK or READ_ERR from the response channel (with timeout).
    async fn wait_read_response(&mut self) -> Result<Vec<u8>, String> {
        match tokio::time::timeout(Duration::from_secs(5), self.response_rx.recv()).await {
            Ok(Some(line)) => {
                if let Some(hex_data) = line.strip_prefix("READ_OK:") {
                    hex::decode(hex_data).map_err(|e| format!("hex decode error: {}", e))
                } else if let Some(err) = line.strip_prefix("READ_ERR:") {
                    Err(format!("read failed: {}", err))
                } else {
                    Err(format!("unexpected response: {}", line))
                }
            }
            Ok(None) => Err("helper response channel closed".to_string()),
            Err(_) => Err("read response timeout".to_string()),
        }
    }

    /// Send QUIT to the helper.
    async fn quit(&mut self) {
        let _ = self.stdin.write_all(b"QUIT\n").await;
        let _ = self.stdin.flush().await;
    }
}

// ── Device discovery & connection ────────────────────────────────────────────

/// Wait for an immurok device to become connected.
///
/// Three-way race:
/// 1. BlueZ PropertiesChanged event for Connected/ServicesResolved (fastest
///    when BlueZ itself decides to rebind — e.g. user-initiated reconnect)
/// 2. Polling fallback every 2s in case events get dropped (D-Bus stream
///    can stall after suspend, after polkit policy reload, etc.)
/// 3. Resume kick from logind PrepareForSleep monitor → active
///    Device.Connect(). BlueZ does NOT auto-reconnect BLE LE devices the
///    way it does classic BR/EDR HID, so after suspend the only way the
///    device comes back without manual action is for SOMEONE to call
///    Device.Connect(). Without (3) the resume reconnect can take 20–60s
///    while waiting for the device's advertise to be scanned.
async fn wait_for_device_connected(coordinator: &Arc<Coordinator>) {
    // Quick check: device might already be connected with services resolved
    if check_immurok_connected().await {
        return;
    }

    info!("Waiting for device to connect as HID keyboard...");
    let event_wait = async {
        let Ok(dbus) = zbus::Connection::system().await else { return };
        // Match PropertiesChanged on org.bluez.Device1 interface
        let rule = "type='signal',sender='org.bluez',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',arg0='org.bluez.Device1'";
        if dbus.call_method(
            Some("org.freedesktop.DBus"), "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"), "AddMatch", &rule,
        ).await.is_err() {
            return;
        }

        let mut stream = zbus::MessageStream::from(&dbus);
        use futures_lite::StreamExt;
        while let Some(Ok(msg)) = stream.next().await {
            let Ok((_, changed, _)): Result<(String, HashMap<String, OwnedValue>, Vec<String>), _> =
                msg.body().deserialize() else { continue };
            // Trigger on Connected or ServicesResolved changes
            let dominated = changed.contains_key("Connected") || changed.contains_key("ServicesResolved");
            if dominated && check_immurok_connected().await {
                info!("Device connected as HID keyboard (via BlueZ event)");
                return;
            }
        }
    };

    // Active-connect fallback. Previously this only *polled* Connected state,
    // which recovers only if SOMETHING else reconnects the device. But BlueZ
    // does not reliably auto-reconnect BLE LE devices, and the resume kick
    // below only covers suspend/resume — so a plain out-of-range → back-in-
    // range drop (no suspend) had no active reconnection path and the daemon
    // could wait forever. Here we periodically fire an active Device.Connect()
    // ourselves. check_immurok_connected() first is the cheap fast path when
    // the link is already up (e.g. BlueZ HID auto-reconnect did fire).
    let poll_wait = async {
        loop {
            if check_immurok_connected().await {
                return;
            }
            if try_active_connect().await {
                info!("Device connected (active-connect fallback)");
                return;
            }
            tokio::time::sleep(Duration::from_secs(BLE_ACTIVE_RECONNECT_INTERVAL_SECS)).await;
        }
    };

    // Resume kick: logind told us the system just resumed → fire an active
    // connect. Loop because a single resume may need multiple attempts if
    // the device hasn't started advertising yet (we just unsuspended too).
    let resume_wait = async {
        loop {
            coordinator.resume_notify.notified().await;
            info!("Resume hook — attempting active Device.Connect()");
            if try_active_connect().await {
                info!("Active connect succeeded after resume");
                return;
            }
            warn!("Active connect failed after resume — will retry on next event/poll/resume");
        }
    };

    tokio::select! {
        _ = event_wait => {},
        _ = poll_wait => {},
        _ = resume_wait => {},
    }
}

/// Find the paired immurok device and call Device.Connect() on it. Returns
/// true once Connected + ServicesResolved settle (so the outer wait can
/// proceed straight into connect_and_serve), false on any failure.
///
/// Wraps the bluer call in a 12s timeout — BlueZ will internally try for
/// roughly that long before giving up if the peer never responds, and we
/// don't want to wedge the wait loop behind a stuck D-Bus call.
async fn try_active_connect() -> bool {
    let Ok(session) = bluer::Session::new().await else { return false };
    let Ok(adapter) = session.default_adapter().await else { return false };
    let Ok(addrs) = adapter.device_addresses().await else { return false };

    for addr in addrs {
        let Ok(device) = adapter.device(addr) else { continue };
        if !device.is_paired().await.unwrap_or(false) { continue; }
        let name_match = device.name().await.ok().flatten()
            .map(|n| n.to_lowercase().starts_with(DEVICE_NAME_PREFIX))
            .unwrap_or(false);
        if !name_match { continue; }

        if device.is_connected().await.unwrap_or(false)
            && device.is_services_resolved().await.unwrap_or(false)
        {
            return true;
        }

        info!("Active connect → {} ({})",
              device.name().await.ok().flatten().unwrap_or_default(), addr);
        let connect_result = tokio::time::timeout(
            Duration::from_secs(12),
            device.connect(),
        ).await;
        match connect_result {
            Ok(Ok(())) => {
                // wait up to 5s for services to resolve
                for _ in 0..50 {
                    if device.is_services_resolved().await.unwrap_or(false) {
                        return true;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                return device.is_connected().await.unwrap_or(false);
            }
            Ok(Err(e)) => {
                warn!("Device.Connect failed: {}", e);
                return false;
            }
            Err(_) => {
                warn!("Device.Connect timed out (12s) — device likely not advertising yet");
                return false;
            }
        }
    }
    false
}

/// Check if any immurok device is connected with services resolved (HID keyboard ready).
async fn check_immurok_connected() -> bool {
    let Ok(session) = bluer::Session::new().await else { return false };
    let Ok(adapter) = session.default_adapter().await else { return false };
    let Ok(addrs) = adapter.device_addresses().await else { return false };
    for addr in addrs {
        let Ok(device) = adapter.device(addr) else { continue };
        if !device.is_connected().await.unwrap_or(false) { continue; }
        if !device.is_services_resolved().await.unwrap_or(false) { continue; }
        if let Ok(Some(name)) = device.name().await {
            if name.to_lowercase().starts_with(DEVICE_NAME_PREFIX) {
                return true;
            }
        }
    }
    false
}

/// Find a connected immurok device and return its D-Bus device path.
/// Also waits for GATT services to be resolved (instead of fixed sleep).
async fn find_immurok_device_path() -> Result<Option<String>, BoxError> {
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    let adapter_name = adapter.name().to_string();

    for addr in adapter.device_addresses().await? {
        let device = adapter.device(addr)?;
        if !device.is_connected().await.unwrap_or(false) { continue; }
        let name_match = device.name().await.ok().flatten()
            .map(|n| n.to_lowercase().starts_with(DEVICE_NAME_PREFIX))
            .unwrap_or(false);
        if !name_match { continue; }

        // Wait for GATT services to be resolved (up to 5s)
        for _ in 0..50 {
            if device.is_services_resolved().await.unwrap_or(false) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let dev_addr = addr.to_string().replace(':', "_");
        let device_path = format!("/org/bluez/{}/dev_{}", adapter_name, dev_addr);
        info!("Found immurok device: {} ({}) path={} services_resolved={}",
              device.name().await.ok().flatten().unwrap_or_default(),
              addr, device_path,
              device.is_services_resolved().await.unwrap_or(false));
        return Ok(Some(device_path));
    }
    Ok(None)
}

// ── GATT discovery via zbus ObjectManager ────────────────────────────────────

struct GattPaths {
    cmd_path: String,
    rsp_path: String,
    ota_path: Option<String>,
    /// All characteristic paths with their flags (for StartNotify on HID etc.)
    all_chars: Vec<(String, Option<Vec<String>>)>,
}

/// Discover GATT characteristic D-Bus paths using org.freedesktop.DBus.ObjectManager.
async fn discover_gatt_paths(
    dbus: &zbus::Connection,
    device_path: &str,
) -> Result<GattPaths, BoxError> {
    let manager_proxy = zbus::Proxy::new(
        dbus,
        "org.bluez",
        "/",
        "org.freedesktop.DBus.ObjectManager",
    ).await?;

    let reply = manager_proxy.call_method("GetManagedObjects", &()).await?;
    let objects: HashMap<
        zbus::zvariant::OwnedObjectPath,
        HashMap<String, HashMap<String, OwnedValue>>,
    > = reply.body().deserialize()?;

    let mut cmd_path = None;
    let mut rsp_path = None;
    let mut ota_path = None;
    let mut all_chars = Vec::new();

    for (path, interfaces) in &objects {
        let path_str = path.as_str();
        if !path_str.starts_with(device_path) {
            continue;
        }
        if let Some(char_props) = interfaces.get("org.bluez.GattCharacteristic1") {
            // Collect flags
            let flags: Option<Vec<String>> = char_props.get("Flags").and_then(|f| {
                <Vec<String>>::try_from(f.clone()).ok()
            });
            all_chars.push((path_str.to_string(), flags));

            if let Some(uuid_val) = char_props.get("UUID") {
                let uuid: String = match String::try_from(uuid_val.clone()) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let uuid_lower = uuid.to_lowercase();
                if uuid_lower == CMD_CHAR_UUID_STR {
                    cmd_path = Some(path_str.to_string());
                } else if uuid_lower == RSP_CHAR_UUID_STR {
                    rsp_path = Some(path_str.to_string());
                } else if uuid_lower == OTA_CHAR_UUID_STR {
                    ota_path = Some(path_str.to_string());
                }
            }
        }
    }

    let cmd = cmd_path.ok_or("CMD characteristic not found")?;
    let rsp = rsp_path.ok_or("RSP characteristic not found")?;
    info!(
        "GATT discovered: CMD={} RSP={}{}",
        cmd,
        rsp,
        if ota_path.is_some() { " + OTA" } else { "" }
    );
    Ok(GattPaths {
        cmd_path: cmd,
        rsp_path: rsp,
        ota_path,
        all_chars,
    })
}

// ── connect_and_serve ───────────────────────────────────────────────────────

async fn connect_and_serve(
    coordinator: &Arc<Coordinator>,
    cmd_rx: &mut mpsc::Receiver<BleCommand>,
) -> Result<(), BoxError> {
    let device_path = match find_immurok_device_path().await? {
        Some(p) => p,
        None => return Err("no device".into()),
    };

    // One-time D-Bus connection for GATT path discovery via ObjectManager
    let dbus_discovery = zbus::Connection::system().await?;

    // Discover GATT characteristic paths via ObjectManager
    let gatt_paths = discover_gatt_paths(&dbus_discovery, &device_path).await?;

    // Drop the discovery connection — all runtime I/O goes through the helper
    drop(dbus_discovery);

    // Collect extra notify paths (HID etc.)
    let extra_notify: Vec<String> = gatt_paths.all_chars.iter()
        .filter(|(p, f)| {
            p != &gatt_paths.rsp_path
                && f.as_ref().is_some_and(|flags| flags.iter().any(|f| f == "notify"))
        })
        .map(|(p, _)| p.clone())
        .collect();

    // Find helper script path
    let helper_path = find_helper_script();
    info!("BLE helper: {}", helper_path);

    // Spawn the Python dbus-fast helper subprocess
    let mut cmd = tokio::process::Command::new("python3");
    cmd.arg(&helper_path)
        .arg(&device_path)
        .arg(&gatt_paths.cmd_path)
        .arg(&gatt_paths.rsp_path);
    // Always pass OTA path as argv[4] (empty string if not available)
    // so helper can use positional args instead of heuristics.
    cmd.arg(gatt_paths.ota_path.as_deref().unwrap_or(""));
    for p in &extra_notify {
        cmd.arg(p);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit());

    let mut child = cmd.spawn()
        .map_err(|e| format!("Failed to spawn BLE helper: {}", e))?;

    let child_stdin = child.stdin.take()
        .ok_or("Failed to get helper stdin")?;
    let child_stdout = child.stdout.take()
        .ok_or("Failed to get helper stdout")?;

    // Channels for routing helper stdout lines
    let (notify_tx, notify_rx) = mpsc::channel::<Vec<u8>>(64);
    let (response_tx, response_rx) = mpsc::channel::<String>(16);
    let (disconnect_tx, mut disconnect_rx) = mpsc::channel::<()>(1);

    // Spawn reader task that reads ALL lines from helper stdout and routes them
    {
        let notify_tx = notify_tx;
        let response_tx = response_tx;
        let disconnect_tx = disconnect_tx;

        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let reader = BufReader::new(child_stdout);
            let mut lines = reader.lines();

            // Wait for READY
            if let Ok(Some(line)) = lines.next_line().await {
                if line.trim() == "READY" {
                    info!("BLE helper ready (dbus-fast)");
                }
            }

            // Route lines
            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() { continue; }

                if let Some(hex_data) = trimmed.strip_prefix("NOTIFY:") {
                    // Async RSP notification → notify channel
                    if let Ok(data) = hex::decode(hex_data) {
                        if !data.is_empty() && notify_tx.send(data).await.is_err() {
                            break;
                        }
                    }
                } else if trimmed == "DISCONNECT" {
                    // Device disconnected
                    info!("BLE helper: device disconnected");
                    let _ = disconnect_tx.send(()).await;
                    break;
                } else {
                    // Command response (WRITE_OK, WRITE_ERR, READ_OK, READ_ERR)
                    if response_tx.send(trimmed.to_string()).await.is_err() { break; }
                }
            }

            info!("BLE helper reader exited");
            let _ = child.wait().await;
        });
    }

    let mut helper = HelperIO {
        stdin: child_stdin,
        response_rx,
    };

    coordinator.is_connected.store(true, Ordering::Relaxed);
    coordinator.is_device_verified.store(false, Ordering::Relaxed);

    let mut state = BleState::new();
    let mut notify_rx = notify_rx;

    info!("BLE session active");

    // GET_STATUS on connect
    if let Ok(rsp) = send_command_inner(&mut helper, &mut state, coordinator, &mut notify_rx, CMD_GET_STATUS, &[], BLE_COMMAND_TIMEOUT_SECS).await {
        if rsp.len() >= 4 && rsp[0] == RSP_OK {
            let bitmap = rsp[1];
            let paired = rsp[2] != 0;
            let battery = if rsp.len() >= 4 { rsp[3] } else { 0 };
            let fw = if rsp.len() >= 8 {
                let build = ((rsp[7] as u16) << 8) | rsp.get(8).copied().unwrap_or(0) as u16;
                format!("{}.{}.{}.{:x}", rsp[4], rsp[5], rsp[6], build)
            } else if rsp.len() >= 7 {
                format!("{}.{}.{}", rsp[4], rsp[5], rsp[6])
            } else {
                String::new()
            };
            let mut ds = coordinator.device_status.write().await;
            *ds = Some(immurok_common::types::DeviceStatus {
                fp_bitmap: bitmap,
                paired,
                battery,
                fw_version: fw,
                pending_match: None,
            });
            info!("Device status: bitmap=0x{:02x} paired={} battery={}", bitmap, paired, battery);
        }
    }

    // Challenge-Response verification
    {
        let pairing = coordinator.pairing.read().await;
        if let Some(ref p) = *pairing {
            let shared_key = p.shared_key;
            drop(pairing);

            // Generate 8-byte random nonce
            let mut nonce = [0u8; 8];
            use rand::RngCore;
            rand::thread_rng().fill_bytes(&mut nonce);

            info!("Challenge-Response: sending nonce");
            match send_command_inner(&mut helper, &mut state, coordinator, &mut notify_rx, CMD_CHALLENGE, &nonce, BLE_COMMAND_TIMEOUT_SECS).await {
                Ok(rsp) if rsp.len() >= 9 && rsp[0] == CMD_CHALLENGE => {
                    let mut device_hmac = [0u8; 8];
                    device_hmac.copy_from_slice(&rsp[1..9]);
                    if security::verify_challenge_response(&shared_key, &nonce, &device_hmac) {
                        info!("Challenge-Response: verified");
                        coordinator.is_device_verified.store(true, Ordering::Relaxed);
                    } else {
                        warn!("Challenge-Response: HMAC mismatch — degraded mode");
                    }
                }
                Ok(rsp) => {
                    warn!("Challenge-Response: unexpected response: [{}]", hex::encode(&rsp));
                    // Still mark as verified for backwards compatibility with old firmware
                    coordinator.is_device_verified.store(true, Ordering::Relaxed);
                }
                Err(e) => {
                    warn!("Challenge-Response failed: {} — assuming verified", e);
                    coordinator.is_device_verified.store(true, Ordering::Relaxed);
                }
            }
        } else {
            drop(pairing);
            info!("No pairing data — skipping challenge");
            // Without pairing, mark verified to allow pairing operation
            coordinator.is_device_verified.store(true, Ordering::Relaxed);
        }
    }

    // Sync key cache after verification
    if coordinator.is_device_verified.load(Ordering::Relaxed) {
        if let Err(e) = sync_ssh_keys(&mut helper, &mut state, coordinator, &mut notify_rx).await {
            warn!("Key sync failed: {}", e);
        }
    }

    // Main serve loop
    loop {
        tokio::select! {
            Some(data) = notify_rx.recv() => {
                route_notification(&data, &mut state, coordinator, &mut helper).await;
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(ble_cmd) => {
                        handle_ble_command(ble_cmd, &mut helper, &mut state, coordinator, &mut notify_rx).await;
                    }
                    None => {
                        helper.quit().await;
                        return Ok(());
                    }
                }
            }
            _ = disconnect_rx.recv() => {
                info!("Device disconnected");
                abort_pending(&mut state);
                helper.quit().await;
                return Err("device disconnected".into());
            }
        }
    }
}

fn abort_pending(state: &mut BleState) {
    if let Some(tx) = state.gate_tx.take() { let _ = tx.send((false, Some(RSP_ERROR))); }
    state.gate_pending = false;
    if let Some(tx) = state.auth_tx.take() { let _ = tx.send(false); }
    state.auth_pending = false;
    if let Some(tx) = state.pair_button_tx.take() { let _ = tx.send(PAIR_BUTTON_CANCELLED); }
    state.pair_button_pending = false;
    state.pending_response.take();
}

// ── Notification routing ────────────────────────────────────────────────────

/// Route a single notification. Called both from the main loop and from
/// inner loops inside send_command / send_fp_gated / do_pair.
async fn route_notification(
    data: &[u8],
    state: &mut BleState,
    coordinator: &Arc<Coordinator>,
    helper: &mut HelperIO,
) {
    let len = data.len();
    if len == 0 { return; }
    let first = data[0];

    info!("BLE RX: [{}] gate={} auth={} pending_rsp={}",
          hex::encode(data), state.gate_pending, state.auth_pending,
          state.pending_response.is_some());

    // 1. Signed FP match: [0x21][page_id:2B LE][hmac:8B] = 11 bytes
    if first == NOTIFY_FP_MATCH_SIGNED && len == 11 {
        handle_fp_match_signed(data, state, coordinator, helper).await;
        return;
    }

    // 1b. Long-press lock request: [0x23] = 1 byte. Independent of any
    //     preceding 0x21 — coordinator decides whether to act based on
    //     settings + screen state + recent auth flow window.
    if first == NOTIFY_LOCK_REQUEST && len == 1 {
        info!("Lock request (long-press) received");
        coordinator.handle_lock_request().await;
        return;
    }

    // 1c. Enroll step-1 keep-alive: [0x12, 0x01, fp_powered, capture] = 4 bytes
    //     Firmware 1.2.31 emits this every ~3s during the lift-finger polling
    //     loop to keep the BLE link from hitting supervision timeout when the
    //     user pauses between captures (commit 25a2f19). Swallow silently —
    //     the legitimate enroll status is on opcode 0x11.
    if first == NOTIFY_ENROLL_KEEPALIVE && len == 4 {
        debug!("Enroll keep-alive: fp_powered={} capture={}", data[2], data[3]);
        return;
    }

    // 2. Enroll progress: [0x11][status][current][total] = 4 bytes
    if first == NOTIFY_ENROLL_PROGRESS && len == 4 {
        let ev = EnrollEvent::from_notification(data[1], data[2], data[3]);
        info!("Enroll: status={} {}/{}", data[1], data[2], data[3]);
        if matches!(ev, EnrollEvent::Complete) {
            coordinator.fp_bitmap_stale.store(true, Ordering::Relaxed);
        }
        *coordinator.last_enroll_event.write().await = Some((data[1], data[2], data[3]));
        let _ = coordinator.enroll_tx.send(ev);
        return;
    }

    // 3. Conn param update: [0xF0][interval:2B BE][latency][timeout:2B BE] = 6 bytes
    if first == NOTIFY_CONN_PARAM_UPDATE && len == 6 {
        let interval = ((data[1] as u16) << 8) | data[2] as u16;
        let latency = data[3];
        let tout = ((data[4] as u16) << 8) | data[5] as u16;
        info!("Conn params: interval={} ({:.1}ms) latency={} timeout={} ({}ms)",
              interval, interval as f64 * 1.25, latency, tout, tout as u32 * 10);
        return;
    }

    // 3b. Pair button event: [0x34, status]
    //   0x01 CONFIRMED: device about to run ECDH; the actual [0x30][pubkey]
    //                   arrives later via pending_response (do nothing here).
    //   0x00 TIMEOUT / 0x02 CANCELLED: terminal — wake wait_pair_button.
    if first == CMD_PAIR_BUTTON && len == 2 {
        let status = data[1];
        info!("Pair button event: 0x{:02x}", status);
        if status == PAIR_BUTTON_CONFIRMED {
            return;
        }
        if status == PAIR_BUTTON_TIMEOUT || status == PAIR_BUTTON_CANCELLED {
            if state.pair_button_pending {
                if let Some(tx) = state.pair_button_tx.take() {
                    let _ = tx.send(status);
                }
            }
            return;
        }
        // Unknown status: fall through to generic handling.
    }

    // 4. FP-gate approved (0x10) — swallow
    if first == RSP_FP_GATE_APPROVED && (state.gate_pending || state.pair_fp_gate) {
        debug!("FP-gate approved, waiting for result");
        if state.gate_pending {
            // Touch matched; signing (~2s) starts now. Let the SSH agent flip
            // the terminal hint to "verified — signing…" at this instant
            // rather than when the signature finally returns.
            coordinator.emit_fp_gate_event(crate::coordinator::FpGateEvent::Approved);
        }
        return;
    }

    // 5. FP not match (0x07) — gate/auth failure counting
    if first == RSP_ERR_FP_NOT_MATCH {
        if state.gate_pending {
            state.auth_failures += 1;
            let rem = FP_GATE_MAX_FAILURES.saturating_sub(state.auth_failures);
            warn!("FP-gate: mismatch ({} left)", rem);
            if rem == 0 {
                if let Some(tx) = state.gate_tx.take() { let _ = tx.send((false, Some(RSP_ERR_FP_NOT_MATCH))); }
                state.gate_pending = false;
            } else {
                // Still retryable — let the SSH agent echo the count on the
                // client terminal (the final rem==0 deny rides the result).
                coordinator.emit_fp_gate_event(
                    crate::coordinator::FpGateEvent::Mismatch { remaining: rem },
                );
            }
            return;
        }
        if state.auth_pending {
            state.auth_failures += 1;
            let rem = FP_GATE_MAX_FAILURES.saturating_sub(state.auth_failures);
            warn!("AUTH: mismatch ({} left)", rem);
            if rem == 0 {
                if let Some(tx) = state.auth_tx.take() { let _ = tx.send(false); }
                state.auth_pending = false;
            }
            return;
        }
        // fall through to command response
    }

    // 6. OK (0x00) — gate/auth success (any length starting with 0x00)
    if first == RSP_OK && (state.gate_pending || state.auth_pending) {
        debug!("Gate/auth OK received (len={})", len);
        if state.gate_pending {
            if let Some(tx) = state.gate_tx.take() { let _ = tx.send((true, None)); }
            state.gate_pending = false;
            return;
        }
        if state.auth_pending {
            if let Some(tx) = state.auth_tx.take() { let _ = tx.send(true); }
            state.auth_pending = false;
            return;
        }
    }

    // 7. Error while gate pending
    if state.gate_pending && matches!(first, RSP_ERR_TIMEOUT | RSP_INVALID_PARAM | RSP_ERROR) {
        warn!("FP-gate error: 0x{:02x}", first);
        if let Some(tx) = state.gate_tx.take() { let _ = tx.send((false, Some(first))); }
        state.gate_pending = false;
        return;
    }

    // 7b. Error while auth pending (device sends 0x06 after max FP retries)
    if state.auth_pending && matches!(first, RSP_ERR_TIMEOUT | RSP_INVALID_PARAM | RSP_ERROR) {
        warn!("AUTH error: 0x{:02x}", first);
        if let Some(tx) = state.auth_tx.take() { let _ = tx.send(false); }
        state.auth_pending = false;
        return;
    }

    // 8. Command response
    debug!("Cmd response: 0x{:02x} len={} data={}", first, len, hex::encode(data));
    if let Some(tx) = state.pending_response.take() {
        let _ = tx.send(data.to_vec());
    }
}

// ── FP match handling ───────────────────────────────────────────────────────

async fn handle_fp_match_signed(
    data: &[u8],
    state: &mut BleState,
    coordinator: &Arc<Coordinator>,
    helper: &mut HelperIO,
) {
    let page_id = u16::from_le_bytes([data[1], data[2]]);
    let mut hmac_val = [0u8; 8];
    hmac_val.copy_from_slice(&data[3..11]);

    // Pairing FP-gate: skip HMAC, just ACK
    if state.pair_fp_gate {
        info!("Pair FP-gate: page_id={} (skip HMAC)", page_id);
        send_ack(helper).await;
        return;
    }

    let pairing = coordinator.pairing.read().await;
    let key = match pairing.as_ref() {
        Some(p) => p.shared_key,
        None => { warn!("FP match but no pairing data"); return; }
    };
    drop(pairing);

    if !security::verify_fp_match_signed(&key, page_id, &hmac_val) {
        warn!("FP match HMAC failed (page_id={})", page_id);
        return;
    }

    info!("Fingerprint match: page_id={}", page_id);
    send_ack(helper).await;

    // During FP-gate, the match is just for verification — don't route to coordinator
    if !state.gate_pending && !state.auth_pending {
        coordinator.on_fp_match(page_id).await;
    }
}

async fn send_ack(helper: &mut HelperIO) {
    if let Err(e) = helper.cmd_write(&[CMD_FP_MATCH_ACK, 0x00]).await {
        warn!("ACK failed: {}", e);
    }
}

// ── Key cache sync (called directly after verification) ─────────────────────

/// Sync SSH/OTP/API key cache from device using send_command_inner directly.
/// This runs inside connect_and_serve, so we cannot use coord.ble_send().
///
/// Per-category digest cache (mirrors macOS 1.2.7+, commit 542c8cb): firmware
/// returns (count, checksum) from KEY_COUNT; if matches saved digest the
/// per-entry KEY_READ chain is skipped. With 100+ entries this trims a 5–7s
/// reconnect to ~150ms (just three KEY_COUNT round-trips).
async fn sync_ssh_keys(
    helper: &mut HelperIO,
    state: &mut BleState,
    coordinator: &Arc<Coordinator>,
    notify_rx: &mut mpsc::Receiver<Vec<u8>>,
) -> Result<(), String> {
    info!("Starting key cache sync...");

    let digests_path = coordinator.immurok_dir.join(KEYSTORE_DIGESTS_FILE);
    let saved = keystore::load_digests(&digests_path);
    let mut fresh = saved.clone();

    // ── SSH ─────────────────────────────────────────────────────────────
    let (ssh_count, ssh_checksum) =
        get_key_count_inner(helper, state, coordinator, notify_rx, KEY_CAT_SSH).await?;
    if digest_hit(saved.get("ssh"), ssh_count, ssh_checksum) {
        info!(
            "SSH digest hit (count={} checksum=0x{:08x}) — skipping full read",
            ssh_count, ssh_checksum
        );
    } else {
        let entries =
            read_ssh_entries(helper, state, coordinator, notify_rx, KEY_CAT_SSH, ssh_count).await?;
        let ssh_path = coordinator.immurok_dir.join(SSH_KEYS_FILE);
        keystore::save_ssh_keys_to(&ssh_path, &entries)?;
        info!(
            "SSH digest miss → cached {} entries (count={} checksum=0x{:08x})",
            entries.len(),
            ssh_count,
            ssh_checksum
        );
        fresh.insert(
            "ssh".into(),
            keystore::DigestEntry { count: ssh_count, checksum: ssh_checksum },
        );
    }

    // ── OTP + API names share the same key_names.json — must coordinate ─
    let (otp_count, otp_checksum) =
        get_key_count_inner(helper, state, coordinator, notify_rx, KEY_CAT_OTP).await?;
    let (api_count, api_checksum) =
        get_key_count_inner(helper, state, coordinator, notify_rx, KEY_CAT_API).await?;
    let otp_hit = digest_hit(saved.get("otp"), otp_count, otp_checksum);
    let api_hit = digest_hit(saved.get("api"), api_count, api_checksum);

    if otp_hit && api_hit {
        info!("OTP+API digest hit — skipping full read");
    } else {
        // Mixed-hit: load disk entries for the hit side, BLE-read the miss side
        let cached = keystore::load_key_names(&coordinator.immurok_dir);
        let otp_entries = if otp_hit {
            cached.iter().filter(|e| e.category == "otp").cloned().collect()
        } else {
            read_name_entries(helper, state, coordinator, notify_rx, KEY_CAT_OTP, otp_count, "otp")
                .await?
        };
        let api_entries = if api_hit {
            cached.iter().filter(|e| e.category == "api").cloned().collect()
        } else {
            read_name_entries(helper, state, coordinator, notify_rx, KEY_CAT_API, api_count, "api")
                .await?
        };
        let mut combined = otp_entries;
        combined.extend(api_entries);
        let names_path = coordinator.immurok_dir.join(KEY_NAMES_FILE);
        keystore::save_key_names_to(&names_path, &combined)?;
        info!(
            "OTP+API digest miss(otp={}, api={}) → cached {} names",
            !otp_hit,
            !api_hit,
            combined.len()
        );
        fresh.insert(
            "otp".into(),
            keystore::DigestEntry { count: otp_count, checksum: otp_checksum },
        );
        fresh.insert(
            "api".into(),
            keystore::DigestEntry { count: api_count, checksum: api_checksum },
        );
    }

    // Persist updated digest map (also covers the hit-only case where saved
    // digest is unchanged — write is cheap and keeps the file canonical).
    if let Err(e) = keystore::save_digests(&digests_path, &fresh) {
        warn!("Failed to save digests: {}", e);
    }

    Ok(())
}

/// Digest hit predicate. checksum == 0 always misses (old firmware sentinel
/// or genuinely empty category — full re-read on either is cheap and safe).
fn digest_hit(saved: Option<&keystore::DigestEntry>, count: u8, checksum: u32) -> bool {
    if checksum == 0 {
        return false;
    }
    match saved {
        Some(s) => s.count == count && s.checksum == checksum,
        None => false,
    }
}

/// Get key count + content checksum for a category.
///
/// Firmware 1.2.7+ response: `[OK][count:1B][checksum:4B LE]` (6 bytes).
/// Older firmware: `[OK][count:1B]` (2 bytes) → checksum reported as 0,
/// which the digest cache treats as an automatic miss.
async fn get_key_count_inner(
    helper: &mut HelperIO,
    state: &mut BleState,
    coordinator: &Arc<Coordinator>,
    notify_rx: &mut mpsc::Receiver<Vec<u8>>,
    category: u8,
) -> Result<(u8, u32), String> {
    let rsp = send_command_inner(helper, state, coordinator, notify_rx, CMD_KEY_COUNT, &[category], BLE_COMMAND_TIMEOUT_SECS).await?;
    if rsp.len() < 2 || rsp[0] != RSP_OK {
        return Err(format!("KEY_COUNT failed: [{}]", hex::encode(&rsp)));
    }
    let count = rsp[1];
    let checksum = if rsp.len() >= 6 {
        u32::from_le_bytes([rsp[2], rsp[3], rsp[4], rsp[5]])
    } else {
        0
    };
    Ok((count, checksum))
}

/// Read a single key entry via send_command_inner. Returns raw data after status byte.
async fn read_key_entry_inner(
    helper: &mut HelperIO,
    state: &mut BleState,
    coordinator: &Arc<Coordinator>,
    notify_rx: &mut mpsc::Receiver<Vec<u8>>,
    category: u8,
    index: u8,
) -> Result<Vec<u8>, String> {
    // KEY_READ payload: [cat:1B][idx:1B][off:1B]
    // Response: [status:1B][total_lo:1B][off:1B][data:<=59B]
    // We need to read multiple chunks if the data is larger than 59 bytes.
    let mut full_data = Vec::new();
    let mut offset: u8 = 0;

    loop {
        let rsp = send_command_inner(helper, state, coordinator, notify_rx, CMD_KEY_READ, &[category, index, offset], BLE_COMMAND_TIMEOUT_SECS).await?;
        if rsp.len() < 3 || rsp[0] != RSP_OK {
            return Err(format!("KEY_READ failed: [{}]", hex::encode(&rsp)));
        }
        let total = rsp[1] as usize;
        let _chunk_off = rsp[2] as usize;
        let chunk = &rsp[3..];
        full_data.extend_from_slice(chunk);

        if full_data.len() >= total {
            full_data.truncate(total);
            break;
        }
        offset = full_data.len() as u8;
    }

    Ok(full_data)
}

/// Read SSH entries for indices 0..count via per-entry KEY_READ.
/// Caller fetches `count` from `get_key_count_inner` separately.
async fn read_ssh_entries(
    helper: &mut HelperIO,
    state: &mut BleState,
    coordinator: &Arc<Coordinator>,
    notify_rx: &mut mpsc::Receiver<Vec<u8>>,
    category: u8,
    count: u8,
) -> Result<Vec<keystore::SshKeyCacheEntry>, String> {
    if count == 0 {
        return Ok(vec![]);
    }

    let mut entries = Vec::new();
    for idx in 0..count {
        match read_key_entry_inner(helper, state, coordinator, notify_rx, category, idx).await {
            Ok(data) => {
                // SSH entry layout: name[16] + pubkey_LE[64] = 80 bytes minimum
                if data.len() < 80 {
                    debug!("Key slot {} too short ({}B), skipping", idx, data.len());
                    continue;
                }
                let name_bytes: Vec<u8> = data[0..16].iter().copied().take_while(|&b| b != 0).collect();
                let name = String::from_utf8_lossy(&name_bytes).to_string();
                let pubkey_le = &data[16..80];
                let pubkey_be = keystore::convert_endianness_64(pubkey_le);
                if let Some(blob) = keystore::build_ssh_public_key_blob(&pubkey_be) {
                    let fingerprint = keystore::compute_fingerprint(&blob);
                    entries.push(keystore::SshKeyCacheEntry {
                        index: idx,
                        name,
                        public_key_blob: blob,
                        fingerprint,
                    });
                }
            }
            Err(e) => warn!("Failed to read key {}: {}", idx, e),
        }
    }

    Ok(entries)
}

/// Read OTP/API name entries for indices 0..count via per-entry KEY_READ.
/// Caller fetches `count` from `get_key_count_inner` separately.
async fn read_name_entries(
    helper: &mut HelperIO,
    state: &mut BleState,
    coordinator: &Arc<Coordinator>,
    notify_rx: &mut mpsc::Receiver<Vec<u8>>,
    category: u8,
    count: u8,
    cat_str: &str,
) -> Result<Vec<keystore::KeyNameEntry>, String> {
    if count == 0 {
        return Ok(vec![]);
    }

    // Firmware entry name field width — used to be hardcoded to 16, but
    // OTP names are 30B and API names are 32B in the firmware structs.
    // The 16-byte read truncated long account names like
    // "user@example.com" or "claude-anthropic-prod-key".
    let name_len = match category {
        KEY_CAT_OTP => NAME_LEN_OTP,
        KEY_CAT_API => NAME_LEN_API,
        _ => NAME_LEN_SSH,
    };

    let mut entries = Vec::new();
    for idx in 0..count {
        match read_key_entry_inner(helper, state, coordinator, notify_rx, category, idx).await {
            Ok(data) => {
                if data.len() < name_len {
                    continue;
                }
                let name_bytes: Vec<u8> =
                    data[0..name_len].iter().copied().take_while(|&b| b != 0).collect();
                let name = String::from_utf8_lossy(&name_bytes).to_string();
                // OTP entries carry an issuer/service field right after the
                // name (otp_entry_t.service[30]); surface it in the cache.
                let service = if category == KEY_CAT_OTP
                    && data.len() >= name_len + SERVICE_LEN_OTP
                {
                    let svc_bytes: Vec<u8> = data[name_len..name_len + SERVICE_LEN_OTP]
                        .iter()
                        .copied()
                        .take_while(|&b| b != 0)
                        .collect();
                    String::from_utf8_lossy(&svc_bytes).to_string()
                } else {
                    String::new()
                };
                entries.push(keystore::KeyNameEntry {
                    index: idx,
                    category: cat_str.to_string(),
                    name,
                    service,
                });
            }
            Err(e) => warn!("Failed to read {} key name {}: {}", cat_str, idx, e),
        }
    }

    Ok(entries)
}

// ── Command sending (processes notifications while waiting) ─────────────────

/// Send [cmd:1B][len:1B][payload] and wait for response, processing
/// notifications in the meantime via the notify channel.
async fn send_command_inner(
    helper: &mut HelperIO,
    state: &mut BleState,
    coordinator: &Arc<Coordinator>,
    notify_rx: &mut mpsc::Receiver<Vec<u8>>,
    cmd: u8,
    payload: &[u8],
    timeout_secs: u64,
) -> Result<Vec<u8>, String> {
    let mut packet = Vec::with_capacity(2 + payload.len());
    packet.push(cmd);
    packet.push(payload.len() as u8);
    packet.extend_from_slice(payload);

    info!("BLE TX: cmd=0x{:02x} payload=[{}]", cmd, hex::encode(payload));

    // Set up response oneshot
    let (tx, mut rx) = oneshot::channel();
    state.pending_response.take(); // drop stale
    state.pending_response = Some(tx);

    helper.cmd_write(&packet).await?;

    // Inner loop: process notifications while waiting for our response
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        tokio::select! {
            result = &mut rx => {
                return result.map_err(|_| format!("response dropped: 0x{:02x}", cmd));
            }
            Some(data) = notify_rx.recv() => {
                route_notification(&data, state, coordinator, helper).await;
            }
            _ = tokio::time::sleep_until(deadline) => {
                state.pending_response.take();
                return Err(format!("timeout: 0x{:02x}", cmd));
            }
        }
    }
}

/// FP-gated command: send -> WAIT_FP -> wait for gate resolution via notifications.
async fn send_fp_gated_inner(
    helper: &mut HelperIO,
    state: &mut BleState,
    coordinator: &Arc<Coordinator>,
    notify_rx: &mut mpsc::Receiver<Vec<u8>>,
    cmd: u8,
    payload: &[u8],
) -> BleResult {
    let rsp = send_command_inner(helper, state, coordinator, notify_rx, cmd, payload, BLE_COMMAND_TIMEOUT_SECS).await?;
    if rsp.is_empty() { return Err("empty response".into()); }

    let status = rsp[0];
    // Cooldown fast path: the command ran immediately. Strip the status
    // byte like the plain SendCommand path does — callers that consume the
    // payload (KEY_OTP_GET's 6-digit code) must not see the frame header.
    if status == RSP_OK { return Ok((RSP_OK, rsp[1..].to_vec())); }

    // Two paths into the gate:
    //   - 0x11 RSP_WAIT_FP: cold path, firmware needs the user to touch
    //   - 0x10 RSP_FP_GATE_APPROVED: cooldown-approved fast path (firmware
    //     1.2.25+, e72bb04). Cooldown means a fingerprint was matched
    //     within the last 10 s for AUTH/KEYSTORE class — KEY_SIGN /
    //     KEY_OTP_GET ride that without re-touching. Firmware sends 0x10
    //     immediately, then runs ECDSA in TMOS (~2 s) and emits the
    //     real result. Treat 0x10 here the same as 0x11 entering gate-
    //     pending state — we still need to wait for the actual data.
    if status != RSP_WAIT_FP && status != RSP_FP_GATE_APPROVED {
        return Err(format!("0x{:02x} failed: 0x{:02x}", cmd, status));
    }

    // Enter FP-gate
    let (gate_tx, mut gate_rx) = oneshot::channel();
    state.gate_pending = true;
    state.gate_tx = Some(gate_tx);
    state.auth_failures = 0;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(BLE_FP_GATE_TIMEOUT_SECS);
    let gate_result = loop {
        tokio::select! {
            result = &mut gate_rx => {
                break result.unwrap_or((false, Some(RSP_ERROR)));
            }
            Some(data) = notify_rx.recv() => {
                route_notification(&data, state, coordinator, helper).await;
            }
            // Caller-driven cancel (CLI socket close / FP:ENROLL_CANCEL).
            // Mirrors mac cancelGateAndRelease (cdd6b07): the BLE worker is
            // parked here, so a GATE_CANCEL submitted via BleCommand would
            // queue behind us. Fire it on the wire directly through the
            // helper and treat the gate as a failure.
            _ = coordinator.gate_cancel.notified() => {
                info!("FP-gate cancelled by caller");
                state.gate_tx.take();
                state.gate_pending = false;
                let _ = helper.cmd_write(&[CMD_GATE_CANCEL, 0x00]).await;
                return Err("FP-gate cancelled".into());
            }
            _ = tokio::time::sleep_until(deadline) => {
                state.gate_tx.take();
                state.gate_pending = false;
                return Err("FP-gate timeout".into());
            }
        }
    };
    state.gate_pending = false;

    match gate_result {
        (true, _) => Ok((RSP_OK, vec![RSP_OK])),
        (false, Some(e)) => Err(format!("FP-gate failed: 0x{:02x}", e)),
        (false, None) => Err("FP-gate failed".into()),
    }
}

// ── Pairing ─────────────────────────────────────────────────────────────────

async fn do_pair(
    helper: &mut HelperIO,
    state: &mut BleState,
    coordinator: &Arc<Coordinator>,
    notify_rx: &mut mpsc::Receiver<Vec<u8>>,
) -> BleResult {
    let mut retries = 3u8;

    loop {
        // PAIR_INIT
        let rsp = send_command_inner(helper, state, coordinator, notify_rx, CMD_PAIR_INIT, &[], 30).await?;

        // 0xE1 = conn params insufficient, retry
        if rsp.len() == 1 && rsp[0] == 0xE1 {
            if retries > 0 {
                retries -= 1;
                warn!("PAIR_INIT rejected (conn params), retry in 5s ({} left)", retries);
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            return Err("PAIR_INIT: conn param update incomplete".into());
        }

        // NEEDS_RESET: device still has fingerprints, must factory-reset first
        // Response: [0x30, 0xF1]
        if rsp.len() == 2 && rsp[0] == CMD_PAIR_INIT && rsp[1] == RSP_PAIR_NEEDS_RESET {
            return Err("device still has fingerprints; factory reset before re-pair".into());
        }

        // WAIT_BUTTON: device requires a physical button press to confirm.
        // Response: [0x30, 0xF0]. The real [0x30][pubkey:33B] arrives after
        // the user short-presses the button and the device finishes ECDH.
        let rsp = if rsp.len() == 2 && rsp[0] == CMD_PAIR_INIT && rsp[1] == RSP_PAIR_WAIT_BUTTON {
            info!("PAIR_INIT accepted; press the device button within 30s to confirm");
            wait_pair_button(helper, state, coordinator, notify_rx).await?
        } else if rsp.len() == 1 && rsp[0] == RSP_WAIT_FP {
            // Backwards compat with pre-1.2.3 firmware that uses FP-gate during pair
            info!("Pairing needs FP verification");
            wait_pair_fp_gate(helper, state, coordinator, notify_rx).await?
        } else {
            rsp
        };

        // 0xE1 after FP-gate
        if rsp.len() == 1 && rsp[0] == 0xE1 {
            if retries > 0 {
                retries -= 1;
                warn!("PAIR_INIT rejected (conn params), retry in 5s ({} left)", retries);
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            return Err("PAIR_INIT: conn param update incomplete".into());
        }

        // Validate: [0x30][pubkey:33B]
        if rsp.len() < 1 + COMPRESSED_PUBKEY_LEN || rsp[0] != CMD_PAIR_INIT {
            return Err(format!("PAIR_INIT bad response: {}", hex::encode(&rsp)));
        }
        let device_pubkey = &rsp[1..1 + COMPRESSED_PUBKEY_LEN];
        info!("Device pubkey: {}...", &hex::encode(device_pubkey)[..20]);

        // Generate app keypair
        let (app_secret, app_pubkey) = security::generate_p256_keypair();

        // PAIR_CONFIRM
        let crsp = send_command_inner(helper, state, coordinator, notify_rx, CMD_PAIR_CONFIRM, &app_pubkey, 30).await?;
        if crsp.len() < 2 || crsp[0] != CMD_PAIR_CONFIRM || crsp[1] != RSP_OK {
            return Err(format!("PAIR_CONFIRM failed: {}", hex::encode(&crsp)));
        }

        // ECDH + HKDF
        let shared_secret = security::ecdh_shared_secret(app_secret, device_pubkey)
            .map_err(|e| format!("ECDH: {}", e))?;
        let shared_key = security::derive_shared_key(&shared_secret)
            .map_err(|e| format!("HKDF: {}", e))?;

        let pairing = PairingData {
            device_uuid: hex::encode(device_pubkey),
            shared_key,
            paired_at: format!("{}Z", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()),
        };

        *coordinator.pairing.write().await = Some(pairing.clone());
        if let Err(e) = security::save_pairing(&pairing) {
            warn!("Failed to save pairing: {}", e);
        }

        info!("ECDH pairing successful");
        return Ok((RSP_OK, vec![RSP_OK]));
    }
}

/// PAIR_INIT was accepted with WAIT_BUTTON. Wait for one of:
///   - [0x34, 0x00 / 0x02] terminal button event → fail (timeout / cancelled)
///   - [0x30][pubkey:33B] arriving via pending_response → success (ECDH done)
///
/// Firmware gives 30s for the button press + ~2s for ECDH, so the wrapping
/// timeout is BLE_PAIR_BUTTON_TIMEOUT_SECS (35s).
async fn wait_pair_button(
    helper: &mut HelperIO,
    state: &mut BleState,
    coordinator: &Arc<Coordinator>,
    notify_rx: &mut mpsc::Receiver<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let (btn_tx, mut btn_rx) = oneshot::channel::<u8>();
    let (cmd_tx, mut cmd_rx) = oneshot::channel::<Vec<u8>>();

    state.pair_button_pending = true;
    state.pair_button_tx = Some(btn_tx);
    state.pending_response.take();
    state.pending_response = Some(cmd_tx);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(BLE_PAIR_BUTTON_TIMEOUT_SECS);
    let result = loop {
        tokio::select! {
            r = &mut cmd_rx => {
                break match r {
                    Ok(data) => Ok(data),
                    Err(_) => Err("pair: response channel dropped".to_string()),
                };
            }
            r = &mut btn_rx => {
                break match r {
                    Ok(s) if s == PAIR_BUTTON_TIMEOUT => {
                        Err("pair timeout: button not pressed within 30s".to_string())
                    }
                    Ok(s) if s == PAIR_BUTTON_CANCELLED => {
                        Err("pair cancelled (long-press on device)".to_string())
                    }
                    Ok(s) => Err(format!("unknown button status: 0x{:02x}", s)),
                    Err(_) => Err("pair: button channel dropped".to_string()),
                };
            }
            Some(data) = notify_rx.recv() => {
                route_notification(&data, state, coordinator, helper).await;
            }
            _ = tokio::time::sleep_until(deadline) => {
                break Err("pair timeout: no button event".to_string());
            }
        }
    };

    state.pair_button_pending = false;
    state.pair_button_tx.take();
    result
}

async fn wait_pair_fp_gate(
    helper: &mut HelperIO,
    state: &mut BleState,
    coordinator: &Arc<Coordinator>,
    notify_rx: &mut mpsc::Receiver<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    state.pair_fp_gate = true;
    state.auth_failures = 0;

    let result = async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let (tx, mut rx) = oneshot::channel();
            state.pending_response.take();
            state.pending_response = Some(tx);

            let rsp = loop {
                tokio::select! {
                    result = &mut rx => {
                        break result.map_err(|_| "pair FP-gate: channel dropped".to_string())?;
                    }
                    Some(data) = notify_rx.recv() => {
                        route_notification(&data, state, coordinator, helper).await;
                    }
                    _ = tokio::time::sleep_until(deadline) => {
                        return Err("pair FP-gate timeout".to_string());
                    }
                }
            };

            // FP not match -> retry
            if rsp.len() == 1 && rsp[0] == RSP_ERR_FP_NOT_MATCH {
                state.auth_failures += 1;
                let rem = FP_GATE_MAX_FAILURES.saturating_sub(state.auth_failures);
                warn!("Pair FP mismatch ({} left)", rem);
                if rem == 0 { return Err("too many FP mismatches".to_string()); }
                continue;
            }
            if rsp.len() == 1 && matches!(rsp[0], RSP_ERR_TIMEOUT | RSP_ERROR | RSP_BUSY) {
                return Err(format!("pair FP-gate error: 0x{:02x}", rsp[0]));
            }
            return Ok(rsp);
        }
    }.await;

    state.pair_fp_gate = false;
    result
}

// ── AUTH_REQUEST ─────────────────────────────────────────────────────────────

/// Send AUTH_REQUEST, wait for WAIT_FP, then wait for [00] success or [07] fail.
/// Uses auth_pending flag so route_notification resolves the result.
async fn do_auth_request(
    helper: &mut HelperIO,
    state: &mut BleState,
    coordinator: &Arc<Coordinator>,
    notify_rx: &mut mpsc::Receiver<Vec<u8>>,
) -> Result<bool, String> {
    let rsp = send_command_inner(helper, state, coordinator, notify_rx, CMD_AUTH_REQUEST, &[], BLE_COMMAND_TIMEOUT_SECS).await?;
    if rsp.is_empty() { return Err("empty response".into()); }

    let status = rsp[0];
    if status == RSP_OK { return Ok(true); } // cooldown
    if status != RSP_WAIT_FP { return Err(format!("AUTH_REQUEST failed: 0x{:02x}", status)); }

    // Wait for [00] (success) or [07] (not match) via auth_pending
    let (auth_tx, mut auth_rx) = oneshot::channel();
    state.auth_pending = true;
    state.auth_tx = Some(auth_tx);
    state.auth_failures = 0;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(BLE_AUTH_TIMEOUT_SECS);
    let result = loop {
        tokio::select! {
            result = &mut auth_rx => {
                break result.unwrap_or(false);
            }
            Some(data) = notify_rx.recv() => {
                route_notification(&data, state, coordinator, helper).await;
            }
            // Caller-driven abort (dialog Cancel button, imk socket close,
            // outer timeout in handle_agent_approve, etc.). Goes through
            // a Notify channel because these run on different tasks and
            // can't reach BLE wire via the BleCommand queue — that queue
            // is blocked behind THIS in-flight AuthRequest. Fire GATE_CANCEL
            // directly via the helper to stop the device LED immediately.
            _ = coordinator.auth_dialog_cancel.notified() => {
                info!("AUTH cancelled by dialog/caller");
                state.auth_tx.take();
                state.auth_pending = false;
                let _ = helper.cmd_write(&[CMD_GATE_CANCEL, 0x00]).await;
                return Ok(false);
            }
            _ = tokio::time::sleep_until(deadline) => {
                state.auth_tx.take();
                state.auth_pending = false;
                return Ok(false); // timeout
            }
        }
    };
    state.auth_pending = false;
    Ok(result)
}

// ── BLE command handler ─────────────────────────────────────────────────────

async fn handle_ble_command(
    cmd: BleCommand,
    helper: &mut HelperIO,
    state: &mut BleState,
    coordinator: &Arc<Coordinator>,
    notify_rx: &mut mpsc::Receiver<Vec<u8>>,
) {
    match cmd {
        BleCommand::SendCommand { cmd: c, payload, reply } => {
            let result = send_command_inner(helper, state, coordinator, notify_rx, c, &payload, BLE_COMMAND_TIMEOUT_SECS).await;
            let _ = reply.send(match result {
                Ok(data) => { let s = data.first().copied().unwrap_or(RSP_ERROR); Ok((s, data[1..].to_vec())) }
                Err(e) => Err(e),
            });
        }
        BleCommand::SendFpGated { cmd: c, payload, reply } => {
            let result = send_fp_gated_inner(helper, state, coordinator, notify_rx, c, &payload).await;
            let _ = reply.send(result);
        }
        BleCommand::Pair { reply } => {
            let result = do_pair(helper, state, coordinator, notify_rx).await;
            let _ = reply.send(result);
        }
        BleCommand::AuthRequest { reply } => {
            let result = do_auth_request(helper, state, coordinator, notify_rx).await;
            let _ = reply.send(result);
        }
        BleCommand::OtaWriteRead { data, timeout_ms, reply } => {
            let result = ota_write_and_read(helper, &data, timeout_ms).await;
            let _ = reply.send(result);
        }
        BleCommand::OtaWrite { data, reply } => {
            let result = ota_write_only(helper, &data).await;
            let _ = reply.send(result);
        }
        BleCommand::SyncKeys { reply } => {
            info!("Key cache re-sync requested");
            let result = sync_ssh_keys(helper, state, coordinator, notify_rx).await;
            let _ = reply.send(result);
        }
        BleCommand::Disconnect => {
            info!("Disconnect requested");
            abort_pending(state);
        }
    }
}

// ── OTA characteristic I/O (via helper subprocess) ───────────────────────

/// Write data to OTA characteristic via helper, then poll-read via helper
/// until response arrives.
async fn ota_write_and_read(
    helper: &mut HelperIO,
    data: &[u8],
    timeout_ms: u64,
) -> Result<Vec<u8>, String> {
    helper.ota_write(data).await?;

    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    let poll_interval = Duration::from_millis(OTA_READ_POLL_INTERVAL_MS);

    loop {
        tokio::time::sleep(poll_interval).await;
        if tokio::time::Instant::now() >= deadline {
            return Err("OTA read timeout".to_string());
        }
        match helper.ota_read().await {
            Ok(result) if !result.is_empty() => return Ok(result),
            Ok(_) => {} // empty result, keep polling
            Err(_) => {} // read error, keep polling
        }
    }
}

/// Write data to OTA characteristic via helper.
async fn ota_write_only(
    helper: &mut HelperIO,
    data: &[u8],
) -> Result<(), String> {
    helper.ota_write(data).await
}

/// Find the ble-notify-helper.py script.
fn find_helper_script() -> String {
    // Check next to the daemon binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("ble-notify-helper.py");
            if candidate.exists() {
                return candidate.to_string_lossy().to_string();
            }
            // Check ../scripts/
            let candidate = dir.join("../scripts/ble-notify-helper.py");
            if candidate.exists() {
                return candidate.canonicalize().unwrap_or(candidate).to_string_lossy().to_string();
            }
        }
    }
    // Check in the source tree (development mode)
    let dev_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/ble-notify-helper.py");
    if std::path::Path::new(dev_path).exists() {
        return dev_path.to_string();
    }
    // Fallback: assume in PATH or current dir
    "ble-notify-helper.py".to_string()
}
