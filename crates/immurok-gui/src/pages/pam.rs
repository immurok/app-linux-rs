//! PAM page (spec §5): isolation banner, per-service install state with
//! Install / Remove, and a one-shot Repair. Edits go through
//! `pkexec immurok-pam-helper` (polkit prompts); everything blocking runs
//! off the main thread.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::pam::{run_helper, service_status, services_to_repair, HelperReport, PamError, PamServiceStatus, PAM_SERVICES};
use immurok_client::probe_isolation;
use immurok_client::status::query_settings;

use super::run_blocking;

struct ServiceRow {
    service: &'static str,
    state: gtk::Label,
    button: gtk::Button,
    installed: Cell<bool>,
}

pub struct PamPage {
    root: adw::PreferencesPage,
    isolation: adw::ActionRow,
    rows: Vec<ServiceRow>,
    repair: gtk::Button,
    refresh: gtk::Button,
    toasts: adw::ToastOverlay,
    busy: Cell<bool>,
    loaded_once: Cell<bool>,
    to_repair: std::cell::RefCell<Vec<&'static str>>,
}

struct Snapshot {
    services: Vec<PamServiceStatus>,
    isolated: Option<bool>,
    to_repair: Vec<&'static str>,
}

fn snapshot() -> Snapshot {
    let services = service_status();
    let isolated = probe_isolation().map(|i| i.isolated);
    // Daemon unreachable → assume both toggles on (prompt rather than miss),
    // same as `immurok-cli pam repair`.
    let (sudo_on, polkit_on) = query_settings().map(|s| (s.unlock_sudo, s.unlock_polkit)).unwrap_or((true, true));
    let to_repair = services_to_repair(sudo_on, polkit_on);
    Snapshot { services, isolated, to_repair }
}

