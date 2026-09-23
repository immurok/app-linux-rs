//! Where a fetched value goes. Phase 1 ships the clipboard only; phase 2 adds
//! the typing backends (portal / xdotool / wtype) as more variants.

use std::time::Duration;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

pub const CLIPBOARD_CLEAR_AFTER: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    Clipboard,
}

impl Output {
    /// Typing backends must run after the panel is gone and focus is back on
    /// the target window. The clipboard is the opposite: GNOME only lets a
    /// focused window write it, so it must run while the panel is still up.
    pub fn needs_focus_return(&self) -> bool {
        match self {
            Output::Clipboard => false,
        }
    }

    pub async fn inject(&self, app: &adw::Application, text: &str) -> Result<(), String> {
        match self {
            Output::Clipboard => copy_and_schedule_clear(app, text),
        }
    }
}

fn copy_and_schedule_clear(app: &adw::Application, text: &str) -> Result<(), String> {
    let display = gtk::gdk::Display::default().ok_or("no display")?;
    let clipboard = display.clipboard();
    clipboard.set_text(text);

    let note = gio::Notification::new("immurok");
    note.set_body(Some(&format!(
        "Copied to clipboard, cleared in {} s",
        CLIPBOARD_CLEAR_AFTER.as_secs()
    )));
    app.send_notification(Some("quick-fill"), &note);

    // Without a resident instance the quick-fill panel is the application's
    // only hold: `deliver()` closes it right after this returns, the use
    // count hits zero, `app.run()` returns and the process exits with the
    // secret still on the clipboard (spec §10 says it must be cleared).
    // Hold the app up for the whole 30 s instead.
    let hold = app.hold();

    // Only clear if the clipboard still holds what we put there; the user
    // may have copied something else in the meantime.
    let ours = text.to_string();
    let app = app.clone();
    glib::spawn_future_local(async move {
        // `hold` is moved into this future by the `drop` at the end; it lives
        // exactly as long as the pending clear.
        glib::timeout_future(CLIPBOARD_CLEAR_AFTER).await;
        if let Ok(Some(current)) = clipboard.read_text_future().await {
            if current.as_str() == ours {
                clipboard.set_text("");
            }
        }
        app.withdraw_notification("quick-fill");
        // Explicit: the app may exit the moment the last hold goes away.
        drop(hold);
    });
    Ok(())
}
