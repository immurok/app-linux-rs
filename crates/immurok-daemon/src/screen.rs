//! Screen lock monitor — watches D-Bus session signals to track lock/unlock state.
//!
//! Listens for `ActiveChanged(bool)` signals on:
//!   - GNOME: `org.gnome.ScreenSaver` at `/org/gnome/ScreenSaver`
//!   - KDE/freedesktop: `org.freedesktop.ScreenSaver` at `/ScreenSaver`
//!
//! If D-Bus connection fails, logs a warning and degrades gracefully (sleeps forever).

use std::sync::atomic::Ordering;
use std::sync::Arc;

use futures_lite::StreamExt;
use tracing::{info, warn};
use zbus::Connection;
use zbus::message::Type as MessageType;

use crate::coordinator::Coordinator;

/// Main screen monitor loop. Connects to the session D-Bus and listens for
/// ScreenSaver ActiveChanged signals from GNOME and KDE/freedesktop.
/// Retries on disconnect (e.g. after logout/login) so the daemon keeps running.
pub async fn monitor(coordinator: Arc<Coordinator>) {
    loop {
        match monitor_inner(&coordinator).await {
            Ok(()) => {
                // Stream ended (session bus disconnected) — retry after delay
                warn!("Screen monitor: D-Bus stream ended, retrying in 5s...");
            }
            Err(e) => {
                warn!("Screen monitor failed: {} — retrying in 5s...", e);
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

async fn monitor_inner(coordinator: &Arc<Coordinator>) -> Result<(), String> {
    let connection = Connection::session()
        .await
        .map_err(|e| format!("D-Bus session connection failed: {}", e))?;

    // Add match rules for both GNOME and KDE/freedesktop screensaver signals.
    // We use raw match rules via the MessageStream to catch signals from either DE.

    // GNOME match rule
    let gnome_rule = "type='signal',interface='org.gnome.ScreenSaver',member='ActiveChanged'";
    connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "AddMatch",
            &gnome_rule,
        )
        .await
        .map_err(|e| format!("AddMatch (GNOME) failed: {}", e))?;

    // KDE/freedesktop match rule
    let kde_rule =
        "type='signal',interface='org.freedesktop.ScreenSaver',member='ActiveChanged'";
    connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "AddMatch",
            &kde_rule,
        )
        .await
        .map_err(|e| format!("AddMatch (KDE) failed: {}", e))?;

    info!("Screen lock monitor started (GNOME + KDE/freedesktop)");

    // Listen for signals using a MessageStream
    let mut stream = zbus::MessageStream::from(&connection);

    loop {
        let msg: zbus::Message = match stream.try_next().await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(e) => {
                warn!("D-Bus stream error: {}", e);
                break;
            }
        };

        // Only process signals
        let header = msg.header();
        if header.message_type() != MessageType::Signal {
            continue;
        }

        let member = match header.member() {
            Some(m) => m.as_str().to_string(),
            None => continue,
        };

        if member != "ActiveChanged" {
            continue;
        }

        // Parse the boolean argument
        let body = msg.body();
        match body.deserialize::<bool>() {
            Ok(active) => {
                coordinator.screen_locked.store(active, Ordering::Relaxed);
                info!(
                    "Screen state: {}",
                    if active { "locked" } else { "unlocked" }
                );

                // If screen just unlocked, no action needed (loginctl handles that).
                // If screen just locked, the coordinator will use this state for FP match routing.
            }
            Err(e) => {
                warn!("Failed to parse ActiveChanged signal: {}", e);
            }
        }
    }

    Ok(())
}
