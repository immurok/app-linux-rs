//! Link guide: the modal that runs the app-level link (ECDH key exchange,
//! `PAIR:START`) and draws the physical steps as a checklist — a port of
//! the macOS `PairingGuideSheet` (docs/setup-wizard-flow.md, "绑定引导框").
//!
//! Why: linking as the *second* host is two physical actions (touch an
//! enrolled finger → press the button) and a one-line hint does not read
//! as two steps; people touched the finger and then waited for the 30 s
//! timeout. Each row is lit only by the daemon's progress report
//! (`PAIR:PROGRESS`), never guessed — a tick that lights up before the
//! user acted is worse than no guide.
//!
//! Terminology (same doc): the desktop's Bluetooth settings step is
//! "Bluetooth connection"; this one is "Link". "Pair" only names the wire
//! commands.
//!
//! The daemon has no `PAIR:CANCEL`: the device-side state machine keeps
//! running whatever this window does, so the dialog refuses to close while
//! the link is in flight and tells the user how to stop it on the device.
//!
//! When the link fails the guide closes and an error dialog takes over: the
//! failure explained, plus the Bluetooth connection parameters read from the
//! device right then (`GET:CONN_PARAMS`). A supervision timeout under 3000 ms
//! is the usual culprit and only a fresh connection renegotiates it, so that
//! case says outright to disconnect and reconnect rather than retry.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::hosts::{pair_progress, pair_start, slot_status, HostSlots};
use immurok_client::status::{query_conn_params, ConnParams};
use immurok_common::types::PairProgress;

use crate::device_illustration::{DeviceIllustration, Scene};
use crate::errors::friendly;
use crate::gate_dialog::ParentCloseGuard;
use crate::pages::run_blocking;

/// How the device button is confirmed on the device (daemon `wait_pair_button`).
const BUTTON_WINDOW_SECS: u32 = 30;

/// True when this computer would land in the device's *second* slot: the
/// device already belongs to another computer, so it wants an enrolled
/// finger before the button. `None` slots (older firmware / not connected)
/// mean a plain first-host link.
pub fn is_second_host(slots: Option<HostSlots>) -> bool {
    match slots {
        Some(s) if s.supported => (s.slot1 || s.slot2) && !s.bound(s.active),
        _ => false,
    }
}

/// The daemon's raw `PAIR:START` failure → what to tell the user. The
/// second-host timeout explains the finger step and the factory-reset
/// escape hatch for a second-hand device.
pub fn failure_text(raw: &str, second_host: bool) -> String {
    let r = raw.trim();
    if r.contains("factory reset before re-pair") {
        return "The device already has enrolled fingerprints and cannot be linked directly. Hold the device \
                button for about 3 seconds (yellow LED after 1 s, keep holding until the red LED) to factory \
                reset, then try again. Note: this erases all fingerprints, keys and links on the device."
            .into();
    }
    // Before the generic "timeout" match: this text mentions the supervision timeout.
    if r.contains("aborted the key exchange") {
        return "The device gave up waiting for this computer to adjust the Bluetooth connection for the \
                key exchange. Disconnect the device in your Bluetooth settings, connect it again, and \
                then link again — retrying on the same connection usually fails the same way."
            .into();
    }
    if r.contains("timeout") || r.contains("Read failed") {
        return if second_host {
            format!(
                "Linking timed out. This device is already linked to another computer: touch an enrolled \
                 finger first, then press the device button within {BUTTON_WINDOW_SECS} seconds. If you \
                 cannot provide an enrolled fingerprint (e.g. a second-hand device), hold the device button \
                 for about 3 seconds (yellow LED after 1 s, keep holding until the red LED) to factory reset, \
                 then try again."
            )
        } else {
            format!(
                "Linking timed out: the device button was not pressed within {BUTTON_WINDOW_SECS} seconds. \
                 Please try again."
            )
        };
    }
    if r.contains("cancelled (long-press") {
        return "Linking cancelled (the device button was long-pressed).".into();
    }
    if r.contains("conn param") {
        return "Linking failed: the Bluetooth connection parameters are inadequate for the secure key \
                exchange. Disconnect and reconnect the device, then try again."
            .into();
    }
    if r.contains("PAIRING_IN_PROGRESS") {
        return "Linking is already in progress on this device. Wait for it to finish or time out, then try \
                again."
            .into();
    }
    if r.contains("NOT_CONNECTED") {
        return "The device is not connected over Bluetooth.".into();
    }
    format!("Linking failed: {}", friendly(r))
}

