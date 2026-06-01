//! CLI subcommand definitions and shared helpers.

pub mod fingerprint;
pub mod info;
pub mod keys;
pub mod ota;
pub mod pair;
pub mod settings;
pub mod status;

use clap::Subcommand;

/// Top-level CLI subcommands.
#[derive(Subcommand)]
pub enum Commands {
    /// Connection status, pairing, battery, firmware version
    Status,

    /// Detailed device information
    Info,

    /// Start ECDH pairing
    Pair,

    /// Clear pairing + factory reset
    Unpair,

    /// Fingerprint management
    #[command(subcommand)]
    Fp(FpCommands),

    /// Key management (SSH/OTP/API)
    #[command(subcommand)]
    Key(KeyCommands),

    /// Toggle settings
    #[command(subcommand)]
    Set(SetCommands),

    /// Show all settings
    Settings,

    /// OTA firmware upgrade
    Ota {
        /// Path to .imfw firmware file
        path: String,
    },

    /// PAM configuration
    #[command(subcommand)]
    Pam(PamCommands),

    /// View daemon logs (journalctl)
    Logs,

    /// Interactive TUI panel
    Tui,
}

/// Fingerprint subcommands.
#[derive(Subcommand)]
pub enum FpCommands {
    /// List enrolled fingerprints
    List,
    /// Enroll fingerprint to a slot (0-4)
    Enroll {
        /// Slot number (0-4)
        slot: u8,
    },
    /// Delete fingerprint from a slot
    Delete {
        /// Slot number (0-4)
        slot: u8,
    },
    /// Verify fingerprint (test)
    Verify,
}

/// Key management subcommands.
#[derive(Subcommand)]
pub enum KeyCommands {
    /// List keys in a category
    List {
        /// Category: ssh, otp, api
        category: String,
    },
    /// Add a key (interactive)
    Add {
        /// Category: ssh, otp, api
        category: String,
    },
    /// Delete a key
    Delete {
        /// Category: ssh, otp, api
        category: String,
        /// Key index
        index: u8,
    },
    /// Export SSH public key
    ExportSsh {
        /// Key index
        index: u8,
    },
    /// Generate SSH keypair on device
    GenerateSsh {
        /// Key name
        name: String,
    },
    /// Import an existing SSH private key (ECDSA P-256 only)
    ImportSsh {
        /// Key name (max 15 chars)
        name: String,
        /// Path to private key file (OpenSSH or SEC1 PEM)
        keyfile: String,
    },
    /// Get TOTP code
    Otp {
        /// Key index
        index: u8,
    },
    /// Bulk-import OTP secrets from CSV (otpauth:// per line) or andOTP JSON
    /// backup. Only standard TOTP / HMAC-SHA1 / 6-digit / 30-second entries
    /// are imported; others are skipped.
    ImportOtp {
        /// Path to .csv or .json file
        file: String,
    },
}

/// Settings toggle subcommands.
#[derive(Subcommand)]
pub enum SetCommands {
    /// Toggle sudo fingerprint auth
    Sudo {
        /// on or off
        value: String,
    },
    /// Toggle polkit fingerprint auth
    Polkit {
        /// on or off
        value: String,
    },
    /// Toggle screen unlock
    Screen {
        /// on or off
        value: String,
    },
    /// Toggle long-press device button → lock screen
    Lock {
        /// on or off
        value: String,
    },
    /// Sound played on screen unlock (empty / "off" / "none" = silent).
    /// Common names: service-login, complete, bell, message
    Sound {
        /// freedesktop sound name, or empty/off/none for silent
        value: String,
    },
}

/// PAM subcommands.
#[derive(Subcommand)]
pub enum PamCommands {
    /// Install PAM config for a service
    Install {
        /// PAM service name (e.g. sudo, polkit-1, gdm-password)
        service: String,
    },
    /// Remove PAM config for a service
    Remove {
        /// PAM service name
        service: String,
    },
}

// ── Shared helpers ───────────────────────────────────────────

/// Parse "on"/"off" to "1"/"0". Returns None for invalid input.
pub fn parse_on_off(s: &str) -> Option<&'static str> {
    match s.to_lowercase().as_str() {
        "on" | "1" | "true" => Some("1"),
        "off" | "0" | "false" => Some("0"),
        _ => None,
    }
}

/// Print an error message and exit.
pub fn error_exit(msg: &str) -> ! {
    eprintln!("Error: {}", msg);
    std::process::exit(1);
}
