//! systemd-logind sleep/resume monitor.
//!
//! Subscribes to `org.freedesktop.login1.Manager.PrepareForSleep(bool start)`
//! on the system bus. The signal fires twice around every suspend cycle:
//!
//!   true  — about to sleep (we don't act; BlueZ tears down BLE itself and
//!           the helper subprocess will emit DISCONNECT, kicking the BLE
//!           main loop into wait_for_device_connected on resume).
//!   false — just resumed.
//!
//! On resume we trigger `coordinator.resume_notify`, which the BLE wait
//! loop turns into an active `Device.Connect()` call. This bypasses the
//! "wait for BlueZ to notice the device is back" delay — BlueZ does not
//! auto-reconnect BLE LE devices the way it does classic BR/EDR HID, so
//! without this nudge the daemon can sit idle for many seconds after
//! resume before the polling fallback rediscovers the device.
//!
//! D-Bus connection retries on disconnect (e.g. system bus restart).

use std::sync::Arc;
use std::time::Duration;

use futures_lite::StreamExt;
use tracing::{info, warn};
use zbus::Connection;

use crate::coordinator::Coordinator;

pub async fn monitor(coordinator: Arc<Coordinator>) {
    loop {
        match monitor_inner(&coordinator).await {
            Ok(()) => warn!("Sleep monitor: D-Bus stream ended, retrying in 5s..."),
            Err(e) => warn!("Sleep monitor failed: {} — retrying in 5s...", e),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn monitor_inner(coordinator: &Arc<Coordinator>) -> Result<(), String> {
    let connection = Connection::system()
        .await
        .map_err(|e| format!("D-Bus system connection failed: {}", e))?;

    let rule = "type='signal',\
                sender='org.freedesktop.login1',\
                interface='org.freedesktop.login1.Manager',\
                member='PrepareForSleep'";
    connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "AddMatch",
            &rule,
        )
        .await
        .map_err(|e| format!("AddMatch (login1.PrepareForSleep) failed: {}", e))?;

    info!("Sleep/resume monitor started (logind PrepareForSleep)");

    let mut stream = zbus::MessageStream::from(&connection);
    while let Some(msg) = stream.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                warn!("Sleep monitor: stream error: {}", e);
                break;
            }
        };
        let going_to_sleep: bool = match msg.body().deserialize() {
            Ok(b) => b,
            Err(_) => continue, // not our signal shape
        };
        if going_to_sleep {
            info!("System preparing to sleep");
        } else {
            info!("System resumed — kicking BLE reconnect");
            coordinator.resume_notify.notify_one();
        }
    }
    Ok(())
}
