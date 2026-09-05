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

/// Log to `<log_dir>/daemon.log`, falling back to stderr (→ journald) when
/// that cannot be opened. Never fatal: a daemon that cannot write its log must
/// still be able to authenticate.
fn init_logging() {
    fn filter() -> tracing_subscriber::EnvFilter {
        tracing_subscriber::EnvFilter::from_default_env()
            .add_directive("immurok=info".parse().unwrap())
    }

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
        .with_env_filter(filter())
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
}

#[tokio::main]
async fn main() {
    init_logging();

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
