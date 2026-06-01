//! immurok-cli — command-line management tool for the immurok daemon.

mod commands;
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
            PamCommands::Install { service } => run_pam_helper("add", &service),
            PamCommands::Remove { service } => run_pam_helper("remove", &service),
        },

        Commands::Logs => {
            let status = std::process::Command::new("journalctl")
                .args(["--user", "-u", "immurok-daemon", "-f", "--no-pager"])
                .status();
            match status {
                Ok(s) if !s.success() => {
                    eprintln!("journalctl exited with: {}", s);
                }
                Err(e) => {
                    eprintln!("Failed to run journalctl: {}", e);
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

/// Run the PAM helper via pkexec.
fn run_pam_helper(action: &str, service: &str) {
    // Find immurok-pam-helper
    let helper = find_pam_helper();
    let helper = match helper {
        Some(h) => h,
        None => {
            eprintln!("Error: immurok-pam-helper not found in PATH or next to this binary.");
            std::process::exit(1);
        }
    };

    println!("Running: pkexec {} {} {}", helper, action, service);

    let status = std::process::Command::new("pkexec")
        .args([&helper, action, service])
        .status();

    match status {
        Ok(s) if s.success() => {
            println!(
                "\x1b[32mPAM {} for '{}' successful.\x1b[0m",
                if action == "add" {
                    "installation"
                } else {
                    "removal"
                },
                service
            );
        }
        Ok(s) => {
            eprintln!(
                "\x1b[31mPAM helper failed (exit code: {})\x1b[0m",
                s.code().unwrap_or(-1)
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to run pkexec: {}", e);
            std::process::exit(1);
        }
    }
}

fn find_pam_helper() -> Option<String> {
    // Try next to our own binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("immurok-pam-helper");
            if candidate.exists() {
                return Some(candidate.to_string_lossy().to_string());
            }
        }
    }

    // Try PATH
    if let Ok(output) = std::process::Command::new("which")
        .arg("immurok-pam-helper")
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(path);
            }
        }
    }

    None
}
