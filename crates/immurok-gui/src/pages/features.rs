//! Features page: the five daemon feature toggles. Toggles write through
//! immediately and only flip once the daemon has confirmed; state is fed
//! by the Dashboard's 2 s poll via [`FeaturesPage::apply`] — no second
//! poll loop.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::status::{set_setting, SettingKey, Settings};

use super::run_blocking;

pub struct FeaturesPage {
    root: gtk::Widget,
    switches: Vec<(SettingKey, gtk::Switch)>,
    toasts: adw::ToastOverlay,
}

impl FeaturesPage {
    pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self> {
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .title("Features")
            .description("Each toggle takes effect immediately.")
            .build();
        let mut switches = Vec::new();
        for (key, title, subtitle) in [
            (SettingKey::UnlockSudo, "sudo with fingerprint", "Touch the device instead of typing your password for sudo"),
            (SettingKey::UnlockPolkit, "System authorization (polkit)", "Touch the device for graphical authorization prompts"),
            (SettingKey::UnlockScreen, "Unlock screen", "Touch the device to unlock the lock screen"),
            (SettingKey::LockScreen, "Long-press to lock", "Long-press the sensor to lock the screen"),
            (SettingKey::SshTakeover, "SSH agent takeover", "Let ssh use the keys on the device"),
        ] {
            let sw = gtk::Switch::builder().valign(gtk::Align::Center).build();
            let row = adw::ActionRow::builder().title(title).subtitle(subtitle).build();
            row.add_suffix(&sw);
            row.set_activatable_widget(Some(&sw));
            group.add(&row);
            switches.push((key, sw));
        }
        page.add(&group);

        let this = Rc::new(Self { root: page.upcast(), switches, toasts: toasts.clone() });
        this.wire_switches();
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        &self.root
    }

    fn wire_switches(&self) {
        for (key, sw) in &self.switches {
            let key = *key;
            let toasts = self.toasts.clone();
            // Set while a `set_setting` write is in flight for this switch.
            let in_flight = Rc::new(Cell::new(false));
            // Return Stop: we own the `state` property and flip it only after
            // the daemon confirms, so a failed write leaves the switch where
            // it was instead of lying.
            sw.connect_state_set(move |sw, wanted| {
                // `gtk_switch_set_active` re-emits `state-set`, so our own
                // revert below and `apply()`'s external sync come back
                // through here. Without this guard a failing write reverts,
                // re-enters with the opposite `wanted`, writes again … a hot
                // loop of failed SETs and toasts.
                if wanted == sw.state() {
                    return glib::Propagation::Stop;
                }
                if in_flight.get() {
                    sw.set_active(sw.state());
                    return glib::Propagation::Stop;
                }
                in_flight.set(true);
                let sw = sw.clone();
                let toasts = toasts.clone();
                let in_flight = in_flight.clone();
                glib::spawn_future_local(async move {
                    match run_blocking(move || set_setting(key, wanted)).await {
                        Some(Ok(())) => sw.set_state(wanted),
                        Some(Err(e)) => {
                            toasts.add_toast(adw::Toast::new(&format!(
                                "Failed to save setting: {}",
                                glib::markup_escape_text(&e)
                            )));
                            sw.set_active(!wanted);
                        }
                        None => sw.set_active(!wanted),
                    }
                    in_flight.set(false);
                });
                glib::Propagation::Stop
            });
        }
    }

    /// Sync switches to what the daemon reports (called from the Dashboard poll).
    pub fn apply(&self, s: &Settings) {
        self.root.set_sensitive(true);
        for (key, sw) in &self.switches {
            let on = match key {
                SettingKey::UnlockSudo => s.unlock_sudo,
                SettingKey::UnlockPolkit => s.unlock_polkit,
                SettingKey::UnlockScreen => s.unlock_screen,
                SettingKey::LockScreen => s.lock_screen,
                SettingKey::SshTakeover => s.ssh_takeover,
            };
            // Setting `active` would re-enter state-set; we own `state`.
            if sw.state() != on {
                sw.set_state(on);
                sw.set_active(on);
            }
        }
    }

    /// Grey the page out while the daemon is unreachable.
    pub fn set_daemon_available(&self, ok: bool) {
        self.root.set_sensitive(ok);
    }
}
