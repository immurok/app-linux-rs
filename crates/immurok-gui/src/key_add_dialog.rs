//! "Add entry" dialog for the Keys page. One window per category:
//!   SSH — name; "Generate on device" or "Import from file…" (P-256 PEM)
//!   OTP — name / service / base32 secret; pasting an otpauth:// URI into
//!         the secret field fills all three
//!   API — name / value
//! Every write goes through the device's fingerprint gate: the primary
//! button turns into "Touch the device…" until the daemon answers. Errors
//! stay in the dialog (red label, daemon text verbatim) so the user can fix
//! the input and retry.
//!
//! Every persistent `connect_clicked`/`connect_changed` closure attached to
//! one of the dialog's own widgets captures a `Weak<Shell>`, not an
//! `Rc<Shell>` — `Shell` owns (via its widget tree, `Shell.window`) the very
//! buttons those closures are attached to, so a strong capture there is a
//! cycle that would leak the whole dialog on every open. The `run` future
//! holds the one strong `Rc<Shell>` for the dialog's lifetime; each handler
//! upgrades at call time and bails out if the dialog has already gone away.
//! A short-lived strong clone inside a `glib::spawn_future_local` block is
//! fine — it only outlives the single in-flight request.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::keys::{add_api, add_otp, generate_ssh, import_ssh, KeyCategory};
use immurok_client::keys_import::{base32_decode, otpauth_secret_field, parse_otp_import_file, parse_otpauth_uri, OtpEntry};
use immurok_common::protocol::{NAME_LEN_API, NAME_LEN_OTP, NAME_LEN_SSH, SERVICE_LEN_OTP};

use crate::pages::run_blocking;

fn entry_row(group: &adw::PreferencesGroup, title: &str, max_bytes: Option<usize>, password: bool) -> gtk::Editable {
    let row = adw::ActionRow::builder().title(title).build();
    let editable: gtk::Editable = if password {
        gtk::PasswordEntry::builder().show_peek_icon(true).activates_default(true).valign(gtk::Align::Center).hexpand(true).build().upcast()
    } else {
        gtk::Entry::builder().activates_default(true).valign(gtk::Align::Center).hexpand(true).build().upcast()
    };
    if let Some(max) = max_bytes {
        // Firmware fields are byte-sized; GTK's max-length counts chars, so
        // clamp on change instead. Use the signal's own `&Editable` argument
        // rather than a captured clone — a clone stored in the widget's own
        // handler list would be a GObject self-cycle that never finalizes.
        editable.connect_changed(move |e| {
            let text = e.text();
            if text.len() > max {
                let mut cut = max;
                while cut > 0 && !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                e.set_text(&text[..cut]);
                e.set_position(-1);
            }
        });
    }
    row.add_suffix(&editable);
    row.set_activatable_widget(Some(&editable));
    group.add(&row);
    editable
}

struct Shell {
    window: gtk::Window,
    group: adw::PreferencesGroup,
    error: gtk::Label,
    primary: gtk::Button,
    secondary: gtk::Button,
    cancel: gtk::Button,
    /// Set while a write is in flight. Checked by the window's
    /// `close-request` handler so the WM close button can't tear the window
    /// down out from under a request that hasn't answered yet.
    busy: Cell<bool>,
}

fn shell(parent: &impl IsA<gtk::Window>, title: &str, primary: &str, secondary: Option<&str>) -> Shell {
    let window = gtk::Window::builder()
        .transient_for(parent)
        .destroy_with_parent(true)
        .modal(true)
        .resizable(false)
        .title(title)
        .default_width(460)
        .build();
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    page.add(&group);

    let error = gtk::Label::builder().wrap(true).visible(false).xalign(0.0).margin_start(24).margin_end(24).build();
    error.add_css_class("error");

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.set_margin_end(24);
    buttons.set_margin_bottom(18);
    buttons.set_margin_top(6);
    let cancel = gtk::Button::builder().label("Cancel").build();
    let secondary_btn = gtk::Button::builder().label(secondary.unwrap_or("")).visible(secondary.is_some()).build();
    let primary_btn = gtk::Button::builder().label(primary).build();
    primary_btn.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&secondary_btn);
    buttons.append(&primary_btn);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&page);
    root.append(&error);
    root.append(&buttons);
    window.set_child(Some(&root));
    window.set_default_widget(Some(&primary_btn));

    {
        let w = window.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.close();
            }
        });
    }
    Shell { window, group, error, primary: primary_btn, secondary: secondary_btn, cancel, busy: Cell::new(false) }
}

