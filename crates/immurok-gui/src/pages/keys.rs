//! Keys page: SSH / OTP / API entries from the daemon cache.
//!
//! Secrets are shown for 30 s and nowhere else — the TUI's `SecretMessage`
//! rule. An OTP code is shown inline on its row (large monospace + Copy);
//! an API value goes in a toast. Either is cleared after 30 s, on reload,
//! or when the page is switched away (`clear_codes`). Delete goes through
//! the device's fingerprint gate, so the button shows a "touch" hint while
//! waiting.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use rust_i18n::t;

use immurok_client::keys::{
    capacity, delete_key, get_api, get_otp, list_keys, ssh_public_key_line, KeyCategory, KeyEntry,
};

use super::{confirm, run_blocking};

/// Whether a connection-state update means the daemon's key cache may have
/// changed behind the page's back, so the rows must be re-read.
///
/// Every transition counts, in both directions. Connecting is when the
/// daemon syncs the cache off the device (`sync_ssh_keys`), so the entries
/// on screen are whatever the *previous* device left behind until that
/// lands. Disconnecting is the tail of an unpair — the device reboots and
/// drops the link right after the daemon purged the cache — so the rows are
/// stale there too. `None` is the first update after construction, which
/// already loaded once.
fn cache_may_have_changed(previous: Option<bool>, now: bool) -> bool {
    matches!(previous, Some(p) if p != now)
}

/// Title/subtitle for the clickable empty-state row shown when a category
/// has no entries yet.
fn empty_hint(cat: KeyCategory) -> (String, String) {
    match cat {
        KeyCategory::Otp => (t!("keys.empty.otp.title").to_string(), t!("keys.empty.otp.subtitle").to_string()),
        KeyCategory::Api => (t!("keys.empty.api.title").to_string(), t!("keys.empty.api.subtitle").to_string()),
        KeyCategory::Ssh => (t!("keys.empty.ssh.title").to_string(), t!("keys.empty.ssh.subtitle").to_string()),
    }
}

pub struct KeysPage {
    root: gtk::Widget,
    groups: Vec<(KeyCategory, adw::PreferencesGroup)>,
    add_buttons: Vec<(KeyCategory, gtk::Button)>,
    rows: RefCell<Vec<(adw::PreferencesGroup, adw::ActionRow)>>,
    counts: RefCell<HashMap<KeyCategory, usize>>,
    /// `None` until the first Dashboard poll reports a connection state.
    connected: Cell<Option<bool>>,
    toasts: adw::ToastOverlay,
    /// Rows currently showing an inline OTP/API code: (row, original
    /// subtitle markup, its Copy button), so `clear_codes` can restore them.
    shown: RefCell<Vec<(adw::ActionRow, String, gtk::Button)>>,
    /// Bumped every time a code is shown; a pending 30 s timeout only clears
    /// if the generation still matches, so an old timer firing after a
    /// newer code was shown (or `clear_codes` already ran) is a no-op.
    code_gen: Cell<u32>,
}

