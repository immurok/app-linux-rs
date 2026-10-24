//! First-run setup wizard — the Linux port of the macOS flow in
//! docs/setup-wizard-flow.md.
//!
//! Steps (the install check is only inserted when something is wrong):
//! Welcome → Install check → Bluetooth → Link this computer → Add
//! fingerprint → Done. The macOS "unlock password" and "accessibility"
//! steps have no Linux counterpart: screen unlock here goes through PAM,
//! not through typed keystrokes.
//!
//! Rules carried over verbatim:
//! - Each page is one picture, one button and one status line; no
//!   explanatory small print.
//! - "Bluetooth connection" (the desktop's job) and "Link" (the app-level
//!   key exchange) are two steps with two names; the word "pair" never
//!   appears in the UI.
//! - The device picture that says "press the button" only appears once
//!   the user has started the link and the device is actually waiting.
//! - Every step's completion is read from live state (1 s poll) and the
//!   wizard moves on by itself; going back pauses the auto-advance until
//!   the user clicks Next again.
//! - "Later" dismisses the whole wizard and records completion; it comes
//!   back only from the header menu.
//!
//! Not ported: "always on top" — GTK 4 has no keep-above on Wayland.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::fingerprint::{fp_list, FpSlots};
use immurok_client::pam::{run_helper, services_to_repair, PamError};
use immurok_client::status::{query_paired, query_settings, query_status};

use crate::device_illustration::{DeviceIllustration, Scene};
use crate::enroll_dialog::{self, EnrollOutcome};
use crate::link_guide;
use crate::pages::{confirm, run_blocking};
use crate::settings_store;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Welcome,
    Install,
    Bluetooth,
    Link,
    Fingerprint,
    Finish,
}

impl Step {
    fn title(self) -> &'static str {
        match self {
            Step::Welcome => "Welcome",
            Step::Install => "Install Check",
            Step::Bluetooth => "Bluetooth",
            Step::Link => "Link This Computer",
            Step::Fingerprint => "Add Fingerprint",
            Step::Finish => "Done",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Step::Welcome => "go-home-symbolic",
            Step::Install => "application-x-addon-symbolic",
            Step::Bluetooth => "bluetooth-active-symbolic",
            Step::Link => "insert-link-symbolic",
            Step::Fingerprint => crate::pages::fingerprints::finger_icon_name(),
            Step::Finish => "checkbox-checked-symbolic",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Step::Welcome => "welcome",
            Step::Install => "install",
            Step::Bluetooth => "bluetooth",
            Step::Link => "link",
            Step::Fingerprint => "fingerprint",
            Step::Finish => "finish",
        }
    }
}

/// Everything the wizard reads from the daemon / the system, refreshed
/// once a second. Pure data so the step logic is unit-testable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub daemon_ok: bool,
    pub daemon_err: String,
    pub connected: bool,
    /// Bluetooth link up but the daemon's session not ready yet (see
    /// `DeviceStatus::link_up`): the Bluetooth step shows "starting up"
    /// instead of "waiting for the connection" for those seconds.
    pub link_up: bool,
    pub device_name: String,
    pub paired: bool,
    pub device_unpaired: bool,
    /// `None` while the device is not connected, or connected but the list
    /// has not been read yet.
    pub fp: Option<FpSlots>,
    pub pam_to_repair: Vec<&'static str>,
}

impl Snapshot {
    pub fn linked(&self) -> bool {
        self.paired && !self.device_unpaired
    }

    pub fn fingerprints(&self) -> usize {
        self.fp.map(|f| f.auth_count()).unwrap_or(0)
    }

    pub fn complete(&self, step: Step) -> bool {
        match step {
            Step::Welcome | Step::Finish => true,
            Step::Install => self.daemon_ok && self.pam_to_repair.is_empty(),
            Step::Bluetooth => self.connected,
            Step::Link => self.linked(),
            Step::Fingerprint => self.fingerprints() > 0,
        }
    }

    /// The install check only joins the flow when there is something to
    /// fix; most users see five steps.
    pub fn steps(&self) -> Vec<Step> {
        let mut v = vec![Step::Welcome];
        if !self.complete(Step::Install) {
            v.push(Step::Install);
        }
        v.extend([Step::Bluetooth, Step::Link, Step::Fingerprint, Step::Finish]);
        v
    }

    /// Where to open: the first unfinished step; the summary when all is
    /// done; the welcome page only when nothing has been set up at all.
    pub fn initial_step(&self, steps: &[Step]) -> usize {
        let nothing_done = !self.connected && !self.linked() && self.fingerprints() == 0;
        if nothing_done {
            return 0;
        }
        steps
            .iter()
            .position(|s| !matches!(s, Step::Welcome | Step::Finish) && !self.complete(*s))
            .unwrap_or(steps.len() - 1)
    }

    /// Skippable: things the user may legitimately not want right now. The
    /// daemon being down is not one of them.
    pub fn skippable(&self, step: Step) -> bool {
        match step {
            Step::Install => self.daemon_ok,
            Step::Fingerprint => true,
            _ => false,
        }
    }
}

