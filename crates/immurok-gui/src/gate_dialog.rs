//! Modal "touch the device" dialog: a 30 s countdown ring, a hint line and
//! Cancel. The daemon runs the fingerprint gate inside the blocking `work`
//! call; this dialog only visualises the wait and turns Cancel into
//! `GATE:CANCEL`. Reused by delete, Test Fingerprint and unbind-other-host;
//! the enrollment dialog embeds [`CountdownRing`] for its own gate page.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::keys::cancel_gate;

use crate::errors::friendly;
use crate::pages::run_blocking;

/// The device's fingerprint gate (`BLE_FP_GATE_TIMEOUT_SECS`).
pub const GATE_SECS: f64 = 30.0;

/// A ring that drains from full to empty over [`GATE_SECS`].
pub struct CountdownRing {
    pub area: gtk::DrawingArea,
    started: Rc<Cell<Option<Instant>>>,
}

impl CountdownRing {
    pub fn new() -> Self {
        let started: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
        let area = gtk::DrawingArea::builder()
            .content_width(72)
            .content_height(72)
            .halign(gtk::Align::Center)
            .build();
        let s = started.clone();
        area.set_draw_func(move |_, cr, w, h| {
            let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
            let r = (w.min(h) as f64 / 2.0) - 4.0;
            let frac = match s.get() {
                Some(t) => (1.0 - t.elapsed().as_secs_f64() / GATE_SECS).clamp(0.0, 1.0),
                None => 1.0,
            };
            cr.set_line_width(4.0);
            cr.set_source_rgba(0.5, 0.5, 0.5, 0.25);
            cr.arc(cx, cy, r, 0.0, std::f64::consts::TAU);
            let _ = cr.stroke();
            cr.set_source_rgba(0.21, 0.52, 0.89, 1.0);
            let start = -std::f64::consts::FRAC_PI_2;
            cr.arc(cx, cy, r, start, start + std::f64::consts::TAU * frac);
            let _ = cr.stroke();
        });
        Self { area, started }
    }

    /// (Re)start the countdown from full.
    pub fn start(&self) {
        self.started.set(Some(Instant::now()));
        let area = self.area.downgrade();
        let s = self.started.clone();
        glib::timeout_add_local(Duration::from_millis(100), move || {
            let Some(area) = area.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if s.get().is_none() {
                return glib::ControlFlow::Break;
            }
            area.queue_draw();
            glib::ControlFlow::Continue
        });
    }

    pub fn stop(&self) {
        self.started.set(None);
        self.area.queue_draw();
    }
}

impl Default for CountdownRing {
    fn default() -> Self {
        Self::new()
    }
}

/// Closes a modal dialog when its parent window is asked to close, and
/// unhooks itself when the dialog's own run loop returns.
///
/// `destroy_with_parent(true)` alone is not enough (spec §13): a WM destroy
/// of the parent tears the child down without ever running the child's
/// `close-request`, which is where the `GATE:CANCEL` / `FP:ENROLL_CANCEL`
/// lives — the daemon would keep the gate armed. Closing the dialog from
/// here runs that path normally.
pub struct ParentCloseGuard {
    parent: gtk::Window,
    id: Option<glib::SignalHandlerId>,
}

impl ParentCloseGuard {
    pub fn new(parent: &impl IsA<gtk::Window>, dialog: &gtk::Window) -> Self {
        let parent: gtk::Window = parent.as_ref().clone();
        // Weak: the handler outlives nothing here, but a strong dialog
        // clone parked on the parent's signal list would keep the dialog
        // alive until the parent dies.
        let d = dialog.downgrade();
        let id = parent.connect_close_request(move |_| {
            if let Some(d) = d.upgrade() {
                d.close();
            }
            glib::Propagation::Proceed
        });
        Self { parent, id: Some(id) }
    }
}

impl Drop for ParentCloseGuard {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            self.parent.disconnect(id);
        }
    }
}

/// Show the dialog, run `work` off the main thread, resolve when it returns
/// or the user cancels. On cancel a `GATE:CANCEL` is sent so the daemon
/// stops waiting for the touch.
pub async fn run<T: Send + 'static>(
    parent: &impl IsA<gtk::Window>,
    title: &str,
    hint: &str,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Option<Result<T, String>> {
    let window = gtk::Window::builder()
        .transient_for(parent)
        .destroy_with_parent(true)
        .modal(true)
        .resizable(false)
        .title(title)
        .default_width(360)
        .build();
    // Dropped on every return path below, which disconnects the handler.
    let _parent_close = ParentCloseGuard::new(parent, &window);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(24);
    root.set_margin_bottom(24);
    root.set_margin_start(24);
    root.set_margin_end(24);

    let title_label = gtk::Label::builder().label(title).wrap(true).build();
    title_label.add_css_class("title-2");
    let ring = CountdownRing::new();
    let hint_label = gtk::Label::builder().label(hint).wrap(true).justify(gtk::Justification::Center).build();
    let cancel = gtk::Button::builder().label("Cancel").halign(gtk::Align::Center).build();

    root.append(&title_label);
    root.append(&ring.area);
    root.append(&hint_label);
    root.append(&cancel);
    window.set_child(Some(&root));

    let cancelled = Rc::new(Cell::new(false));
    let c = cancelled.clone();
    // A `WeakRef`, not a strong clone: the window strong-owns this button
    // via the widget tree, so a strong `window` clone here would keep the
    // whole dialog alive forever after it closes (`gtk_window_destroy` does
    // not run dispose).
    let w = window.downgrade();
    cancel.connect_clicked(move |_| {
        c.set(true);
        std::thread::spawn(cancel_gate);
        if let Some(w) = w.upgrade() {
            w.close();
        }
    });
    let c = cancelled.clone();
    window.connect_close_request(move |_| {
        // Closing via the WM (Alt+F4) is a cancel too; the button path has
        // already set the flag and sent the cancel.
        if !c.get() {
            c.set(true);
            std::thread::spawn(cancel_gate);
        }
        glib::Propagation::Proceed
    });

    window.present();
    ring.start();

    let result = run_blocking(work).await;
    if cancelled.get() {
        // The daemon's reply to a cancelled gate is not interesting.
        return None;
    }
    ring.stop();
    let (text, hold) = match &result {
        Some(Ok(_)) => ("Verified, processing…".to_string(), 300),
        Some(Err(e)) => (friendly(e), 1500),
        None => ("Something went wrong.".to_string(), 1500),
    };
    hint_label.set_text(&text);
    cancel.set_sensitive(false);
    glib::timeout_future(Duration::from_millis(hold)).await;
    // Mark as handled so the close-request below does not send a stray
    // GATE:CANCEL for a gate that already finished.
    cancelled.set(true);
    window.close();
    Some(result.unwrap_or_else(|| Err("worker panicked".into())))
}
