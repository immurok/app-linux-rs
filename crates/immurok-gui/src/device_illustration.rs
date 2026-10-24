//! Device illustrations for the setup wizard and the link guide — a port of
//! the macOS `DeviceIllustration` (docs/setup-wizard-flow.md, "设备示意图规范").
//!
//! Why this exists: users pressed the fingerprint sensor on the *top* of the
//! device when the guide said "press the button on the bottom", then reported
//! "cannot link". Words do not fix that; a picture of the bottom face with a
//! halo on the button does. The bottom face uses the same view as the user
//! manual's parts diagram, so flipping the real device over matches the
//! drawing one-to-one.
//!
//! Everything is drawn with cairo, no bitmaps: colours follow the widget's
//! foreground (so dark mode works) and the libadwaita accent colour. Text is
//! never drawn into the canvas — the one-line instruction sits in a
//! `gtk::Label` under the drawing, positioned through a `gtk::Fixed`.
//! The canvas is a fixed 340×190, which fits both the wizard page and the
//! 400 px wide link guide.

use std::cell::Cell;
use std::f64::consts::{FRAC_PI_2, PI, TAU};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::cairo;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

pub const CANVAS_W: i32 = 340;
pub const CANVAS_H: i32 = 190;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scene {
    /// The desktop's Bluetooth settings window with a "Connect" row next
    /// to the device's top face; the side LED blinks blue.
    BluetoothConnect,
    /// Bottom face with a halo rippling out of the button.
    PressButton,
    /// Top face with a finger pressing the sensor (first step when
    /// linking as the second host).
    TouchSensor,
}

/// Illustration widget. `animated` runs the ripple / press / blink loops;
/// pass `false` to freeze (the macOS guide freezes during key exchange).
pub struct DeviceIllustration {
    root: gtk::Fixed,
    area: gtk::DrawingArea,
    started: Rc<Cell<Option<Instant>>>,
    animated: Rc<Cell<bool>>,
}

struct Palette {
    fg: (f64, f64, f64),
    bg: (f64, f64, f64),
    accent: (f64, f64, f64),
}

impl Palette {
    fn of(widget: &impl IsA<gtk::Widget>) -> Self {
        let w: &gtk::Widget = widget.upcast_ref();
        #[allow(deprecated)]
        let ctx = w.style_context();
        #[allow(deprecated)]
        let fg = ctx.color();
        #[allow(deprecated)]
        let bg = ctx.lookup_color("window_bg_color");
        #[allow(deprecated)]
        let accent = ctx.lookup_color("accent_color");
        let rgb = |c: gtk::gdk::RGBA| (f64::from(c.red()), f64::from(c.green()), f64::from(c.blue()));
        let fg_rgb = rgb(fg);
        // Dark foreground ⇒ light theme. Only used when the theme does not
        // export `window_bg_color` (non-libadwaita themes).
        let light = (fg_rgb.0 + fg_rgb.1 + fg_rgb.2) / 3.0 < 0.5;
        Self {
            fg: fg_rgb,
            bg: bg.map(rgb).unwrap_or(if light { (0.98, 0.98, 0.98) } else { (0.14, 0.14, 0.14) }),
            accent: accent.map(rgb).unwrap_or((0.21, 0.52, 0.89)),
        }
    }

    fn set_fg(&self, cr: &cairo::Context, alpha: f64) {
        cr.set_source_rgba(self.fg.0, self.fg.1, self.fg.2, alpha);
    }

    /// "Secondary" in the macOS drawing: the foreground at ~55 %.
    fn set_secondary(&self, cr: &cairo::Context) {
        self.set_fg(cr, 0.55);
    }

    fn set_accent(&self, cr: &cairo::Context, alpha: f64) {
        cr.set_source_rgba(self.accent.0, self.accent.1, self.accent.2, alpha);
    }

    fn set_bg(&self, cr: &cairo::Context) {
        cr.set_source_rgb(self.bg.0, self.bg.1, self.bg.2);
    }
}

/// Animation phases at `t` seconds since the illustration was shown.
struct Phases {
    /// Button halo 0→1: grows out of the button and fades; loops, no bounce.
    ripple: f64,
    /// Finger press 0→1→0, ease in/out.
    pressed: f64,
    /// Side LED on/off, slow blink.
    led_on: bool,
}

impl Phases {
    fn at(t: f64, animated: bool) -> Self {
        if !animated {
            return Self { ripple: 0.0, pressed: 0.0, led_on: true };
        }
        let r = (t % 1.4) / 1.4;
        let ripple = 1.0 - (1.0 - r) * (1.0 - r); // ease-out
        let p = (t % 1.4) / 1.4;
        let pressed = 0.5 - 0.5 * (p * TAU).cos();
        let led_on = (t % 1.8) < 0.9;
        Self { ripple, pressed, led_on }
    }
}