fn probe() -> Snapshot {
    let status = query_status();
    let (daemon_ok, daemon_err) = match &status {
        Ok(_) => (true, String::new()),
        Err(e) => (false, e.clone()),
    };
    let status = status.unwrap_or_default();
    let paired = query_paired().unwrap_or(false);
    // Daemon unreachable → assume both toggles on (prompt rather than
    // miss), same as the PAM page.
    let (sudo_on, polkit_on) = query_settings().map(|s| (s.unlock_sudo, s.unlock_polkit)).unwrap_or((true, true));
    let pam_to_repair = services_to_repair(sudo_on, polkit_on);
    Snapshot {
        daemon_ok,
        daemon_err,
        connected: status.connected,
        link_up: status.link_up,
        device_name: status.name,
        paired,
        device_unpaired: status.device_unpaired,
        // Filled in by the separate fingerprint probe (see `refresh_fp`).
        fp: None,
        pam_to_repair,
    }
}

/// `FP:LIST` is served from the daemon's cache once the device is fully
/// up, but right after the HID link comes up (`is_connected` already true,
/// status cache not seeded yet) it is a real BLE round-trip that can block
/// for a long time. It therefore runs apart from the 1 s status poll and
/// simply retries on the next tick until it answers.
fn probe_fp() -> Option<FpSlots> {
    fp_list().ok()
}

// ── Bluetooth settings launcher ─────────────────────────────────────────

/// Candidate commands for the desktop's Bluetooth settings, most specific
/// desktop first. The first one found on `PATH` wins.
pub fn bluetooth_settings_candidates(desktop: &str) -> Vec<Vec<&'static str>> {
    let d = desktop.to_ascii_lowercase();
    let gnome = vec!["gnome-control-center", "bluetooth"];
    let kde6 = vec!["systemsettings", "kcm_bluetooth"];
    let kde5 = vec!["systemsettings5", "kcm_bluetooth"];
    let blueman = vec!["blueman-manager"];
    if d.contains("kde") || d.contains("plasma") {
        vec![kde6, kde5, blueman, gnome]
    } else if d.contains("gnome") || d.contains("unity") || d.contains("cinnamon") {
        vec![gnome, blueman, kde6, kde5]
    } else {
        vec![blueman, gnome, kde6, kde5]
    }
}

fn on_path(bin: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|p| p.join(bin).is_file())
}

fn open_bluetooth_settings() -> Result<(), String> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    for cmd in bluetooth_settings_candidates(&desktop) {
        if !on_path(cmd[0]) {
            continue;
        }
        return std::process::Command::new(cmd[0])
            .args(&cmd[1..])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string());
    }
    Err("No Bluetooth settings app found".into())
}

// ── Widgets ─────────────────────────────────────────────────────────────

/// Sidebar row: icon, title, a check once the step is complete.
struct StepRow {
    row: gtk::ListBoxRow,
    check: gtk::Image,
}

/// A page's single status line: spinner while waiting, check when done.
struct StatusLine {
    root: gtk::Box,
    spinner: gtk::Spinner,
    icon: gtk::Image,
    label: gtk::Label,
}

impl StatusLine {
    fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        root.set_halign(gtk::Align::Center);
        let spinner = gtk::Spinner::new();
        let icon = gtk::Image::from_icon_name("object-select-symbolic");
        let label = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).build();
        root.append(&spinner);
        root.append(&icon);
        root.append(&label);
        let s = Self { root, spinner, icon, label };
        s.hide();
        s
    }

    fn hide(&self) {
        self.root.set_visible(false);
        self.spinner.set_spinning(false);
    }

    fn waiting(&self, text: &str) {
        self.root.set_visible(true);
        self.spinner.set_visible(true);
        self.spinner.set_spinning(true);
        self.icon.set_visible(false);
        self.label.set_text(text);
        self.label.remove_css_class("success");
        self.label.remove_css_class("error");
        self.label.add_css_class("dim-label");
    }

    fn done(&self, text: &str) {
        self.root.set_visible(true);
        self.spinner.set_visible(false);
        self.spinner.set_spinning(false);
        self.icon.set_visible(true);
        self.icon.set_icon_name(Some("object-select-symbolic"));
        self.icon.remove_css_class("error");
        self.icon.add_css_class("success");
        self.label.set_text(text);
        self.label.remove_css_class("dim-label");
        self.label.remove_css_class("error");
        self.label.add_css_class("success");
    }

    fn error(&self, text: &str) {
        self.root.set_visible(true);
        self.spinner.set_visible(false);
        self.spinner.set_spinning(false);
        self.icon.set_visible(true);
        self.icon.set_icon_name(Some("dialog-warning-symbolic"));
        self.icon.remove_css_class("success");
        self.icon.add_css_class("error");
        self.label.set_text(text);
        self.label.remove_css_class("dim-label");
        self.label.remove_css_class("success");
        self.label.add_css_class("error");
    }
}

