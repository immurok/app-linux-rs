//! Firmware update orchestration errors (design doc §4).

#[derive(Debug, thiserror::Error)]
pub enum FwUpdateError {
    #[error("failed to fetch update manifest: {0}")]
    ManifestFetch(String),
    #[error("unsupported manifest: {0}")]
    ManifestSchema(String),
    #[error("download failed: {0}")]
    Download(String),
    #[error("downloaded file is corrupt (sha256 mismatch)")]
    Sha256Mismatch,
    #[error("invalid firmware package: {0}")]
    PackageInvalid(String),
    #[error("preflight failed: {0}")]
    Preflight(String),
    #[error("transfer failed at {stage}: {detail}")]
    Transfer { stage: &'static str, detail: String },
    #[error("device rejected firmware header: {0}")]
    HeaderRejected(String),
    #[error("device rejected firmware signature (unofficial build?)")]
    SignatureRejected,
    #[error("device did not report version {0} within 60s — it may still be verifying; run `immurok-cli fw check` in a minute")]
    ReconnectTimeout(String),
    #[error("device battery too low — charge to at least 30% and retry")]
    LowBattery,
    #[error("storage error: {0}")]
    Store(String),
}
