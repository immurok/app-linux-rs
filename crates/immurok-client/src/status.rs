//! Device status and feature toggles.
//!
//! Wire formats (see `immurok-common/src/socket_proto.rs` and
//! `immurok-daemon/src/socket.rs`):
//!   STATUS         → `STATUS:<connected 0/1>:<name>:<battery>:<fw>[:<device_unpaired 0/1>]`
//!   GET:SETTINGS   → `OK:sudo=1:polkit=0:screen=1:lock=0:ssh=1`
//!   SET:<KEY>:<0|1> → `OK:…`
//!   PAIR:STATUS    → `OK:PAIRED` / `OK:UNPAIRED`

use crate::DaemonClient;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceStatus {
    pub connected: bool,
    pub name: String,
    pub battery: u8,
    pub fw_version: String,
    /// The device itself says it is not paired with this host (factory
    /// reset / slot cleared). Older daemons omit the field.
    pub device_unpaired: bool,
    /// BlueZ-level link is up (Connected + ServicesResolved) but the GATT
    /// session is not ready yet: helper starting or being retried. Always
    /// true when `connected` is. Older daemons omit the field.
    pub link_up: bool,
}

pub fn parse_status_line(line: &str) -> Option<DeviceStatus> {
    let parts: Vec<&str> = line.trim().split(':').collect();
    if parts.first() != Some(&"STATUS") || parts.len() < 5 {
        return None;
    }
    Some(DeviceStatus {
        connected: parts[1] == "1",
        name: parts[2].to_string(),
        battery: parts[3].parse().unwrap_or(0),
        fw_version: parts[4].to_string(),
        device_unpaired: parts.get(5) == Some(&"1"),
        link_up: parts[1] == "1" || parts.get(6) == Some(&"1"),
    })
}

pub fn query_status() -> Result<DeviceStatus, String> {
    let rsp = DaemonClient::connect()?.send("STATUS")?;
    parse_status_line(&rsp).ok_or_else(|| format!("unexpected STATUS reply: {rsp}"))
}

