mod ble;
mod coordinator;
mod keystore;
mod logbuf;
mod ota;
mod screen;
mod session;
mod settings;
mod socket;
mod ssh_agent;
mod suspend;

use tokio::sync::mpsc;
use tracing::info;
use immurok_common::paths;
use immurok_common::protocol;
use immurok_common::security;

/// Command-line options. The daemon takes exactly one flag.
#[derive(Debug, Default, PartialEq, Eq)]
struct Opts {
    /// `--debug`: log at debug level, including every BLE frame in hex.
    debug: bool,
}

/// Parse argv (without the program name). Unknown arguments are an error
/// rather than ignored: a typo in a unit file should fail loudly, not run
/// the daemon in an unintended mode.
fn parse_opts<I: IntoIterator<Item = String>>(args: I) -> Result<Opts, String> {
    let mut opts = Opts::default();
    for arg in args {
        match arg.as_str() {
            "--debug" => opts.debug = true,
            other => return Err(format!("unknown argument: {}", other)),
        }
    }
    Ok(opts)
}

/// The `tracing` filter directive for this run.
///
/// Default is warnings and errors only. `--debug` turns on everything,
/// which includes the raw BLE payloads — key material included — so it is
/// opt-in and never the packaged default. An explicit `RUST_LOG` still wins
/// over both, for one-off targeted filters.
fn log_directive(debug: bool, rust_log: Option<&str>) -> String {
    match rust_log.map(str::trim).filter(|s| !s.is_empty()) {
        Some(explicit) => explicit.to_string(),
        None if debug => "immurok=debug".to_string(),
        None => "immurok=warn".to_string(),
    }
}

/// Log to `<log_dir>/daemon.log`, falling back to stderr (→ journald) when
/// that cannot be opened. Never fatal: a daemon that cannot write its log must
/// still be able to authenticate.
fn init_logging(opts: &Opts) {
    let directive = log_directive(opts.debug, std::env::var("RUST_LOG").ok().as_deref());
    let filter = tracing_subscriber::EnvFilter::try_new(&directive)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("immurok=warn"));

    let _ = std::fs::create_dir_all(paths::log_dir());
    let log_path = paths::daemon_log();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path);
    let opened = file.is_ok();

    // Everything goes through the same sink: the file (for post-mortem and
    // for root) and an in-memory ring the CLI/TUI can subscribe to over the
    // socket, since they can no longer read the file themselves.
    let writer = logbuf::install(file.ok());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(writer)
        .with_ansi(false)
        .init();

    if !opened {
        tracing::warn!(
            "cannot open {} — log file disabled, socket log stream still works",
            log_path.display()
        );
    }
    if opts.debug {
        // Logged at warn so the line survives any later filter and the log
        // itself says it contains raw device traffic.
        tracing::warn!(
            "debug logging enabled (--debug): BLE frames are logged in hex, \
             including secrets written to or read from the device"
        );
    }
}

#[tokio::main]
async fn main() {
    let opts = match parse_opts(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("immurok-daemon: {}\nusage: immurok-daemon [--debug]", e);
            std::process::exit(2);
        }
    };
    init_logging(&opts);

    info!("immurok-daemon starting");

    // State lives at a machine-level path (systemd StateDirectory, or the
    // compiled-in fallback) — the daemon runs as the `immurok` system user
    // with ProtectHome=yes and must never derive anything from $HOME.
    let state_dir = paths::state_dir();
    std::fs::create_dir_all(&state_dir)
        .unwrap_or_else(|e| panic!("cannot create {}: {}", state_dir.display(), e));
    // systemd's StateDirectory= already lands on 0700; a hand-started daemon
    // must not leave the pairing key readable by the rest of the machine.
    if let Err(e) = std::fs::set_permissions(
        &state_dir,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    ) {
        tracing::warn!("cannot chmod 0700 {}: {}", state_dir.display(), e);
    }

    let runtime_dir = paths::runtime_dir();
    std::fs::create_dir_all(&runtime_dir)
        .unwrap_or_else(|e| panic!("cannot create {}: {}", runtime_dir.display(), e));

    let pairing = security::load_pairing().unwrap_or(None);
    let user_settings = settings::Settings::load(&state_dir.join(protocol::SETTINGS_FILE));

    // 启动 PAM 自检：按已启用功能检查 /etc/pam.d，仅日志不修复（修复需 pkexec/TTY）。
    {
        use immurok_common::pam::pam_line_present;
        let st = |svc: &str| if pam_line_present(svc) { "OK" } else { "MISSING" };
        let sudo = if user_settings.unlock_sudo { st("sudo") } else { "off" };
        let polkit = if user_settings.unlock_polkit { st("polkit-1") } else { "off" };
        info!("PAM status: sudo={} polkit={}", sudo, polkit);
        let missing = (user_settings.unlock_sudo && !pam_line_present("sudo"))
            || (user_settings.unlock_polkit && !pam_line_present("polkit-1"));
        if missing {
            tracing::warn!("PAM config incomplete — run 'immurok-cli pam repair'");
        }
    }

    let (ble_cmd_tx, ble_cmd_rx) = mpsc::channel(32);
    let coord = coordinator::Coordinator::new(ble_cmd_tx, state_dir.clone());
    {
        let mut p = coord.pairing.write().await;
        *p = pairing;
        let mut s = coord.settings.write().await;
        *s = user_settings;
    }

    let pam_sock = paths::pam_socket();
    let agent_sock = paths::agent_socket();

    tokio::select! {
        _ = ble::run(coord.clone(), ble_cmd_rx) => {},
        _ = socket::serve(coord.clone(), &pam_sock) => {},
        _ = ssh_agent::serve(coord.clone(), &agent_sock) => {},
        _ = screen::monitor(coord.clone()) => {},
        _ = suspend::monitor(coord.clone()) => {},
        _ = tokio::signal::ctrl_c() => {
            info!("Received shutdown signal");
        },
    }

    let _ = std::fs::remove_file(&pam_sock);
    let _ = std::fs::remove_file(&agent_sock);
    info!("immurok-daemon stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_args_is_quiet() {
        assert_eq!(parse_opts(args(&[])), Ok(Opts { debug: false }));
    }

    #[test]
    fn debug_flag() {
        assert_eq!(parse_opts(args(&["--debug"])), Ok(Opts { debug: true }));
    }

    #[test]
    fn unknown_arg_is_rejected() {
        assert!(parse_opts(args(&["--verbose"])).is_err());
        assert!(parse_opts(args(&["--debug", "extra"])).is_err());
    }

    #[test]
    fn default_level_is_warn() {
        assert_eq!(log_directive(false, None), "immurok=warn");
    }

    #[test]
    fn debug_flag_enables_debug_level() {
        assert_eq!(log_directive(true, None), "immurok=debug");
    }

    #[test]
    fn rust_log_overrides_flag() {
        assert_eq!(log_directive(false, Some("immurok=trace")), "immurok=trace");
        assert_eq!(log_directive(true, Some("immurok=error")), "immurok=error");
    }

    #[test]
    fn empty_rust_log_is_ignored() {
        assert_eq!(log_directive(true, Some("  ")), "immurok=debug");
        assert_eq!(log_directive(false, Some("")), "immurok=warn");
    }
}