impl DeviceIllustration {
    pub fn new(scene: Scene, animated: bool) -> Self {
        let root = gtk::Fixed::new();
        root.set_size_request(CANVAS_W, CANVAS_H);
        root.set_halign(gtk::Align::Center);
        let area = gtk::DrawingArea::builder().content_width(CANVAS_W).content_height(CANVAS_H).build();
        root.put(&area, 0.0, 0.0);

        let started: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
        let animated_cell = Rc::new(Cell::new(animated));
        {
            let started = started.clone();
            let animated = animated_cell.clone();
            area.set_draw_func(move |area, cr, _w, _h| {
                let pal = Palette::of(area);
                let t = started.get().map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
                let ph = Phases::at(t, animated.get());
                match scene {
                    Scene::BluetoothConnect => draw_bluetooth_scene(cr, &pal, &ph),
                    Scene::PressButton => draw_press_button_scene(cr, &pal, &ph, animated.get()),
                    Scene::TouchSensor => draw_touch_sensor_scene(cr, &pal, &ph),
                }
            });
        }

        // Labels live outside the canvas (no pangocairo in the tree, and a
        // real label follows the theme font for free).
        match scene {
            Scene::BluetoothConnect => {
                root.put(&caption("Bluetooth", true), 18.0, 66.0);
                root.put(&caption("immurok IK-1", true), 26.0, 100.0);
                let connect = caption("Connect", true);
                connect.add_css_class("accent");
                root.put(&connect, 126.0, 101.0);
            }
            Scene::PressButton => {
                root.put(&instruction("Flip it over and short-press this button"), 20.0, 162.0);
            }
            Scene::TouchSensor => {
                root.put(&instruction("Touch an enrolled finger"), 20.0, 170.0);
            }
        }

        let this = Self { root, area, started, animated: animated_cell };
        this.hook_animation();
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    /// Freeze or resume the loops (freeze while the device is busy).
    pub fn set_animated(&self, on: bool) {
        self.animated.set(on);
        self.area.queue_draw();
    }

    /// The loops only tick while the drawing is mapped; the timer breaks
    /// itself on unmap so a closed dialog cannot keep it alive.
    fn hook_animation(&self) {
        let started = self.started.clone();
        let animated = self.animated.clone();
        self.area.connect_map(move |area| {
            // Start on the next main-loop turn (same trick as the macOS
            // sheet): starting inside `map` would fold the dialog's own
            // presentation into the first frame.
            started.set(None);
            let s = started.clone();
            let an = animated.clone();
            let weak = area.downgrade();
            glib::idle_add_local_once(move || {
                let Some(area) = weak.upgrade() else { return };
                s.set(Some(Instant::now()));
                let s2 = s.clone();
                let weak = area.downgrade();
                glib::timeout_add_local(Duration::from_millis(33), move || {
                    let Some(area) = weak.upgrade() else {
                        return glib::ControlFlow::Break;
                    };
                    if !area.is_mapped() || s2.get().is_none() {
                        return glib::ControlFlow::Break;
                    }
                    if an.get() {
                        area.queue_draw();
                    }
                    glib::ControlFlow::Continue
                });
            });
        });
        let started = self.started.clone();
        self.area.connect_unmap(move |_| started.set(None));
    }
}

fn caption(text: &str, bold: bool) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class(if bold { "caption-heading" } else { "caption" });
    l
}

fn instruction(text: &str) -> gtk::Label {
    let l = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .width_request(CANVAS_W - 40)
        .max_width_chars(40)
        .build();
    l.add_css_class("caption-heading");
    l
}

// ── Scenes ──────────────────────────────────────────────────────────────

fn draw_bluetooth_scene(cr: &cairo::Context, pal: &Palette, ph: &Phases) {
    bluetooth_window_mock(cr, pal, 6.0, 44.0, 190.0, 96.0);
    arrow(cr, pal, (204.0, 92.0), (232.0, 92.0));
    top_face(cr, pal, 288.0, 88.0, 96.0, None, Some(ph.led_on));
}

fn draw_press_button_scene(cr: &cairo::Context, pal: &Palette, ph: &Phases, animated: bool) {
    // No finger here: a finger would cover the very button we point at.
    bottom_face(cr, pal, 170.0, 84.0, 140.0, Some(if animated { ph.ripple } else { 0.0 }));
}

