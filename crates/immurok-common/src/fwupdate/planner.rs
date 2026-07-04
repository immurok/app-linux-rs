//! Update planning — ported from FirmwareUpdateKit/UpdatePlanner.swift.
//!
//! dev >= latest            → UpToDate
//! dev >= min_direct(1.6.0) → Direct   (one hop, push latest)
//! dev < min_direct:
//!   latest == min_direct   → BridgeOnly (one hop, push bridge)
//!   otherwise              → TwoHops    (bridge → latest)

use super::version::FirmwareVersion;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatePlan {
    UpToDate,
    Direct,
    BridgeOnly,
    TwoHops,
    /// Version string failed to parse — caller should retry/report.
    Unknown,
}

/// Signing-era gate: below this version the device only accepts v1 (HMAC)
/// packages, so it must go through the bridge firmware first.
pub const FALLBACK_MIN_DIRECT: &str = "1.6.0";

pub fn plan(device: &str, latest: &str, min_direct: Option<&str>) -> UpdatePlan {
    let dev = FirmwareVersion::parse(device);
    let tgt = FirmwareVersion::parse(latest);
    let gate = FirmwareVersion::parse(min_direct.unwrap_or(FALLBACK_MIN_DIRECT));
    let (dev, tgt, gate) = match (dev, tgt, gate) {
        (Some(d), Some(t), Some(g)) => (d, t, g),
        _ => return UpdatePlan::Unknown,
    };
    if dev >= tgt {
        UpdatePlan::UpToDate
    } else if dev >= gate {
        UpdatePlan::Direct
    } else if tgt == gate {
        UpdatePlan::BridgeOnly
    } else {
        UpdatePlan::TwoHops
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_device_two_hops() {
        assert_eq!(plan("1.3.11", "1.6.1", Some("1.6.0")), UpdatePlan::TwoHops);
    }

    #[test]
    fn bridge_device_one_hop() {
        assert_eq!(plan("1.6.0", "1.6.1", Some("1.6.0")), UpdatePlan::Direct);
    }

    #[test]
    fn up_to_date() {
        assert_eq!(plan("1.6.1", "1.6.1", Some("1.6.0")), UpdatePlan::UpToDate);
        // Device newer than latest (local dev build) also counts as up to date.
        assert_eq!(plan("1.7.0", "1.6.1", Some("1.6.0")), UpdatePlan::UpToDate);
    }

    #[test]
    fn target_is_bridge() {
        assert_eq!(plan("1.3.11", "1.6.0", Some("1.6.0")), UpdatePlan::BridgeOnly);
    }

    #[test]
    fn min_direct_fallback() {
        assert_eq!(plan("1.5.5", "1.6.1", None), UpdatePlan::TwoHops);
        assert_eq!(plan("1.6.0", "1.6.1", None), UpdatePlan::Direct);
    }

    #[test]
    fn unparsable_versions() {
        assert_eq!(plan("garbage", "1.6.1", None), UpdatePlan::Unknown);
        assert_eq!(plan("1.6.0", "", None), UpdatePlan::Unknown);
    }
}
