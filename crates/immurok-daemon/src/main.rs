mod ble;
mod coordinator;
mod keystore;
mod ota;
mod screen;
mod settings;
mod socket;
mod ssh_agent;
mod suspend;

use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::info;
use immurok_common::protocol;
use immurok_common::security;

#[tokio::main]
async fn main() {
    let home_dir = std::env::var("HOME").expect("HOME not set");
    let log_path = PathBuf::from(&home_dir).join(protocol::IMMUROK_DIR).join("logs.txt");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("cannot open log file");

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("immurok=info".parse().unwrap()),
        )
        .with_target(false)
        .with_writer(std::sync::Mutex::new(log_file))
        .with_ansi(false)
        .init();

    info!("immurok-daemon starting");

    let home = std::env::var("HOME").expect("HOME not set");
    let immurok_dir = PathBuf::from(&home).join(protocol::IMMUROK_DIR);
    std::fs::create_dir_all(&immurok_dir).expect("cannot create ~/.immurok");

    // Use XDG_RUNTIME_DIR for sockets — accessible even with ProtectHome=yes (polkitd)
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })));
    let runtime_immurok = runtime_dir.join("immurok");
    std::fs::create_dir_all(&runtime_immurok).expect("cannot create runtime dir");

    let pairing = security::load_pairing().unwrap_or(None);
    let user_settings = settings::Settings::load(&immurok_dir.join(protocol::SETTINGS_FILE));

    let (ble_cmd_tx, ble_cmd_rx) = mpsc::channel(32);
    let coord = coordinator::Coordinator::new(ble_cmd_tx, immurok_dir.clone());
    {
        let mut p = coord.pairing.write().await;
        *p = pairing;
        let mut s = coord.settings.write().await;
        *s = user_settings;
    }

    let pam_sock = runtime_immurok.join(protocol::PAM_SOCKET_NAME);
    let agent_sock = runtime_immurok.join(protocol::AGENT_SOCKET_NAME);

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
