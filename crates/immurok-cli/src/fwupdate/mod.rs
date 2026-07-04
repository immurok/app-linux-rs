//! Firmware update orchestration — the Linux counterpart of macOS
//! FirmwareUpdateService. Pure logic lives in immurok_common::fwupdate;
//! this module owns IO: HTTP fetch/download, on-disk state, socket push.

pub mod error;
pub mod http;
pub mod push;
pub mod store;

use std::fs;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use immurok_common::fwupdate::imfw;
use immurok_common::fwupdate::manifest::{self, UpdateManifest};
use immurok_common::fwupdate::planner::{self, UpdatePlan};
use immurok_common::fwupdate::version::{normalize_semver, FirmwareVersion};

use crate::socket_client::DaemonClient;
use error::FwUpdateError;
use store::{FwStore, LastCheck, PendingHop};

/// Below this version the device is on the old signing era (soft warning
/// surfaces in `status` and the TUI dashboard).
pub const MANDATORY_MIN_VERSION: &str = "1.6.0";
/// App-side battery gate (firmware separately hard-refuses below 5%).
pub const BATTERY_MIN_PERCENT: u8 = 30;
pub const RECONNECT_TIMEOUT_SECS: u64 = 60;

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── device status ───────────────────────────────────────────────

pub struct DeviceStatus {
    pub connected: bool,
    pub battery: u8,
    pub version: String,
}

/// STATUS → `STATUS:connected(0/1):name:battery:version`
pub fn query_device_status() -> Result<DeviceStatus, FwUpdateError> {
    let mut c = DaemonClient::connect().map_err(FwUpdateError::Preflight)?;
    let resp = c.send("STATUS").map_err(FwUpdateError::Preflight)?;
    let parts: Vec<&str> = resp.split(':').collect();
    if parts.first() != Some(&"STATUS") || parts.len() < 5 {
        return Err(FwUpdateError::Preflight(format!(
            "unexpected STATUS response: {resp}"
        )));
    }
    Ok(DeviceStatus {
        connected: parts[1] == "1",
        battery: parts[3].parse().unwrap_or(0),
        version: parts[4].to_string(),
    })
}

// ── planning ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum HopSource {
    Remote { url: String, sha256: String },
    /// Resume path: package already persisted at store.pending_package_path().
    PendingFile,
}

#[derive(Debug, Clone)]
pub struct Hop {
    // hop kind ("bridge"/"final"); read by tests + kept for diagnostics
    #[allow(dead_code)]
    pub label: &'static str,
    pub version: String,
    pub source: HopSource,
}

#[derive(Debug, Clone)]
pub struct PreparedUpdate {
    pub device_version: String,
    pub target_version: String,
    pub notes: Option<String>,
    pub hops: Vec<Hop>,
    pub resumed: bool,
}

/// Manifest with 24h throttle: within the window (and not forced) the cached
/// copy is reused, so offline TUI startups stay silent and fast.
pub fn fetch_manifest_cached(
    store: &FwStore,
    force: bool,
    now: u64,
) -> Result<UpdateManifest, FwUpdateError> {
    if !force && !store.is_check_due(now) {
        if let Some(lc) = store.load_last_check() {
            if let Ok(m) = manifest::decode(&lc.manifest_json) {
                return Ok(m);
            }
        }
    }
    let body = http::fetch_manifest_body()?;
    let m = manifest::decode(&body).map_err(|e| FwUpdateError::ManifestSchema(e.to_string()))?;
    store.save_last_check(&LastCheck { checked_at: now, manifest_json: body });
    Ok(m)
}

pub fn hops_for(plan: &UpdatePlan, m: &UpdateManifest) -> Result<Vec<Hop>, FwUpdateError> {
    let latest_hop = || Hop {
        label: "final",
        version: m.latest.version.clone(),
        source: HopSource::Remote { url: m.latest.url.clone(), sha256: m.latest.sha256.clone() },
    };
    let bridge_hop = |label: &'static str| -> Result<Hop, FwUpdateError> {
        let b = m.bridge.as_ref().ok_or_else(|| {
            FwUpdateError::ManifestSchema("manifest has no bridge entry".into())
        })?;
        Ok(Hop {
            label,
            version: b.version.clone(),
            source: HopSource::Remote { url: b.url.clone(), sha256: b.sha256.clone() },
        })
    };
    match plan {
        UpdatePlan::Direct => Ok(vec![latest_hop()]),
        UpdatePlan::BridgeOnly => Ok(vec![bridge_hop("final")?]),
        UpdatePlan::TwoHops => Ok(vec![bridge_hop("bridge")?, latest_hop()]),
        UpdatePlan::UpToDate | UpdatePlan::Unknown => Ok(vec![]),
    }
}