/// Summary row on the Done page.
struct SummaryRow {
    icon: gtk::Image,
    label: gtk::Label,
}

impl SummaryRow {
    fn set(&self, ok: bool, text: &str) {
        self.icon.set_icon_name(Some(if ok { "object-select-symbolic" } else { "window-close-symbolic" }));
        self.icon.remove_css_class(if ok { "error" } else { "success" });
        self.icon.add_css_class(if ok { "success" } else { "error" });
        self.label.set_text(text);
    }
}

pub struct SetupWizard {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    steps: RefCell<Vec<Step>>,
    current: Cell<usize>,
    /// Set by Back / sidebar clicks: auto-advance waits for an explicit Next.
    paused: Cell<bool>,
    snapshot: RefCell<Snapshot>,
    busy: Cell<bool>,
    polling: Cell<bool>,
    fp_polling: Cell<bool>,

    sidebar: gtk::ListBox,
    rows: RefCell<Vec<StepRow>>,
    content: gtk::Stack,

    later: gtk::Button,
    back: gtk::Button,
    next: gtk::Button,

    // install
    install_stack: gtk::Stack,
    daemon_error: gtk::Label,
    install_button: gtk::Button,
    install_status: StatusLine,
    // bluetooth
    bt_stack: gtk::Stack,
    bt_status: StatusLine,
    bt_waiting: StatusLine,
    // link
    link_button: gtk::Button,
    link_status: StatusLine,
    // fingerprint
    fp_button: gtk::Button,
    fp_status: StatusLine,
    // finish
    summary: [SummaryRow; 4],
}

impl SetupWizard {
    /// Present the wizard (one per application). The first snapshot is
    /// taken off the main thread before the window shows, so the step list
    /// and the opening page are right from the first frame.
    pub fn open(app: &adw::Application) {
        if let Some(existing) = app.windows().into_iter().find(|w| w.widget_name() == "setup") {
            existing.present();
            return;
        }
        // Hold the application across the probe: in one-shot mode
        // (`immurok-gui --setup`) there is no window yet, and GApplication
        // quits as soon as the command-line handler returns without one.
        let hold = app.hold();
        let app = app.clone();
        glib::spawn_future_local(async move {
            let mut snap = run_blocking(probe).await.unwrap_or_default();
            if snap.connected {
                // Bounded: a device mid-connect can stall FP:LIST; the
                // regular ticks pick the list up later.
                if let Ok(Some(fp)) = glib::future_with_timeout(Duration::from_secs(2), run_blocking(probe_fp)).await {
                    snap.fp = fp;
                }
            }
            let wizard = Self::build(&app);
            wizard.set_snapshot(snap, true);
            wizard.window.present();
            wizard.start_polling();
            // Keep the wizard alive as long as its window.
            unsafe { wizard.window.set_data("setup-wizard", wizard.clone()) };
            drop(hold);
        });
    }

    fn build(app: &adw::Application) -> Rc<Self> {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("immurok Setup")
            .default_width(720)
            .default_height(540)
            .build();
        window.set_widget_name("setup");
        // Stay stacked above the settings window, as the macOS wizard floats
        // above everything (docs/setup-wizard-flow.md "始终置顶"). GTK 4 has
        // no keep-above; transient-for is what Wayland offers, and it is
        // enough for the window the wizard actually competes with. The main
        // window re-parents a wizard that was opened before it existed.
        if let Some(main) = app.windows().into_iter().find(|w| w.widget_name() == "main") {
            window.set_transient_for(Some(&main));
        }

        let toasts = adw::ToastOverlay::new();
        let header = adw::HeaderBar::new();

        // ── sidebar ──
        let side = gtk::Box::new(gtk::Orientation::Vertical, 0);
        side.set_size_request(190, -1);
        let logo_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        logo_box.set_margin_top(18);
        logo_box.set_margin_bottom(12);
        let logo = gtk::Image::from_icon_name("com.immurok.Settings");
        logo.set_pixel_size(56);
        let logo_label = gtk::Label::new(Some("immurok"));
        logo_label.add_css_class("title-3");
        logo_box.append(&logo);
        logo_box.append(&logo_label);
        side.append(&logo_box);
        let sidebar = gtk::ListBox::new();
        sidebar.add_css_class("navigation-sidebar");
        sidebar.set_selection_mode(gtk::SelectionMode::Single);
        sidebar.set_vexpand(true);
        side.append(&sidebar);

        // ── content ──
        let content = gtk::Stack::new();
        content.set_hexpand(true);
        content.set_vexpand(true);
        content.set_transition_type(gtk::StackTransitionType::SlideLeftRight);

        let (welcome, _) = page(Step::Welcome, "Welcome to immurok");
        content.add_named(&welcome, Some(Step::Welcome.name()));

        // install
        let (install, install_body) = page(Step::Install, "Install Check");
        let install_stack = gtk::Stack::new();
        install_stack.set_vhomogeneous(false);
        let daemon_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
        let daemon_title = gtk::Label::new(Some("The immurok daemon is not running"));
        daemon_title.add_css_class("heading");
        daemon_title.add_css_class("error");
        let daemon_error = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).build();
        daemon_error.add_css_class("caption");
        daemon_error.add_css_class("dim-label");
        let daemon_cmd = gtk::Label::new(Some("sudo systemctl start immurok-daemon"));
        daemon_cmd.set_selectable(true);
        daemon_cmd.add_css_class("monospace");
        daemon_box.append(&daemon_title);
        daemon_box.append(&daemon_error);
        daemon_box.append(&daemon_cmd);
        install_stack.add_named(&daemon_box, Some("daemon"));
        let hooks_box = gtk::Box::new(gtk::Orientation::Vertical, 14);
        let hooks_body = body_label(
            "sudo and system authorization go through PAM. The package leaves /etc/pam.d alone, so the hooks \
             are enabled here. Installing needs administrator authorization — polkit will prompt.",
        );
        let install_button = primary_button("Install PAM Hooks");
        hooks_box.append(&hooks_body);
        hooks_box.append(&install_button);
        install_stack.add_named(&hooks_box, Some("hooks"));
        let install_status = StatusLine::new();
        install_body.append(&install_stack);
        install_body.append(&install_status.root);
        content.add_named(&install, Some(Step::Install.name()));

