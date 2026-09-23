//! Daemon error strings → user-facing text.
//!
//! The literals come from `immurok-daemon/src/ble.rs` (`FP-gate timeout` /
//! `FP-gate cancelled` / `FP-gate failed: 0x..`) and `socket.rs`
//! (`ERROR:BUSY`, `ERROR:NOT_CONNECTED`, `ERROR:INVALID_SLOT`,
//! `ERROR:DUAL_HOST_UNSUPPORTED`, `ERROR:SLOT_CLEAR_REFUSED`, and the
//! `ERROR:<KIND>_FAILED:<reason>` wrappers). `Read failed: …` is our own
//! socket read timeout (`immurok-client/src/daemon.rs`).

const WRAPPERS: [&str; 7] = [
    "ERROR:ENROLL_FAILED:",
    "ERROR:DELETE_FAILED:",
    "ERROR:SLOT_CLEAR_FAILED:",
    "ERROR:OTP_FAILED:",
    "ERROR:READ_FAILED:",
    "ERROR:VERIFY_FAILED:",
    "ERROR:FACTORY_RESET_FAILED:",
];

fn map(s: &str) -> Option<&'static str> {
    Some(match s {
        r if r.contains("FP-gate timeout") || r.contains("Read failed") => {
            "Timed out waiting for a fingerprint touch."
        }
        r if r.contains("FP-gate cancelled") => "Cancelled",
        r if r.contains("FP-gate failed") => "Fingerprint didn't match after multiple attempts.",
        r if r.contains("NOT_CONNECTED") => "Device not connected",
        r if r.contains("NOT_VERIFIED") => "Device not verified with this computer",
        r if r.contains("BUSY") => "Device busy, try again",
        r if r.contains("INVALID_SLOT") => "Invalid slot",
        r if r.contains("DUAL_HOST_UNSUPPORTED") => "This firmware does not support two hosts",
        r if r.contains("SLOT_CLEAR_REFUSED") => "The device refused to clear the slot",
        r if r.contains("PAIRING_IN_PROGRESS") => "Pairing is already in progress",
        _ => return None,
    })
}

/// Best-effort translation; unknown strings pass through verbatim (with the
/// `ERROR:<KIND>_FAILED:` wrapper intact so the raw code stays visible).
pub fn friendly(raw: &str) -> String {
    if let Some(m) = map(raw) {
        return m.to_string();
    }
    for w in WRAPPERS {
        if let Some(inner) = raw.strip_prefix(w) {
            if let Some(m) = map(inner) {
                return m.to_string();
            }
        }
    }
    raw.to_string()
}

/// Firmware-update errors (`immurok_client::fwupdate::error`) → user text.
pub fn fw_friendly(e: &immurok_client::fwupdate::error::FwUpdateError) -> String {
    use immurok_client::fwupdate::error::FwUpdateError as E;
    match e {
        E::LowBattery => "Battery below 30 % — charge the device first".into(),
        E::ReconnectTimeout(_) => {
            "Device did not come back after the update; power-cycle it and check again".into()
        }
        E::ManifestFetch(_) | E::Download(_) => "Could not reach the update server".into(),
        E::ManifestSchema(_) => "Update server returned an invalid manifest".into(),
        E::Sha256Mismatch | E::PackageInvalid(_) | E::HeaderRejected(_) | E::SignatureRejected => {
            format!("Firmware package rejected: {e}")
        }
        E::Preflight(msg) if msg.contains("not connected") => "Device not connected".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_outcomes() {
        assert_eq!(friendly("ERROR:OTP_FAILED:FP-gate timeout"), "Timed out waiting for a fingerprint touch.");
        assert_eq!(friendly("ERROR:READ_FAILED:FP-gate timeout"), "Timed out waiting for a fingerprint touch.");
        assert_eq!(friendly("ERROR:DELETE_FAILED:FP-gate timeout"), "Timed out waiting for a fingerprint touch.");
        assert_eq!(
            friendly("Read failed: Resource temporarily unavailable (os error 11)"),
            "Timed out waiting for a fingerprint touch."
        );
        assert_eq!(friendly("ERROR:ENROLL_FAILED:FP-gate cancelled"), "Cancelled");
        assert_eq!(friendly("ERROR:SLOT_CLEAR_FAILED:FP-gate failed: 0x21"), "Fingerprint didn't match after multiple attempts.");
        assert_eq!(friendly("ERROR:OTP_FAILED:FP-gate failed"), "Fingerprint didn't match after multiple attempts.");
    }

    #[test]
    fn plain_codes() {
        assert_eq!(friendly("ERROR:BUSY"), "Device busy, try again");
        assert_eq!(friendly("ERROR:NOT_CONNECTED"), "Device not connected");
        assert_eq!(friendly("ERROR:NOT_VERIFIED"), "Device not verified with this computer");
        assert_eq!(friendly("ERROR:INVALID_SLOT"), "Invalid slot");
        assert_eq!(friendly("ERROR:DUAL_HOST_UNSUPPORTED"), "This firmware does not support two hosts");
        assert_eq!(friendly("ERROR:SLOT_CLEAR_REFUSED"), "The device refused to clear the slot");
    }

    #[test]
    fn unknown_passes_through_with_wrapper() {
        assert_eq!(friendly("ERROR:OTP_FAILED:0x21"), "ERROR:OTP_FAILED:0x21");
        assert_eq!(friendly("ERROR:ENROLL_FAILED:0x28"), "ERROR:ENROLL_FAILED:0x28");
    }

    #[test]
    fn firmware_errors() {
        use immurok_client::fwupdate::error::FwUpdateError as E;
        assert_eq!(fw_friendly(&E::LowBattery), "Battery below 30 % — charge the device first");
        assert_eq!(
            fw_friendly(&E::ReconnectTimeout("x".into())),
            "Device did not come back after the update; power-cycle it and check again"
        );
        assert_eq!(fw_friendly(&E::ManifestFetch("dns".into())), "Could not reach the update server");
        assert_eq!(fw_friendly(&E::Download("404".into())), "Could not reach the update server");
        assert_eq!(fw_friendly(&E::ManifestSchema("bad".into())), "Update server returned an invalid manifest");
        assert!(fw_friendly(&E::Sha256Mismatch).starts_with("Firmware package rejected: "));
        assert!(fw_friendly(&E::SignatureRejected).starts_with("Firmware package rejected: "));
        assert_eq!(fw_friendly(&E::Preflight("device not connected".into())), "Device not connected");
        assert_eq!(
            fw_friendly(&E::Preflight("bad target version 'x'".into())),
            E::Preflight("bad target version 'x'".into()).to_string()
        );
    }
}
