//! Update manifest (https://immurok.com/fw/manifest.json) — schema must be 1.
//! Field additions/changes require a schema bump.
//! Ported from FirmwareUpdateKit/UpdateManifest.swift.

use serde::Deserialize;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ManifestError {
    #[error("unsupported manifest schema {0}")]
    UnsupportedSchema(u32),
    #[error("manifest parse error: {0}")]
    Parse(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    pub version: String,
    pub sec_version: Option<u32>,
    pub format: String, // "v1" | "v2"
    pub url: String,
    pub sha256: String,
    pub size: Option<u64>,
    pub min_direct: Option<String>, // latest only
    pub notes: Option<String>,      // latest only, English release notes
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateManifest {
    pub schema: u32,
    pub latest: Asset,
    pub bridge: Option<Asset>,
}

pub fn decode(json: &str) -> Result<UpdateManifest, ManifestError> {
    let m: UpdateManifest =
        serde_json::from_str(json).map_err(|e| ManifestError::Parse(e.to_string()))?;
    if m.schema != 1 {
        return Err(ManifestError::UnsupportedSchema(m.schema));
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 与线上 manifest 同构的完整样例
    const FULL_JSON: &str = r#"{
      "schema": 1,
      "latest": {
        "version": "1.6.2",
        "sec_version": 1,
        "format": "v2",
        "url": "https://immurok.com/fw/immurok-ik1-v1.6.2.imfw",
        "sha256": "fd19c2de00",
        "size": 190532,
        "min_direct": "1.6.0",
        "notes": "Hotfix to GUI auth functionality"
      },
      "bridge": {
        "version": "1.6.0",
        "format": "v1",
        "url": "https://immurok.com/fw/immurok-ik1-v1.6.0-bridge.imfw",
        "sha256": "08a22d3900"
      }
    }"#;

    #[test]
    fn decode_full() {
        let m = decode(FULL_JSON).unwrap();
        assert_eq!(m.schema, 1);
        assert_eq!(m.latest.version, "1.6.2");
        assert_eq!(m.latest.min_direct.as_deref(), Some("1.6.0"));
        assert_eq!(m.latest.notes.as_deref(), Some("Hotfix to GUI auth functionality"));
        assert_eq!(m.bridge.as_ref().unwrap().sha256, "08a22d3900");
        assert_eq!(m.bridge.as_ref().unwrap().sec_version, None);
    }

    #[test]
    fn unknown_schema_rejected() {
        let json = FULL_JSON.replace("\"schema\": 1", "\"schema\": 99");
        match decode(&json) {
            Err(e) => assert_eq!(e, ManifestError::UnsupportedSchema(99)),
            Ok(_) => panic!("schema 99 must be rejected"),
        }
    }

    #[test]
    fn missing_latest_rejected() {
        assert!(matches!(decode(r#"{"schema": 1}"#), Err(ManifestError::Parse(_))));
    }

    #[test]
    fn bridge_optional() {
        let json = r#"{"schema":1,"latest":{"version":"1.6.0","format":"v1",
          "url":"https://immurok.com/fw/x.imfw","sha256":"cc","size":10,"min_direct":"1.6.0"}}"#;
        let m = decode(json).unwrap();
        assert!(m.bridge.is_none());
        assert!(m.latest.notes.is_none());
    }
}