        // bluetooth
        let (bluetooth, bt_body) = page(Step::Bluetooth, "Connect over Bluetooth");
        let bt_stack = gtk::Stack::new();
        bt_stack.set_vhomogeneous(false);
        let bt_waiting_box = gtk::Box::new(gtk::Orientation::Vertical, 14);
        let bt_illustration = DeviceIllustration::new(Scene::BluetoothConnect, true);
        bt_waiting_box.append(bt_illustration.widget());
        let bt_open = primary_button("Open Bluetooth Settings");
        bt_waiting_box.append(&bt_open);
        let bt_waiting = StatusLine::new();
        bt_waiting_box.append(&bt_waiting.root);
        bt_stack.add_named(&bt_waiting_box, Some("waiting"));
        let bt_done_box = gtk::Box::new(gtk::Orientation::Vertical, 14);
        let bt_status = StatusLine::new();
        bt_done_box.append(&bt_status.root);
        bt_stack.add_named(&bt_done_box, Some("done"));
        bt_body.append(&bt_stack);
        content.add_named(&bluetooth, Some(Step::Bluetooth.name()));
        // Keep the illustration alive with the page.
        unsafe { bluetooth.set_data("illustration", bt_illustration) };

        // link
        let (link, link_body) = page(Step::Link, "Link This Computer");
        link_body.append(&body_label(
            "Linking is different from the Bluetooth connection you just made: it creates an encrypted channel \
             between the app and the device, so only this computer can use it. You'll press the button on the \
             device once to confirm.",
        ));
        let link_button = primary_button("Link This Computer");
        let link_status = StatusLine::new();
        link_body.append(&link_button);
        link_body.append(&link_status.root);
        content.add_named(&link, Some(Step::Link.name()));

        // fingerprint
        let (fingerprint, fp_body) = page(Step::Fingerprint, "Add Your Fingerprint");
        fp_body.append(&body_label(
            "Just like Touch ID: repeatedly lift and rest your finger on the sensor, following the arrows to \
             adjust the angle until the progress completes.",
        ));
        let fp_button = primary_button("Start Enrollment");
        let fp_status = StatusLine::new();
        fp_body.append(&fp_button);
        fp_body.append(&fp_status.root);
        fp_body.append(&hint_card(
            "Enroll the finger you use most (index finger or thumb). You can add more later on the Fingerprints page.",
        ));
        content.add_named(&fingerprint, Some(Step::Fingerprint.name()));

        // finish
        let (finish, finish_body) = page(Step::Finish, "Setup Complete");
        let summary_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        summary_box.set_halign(gtk::Align::Center);
        let summary = ["Bluetooth", "Link", "Fingerprint", "PAM hooks"].map(|_| {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let icon = gtk::Image::from_icon_name("object-select-symbolic");
            let label = gtk::Label::builder().xalign(0.0).build();
            row.append(&icon);
            row.append(&label);
            summary_box.append(&row);
            SummaryRow { icon, label }
        });
        finish_body.append(&summary_box);
        finish_body.append(&hint_card(
            "Turn on sudo, system authorization and screen unlock on the Features page. immurok keeps running \
             in the background; closing its window does not quit it.",
        ));
        content.add_named(&finish, Some(Step::Finish.name()));

        // ── bottom bar ──
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        bar.set_margin_top(10);
        bar.set_margin_bottom(14);
        bar.set_margin_start(18);
        bar.set_margin_end(18);
        let later = gtk::Button::builder().label("Later").build();
        later.add_css_class("flat");
        let back = gtk::Button::builder().label("Back").build();
        let next = gtk::Button::builder().label("Next").build();
        next.add_css_class("suggested-action");
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        bar.append(&later);
        bar.append(&spacer);
        bar.append(&back);
        bar.append(&next);

