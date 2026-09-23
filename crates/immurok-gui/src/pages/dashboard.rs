//! Dashboard: two hosts, firmware. Device status lives in the sidebar card
//! (`sidebar.rs`) and feature toggles in `features.rs`; both are fed from
//! this page's poll.
//!
//! Mirrors the TUI Dashboard minus enrollment (phase 3). State is polled
//! every 2 s while the window is visible; toggles write through immediately
//! and only flip the switch once the daemon has confirmed.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::hosts::{slot_status, HostSlots};
use immurok_client::status::{query_paired, query_settings, query_status, DeviceStatus, Settings};

use super::firmware::FirmwarePage;
use super::hosts::HostsGroup;
use super::run_blocking;

pub struct DashboardPage {
    root: gtk::Widget,
    hosts: Rc<HostsGroup>,
    toasts: adw::ToastOverlay,
    /// Set while a poll is in flight so a slow daemon does not pile up.
    polling: Rc<Cell<bool>>,
    paired: Rc<Cell<bool>>,
    features: RefCell<Option<Weak<super::features::FeaturesPage>>>,
    keys: RefCell<Option<Weak<super::keys::KeysPage>>>,
    sidebar: RefCell<Option<Weak<crate::sidebar::SidebarStatus>>>,
}

impl DashboardPage {
    pub fn new(toasts: &adw::ToastOverlay, firmware: &Rc<FirmwarePage>) -> Self {
        let page = adw::PreferencesPage::new();

        // ── Two Hosts ──
        let hosts = HostsGroup::new(toasts);
        // `PreferencesPage::add` wants `&impl IsA<PreferencesGroup>`, not the
        // `&gtk::Widget` `widget()` returns (same accessor every other page
        // uses for `ViewStack::add_titled`, which only needs a `Widget`)  —
        // downcast back rather than special-case the shared interface.
        page.add(hosts.widget().downcast_ref::<adw::PreferencesGroup>().expect("HostsGroup::widget is a PreferencesGroup"));

        // ── Firmware ──
        page.add(firmware.widget().downcast_ref::<adw::PreferencesGroup>().expect("FirmwarePage::widget is a PreferencesGroup"));

        Self {
            root: page.upcast(),
            hosts,
            toasts: toasts.clone(),
            polling: Rc::new(Cell::new(false)),
            paired: Rc::new(Cell::new(false)),
            features: RefCell::new(None),
            keys: RefCell::new(None),
            sidebar: RefCell::new(None),
        }
    }

    /// The Features page shares this page's 2 s poll instead of running its own.
    pub fn set_features(&self, page: &Rc<super::features::FeaturesPage>) {
        *self.features.borrow_mut() = Some(Rc::downgrade(page));
    }

    fn features(&self) -> Option<Rc<super::features::FeaturesPage>> {
        self.features.borrow().as_ref().and_then(Weak::upgrade)
    }

    /// The Keys page's add buttons need to know whether a device is connected.
    pub fn set_keys(&self, page: &Rc<super::keys::KeysPage>) {
        *self.keys.borrow_mut() = Some(Rc::downgrade(page));
    }

    fn keys(&self) -> Option<Rc<super::keys::KeysPage>> {
        self.keys.borrow().as_ref().and_then(Weak::upgrade)
    }

    /// The sidebar device card is fed from this page's poll instead of
    /// running its own.
    pub fn set_sidebar(&self, s: &Rc<crate::sidebar::SidebarStatus>) {
        *self.sidebar.borrow_mut() = Some(Rc::downgrade(s));
    }

    fn sidebar(&self) -> Option<Rc<crate::sidebar::SidebarStatus>> {
        self.sidebar.borrow().as_ref().and_then(Weak::upgrade)
    }

    pub fn widget(&self) -> &gtk::Widget {
        &self.root
    }

    // Not yet called from this page (every call site so far reaches for
    // `self.toasts.add_toast` directly); kept for the next caller instead of
    // re-adding it, `#[allow(dead_code)]` to keep the build warning-free.
    #[allow(dead_code)]
    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    /// Poll every 2 s while `window` is visible. Call once after construction.
    pub fn start_polling(self: &Rc<Self>, window: &adw::ApplicationWindow) {
        let this = Rc::downgrade(self);
        // A `WeakRef`, not a strong clone: the window strong-owns this page
        // via `set_data`, so a strong `window` clone here would keep both the
        // window and this timer alive forever after the window closes.
        let window = window.downgrade();
        // Fire once immediately, then on the timer.
        Self::poll_once(self);
        glib::timeout_add_local(Duration::from_secs(2), move || {
            let Some(this) = this.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let Some(window) = window.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if window.is_visible() {
                Self::poll_once(&this);
            }
            glib::ControlFlow::Continue
        });
    }

    fn poll_once(this: &Rc<Self>) {
        if this.polling.replace(true) {
            return;
        }
        let weak = Rc::downgrade(this);
        glib::spawn_future_local(async move {
            let result = run_blocking(|| {
                let status = query_status();
                let settings = status.as_ref().ok().and_then(|_| query_settings().ok());
                let paired = query_paired().unwrap_or(false);
                let slots = match &status {
                    Ok(s) if s.connected => slot_status().ok(),
                    _ => None,
                };
                (status, settings, paired, slots)
            })
            .await;
            let Some(this) = weak.upgrade() else { return };
            this.polling.set(false);
            match result {
                Some((Ok(status), settings, paired, slots)) => this.apply(&status, settings.as_ref(), paired, slots),
                Some((Err(e), _, _, _)) => this.apply_daemon_down(&e),
                None => {}
            }
        });
    }

    fn apply(&self, status: &DeviceStatus, settings: Option<&Settings>, paired: bool, slots: Option<HostSlots>) {
        self.paired.set(paired);
        if let Some(s) = self.sidebar() {
            s.apply(status, paired);
        }
        self.hosts.apply(status, paired, slots);

        if let Some(f) = self.features() {
            match settings {
                Some(s) => f.apply(s),
                None => f.set_daemon_available(false),
            }
        }
        if let Some(k) = self.keys() {
            k.set_connected(status.connected);
        }
    }

    fn apply_daemon_down(&self, err: &str) {
        if let Some(s) = self.sidebar() {
            s.apply_daemon_down(err);
        }
        self.hosts.apply_daemon_down();
        if let Some(f) = self.features() {
            f.set_daemon_available(false);
        }
        if let Some(k) = self.keys() {
            k.set_connected(false);
        }
    }
}
