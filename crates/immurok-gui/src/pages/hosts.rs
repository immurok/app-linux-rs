//! "Two Hosts" group on the Device page (spec §11): one card per host slot,
//! Pair / Unpair on this computer's slot, gated Unbind on the other one.
//! Firmware without dual-host support (or a disconnected device) falls back
//! to a single Pair / Unpair button.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::hosts::{clear_other_slot, clear_own_slot, pair_progress, pair_start, HostSlots};
use immurok_client::status::DeviceStatus;
use immurok_common::types::PairProgress;

use crate::errors::friendly;
use crate::gate_dialog;
use crate::settings_store;

use super::{confirm, run_blocking};

// Shown on the other computer's slot card when there is no button to offer:
// the slot is empty, or it is taken and this computer is not paired yet (the
// firmware refuses SLOT:CLEAR:<n> from an unpaired host).
const EMPTY_SLOT_HINT: &str = "To fill this slot, open immurok on that computer and click Pair.";
const FOREIGN_SLOT_HINT: &str =
    "Bound to another computer. Pair this computer first to manage this slot.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Pair,
    Unpair,
    Unbind(u8),
}

struct HostCard {
    root: gtk::Box,
    icon: gtk::Image,
    badge: gtk::Label,
    state: gtk::Label,
    action: gtk::Button,
    other_hint: gtk::Label,
    pending: Cell<Option<Action>>,
}

impl HostCard {
    fn build(n: u8) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.add_css_class("card");
        root.set_hexpand(true);
        let inner = gtk::Box::new(gtk::Orientation::Vertical, 6);
        inner.set_margin_top(14);
        inner.set_margin_bottom(14);
        inner.set_margin_start(14);
        inner.set_margin_end(14);
        root.append(&inner);

        let icon = gtk::Image::from_icon_name("computer-symbolic");
        icon.set_pixel_size(36);
        let title = gtk::Label::new(Some(&format!("Host {n}")));
        title.add_css_class("heading");
        let badge = gtk::Label::builder().label("This computer").visible(false).build();
        badge.add_css_class("caption");
        badge.add_css_class("accent");
        let state = gtk::Label::new(Some("○ Empty"));
        state.add_css_class("dim-label");
        let action = gtk::Button::builder().visible(false).halign(gtk::Align::Center).build();
        let other_hint = gtk::Label::builder()
            .label(EMPTY_SLOT_HINT)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .visible(false)
            .build();
        other_hint.add_css_class("dim-label");
        other_hint.add_css_class("caption");
        inner.append(&icon);
        inner.append(&title);
        inner.append(&badge);
        inner.append(&state);
        inner.append(&action);
        inner.append(&other_hint);
        Self { root, icon, badge, state, action, other_hint, pending: Cell::new(None) }
    }

    fn show(&self, bound: bool, mine: bool, this_computer: bool) {
        self.badge.set_visible(this_computer);
        self.state.set_text(if bound { "● Bound" } else { "○ Empty" });
        if bound {
            self.icon.add_css_class("accent");
        } else {
            self.icon.remove_css_class("accent");
        }
        let action = match (mine, bound) {
            (true, true) => Some(Action::Unpair),
            (true, false) => Some(Action::Pair),
            (false, true) => None, // filled in by caller with the slot number
            (false, false) => None,
        };
        self.pending.set(action);
        self.action.set_visible(action.is_some());
        self.other_hint.set_text(EMPTY_SLOT_HINT);
        self.other_hint.set_visible(!mine && !bound);
        if let Some(a) = action {
            self.action.set_label(match a {
                Action::Pair => "Pair",
                Action::Unpair => "Unpair",
                Action::Unbind(_) => "Unbind",
            });
        }
    }

    /// The other computer's slot on a host that has nothing to offer for it:
    /// no button, an explanatory line instead.
    fn show_foreign(&self) {
        self.pending.set(None);
        self.action.set_visible(false);
        self.other_hint.set_text(FOREIGN_SLOT_HINT);
        self.other_hint.set_visible(true);
    }

    fn show_unbind(&self, n: u8) {
        self.pending.set(Some(Action::Unbind(n)));
        self.action.set_label("Unbind");
        self.action.set_visible(true);
        self.other_hint.set_visible(false);
    }
}

pub struct HostsGroup {
    group: adw::PreferencesGroup,
    /// "simple" (label + one button) | "cards"
    stack: gtk::Stack,
    simple_label: gtk::Label,
    simple_button: gtk::Button,
    cards: [HostCard; 2],
    hint: gtk::Label,
    progress: gtk::Label,
    toasts: adw::ToastOverlay,
    paired: Cell<bool>,
    connected: Cell<bool>,
    slots: Cell<Option<HostSlots>>,
    busy: Cell<bool>,
    simple_pending: Cell<Option<Action>>,
}

