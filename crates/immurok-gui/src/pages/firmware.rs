//! Firmware group on the Device page (spec §6): check the update server, show
//! the plan, run the OTA update with merged progress. Same state machine as
//! the TUI's Firmware tab. `execute` cannot be cancelled, so while it runs
//! the main window refuses to close (`FW_UPDATING`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use immurok_client::fwupdate::error::FwUpdateError;
use immurok_client::fwupdate::store::FwStore;
use immurok_client::fwupdate::{
    execute, prepare, query_device_status, stage_label, PreparedUpdate, ProgressEvent, MANDATORY_MIN_VERSION,
};
use immurok_common::fwupdate::version::{normalize_semver, FirmwareVersion};

use crate::errors::fw_friendly;

use super::run_blocking;

/// True while `execute` is running on the worker thread. The main window and
/// the `quit` action consult it to refuse closing mid-update.
pub static FW_UPDATING: AtomicBool = AtomicBool::new(false);

pub fn updating() -> bool {
    FW_UPDATING.load(Ordering::Relaxed)
}

// ── progress merging (same rules as the TUI) ──

#[derive(Debug, Clone, PartialEq)]
pub struct Merged {
    pub stage: String,
    /// Overall 0.0..=1.0 across all hops.
    pub fraction: f64,
    /// 1-based hop for display.
    pub hop: usize,
    pub hops: usize,
}

/// Within-hop fraction must never regress on `Stage` events (they would
/// snap the bar back to the hop baseline after `Transfer` reached ~1.0);
/// "retry" is the one exception since the push restarts from ERASE. Reset
/// when the hop index advances.
pub struct ProgressMerger {
    last_frac: f64,
    last_hop: usize,
}

impl Default for ProgressMerger {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressMerger {
    pub fn new() -> Self {
        Self { last_frac: 0.0, last_hop: usize::MAX }
    }

