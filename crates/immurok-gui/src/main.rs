//! immurok-gui — graphical settings app + hotkey quick-fill panel.
//!
//! One `adw::Application` with id `com.immurok.Settings`. GApplication's
//! single-instance machinery does the rest: a second `immurok-gui
//! --quick-fill` forwards its command line to the running instance, which
//! fires the `quick-fill` action. Portal shortcuts (phase 2) and X11 key
//! grabs fire the same action, so every entry path converges here.

mod cli;
mod enroll_dialog;
mod errors;
mod filter;
mod gate_dialog;
mod key_add_dialog;
mod main_window;
mod output;
mod pages;
mod quickfill;
mod settings_store;
mod sidebar;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

pub const APP_ID: &str = "com.immurok.Settings";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    app.connect_startup(|app| {
        gio::resources_register_include!("immurok.gresource").expect("gresource compiled by build.rs");
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::IconTheme::for_display(&display).add_resource_path("/com/immurok/Settings/icons");
        }

        register_actions(app);

        // `--gapplication-service` is consumed by GLib itself (IS_SERVICE) and
        // never reaches `command-line`; hold here so the resident instance
        // survives the 10 s inactivity timeout. Leaked on purpose: lives as
        // long as the process.
        if app.flags().contains(gio::ApplicationFlags::IS_SERVICE) {
            std::mem::forget(app.hold());
        }
    });

    // The `.desktop` is `DBusActivatable=true`, so the app menu launches us
    // through `org.freedesktop.Application.Activate`, which emits `activate`
    // — not `command-line`. Without a handler GLib only warns and no window
    // ever appears.
    app.connect_activate(|app| app.activate_action("show", None));

    // HANDLES_COMMAND_LINE: this runs in the *primary* instance for both the
    // first launch and every forwarded invocation.
    app.connect_command_line(|app, cmdline| {
        let args: Vec<String> = cmdline
            .arguments()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        match cli::parse_launch(&args) {
            cli::Launch::QuickFill => app.activate_action("quick-fill", None),
            cli::Launch::Main => app.activate_action("show", None),
            cli::Launch::Service => {
                // The real hold happens in `connect_startup` (see above) —
                // GLib normally consumes `--gapplication-service` itself and
                // never emits `command-line` for the initial service launch,
                // so this arm is only reached if another instance forwards
                // the flag explicitly. Harmless either way: hold again so
                // the resident instance stays alive with no window.
                std::mem::forget(app.hold());
            }
        }
        0
    });

    app.run()
}

fn register_actions(app: &adw::Application) {
    let show = gio::SimpleAction::new("show", None);
    show.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| main_window::MainWindow::present_for(&app)
    ));
    app.add_action(&show);

    let quick = gio::SimpleAction::new("quick-fill", None);
    quick.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| quickfill::open(&app)
    ));
    app.add_action(&quick);

    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| {
            if pages::firmware::updating() {
                // Close instead of returning silently: the main window's
                // close-request guard refuses and toasts the reason, so
                // Ctrl+Q gets an answer in both service and one-shot modes.
                for w in app.windows() {
                    w.close();
                }
                return;
            }
            // In service mode Ctrl+Q must only dismiss the windows: quitting
            // would kill the resident instance, and every later hotkey press
            // would have to start a fresh process instead of being forwarded.
            if app.flags().contains(gio::ApplicationFlags::IS_SERVICE) {
                for w in app.windows() {
                    w.close();
                }
            } else {
                app.quit();
            }
        }
    ));
    app.add_action(&quit);
    app.set_accels_for_action("app.quit", &["<Primary>q"]);
}
