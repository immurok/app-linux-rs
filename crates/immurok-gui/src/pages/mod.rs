//! Pages of the main window. Every daemon round-trip goes through
//! [`run_blocking`] — a BLE-backed request can take seconds and must never
//! run on the GTK main thread.

pub mod dashboard;
pub mod features;
pub mod fingerprints;
pub mod firmware;
pub mod hosts;
pub mod keys;
pub mod logs;
pub mod pam;

use gtk4 as gtk;
use gtk::gio;

pub async fn run_blocking<T, F>(f: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    gio::spawn_blocking(f).await.ok()
}

/// Ask the user a yes/no question. Resolves to `true` on the affirmative
/// button. `gtk::MessageDialog` rather than `adw::MessageDialog`: the latter
/// is libadwaita 1.2 and Ubuntu 22.04 ships 1.1.
pub async fn confirm(parent: &impl glib::object::IsA<gtk::Window>, title: &str, body: &str, yes: &str) -> bool {
    use gtk::prelude::*;
    let dialog = gtk::MessageDialog::builder()
        .transient_for(parent)
        .modal(true)
        .message_type(gtk::MessageType::Question)
        .text(title)
        .secondary_text(body)
        .build();
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button(yes, gtk::ResponseType::Accept);
    let response = dialog.run_future().await;
    dialog.close();
    response == gtk::ResponseType::Accept
}

use gtk::glib;