    pub fn merge(&mut self, ev: &ProgressEvent) -> Merged {
        let ev_hop = match ev {
            ProgressEvent::Stage { hop, .. } | ProgressEvent::Transfer { hop, .. } | ProgressEvent::Reconnect { hop, .. } => *hop,
        };
        if ev_hop != self.last_hop {
            self.last_hop = ev_hop;
            self.last_frac = 0.0;
        }
        let (stage, frac, hop, hops) = match ev {
            ProgressEvent::Stage { hop, hops, name } => {
                if *name == "retry" {
                    self.last_frac = 0.0;
                }
                (stage_label(name).to_string(), self.last_frac, *hop, *hops)
            }
            ProgressEvent::Transfer { hop, hops, fraction } => {
                self.last_frac = *fraction;
                ("writing firmware".to_string(), *fraction, *hop, *hops)
            }
            ProgressEvent::Reconnect { hop, hops } => {
                self.last_frac = 1.0;
                ("waiting for device reboot".to_string(), 1.0, *hop, *hops)
            }
        };
        let hops = hops.max(1);
        Merged { stage, fraction: (hop as f64 + frac) / hops as f64, hop: hop + 1, hops }
    }
}

/// Old-signing-era check (same comparison as `immurok-cli status`): the
/// device's firmware predates `MANDATORY_MIN_VERSION`.
fn is_outdated(device_version: &str) -> bool {
    let norm = normalize_semver(device_version);
    match (FirmwareVersion::parse(&norm), FirmwareVersion::parse(MANDATORY_MIN_VERSION)) {
        (Some(v), Some(min)) => v < min,
        _ => false,
    }
}

fn plan_label(hops: usize, resumed: bool, bridge: Option<(&str, &str)>) -> String {
    if resumed {
        "Plan: resume interrupted update".into()
    } else if let Some((from, to)) = bridge.filter(|_| hops == 2) {
        format!("Plan: 2 hops (bridge {from} → {to})")
    } else {
        "Plan: direct (1 hop)".into()
    }
}

// ── page ──

enum FwState {
    Idle,
    Checking,
    UpToDate,
    Ready(PreparedUpdate),
    Updating,
    Success(String),
    Failed(String),
}

enum Msg {
    Progress(Merged),
    Done(Result<String, String>),
}

pub struct FirmwarePage {
    root: adw::PreferencesGroup,
    device_row: adw::ActionRow,
    status_row: adw::ActionRow,
    progress: gtk::ProgressBar,
    warning: gtk::Label,
    update_btn: gtk::Button,
    check_btn: gtk::Button,
    toasts: adw::ToastOverlay,
    state: RefCell<FwState>,
    /// Latest known device version predates `MANDATORY_MIN_VERSION` (old
    /// signing era) — drives the `error` css class + title prefix in `render`.
    outdated: Cell<bool>,
}

impl FirmwarePage {
    pub fn new(toasts: &adw::ToastOverlay) -> Rc<Self> {
        let root = adw::PreferencesGroup::builder().title("Firmware").build();
        let check_btn = gtk::Button::builder().icon_name("view-refresh-symbolic").valign(gtk::Align::Center).tooltip_text("Check again").build();
        check_btn.add_css_class("flat");
        root.set_header_suffix(Some(&check_btn));

        let device_row = adw::ActionRow::builder().title("Version").subtitle("-").build();
        let update_btn = gtk::Button::builder().label("Update").valign(gtk::Align::Center).visible(false).build();
        update_btn.add_css_class("suggested-action");
        device_row.add_suffix(&update_btn);
        root.add(&device_row);

        let status_row = adw::ActionRow::builder().title("Checking for updates…").build();
        root.add(&status_row);

        let progress = gtk::ProgressBar::builder().visible(false).show_text(true).margin_top(6).build();
        let warning = gtk::Label::builder().label("Do not power off the device.").xalign(0.0).visible(false).build();
        warning.add_css_class("error");
        root.add(&progress);
        root.add(&warning);

        let this = Rc::new(Self {
            root,
            device_row,
            status_row,
            progress,
            warning,
            update_btn,
            check_btn,
            toasts: toasts.clone(),
            state: RefCell::new(FwState::Idle),
            outdated: Cell::new(false),
        });
        this.render();

        let weak = Rc::downgrade(&this);
        this.check_btn.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.check(true);
            }
        });
        let weak = Rc::downgrade(&this);
        this.update_btn.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.update();
            }
        });
        let weak = Rc::downgrade(&this);
        // Spec §6.3: every time the page is entered with a settled state a
        // fresh check runs, so a stale "could not reach the update server"
        // never survives coming back to the page. `check()` itself refuses
        // while Checking / Updating, so no in-flight work is disturbed; this
        // one honours the 24h throttle (unlike the header refresh button).
        this.root.connect_map(move |_| {
            if let Some(p) = weak.upgrade() {
                if matches!(*p.state.borrow(), FwState::Idle | FwState::Success(_) | FwState::Failed(_)) {
                    p.check(false);
                }
            }
        });
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    fn set_state(&self, s: FwState) {
        *self.state.borrow_mut() = s;
        self.render();
    }

    fn render(&self) {
        let st = self.state.borrow();
        let (title, notes, can_update, can_check, updating) = match &*st {
            FwState::Idle | FwState::Checking => ("Checking for updates…".to_string(), None, false, false, false),
            FwState::UpToDate => ("Up to date".to_string(), None, false, true, false),
            FwState::Ready(p) => {
                let bridge = p.hops.first().zip(p.hops.last()).map(|(a, b)| (a.version.as_str(), b.version.as_str()));
                (plan_label(p.hops.len(), p.resumed, bridge), p.notes.clone(), true, true, false)
            }
            FwState::Updating => ("Updating…".to_string(), None, false, false, true),
            FwState::Success(v) => (format!("Update complete — device is now on {v}."), None, false, true, false),
            FwState::Failed(e) => (format!("Could not check: {e}"), None, false, true, false),
        };
        if self.outdated.get() {
            self.status_row.add_css_class("error");
        } else {
            self.status_row.remove_css_class("error");
        }
        if !updating {
            let title = if self.outdated.get() {
                format!(
                    "Firmware outdated (old signing era) — update to keep unlocking and key operations working. {title}"
                )
            } else {
                title
            };
            self.status_row.set_title(&glib::markup_escape_text(&title));
        }
        self.status_row.set_subtitle(&notes.map(|n| glib::markup_escape_text(&n).to_string()).unwrap_or_default());
        self.progress.set_visible(updating);
        self.warning.set_visible(updating);
        self.update_btn.set_visible(can_update);
        self.check_btn.set_sensitive(can_check);
        self.check_btn.set_tooltip_text(Some(if matches!(&*st, FwState::Failed(_)) { "Retry check" } else { "Check again" }));
    }

    /// `force` bypasses the store's 24h manifest-check throttle (the header
    /// refresh button); the page-entry auto-check passes `false` so opening
    /// the window repeatedly does not hit the network every time.
    pub fn check(self: &Rc<Self>, force: bool) {
        if matches!(*self.state.borrow(), FwState::Checking | FwState::Updating) {
            return;
        }
        self.set_state(FwState::Checking);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let r = run_blocking(move || {
                let st = query_device_status().ok();
                let store = FwStore::open_default()?;
                let prep = prepare(&store, force)?;
                Ok::<_, FwUpdateError>((st, prep))
            })
            .await;
            let Some(this) = weak.upgrade() else { return };
            match r {
                Some(Ok((st, prep))) => {
                    let dev = st.as_ref().filter(|s| s.connected).map(|s| s.version.clone()).unwrap_or_else(|| "-".into());
                    this.device_row.set_subtitle(&glib::markup_escape_text(&dev));
                    let outdated = st.as_ref().filter(|s| s.connected).is_some_and(|s| is_outdated(&s.version));
                    this.outdated.set(outdated);
                    match prep {
                        Some(p) => this.set_state(FwState::Ready(p)),
                        None => this.set_state(FwState::UpToDate),
                    }
                }
                // Spec §6.2: both versions are unknown when the check failed.
                Some(Err(e)) => {
                    this.device_row.set_subtitle("-");
                    this.outdated.set(false);
                    this.set_state(FwState::Failed(fw_friendly(&e)));
                }
                None => {
                    this.device_row.set_subtitle("-");
                    this.outdated.set(false);
                    this.set_state(FwState::Failed("Check failed".into()));
                }
            }
        });
    }

    fn update(self: &Rc<Self>) {
        let prep = match &*self.state.borrow() {
            FwState::Ready(p) => p.clone(),
            _ => return,
        };
        FW_UPDATING.store(true, Ordering::Relaxed);
        self.set_state(FwState::Updating);
        self.status_row.set_title(&glib::markup_escape_text("Starting…"));
        self.progress.set_fraction(0.0);
        self.progress.set_text(Some("0 %"));

        let (tx, rx) = async_channel::unbounded::<Msg>();
        std::thread::spawn(move || {
            let target = prep.target_version.clone();
            let result = FwStore::open_default()
                .and_then(|store| {
                    let mut merger = ProgressMerger::new();
                    let tx_p = tx.clone();
                    let mut progress = |ev: ProgressEvent| {
                        let _ = tx_p.send_blocking(Msg::Progress(merger.merge(&ev)));
                    };
                    execute(&store, &prep, &mut progress)
                })
                .map(|_| target)
                .map_err(|e| fw_friendly(&e));
            let _ = tx.send_blocking(Msg::Done(result));
        });

        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(msg) = rx.recv().await {
                let Some(this) = weak.upgrade() else { break };
                match msg {
                    Msg::Progress(m) => {
                        let stage = if m.hops > 1 { format!("hop {}/{}: {}", m.hop, m.hops, m.stage) } else { m.stage };
                        this.status_row.set_title(&glib::markup_escape_text(&stage));
                        this.progress.set_fraction(m.fraction.clamp(0.0, 1.0));
                        this.progress.set_text(Some(&format!("{:.0} %", m.fraction * 100.0)));
                    }
                    Msg::Done(result) => {
                        FW_UPDATING.store(false, Ordering::Relaxed);
                        match result {
                            Ok(v) => {
                                this.device_row.set_subtitle(&glib::markup_escape_text(&v));
                                this.outdated.set(is_outdated(&v));
                                this.toasts.add_toast(adw::Toast::new("Firmware updated"));
                                this.set_state(FwState::Success(v));
                            }
                            Err(e) => this.set_state(FwState::Failed(e)),
                        }
                        break;
                    }
                }
            }
            FW_UPDATING.store(false, Ordering::Relaxed);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use immurok_client::fwupdate::ProgressEvent as PE;

    #[test]
    fn stage_events_never_regress_within_a_hop() {
        let mut m = ProgressMerger::new();
        assert_eq!(m.merge(&PE::Stage { hop: 0, hops: 1, name: "erase" }).fraction, 0.0);
        let w = m.merge(&PE::Transfer { hop: 0, hops: 1, fraction: 0.6 });
        assert!((w.fraction - 0.6).abs() < 1e-9);
        assert_eq!(w.stage, "writing firmware");
        let s = m.merge(&PE::Stage { hop: 0, hops: 1, name: "end" });
        assert!((s.fraction - 0.6).abs() < 1e-9);
        assert_eq!(s.stage, "verifying + rebooting");
    }

    #[test]
    fn retry_resets_and_reconnect_fills() {
        let mut m = ProgressMerger::new();
        m.merge(&PE::Transfer { hop: 0, hops: 1, fraction: 0.9 });
        assert_eq!(m.merge(&PE::Stage { hop: 0, hops: 1, name: "retry" }).fraction, 0.0);
        let r = m.merge(&PE::Reconnect { hop: 0, hops: 1 });
        assert_eq!(r.fraction, 1.0);
        assert_eq!(r.stage, "waiting for device reboot");
    }

    #[test]
    fn two_hops_merge_and_display_one_based() {
        let mut m = ProgressMerger::new();
        let a = m.merge(&PE::Transfer { hop: 0, hops: 2, fraction: 1.0 });
        assert!((a.fraction - 0.5).abs() < 1e-9);
        assert_eq!((a.hop, a.hops), (1, 2));
        // Next hop starts from its own baseline, not the previous 1.0.
        let b = m.merge(&PE::Stage { hop: 1, hops: 2, name: "info" });
        assert!((b.fraction - 0.5).abs() < 1e-9);
        assert_eq!(b.hop, 2);
        let c = m.merge(&PE::Transfer { hop: 1, hops: 2, fraction: 0.5 });
        assert!((c.fraction - 0.75).abs() < 1e-9);
    }

    #[test]
    fn plan_text() {
        assert_eq!(plan_label(1, false, None), "Plan: direct (1 hop)");
        assert_eq!(plan_label(2, false, Some(("1.6.4", "1.8.0"))), "Plan: 2 hops (bridge 1.6.4 → 1.8.0)");
        assert_eq!(plan_label(1, true, None), "Plan: resume interrupted update");
    }

    #[test]
    fn old_signing_era_detection() {
        assert!(is_outdated("1.3.11"));
        assert!(!is_outdated(MANDATORY_MIN_VERSION));
        assert!(!is_outdated("1.8.2"));
        // 4-segment device report (GET_STATUS) normalizes before comparing.
        assert!(is_outdated("1.5.9.42"));
        assert!(!is_outdated("1.8.2.7"));
        // Unparsable version never claims "outdated".
        assert!(!is_outdated("garbage"));
    }
}
