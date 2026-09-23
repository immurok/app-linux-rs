//! Fingerprints page (spec §8): one card per enrolled slot with a local,
//! editable name; "+" to enroll; slot 5 is the fixed "Switch Host" finger.
//! Delete / Test go through the device's fingerprint gate.
//!
//! Connection state is polled here (every 2 s while the page is showing)
//! independently of the Dashboard, so the page reloads when the device
//! comes and goes.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::fingerprint::{fp_delete, fp_list, fp_verify, FpSlots, SWITCH_SLOT};
use immurok_client::hosts::slot_status;
use immurok_client::status::query_status;

use crate::enroll_dialog::{self, EnrollOutcome};
use crate::errors::friendly;
use crate::gate_dialog;
use crate::settings_store;

use super::{confirm, run_blocking};

pub struct FingerprintsPage {
    /// "loading" | "disconnected" | "content"
    root: gtk::Stack,
    flow: gtk::FlowBox,
    add_switch: gtk::Button,
    test_button: gtk::Button,
    refresh_button: gtk::Button,
    toasts: adw::ToastOverlay,
    slots: Cell<FpSlots>,
    hosts_supported: Cell<bool>,
    connected: Cell<bool>,
    /// A gated / enrollment session is running: every action disabled,
    /// polling paused.
    busy: Cell<bool>,
    loading: Cell<bool>,
    have_data: Cell<bool>,
}

/// Our own symbolic fingerprint glyph, bundled as a gresource
/// (`data/icons/…`) so it renders the same under every icon theme —
/// Fluent, for one, claims `fingerprint-symbolic` but draws nothing.
pub fn finger_icon_name() -> &'static str {
    "immurok-fingerprint-symbolic"
}