fn draw_touch_sensor_scene(cr: &cairo::Context, pal: &Palette, ph: &Phases) {
    top_face(cr, pal, 170.0, 100.0, 140.0, Some((0.20, 0.72, 0.36)), None);
    // Finger comes from above; its pad centre sits on the sensor. The press
    // loop nudges it 5 px along its own direction.
    let dy = 2.0 - 7.0 * ph.pressed;
    finger_down(cr, pal, 170.0, 94.0 + dy, 64.0);
}

// ── Parts ───────────────────────────────────────────────────────────────

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * FRAC_PI_2);
    cr.close_path();
}

fn circle(cr: &cairo::Context, cx: f64, cy: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(cx, cy, r, 0.0, TAU);
}

/// Vertical stadium (capsule) centred at (cx, cy).
fn capsule(cr: &cairo::Context, cx: f64, cy: f64, w: f64, h: f64) {
    rounded_rect(cr, cx - w / 2.0, cy - h / 2.0, w, h, w.min(h) / 2.0);
}

/// Bottom face (the manual's parts-diagram view): rounded square shell,
/// inner ring, four screws, centre cover, power switch above, round button
/// below. Ratios are fractions of the side length and must keep every part
/// inside its enclosing outline (button 0.25+0.055 and switch 0.24+0.065
/// inside the 0.34 cover; screws 0.36+0.03 inside the 0.44 inner ring) —
/// an earlier 0.27 / r=0.075 button poked out of the cover on the real
/// screen.
fn bottom_face(cr: &cairo::Context, pal: &Palette, cx: f64, cy: f64, s: f64, ripple: Option<f64>) {
    const BUTTON_OFFSET: f64 = 0.25;
    const SWITCH_OFFSET: f64 = 0.24;
    const BUTTON_DIAMETER: f64 = 0.11;

    let r = s * 0.22;
    let inset = s * 0.06;
    let (x, y) = (cx - s / 2.0, cy - s / 2.0);

    cr.set_line_width(2.0);
    pal.set_fg(cr, 1.0);
    rounded_rect(cr, x, y, s, s, r);
    let _ = cr.stroke();

    cr.set_line_width(1.2);
    pal.set_secondary(cr);
    rounded_rect(cr, x + inset, y + inset, s - 2.0 * inset, s - 2.0 * inset, r - inset * 0.8);
    let _ = cr.stroke();

    for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
        circle(cr, cx + sx * s * 0.36, cy + sy * s * 0.36, s * 0.03);
        let _ = cr.stroke();
    }

    circle(cr, cx, cy, s * 0.34);
    let _ = cr.stroke();

    // Power switch (top): capsule outline + filled nub.
    capsule(cr, cx, cy - s * SWITCH_OFFSET, s * 0.07, s * 0.13);
    let _ = cr.stroke();
    circle(cr, cx, cy - s * SWITCH_OFFSET - s * 0.03, s * 0.0225);
    let _ = cr.fill();

    // Button (bottom): halo ripples outward and fades; a 0 phase leaves a
    // static ring so a frozen drawing still points at the button.
    let by = cy + s * BUTTON_OFFSET;
    let br = s * BUTTON_DIAMETER / 2.0;
    if let Some(phase) = ripple {
        cr.set_line_width(2.0);
        pal.set_accent(cr, (1.0 - phase) * 0.9);
        circle(cr, cx, by, br * (1.0 + phase * 2.2));
        let _ = cr.stroke();
        pal.set_accent(cr, 0.25);
        circle(cr, cx, by, br);
        let _ = cr.fill();
    }
    cr.set_line_width(2.0);
    pal.set_fg(cr, 1.0);
    circle(cr, cx, by, br);
    let _ = cr.stroke();
}

/// Top face: rounded square shell + the sensor's double ring. `glow` tints
/// the sensor (green while a finger is wanted); `led` draws the side LED
/// just under the bottom edge, bright or dim.
fn top_face(cr: &cairo::Context, pal: &Palette, cx: f64, cy: f64, s: f64, glow: Option<(f64, f64, f64)>, led: Option<bool>) {
    let r = s * 0.24;
    let sensor = s * 0.32;
    let (x, y) = (cx - s / 2.0, cy - s / 2.0);

    cr.set_line_width(2.0);
    pal.set_fg(cr, 1.0);
    rounded_rect(cr, x, y, s, s, r);
    let _ = cr.stroke();

    if let Some((gr, gg, gb)) = glow {
        cr.set_source_rgba(gr, gg, gb, 0.25);
        circle(cr, cx, cy, sensor * 0.7);
        let _ = cr.fill();
    }
    cr.set_line_width(1.6);
    pal.set_fg(cr, 1.0);
    circle(cr, cx, cy, sensor / 2.0);
    let _ = cr.stroke();
    cr.set_line_width(1.2);
    pal.set_secondary(cr);
    circle(cr, cx, cy, sensor * 0.36);
    let _ = cr.stroke();

    if let Some(on) = led {
        cr.set_source_rgba(0.0, 0.48, 1.0, if on { 1.0 } else { 0.15 });
        let (w, h) = (s * 0.14, s * 0.05);
        rounded_rect(cr, cx - w / 2.0, cy + s * 0.5 + s * 0.07 - h / 2.0, w, h, h / 2.0);
        let _ = cr.fill();
    }
}

