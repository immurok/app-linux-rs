//! Sidebar device card: the always-visible anchor for "what state is the
//! device in". Fed from the Dashboard's 2 s poll; owns no I/O.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::status::{ConnParams, DeviceStatus};

use crate::i18n::TEXT;

// The firmware line is a button only so it can be clicked: strip the button
// padding so it sits flush with the caption lines above it. The flat style's
// hover background is the affordance.
const SIDEBAR_CSS: &str = r#"
button.sidebar-fw { padding: 0 4px; margin-left: -4px; min-height: 0; }
"#;

fn install_css_once() {
    thread_local! { static DONE: Cell<bool> = const { Cell::new(false) }; }
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_data(SIDEBAR_CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

pub struct SidebarStatus {
    root: gtk::Widget,
    icon: gtk::Image,
    name: gtk::Label,
    state: gtk::Label,
    battery_box: gtk::Box,
    battery_bar: gtk::LevelBar,
    battery_pct: gtk::Label,
    fw: gtk::Label,
    /// The firmware line is a flat button: clicking it reads the live BLE
    /// connection parameters from the device into `conn_popover`.
    fw_button: gtk::Button,
    conn_popover: gtk::Popover,
    conn_body: gtk::Label,
}

impl SidebarStatus {
    pub fn new() -> Rc<Self> {
        install_css_once();
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        root.set_margin_top(14);
        root.set_margin_bottom(10);
        root.set_margin_start(14);
        root.set_margin_end(14);
        root.set_hexpand(false);

        let icon = gtk::Image::from_icon_name("bluetooth-disconnected-symbolic");
        icon.set_pixel_size(32);
        icon.set_valign(gtk::Align::Start);
        root.append(&icon);

        let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
        col.set_hexpand(true);
        let name = gtk::Label::builder()
            .label("immurok")
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(14)
            .build();
        name.add_css_class("title-4");
        let state = gtk::Label::builder()
            .label(TEXT.sidebar_connecting())
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .max_width_chars(18)
            .build();
        state.add_css_class("caption");
        state.add_css_class("dim-label");

        let battery_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        battery_box.set_visible(false);
        let battery_bar = gtk::LevelBar::builder().min_value(0.0).max_value(1.0).hexpand(true).valign(gtk::Align::Center).build();
        // Default LevelBar offsets: low < 0.25 → red, high < 0.75 → yellow, else green.
        let battery_pct = gtk::Label::builder().xalign(1.0).width_chars(4).build();
        battery_pct.add_css_class("caption");
        battery_box.append(&battery_bar);
        battery_box.append(&battery_pct);

        let fw = gtk::Label::builder()
            .label("")
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(14)
            .build();
        fw.add_css_class("caption");
        fw.add_css_class("dim-label");
        let fw_button = gtk::Button::builder()
            .child(&fw)
            .has_frame(false)
            .halign(gtk::Align::Start)
            .visible(false)
            .tooltip_text(TEXT.sidebar_show_params())
            .build();
        fw_button.add_css_class("flat");
        fw_button.add_css_class("sidebar-fw");

        let conn_body = gtk::Label::builder()
            .xalign(0.0)
            .use_markup(true)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .max_width_chars(44)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(10)
            .margin_end(10)
            .build();
        let conn_popover = gtk::Popover::new();
        conn_popover.set_position(gtk::PositionType::Bottom);
        conn_popover.set_child(Some(&conn_body));
        conn_popover.set_parent(&fw_button);

        col.append(&name);
        col.append(&state);
        col.append(&battery_box);
        col.append(&fw_button);
        root.append(&col);

        let this = Rc::new(Self {
            root: root.upcast(),
            icon,
            name,
            state,
            battery_box,
            battery_bar,
            battery_pct,
            fw,
            fw_button,
            conn_popover,
            conn_body,
        });
        let weak = Rc::downgrade(&this);
        this.fw_button.connect_clicked(move |_| {
            if let Some(t) = weak.upgrade() {
                t.show_conn_params();
            }
        });
        this
    }

    /// Pop the connection-parameter card and fill it from the device. Read
    /// fresh on every click: these values change whenever either side
    /// renegotiates, and "too short a supervision timeout" is exactly what
    /// makes linking fail with 0xFF, so stale numbers would mislead.
    fn show_conn_params(self: &Rc<Self>) {
        self.conn_body.set_markup(&TEXT.sidebar_reading_params());
        self.conn_popover.popup();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let r = crate::pages::run_blocking(immurok_client::status::query_conn_params).await;
            let Some(t) = weak.upgrade() else { return };
            let text = match r {
                Some(Ok(p)) => conn_params_markup(&p),
                Some(Err(e)) => format!(
                    "{}",
                    TEXT.sidebar_read_params_failed(
                        glib::markup_escape_text(&crate::errors::friendly(&e)).as_str()
                    )
                ),
                None => TEXT.sidebar_read_params_unavailable(),
            };
            t.conn_body.set_markup(&text);
        });
    }

    pub fn widget(&self) -> &gtk::Widget {
        &self.root
    }

    /// Two lines, two layers (docs/setup-wizard-flow.md): the desktop's
    /// Bluetooth connection and this computer's app-level link are
    /// different things, and showing one word for both is what made people
    /// think "Connected" meant "done".
    pub fn apply(&self, s: &DeviceStatus, paired: bool) {
        self.name.set_text(if s.name.is_empty() { "immurok" } else { &s.name });
        let bluetooth = if s.connected {
            TEXT.sidebar_bluetooth_connected()
        } else if s.link_up {
            TEXT.sidebar_bluetooth_starting()
        } else {
            TEXT.sidebar_bluetooth_disconnected()
        };
        let link = if s.device_unpaired {
            TEXT.sidebar_link_no_longer()
        } else if paired {
            TEXT.sidebar_linked()
        } else if s.connected {
            TEXT.sidebar_not_linked()
        } else {
            TEXT.sidebar_shown_once()
        };
        let icon = if s.connected {
            "bluetooth-active-symbolic"
        } else if paired {
            "bluetooth-disconnected-symbolic"
        } else {
            "bluetooth-disabled-symbolic"
        };
        self.state.set_text(&format!("{bluetooth}\n{link}"));
        let tooltip = if s.device_unpaired {
            Some(TEXT.sidebar_tooltip_unpaired())
        } else if s.connected && !paired {
            Some(TEXT.sidebar_tooltip_bluetooth_only())
        } else {
            None
        };
        self.state.set_tooltip_text(tooltip.as_deref());
        self.icon.set_icon_name(Some(icon));
        if s.connected && paired && !s.device_unpaired {
            self.icon.add_css_class("accent");
        } else {
            self.icon.remove_css_class("accent");
        }
        self.battery_box.set_visible(s.connected);
        if s.connected {
            self.battery_bar.set_value(f64::from(s.battery.min(100)) / 100.0);
            self.battery_pct.set_text(&format!("{}%", s.battery.min(100)));
            self.fw.set_text(&TEXT.sidebar_firmware(&s.fw_version));
            self.fw_button.set_visible(true);
        } else {
            self.fw.set_text("");
            self.fw_button.set_visible(false);
            self.conn_popover.popdown();
        }
    }

    pub fn apply_daemon_down(&self, err: &str) {
        self.state.set_text(&TEXT.sidebar_daemon_unavailable());
        self.state.set_tooltip_text(Some(err));
        self.icon.set_icon_name(Some("bluetooth-disabled-symbolic"));
        self.icon.remove_css_class("accent");
        self.battery_box.set_visible(false);
        self.fw.set_text("");
        self.fw_button.set_visible(false);
        self.conn_popover.popdown();
    }
}

/// Popover text: the raw link-layer values with their units, plus the one
/// derived fact the user needs — whether the device will run its key
/// exchange on this link.
pub fn conn_params_markup(p: &ConnParams) -> String {
    TEXT.conn_params_markup(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_states_verdict() {
        let short = conn_params_markup(&ConnParams { interval: 24, latency: 0, timeout: 42 });
        assert!(short.contains("420 ms") && short.contains("Too short"));
        let fine = conn_params_markup(&ConnParams { interval: 48, latency: 20, timeout: 600 });
        assert!(fine.contains("6000 ms") && fine.contains("Long enough"));
    }
}