impl FingerprintsPage {
    pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self> {
        // ── content ──
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .title("Fingerprints")
            .description("Touch the sensor to unlock and authorize sudo.")
            .build();
        let flow = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .max_children_per_line(4)
            .min_children_per_line(2)
            .column_spacing(12)
            .row_spacing(12)
            .homogeneous(true)
            .build();
        group.add(&flow);

        let header_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let add_switch = gtk::Button::builder().label("Add switch fingerprint").valign(gtk::Align::Center).visible(false).build();
        let test_button = gtk::Button::builder().label("Test").valign(gtk::Align::Center).tooltip_text("Verify with an enrolled finger").build();
        let refresh_button = gtk::Button::builder().icon_name("view-refresh-symbolic").valign(gtk::Align::Center).tooltip_text("Refresh").build();
        refresh_button.add_css_class("flat");
        header_actions.append(&add_switch);
        header_actions.append(&test_button);
        header_actions.append(&refresh_button);
        group.set_header_suffix(Some(&header_actions));
        page.add(&group);

        // ── loading / disconnected ──
        let loading = gtk::Box::new(gtk::Orientation::Vertical, 12);
        loading.set_valign(gtk::Align::Center);
        let spinner = gtk::Spinner::new();
        spinner.set_spinning(true);
        loading.append(&spinner);
        loading.append(&gtk::Label::new(Some("Fetching fingerprint info from device...")));

        let disconnected = adw::StatusPage::builder()
            .icon_name("bluetooth-disabled-symbolic")
            .title("Device not connected")
            .description("Connect the device to manage fingerprints.")
            .build();

        let root = gtk::Stack::new();
        root.add_named(&loading, Some("loading"));
        root.add_named(&disconnected, Some("disconnected"));
        root.add_named(&page, Some("content"));
        root.set_visible_child_name("loading");

        let this = Rc::new(Self {
            root,
            flow,
            add_switch,
            test_button,
            refresh_button,
            toasts: toasts.clone(),
            slots: Cell::new(FpSlots::default()),
            hosts_supported: Cell::new(false),
            connected: Cell::new(false),
            busy: Cell::new(false),
            loading: Cell::new(false),
            have_data: Cell::new(false),
        });

        let weak = Rc::downgrade(&this);
        this.add_switch.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.add(SWITCH_SLOT);
            }
        });
        let weak = Rc::downgrade(&this);
        this.test_button.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.test();
            }
        });
        let weak = Rc::downgrade(&this);
        this.refresh_button.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.load();
            }
        });
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    /// Initial load, then a 2 s connection poll while the window is visible
    /// and this page is showing. Call once after construction.
    pub fn start(self: &Rc<Self>, window: &adw::ApplicationWindow) {
        self.load();
        let weak = Rc::downgrade(self);
        let window = window.downgrade();
        glib::timeout_add_local(Duration::from_secs(2), move || {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let Some(window) = window.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if window.is_visible() && this.root.is_mapped() && !this.busy.get() && !this.loading.get() {
                let weak = Rc::downgrade(&this);
                glib::spawn_future_local(async move {
                    let r = run_blocking(query_status).await;
                    let Some(this) = weak.upgrade() else { return };
                    match r {
                        Some(Ok(s)) if s.connected != this.connected.get() => this.load(),
                        Some(Err(_)) if this.connected.get() => this.load(),
                        _ => {}
                    }
                });
            }
            glib::ControlFlow::Continue
        });
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    fn window(&self) -> Option<gtk::Window> {
        self.root.root().and_then(|r| r.downcast::<gtk::Window>().ok())
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.update_buttons();
    }

    fn update_buttons(&self) {
        let slots = self.slots.get();
        let free = self.connected.get() && !self.busy.get() && !self.loading.get();
        self.add_switch.set_visible(self.hosts_supported.get() && !slots.switch_enrolled());
        self.add_switch.set_sensitive(free);
        self.test_button.set_sensitive(free && slots.any());
        self.refresh_button.set_sensitive(!self.busy.get() && !self.loading.get());
        // The "+" card is rebuilt by populate(); flip its sensitivity here.
        if let Some(add) = self.flow.last_child().and_then(|c| c.first_child()) {
            add.set_sensitive(free && slots.first_free_auth_slot().is_some());
        }
    }

    fn load(self: &Rc<Self>) {
        if self.loading.replace(true) {
            return;
        }
        if !self.have_data.get() {
            self.root.set_visible_child_name("loading");
        }
        self.update_buttons();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let r = run_blocking(|| {
                let slots = fp_list();
                let hosts = slot_status().map(|h| h.supported).unwrap_or(false);
                (slots, hosts)
            })
            .await;
            let Some(this) = weak.upgrade() else { return };
            this.loading.set(false);
            match r {
                Some((Ok(slots), hosts)) => {
                    this.connected.set(true);
                    this.have_data.set(true);
                    this.slots.set(slots);
                    this.hosts_supported.set(hosts);
                    this.populate();
                    this.root.set_visible_child_name("content");
                }
                Some((Err(e), _)) => {
                    this.connected.set(false);
                    this.have_data.set(false);
                    if !e.contains("NOT_CONNECTED") {
                        this.toast(&format!("Daemon unavailable: {}", glib::markup_escape_text(&e)));
                    }
                    this.root.set_visible_child_name("disconnected");
                }
                None => this.loading.set(false),
            }
            this.update_buttons();
        });
    }

    fn populate(self: &Rc<Self>) {
        while let Some(child) = self.flow.first_child() {
            self.flow.remove(&child);
        }
        let settings = settings_store::load();
        let slots = self.slots.get();
        for slot in slots.auth_slots() {
            let card = self.finger_card(slot, &settings.fingerprint_name(slot));
            self.flow.insert(&card, -1);
        }
        if slots.switch_enrolled() {
            let card = self.finger_card(SWITCH_SLOT, settings_store::GuiSettings::SWITCH_NAME);
            self.flow.insert(&card, -1);
        }
        self.flow.insert(&self.add_card(), -1);
        self.update_buttons();
    }

    fn finger_card(self: &Rc<Self>, slot: u8, name: &str) -> gtk::Widget {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
        card.add_css_class("card");
        card.set_margin_top(12);
        card.set_margin_bottom(12);
        card.set_margin_start(12);
        card.set_margin_end(12);
        let inner = gtk::Box::new(gtk::Orientation::Vertical, 6);
        inner.set_margin_top(14);
        inner.set_margin_bottom(14);
        inner.set_margin_start(14);
        inner.set_margin_end(14);
        card.append(&inner);

        let icon = gtk::Image::from_icon_name(finger_icon_name());
        icon.set_pixel_size(40);
        icon.add_css_class("accent");
        inner.append(&icon);

        // Name: label ↔ entry (inline rename), except for the switch slot.
        let name_stack = gtk::Stack::new();
        let label = gtk::Label::builder()
            .label(name)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(14)
            .build();
        let entry = gtk::Entry::builder().text(name).max_width_chars(14).build();
        name_stack.add_named(&label, Some("label"));
        name_stack.add_named(&entry, Some("entry"));
        name_stack.set_visible_child_name("label");
        inner.append(&name_stack);

        if slot == SWITCH_SLOT {
            let hint = gtk::Label::builder()
                .label("This finger only switches hosts — it never unlocks or authenticates")
                .wrap(true)
                .justify(gtk::Justification::Center)
                .build();
            hint.add_css_class("dim-label");
            hint.add_css_class("caption");
            inner.append(&hint);
        } else {
            // Every closure below lives inside a widget that the stack /
            // label / entry own (directly or via a controller), so strong
            // clones of them would form a reference cycle and leak one
            // stack + label + entry + controller set per populate() — which
            // runs on Refresh, on every connection flip and after every
            // operation. WeakRef + upgrade() breaks it.
            let click = gtk::GestureClick::new();
            let (ns, en) = (name_stack.downgrade(), entry.downgrade());
            click.connect_released(move |_, _, _, _| {
                let (Some(ns), Some(en)) = (ns.upgrade(), en.upgrade()) else { return };
                ns.set_visible_child_name("entry");
                en.grab_focus();
            });
            label.add_controller(click);

            let weak = Rc::downgrade(self);
            let (ns, lb) = (name_stack.downgrade(), label.downgrade());
            entry.connect_activate(move |e| {
                let text = e.text().to_string();
                if let (Some(p), Some(lb)) = (weak.upgrade(), lb.upgrade()) {
                    let shown = p.rename(slot, &text);
                    lb.set_text(&shown);
                    e.set_text(&shown);
                }
                if let Some(ns) = ns.upgrade() {
                    ns.set_visible_child_name("label");
                }
            });
            let keys = gtk::EventControllerKey::new();
            let (ns, lb, en) = (name_stack.downgrade(), label.downgrade(), entry.downgrade());
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    let (Some(ns), Some(lb), Some(en)) = (ns.upgrade(), lb.upgrade(), en.upgrade())
                    else {
                        return glib::Propagation::Proceed;
                    };
                    en.set_text(&lb.text());
                    ns.set_visible_child_name("label");
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            entry.add_controller(keys);
            let focus = gtk::EventControllerFocus::new();
            let (ns, lb, en) = (name_stack.downgrade(), label.downgrade(), entry.downgrade());
            focus.connect_leave(move |_| {
                let (Some(ns), Some(lb), Some(en)) = (ns.upgrade(), lb.upgrade(), en.upgrade())
                else {
                    return;
                };
                en.set_text(&lb.text());
                ns.set_visible_child_name("label");
            });
            entry.add_controller(focus);
        }

        // Hover-revealed delete button in the top-right corner.
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&card));
        let trash = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .visible(false)
            .tooltip_text("Delete (requires a touch on the device)")
            .build();
        trash.add_css_class("destructive-action");
        trash.add_css_class("circular");
        overlay.add_overlay(&trash);
        let motion = gtk::EventControllerMotion::new();
        let t = trash.clone();
        motion.connect_enter(move |_, _, _| t.set_visible(true));
        let t = trash.clone();
        motion.connect_leave(move |_| t.set_visible(false));
        overlay.add_controller(motion);

        let weak = Rc::downgrade(self);
        // Read the name at click time, not at card-build time: an inline
        // rename that has not triggered a repopulate yet would otherwise
        // put the stale name in the confirmation dialog.
        let built_name = name.to_string();
        let lb = label.downgrade();
        trash.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                let name = lb.upgrade().map(|l| l.text().to_string()).unwrap_or_else(|| built_name.clone());
                p.delete(slot, name);
            }
        });

        overlay.upcast()
    }

    fn add_card(self: &Rc<Self>) -> gtk::Widget {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.set_margin_top(14);
        content.set_margin_bottom(14);
        content.set_margin_start(14);
        content.set_margin_end(14);
        let icon = gtk::Image::from_icon_name("list-add-symbolic");
        icon.set_pixel_size(40);
        content.append(&icon);
        content.append(&gtk::Label::new(Some("Add Fingerprint")));
        let button = gtk::Button::builder().child(&content).build();
        button.add_css_class("card");
        button.add_css_class("flat");
        button.set_margin_top(12);
        button.set_margin_bottom(12);
        button.set_margin_start(12);
        button.set_margin_end(12);
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                if let Some(slot) = p.slots.get().first_free_auth_slot() {
                    p.add(slot);
                }
            }
        });
        button.upcast()
    }

    /// Persist a new name; returns the name that is now displayed.
    fn rename(&self, slot: u8, text: &str) -> String {
        let mut s = settings_store::load();
        s.set_fingerprint_name(slot, text);
        if let Err(e) = settings_store::save(&s) {
            self.toast(&format!("Could not save the name: {}", glib::markup_escape_text(&e)));
        }
        s.fingerprint_name(slot)
    }

    fn add(self: &Rc<Self>, slot: u8) {
        if self.busy.get() {
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let Some(win) = this.window() else { return };
            if this.slots.get().any() {
                let ok = confirm(
                    &win,
                    "Add a New Fingerprint",
                    "The device first asks you to verify with an already-enrolled finger. After that, switch to the NEW finger.",
                    "Start",
                )
                .await;
                if !ok {
                    return;
                }
            }
            this.set_busy(true);
            let outcome = enroll_dialog::run(&win, slot).await;
            this.set_busy(false);
            match outcome {
                EnrollOutcome::Enrolled => {
                    this.toast("Fingerprint enrolled successfully!");
                    this.load();
                }
                EnrollOutcome::Failed => this.load(),
                EnrollOutcome::Cancelled => {}
            }
        });
    }

    fn delete(self: &Rc<Self>, slot: u8, name: String) {
        if self.busy.get() {
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let Some(win) = this.window() else { return };
            let body = if slot == SWITCH_SLOT {
                "After this you can no longer switch between computers by touch."
            } else {
                "This cannot be undone. You will need to enroll it again."
            };
            if !confirm(&win, &format!("Delete \"{name}\"?"), body, "Delete").await {
                return;
            }
            this.set_busy(true);
            let r = gate_dialog::run(&win, &format!("Delete \"{name}\""), "Verify with an enrolled finger", move || {
                fp_delete(slot)
            })
            .await;
            this.set_busy(false);
            match r {
                Some(Ok(())) => {
                    this.toast("Deleted");
                    let mut s = settings_store::load();
                    s.set_fingerprint_name(slot, "");
                    let _ = settings_store::save(&s);
                    this.load();
                }
                Some(Err(e)) => this.toast(&glib::markup_escape_text(&friendly(&e))),
                None => {}
            }
        });
    }

    fn test(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let Some(win) = this.window() else { return };
            this.set_busy(true);
            let r = gate_dialog::run(&win, "Test Fingerprint", "Touch the sensor with an enrolled finger", fp_verify).await;
            this.set_busy(false);
            match r {
                Some(Ok(true)) => this.toast("Fingerprint matched"),
                Some(Ok(false)) => this.toast("No match"),
                Some(Err(e)) => this.toast(&glib::markup_escape_text(&friendly(&e))),
                None => {}
            }
        });
    }
}
