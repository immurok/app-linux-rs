//! Enrollment driver: `FP:ENROLL`, then poll `FP:STATUS` until the device
//! reports complete or failed.
//!
//! Same state machine as the TUI's `App::action_enroll`, written as a pure
//! function ([`drive`]) over injected poll / bitmap / clock functions so the
//! event sequence can be unit-tested. [`run_enrollment`] binds it to the real
//! socket and blocks the calling thread; the GUI runs it on a worker thread
//! and forwards each event to the main loop.
//!
//! The daemon caches only the *latest* enrollment notification, so two
//! consecutive polls that read the same tuple are the same event and are
//! reported once. (Two overlap rejects with no other event in between are
//! therefore indistinguishable — same limitation as the TUI.)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::fingerprint::{enroll_cancel, enroll_start, fp_list, fp_status, EnrollStatus, FpSlots};

pub const POLL_INTERVAL: Duration = Duration::from_millis(150);
/// While `FP:STATUS` keeps saying IDLE, re-read the bitmap this often in
/// case the completion notification was missed.
pub const IDLE_BITMAP_CHECK: Duration = Duration::from_secs(3);
pub const ENROLL_TIMEOUT: Duration = Duration::from_secs(360);
pub const DEFAULT_TOTAL: u8 = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrollProgress {
    /// `FP:ENROLL` is about to be sent; the device may first ask for an
    /// already-enrolled finger.
    GateWaiting,
    /// Capture has begun on the device.
    Started,
    /// The user should now press frame `next_step` (1-based); `captured`
    /// frames are already in.
    Step { next_step: u8, captured: u8, total: u8 },
    LiftFinger,
    /// Too similar to the previous frame — shift and press again.
    Overlap,
    Processing,
    Complete,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continue {
    Go,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Complete,
    Failed,
    Cancelled,
}

pub trait Clock {
    fn now(&self) -> Instant;
    fn sleep(&mut self, d: Duration);
}

struct RealClock;

impl Clock for RealClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
}

fn step_event(current: u8, total: u8) -> EnrollProgress {
    EnrollProgress::Step {
        next_step: current.saturating_add(1).min(total.max(1)),
        captured: current,
        total,
    }
}

/// Poll loop after `FP:ENROLL` has been accepted. Emits `Started` first.
///
/// `stop` is the out-of-band cancel: `tick` only runs when the device
/// reports something new, and after an `FP:ENROLL_CANCEL` the firmware
/// sends no further progress at all (`hidkbd.c` ENROLL_CANCEL) — every
/// poll then reads IDLE and `tick` is never called again, so a UI that
/// cancels mid-capture would be stuck here until the 360 s timeout.
/// Checking `stop` every iteration bounds that at one poll interval.
pub fn drive(
    slot: u8,
    mut poll: impl FnMut() -> Result<EnrollStatus, String>,
    mut bitmap: impl FnMut() -> Result<FpSlots, String>,
    stop: &mut impl FnMut() -> bool,
    tick: &mut impl FnMut(EnrollProgress) -> Continue,
    clock: &mut impl Clock,
) -> Outcome {
    if stop() || tick(EnrollProgress::Started) == Continue::Stop {
        return Outcome::Cancelled;
    }
    let started = clock.now();
    let mut last: Option<EnrollStatus> = None;
    let mut idle_since: Option<Instant> = None;

    loop {
        if clock.now().duration_since(started) > ENROLL_TIMEOUT {
            tick(EnrollProgress::Failed("Enrollment timed out".into()));
            return Outcome::Failed;
        }
        // Cancelled by the UI: leave without a further event — the caller
        // already knows, and the dialog is closing.
        if stop() {
            return Outcome::Cancelled;
        }
        clock.sleep(POLL_INTERVAL);

        let status = match poll() {
            Ok(s) => s,
            Err(e) if e.contains("NOT_CONNECTED") => {
                tick(EnrollProgress::Failed("Device disconnected".into()));
                return Outcome::Failed;
            }
            // A single failed poll (socket hiccup) is not the end of the world.
            Err(_) => continue,
        };

        if status == EnrollStatus::Idle {
            let now = clock.now();
            match idle_since {
                None => idle_since = Some(now),
                Some(t) if now.duration_since(t) >= IDLE_BITMAP_CHECK => {
                    idle_since = Some(now);
                    if let Ok(b) = bitmap() {
                        if b.is_enrolled(slot) {
                            tick(EnrollProgress::Complete);
                            return Outcome::Complete;
                        }
                    }
                }
                Some(_) => {}
            }
            continue;
        }
        idle_since = None;

        if last == Some(status) {
            continue;
        }
        last = Some(status);

        let cont = match status {
            EnrollStatus::Waiting { current, total } => tick(step_event(current, total)),
            EnrollStatus::Captured { current, total } => tick(step_event(current, total)),
            EnrollStatus::LiftFinger => tick(EnrollProgress::LiftFinger),
            EnrollStatus::Overlap => tick(EnrollProgress::Overlap),
            EnrollStatus::Processing => tick(EnrollProgress::Processing),
            EnrollStatus::Complete => {
                tick(EnrollProgress::Complete);
                return Outcome::Complete;
            }
            EnrollStatus::Failed => {
                tick(EnrollProgress::Failed("Enrollment failed".into()));
                return Outcome::Failed;
            }
            EnrollStatus::Idle => unreachable!("handled above"),
        };
        if cont == Continue::Stop {
            return Outcome::Cancelled;
        }
    }
}

