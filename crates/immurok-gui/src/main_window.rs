//! Main settings window: header bar + sidebar-driven view stack of pages.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use crate::pages;

pub struct MainWindow;

impl MainWindow {
    pub fn present_for(app: &adw::Application) {
        if let Some(existing) = app.windows().into_iter().find(|w| w.widget_name() == "main") {
            existing.present();
            return;
        }
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("immurok")
            .default_width(900)
            .default_height(620)
            .build();
        window.set_widget_name("main");

        let toasts = adw::ToastOverlay::new();
        let stack = adw::ViewStack::new();

        let firmware = pages::firmware::FirmwarePage::new(&toasts);

        let dashboard = Rc::new(pages::dashboard::DashboardPage::new(&toasts, &firmware));
        stack
            .add_titled(dashboard.widget(), Some("dashboard"), "Device")
            .set_icon_name(Some("bluetooth-active-symbolic"));

        let features = pages::features::FeaturesPage::new(&toasts);
        stack
            .add_titled(features.widget(), Some("features"), "Features")
            .set_icon_name(Some("preferences-other-symbolic"));
        dashboard.set_features(&features);

        let keys = pages::keys::KeysPage::new(&toasts);
        stack
            .add_titled(keys.widget(), Some("keys"), "Keys")
            .set_icon_name(Some("dialog-password-symbolic"));
        dashboard.set_keys(&keys);

        let fingerprints = pages::fingerprints::FingerprintsPage::new(&toasts);
        stack
            .add_titled(fingerprints.widget(), Some("fingerprints"), "Fingerprints")
            .set_icon_name(Some(pages::fingerprints::finger_icon_name()));

        let pam = pages::pam::PamPage::new(&toasts);
        stack
            .add_titled(pam.widget(), Some("pam"), "PAM")
            .set_icon_name(Some("security-high-symbolic"));

        let logs = pages::logs::LogsPage::new();
        stack
            .add_titled(logs.widget(), Some("logs"), "Logs")
            .set_icon_name(Some("utilities-terminal-symbolic"));

        let header = adw::HeaderBar::new();

        let nav_list = build_sidebar(&stack);
        let status = crate::sidebar::SidebarStatus::new();
        let left = gtk::Box::new(gtk::Orientation::Vertical, 0);
        left.set_size_request(200, -1);
        left.set_hexpand(false);
        left.append(status.widget());
        left.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        left.append(&nav_list);
        nav_list.set_vexpand(true);
        dashboard.set_sidebar(&status);
        let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        body.append(&left);
        body.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        body.append(&stack);
        stack.set_hexpand(true);
        stack.set_vexpand(true);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header);
        content.append(&body);
        toasts.set_child(Some(&content));
        window.set_content(Some(&toasts));

        dashboard.start_polling(&window);
        fingerprints.start(&window);

        {
            let toasts = toasts.clone();
            let logs = Rc::downgrade(&logs);
            window.connect_close_request(move |_| {
                if pages::firmware::updating() {
                    toasts.add_toast(adw::Toast::new("Firmware update in progress — please wait."));
                    return glib::Propagation::Stop;
                }
                if let Some(l) = logs.upgrade() {
                    l.shutdown();
                }
                glib::Propagation::Proceed
            });
        }

        {
            let keys = Rc::downgrade(&keys);
            stack.connect_visible_child_name_notify(move |s| {
                if s.visible_child_name().as_deref() != Some("keys") {
                    if let Some(k) = keys.upgrade() {
                        k.clear_codes();
                    }
                }
            });
        }

        // Keep the page alive as long as the window; the poll closure only
        // holds Weak/WeakRef to both the page and the window so a closed
        // window lets everything drop instead of the timer ticking forever.
        unsafe { window.set_data("sidebar-status", status) };
        unsafe { window.set_data("dashboard-page", dashboard) };
        unsafe { window.set_data("features-page", features) };
        unsafe { window.set_data("keys-page", keys) };
        unsafe { window.set_data("fingerprints-page", fingerprints) };
        unsafe { window.set_data("pam-page", pam) };
        unsafe { window.set_data("firmware-page", firmware) };
        unsafe { window.set_data("logs-page", logs) };

        window.present();
    }
}

/// Left-hand page list (GNOME Settings style) driving `stack`. Rows are
/// built from the stack's own pages so adding a page in `present_for` is
/// enough. Selection is kept in sync both ways: clicking a row switches the
/// page, and a programmatic `set_visible_child_name` highlights the
/// matching row.
fn build_sidebar(stack: &adw::ViewStack) -> gtk::ListBox {
    let list = gtk::ListBox::new();
    list.add_css_class("navigation-sidebar");
    list.set_selection_mode(gtk::SelectionMode::Single);

    let pages = stack.pages();
    for i in 0..pages.n_items() {
        let page = pages.item(i).and_then(|o| o.downcast::<adw::ViewStackPage>().ok());
        let Some(page) = page else { continue };
        let Some(name) = page.name() else { continue };
        let row = gtk::ListBoxRow::new();
        let inner = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        inner.set_margin_top(6);
        inner.set_margin_bottom(6);
        inner.set_margin_start(6);
        inner.set_margin_end(6);
        if let Some(icon) = page.icon_name() {
            inner.append(&gtk::Image::from_icon_name(&icon));
        }
        let label = gtk::Label::new(page.title().as_deref());
        label.set_xalign(0.0);
        inner.append(&label);
        row.set_child(Some(&inner));
        unsafe { row.set_data("page-name", name.to_string()) };
        list.append(&row);
    }

    {
        let stack = stack.downgrade();
        list.connect_row_selected(move |_, row| {
            let (Some(stack), Some(row)) = (stack.upgrade(), row) else { return };
            let name = unsafe { row.data::<String>("page-name").map(|p| p.as_ref().clone()) };
            if let Some(name) = name {
                if stack.visible_child_name().as_deref() != Some(name.as_str()) {
                    stack.set_visible_child_name(&name);
                }
            }
        });
    }
    {
        let list = list.downgrade();
        stack.connect_visible_child_name_notify(move |stack| {
            let Some(list) = list.upgrade() else { return };
            let current = stack.visible_child_name().map(|n| n.to_string());
            let mut i = 0;
            while let Some(row) = list.row_at_index(i) {
                let name = unsafe { row.data::<String>("page-name").map(|p| p.as_ref().clone()) };
                if name == current {
                    if list.selected_row().as_ref() != Some(&row) {
                        list.select_row(Some(&row));
                    }
                    break;
                }
                i += 1;
            }
        });
    }
    if let Some(first) = list.row_at_index(0) {
        list.select_row(Some(&first));
    }
    list
}
