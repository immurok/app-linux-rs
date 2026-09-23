//! immurok-cli — command-line management tool for the immurok daemon.

mod commands;
// Firmware-update orchestration lives in immurok-client now (shared with
// immurok-gui); re-exported under the old module name so `crate::fwupdate::…`
// across commands/ and tui/ keeps working unchanged.
use immurok_client::fwupdate;
// The socket client lives in its own crate now (shared with immurok-gui).
// Re-exported under the old module name so `crate::socket_client::…` paths
// across commands/ and tui/ keep working unchanged.
use immurok_client as socket_client;
// Enrollment step text now lives in immurok-client (shared with the GUI);
// re-exported under the old module name so `crate::enroll_hint::…` in
// commands/ and tui/ keeps working unchanged.
use immurok_client::enroll_hint;
mod tui;

use clap::Parser;

use commands::{
    Commands, DaemonCommands, FpCommands, FwCommands, KeyCommands, PamCommands, SetCommands,
    SlotCommands,
};

/// immurok-cli — manage immurok fingerprint authentication
#[derive(Parser)]
#[command(name = "immurok-cli", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

fn main() {
    let cli = Cli::parse();

    // Pre-pair gate: most operations are meaningless until the device is
    // paired (design doc docs/plans/2026-07-04-linux-daemon-restart-pair-gate-design.md §2).
    if commands::requires_pairing(&cli.command) {
        commands::ensure_paired();
    }

    match cli.command {
        Commands::Status => commands::status::run(),
        Commands::Info => commands::info::run(),
        Commands::Pair => commands::pair::run_pair(),
        Commands::Unpair { slot } => commands::pair::run_unpair(slot),
        Commands::Slot(s) => match s {
            SlotCommands::Status => commands::slot::run_status(),
        },
        Commands::FactoryReset => commands::pair::run_factory_reset(),

        Commands::Fp(fp) => match fp {
            FpCommands::List => commands::fingerprint::run_list(),
            FpCommands::Enroll { slot } => commands::fingerprint::run_enroll(slot),
            FpCommands::Delete { slot } => commands::fingerprint::run_delete(slot),
            FpCommands::Verify => commands::fingerprint::run_verify(),
        },

        Commands::Key(key) => match key {
            KeyCommands::List { category } => commands::keys::run_list(&category),
            KeyCommands::Add { category } => commands::keys::run_add(&category),
            KeyCommands::Delete { category, index } => {
                commands::keys::run_delete(&category, index)
            }
            KeyCommands::ExportSsh { index } => commands::keys::run_export_ssh(index),
            KeyCommands::GenerateSsh { name } => commands::keys::run_generate_ssh(&name),
            KeyCommands::ImportSsh { name, keyfile } => {
                commands::keys::run_import_ssh(&name, &keyfile)
            }
            KeyCommands::Otp { index } => commands::keys::run_otp(index),
            KeyCommands::ImportOtp { file } => commands::keys::run_import_otp(&file),
        },

        Commands::Set(set) => match set {
            SetCommands::Sudo { value } => commands::settings::run_set("sudo", &value),
            SetCommands::Polkit { value } => commands::settings::run_set("polkit", &value),
            SetCommands::Screen { value } => commands::settings::run_set("screen", &value),
            SetCommands::Lock { value } => commands::settings::run_set("lock", &value),
        },

        Commands::Settings => commands::settings::run_show(),

        Commands::Daemon(d) => match d {
            DaemonCommands::Restart => commands::daemon::run_restart(),
        },

        Commands::Fw(fw) => match fw {
            FwCommands::Check { force } => commands::fw::run_check(force),
            FwCommands::Update { yes } => commands::fw::run_update(yes),
            FwCommands::Status => commands::fw::run_status(),
        },

        Commands::Ota { path } => commands::ota::run(&path),

        Commands::Pam(pam) => match pam {
            PamCommands::Install { service } => commands::pam::run_helper("add", &[&service]),
            PamCommands::Remove { service } => commands::pam::run_helper("remove", &[&service]),
            PamCommands::Check => commands::pam::run_check(),
            PamCommands::Repair => commands::pam::run_repair(),
        },

        Commands::Logs => {
            // The daemon writes to /var/log/immurok, owned by its own system
            // user — we cannot read it, so it streams the buffered tail plus
            // live lines over the socket instead.
            use std::io::{BufRead, BufReader};
            match socket_client::open_log_stream() {
                Ok(stream) => {
                    for line in BufReader::new(stream).lines() {
                        match line {
                            Ok(l) => println!("{}", l),
                            Err(_) => break,
                        }
                    }
                }
                Err(e) => eprintln!("{}", e),
            }
        }

        Commands::Tui => {
            if let Err(e) = tui::run() {
                eprintln!("TUI error: {}", e);
                std::process::exit(1);
            }
        }
    }
}