        let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        body.append(&side);
        body.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        body.append(&content);

        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&header);
        column.append(&body);
        column.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        column.append(&bar);
        toasts.set_child(Some(&column));
        window.set_content(Some(&toasts));

        let this = Rc::new(Self {
            window,
            toasts,
            steps: RefCell::new(Vec::new()),
            current: Cell::new(0),
            paused: Cell::new(false),
            snapshot: RefCell::new(Snapshot::default()),
            busy: Cell::new(false),
            polling: Cell::new(false),
            fp_polling: Cell::new(false),
            sidebar,
            rows: RefCell::new(Vec::new()),
            content,
            later,
            back,
            next,
            install_stack,
            daemon_error,
            install_button,
            install_status,
            bt_stack,
            bt_status,
            bt_waiting,
            link_button,
            link_status,
            fp_button,
            fp_status,
            summary,
        });

        this.wire(bt_open);
        this
    }

    fn wire(self: &Rc<Self>, bt_open: gtk::Button) {
        let w = Rc::downgrade(self);
        self.later.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.finish_and_close();
            }
        });
        let w = Rc::downgrade(self);
        self.back.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                let i = t.current.get();
                if i > 0 {
                    t.paused.set(true);
                    t.go_to(i - 1);
                }
            }
        });
        let w = Rc::downgrade(self);
        self.next.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.on_next();
            }
        });
        let w = Rc::downgrade(self);
        self.sidebar.connect_row_activated(move |_, row| {
            if let Some(t) = w.upgrade() {
                let i = row.index();
                if i >= 0 && i as usize != t.current.get() {
                    t.paused.set(true);
                    t.go_to(i as usize);
                }
            }
        });

        let w = Rc::downgrade(self);
        bt_open.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                if let Err(e) = open_bluetooth_settings() {
                    t.toast(&format!(
                        "{}. Open your desktop's Bluetooth settings and connect \"immurok IK-1\".",
                        glib::markup_escape_text(&e)
                    ));
                }
            }
        });
        let w = Rc::downgrade(self);
        self.install_button.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.install_hooks();
            }
        });
        let w = Rc::downgrade(self);
        self.link_button.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.link();
            }
        });
        let w = Rc::downgrade(self);
        self.fp_button.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.enroll();
            }
        });
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    // ── state ──

    /// Poll once a second while the window is visible; the timer holds
    /// only a weak ref so a closed window lets everything drop.
    fn start_polling(self: &Rc<Self>) {
        let w = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_secs(1), move || {
            let Some(t) = w.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if t.window.is_visible() {
                t.refresh();
            }
            glib::ControlFlow::Continue
        });
    }

    fn refresh(self: &Rc<Self>) {
        if self.polling.replace(true) {
            return;
        }
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let snap = run_blocking(probe).await;
            let Some(t) = w.upgrade() else { return };
            t.polling.set(false);
            if let Some(snap) = snap {
                t.set_snapshot(snap, false);
            }
            t.refresh_fp();
        });
    }

    /// Read the fingerprint list when the device is connected and the list
    /// is unknown. Independent of the status poll so a slow `FP:LIST`
    /// never freezes the Bluetooth / Link pages; a failed read leaves the
    /// list unknown and the next tick retries.
    fn refresh_fp(self: &Rc<Self>) {
        let need = {
            let s = self.snapshot.borrow();
            s.connected && s.fp.is_none()
        };
        if !need || self.fp_polling.replace(true) {
            return;
        }
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let r = run_blocking(probe_fp).await.flatten();
            let Some(t) = w.upgrade() else { return };
            t.fp_polling.set(false);
            let Some(slots) = r else { return };
            let still_connected = t.snapshot.borrow().connected;
            if still_connected {
                t.snapshot.borrow_mut().fp = Some(slots);
                t.render();
                t.maybe_advance();
            }
        });
    }

    /// Apply a fresh snapshot. `initial` also (re)builds the step list and
    /// picks the opening page; later snapshots keep the list stable so the
    /// sidebar does not jump while the user is working through it.
    fn set_snapshot(self: &Rc<Self>, mut snap: Snapshot, initial: bool) {
        if !initial {
            // The status poll does not read the list; keep the last one
            // while the device stays connected, forget it on disconnect.
            snap.fp = if snap.connected { self.snapshot.borrow().fp } else { None };
        }
        if initial {
            let steps = snap.steps();
            let start = snap.initial_step(&steps);
            self.rebuild_sidebar(&steps);
            *self.steps.borrow_mut() = steps;
            self.current.set(start);
        }
        *self.snapshot.borrow_mut() = snap;
        self.render();
        if initial {
            self.go_to(self.current.get());
        } else {
            self.maybe_advance();
        }
    }

    fn rebuild_sidebar(&self, steps: &[Step]) {
        while let Some(r) = self.sidebar.row_at_index(0) {
            self.sidebar.remove(&r);
        }
        let mut rows = Vec::new();
        for s in steps {
            let row = gtk::ListBoxRow::new();
            let inner = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            inner.set_margin_top(6);
            inner.set_margin_bottom(6);
            inner.set_margin_start(6);
            inner.set_margin_end(6);
            let icon = gtk::Image::from_icon_name(s.icon());
            let label = gtk::Label::builder().label(s.title()).xalign(0.0).hexpand(true).build();
            let check = gtk::Image::from_icon_name("object-select-symbolic");
            check.add_css_class("success");
            check.set_visible(false);
            inner.append(&icon);
            inner.append(&label);
            inner.append(&check);
            row.set_child(Some(&inner));
            self.sidebar.append(&row);
            rows.push(StepRow { row, check });
        }
        *self.rows.borrow_mut() = rows;
    }

    fn step_at(&self, i: usize) -> Step {
        self.steps.borrow()[i]
    }

    fn go_to(self: &Rc<Self>, i: usize) {
        let n = self.steps.borrow().len();
        if i >= n {
            return;
        }
        self.current.set(i);
        let step = self.step_at(i);
        self.content.set_visible_child_name(step.name());
        if let Some(r) = self.rows.borrow().get(i) {
            self.sidebar.select_row(Some(&r.row));
        }
        self.render_buttons();
    }

    fn on_next(self: &Rc<Self>) {
        let i = self.current.get();
        let step = self.step_at(i);
        if step == Step::Finish {
            self.finish_and_close();
            return;
        }
        self.paused.set(false);
        self.go_to(i + 1);
    }

    /// Auto-advance: the current step just completed and the user is not
    /// looking back at an earlier page.
    fn maybe_advance(self: &Rc<Self>) {
        if self.paused.get() || self.busy.get() {
            return;
        }
        let i = self.current.get();
        let step = self.step_at(i);
        if matches!(step, Step::Welcome | Step::Finish) {
            return;
        }
        if self.snapshot.borrow().complete(step) {
            self.go_to(i + 1);
        }
    }

    fn finish_and_close(&self) {
        let mut s = settings_store::load();
        s.setup_completed = true;
        if let Err(e) = settings_store::save(&s) {
            self.toast(&format!("Could not save settings: {}", glib::markup_escape_text(&e)));
        }
        self.window.close();
    }

    // ── rendering ──

    fn render(&self) {
        let snap = self.snapshot.borrow();
        let steps = self.steps.borrow();
        for (i, row) in self.rows.borrow().iter().enumerate() {
            let s = steps[i];
            row.check.set_visible(!matches!(s, Step::Welcome | Step::Finish) && snap.complete(s));
        }

        // install
        if !snap.daemon_ok {
            self.install_stack.set_visible_child_name("daemon");
            self.daemon_error.set_text(&snap.daemon_err);
            self.install_status.hide();
        } else if snap.pam_to_repair.is_empty() {
            self.install_stack.set_visible_child_name("hooks");
            self.install_button.set_visible(false);
            self.install_status.done("PAM hooks installed");
        } else {
            self.install_stack.set_visible_child_name("hooks");
            self.install_button.set_visible(true);
            self.install_button.set_sensitive(!self.busy.get());
            self.install_status.hide();
        }

        // bluetooth
        if snap.connected {
            self.bt_stack.set_visible_child_name("done");
            self.bt_status.done(&format!("Bluetooth connected: {}", if snap.device_name.is_empty() { "immurok" } else { &snap.device_name }));
        } else {
            self.bt_stack.set_visible_child_name("waiting");
            if !snap.daemon_ok {
                self.bt_waiting.error("The immurok daemon is not running");
            } else if snap.link_up {
                self.bt_waiting.waiting("Bluetooth connected — the device is starting up…");
            } else {
                self.bt_waiting.waiting("Waiting for the Bluetooth connection…");
            }
        }

        // link
        if snap.linked() {
            self.link_button.set_visible(false);
            self.link_status.done("Linked");
        } else {
            self.link_button.set_visible(true);
            self.link_button.set_sensitive(snap.connected && !self.busy.get());
            if snap.device_unpaired {
                self.link_status.error("The device no longer knows this computer. Link it again.");
            } else if snap.connected {
                self.link_status.hide();
            } else {
                self.link_status.waiting("Waiting for the Bluetooth connection first");
            }
        }

        // fingerprint
        let n = snap.fingerprints();
        let ready = snap.connected && snap.linked();
        if n > 0 {
            self.fp_button.set_visible(false);
            self.fp_status.done(&format!("{n} fingerprint(s) enrolled"));
        } else {
            self.fp_button.set_visible(true);
            self.fp_button.set_sensitive(ready && snap.fp.is_some() && !self.busy.get());
            if !ready {
                self.fp_status.waiting("Link this computer first");
            } else if snap.fp.is_none() {
                self.fp_status.waiting("Reading fingerprints from the device…");
            } else {
                self.fp_status.hide();
            }
        }

        // finish
        self.summary[0].set(snap.connected, if snap.connected { "Bluetooth connected" } else { "Bluetooth not connected" });
        self.summary[1].set(snap.linked(), if snap.linked() { "Linked to this computer" } else { "Not linked" });
        self.summary[2].set(n > 0, &if n > 0 { format!("{n} fingerprint(s) enrolled") } else { "No fingerprint enrolled".into() });
        let pam_ok = snap.complete(Step::Install);
        self.summary[3].set(pam_ok, if pam_ok { "PAM hooks installed" } else { "PAM hooks not installed" });

        drop(snap);
        drop(steps);
        self.render_buttons();
    }

    fn render_buttons(&self) {
        let i = self.current.get();
        let steps = self.steps.borrow();
        if steps.is_empty() {
            return;
        }
        let step = steps[i];
        let snap = self.snapshot.borrow();
        self.back.set_visible(i > 0);
        self.later.set_visible(step != Step::Finish);
        match step {
            Step::Welcome => {
                self.next.set_label("Next");
                self.next.set_sensitive(true);
            }
            Step::Finish => {
                self.next.set_label("Done");
                self.next.set_sensitive(true);
            }
            s => {
                if snap.complete(s) {
                    self.next.set_label("Next");
                    self.next.set_sensitive(!self.busy.get());
                } else if snap.skippable(s) {
                    self.next.set_label("Skip This Step");
                    self.next.set_sensitive(!self.busy.get());
                } else {
                    self.next.set_label("Next");
                    self.next.set_sensitive(false);
                }
            }
        }
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.render();
    }

    // ── actions ──

    fn install_hooks(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        let svcs = self.snapshot.borrow().pam_to_repair.clone();
        if svcs.is_empty() {
            return;
        }
        self.set_busy(true);
        let this = self.clone();
        glib::spawn_future_local(async move {
            let r = run_blocking(move || run_helper("add", &svcs)).await;
            match r {
                Some(Ok(_)) => this.toast("PAM hooks installed"),
                Some(Err(e)) => this.toast(&pam_error_text(e)),
                None => {}
            }
            this.set_busy(false);
            this.refresh();
        });
    }

    fn link(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        self.set_busy(true);
        let this = self.clone();
        glib::spawn_future_local(async move {
            // The guide keeps its own failure explanation on screen until
            // closed; only the outcome is echoed here.
            match link_guide::run(&this.window).await {
                Ok(()) => this.toast("Linked"),
                Err(_) => this.toast("Linking failed"),
            }
            this.snapshot.borrow_mut().fp = None;
            this.set_busy(false);
            this.refresh();
        });
    }

    fn enroll(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        let fp = self.snapshot.borrow().fp;
        let Some(slot) = fp.and_then(|f| f.first_free_auth_slot()) else {
            self.toast("No free fingerprint slot");
            return;
        };
        self.set_busy(true);
        let this = self.clone();
        glib::spawn_future_local(async move {
            if fp.map(|f| f.any()).unwrap_or(false) {
                let ok = confirm(
                    &this.window,
                    "Add a New Fingerprint",
                    "The device first asks you to verify with an already-enrolled finger. After that, switch to the NEW finger.",
                    "Start",
                )
                .await;
                if !ok {
                    this.set_busy(false);
                    return;
                }
            }
            match enroll_dialog::run(&this.window, slot).await {
                EnrollOutcome::Enrolled => this.toast("Fingerprint enrolled successfully!"),
                EnrollOutcome::Failed | EnrollOutcome::Cancelled => {}
            }
            this.snapshot.borrow_mut().fp = None;
            this.set_busy(false);
            this.refresh();
        });
    }
}