/// Check + plan. Ok(None) = device already up to date.
/// Resume detection (design doc §2 step 1) runs first:
///   device ≥ pending target → clear, done
///   device < pending bridge → bridge hop never landed, clear, full flow
///   otherwise               → resume the final hop from the persisted package
pub fn prepare(store: &FwStore, force_check: bool) -> Result<Option<PreparedUpdate>, FwUpdateError> {
    let st = query_device_status()?;
    if !st.connected {
        return Err(FwUpdateError::Preflight("device not connected".into()));
    }
    let device = normalize_semver(&st.version);

    if let Some(p) = store.load_pending() {
        let dev = FirmwareVersion::parse(&device);
        let tgt = FirmwareVersion::parse(&p.target_version);
        let bri = FirmwareVersion::parse(&p.bridge_version);
        match (dev, tgt, bri) {
            (Some(d), Some(t), _) if d >= t => {
                store.clear_pending();
                return Ok(None);
            }
            (Some(d), Some(_), Some(b)) if d < b => {
                store.clear_pending(); // bridge hop failed — restart from scratch
            }
            (Some(_), Some(t), Some(_)) if store.pending_package_path().exists() => {
                return Ok(Some(PreparedUpdate {
                    device_version: device,
                    target_version: p.target_version.clone(),
                    notes: None,
                    hops: vec![Hop {
                        label: "final",
                        version: t.to_string(),
                        source: HopSource::PendingFile,
                    }],
                    resumed: true,
                }));
            }
            _ => store.clear_pending(), // unparsable/incomplete state — drop it
        }
    }

    let m = fetch_manifest_cached(store, force_check, unix_now())?;
    let plan = planner::plan(&device, &m.latest.version, m.latest.min_direct.as_deref());
    match plan {
        UpdatePlan::UpToDate => Ok(None),
        UpdatePlan::Unknown => Err(FwUpdateError::Preflight(format!(
            "cannot parse device firmware version '{}'",
            st.version
        ))),
        _ => {
            let hops = hops_for(&plan, &m)?;
            let target_version = hops.last().map(|h| h.version.clone()).unwrap_or_default();
            Ok(Some(PreparedUpdate {
                device_version: device,
                target_version,
                notes: m.latest.notes.clone(),
                hops,
                resumed: false,
            }))
        }
    }
}

// ── execution ───────────────────────────────────────────────────

pub enum ProgressEvent {
    Stage { hop: usize, hops: usize, name: &'static str },
    Transfer { hop: usize, hops: usize, fraction: f64 },
    Reconnect { hop: usize, hops: usize },
}

pub fn execute(
    store: &FwStore,
    prep: &PreparedUpdate,
    progress: &mut dyn FnMut(ProgressEvent),
) -> Result<(), FwUpdateError> {
    // Preflight (design doc §2 step 6)
    let st = query_device_status()?;
    if !st.connected {
        return Err(FwUpdateError::Preflight("device not connected".into()));
    }
    if st.battery < BATTERY_MIN_PERCENT {
        return Err(FwUpdateError::LowBattery);
    }

    // Download/validate every hop package up front (step 5) — a two-hop
    // update must not strand the device on the bridge because the final
    // download failed mid-flight.
    let mut packages: Vec<Vec<u8>> = Vec::with_capacity(prep.hops.len());
    for hop in &prep.hops {
        let data = match &hop.source {
            HopSource::PendingFile => fs::read(store.pending_package_path())
                .map_err(|e| FwUpdateError::Store(e.to_string()))?,
            HopSource::Remote { url, sha256 } => {
                // Cache hit: re-verify the digest before trusting the bytes —
                // a bit-rotted or partially-written cache file must not wedge
                // the update loop forever with a permanent Sha256Mismatch.
                let cached = match store.cached_package(sha256) {
                    Some(p) => match fs::read(&p) {
                        Ok(d) if http::sha256_hex(&d).eq_ignore_ascii_case(sha256) => Some(d),
                        Ok(_) => {
                            let _ = std::fs::remove_file(&p);
                            None
                        }
                        Err(_) => None,
                    },
                    None => None,
                };
                match cached {
                    Some(d) => d,
                    None => {
                        let d = http::download(url, sha256)?;
                        store.store_package(sha256, &d)?;
                        d
                    }
                }
            }
        };
        imfw::parse(&data).map_err(|e| FwUpdateError::PackageInvalid(e.to_string()))?;
        packages.push(data);
    }

    let hops = prep.hops.len();
    for (i, hop) in prep.hops.iter().enumerate() {
        let pkg = imfw::parse(&packages[i]).expect("validated above");
        let mut cb = |ev: push::PushEvent| match ev {
            push::PushEvent::Stage(name) => progress(ProgressEvent::Stage { hop: i, hops, name }),
            push::PushEvent::Chunk { done, total } => progress(ProgressEvent::Transfer {
                hop: i,
                hops,
                fraction: done as f64 / total as f64,
            }),
            push::PushEvent::DeviceInfo(_) => {}
        };
        push::push_with_retry(
            || {
                DaemonClient::connect()
                    .map_err(|e| FwUpdateError::Transfer { stage: "connect", detail: e })
            },
            &pkg,
            &mut cb,
        )?;

        // Device is now verifying + rebooting; confirm it comes back on the
        // hop's version before declaring this hop done (step 8).
        progress(ProgressEvent::Reconnect { hop: i, hops });
        wait_for_version(&hop.version, RECONNECT_TIMEOUT_SECS)?;

        // Bridge hop confirmed → persist the final hop so a crash/power-loss
        // between hops can resume (step 9).
        if i + 1 < hops {
            let fin = &prep.hops[i + 1];
            store.save_pending(
                &PendingHop {
                    target_version: fin.version.clone(),
                    bridge_version: hop.version.clone(),
                },
                &packages[i + 1],
            )?;
        }
    }
    store.clear_pending();
    Ok(())
}

/// Human label for push stages — shared by the fw command's progress bar
/// and the TUI Firmware page gauge.
pub fn stage_label(name: &str) -> &'static str {
    match name {
        "info" => "querying device",
        "erase" => "erasing Image B",
        "header" => "sending header",
        "write" => "writing firmware",
        "end" => "verifying + rebooting",
        "retry" => "retrying after transfer error",
        _ => "working",
    }
}