pub fn query_paired() -> Result<bool, String> {
    let rsp = DaemonClient::connect()?.send("PAIR:STATUS")?;
    Ok(rsp.split(':').nth(1) == Some("PAIRED"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Settings {
    pub unlock_sudo: bool,
    pub unlock_polkit: bool,
    pub unlock_screen: bool,
    pub lock_screen: bool,
    pub ssh_takeover: bool,
}

pub fn parse_settings_line(line: &str) -> Option<Settings> {
    let mut parts = line.trim().split(':');
    if parts.next() != Some("OK") {
        return None;
    }
    let mut s = Settings::default();
    for part in parts {
        if let Some((k, v)) = part.split_once('=') {
            let on = v == "1";
            match k {
                "sudo" => s.unlock_sudo = on,
                "polkit" => s.unlock_polkit = on,
                "screen" => s.unlock_screen = on,
                "lock" => s.lock_screen = on,
                "ssh" => s.ssh_takeover = on,
                _ => {}
            }
        }
    }
    Some(s)
}

/// Live BLE connection parameters as the device sees them (`GET:CONN_PARAMS`).
/// Units are the link layer's: interval × 1.25 ms, latency in intervals,
/// supervision timeout × 10 ms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnParams {
    pub interval: u16,
    pub latency: u16,
    pub timeout: u16,
}

impl ConnParams {
    /// The device runs its ~2 s ECC steps (linking, SSH signing) only when
    /// the supervision timeout is at least this long; it asks for 6000 ms.
    pub const ECC_MIN_TIMEOUT_MS: u32 = 3000;

    pub fn interval_ms(&self) -> f64 {
        f64::from(self.interval) * 1.25
    }
    pub fn timeout_ms(&self) -> u32 {
        u32::from(self.timeout) * 10
    }
    /// Longest gap between radio events the peripheral may take.
    pub fn effective_interval_ms(&self) -> f64 {
        self.interval_ms() * f64::from(self.latency + 1)
    }
    pub fn ecc_ok(&self) -> bool {
        self.timeout_ms() >= Self::ECC_MIN_TIMEOUT_MS
    }
}

pub fn parse_conn_params_line(line: &str) -> Option<ConnParams> {
    let parts: Vec<&str> = line.trim().split(':').collect();
    if parts.first() != Some(&"OK") || parts.len() < 4 {
        return None;
    }
    Some(ConnParams {
        interval: parts[1].parse().ok()?,
        latency: parts[2].parse().ok()?,
        timeout: parts[3].parse().ok()?,
    })
}

pub fn query_conn_params() -> Result<ConnParams, String> {
    let rsp = DaemonClient::connect()?.send("GET:CONN_PARAMS")?;
    if !rsp.starts_with("OK") {
        return Err(rsp);
    }
    parse_conn_params_line(&rsp).ok_or_else(|| format!("unexpected GET:CONN_PARAMS reply: {rsp}"))
}

pub fn query_settings() -> Result<Settings, String> {
    let rsp = DaemonClient::connect()?.send("GET:SETTINGS")?;
    parse_settings_line(&rsp).ok_or_else(|| format!("unexpected GET:SETTINGS reply: {rsp}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKey {
    UnlockSudo,
    UnlockPolkit,
    UnlockScreen,
    LockScreen,
    SshTakeover,
}

impl SettingKey {
    pub fn wire(self) -> &'static str {
        match self {
            SettingKey::UnlockSudo => "UNLOCK_SUDO",
            SettingKey::UnlockPolkit => "UNLOCK_POLKIT",
            SettingKey::UnlockScreen => "UNLOCK_SCREEN",
            SettingKey::LockScreen => "LOCK_SCREEN",
            SettingKey::SshTakeover => "SSH_TAKEOVER",
        }
    }
}

pub fn set_setting(key: SettingKey, on: bool) -> Result<(), String> {
    let cmd = format!("SET:{}:{}", key.wire(), if on { 1 } else { 0 });
    let rsp = DaemonClient::connect()?.send(&cmd)?;
    if rsp.starts_with("OK") {
        Ok(())
    } else {
        Err(rsp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_status_line() {
        let s = parse_status_line("STATUS:1:immurok IK-1:87:1.8.0:0").unwrap();
        assert!(s.connected);
        assert_eq!(s.name, "immurok IK-1");
        assert_eq!(s.battery, 87);
        assert_eq!(s.fw_version, "1.8.0");
        assert!(!s.device_unpaired);
    }

    #[test]
    fn parses_old_daemon_without_unpaired_flag() {
        let s = parse_status_line("STATUS:0:-:0:-").unwrap();
        assert!(!s.connected);
        assert!(!s.device_unpaired);
    }

    #[test]
    fn rejects_non_status_line() {
        assert!(parse_status_line("OK:PAIRED").is_none());
        assert!(parse_status_line("STATUS:1").is_none());
    }

    #[test]
    fn conn_params_parse_and_units() {
        let p = parse_conn_params_line("OK:24:0:42").unwrap();
        assert_eq!((p.interval, p.latency, p.timeout), (24, 0, 42));
        assert_eq!(p.interval_ms(), 30.0);
        assert_eq!(p.timeout_ms(), 420);
        assert!(!p.ecc_ok());
        let p = parse_conn_params_line("OK:48:20:600").unwrap();
        assert_eq!(p.effective_interval_ms(), 60.0 * 21.0);
        assert!(p.ecc_ok());
        assert!(parse_conn_params_line("ERROR:NOT_CONNECTED").is_none());
        assert!(parse_conn_params_line("OK:24:0").is_none());
    }

    #[test]
    fn link_up_field() {
        // Bluetooth link up, session not ready: the seventh field says so.
        let s = parse_status_line("STATUS:0::0::0:1").unwrap();
        assert!(!s.connected);
        assert!(s.link_up);
        // Connected implies link up even when an old daemon omits the field.
        let s = parse_status_line("STATUS:1:immurok:80:1.8.0").unwrap();
        assert!(s.connected && s.link_up);
        let s = parse_status_line("STATUS:0::0::0").unwrap();
        assert!(!s.link_up);
    }

    #[test]
    fn parses_settings() {
        let s = parse_settings_line("OK:sudo=1:polkit=0:screen=1:lock=0:ssh=1").unwrap();
        assert!(s.unlock_sudo);
        assert!(!s.unlock_polkit);
        assert!(s.unlock_screen);
        assert!(!s.lock_screen);
        assert!(s.ssh_takeover);
    }

    #[test]
    fn settings_rejects_error_reply() {
        assert!(parse_settings_line("ERROR:NOT_CONNECTED").is_none());
    }

    #[test]
    fn setting_key_wire_names_match_daemon() {
        assert_eq!(SettingKey::UnlockSudo.wire(), "UNLOCK_SUDO");
        assert_eq!(SettingKey::UnlockPolkit.wire(), "UNLOCK_POLKIT");
        assert_eq!(SettingKey::UnlockScreen.wire(), "UNLOCK_SCREEN");
        assert_eq!(SettingKey::LockScreen.wire(), "LOCK_SCREEN");
        assert_eq!(SettingKey::SshTakeover.wire(), "SSH_TAKEOVER");
    }
}
