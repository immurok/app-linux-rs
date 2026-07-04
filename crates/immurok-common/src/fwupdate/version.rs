//! 3-segment semver for firmware versions (device DIS 2A26 / manifest).
//! Ported from FirmwareUpdateKit/FirmwareVersion.swift.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FirmwareVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl FirmwareVersion {
    /// Parse "MAJOR.MINOR.PATCH" — exactly 3 numeric segments.
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 3 {
            return None;
        }
        Some(Self {
            major: parts[0].parse().ok()?,
            minor: parts[1].parse().ok()?,
            patch: parts[2].parse().ok()?,
        })
    }
}

impl fmt::Display for FirmwareVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Device GET_STATUS reports 4-segment "a.b.c.build" — trim to 3 segments.
/// Anything with fewer than 3 segments is returned unchanged.
/// Ported from FirmwareUpdateService.normalizeSemver.
pub fn normalize_semver(raw: &str) -> String {
    let parts: Vec<&str> = raw.split('.').collect();
    if parts.len() < 3 {
        return raw.to_string();
    }
    parts[..3].join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid() {
        let v = FirmwareVersion::parse("1.3.11").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (1, 3, 11));
    }

    #[test]
    fn parse_invalid() {
        assert!(FirmwareVersion::parse("").is_none());
        assert!(FirmwareVersion::parse("1.2").is_none());
        assert!(FirmwareVersion::parse("a.b.c").is_none());
        assert!(FirmwareVersion::parse("1.2.3.4").is_none());
        assert!(FirmwareVersion::parse("1..3").is_none());
    }

    #[test]
    fn compare_numeric_not_lexicographic() {
        let v = |s| FirmwareVersion::parse(s).unwrap();
        assert!(v("1.3.11") < v("1.6.0"));
        assert!(v("1.6.0") < v("1.6.1"));
        assert!(v("1.9.9") < v("1.10.0")); // numeric, not lexicographic
        assert_eq!(v("1.6.0"), v("1.6.0"));
        assert!(v("2.0.0") > v("1.99.99"));
    }

    #[test]
    fn display_roundtrip() {
        assert_eq!(FirmwareVersion::parse("1.6.0").unwrap().to_string(), "1.6.0");
    }

    #[test]
    fn normalize() {
        assert_eq!(normalize_semver("1.6.2.42"), "1.6.2"); // 4-seg GET_STATUS
        assert_eq!(normalize_semver("1.6.2"), "1.6.2");
        assert_eq!(normalize_semver("1.6"), "1.6"); // <3 segs unchanged
        assert_eq!(normalize_semver(""), "");
    }
}