fn pam_error_text(e: PamError) -> String {
    match e {
        PamError::AuthCancelled => "Authorization cancelled".into(),
        PamError::HelperFailed { lines, .. } => {
            let errs: Vec<&String> = lines.iter().filter(|l| l.starts_with("ERROR:")).collect();
            if errs.is_empty() {
                "PAM helper failed".into()
            } else {
                glib::markup_escape_text(errs[0]).to_string()
            }
        }
        PamError::NoPolkitAgent => {
            "No polkit authentication agent is running. Start your desktop's polkit agent, or run \
             `immurok-cli pam install sudo` in a terminal."
                .into()
        }
        PamError::HelperNotFound => "immurok-pam-helper was not found. Re-run `make install`.".into(),
        PamError::Spawn(s) => format!("Could not run pkexec: {}", glib::markup_escape_text(&s)),
    }
}

// ── page scaffolding ────────────────────────────────────────────────────

/// A page: big icon + title on top, then the body column the caller fills
/// (picture / one line, one button, one status line).
fn page(step: Step, title: &str) -> (gtk::Box, gtk::Box) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 18);
    outer.set_margin_top(28);
    outer.set_margin_bottom(20);
    outer.set_margin_start(32);
    outer.set_margin_end(32);
    outer.set_valign(gtk::Align::Start);

    let icon = gtk::Image::from_icon_name(step.icon());
    icon.set_pixel_size(40);
    icon.add_css_class("accent");
    let t = gtk::Label::new(Some(title));
    t.add_css_class("title-2");
    outer.append(&icon);
    outer.append(&t);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 14);
    body.set_halign(gtk::Align::Fill);
    outer.append(&body);

    if step == Step::Welcome {
        let list = gtk::Box::new(gtk::Orientation::Vertical, 6);
        list.set_halign(gtk::Align::Center);
        for (icon, text) in [
            ("system-lock-screen-symbolic", "Unlock the screen"),
            ("utilities-terminal-symbolic", "Authorize sudo"),
            ("security-high-symbolic", "Authorize system prompts (polkit)"),
        ] {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            let i = gtk::Image::from_icon_name(icon);
            i.add_css_class("dim-label");
            row.append(&i);
            row.append(&gtk::Label::new(Some(text)));
            list.append(&row);
        }
        body.append(&list);
        body.append(&hint_card(
            "You'll need your immurok fingerprint device (charged). Keep it near this computer throughout setup.",
        ));
        let time = gtk::Label::new(Some("Takes about 3 minutes — click \"Later\" to exit anytime."));
        time.add_css_class("caption");
        time.add_css_class("dim-label");
        body.append(&time);
    }
    (outer, body)
}

