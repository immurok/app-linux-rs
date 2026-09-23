//! Quick-fill panel: hotkey → pick an entry → touch → value delivered.
//!
//! State machine (spec §5):
//!   Listing ──Enter(OTP/API)──► Waiting(30 s) ──OK──► Delivering ──► closed
//!      │  └─Enter(SSH)──► Delivering (no touch)          │ error / Esc → red hint, back to Listing
//!      └─ Esc / focus lost ──► closed
//!
//! The window is undecorated, created fresh on every open and destroyed on
//! close so the compositor hands focus back to the previous window — under
//! Wayland we cannot do that ourselves.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::gdk;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::keys::{
    cancel_gate, get_api, get_otp, list_keys, ssh_public_key_line, KeyCategory, KeyEntry,
};
#[cfg(test)]
use immurok_client::keys::GATE_TIMEOUT;

use crate::errors::friendly;
use crate::filter::filter_entries;
use crate::output::Output;

const PANEL_NAME: &str = "quick-fill";
/// Delay between the panel closing and a typing backend firing — long
/// enough for the compositor to restore focus to the previous window.
const FOCUS_RETURN_DELAY: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Listing,
    Waiting,
    Delivering,
}

struct Panel {
    window: gtk::Window,
    app: adw::Application,
    search: gtk::SearchEntry,
    list: gtk::ListBox,
    progress: gtk::ProgressBar,
    hint: gtk::Label,
    all: RefCell<Vec<KeyEntry>>,
    shown: RefCell<Vec<KeyEntry>>,
    phase: Cell<Phase>,
    output: Output,
    /// Set once `is-active` is observed true. GNOME's focus-stealing
    /// prevention can leave a freshly-presented window with `is-active`
    /// false (or flip it false right after creation) when it was opened
    /// from a background/forwarded launch rather than direct user input;
    /// without this guard that reads as "focus lost" and closes the panel
    /// before the user ever saw it. Observed directly: repeated
    /// `--quick-fill` launches exited after 1-8 s with no user input at
    /// all — see task-8-report.md for the measured spread.
    was_active: Cell<bool>,
}

pub fn open(app: &adw::Application) {
    // A second hotkey press while the panel is up just re-focuses it.
    if let Some(w) = app.windows().into_iter().find(|w| w.widget_name() == PANEL_NAME) {
        w.present();
        return;
    }
    let panel = Panel::build(app, Output::Clipboard);
    panel.window.present();
    panel.load();
}

impl Panel {
    fn build(app: &adw::Application, output: Output) -> Rc<Self> {
        let window = gtk::Window::builder()
            .application(app)
            .decorated(false)
            .resizable(false)
            .default_width(480)
            .title("immurok quick-fill")
            .build();
        window.set_widget_name(PANEL_NAME);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.set_margin_top(8);
        root.set_margin_bottom(8);
        root.set_margin_start(8);
        root.set_margin_end(8);

        let search = gtk::SearchEntry::builder().placeholder_text("Search OTP / API / SSH…").build();
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        list.add_css_class("boxed-list");
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .propagate_natural_height(true)
            .max_content_height(320)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let progress = gtk::ProgressBar::builder().visible(false).build();
        let hint = gtk::Label::builder().visible(false).xalign(0.0).build();
        hint.add_css_class("error");

        root.append(&search);
        root.append(&scroller);
        root.append(&progress);
        root.append(&hint);
        window.set_child(Some(&root));

        let panel = Rc::new(Self {
            window: window.clone(),
            app: app.clone(),
            search,
            list,
            progress,
            hint,
            all: RefCell::new(Vec::new()),
            shown: RefCell::new(Vec::new()),
            phase: Cell::new(Phase::Listing),
            output,
            was_active: Cell::new(false),
        });
        panel.wire();
        panel
    }

