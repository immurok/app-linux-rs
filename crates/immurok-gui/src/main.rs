//! immurok-gui — graphical settings app + hotkey quick-fill panel.
//!
//! One `adw::Application` with id `com.immurok.Settings`. GApplication's
//! single-instance machinery does the rest: a second `immurok-gui
//! --quick-fill` forwards its command line to the running instance, which
//! fires the `quick-fill` action. Portal shortcuts (phase 2) and X11 key
//! grabs fire the same action, so every entry path converges here.

rust_i18n::i18n!("locales", fallback = "en");

mod cli;
mod device_illustration;
mod enroll_dialog;
mod errors;
mod filter;
mod gate_dialog;
mod i18n;
mod key_add_dialog;
mod link_guide;
mod main_window;
mod output;
mod pages;
mod quickfill;
mod settings_store;
mod setup_wizard;
mod sidebar;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

pub const APP_ID: &str = "com.immurok.Settings";

fn main() -> glib::ExitCode {
    i18n::init_system_locale();

    // A resident instance started before `make install` keeps running the
    // old binary; GApplication would forward this launch to it. Only the
    // service launch itself skips the check (it is about to become primary).
    if !std::env::args().any(|a| a == "--gapplication-service") {
        retire_stale_primary();
    }

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    app.connect_startup(|app| {
        gio::resources_register_include!("immurok.gresource").expect("gresource compiled by build.rs");
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::IconTheme::for_display(&display).add_resource_path("/com/immurok/Settings/icons");
        }
        // App icon (same artwork as the macOS AppIcon). Installed into
        // hicolor by the package / `make install`; the gresource copy covers
        // a build run straight from `target/`.
        gtk::Window::set_default_icon_name("com.immurok.Settings");

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
            cli::Launch::Setup => app.activate_action("setup", None),
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

    let setup = gio::SimpleAction::new("setup", None);
    setup.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| setup_wizard::SetupWizard::open(&app)
    ));
    app.add_action(&setup);

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

    // Asked over D-Bus (`org.freedesktop.Application.ActivateAction retire`)
    // by a newer `immurok-gui` (see `retire_stale_primary`) or by
    // `scripts/retire-gui.sh` after `make install`. Unlike `quit` it ends the
    // resident instance too; the next launch starts the new binary. Refused
    // while a firmware update runs — dying mid-OTA is worse than one more
    // session of the old build.
    let retire = gio::SimpleAction::new("retire", None);
    retire.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| {
            if pages::firmware::updating() {
                eprintln!("immurok-gui: retire refused: firmware update in progress");
                return;
            }
            app.quit();
        }
    ));
    app.add_action(&retire);
}

const APP_OBJECT_PATH: &str = "/com/immurok/Settings";

/// If the current primary instance runs a binary that has since been replaced
/// on disk (`/proc/<pid>/exe` → "… (deleted)"), ask it to `retire` and wait for
/// the bus name to be released, so `app.run()` makes this process the primary.
/// Otherwise every `immurok-gui` after an upgrade keeps landing in the old
/// build until logout — fixes and new pages invisible, no hint why.
///
/// Best effort: any D-Bus hiccup just falls through to normal forwarding.
fn retire_stale_primary() {
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else { return };
    let dbus = |method: &str, args: glib::Variant| {
        bus.call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            method,
            Some(&args),
            None,
            gio::DBusCallFlags::NONE,
            1000,
            gio::Cancellable::NONE,
        )
    };
    // Errors here mean nobody owns the name: this process becomes primary.
    let Ok(reply) = dbus("GetConnectionUnixProcessID", (APP_ID,).to_variant()) else { return };
    let Some(pid) = reply.child_value(0).get::<u32>() else { return };
    if pid == std::process::id() {
        return;
    }
    let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) else { return };
    if !exe.to_string_lossy().ends_with(" (deleted)") {
        return;
    }

    let app_call = |iface: &str, method: &str, args: Option<glib::Variant>| {
        bus.call_sync(
            Some(APP_ID),
            APP_OBJECT_PATH,
            iface,
            method,
            args.as_ref(),
            None,
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
        )
    };
    // Builds before the `retire` action existed can only be killed.
    let has_retire = app_call("org.gtk.Actions", "List", None)
        .ok()
        .and_then(|r| r.child_value(0).get::<Vec<String>>())
        .is_some_and(|names| names.iter().any(|n| n == "retire"));
    if !has_retire {
        eprintln!(
            "immurok-gui: the resident instance (pid {pid}) runs a replaced binary and predates \
             the retire action; stop it with: pkill -f 'immurok-gui --gapplication-service'"
        );
        return;
    }

    eprintln!("immurok-gui: resident instance (pid {pid}) runs a replaced binary; asking it to exit");
    let platform_data: std::collections::HashMap<String, glib::Variant> = std::collections::HashMap::new();
    let _ = app_call(
        "org.freedesktop.Application",
        "ActivateAction",
        Some(("retire", Vec::<glib::Variant>::new(), platform_data).to_variant()),
    );
    for _ in 0..30 {
        if let Ok(r) = dbus("NameHasOwner", (APP_ID,).to_variant()) {
            if r.child_value(0).get::<bool>() == Some(false) {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    eprintln!("immurok-gui: resident instance did not exit (firmware update running?); using it as is");
}