/// A finger pointing down whose pad centre is at (cx, pad_y). The body is
/// filled with the window background first so the device lines underneath
/// do not show through it.
fn finger_down(cr: &cairo::Context, pal: &Palette, cx: f64, pad_y: f64, length: f64) {
    let w = length * 0.42;
    let half = w / 2.0;
    let top = pad_y - length; // open end, runs off the top of the drawing

    let body = |cr: &cairo::Context| {
        cr.new_sub_path();
        cr.move_to(cx - half, top);
        cr.line_to(cx - half, pad_y);
        // Round tip at the bottom, left → right.
        cr.arc_negative(cx, pad_y, half, PI, 0.0);
        cr.line_to(cx + half, top);
    };

    body(cr);
    cr.close_path();
    pal.set_bg(cr);
    let _ = cr.fill();

    body(cr);
    cr.set_line_width(2.0);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    pal.set_fg(cr, 1.0);
    let _ = cr.stroke();

    // Nail: a shallow arc inside the tip.
    cr.new_sub_path();
    cr.arc(cx, pad_y - w * 0.12, w * 0.3, 20f64.to_radians(), 160f64.to_radians());
    cr.set_line_width(1.2);
    pal.set_secondary(cr);
    let _ = cr.stroke();
    cr.set_line_cap(cairo::LineCap::Butt);
}

/// Sketch of the desktop's Bluetooth settings window: three window dots,
/// a title (label placed by the caller) and one device row with a
/// "Connect" pill. Labels are real widgets; only frames are drawn here.
fn bluetooth_window_mock(cr: &cairo::Context, pal: &Palette, x: f64, y: f64, w: f64, h: f64) {
    cr.set_line_width(2.0);
    pal.set_fg(cr, 1.0);
    rounded_rect(cr, x, y, w, h, 10.0);
    let _ = cr.stroke();

    cr.set_line_width(1.0);
    pal.set_secondary(cr);
    for i in 0..3 {
        circle(cr, x + 10.0 + 3.5 + f64::from(i) * 12.0, y + 8.0 + 3.5, 3.5);
        let _ = cr.stroke();
    }

    // Device row.
    cr.set_line_width(1.2);
    rounded_rect(cr, x + 10.0, y + 51.0, w - 20.0, 30.0, 6.0);
    let _ = cr.stroke();

    // "Connect" pill, accent outline; the label is placed by the caller.
    pal.set_accent(cr, 1.0);
    rounded_rect(cr, x + 112.0, y + 55.0, 66.0, 22.0, 11.0);
    let _ = cr.stroke();
}

fn arrow(cr: &cairo::Context, pal: &Palette, from: (f64, f64), to: (f64, f64)) {
    cr.set_line_width(1.5);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    pal.set_secondary(cr);
    cr.move_to(from.0, from.1);
    cr.line_to(to.0, to.1);
    cr.move_to(to.0 - 7.0, to.1 - 6.0);
    cr.line_to(to.0, to.1);
    cr.line_to(to.0 - 7.0, to.1 + 6.0);
    let _ = cr.stroke();
    cr.set_line_cap(cairo::LineCap::Butt);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_are_bounded() {
        for i in 0..200 {
            let t = f64::from(i) * 0.037;
            let p = Phases::at(t, true);
            assert!((0.0..=1.0).contains(&p.ripple), "ripple {} at {t}", p.ripple);
            assert!((-1e-9..=1.0 + 1e-9).contains(&p.pressed), "pressed {} at {t}", p.pressed);
        }
    }

    #[test]
    fn frozen_phases_are_static() {
        let p = Phases::at(12.3, false);
        assert_eq!(p.ripple, 0.0);
        assert_eq!(p.pressed, 0.0);
        assert!(p.led_on);
    }

    #[test]
    fn bottom_face_parts_stay_inside_outlines() {
        // Mirror of the Swift constants: button and switch inside the 0.34
        // cover, screws inside the 0.44 inner ring.
        assert!(0.25 + 0.11 / 2.0 < 0.34);
        assert!(0.24 + 0.13 / 2.0 < 0.34);
        assert!(0.36 + 0.03 < 0.44);
    }
}
