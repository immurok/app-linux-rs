//! systemd-logind sleep/resume monitor.
//!
//! Subscribes to `org.freedesktop.login1.Manager.PrepareForSleep(bool start)`
//! on the system bus. The signal fires twice around every suspend cycle:
//!
//!   true  — about to sleep. We set `coordinator.is_suspending` so the BLE
//!           wait loop stops firing active connects, and cancel any pending
//!           `Device.Connect()` (BlueZ keeps the LE create-connection alive
//!           internally even after our client-side timeout drops the D-Bus
//!           call). Carrying that pending connect across the suspend
//!           boundary races the kernel's hci resume re-init (LL-privacy
//!           enable, opcode 0x202d) and has wedged Intel BT firmware hard
//!           enough that only a module reload revives it.
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
            info!("System preparing to sleep — pausing reconnect, cancelling pending connect");
            coordinator
                .is_suspending
                .store(true, std::sync::atomic::Ordering::Relaxed);
            // Bounded: logind only gives inhibitor-delay time (~5s) before
            // it suspends anyway; a wedged D-Bus call must not hold this up.
            if tokio::time::timeout(
                Duration::from_secs(3),
                crate::ble::cancel_pending_connect(),
            )
            .await
            .is_err()
            {
                warn!("Pending-connect cancel timed out before sleep");
            }
        } else {
            info!("System resumed — kicking BLE reconnect");
            coordinator
                .is_suspending
                .store(false, std::sync::atomic::Ordering::Relaxed);
            coordinator.resume_notify.notify_one();
        }
    }
    Ok(())
}
