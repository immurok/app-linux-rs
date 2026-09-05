//! CLI subcommand definitions and shared helpers.

pub mod daemon;
pub mod fingerprint;
pub mod fw;
pub mod info;
pub mod keys;
pub mod ota;
pub mod pair;
pub mod pam;
pub mod settings;
pub mod slot;
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

    /// Clear this computer's pairing on the device (keeps fingerprints and keys)
    Unpair {
        /// Clear the OTHER host's slot (1 or 2) instead of this computer's.
        /// Requires touching an enrolled fingerprint on the device.
        #[arg(long)]
        slot: Option<u8>,
    },

    /// Dual-host slot inspection
    #[command(subcommand)]
    Slot(SlotCommands),

    /// Wipe the device: all fingerprints, all keys, both host pairings
    FactoryReset,

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

    /// Background daemon service management
    #[command(subcommand)]
    Daemon(DaemonCommands),

    /// Firmware update from immurok.com (check / install)
    #[command(subcommand)]
    Fw(FwCommands),

    /// OTA firmware upgrade
    Ota {
        /// Path to .imfw firmware file
        path: String,
    },

    /// PAM configuration
    #[command(subcommand)]
    Pam(PamCommands),

    /// Tail daemon logs (~/.immurok/logs.txt)
    Logs,

    /// Interactive TUI panel
    Tui,
}