impl PamPage {
    pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self> {
        let root = adw::PreferencesPage::new();

        let daemon = adw::PreferencesGroup::builder().title("Daemon").build();
        let isolation = adw::ActionRow::builder().title("Isolation").subtitle("Checking…").build();
        daemon.add(&isolation);
        root.add(&daemon);

        let group = adw::PreferencesGroup::builder()
            .title("PAM services")
            .description("Installing or removing needs administrator authorization — polkit will prompt.")
            .build();
        let mut rows = Vec::new();
        // NOTE (deviation from brief): iterate `PAM_SERVICES` directly rather
        // than calling `service_status()` here — that call reads
        // `/etc/pam.d` off disk and `new()` runs on the GTK main thread at
        // window construction. `PAM_SERVICES` has the same label/path fields
        // and no I/O; the real installed/not-installed state still comes
        // from `snapshot()` inside `run_blocking` via `load()`.
        for s in PAM_SERVICES {
            let row = adw::ActionRow::builder().title(s.label).subtitle(s.path).build();
            let state = gtk::Label::new(Some("…"));
            state.add_css_class("dim-label");
            let button = gtk::Button::builder().label("Install").valign(gtk::Align::Center).build();
            row.add_suffix(&state);
            row.add_suffix(&button);
            group.add(&row);
            rows.push(ServiceRow { service: s.service, state, button, installed: Cell::new(false) });
        }

        let header_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let repair = gtk::Button::builder().label("Repair").valign(gtk::Align::Center).visible(false).build();
        repair.add_css_class("suggested-action");
        let refresh = gtk::Button::builder().icon_name("view-refresh-symbolic").valign(gtk::Align::Center).tooltip_text("Refresh").build();
        refresh.add_css_class("flat");
        header_actions.append(&repair);
        header_actions.append(&refresh);
        group.set_header_suffix(Some(&header_actions));
        root.add(&group);

        let this = Rc::new(Self {
            root,
            isolation,
            rows,
            repair,
            refresh,
            toasts: toasts.clone(),
            busy: Cell::new(false),
            loaded_once: Cell::new(false),
            to_repair: std::cell::RefCell::new(Vec::new()),
        });

        for (i, r) in this.rows.iter().enumerate() {
            let weak = Rc::downgrade(&this);
            r.button.connect_clicked(move |_| {
                if let Some(p) = weak.upgrade() {
                    let row = &p.rows[i];
                    let action = if row.installed.get() { "remove" } else { "add" };
                    p.run(action, vec![row.service]);
                }
            });
        }
        let weak = Rc::downgrade(&this);
        this.repair.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                let svcs = p.to_repair.borrow().clone();
                if !svcs.is_empty() {
                    p.run("add", svcs);
                }
            }
        });
        let weak = Rc::downgrade(&this);
        this.refresh.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.load();
            }
        });
        // Load when the page first becomes visible (not at window build).
        let weak = Rc::downgrade(&this);
        this.root.connect_map(move |_| {
            if let Some(p) = weak.upgrade() {
                if !p.loaded_once.replace(true) {
                    p.load();
                }
            }
        });
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        for r in &self.rows {
            r.button.set_sensitive(!busy);
        }
        self.refresh.set_sensitive(!busy);
        self.repair.set_visible(!self.to_repair.borrow().is_empty());
        self.repair.set_sensitive(!busy);
    }

    fn load(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        self.set_busy(true);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let snap = run_blocking(snapshot).await;
            let Some(this) = weak.upgrade() else { return };
            if let Some(snap) = snap {
                this.apply(snap);
            }
            this.set_busy(false);
        });
    }

    fn apply(&self, snap: Snapshot) {
        let (text, class) = match snap.isolated {
            Some(true) => ("Isolated daemon — running as its own system user", "success"),
            Some(false) => (
                "NOT isolated — the daemon runs as your user; any of your processes can pass sudo. Run `make install`.",
                "error",
            ),
            None => ("Isolation unknown — daemon not reachable", "dim-label"),
        };
        for c in ["success", "error", "dim-label"] {
            self.isolation.remove_css_class(c);
        }
        self.isolation.add_css_class(class);
        self.isolation.set_subtitle(text);

        for (row, st) in self.rows.iter().zip(snap.services.iter()) {
            row.installed.set(st.installed);
            row.state.set_text(if st.installed { "Installed" } else { "Not installed" });
            row.button.set_label(if st.installed { "Remove" } else { "Install" });
        }

        if !snap.to_repair.is_empty() {
            self.repair.set_tooltip_text(Some(&format!("{} service(s) need repair", snap.to_repair.len())));
        }
        *self.to_repair.borrow_mut() = snap.to_repair;
    }

    fn run(self: &Rc<Self>, action: &'static str, services: Vec<&'static str>) {
        if self.busy.get() {
            return;
        }
        self.set_busy(true);
        let this = self.clone();
        glib::spawn_future_local(async move {
            let svcs = services.clone();
            let r = run_blocking(move || run_helper(action, &svcs)).await;
            match r {
                Some(Ok(report)) => this.report_ok(action, &report),
                Some(Err(e)) => this.report_err(e).await,
                None => {}
            }
            this.set_busy(false);
            this.load();
        });
    }

    fn report_ok(&self, action: &str, report: &HelperReport) {
        for line in &report.lines {
            // `OK:ADDED(sudo)` / `OK:ALREADY_PRESENT(polkit-1)` / `OK:REMOVED(sudo)` …
            let svc = line.rsplit('(').next().map(|s| s.trim_end_matches(')')).unwrap_or("");
            let text = if line.contains("ALREADY_PRESENT") {
                format!("Already present for {}", glib::markup_escape_text(svc))
            } else if line.contains("NOT_PRESENT") {
                // `OK:NOT_PRESENT(gdm-password)` — a remove that had nothing to do.
                format!("Not present for {}", glib::markup_escape_text(svc))
            } else if action == "add" {
                format!("Installed for {}", glib::markup_escape_text(svc))
            } else {
                format!("Removed from {}", glib::markup_escape_text(svc))
            };
            self.toast(&text);
        }
    }

    async fn report_err(&self, e: PamError) {
        match e {
            PamError::AuthCancelled => self.toast("Authorization cancelled"),
            PamError::HelperFailed { lines, .. } => {
                let errs: Vec<&String> = lines.iter().filter(|l| l.starts_with("ERROR:")).collect();
                if errs.is_empty() {
                    self.toast("PAM helper failed");
                }
                for l in errs {
                    self.toast(&glib::markup_escape_text(l));
                }
            }
            PamError::NoPolkitAgent => {
                self.dialog(
                    "No authentication agent",
                    "No polkit authentication agent is running. Start your desktop's polkit agent, or run `immurok-cli pam install <service>` in a terminal.",
                )
                .await
            }
            PamError::HelperNotFound => {
                self.dialog("Helper not found", "immurok-pam-helper was not found. Re-run `make install`.").await
            }
            PamError::Spawn(s) => self.toast(&format!("Could not run pkexec: {}", glib::markup_escape_text(&s))),
        }
    }

    async fn dialog(&self, title: &str, body: &str) {
        let Some(win) = self.root.root().and_then(|r| r.downcast::<gtk::Window>().ok()) else { return };
        let dialog = gtk::MessageDialog::builder()
            .transient_for(&win)
            .modal(true)
            .message_type(gtk::MessageType::Error)
            .text(title)
            .secondary_text(body)
            .build();
        dialog.add_button("Close", gtk::ResponseType::Close);
        let _ = dialog.run_future().await;
        dialog.close();
    }
}