impl HostsGroup {
    pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self> {
        let group = adw::PreferencesGroup::builder().title("Two Hosts").build();

        let simple = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let simple_label = gtk::Label::builder().wrap(true).xalign(0.0).build();
        simple_label.add_css_class("dim-label");
        let simple_button = gtk::Button::builder().label("Pair").halign(gtk::Align::Start).visible(false).build();
        simple.append(&simple_label);
        simple.append(&simple_button);

        let cards_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        cards_box.set_homogeneous(true);
        let cards = [HostCard::build(1), HostCard::build(2)];
        cards_box.append(&cards[0].root);
        cards_box.append(&cards[1].root);

        let stack = gtk::Stack::new();
        // Homogeneous-by-default would size the stack to the taller "cards"
        // child even while showing "simple" (disconnected text + Unpair),
        // leaving ~110px of blank space above the Firmware group.
        stack.set_vhomogeneous(false);
        stack.add_named(&simple, Some("simple"));
        stack.add_named(&cards_box, Some("cards"));
        stack.set_visible_child_name("simple");

        let hint = gtk::Label::builder().wrap(true).xalign(0.0).build();
        hint.add_css_class("dim-label");
        hint.add_css_class("caption");
        let progress = gtk::Label::builder().wrap(true).xalign(0.0).visible(false).build();
        progress.add_css_class("accent");

        let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
        column.append(&stack);
        column.append(&progress);
        column.append(&hint);
        group.add(&column);

        let this = Rc::new(Self {
            group,
            stack,
            simple_label,
            simple_button,
            cards,
            hint,
            progress,
            toasts: toasts.clone(),
            paired: Cell::new(false),
            connected: Cell::new(false),
            slots: Cell::new(None),
            busy: Cell::new(false),
            simple_pending: Cell::new(None),
        });

        for (i, card) in this.cards.iter().enumerate() {
            let weak = Rc::downgrade(&this);
            card.action.connect_clicked(move |_| {
                if let Some(g) = weak.upgrade() {
                    if let Some(a) = g.cards[i].pending.get() {
                        g.run(a);
                    }
                }
            });
        }
        let weak = Rc::downgrade(&this);
        this.simple_button.connect_clicked(move |_| {
            if let Some(g) = weak.upgrade() {
                if let Some(a) = g.simple_pending.get() {
                    g.run(a);
                }
            }
        });
        this.simple_label.set_text("Connecting to daemon…");
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.group.upcast_ref()
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    fn window(&self) -> Option<gtk::Window> {
        self.group.root().and_then(|r| r.downcast::<gtk::Window>().ok())
    }

    /// Feed the Dashboard's poll result. Skipped while an operation is
    /// running so a tick cannot stomp the buttons mid-flight.
    pub fn apply(&self, status: &DeviceStatus, paired: bool, slots: Option<HostSlots>) {
        self.paired.set(paired);
        self.connected.set(status.connected);
        self.slots.set(slots);
        if self.busy.get() {
            return;
        }
        self.render();
    }

    pub fn apply_daemon_down(&self) {
        self.connected.set(false);
        self.slots.set(None);
        if self.busy.get() {
            return;
        }
        self.stack.set_visible_child_name("simple");
        self.simple_label.set_text("Daemon unavailable.");
        self.simple_button.set_visible(false);
        self.hint.set_text("");
    }

    fn render(&self) {
        let paired = self.paired.get();
        let connected = self.connected.get();
        let slots = self.slots.get();

        let simple_mode = |label: &str, action: Option<Action>| {
            self.stack.set_visible_child_name("simple");
            self.simple_label.set_text(label);
            self.simple_pending.set(action);
            self.simple_button.set_visible(action.is_some());
            self.simple_button.set_label(match action {
                Some(Action::Unpair) => "Unpair",
                _ => "Pair",
            });
            self.hint.set_text("");
        };

        if !connected {
            simple_mode(
                "Device not connected. Host binding status will appear once connected.",
                paired.then_some(Action::Unpair),
            );
            return;
        }
        let Some(slots) = slots else {
            simple_mode(
                "Host status unavailable.",
                Some(if paired { Action::Unpair } else { Action::Pair }),
            );
            return;
        };
        if !slots.supported {
            simple_mode(
                "This firmware does not support two hosts.",
                Some(if paired { Action::Unpair } else { Action::Pair }),
            );
            return;
        }

        // "This computer" is ONLY ever `slots.mine` — the slot the daemon
        // proved by challenge-response (`parse_slot_owner`: never guess).
        // Paired but unproven (the device is presenting the other slot, or
        // the challenge failed) used to fall back to `active`, which labels
        // a foreign — possibly empty — slot as ours and offers Unbind on
        // our own pairing.
        if paired && slots.mine.is_none() {
            simple_mode("Could not verify which host slot belongs to this computer.", Some(Action::Unpair));
            return;
        }
        // Not paired: the slot the device is presenting is where pairing
        // will land, so that card gets Pair. The other card never gets
        // Unbind — the firmware refuses SLOT:CLEAR:<n> from an unpaired
        // host — so it keeps the "pair from that computer" hint.
        let target = slots.mine.unwrap_or(slots.active);
        for (i, card) in self.cards.iter().enumerate() {
            let n = (i + 1) as u8;
            let bound = slots.bound(n);
            let mine = n == target;
            card.show(bound, mine, slots.mine == Some(n));
            if !mine && bound {
                if paired {
                    card.show_unbind(n);
                } else {
                    card.show_foreign();
                }
            }
        }
        self.stack.set_visible_child_name("cards");
        self.hint.set_text(if slots.both_bound() {
            "Both host slots are in use. To swap one out, unbind it on that computer first."
        } else {
            "This device can be bound to up to two computers. Touch the switch fingerprint to move between them."
        });
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        for c in &self.cards {
            c.action.set_sensitive(!busy);
        }
        self.simple_button.set_sensitive(!busy);
        if !busy {
            self.render();
        }
    }

    fn run(self: &Rc<Self>, action: Action) {
        if self.busy.get() {
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let Some(win) = this.window() else { return };
            match action {
                Action::Unpair => this.unpair(&win).await,
                Action::Pair => this.pair().await,
                Action::Unbind(n) => this.unbind(&win, n).await,
            }
        });
    }

