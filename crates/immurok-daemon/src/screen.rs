//! Screen lock monitor — tracks whether the owner's screen is locked.
//!
//! This used to subscribe to `org.gnome.ScreenSaver` / `org.freedesktop
//! .ScreenSaver`'s `ActiveChanged` on the **session** bus. The daemon runs as
//! a dedicated system user now and has no session bus at all, so that code
//! could only ever fail with EACCES in a 5-second retry loop — and with it,
//! "touch the sensor to unlock" silently stopped working.
//!
//! logind exposes the same fact on the **system** bus as the session's
//! `LockedHint`, which GNOME and KDE both set. We use the bus purely as a
//! wake-up (a `Lock`/`Unlock` signal, or a `PropertiesChanged` on any session
//! object) and then ask logind for the authoritative state, plus a slow poll
//! so a missed signal self-corrects.
//!
//! Compositors that never set LockedHint (swaylock, i3lock) look permanently
//! unlocked — the same blind spot the screensaver-signal version had, since
//! they do not emit those signals either.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use futures_lite::StreamExt;
use tracing::{debug, info, warn};
use zbus::Connection;

use crate::coordinator::Coordinator;
use crate::session;

/// How often to re-check regardless of signals.
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Main screen monitor loop. Retries on disconnect (e.g. system bus restart),
/// backing off so a permanently unavailable bus does not flood the log — the
/// old version wrote a warning every 5 seconds, forever.
pub async fn monitor(coordinator: Arc<Coordinator>) {
    let mut backoff = Duration::from_secs(5);
    loop {
        match monitor_inner(&coordinator).await {
            Ok(()) => {
                warn!("Screen monitor: D-Bus stream ended, reconnecting in {:?}", backoff);
            }
            Err(e) => {
                warn!("Screen monitor failed: {} — retrying in {:?}", e, backoff);
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(300));
    }
}

/// Ask logind whether the owner's screen is locked and store the answer.
///
/// Silent when nothing is known (no owner recorded, no graphical session):
/// leaving the previous value alone is better than claiming "unlocked", which
/// would route a fingerprint touch to a pre-auth grant instead of an unlock.
async fn refresh(coordinator: &Arc<Coordinator>) {
    let Some(uid) = session::owner_uid() else { return };
    let Some(sid) = session::graphical_session_id(uid).await else { return };
    let Some(locked) = session::session_locked(&sid).await else { return };

    let previous = coordinator.screen_locked.swap(locked, Ordering::Relaxed);
    if previous != locked {
        info!("Screen state: {}", if locked { "locked" } else { "unlocked" });
    }
}

async fn monitor_inner(coordinator: &Arc<Coordinator>) -> Result<(), String> {
    let connection = Connection::system()
        .await
        .map_err(|e| format!("D-Bus system connection failed: {}", e))?;

    // Lock()/Unlock() are what a lock request looks like on the wire;
    // LockedHint arrives as a PropertiesChanged on the session object. Match
    // all three rather than guessing which one a given desktop emits.
    let rules = [
        "type='signal',sender='org.freedesktop.login1',\
         interface='org.freedesktop.login1.Session',member='Lock'",
        "type='signal',sender='org.freedesktop.login1',\
         interface='org.freedesktop.login1.Session',member='Unlock'",
        "type='signal',sender='org.freedesktop.login1',\
         interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',\
         path_namespace='/org/freedesktop/login1/session'",
    ];
    for rule in rules {
        connection
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "AddMatch",
                &rule,
            )
            .await
            .map_err(|e| format!("AddMatch failed: {}", e))?;
    }

    info!("Screen lock monitor started (logind LockedHint)");
    refresh(coordinator).await;

    let mut stream = zbus::MessageStream::from(&connection);
    loop {
        tokio::select! {
            msg = stream.next() => {
                match msg {
                    Some(Ok(m)) => {
                        debug!("Screen monitor: logind signal {:?}", m.header().member());
                        // The signal only says "something changed"; logind is
                        // asked for the actual state so we never have to map
                        // per-desktop signal shapes onto locked/unlocked.
                        refresh(coordinator).await;
                    }
                    Some(Err(e)) => return Err(format!("D-Bus stream error: {}", e)),
                    None => return Ok(()),
                }
            }
            _ = tokio::time::sleep(POLL_INTERVAL) => refresh(coordinator).await,
        }
    }
}
