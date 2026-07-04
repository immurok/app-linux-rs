//! immurok-cli — command-line management tool for the immurok daemon.

mod commands;
mod enroll_hint;
mod socket_client;
mod tui;

use clap::Parser;

use commands::{Commands, FpCommands, KeyCommands, PamCommands, SetCommands};

/// immurok-cli — manage immurok fingerprint authentication
#[derive(Parser)]
#[command(name = "immurok-cli", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Status => commands::status::run(),
        Commands::Info => commands::info::run(),
        Commands::Pair => commands::pair::run_pair(),
        Commands::Unpair => commands::pair::run_unpair(),

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
            SetCommands::Sound { value } => commands::settings::run_sound(&value),
        },

        Commands::Settings => commands::settings::run_show(),

        Commands::Ota { path } => commands::ota::run(&path),

        Commands::Pam(pam) => match pam {
            PamCommands::Install { service } => commands::pam::run_helper("add", &[&service]),
            PamCommands::Remove { service } => commands::pam::run_helper("remove", &[&service]),
            PamCommands::Check => commands::pam::run_check(),
            PamCommands::Repair => commands::pam::run_repair(),
        },

        Commands::Logs => {
            // The daemon's tracing output goes to ~/.immurok/logs.txt —
            // the journal only carries systemd start/stop lines.
            let home = std::env::var("HOME").unwrap_or_default();
            let log_path = std::path::PathBuf::from(&home)
                .join(immurok_common::protocol::IMMUROK_DIR)
                .join(immurok_common::protocol::LOG_FILE);
            let status = std::process::Command::new("tail")
                .args(["-n", "200", "-F"])
                .arg(&log_path)
                .status();
            match status {
                Ok(s) if !s.success() => {
                    eprintln!("tail exited with: {}", s);
                }
                Err(e) => {
                    eprintln!("Failed to run tail: {}", e);
                }
                _ => {}
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
