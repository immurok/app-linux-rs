//! Guided enrollment dialog (spec §10).
//!
//! Page "gate": the device may first ask for an already-enrolled finger —
//! same ring as `gate_dialog`. Page "guide": six-step capture with the macOS
//! step titles / arrows, a progress bar, and an orange flash + shake when
//! the device rejects a frame as too similar (Overlap) — progress does not
//! advance on that. The enrollment itself runs on a worker thread
//! (`run_enrollment`); events arrive over an async channel.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::enroll_hint::{step_arrow, step_hint};
use immurok_client::enroll_session::{run_enrollment, Continue, EnrollProgress};
use immurok_client::fingerprint::{enroll_cancel, SWITCH_SLOT};

use crate::errors::friendly;
use crate::gate_dialog::{CountdownRing, ParentCloseGuard};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollOutcome {
    Enrolled,
    Failed,
    Cancelled,
}

const SHAKE_CSS: &str = r#"
@keyframes immurok-shake {
  0%   { transform: translateX(0); }
  25%  { transform: translateX(-8px); }
  50%  { transform: translateX(8px); }
  75%  { transform: translateX(-8px); }
  100% { transform: translateX(0); }
}
.enroll-card { border-radius: 12px; padding: 18px; }
.enroll-card.overlap {
  animation: immurok-shake 300ms ease-in-out;
  background-color: alpha(@warning_color, 0.25);
}
"#;

fn install_css_once() {
    thread_local! { static DONE: Cell<bool> = const { Cell::new(false) }; }
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_data(SHAKE_CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

struct Guide {
    card: gtk::Box,
    step_title: gtk::Label,
    arrow: gtk::Label,
    progress: gtk::ProgressBar,
    subtext: gtk::Label,
    error: gtk::Label,
}

impl Guide {
    fn build() -> Self {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 10);
        card.add_css_class("card");
        card.add_css_class("enroll-card");
        let step_title = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).build();
        step_title.add_css_class("title-2");
        let arrow = gtk::Label::builder().label("·").build();
        arrow.add_css_class("title-1");
        let progress = gtk::ProgressBar::builder().show_text(true).build();
        let subtext = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).build();
        let error = gtk::Label::builder().wrap(true).visible(false).justify(gtk::Justification::Center).build();
        error.add_css_class("error");
        card.append(&step_title);
        card.append(&arrow);
        card.append(&progress);
        card.append(&subtext);
        card.append(&error);
        Self { card, step_title, arrow, progress, subtext, error }
    }

    fn set_step(&self, next_step: u8, captured: u8, total: u8) {
        self.step_title.set_text(step_hint(next_step));
        let a = step_arrow(next_step);
        self.arrow.set_text(a);
        self.arrow.set_visible((2..=5).contains(&next_step));
        let total = total.max(1);
        self.progress.set_fraction(captured as f64 / total as f64);
        self.progress.set_text(Some(&format!("Captured ({captured}/{total})")));
    }

    /// Orange flash + shake; removing and re-adding the class restarts the
    /// CSS animation.
    fn shake(&self) {
        self.card.remove_css_class("overlap");
        let card = self.card.clone();
        glib::idle_add_local_once(move || {
            card.add_css_class("overlap");
            let card = card.clone();
            glib::timeout_add_local_once(Duration::from_millis(400), move || {
                card.remove_css_class("overlap");
            });
        });
    }
}