/// Fingerprint subcommands.
#[derive(Subcommand)]
pub enum FpCommands {
    /// List enrolled fingerprints
    List,
    /// Enroll fingerprint to a slot (0-4 auth, 5 = host switch)
    Enroll {
        /// Slot number: 0-4 authenticate, 5 = host-switch finger
        slot: u8,
    },
    /// Delete fingerprint from a slot
    Delete {
        /// Slot number: 0-4 authenticate, 5 = host-switch finger
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
    /// Check PAM config against enabled features (exit 1 if anything missing)
    Check,
    /// Install any missing PAM config for enabled features (one pkexec)
    Repair,
}

/// Firmware update subcommands.
#[derive(Subcommand)]
pub enum FwCommands {
    /// Check for firmware updates
    Check {
        /// Bypass the 24h check throttle
        #[arg(long)]
        force: bool,
    },
    /// Download and install the latest firmware
    Update {
        /// Skip the confirmation prompt
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Show last check result and pending resume state
    Status,
}

/// Daemon service subcommands.
#[derive(Subcommand)]
pub enum DaemonCommands {
    /// Restart the daemon (systemctl --user restart immurok-daemon)
    Restart,
}

/// Dual-host slot subcommands. Clearing a slot is `unpair` / `unpair --slot N`
/// — deliberately not duplicated here, so there is one way to do it.
#[derive(Subcommand)]
pub enum SlotCommands {
    /// Show which host slots are occupied and which one is active
    Status,
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

/// Pre-pair gate (design doc §2): before the device is paired, only
/// firmware update (fw/ota), daemon restart, pair itself, and read-only
/// diagnostics (status/logs/tui entry) are meaningful. Everything else is
/// rejected with a hint. Exhaustive match: adding a new Commands variant
/// forces an explicit decision here.
pub fn requires_pairing(cmd: &Commands) -> bool {
    match cmd {
        // Whitelist — usable before pairing
        Commands::Status
        | Commands::Pair
        | Commands::Logs
        | Commands::Tui
        | Commands::Ota { .. }
        | Commands::Fw(_)
        // Slot status is readable while unpaired — that is exactly how a
        // new host learns it is the second one.
        | Commands::Slot(_)
        // `settings` is read-only (run_show) and everything it prints is about
        // *this host*, not the device: feature toggles, whether the daemon is
        // privilege-separated, and which PAM services are wired up. A fresh
        // install that has not paired yet is exactly when someone wants to
        // check those — gating it behind pairing hid the isolation line from
        // the one situation it was added for.
        | Commands::Settings
        // Both recovery commands are deliberately NOT gated. Their real
        // authority lives on the device (SLOT_CLEAR runs on the slot's own
        // link; FACTORY_RESET is fingerprint-gated by the firmware, which
        // never reads a payload or checks an HMAC on it), and the daemon
        // validates both properly. Gating them on *local* pairing state
        // makes the documented recovery path unfollowable: `unpair` while
        // the device is out of range answers CLEARED_LOCAL_ONLY and tells
        // the user to re-run it while connected — which the gate would then
        // refuse, because local pairing is exactly what that run dropped.
        | Commands::Unpair { .. }
        | Commands::FactoryReset
        | Commands::Daemon(_) => false,

        // Gated — device-facing / config operations need a paired device
        Commands::Info
        | Commands::Fp(_)
        | Commands::Key(_)
        | Commands::Set(_)
        | Commands::Pam(_) => true,
    }
}

/// Yes/no prompt on stdin. Anything other than y/yes — including EOF —
/// counts as no, so a piped or closed stdin never confirms a destructive
/// action by accident.
pub fn confirm(question: &str) -> bool {
    use std::io::Write;
    print!("{} [y/N] ", question);
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Strong confirmation: only the literal word `yes` counts, and it is never
/// abbreviated to a keystroke. Reserved for actions that can destroy data
/// which exists nowhere else — a stray `y` must not be able to trigger them.
/// EOF counts as no.
pub fn confirm_literal_yes(prompt: &str) -> bool {
    println!("{}", prompt);
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    line.trim() == "yes"
}

/// Query PAIR:STATUS on a fresh connection; exit with a hint if unpaired.
/// A daemon connection failure is reported as-is (NOT as "unpaired").
pub fn ensure_paired() {
    let rsp = match crate::socket_client::DaemonClient::connect()
        .and_then(|mut c| c.send("PAIR:STATUS"))
    {
        Ok(r) => r,
        Err(e) => error_exit(&e),
    };
    // Response format: "OK:PAIRED" / "OK:UNPAIRED" (Response::Ok serialization,
    // see immurok-common/src/socket_proto.rs)
    let paired = rsp.split(':').nth(1) == Some("PAIRED");
    if !paired {
        error_exit(
            "Device not paired. Run 'immurok-cli pair' first. \
             (Before pairing only fw/ota, daemon restart, status and logs are available.)",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_gate_classification() {
        // Whitelist — usable before pairing (design doc §2)
        assert!(!requires_pairing(&Commands::Status));
        // 这条曾经断言 settings 必须被闸住（写白名单时它只显示设备相关的功能
        // 开关）。加入「daemon 是否特权分离」和 PAM 装配状态之后，它的内容变成
        // 了纯本机信息，而新装未配对恰恰是最需要看这两项的时刻。改的是只读命令
        // 的可见性，写操作（Set）仍然闸住。
        assert!(!requires_pairing(&Commands::Settings));
        assert!(!requires_pairing(&Commands::Pair));
        assert!(!requires_pairing(&Commands::Logs));
        assert!(!requires_pairing(&Commands::Tui));
        assert!(!requires_pairing(&Commands::Ota { path: "x.imfw".into() }));
        assert!(!requires_pairing(&Commands::Fw(FwCommands::Check { force: false })));
        assert!(!requires_pairing(&Commands::Fw(FwCommands::Update { yes: false })));
        assert!(!requires_pairing(&Commands::Fw(FwCommands::Status)));
        assert!(!requires_pairing(&Commands::Daemon(DaemonCommands::Restart)));
        assert!(!requires_pairing(&Commands::Slot(SlotCommands::Status)));

        // Recovery commands — ungated on purpose. `unpair` run while the
        // device was unreachable clears local pairing and asks to be re-run
        // while connected; gating it on local pairing would make that
        // instruction impossible to follow. Factory reset carries no local
        // authority at all (the firmware gates it on a fingerprint), so a
        // local-state gate only blocks the user, never an attacker.
        assert!(!requires_pairing(&Commands::Unpair { slot: None }));
        assert!(!requires_pairing(&Commands::Unpair { slot: Some(2) }));
        assert!(!requires_pairing(&Commands::FactoryReset));

        // Gated — rejected until paired
        assert!(requires_pairing(&Commands::Info));
        assert!(requires_pairing(&Commands::Fp(FpCommands::List)));
        assert!(requires_pairing(&Commands::Key(KeyCommands::List { category: "ssh".into() })));
        assert!(requires_pairing(&Commands::Set(SetCommands::Sudo { value: "on".into() })));
        assert!(requires_pairing(&Commands::Pam(PamCommands::Check)));
    }
}
