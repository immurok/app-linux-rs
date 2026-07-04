//! `immurok-cli fw` — orchestrated firmware update (design doc
//! docs/plans/2026-07-04-linux-fw-update-design.md).

use std::io::Write;

use indicatif::{ProgressBar, ProgressStyle};

use immurok_common::fwupdate::planner::{self, UpdatePlan};
use immurok_common::fwupdate::version::normalize_semver;

use crate::fwupdate::{self, store::FwStore, ProgressEvent};

fn open_store() -> FwStore {
    match FwStore::open_default() {
        Ok(s) => s,
        Err(e) => super::error_exit(&e.to_string()),
    }
}

pub fn run_check(force: bool) {
    let store = open_store();
    let st = match fwupdate::query_device_status() {
        Ok(s) => s,
        Err(e) => super::error_exit(&e.to_string()),
    };
    if !st.connected {
        super::error_exit("Device not connected.");
    }
    let device = normalize_semver(&st.version);
    let m = match fwupdate::fetch_manifest_cached(&store, force, fwupdate::unix_now()) {
        Ok(m) => m,
        Err(e) => super::error_exit(&e.to_string()),
    };
    println!("Device firmware: {}", device);
    println!("Latest version:  {}", m.latest.version);
    match planner::plan(&device, &m.latest.version, m.latest.min_direct.as_deref()) {
        UpdatePlan::UpToDate => println!("\x1b[32mFirmware is up to date.\x1b[0m"),
        UpdatePlan::Unknown => super::error_exit(&format!(
            "Cannot parse device version '{}'. Reconnect and retry.",
            st.version
        )),
        plan => {
            let hops = match plan {
                UpdatePlan::TwoHops => " (2 hops via bridge)",
                _ => "",
            };
            println!("\x1b[33mUpdate available{}\x1b[0m", hops);
            if let Some(notes) = &m.latest.notes {
                println!("Notes: {}", notes);
            }
            println!("Run `immurok-cli fw update` to install.");
        }
    }
}

pub fn run_update(yes: bool) {
    let store = open_store();
    let prep = match fwupdate::prepare(&store, true) {
        Ok(Some(p)) => p,
        Ok(None) => {
            println!("\x1b[32mFirmware is up to date.\x1b[0m");
            return;
        }
        Err(e) => super::error_exit(&e.to_string()),
    };

    if prep.resumed {
        println!(
            "Resuming interrupted update: {} → {}",
            prep.device_version, prep.target_version
        );
    } else {
        println!("Update: {} → {}", prep.device_version, prep.target_version);
        if prep.hops.len() == 2 {
            println!("Plan: 2 hops (bridge {} → {})", prep.hops[0].version, prep.hops[1].version);
        }
        if let Some(notes) = &prep.notes {
            println!("Notes: {}", notes);
        }
    }

    if !yes && !confirm("Proceed with the update? [y/N] ") {
        println!("Aborted.");
        return;
    }

    println!();
    let pb = ProgressBar::new(1000);
    pb.set_style(
        ProgressStyle::with_template(
            "  {msg:<28} [{bar:40.cyan/blue}] {percent:>3}%",
        )
        .unwrap()
        .progress_chars("█░ "),
    );

    let hops_total = prep.hops.len();
    // Within-hop fraction reached so far. `Stage` events must never regress
    // it (they'd otherwise snap the bar back to the hop baseline after
    // Transfer already reached ~1.0) — the "retry" stage is the one genuine
    // exception, since the push really does restart from ERASE. Reset when
    // the hop index advances, or hop N+1's early Stage events would inherit
    // hop N's 1.0 and briefly show the next hop as already complete.
    let mut last_frac: f64 = 0.0;
    let mut last_hop = usize::MAX;
    let mut progress = |ev: ProgressEvent| {
        let ev_hop = match ev {
            ProgressEvent::Stage { hop, .. }
            | ProgressEvent::Transfer { hop, .. }
            | ProgressEvent::Reconnect { hop, .. } => hop,
        };
        if ev_hop != last_hop {
            last_hop = ev_hop;
            last_frac = 0.0;
        }
        // Merged progress across hops: base + p * hop_weight (design doc §3).
        let (hop, frac, label) = match ev {
            ProgressEvent::Stage { hop, name, .. } => {
                if name == "retry" {
                    last_frac = 0.0;
                }
                (hop, last_frac, fwupdate::stage_label(name))
            }
            ProgressEvent::Transfer { hop, fraction, .. } => {
                last_frac = fraction;
                (hop, fraction, "writing firmware")
            }
            ProgressEvent::Reconnect { hop, .. } => {
                last_frac = 1.0;
                (hop, 1.0, "waiting for device reboot")
            }
        };
        let overall = (hop as f64 + frac) / hops_total as f64;
        pb.set_position((overall * 1000.0) as u64);
        if hops_total > 1 {
            pb.set_message(format!("hop {}/{}: {}", hop + 1, hops_total, label));
        } else {
            pb.set_message(label.to_string());
        }
    };

    match fwupdate::execute(&store, &prep, &mut progress) {
        Ok(()) => {
            pb.finish_and_clear();
            println!(
                "\x1b[32mUpdate complete — device is now on {}.\x1b[0m",
                prep.target_version
            );
        }
        Err(e) => {
            pb.finish_and_clear();
            super::error_exit(&e.to_string());
        }
    }
}

pub fn run_status() {
    let store = open_store();
    match store.load_last_check() {
        Some(lc) => {
            let age_h = fwupdate::unix_now().saturating_sub(lc.checked_at) / 3600;
            println!("Last check: {}h ago", age_h);
            match immurok_common::fwupdate::manifest::decode(&lc.manifest_json) {
                Ok(m) => println!("Cached latest: {}", m.latest.version),
                Err(_) => println!("Cached manifest: (unreadable)"),
            }
        }
        None => println!("Last check: never"),
    }
    match store.load_pending() {
        Some(p) => println!(
            "\x1b[33mPending resume: → {} (bridge {} done). Run `immurok-cli fw update`.\x1b[0m",
            p.target_version, p.bridge_version
        ),
        None => println!("Pending resume: none"),
    }
}

fn confirm(prompt: &str) -> bool {
    print!("{}", prompt);
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}