/// Poll OTA:VERSION once per second until the device reports ≥ target.
/// Each poll uses a fresh connection (the daemon's OTA session is
/// per-connection; dropping the client closes it cleanly).
pub fn wait_for_version(target: &str, timeout_secs: u64) -> Result<(), FwUpdateError> {
    let tgt = FirmwareVersion::parse(&normalize_semver(target)).ok_or_else(|| {
        FwUpdateError::Preflight(format!("bad target version '{target}'"))
    })?;
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if let Ok(mut c) = DaemonClient::connect() {
            if let Ok(resp) = c.send("OTA:VERSION") {
                if let Some(raw) = resp.trim().strip_prefix("OK:") {
                    if let Some(v) = FirmwareVersion::parse(&normalize_semver(raw.trim())) {
                        if v >= tgt {
                            return Ok(());
                        }
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(FwUpdateError::ReconnectTimeout(target.to_string()));
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use immurok_common::fwupdate::{manifest, planner::UpdatePlan};

    fn m() -> manifest::UpdateManifest {
        manifest::decode(
            r#"{
              "schema": 1,
              "latest": {"version":"1.6.2","format":"v2",
                "url":"https://x/l.imfw","sha256":"aa","min_direct":"1.6.0"},
              "bridge": {"version":"1.6.0","format":"v1",
                "url":"https://x/b.imfw","sha256":"bb"}
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn direct_one_hop() {
        let hops = hops_for(&UpdatePlan::Direct, &m()).unwrap();
        assert_eq!(hops.len(), 1);
        assert_eq!(hops[0].label, "final");
        assert_eq!(hops[0].version, "1.6.2");
        assert!(matches!(&hops[0].source, HopSource::Remote { sha256, .. } if sha256 == "aa"));
    }

    #[test]
    fn bridge_only_pushes_bridge_asset() {
        let hops = hops_for(&UpdatePlan::BridgeOnly, &m()).unwrap();
        assert_eq!(hops.len(), 1);
        assert_eq!(hops[0].version, "1.6.0");
        assert!(matches!(&hops[0].source, HopSource::Remote { sha256, .. } if sha256 == "bb"));
    }

    #[test]
    fn two_hops_bridge_then_final() {
        let hops = hops_for(&UpdatePlan::TwoHops, &m()).unwrap();
        assert_eq!(hops.len(), 2);
        assert_eq!((hops[0].label, hops[0].version.as_str()), ("bridge", "1.6.0"));
        assert_eq!((hops[1].label, hops[1].version.as_str()), ("final", "1.6.2"));
    }

    #[test]
    fn two_hops_without_bridge_entry_fails() {
        let mut man = m();
        man.bridge = None;
        assert!(matches!(
            hops_for(&UpdatePlan::TwoHops, &man).unwrap_err(),
            FwUpdateError::ManifestSchema(_)
        ));
    }

    #[test]
    fn up_to_date_empty() {
        assert!(hops_for(&UpdatePlan::UpToDate, &m()).unwrap().is_empty());
    }
}