impl Shell {
    fn show_error(&self, text: &str) {
        self.error.set_text(text);
        self.error.set_visible(true);
    }
    fn busy(&self, on: bool, label_idle: &str) {
        self.busy.set(on);
        self.primary.set_sensitive(!on);
        self.secondary.set_sensitive(!on);
        self.cancel.set_sensitive(!on);
        self.primary.set_label(if on { "Touch the device…" } else { label_idle });
        if on {
            self.error.set_visible(false);
        }
    }
    /// Close the window after a successful write. Must clear `busy` first —
    /// `window.close()` emits `close-request`, and that handler refuses to
    /// close while `busy` is set (see `run`), which would otherwise leave
    /// the dialog stuck open and insensitive forever after success.
    fn finish(&self) {
        self.busy.set(false);
        self.window.close();
    }
}

fn name_of(e: &gtk::Editable) -> String {
    e.text().trim().to_string()
}

/// Run the dialog; resolves when it closes. `Some(name)` if something was
/// written (the bulk OTP file import has no single name, so it returns e.g.
/// `"3 entries"`); `None` if the dialog was cancelled or closed without a
/// write.
pub async fn run(parent: &impl IsA<gtk::Window>, cat: KeyCategory) -> Option<String> {
    let written: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let (done_tx, done_rx) = async_channel::bounded::<()>(1);

    let sh = match cat {
        KeyCategory::Ssh => shell(parent, "Add SSH key", "Generate on device", Some("Import from file…")),
        KeyCategory::Otp => shell(parent, "Add OTP entry", "Add", Some("Import from file…")),
        KeyCategory::Api => shell(parent, "Add API key", "Add", None),
    };
    let sh = Rc::new(sh);
    // The only strong `Rc<Shell>` that lives for the dialog's whole lifetime
    // is this local `sh` (held by this `run` future until it returns).
    // Every closure attached to one of `sh`'s own widgets below captures
    // this `Weak` instead and upgrades at call time — see the module doc
    // comment for why a strong capture there would be a leak.
    let sh_weak = Rc::downgrade(&sh);
    {
        let (tx, sh_weak) = (done_tx.clone(), sh_weak.clone());
        sh.window.connect_close_request(move |_| {
            let Some(sh2) = sh_weak.upgrade() else { return glib::Propagation::Proceed };
            // A write is in flight: the button that started it is already
            // insensitive (see `busy`), and letting the WM close button tear
            // the window down here would make the pending write finish
            // silently — its future would later call back into a window
            // nobody's watching anymore, with no toast and no reload.
            if sh2.busy.get() {
                return glib::Propagation::Stop;
            }
            let _ = tx.try_send(());
            glib::Propagation::Proceed
        });
    }
    {
        // `destroy_with_parent` windows never emit `close-request` when the
        // parent goes away — only `destroy` — so without this, `run` would
        // hang forever if the main window closes while this dialog is open.
        let tx = done_tx.clone();
        sh.window.connect_destroy(move |_| {
            let _ = tx.try_send(());
        });
    }

    let name = entry_row(
        &sh.group,
        "Name",
        Some(match cat {
            KeyCategory::Ssh => NAME_LEN_SSH - 1,
            KeyCategory::Otp => NAME_LEN_OTP - 1,
            KeyCategory::Api => NAME_LEN_API - 1,
        }),
        false,
    );

    match cat {
        KeyCategory::Ssh => {
            sh.group.set_description(Some("ECDSA P-256. The private key never leaves the device."));
            // Generate
            {
                let (sh_weak, name, written, tx) = (sh_weak.clone(), name.clone(), written.clone(), done_tx.clone());
                sh.primary.connect_clicked(move |_| {
                    let Some(sh2) = sh_weak.upgrade() else { return };
                    let n = name_of(&name);
                    if n.is_empty() {
                        sh2.show_error("Name cannot be empty.");
                        return;
                    }
                    sh2.busy(true, "Generate on device");
                    let (sh3, written, tx) = (sh2.clone(), written.clone(), tx.clone());
                    let name_for_result = n.clone();
                    glib::spawn_future_local(async move {
                        match run_blocking(move || generate_ssh(&n)).await {
                            Some(Ok(())) => {
                                *written.borrow_mut() = Some(name_for_result);
                                let _ = tx.try_send(());
                                sh3.finish();
                            }
                            Some(Err(e)) => {
                                sh3.busy(false, "Generate on device");
                                sh3.show_error(&format!("Generate failed: {}", e));
                            }
                            None => sh3.busy(false, "Generate on device"),
                        }
                    });
                });
            }
            // Import
            {
                let (sh_weak, name, written, tx) = (sh_weak.clone(), name.clone(), written.clone(), done_tx.clone());
                sh.secondary.connect_clicked(move |_| {
                    let Some(sh2) = sh_weak.upgrade() else { return };
                    let n = name_of(&name);
                    if n.is_empty() {
                        sh2.show_error("Name cannot be empty.");
                        return;
                    }
                    let (sh3, written, tx) = (sh2.clone(), written.clone(), tx.clone());
                    glib::spawn_future_local(async move {
                        let pem = match pick_text_file(&sh3.window, "Choose a private key file").await {
                            None => return,
                            Some(Err(e)) => {
                                sh3.show_error(&e);
                                return;
                            }
                            Some(Ok(pem)) => pem,
                        };
                        sh3.busy(true, "Generate on device");
                        let name_for_result = n.clone();
                        match run_blocking(move || import_ssh(&n, &pem)).await {
                            Some(Ok(())) => {
                                *written.borrow_mut() = Some(name_for_result);
                                let _ = tx.try_send(());
                                sh3.finish();
                            }
                            Some(Err(e)) => {
                                sh3.busy(false, "Generate on device");
                                sh3.show_error(&format!("Import failed: {}", e));
                            }
                            None => sh3.busy(false, "Generate on device"),
                        }
                    });
                });
            }
        }
        KeyCategory::Otp => {
            sh.group.set_description(Some("TOTP, SHA-1, 6 digits, 30 s. Paste an otpauth:// URI into Secret to fill everything."));
            let service = entry_row(&sh.group, "Service", Some(SERVICE_LEN_OTP - 1), false);
            let secret = entry_row(&sh.group, "Secret", None, true);
            // otpauth:// auto-fill
            {
                let (name, service, sh_weak) = (name.clone(), service.clone(), sh_weak.clone());
                // Use the signal's own `&Editable` argument for the widget
                // this closure is attached to (`secret`) rather than a
                // captured clone — a clone stored in the widget's own
                // handler list would be a GObject self-cycle that never
                // finalizes.
                secret.connect_changed(move |secret2| {
                    let t = secret2.text();
                    if !t.trim_start().starts_with("otpauth://") {
                        return;
                    }
                    let Some(sh2) = sh_weak.upgrade() else { return };
                    match parse_otpauth_uri(t.trim()) {
                        Some(e) => {
                            name.set_text(&e.name);
                            service.set_text(&e.service);
                            // `parse_otpauth_uri` re-derives the secret bytes
                            // for validation only; put back the percent-decoded
                            // `secret=` value itself so odd-length base32 with
                            // padding round-trips exactly.
                            let raw = otpauth_secret_field(t.trim()).unwrap_or_default();
                            secret2.set_text(&raw);
                            sh2.error.set_visible(false);
                        }
                        None => sh2.show_error("Unsupported otpauth URI: only TOTP / SHA1 / 6 digits / 30 s."),
                    }
                });
            }
            {
                let (sh_weak, written, tx) = (sh_weak.clone(), written.clone(), done_tx.clone());
                sh.primary.connect_clicked(move |_| {
                    let Some(sh2) = sh_weak.upgrade() else { return };
                    let n = name_of(&name);
                    if n.is_empty() {
                        sh2.show_error("Name cannot be empty.");
                        return;
                    }
                    let Some(sec) = base32_decode(&secret.text()).filter(|b| !b.is_empty()) else {
                        sh2.show_error("Invalid base32 secret — check for typos.");
                        return;
                    };
                    let entry = OtpEntry { name: n, service: name_of(&service), secret: sec };
                    let name_for_result = entry.name.clone();
                    sh2.busy(true, "Add");
                    let (sh3, written, tx) = (sh2.clone(), written.clone(), tx.clone());
                    glib::spawn_future_local(async move {
                        match run_blocking(move || add_otp(&entry)).await {
                            Some(Ok(())) => {
                                *written.borrow_mut() = Some(name_for_result);
                                let _ = tx.try_send(());
                                sh3.finish();
                            }
                            Some(Err(e)) => {
                                sh3.busy(false, "Add");
                                sh3.show_error(&format!("Add failed: {}", e));
                            }
                            None => sh3.busy(false, "Add"),
                        }
                    });
                });
            }
            // "Import from file…": andOTP JSON / otpauth CSV bulk import.
            {
                let (sh_weak, written, tx) = (sh_weak.clone(), written.clone(), done_tx.clone());
                sh.secondary.connect_clicked(move |_| {
                    let Some(sh2) = sh_weak.upgrade() else { return };
                    let (sh3, written, tx) = (sh2.clone(), written.clone(), tx.clone());
                    glib::spawn_future_local(async move {
                        let chooser = gtk::FileChooserNative::new(
                            Some("Choose an OTP export (andOTP .json or otpauth .csv)"),
                            Some(&sh3.window),
                            gtk::FileChooserAction::Open,
                            Some("Open"),
                            Some("Cancel"),
                        );
                        chooser.set_modal(true);
                        if chooser.run_future().await != gtk::ResponseType::Accept {
                            return;
                        }
                        let Some(path) = chooser.file().and_then(|f| f.path()) else { return };
                        let content = match std::fs::read_to_string(&path) {
                            Ok(c) => c,
                            Err(e) => {
                                sh3.show_error(&format!("Cannot read file: {}", e));
                                return;
                            }
                        };
                        let (entries, skipped) = match parse_otp_import_file(&path.to_string_lossy(), &content) {
                            Ok(v) => v,
                            Err(e) => {
                                sh3.show_error(&e);
                                return;
                            }
                        };
                        // Capacity: count what the daemon has cached right now.
                        let used = run_blocking(|| immurok_client::keys::list_keys().into_iter().filter(|e| e.category == KeyCategory::Otp).count())
                            .await
                            .unwrap_or(0);
                        let free = (immurok_client::keys::capacity(KeyCategory::Otp) as usize).saturating_sub(used);
                        if entries.len() > free {
                            sh3.show_error(&format!(
                                "Cannot import {} entries: only {} OTP slots remaining. Delete some entries first.",
                                entries.len(),
                                free
                            ));
                            return;
                        }
                        let skip_note = if skipped > 0 {
                            format!(" ({} skipped: only TOTP / SHA1 / 6-digit / 30 s supported)", skipped)
                        } else {
                            String::new()
                        };
                        let ok = crate::pages::confirm(
                            &sh3.window,
                            &format!("Import {} OTP entr{}?", entries.len(), if entries.len() == 1 { "y" } else { "ies" }),
                            &format!("Touch the device when asked.{}", skip_note),
                            "Import",
                        )
                        .await;
                        if !ok {
                            return;
                        }
                        sh3.busy(true, "Add");
                        let total = entries.len();
                        let mut imported = 0;
                        let mut failure: Option<String> = None;
                        for (i, entry) in entries.into_iter().enumerate() {
                            sh3.primary.set_label(&format!("Importing {}/{}: {}", i + 1, total, entry.name));
                            match run_blocking(move || add_otp(&entry)).await {
                                Some(Ok(())) => imported += 1,
                                Some(Err(e)) => {
                                    failure = Some(e);
                                    break;
                                }
                                None => break,
                            }
                        }
                        if imported > 0 {
                            *written.borrow_mut() = Some(format!("{} entries", imported));
                        }
                        match failure {
                            None if imported == total => {
                                let _ = tx.try_send(());
                                sh3.finish();
                            }
                            None => {
                                sh3.busy(false, "Add");
                                sh3.show_error(&format!("Imported {}/{} entries.", imported, total));
                            }
                            Some(e) => {
                                sh3.busy(false, "Add");
                                sh3.show_error(&format!("Imported {}/{} entries; stopped at: {}", imported, total, e));
                            }
                        }
                    });
                });
            }
        }
        KeyCategory::Api => {
            let value = entry_row(&sh.group, "Value", None, true);
            let (sh_weak, written, tx) = (sh_weak.clone(), written.clone(), done_tx.clone());
            sh.primary.connect_clicked(move |_| {
                let Some(sh2) = sh_weak.upgrade() else { return };
                let n = name_of(&name);
                if n.is_empty() {
                    sh2.show_error("Name cannot be empty.");
                    return;
                }
                let v = value.text().to_string();
                sh2.busy(true, "Add");
                let (sh3, written, tx) = (sh2.clone(), written.clone(), tx.clone());
                let name_for_result = n.clone();
                glib::spawn_future_local(async move {
                    match run_blocking(move || add_api(&n, &v)).await {
                        Some(Ok(())) => {
                            *written.borrow_mut() = Some(name_for_result);
                            let _ = tx.try_send(());
                            sh3.finish();
                        }
                        Some(Err(e)) => {
                            sh3.busy(false, "Add");
                            sh3.show_error(&format!("Add failed: {}", e));
                        }
                        None => sh3.busy(false, "Add"),
                    }
                });
            });
        }
    }

    sh.window.present();
    let _ = done_rx.recv().await;
    written.take()
}

/// Native file chooser → file contents as text. `None` if the user
/// cancelled; `Some(Err(_))` if a file was chosen but could not be read
/// (caller shows this in the dialog, since a cancel and a read failure need
/// different handling); `Some(Ok(text))` otherwise.
pub async fn pick_text_file(parent: &gtk::Window, title: &str) -> Option<Result<String, String>> {
    let chooser = gtk::FileChooserNative::new(Some(title), Some(parent), gtk::FileChooserAction::Open, Some("Open"), Some("Cancel"));
    chooser.set_modal(true);
    if chooser.run_future().await != gtk::ResponseType::Accept {
        return None;
    }
    let path = chooser.file().and_then(|f| f.path())?;
    Some(std::fs::read_to_string(path).map_err(|e| format!("Cannot read file: {}", e)))
}