impl KeysPage {
    pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self> {
        let page = adw::PreferencesPage::new();
        let mut groups = Vec::new();
        let mut add_buttons = Vec::new();
        for cat in [KeyCategory::Otp, KeyCategory::Api, KeyCategory::Ssh] {
            let g = adw::PreferencesGroup::builder()
                .title(format!("{} (0/{})", cat.label(), capacity(cat)))
                .build();
            let add_tooltip = t!("keys.add_entry", category = cat.label()).to_string();
            let add = gtk::Button::builder()
                .icon_name("list-add-symbolic")
                .valign(gtk::Align::Center)
                .tooltip_text(&add_tooltip)
                .build();
            add.add_css_class("flat");
            g.set_header_suffix(Some(&add));
            page.add(&g);
            groups.push((cat, g));
            add_buttons.push((cat, add));
        }
        let this = Rc::new(Self {
            root: page.upcast(),
            groups,
            add_buttons,
            rows: RefCell::new(Vec::new()),
            counts: RefCell::new(HashMap::new()),
            connected: Cell::new(None),
            toasts: toasts.clone(),
            shown: RefCell::new(Vec::new()),
            code_gen: Cell::new(0),
        });
        for (cat, btn) in &this.add_buttons {
            let cat = *cat;
            let weak = Rc::downgrade(&this);
            btn.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.open_add(cat);
                }
            });
        }
        // The daemon cache changes without the page knowing: a connect-time
        // sync, a key added from the CLI, an unpair purging it. Re-read every
        // time the page comes back into view — a GtkStack unmaps the pages it
        // is not showing, so this fires on each switch into Keys. Without it
        // the rows were whatever was cached at window construction, forever.
        let weak = Rc::downgrade(&this);
        this.root.connect_map(move |_| {
            if let Some(p) = weak.upgrade() {
                p.reload();
            }
        });
        this.refresh_add_buttons();
        this.reload();
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        &self.root
    }

    fn toast(&self, text: &str, secs: u32) {
        let t = adw::Toast::new(text);
        t.set_timeout(secs);
        self.toasts.add_toast(t);
    }

    pub fn reload(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let entries = run_blocking(list_keys).await.unwrap_or_default();
            if let Some(this) = weak.upgrade() {
                this.populate(entries);
            }
        });
    }

    /// Hide every inline OTP code (page switch, reload, expiry).
    pub fn clear_codes(&self) {
        self.code_gen.set(self.code_gen.get().wrapping_add(1));
        for (row, orig, copy) in self.shown.borrow_mut().drain(..) {
            row.set_subtitle(&orig);
            row.remove(&copy);
        }
    }

    /// Show `code` on `row` for 30 s with a Copy button.
    fn show_code(self: &Rc<Self>, row: &adw::ActionRow, code: &str) {
        self.clear_codes();
        let gen = self.code_gen.get();
        let orig = row.subtitle().map(|s| s.to_string()).unwrap_or_default();
        let pretty = if code.len() == 6 { format!("{} {}", &code[..3], &code[3..]) } else { code.to_string() };
        row.set_subtitle(&format!(
            "<span font_family=\"monospace\" size=\"x-large\" weight=\"bold\">{}</span>",
            glib::markup_escape_text(&pretty)
        ));
        let copy_tooltip = t!("keys.copy_code").to_string();
        let copy = gtk::Button::builder().icon_name("edit-copy-symbolic").valign(gtk::Align::Center).tooltip_text(&copy_tooltip).build();
        copy.add_css_class("flat");
        {
            let code = code.to_string();
            let this = Rc::downgrade(self);
            copy.connect_clicked(move |_| {
                if let Some(display) = gtk::gdk::Display::default() {
                    display.clipboard().set_text(&code);
                }
                if let Some(this) = this.upgrade() {
                    this.toast(&t!("keys.code_copied"), 2);
                }
            });
        }
        row.add_suffix(&copy);
        let this = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_secs(30), move || {
            if let Some(this) = this.upgrade() {
                if this.code_gen.get() == gen {
                    this.clear_codes();
                }
            }
        });
        self.shown.borrow_mut().push((row.clone(), orig, copy));
    }

    fn populate(self: &Rc<Self>, entries: Vec<KeyEntry>) {
        self.clear_codes();
        for (group, row) in self.rows.borrow_mut().drain(..) {
            group.remove(&row);
        }
        let mut counts: HashMap<KeyCategory, usize> = HashMap::new();
        for e in &entries {
            *counts.entry(e.category).or_default() += 1;
        }
        for (cat, group) in &self.groups {
            let n = counts.get(cat).copied().unwrap_or(0);
            group.set_title(&format!("{} ({}/{})", cat.label(), n, capacity(*cat)));
            if n == 0 {
                let (t, s) = empty_hint(*cat);
                let row = adw::ActionRow::builder().title(&t).subtitle(&s).activatable(true).build();
                row.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
                let weak = Rc::downgrade(self);
                let cat = *cat;
                row.connect_activated(move |_| {
                    if let Some(this) = weak.upgrade() {
                        this.open_add(cat);
                    }
                });
                group.add(&row);
                self.rows.borrow_mut().push((group.clone(), row));
            }
        }
        *self.counts.borrow_mut() = counts;
        self.refresh_add_buttons();

        for entry in entries {
            let Some((_, group)) = self.groups.iter().find(|(c, _)| *c == entry.category) else { continue };
            let subtitle = match entry.category {
                KeyCategory::Otp => entry.service.clone(),
                KeyCategory::Api => String::new(),
                KeyCategory::Ssh => "ecdsa-sha2-nistp256".to_string(),
            };
            // Title and subtitle are Pango markup (`AdwPreferencesRow`), so a
            // key named `a&b` would otherwise render blank and warn.
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&entry.name))
                .subtitle(glib::markup_escape_text(&subtitle))
                .build();

            let primary_label = match entry.category {
                KeyCategory::Otp => t!("keys.get_code").to_string(),
                KeyCategory::Api => t!("keys.show").to_string(),
                KeyCategory::Ssh => t!("keys.copy_public_key").to_string(),
            };
            let primary = gtk::Button::builder()
                .label(&primary_label)
                .valign(gtk::Align::Center)
                .build();
            let delete_tooltip = t!("keys.delete_tooltip").to_string();
            let delete = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .valign(gtk::Align::Center)
                .tooltip_text(&delete_tooltip)
                .build();
            delete.add_css_class("flat");
            row.add_suffix(&primary);
            row.add_suffix(&delete);
            row.set_activatable_widget(Some(&primary));

            let this = Rc::downgrade(self);
            let e = entry.clone();
            primary.connect_clicked(move |b| {
                let Some(this) = this.upgrade() else { return };
                // Recover the row from the button rather than capturing it:
                // `primary` is a child of `row`, so a captured `row` clone
                // held by this closure (itself owned by `primary`) would be
                // row → button → closure → row, a cycle `group.remove(&row)`
                // never breaks.
                let Some(row) = b.ancestor(adw::ActionRow::static_type()).and_downcast::<adw::ActionRow>() else { return };
                this.primary_action(b.clone(), row, e.clone());
            });
            let this = Rc::downgrade(self);
            let e = entry.clone();
            delete.connect_clicked(move |b| {
                let Some(this) = this.upgrade() else { return };
                this.delete_action(b.clone(), e.clone());
            });

            group.add(&row);
            self.rows.borrow_mut().push((group.clone(), row));
        }
    }

    /// Fed from the Dashboard poll; the add buttons need a live device, and
    /// a changed connection state means the cache behind the rows moved.
    pub fn set_connected(self: &Rc<Self>, connected: bool) {
        let previous = self.connected.replace(Some(connected));
        if previous == Some(connected) {
            return;
        }
        self.refresh_add_buttons();
        if cache_may_have_changed(previous, connected) {
            self.reload();
        }
    }

    fn refresh_add_buttons(&self) {
        let counts = self.counts.borrow();
        for (cat, btn) in &self.add_buttons {
            let n = counts.get(cat).copied().unwrap_or(0);
            let full = n >= capacity(*cat) as usize;
            let (sensitive, tip) = if self.connected.get() != Some(true) {
                (false, t!("keys.device_not_connected").to_string())
            } else if full {
                (false, t!("keys.keystore_full").to_string())
            } else {
                (true, t!("keys.add_entry", category = cat.label()).to_string())
            };
            btn.set_sensitive(sensitive);
            btn.set_tooltip_text(Some(&tip));
        }
    }

    fn open_add(self: &Rc<Self>, cat: KeyCategory) {
        if self.connected.get() != Some(true) {
            self.toast(&t!("keys.device_not_connected"), 3);
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let Some(win) = this.root.root().and_then(|r| r.downcast::<gtk::Window>().ok()) else { return };
            if let Some(name) = crate::key_add_dialog::run(&win, cat).await {
                let escaped_name = glib::markup_escape_text(&name);
                let added = t!("keys.added", category = cat.label(), name = escaped_name.as_str()).to_string();
                this.toast(&added, 3);
                // The daemon re-syncs its cache after a successful write;
                // give it a beat before re-reading.
                glib::timeout_future(Duration::from_millis(500)).await;
                this.reload();
            }
        });
    }

    fn primary_action(self: &Rc<Self>, button: gtk::Button, row: adw::ActionRow, entry: KeyEntry) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            match entry.category {
                KeyCategory::Ssh => {
                    let line = ssh_public_key_line(&entry);
                    if let Some(display) = gtk::gdk::Display::default() {
                        display.clipboard().set_text(&line);
                        this.toast(&t!("keys.public_key_copied"), 3);
                    }
                }
                KeyCategory::Otp | KeyCategory::Api => {
                    let original = button.label().map(|l| l.to_string()).unwrap_or_default();
                    button.set_sensitive(false);
                    button.set_label(&t!("keys.touch_device"));
                    let name = entry.name.clone();
                    let cat = entry.category;
                    let r = run_blocking(move || match cat {
                        KeyCategory::Otp => get_otp(&name),
                        _ => get_api(&name),
                    })
                    .await;
                    button.set_label(&original);
                    button.set_sensitive(true);
                    // `AdwToast:title` is Pango markup; an API value or name
                    // containing `&` / `<` would render blank. The value is
                    // still never logged — only escaped for display.
                    match r {
                        Some(Ok(value)) if entry.category == KeyCategory::Otp => this.show_code(&row, &value),
                        Some(Ok(value)) => this.toast(
                            &format!("{}: {}", glib::markup_escape_text(&entry.name), glib::markup_escape_text(&value)),
                            30,
                        ),
                        Some(Err(e)) => {
                            let escaped = glib::markup_escape_text(&e);
                            this.toast(&t!("keys.read_failed", error = escaped.as_str()), 5)
                        }
                        None => {}
                    }
                }
            }
        });
    }

    fn delete_action(self: &Rc<Self>, button: gtk::Button, entry: KeyEntry) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            let Some(win) = button.root().and_then(|r| r.downcast::<gtk::Window>().ok()) else { return };
            let escaped_name = glib::markup_escape_text(&entry.name);
            let title = t!(
                "keys.delete_confirm_title",
                category = entry.category.label(),
                name = escaped_name.as_str()
            )
            .to_string();
            let body = t!("keys.delete_confirm_body").to_string();
            let delete_label = t!("keys.delete").to_string();
            let ok = confirm(
                &win,
                &title,
                &body,
                &delete_label,
            )
            .await;
            if !ok {
                return;
            }
            button.set_sensitive(false);
            this.toast(&t!("keys.touch_delete"), 30);
            let (cat, idx) = (entry.category, entry.index);
            let r = run_blocking(move || delete_key(cat, idx)).await;
            button.set_sensitive(true);
            match r {
                Some(Ok(())) => {
                    this.toast(&t!("keys.deleted"), 3);
                    // The daemon re-syncs its cache after a successful delete;
                    // give it a beat before re-reading.
                    glib::timeout_future(Duration::from_millis(500)).await;
                    this.reload();
                }
                Some(Err(e)) => {
                    let escaped = glib::markup_escape_text(&e);
                    this.toast(&t!("keys.delete_failed", error = escaped.as_str()), 5)
                }
                None => {}
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::cache_may_have_changed;

    /// The first poll after construction must not re-read: `new()` already
    /// loaded, and reloading would throw away a just-shown OTP code.
    #[test]
    fn first_update_is_not_a_change() {
        assert!(!cache_may_have_changed(None, true));
        assert!(!cache_may_have_changed(None, false));
    }

    /// Connecting is when the daemon syncs the cache off the device — until
    /// then the rows are the previous device's entries, which is exactly the
    /// bug: after relinking to a new device the GUI kept showing the old
    /// device's OTP names even though the daemon cache was already empty.
    #[test]
    fn reconnect_reloads() {
        assert!(cache_may_have_changed(Some(false), true));
    }

    /// Disconnecting is the tail of an unpair (device reboots and drops the
    /// link right after the daemon purged the cache), so re-read there too.
    #[test]
    fn disconnect_reloads() {
        assert!(cache_may_have_changed(Some(true), false));
    }

    #[test]
    fn a_repeated_state_does_not_reload() {
        assert!(!cache_may_have_changed(Some(true), true));
        assert!(!cache_may_have_changed(Some(false), false));
    }
}
