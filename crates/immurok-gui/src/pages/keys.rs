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

use immurok_client::keys::{
    capacity, delete_key, get_api, get_otp, list_keys, ssh_public_key_line, KeyCategory, KeyEntry,
};

use super::{confirm, run_blocking};

/// Title/subtitle for the clickable empty-state row shown when a category
/// has no entries yet.
fn empty_hint(cat: KeyCategory) -> (&'static str, &'static str) {
    match cat {
        KeyCategory::Otp => ("Add your first OTP entry", "Generates a 6-digit code after you touch the device"),
        KeyCategory::Api => ("Add your first API key", "Shows the stored value after you touch the device"),
        KeyCategory::Ssh => ("Add your first SSH key", "Public key can be copied; signing goes through the SSH agent"),
    }
}

pub struct KeysPage {
    root: gtk::Widget,
    groups: Vec<(KeyCategory, adw::PreferencesGroup)>,
    add_buttons: Vec<(KeyCategory, gtk::Button)>,
    rows: RefCell<Vec<(adw::PreferencesGroup, adw::ActionRow)>>,
    counts: RefCell<HashMap<KeyCategory, usize>>,
    connected: Cell<bool>,
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
            let add = gtk::Button::builder()
                .icon_name("list-add-symbolic")
                .valign(gtk::Align::Center)
                .tooltip_text(format!("Add {} entry", cat.label()))
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
            connected: Cell::new(false),
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
        let copy = gtk::Button::builder().icon_name("edit-copy-symbolic").valign(gtk::Align::Center).tooltip_text("Copy code").build();
        copy.add_css_class("flat");
        {
            let code = code.to_string();
            let this = Rc::downgrade(self);
            copy.connect_clicked(move |_| {
                if let Some(display) = gtk::gdk::Display::default() {
                    display.clipboard().set_text(&code);
                }
                if let Some(this) = this.upgrade() {
                    this.toast("Code copied", 2);
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
                let row = adw::ActionRow::builder().title(t).subtitle(s).activatable(true).build();
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

            let primary = gtk::Button::builder()
                .label(match entry.category {
                    KeyCategory::Otp => "Get code",
                    KeyCategory::Api => "Show",
                    KeyCategory::Ssh => "Copy public key",
                })
                .valign(gtk::Align::Center)
                .build();
            let delete = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .valign(gtk::Align::Center)
                .tooltip_text("Delete (requires a touch on the device)")
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

    /// Fed from the Dashboard poll; the add buttons need a live device.
    pub fn set_connected(&self, connected: bool) {
        if self.connected.replace(connected) != connected {
            self.refresh_add_buttons();
        }
    }

    fn refresh_add_buttons(&self) {
        let counts = self.counts.borrow();
        for (cat, btn) in &self.add_buttons {
            let n = counts.get(cat).copied().unwrap_or(0);
            let full = n >= capacity(*cat) as usize;
            let (sensitive, tip) = if !self.connected.get() {
                (false, "Device not connected".to_string())
            } else if full {
                (false, "Keystore full — delete an entry first".to_string())
            } else {
                (true, format!("Add {} entry", cat.label()))
            };
            btn.set_sensitive(sensitive);
            btn.set_tooltip_text(Some(&tip));
        }
    }

    fn open_add(self: &Rc<Self>, cat: KeyCategory) {
        if !self.connected.get() {
            self.toast("Device not connected", 3);
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let Some(win) = this.root.root().and_then(|r| r.downcast::<gtk::Window>().ok()) else { return };
            if let Some(name) = crate::key_add_dialog::run(&win, cat).await {
                this.toast(&format!("{} '{}' added", cat.label(), glib::markup_escape_text(&name)), 3);
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
                        this.toast("Public key copied", 3);
                    }
                }
                KeyCategory::Otp | KeyCategory::Api => {
                    let original = button.label().map(|l| l.to_string()).unwrap_or_default();
                    button.set_sensitive(false);
                    button.set_label("Touch the device…");
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
                        Some(Err(e)) => this.toast(&format!("Read failed: {}", glib::markup_escape_text(&e)), 5),
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
            let ok = confirm(
                &win,
                &format!("Delete {} \"{}\"?", entry.category.label(), entry.name),
                "This requires a touch on the device and cannot be undone.",
                "Delete",
            )
            .await;
            if !ok {
                return;
            }
            button.set_sensitive(false);
            this.toast("Touch the device to confirm deletion…", 30);
            let (cat, idx) = (entry.category, entry.index);
            let r = run_blocking(move || delete_key(cat, idx)).await;
            button.set_sensitive(true);
            match r {
                Some(Ok(())) => {
                    this.toast("Deleted", 3);
                    // The daemon re-syncs its cache after a successful delete;
                    // give it a beat before re-reading.
                    glib::timeout_future(Duration::from_millis(500)).await;
                    this.reload();
                }
                Some(Err(e)) => this.toast(&format!("Delete failed: {}", glib::markup_escape_text(&e)), 5),
                None => {}
            }
        });
    }
}