/// Blocking end-to-end enrollment of `slot` against the real daemon.
/// `tick` is called on the calling thread for every event; returning
/// [`Continue::Stop`] aborts (an `FP:ENROLL_CANCEL` is sent).
///
/// `cancelled` is the same abort seen from another thread: the GUI sets it
/// from its Cancel button, and the poll loop checks it every 150 ms even
/// when the device has gone quiet (which is exactly what happens after a
/// cancel during capture).
pub fn run_enrollment(
    slot: u8,
    cancelled: Arc<AtomicBool>,
    mut tick: impl FnMut(EnrollProgress) -> Continue,
) {
    let mut stop = || cancelled.load(Ordering::Relaxed);
    if stop() || tick(EnrollProgress::GateWaiting) == Continue::Stop {
        return;
    }
    if let Err(e) = enroll_start(slot) {
        tick(EnrollProgress::Failed(e));
        return;
    }
    let mut clock = RealClock;
    // Harmless duplicate when the UI already sent its own cancel; kept so
    // callers that only flip the flag (CLI/TUI-style) still release the
    // device.
    if drive(slot, fp_status, fp_list, &mut stop, &mut tick, &mut clock) == Outcome::Cancelled {
        enroll_cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct FakeClock {
        now: Instant,
    }
    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            self.now
        }
        fn sleep(&mut self, d: Duration) {
            self.now += d;
        }
    }

    /// Runs `drive` over a scripted FP:STATUS sequence; once the script is
    /// exhausted every poll says IDLE. The bitmap query always reports
    /// `bitmap_after`. Returns the events the UI would see.
    fn script(
        polls: Vec<Result<EnrollStatus, String>>,
        bitmap_after: u8,
        stop_on: Option<EnrollProgress>,
    ) -> (Vec<EnrollProgress>, Outcome) {
        script_with_stop(polls, bitmap_after, stop_on, |_| false)
    }

    /// As [`script`], plus an out-of-band cancel: `stop(n)` is asked on the
    /// n-th (1-based) `drive` cancel check.
    fn script_with_stop(
        polls: Vec<Result<EnrollStatus, String>>,
        bitmap_after: u8,
        stop_on: Option<EnrollProgress>,
        stop: impl Fn(u32) -> bool,
    ) -> (Vec<EnrollProgress>, Outcome) {
        let mut polls: VecDeque<_> = polls.into();
        let mut events = Vec::new();
        let mut clock = FakeClock { now: Instant::now() };
        let mut stop_calls = 0u32;
        let outcome = drive(
            1,
            || polls.pop_front().unwrap_or(Ok(EnrollStatus::Idle)),
            || Ok(FpSlots { bitmap: bitmap_after }),
            &mut || {
                stop_calls += 1;
                stop(stop_calls)
            },
            &mut |ev| {
                let stop = stop_on.as_ref() == Some(&ev);
                events.push(ev);
                if stop {
                    Continue::Stop
                } else {
                    Continue::Go
                }
            },
            &mut clock,
        );
        (events, outcome)
    }

    #[test]
    fn happy_path_emits_steps_then_complete() {
        use EnrollStatus as S;
        let (events, outcome) = script(
            vec![
                Ok(S::Waiting { current: 0, total: 6 }),
                Ok(S::Captured { current: 1, total: 6 }),
                Ok(S::LiftFinger),
                Ok(S::Waiting { current: 1, total: 6 }),
                Ok(S::Captured { current: 2, total: 6 }),
                Ok(S::Processing),
                Ok(S::Complete),
            ],
            0,
            None,
        );
        assert_eq!(outcome, Outcome::Complete);
        assert_eq!(
            events,
            vec![
                EnrollProgress::Started,
                EnrollProgress::Step { next_step: 1, captured: 0, total: 6 },
                EnrollProgress::Step { next_step: 2, captured: 1, total: 6 },
                EnrollProgress::LiftFinger,
                EnrollProgress::Step { next_step: 2, captured: 1, total: 6 },
                EnrollProgress::Step { next_step: 3, captured: 2, total: 6 },
                EnrollProgress::Processing,
                EnrollProgress::Complete,
            ]
        );
    }

    #[test]
    fn repeated_identical_polls_are_not_repeated_events() {
        use EnrollStatus as S;
        let (events, _) = script(
            vec![
                Ok(S::Waiting { current: 0, total: 6 }),
                Ok(S::Waiting { current: 0, total: 6 }),
                Ok(S::Waiting { current: 0, total: 6 }),
                Ok(S::Complete),
            ],
            0,
            None,
        );
        assert_eq!(events.len(), 3); // Started, Step 1, Complete
    }

    #[test]
    fn overlap_does_not_advance_step() {
        use EnrollStatus as S;
        let (events, _) = script(
            vec![
                Ok(S::Captured { current: 2, total: 6 }),
                Ok(S::Overlap),
                Ok(S::Waiting { current: 2, total: 6 }),
                Ok(S::Complete),
            ],
            0,
            None,
        );
        assert_eq!(events[1], EnrollProgress::Step { next_step: 3, captured: 2, total: 6 });
        assert_eq!(events[2], EnrollProgress::Overlap);
        assert_eq!(events[3], EnrollProgress::Step { next_step: 3, captured: 2, total: 6 });
    }

    #[test]
    fn failed_status_ends_with_failed() {
        let (events, outcome) = script(vec![Ok(EnrollStatus::Failed)], 0, None);
        assert_eq!(outcome, Outcome::Failed);
        assert!(matches!(events.last(), Some(EnrollProgress::Failed(_))));
    }

    #[test]
    fn not_connected_is_reported_as_disconnect() {
        let (events, outcome) = script(vec![Err("ERROR:NOT_CONNECTED".into())], 0, None);
        assert_eq!(outcome, Outcome::Failed);
        assert_eq!(events.last(), Some(&EnrollProgress::Failed("Device disconnected".into())));
    }

    #[test]
    fn transient_poll_errors_are_skipped() {
        let (events, outcome) =
            script(vec![Err("Read failed: EAGAIN".into()), Ok(EnrollStatus::Complete)], 0, None);
        assert_eq!(outcome, Outcome::Complete);
        assert_eq!(events, vec![EnrollProgress::Started, EnrollProgress::Complete]);
    }

    #[test]
    fn stop_from_ui_cancels() {
        let (events, outcome) = script(
            vec![Ok(EnrollStatus::Waiting { current: 0, total: 6 }), Ok(EnrollStatus::Complete)],
            0,
            Some(EnrollProgress::Step { next_step: 1, captured: 0, total: 6 }),
        );
        assert_eq!(outcome, Outcome::Cancelled);
        assert_eq!(events.len(), 2); // Started, Step 1 — nothing after Stop
    }

    #[test]
    fn external_cancel_stops_without_waiting_for_the_timeout() {
        // After FP:ENROLL_CANCEL the firmware goes quiet: every poll reads
        // IDLE and `tick` is never called again. Only the out-of-band stop
        // gets us out — and it must not take 360 s.
        let (events, outcome) = script_with_stop(vec![], 0, None, |n| n >= 2);
        assert_eq!(outcome, Outcome::Cancelled);
        assert_eq!(events, vec![EnrollProgress::Started]);
    }

    #[test]
    fn idle_falls_back_to_bitmap_after_three_seconds() {
        // Every poll says IDLE (e.g. the completion notification was missed);
        // the bitmap says slot 1 is enrolled → Complete.
        let (events, outcome) = script(vec![], 0b10, None);
        assert_eq!(outcome, Outcome::Complete);
        assert_eq!(events.last(), Some(&EnrollProgress::Complete));
    }

    #[test]
    fn times_out_after_enroll_timeout() {
        let (events, outcome) = script(vec![], 0, None);
        assert_eq!(outcome, Outcome::Failed);
        assert_eq!(events.last(), Some(&EnrollProgress::Failed("Enrollment timed out".into())));
    }
}