/// Run a full enrollment of `slot` in a modal dialog. Resolves when the
/// dialog closes.
pub async fn run(parent: &impl IsA<gtk::Window>, slot: u8) -> EnrollOutcome {
    install_css_once();

    let is_switch = slot == SWITCH_SLOT;
    let window = gtk::Window::builder()
        .transient_for(parent)
        .destroy_with_parent(true)
        .modal(true)
        .resizable(false)
        .title(if is_switch { "Add switch fingerprint" } else { "Add Fingerprint" })
        .default_width(420)
        .build();

    let stack = gtk::Stack::new();
    stack.set_margin_top(24);
    stack.set_margin_bottom(24);
    stack.set_margin_start(24);
    stack.set_margin_end(24);

    // ── gate page ──
    let gate = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let ring = CountdownRing::new();
    let gate_hint = gtk::Label::builder()
        .label("Verify with an enrolled finger")
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    gate.append(&ring.area);
    gate.append(&gate_hint);
    stack.add_named(&gate, Some("gate"));

    // ── guide page ──
    let guide = Guide::build();
    let subtitle = gtk::Label::builder()
        .label(if is_switch {
            "This finger will only switch between the two hosts."
        } else {
            "Press the sensor six times, shifting slightly each time."
        })
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    subtitle.add_css_class("dim-label");
    let guide_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    guide_box.append(&subtitle);
    guide_box.append(&guide.card);
    stack.add_named(&guide_box, Some("guide"));

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::Center);
    buttons.set_margin_bottom(18);
    let cancel = gtk::Button::builder().label("Cancel").build();
    let close = gtk::Button::builder().label("Close").visible(false).build();
    close.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&close);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&stack);
    root.append(&buttons);
    window.set_child(Some(&root));
    stack.set_visible_child_name("gate");

    // Closing the main window mid-enrollment must close this dialog and
    // send FP:ENROLL_CANCEL (spec §13); the guard disconnects itself when
    // `run` returns.
    let _parent_close = ParentCloseGuard::new(parent, &window);

    // ── worker ──
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let (tx, rx) = async_channel::unbounded::<EnrollProgress>();
    {
        let flag = cancel_flag.clone();
        std::thread::spawn(move || {
            run_enrollment(slot, flag.clone(), |ev| {
                let _ = tx.send_blocking(ev);
                if flag.load(Ordering::Relaxed) {
                    Continue::Stop
                } else {
                    Continue::Go
                }
            });
        });
    }

    // Cancel = flag for the poll loop + FP:ENROLL_CANCEL right away, which
    // also unblocks a gate wait inside `enroll_start`.
    let user_cancelled = Rc::new(Cell::new(false));
    let finished = Rc::new(Cell::new(false));
    {
        let flag = cancel_flag.clone();
        let uc = user_cancelled.clone();
        // A `WeakRef`, not a strong clone: the window strong-owns this
        // button via the widget tree, so a strong `window` clone here would
        // keep the whole dialog alive forever after it closes
        // (`gtk_window_destroy` does not run dispose).
        let w = window.downgrade();
        cancel.connect_clicked(move |_| {
            uc.set(true);
            flag.store(true, Ordering::Relaxed);
            std::thread::spawn(enroll_cancel);
            if let Some(w) = w.upgrade() {
                w.close();
            }
        });
    }
    {
        let flag = cancel_flag.clone();
        let uc = user_cancelled.clone();
        let fin = finished.clone();
        window.connect_close_request(move |_| {
            if !fin.get() && !uc.get() {
                uc.set(true);
                flag.store(true, Ordering::Relaxed);
                std::thread::spawn(enroll_cancel);
            }
            glib::Propagation::Proceed
        });
    }
    {
        // Same weak-ref rationale as the cancel handler above.
        let w = window.downgrade();
        close.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.close();
            }
        });
    }

    window.present();

    let mut outcome = EnrollOutcome::Cancelled;
    while let Ok(ev) = rx.recv().await {
        if user_cancelled.get() {
            break;
        }
        match ev {
            EnrollProgress::GateWaiting => {
                stack.set_visible_child_name("gate");
                ring.start();
            }
            EnrollProgress::Started => {
                ring.stop();
                stack.set_visible_child_name("guide");
                guide.set_step(1, 0, 6);
                guide.subtext.set_text("Place your finger on the sensor...");
            }
            EnrollProgress::Step { next_step, captured, total } => {
                guide.set_step(next_step, captured, total);
                guide.subtext.set_text(if captured == 0 {
                    "Place your finger on the sensor..."
                } else {
                    "Lift your finger, then press again..."
                });
            }
            EnrollProgress::LiftFinger => guide.subtext.set_text("Lift your finger, then press again..."),
            EnrollProgress::Overlap => {
                guide.subtext.set_text("Too similar — shift your finger and press again");
                guide.shake();
            }
            EnrollProgress::Processing => guide.subtext.set_text("Processing..."),
            EnrollProgress::Complete => {
                outcome = EnrollOutcome::Enrolled;
                break;
            }
            EnrollProgress::Failed(reason) => {
                if reason.contains("FP-gate cancelled") {
                    // Our own cancel racing the gate — not a failure.
                    outcome = EnrollOutcome::Cancelled;
                    break;
                }
                outcome = EnrollOutcome::Failed;
                let text = if reason.contains("disconnected") || reason.contains("NOT_CONNECTED") {
                    "Device disconnected before enrollment finished. Reconnect the device and try again.".to_string()
                } else if reason.contains("FP-gate") {
                    friendly(&reason)
                } else {
                    "Enrollment failed. Please try again.".to_string()
                };
                // The session is over: closing the window from here must
                // not send another FP:ENROLL_CANCEL.
                finished.set(true);
                ring.stop();
                stack.set_visible_child_name("guide");
                guide.error.set_text(&text);
                guide.error.set_visible(true);
                cancel.set_visible(false);
                close.set_visible(true);
                // Wait for the user to dismiss.
                let (done_tx, done_rx) = async_channel::bounded::<()>(1);
                {
                    let done_tx = done_tx.clone();
                    close.connect_clicked(move |_| {
                        let _ = done_tx.try_send(());
                    });
                }
                window.connect_close_request(move |_| {
                    let _ = done_tx.try_send(());
                    glib::Propagation::Proceed
                });
                let _ = done_rx.recv().await;
                break;
            }
        }
    }
    finished.set(true);
    window.close();
    outcome
}