    async fn unpair(self: &Rc<Self>, win: &gtk::Window) {
        if !confirm(win, "Unpair from this computer?", "Fingerprints and keys stay on the device.", "Unpair").await {
            return;
        }
        self.set_busy(true);
        let r = run_blocking(clear_own_slot).await;
        match r {
            Some(Ok(())) => {
                self.toast("Unpaired");
                let mut s = settings_store::load();
                s.clear_fingerprint_names();
                let _ = settings_store::save(&s);
            }
            Some(Err(e)) => self.toast(&format!("Unpair failed: {}", glib::markup_escape_text(&friendly(&e)))),
            None => {}
        }
        self.set_busy(false);
    }

    async fn pair(self: &Rc<Self>) {
        self.set_busy(true);
        self.progress.set_text("Waiting for the device…");
        self.progress.set_visible(true);
        self.toast("Confirm pairing on the device (up to 150 s)");

        // Progress poller on a second connection (PAIR:START blocks its own).
        let stop = Rc::new(Cell::new(false));
        {
            let weak = Rc::downgrade(self);
            let stop = stop.clone();
            glib::spawn_future_local(async move {
                let mut last: Option<PairProgress> = None;
                while !stop.get() {
                    glib::timeout_future(Duration::from_millis(300)).await;
                    if stop.get() {
                        break;
                    }
                    let p = run_blocking(pair_progress).await;
                    let Some(this) = weak.upgrade() else { break };
                    if let Some(Ok(p)) = p {
                        if last != Some(p) {
                            last = Some(p);
                            this.progress.set_text(match p {
                                PairProgress::Idle => "Waiting for the device…",
                                PairProgress::WaitFp => "Touch an enrolled finger on the device",
                                PairProgress::WaitButton => "Press the button on the device",
                                PairProgress::Ecdh => "Exchanging keys…",
                                PairProgress::Done => "Paired",
                                PairProgress::Failed => "Pairing failed",
                            });
                        }
                    }
                }
            });
        }

        let r = run_blocking(pair_start).await;
        stop.set(true);
        match r {
            Some(Ok(())) => self.toast("Paired"),
            Some(Err(e)) => self.toast(&format!("Pairing failed: {}", glib::markup_escape_text(&friendly(&e)))),
            None => {}
        }
        self.progress.set_visible(false);
        self.set_busy(false);
    }

    async fn unbind(self: &Rc<Self>, win: &gtk::Window, n: u8) {
        let ok = confirm(
            win,
            &format!("Unbind Host {n}?"),
            "That computer will no longer be able to authenticate with this device until it pairs again. Requires one touch of an enrolled finger.",
            "Unbind",
        )
        .await;
        if !ok {
            return;
        }
        self.set_busy(true);
        let r = gate_dialog::run(
            win,
            &format!("Unbind Host {n}"),
            "Touch a registered fingerprint on the device to confirm unbinding the other host.",
            move || clear_other_slot(n),
        )
        .await;
        match r {
            Some(Ok(())) => self.toast(&format!("Host {n} unbound")),
            Some(Err(e)) => {
                self.toast(&format!("Unbind failed: {}", glib::markup_escape_text(&friendly(&e))))
            }
            None => {}
        }
        self.set_busy(false);
    }
}