    fn wire(self: &Rc<Self>) {
        // Search filters live.
        let weak = Rc::downgrade(self);
        self.search.connect_search_changed(move |_| {
            if let Some(p) = weak.upgrade() {
                p.refill();
            }
        });

        // Enter in the search box == activate the selected row.
        let weak = Rc::downgrade(self);
        self.search.connect_activate(move |_| {
            if let Some(p) = weak.upgrade() {
                p.activate_selected();
            }
        });

        let weak = Rc::downgrade(self);
        self.list.connect_row_activated(move |_, _| {
            if let Some(p) = weak.upgrade() {
                p.activate_selected();
            }
        });

        // `GtkSearchEntry` binds Escape to its own `stop-search` keybinding
        // signal and treats it as handled, so with focus in the box (the
        // normal case) a bubble-phase controller on the window never sees
        // it. Esc must abort the fingerprint gate in Waiting too, where
        // focus loss deliberately does not close the panel.
        let weak = Rc::downgrade(self);
        self.search.connect_stop_search(move |_| {
            if let Some(p) = weak.upgrade() {
                p.close();
            }
        });

        // Esc closes; Up/Down move the selection even while the entry has focus.
        let keys = gtk::EventControllerKey::new();
        // Capture: run before the focused widget's own key bindings.
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(p) = weak.upgrade() else { return glib::Propagation::Proceed };
            match key {
                gdk::Key::Escape => {
                    p.close();
                    glib::Propagation::Stop
                }
                gdk::Key::Up => {
                    p.move_selection(-1);
                    glib::Propagation::Stop
                }
                gdk::Key::Down => {
                    p.move_selection(1);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        self.window.add_controller(keys);

        // Clicking elsewhere dismisses the panel — except while we are waiting
        // for a touch, when the user may well be looking at the device.
        //
        // GNOME's focus-stealing prevention can leave `is-active` false (or
        // flip it false right away) on a window that was just presented
        // without direct user input — observed on both the direct
        // `--quick-fill` launch and the forwarded/background one. Ignoring
        // transitions until the window has genuinely been active once
        // avoids closing the panel before the user ever saw it.
        let weak = Rc::downgrade(self);
        self.window.connect_is_active_notify(move |w| {
            let Some(p) = weak.upgrade() else { return };
            if w.is_active() {
                p.was_active.set(true);
                return;
            }
            if p.was_active.get() && p.phase.get() == Phase::Listing {
                p.close();
            }
        });

        // Closing mid-wait aborts the device gate.
        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |w| {
            if let Some(p) = weak.upgrade() {
                if p.phase.get() == Phase::Waiting {
                    std::thread::spawn(cancel_gate);
                }
            }
            // Break the Panel ⇄ Window cycle (`Panel.window` is a strong ref,
            // the window's qdata owns the `Rc<Panel>`): dropping the stolen
            // Rc here lets the panel — and with it the widget tree — go away
            // instead of leaking one per open. The countdown timer and the
            // gated future only hold `Weak<Panel>`, so they stop on their
            // next tick / completion. GTK4 does not emit `destroy` while the
            // cycle keeps the object alive, so doing this in `destroy` would
            // never run.
            let _dropped = unsafe { w.steal_data::<Rc<Self>>("panel") };
            glib::Propagation::Proceed
        });

        // Keep the Rc alive exactly as long as the window (see the
        // `close-request` handler above, which hands it back).
        unsafe { self.window.set_data("panel", self.clone()) };
    }

    fn load(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let entries = gtk::gio::spawn_blocking(list_keys).await.unwrap_or_default();
            if let Some(p) = weak.upgrade() {
                *p.all.borrow_mut() = entries;
                p.refill();
            }
        });
    }

    fn refill(&self) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let query = self.search.text().to_string();
        let shown = filter_entries(&self.all.borrow(), &query);
        for e in &shown {
            let subtitle = match e.category {
                KeyCategory::Otp => if e.service.is_empty() { "OTP".to_string() } else { e.service.clone() },
                KeyCategory::Api => "API".to_string(),
                KeyCategory::Ssh => "SSH public key".to_string(),
            };
            // `AdwPreferencesRow:title` / `AdwActionRow:subtitle` are parsed
            // as Pango markup: an entry named `a&b` would render blank.
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&e.name))
                .subtitle(glib::markup_escape_text(&subtitle))
                .activatable(true)
                .build();
            self.list.append(&row);
        }
        *self.shown.borrow_mut() = shown;
        if let Some(first) = self.list.row_at_index(0) {
            self.list.select_row(Some(&first));
        }
    }

    fn move_selection(&self, delta: i32) {
        let n = self.shown.borrow().len() as i32;
        if n == 0 {
            return;
        }
        let cur = self.list.selected_row().map(|r| r.index()).unwrap_or(0);
        let next = (cur + delta).clamp(0, n - 1);
        if let Some(row) = self.list.row_at_index(next) {
            self.list.select_row(Some(&row));
        }
    }

    fn activate_selected(self: &Rc<Self>) {
        if self.phase.get() != Phase::Listing {
            return;
        }
        let idx = match self.list.selected_row() {
            Some(r) => r.index() as usize,
            None => return,
        };
        let entry = match self.shown.borrow().get(idx) {
            Some(e) => e.clone(),
            None => return,
        };
        match entry.category {
            KeyCategory::Ssh => self.deliver(ssh_public_key_line(&entry)),
            KeyCategory::Otp | KeyCategory::Api => self.wait_for_touch(entry),
        }
    }

    fn wait_for_touch(self: &Rc<Self>, entry: KeyEntry) {
        self.phase.set(Phase::Waiting);
        self.search.set_sensitive(false);
        self.list.set_sensitive(false);
        self.hint.set_visible(false);
        self.progress.set_visible(true);
        self.progress.set_fraction(1.0);
        // No markup escaping here: unlike `AdwToast:title` and the row
        // title/subtitle, `GtkProgressBar:text` is plain text (its internal
        // label leaves `use-markup` at FALSE), so escaping would show a
        // literal `&amp;` for an entry named `a&b`.
        self.progress.set_text(Some(&format!("Touch the device to read \"{}\"…", entry.name)));
        self.progress.set_show_text(true);

        // Countdown bar: 1.0 → 0.0 over the device's 30 s gate.
        let started = Instant::now();
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(100), move || {
            let Some(p) = weak.upgrade() else { return glib::ControlFlow::Break };
            if p.phase.get() != Phase::Waiting {
                return glib::ControlFlow::Break;
            }
            let left = 1.0 - started.elapsed().as_secs_f64() / 30.0;
            p.progress.set_fraction(left.max(0.0));
            glib::ControlFlow::Continue
        });

        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let name = entry.name.clone();
            let cat = entry.category;
            let result = gtk::gio::spawn_blocking(move || match cat {
                KeyCategory::Otp => get_otp(&name),
                _ => get_api(&name),
            })
            .await
            .unwrap_or_else(|_| Err("worker panicked".into()));
            let Some(p) = weak.upgrade() else { return };
            match result {
                Ok(value) => p.deliver(value),
                Err(e) => p.back_to_listing_with_error(&friendly(&e)),
            }
        });
    }

    fn back_to_listing_with_error(self: &Rc<Self>, msg: &str) {
        self.phase.set(Phase::Listing);
        self.progress.set_visible(false);
        self.search.set_sensitive(true);
        self.list.set_sensitive(true);
        self.hint.set_text(msg);
        self.hint.set_visible(true);
        self.search.grab_focus();
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            if let Some(p) = weak.upgrade() {
                p.hint.set_visible(false);
            }
        });
    }

    fn deliver(self: &Rc<Self>, value: String) {
        self.phase.set(Phase::Delivering);
        let app = self.app.clone();
        let output = self.output;
        let window = self.window.clone();
        glib::spawn_future_local(async move {
            let result = if output.needs_focus_return() {
                window.close();
                glib::timeout_future(FOCUS_RETURN_DELAY).await;
                output.inject(&app, &value).await
            } else {
                let r = output.inject(&app, &value).await;
                window.close();
                r
            };
            if let Err(e) = result {
                // Last resort so the user is never left with nothing.
                let _ = Output::Clipboard.inject(&app, &value).await;
                eprintln!("immurok-gui: {output:?} failed ({e}); fell back to clipboard");
            }
        });
    }

    fn close(&self) {
        self.window.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_timeout_constant_covers_device_gate() {
        assert!(GATE_TIMEOUT >= Duration::from_secs(30));
    }
}