/// The second paragraph of the failure dialog: the Bluetooth connection
/// parameters read from the device right after the failure, and the one
/// conclusion they support. A supervision timeout under
/// [`ConnParams::ECC_MIN_TIMEOUT_MS`] is the usual reason the device refuses
/// its key exchange, and only a fresh connection renegotiates it — so the
/// advice is to disconnect and reconnect, not to retry on the same link.
/// `None` when the parameters could not be read (older firmware, or the
/// device dropped off), in which case the dialog says only what failed.
pub fn conn_params_text(p: Option<&ConnParams>) -> Option<String> {
    let p = p?;
    let values = format!(
        "Current Bluetooth connection: interval {:.2} ms, peripheral latency {} (up to {:.0} ms between \
         packets), supervision timeout {} ms.",
        p.interval_ms(),
        p.latency,
        p.effective_interval_ms(),
        p.timeout_ms()
    );
    let verdict = if p.ecc_ok() {
        format!(
            "The supervision timeout is long enough for the key exchange (at least {} ms), so the connection \
             parameters are not the cause.",
            ConnParams::ECC_MIN_TIMEOUT_MS
        )
    } else {
        format!(
            "The supervision timeout is below the {} ms the device needs for the key exchange (it asks this \
             computer for 6000 ms), so the device refuses to link on this connection. Disconnect the device \
             in your Bluetooth settings, connect it again so the connection parameters are renegotiated, then \
             link again.",
            ConnParams::ECC_MIN_TIMEOUT_MS
        )
    };
    Some(format!("{values}\n{verdict}"))
}