fn body_label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .max_width_chars(60)
        .build()
}

fn primary_button(label: &str) -> gtk::Button {
    let b = gtk::Button::builder().label(label).halign(gtk::Align::Center).build();
    b.add_css_class("suggested-action");
    b.add_css_class("pill");
    b
}

fn hint_card(text: &str) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    card.add_css_class("card");
    card.set_halign(gtk::Align::Center);
    let inner = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    inner.set_margin_top(10);
    inner.set_margin_bottom(10);
    inner.set_margin_start(12);
    inner.set_margin_end(12);
    let icon = gtk::Image::from_icon_name("dialog-information-symbolic");
    icon.add_css_class("dim-label");
    icon.set_valign(gtk::Align::Start);
    let label = gtk::Label::builder().label(text).wrap(true).xalign(0.0).max_width_chars(56).build();
    label.add_css_class("caption");
    inner.append(&icon);
    inner.append(&label);
    card.append(&inner);
    card
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> Snapshot {
        Snapshot { daemon_ok: true, ..Snapshot::default() }
    }

    #[test]
    fn install_step_only_when_something_is_wrong() {
        assert_eq!(snap().steps(), [Step::Welcome, Step::Bluetooth, Step::Link, Step::Fingerprint, Step::Finish]);
        let mut s = snap();
        s.pam_to_repair = vec!["sudo"];
        assert_eq!(s.steps()[1], Step::Install);
        let mut s = snap();
        s.daemon_ok = false;
        assert_eq!(s.steps()[1], Step::Install);
    }

    #[test]
    fn fresh_machine_opens_on_welcome() {
        let s = snap();
        assert_eq!(s.initial_step(&s.steps()), 0);
    }

    #[test]
    fn resumes_at_first_unfinished_step() {
        let mut s = snap();
        s.connected = true;
        let steps = s.steps();
        assert_eq!(steps[s.initial_step(&steps)], Step::Link);
        s.paired = true;
        assert_eq!(steps[s.initial_step(&steps)], Step::Fingerprint);
    }

    #[test]
    fn unknown_fingerprint_list_counts_as_none_yet() {
        let mut s = snap();
        s.connected = true;
        s.paired = true;
        s.fp = None;
        assert_eq!(s.fingerprints(), 0);
        assert!(!s.complete(Step::Fingerprint));
    }

    #[test]
    fn everything_done_opens_on_summary() {
        let mut s = snap();
        s.connected = true;
        s.paired = true;
        s.fp = Some(FpSlots { bitmap: 0b1 });
        let steps = s.steps();
        assert_eq!(steps[s.initial_step(&steps)], Step::Finish);
    }

    #[test]
    fn device_unpaired_is_not_linked() {
        let mut s = snap();
        s.connected = true;
        s.paired = true;
        s.device_unpaired = true;
        assert!(!s.complete(Step::Link));
    }

    #[test]
    fn daemon_down_is_not_skippable() {
        let mut s = snap();
        s.daemon_ok = false;
        assert!(!s.skippable(Step::Install));
        s.daemon_ok = true;
        s.pam_to_repair = vec!["sudo"];
        assert!(s.skippable(Step::Install));
        assert!(s.skippable(Step::Fingerprint));
        assert!(!s.skippable(Step::Link));
        assert!(!s.skippable(Step::Bluetooth));
    }

    #[test]
    fn bluetooth_settings_prefers_the_running_desktop() {
        assert_eq!(bluetooth_settings_candidates("KDE")[0][0], "systemsettings");
        assert_eq!(bluetooth_settings_candidates("GNOME")[0][0], "gnome-control-center");
        assert_eq!(bluetooth_settings_candidates("ubuntu:GNOME")[0][0], "gnome-control-center");
        assert_eq!(bluetooth_settings_candidates("sway")[0][0], "blueman-manager");
    }
}
