//! Sidebar device card: the always-visible anchor for "what state is the
//! device in". Fed from the Dashboard's 2 s poll; owns no I/O.

use std::rc::Rc;

use adw::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::status::DeviceStatus;

pub struct SidebarStatus {
    root: gtk::Widget,
    icon: gtk::Image,
    name: gtk::Label,
    state: gtk::Label,
    battery_box: gtk::Box,
    battery_bar: gtk::LevelBar,
    battery_pct: gtk::Label,
    fw: gtk::Label,
}

impl SidebarStatus {
    pub fn new() -> Rc<Self> {
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
            .label("Connecting to daemon…")
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .max_width_chars(14)
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

        col.append(&name);
        col.append(&state);
        col.append(&battery_box);
        col.append(&fw);
        root.append(&col);

        Rc::new(Self { root: root.upcast(), icon, name, state, battery_box, battery_bar, battery_pct, fw })
    }

    pub fn widget(&self) -> &gtk::Widget {
        &self.root
    }

    pub fn apply(&self, s: &DeviceStatus, paired: bool) {
        self.name.set_text(if s.name.is_empty() { "immurok" } else { &s.name });
        let (text, icon, accent) = if s.device_unpaired {
            ("No longer paired with this host", "bluetooth-disconnected-symbolic", false)
        } else if s.connected {
            ("Connected", "bluetooth-active-symbolic", true)
        } else if paired {
            ("Paired, not connected", "bluetooth-disconnected-symbolic", false)
        } else {
            ("Not paired", "bluetooth-disabled-symbolic", false)
        };
        self.state.set_text(text);
        self.state.set_tooltip_text(None);
        self.icon.set_icon_name(Some(icon));
        if accent {
            self.icon.add_css_class("accent");
        } else {
            self.icon.remove_css_class("accent");
        }
        self.battery_box.set_visible(s.connected);
        if s.connected {
            self.battery_bar.set_value(f64::from(s.battery.min(100)) / 100.0);
            self.battery_pct.set_text(&format!("{}%", s.battery.min(100)));
            self.fw.set_text(&format!("Firmware {}", s.fw_version));
        } else {
            self.fw.set_text("");
        }
    }

    pub fn apply_daemon_down(&self, err: &str) {
        self.state.set_text("Daemon unavailable");
        self.state.set_tooltip_text(Some(err));
        self.icon.set_icon_name(Some("bluetooth-disabled-symbolic"));
        self.icon.remove_css_class("accent");
        self.battery_box.set_visible(false);
        self.fw.set_text("");
    }
}