/// Full body of the failure dialog: what failed, then (when readable) the
/// connection parameters and what they imply.
pub fn failure_dialog_text(raw: &str, second_host: bool, params: Option<&ConnParams>) -> String {
    let mut text = failure_text(raw, second_host);
    if let Some(p) = conn_params_text(params) {
        text.push_str("\n\n");
        text.push_str(&p);
    }
    text
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowState {
    Pending,
    Active,
    Done,
}

struct Row {
    root: gtk::Box,
    badge: gtk::Image,
    badge_frame: gtk::Box,
    title: gtk::Label,
    icon: &'static str,
}

impl Row {
    fn build(icon: &'static str, title: &str, detail: &str) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        let badge_frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        badge_frame.set_size_request(40, 40);
        badge_frame.set_valign(gtk::Align::Start);
        badge_frame.add_css_class("link-guide-badge");
        let badge = gtk::Image::from_icon_name(icon);
        badge.set_pixel_size(18);
        badge.set_vexpand(true);
        badge.set_valign(gtk::Align::Center);
        badge.set_halign(gtk::Align::Center);
        badge_frame.append(&badge);

        let col = gtk::Box::new(gtk::Orientation::Vertical, 3);
        col.set_hexpand(true);
        let title_l = gtk::Label::builder().label(title).xalign(0.0).wrap(true).build();
        let detail_l = gtk::Label::builder().label(detail).xalign(0.0).wrap(true).build();
        detail_l.add_css_class("caption");
        detail_l.add_css_class("dim-label");
        col.append(&title_l);
        col.append(&detail_l);

        root.append(&badge_frame);
        root.append(&col);
        Self { root, badge, badge_frame, title: title_l, icon }
    }

    fn set_state(&self, s: RowState) {
        for c in ["pending", "active", "done"] {
            self.badge_frame.remove_css_class(c);
        }
        for c in ["heading", "dim-label"] {
            self.title.remove_css_class(c);
        }
        match s {
            RowState::Pending => {
                self.badge_frame.add_css_class("pending");
                self.badge.set_icon_name(Some(self.icon));
                self.title.add_css_class("dim-label");
            }
            RowState::Active => {
                self.badge_frame.add_css_class("active");
                self.badge.set_icon_name(Some(self.icon));
                self.title.add_css_class("heading");
            }
            RowState::Done => {
                self.badge_frame.add_css_class("done");
                self.badge.set_icon_name(Some("object-select-symbolic"));
            }
        }
    }
}

const GUIDE_CSS: &str = r#"
.link-guide-badge { border-radius: 20px; }
.link-guide-badge.pending { background: alpha(@accent_bg_color, 0.06); color: alpha(currentColor, 0.55); }
.link-guide-badge.active  { background: alpha(@accent_bg_color, 0.15); color: @accent_color; }
.link-guide-badge.done    { background: alpha(@success_color, 0.15); color: @success_color; }
"#;

fn install_css_once() {
    thread_local! { static DONE: Cell<bool> = const { Cell::new(false) }; }
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_data(GUIDE_CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

/// Run the link in a modal over `parent`. Resolves `Ok(())` once the daemon
/// reports `OK:PAIRED`, `Err(text)` with user-facing text otherwise. The
/// pre-check (which slot this computer lands in) runs here so the guide
/// draws the right branch before the first progress report arrives.
pub async fn run(parent: &impl IsA<gtk::Window>) -> Result<(), String> {
    install_css_once();

    // Resolve the slot picture BEFORE starting the handshake: issued
    // concurrently, WAIT_BUTTON could come back first and the guide would
    // render the one-step branch for a two-step link.
    let slots = run_blocking(|| slot_status().ok()).await.flatten();
    let second_host = is_second_host(slots);

    let window = gtk::Window::builder()
        .transient_for(parent)
        .destroy_with_parent(true)
        .modal(true)
        .resizable(false)
        .title("Link This Computer")
        .default_width(400)
        .build();
    let _parent_close = ParentCloseGuard::new(parent, &window);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 18);
    root.set_margin_top(24);
    root.set_margin_bottom(24);
    root.set_margin_start(24);
    root.set_margin_end(24);

    let title = gtk::Label::builder()
        .label(if second_host { "Two steps to finish linking" } else { "One step to finish linking" })
        .xalign(0.0)
        .build();
    title.add_css_class("title-4");
    root.append(&title);

    // The picture of where to press. Swapped (rebuilt) when the step
    // changes so each scene's loop starts from its first frame.
    let picture = gtk::Box::new(gtk::Orientation::Vertical, 0);
    picture.set_halign(gtk::Align::Center);
    root.append(&picture);
    let shown: Rc<Cell<Option<(Scene, bool)>>> = Rc::new(Cell::new(None));
    let illustration: Rc<std::cell::RefCell<Option<DeviceIllustration>>> = Rc::new(std::cell::RefCell::new(None));
    let set_scene = {
        let picture = picture.clone();
        let shown = shown.clone();
        let illustration = illustration.clone();
        move |scene: Scene, animated: bool| {
            if shown.get() == Some((scene, animated)) {
                return;
            }
            if let Some((s, _)) = shown.get() {
                if s == scene {
                    if let Some(i) = illustration.borrow().as_ref() {
                        i.set_animated(animated);
                    }
                    shown.set(Some((scene, animated)));
                    return;
                }
            }
            while let Some(c) = picture.first_child() {
                picture.remove(&c);
            }
            let i = DeviceIllustration::new(scene, animated);
            picture.append(i.widget());
            *illustration.borrow_mut() = Some(i);
            shown.set(Some((scene, animated)));
        }
    };

    let rows_box = gtk::Box::new(gtk::Orientation::Vertical, 14);
    let fp_row = second_host.then(|| {
        Row::build(
            crate::pages::fingerprints::finger_icon_name(),
            "Touch an enrolled finger",
            "Place your finger on the sensor at the top of the device to prove it is you.",
        )
    });
    let button_row = Row::build(
        "input-mouse-symbolic",
        "Press the button on the bottom",
        "A single short press. The device blinks white while it waits.",
    );
    if let Some(r) = &fp_row {
        rows_box.append(&r.root);
    }
    rows_box.append(&button_row.root);
    root.append(&rows_box);
    // Shared by the progress poller and the final-state writer below.
    let rows = Rc::new((fp_row, button_row));

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let spinner = gtk::Spinner::builder().visible(false).build();
    let info_icon = gtk::Image::from_icon_name("dialog-information-symbolic");
    info_icon.add_css_class("dim-label");
    let footer_label = gtk::Label::builder()
        .label("To stop: long-press the device button, or do nothing and it cancels after 30 seconds.")
        .wrap(true)
        .xalign(0.0)
        .hexpand(true)
        .build();
    footer_label.add_css_class("caption");
    footer_label.add_css_class("dim-label");
    footer.append(&spinner);
    footer.append(&info_icon);
    footer.append(&footer_label);
    root.append(&footer);

    window.set_child(Some(&root));

    // Esc / Alt+F4 only ever hid the UI on macOS while the device kept
    // waiting; here the window simply refuses to go while in flight.
    let in_flight = Rc::new(Cell::new(true));
    {
        let f = in_flight.clone();
        window.connect_close_request(move |_| {
            if f.get() {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
    }
    let apply = {
        let set_scene = set_scene.clone();
        let spinner = spinner.clone();
        let info_icon = info_icon.clone();
        let footer_label = footer_label.clone();
        let rows = rows.clone();
        move |p: PairProgress| {
            let (fp, button, scene, animated, working) = match p {
                PairProgress::Idle => {
                    if second_host {
                        (RowState::Active, RowState::Pending, Scene::TouchSensor, true, false)
                    } else {
                        (RowState::Done, RowState::Active, Scene::PressButton, true, false)
                    }
                }
                PairProgress::WaitFp => (RowState::Active, RowState::Pending, Scene::TouchSensor, true, false),
                PairProgress::WaitButton => (RowState::Done, RowState::Active, Scene::PressButton, true, false),
                PairProgress::Ecdh => (RowState::Done, RowState::Done, Scene::PressButton, false, true),
                PairProgress::Done => (RowState::Done, RowState::Done, Scene::PressButton, false, false),
                PairProgress::Failed => (RowState::Pending, RowState::Pending, Scene::PressButton, false, false),
            };
            if let Some(r) = &rows.0 {
                r.set_state(fp);
            }
            rows.1.set_state(button);
            set_scene(scene, animated);
            spinner.set_visible(working);
            spinner.set_spinning(working);
            info_icon.set_visible(!working);
            footer_label.set_text(if working {
                "Exchanging keys, about 4 seconds…"
            } else {
                "To stop: long-press the device button, or do nothing and it cancels after 30 seconds."
            });
        }
    };
    apply(PairProgress::Idle);

    window.present();

    // `PAIR:START` blocks its own connection for the whole flow; progress
    // is polled on a second connection.
    let stop = Rc::new(Cell::new(false));
    {
        let stop = stop.clone();
        let apply = apply.clone();
        glib::spawn_future_local(async move {
            let mut last: Option<PairProgress> = None;
            while !stop.get() {
                glib::timeout_future(Duration::from_millis(300)).await;
                if stop.get() {
                    break;
                }
                if let Some(Ok(p)) = run_blocking(pair_progress).await {
                    if stop.get() {
                        break;
                    }
                    if last != Some(p) {
                        last = Some(p);
                        apply(p);
                    }
                }
            }
        });
    }

    let r = run_blocking(pair_start).await;
    stop.set(true);
    in_flight.set(false);

    match r {
        Some(Ok(())) => {
            apply(PairProgress::Done);
            footer_label.set_text("Linked.");
            glib::timeout_future(Duration::from_millis(600)).await;
            window.close();
            Ok(())
        }
        other => {
            let raw = match other {
                Some(Err(e)) => e,
                _ => "worker panicked".to_string(),
            };
            apply(PairProgress::Failed);
            spinner.set_visible(false);
            footer_label.set_text("Linking failed, reading the connection parameters…");

            // The BLE worker is free again now that `do_pair` returned, so
            // this reads the live link-layer values rather than the last
            // ones the device happened to report. Not available on firmware
            // before 1.8 or once the device has dropped off — then the
            // dialog carries only the failure itself.
            let params = run_blocking(query_conn_params).await.and_then(Result::ok);
            let text = failure_dialog_text(&raw, second_host, params.as_ref());

            // The guide has done its job; the explanation goes in its own
            // error dialog over the caller's window and stays until closed.
            window.close();
            let dialog = gtk::MessageDialog::builder()
                .transient_for(parent)
                .modal(true)
                .message_type(gtk::MessageType::Error)
                .buttons(gtk::ButtonsType::Close)
                .text("Linking failed")
                .secondary_text(&text)
                .build();
            dialog.run_future().await;
            dialog.close();
            // The caller gets the same text for its toast / status line.
            Err(text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slots(slot1: bool, slot2: bool, active: u8) -> HostSlots {
        HostSlots { supported: true, slot1, slot2, active, mine: None }
    }

    #[test]
    fn second_host_when_other_slot_taken_and_active_empty() {
        assert!(is_second_host(Some(slots(true, false, 2))));
        assert!(is_second_host(Some(slots(false, true, 1))));
    }

    #[test]
    fn first_host_when_device_empty_or_unsupported() {
        assert!(!is_second_host(Some(slots(false, false, 1))));
        assert!(!is_second_host(Some(HostSlots::default())));
        assert!(!is_second_host(None));
    }

    #[test]
    fn re_link_on_own_bound_slot_is_not_second_host() {
        // The device presents a slot that is already bound (ours, stale):
        // the firmware answers WAIT_BUTTON straight away.
        assert!(!is_second_host(Some(slots(true, false, 1))));
    }

    #[test]
    fn timeout_text_depends_on_branch() {
        let a = failure_text("pair timeout: button not pressed within 30s", false);
        let b = failure_text("pair timeout: button not pressed within 30s", true);
        assert!(a.contains("not pressed"));
        assert!(b.contains("enrolled finger") && b.contains("factory reset"));
    }

    #[test]
    fn ecdh_abort_is_explained() {
        let t = failure_text(
            "PAIR_INIT: device aborted the key exchange (0xFF) — usually the Bluetooth link's supervision \
             timeout was too short for it (last link params: interval 30.0 ms, latency 0, supervision \
             timeout 2000 ms); disconnect and reconnect the device, then try again",
            true,
        );
        assert!(t.contains("Disconnect the device") && t.contains("link again"));
    }

    #[test]
    fn needs_reset_is_explained() {
        let t = failure_text("device still has fingerprints; factory reset before re-pair", false);
        assert!(t.contains("factory reset"));
    }

    #[test]
    fn short_supervision_timeout_advises_reconnect() {
        let short = ConnParams { interval: 24, latency: 0, timeout: 200 };
        let t = failure_dialog_text("pair timeout: button not pressed within 30s", false, Some(&short));
        assert!(t.starts_with("Linking timed out"));
        assert!(t.contains("supervision timeout 2000 ms"));
        assert!(t.contains("below the 3000 ms") && t.contains("Disconnect the device"));
    }

    #[test]
    fn adequate_params_are_ruled_out_as_cause() {
        let fine = ConnParams { interval: 48, latency: 20, timeout: 600 };
        let t = conn_params_text(Some(&fine)).unwrap();
        assert!(t.contains("supervision timeout 6000 ms") && t.contains("not the cause"));
        assert!(!t.contains("Disconnect"));
    }

    #[test]
    fn unreadable_params_leave_only_the_failure() {
        assert!(conn_params_text(None).is_none());
        let t = failure_dialog_text("ERROR:NOT_CONNECTED", false, None);
        assert_eq!(t, "The device is not connected over Bluetooth.");
    }

    #[test]
    fn unknown_errors_pass_through_friendly() {
        assert_eq!(failure_text("ERROR:NOT_CONNECTED", false), "The device is not connected over Bluetooth.");
        assert!(failure_text("PAIR_INIT bad response: 00", false).contains("PAIR_INIT bad response"));
    }
}
