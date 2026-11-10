//! Writing fingerprint-gate hints onto the client's terminal.
//!
//! The daemon's ssh-agent knows *when* a gate is open and for which client
//! pid, but since privsep it runs as the `immurok` system user and can touch
//! neither `/proc/<pid>/fd/*` (ptrace check) nor the user's `/dev/pts/N`
//! (0600, owned by the user). We can: same uid as the `ssh` process that
//! asked for the signature. The daemon pushes [`TtyHint`] codes, this module
//! turns them into the spinner line.
//!
//! Two rules keep the delegation from widening what the daemon can do:
//!   * the text is ours, never the daemon's — see `immurok_common::tty_hint`;
//!   * we only write to a pid that belongs to **us** ([`pid_is_mine`]), so a
//!     compromised daemon cannot use the agent as a "write to any user's
//!     terminal" gadget.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use immurok_common::tty_hint::TtyHint;

/// Whether `pid` is a live process owned by our own uid.
pub fn pid_is_mine(pid: i32) -> bool {
    use std::os::unix::fs::MetadataExt;
    if pid <= 0 {
        return false;
    }
    // The owner of /proc/<pid> is the process's real uid. A pid that has
    // exited (or never existed) has no /proc entry, so this also covers the
    // window where ssh is gone before the hint arrives.
    match std::fs::metadata(format!("/proc/{}", pid)) {
        Ok(m) => m.uid() == unsafe { libc::geteuid() },
        Err(_) => false,
    }
}

/// Whether a `/proc/<pid>/fd/*` link target is a terminal we may animate.
fn is_terminal_path(p: &Path) -> bool {
    let Some(s) = p.to_str() else { return false };
    // "/dev/pts/" keeps the trailing slash so "/dev/ptsfoo" does not match;
    // "/dev/tty" covers the controlling terminal and the VCs.
    s.starts_with("/dev/pts/") || s.starts_with("/dev/tty")
}

/// Resolve the controlling terminal of `pid`, so the hint lands on the
/// terminal that ran `git push` / `ssh host`. ssh keeps its stderr (fd 2) on
/// the terminal even when git pipes its stdin/stdout, so try fd 2 first, then
/// 0/1. `None` if no fd points at a terminal (fully detached / piped).
pub fn client_tty_path(pid: i32) -> Option<PathBuf> {
    for fd in [2, 0, 1] {
        if let Ok(target) = std::fs::read_link(format!("/proc/{}/fd/{}", pid, fd)) {
            if is_terminal_path(&target) {
                return Some(target);
            }
        }
    }
    None
}

/// Animated spinner on one terminal — the ssh-agent's equivalent of the PAM
/// module's sudo spinner. A dedicated OS thread re-draws the (mutable)
/// message line every 80 ms.
pub struct TtySpinner {
    stop: Arc<AtomicBool>,
    msg: Arc<Mutex<String>>,
    join: Option<std::thread::JoinHandle<()>>,
    tty: PathBuf,
    finalized: bool,
}

impl TtySpinner {
    /// Start spinning on `tty`. `None` if the terminal cannot be opened.
    pub fn start(tty: PathBuf, initial_msg: &str) -> Option<TtySpinner> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().write(true).open(&tty).ok()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let msg = Arc::new(Mutex::new(initial_msg.to_string()));
        let msg_t = msg.clone();
        let join = std::thread::Builder::new()
            .name("imk-tty-hint".into())
            .spawn(move || {
                const FRAMES: [&str; 10] =
                    ["⠋", "⠙", "⠹", "⠸", "⠼",
                     "⠴", "⠦", "⠧", "⠇", "⠏"];
                let mut i = 0usize;
                while !stop_t.load(Ordering::Relaxed) {
                    let line = msg_t.lock().map(|g| g.clone()).unwrap_or_default();
                    // \r + erase-line + yellow frame + " " + message (self-coloured)
                    let _ = write!(
                        f,
                        "\r\x1b[K\x1b[33m{}\x1b[0m {}",
                        FRAMES[i % FRAMES.len()],
                        line
                    );
                    let _ = f.flush();
                    i += 1;
                    std::thread::sleep(Duration::from_millis(80));
                }
            })
            .ok()?;
        Some(TtySpinner {
            stop,
            msg,
            join: Some(join),
            tty,
            finalized: false,
        })
    }

    /// Replace the spinning line's text (kept until the next call / finish).
    pub fn set_message(&self, m: impl Into<String>) {
        if let Ok(mut g) = self.msg.lock() {
            *g = m.into();
        }
    }

    /// Stop the spinner. `Some(line)` leaves that line on the terminal (with a
    /// trailing newline); `None` erases the spinner line.
    pub fn finish(mut self, final_line: Option<String>) {
        self.finalize(final_line);
    }

    fn finalize(&mut self, final_line: Option<String>) {
        use std::io::Write;
        if self.finalized {
            return;
        }
        self.finalized = true;
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(&self.tty) {
            match final_line {
                Some(l) => {
                    let _ = write!(f, "\r\x1b[K{}\n", l);
                }
                None => {
                    let _ = f.write_all(b"\r\x1b[K"); // carriage return + erase to EOL
                }
            }
            let _ = f.flush();
        }
    }
}

impl Drop for TtySpinner {
    fn drop(&mut self) {
        self.finalize(None);
    }
}

/// The one hint line the session may have on screen, plus the pid it belongs
/// to. A hint for a different pid replaces it: concurrent signatures are
/// possible in principle, and a stale spinner is worse than a switched one.
#[derive(Default)]
pub struct HintRenderer {
    active: Option<(i32, TtySpinner)>,
}

impl HintRenderer {
    /// Apply one hint from the daemon. Best-effort throughout: a pid that is
    /// not ours, a client with no terminal, or a terminal we cannot open all
    /// mean "no hint", never an error.
    pub fn apply(&mut self, pid: i32, hint: TtyHint) {
        // A hint for someone else's pid is dropped here: the daemon is the
        // only writer on this channel, but it is also the component privsep
        // exists to contain, so it does not get to pick our write target.
        if !pid_is_mine(pid) {
            return;
        }

        if hint.is_final() {
            if let Some((active_pid, sp)) = self.active.take() {
                if active_pid == pid {
                    sp.finish(hint.message());
                } else {
                    // Someone else's spinner is on screen; leave it alone and
                    // put it back (dropping it would erase their line).
                    self.active = Some((active_pid, sp));
                }
            }
            return;
        }

        let Some(text) = hint.message() else { return };
        match &self.active {
            Some((active_pid, sp)) if *active_pid == pid => sp.set_message(text),
            _ => {
                // Replaces any spinner for a stale pid (its Drop erases it).
                self.active = client_tty_path(pid)
                    .and_then(|tty| TtySpinner::start(tty, &text))
                    .map(|sp| (pid, sp));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use immurok_common::tty_hint::TtyHintFailure;
    use std::io::Read;
    use std::os::unix::io::FromRawFd;

    #[test]
    fn own_pid_is_mine() {
        assert!(pid_is_mine(std::process::id() as i32));
    }

    #[test]
    fn pid_1_is_not_mine() {
        // init is root; skip if the suite itself runs as root.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        assert!(!pid_is_mine(1));
    }

    #[test]
    fn absent_and_invalid_pids_are_not_mine() {
        assert!(!pid_is_mine(0));
        assert!(!pid_is_mine(-1));
        // Above /proc/sys/kernel/pid_max on every sane system.
        assert!(!pid_is_mine(0x7fff_fffe));
    }

    #[test]
    fn only_terminal_devices_are_animated() {
        for good in ["/dev/pts/0", "/dev/pts/17", "/dev/tty", "/dev/tty3"] {
            assert!(is_terminal_path(Path::new(good)), "{}", good);
        }
        for bad in [
            "/dev/null",
            "/dev/ptsfoo",
            "/dev/ptmx",
            "/tmp/pts/0",
            "/home/user/.bash_history",
            "pipe:[12345]",
            "socket:[12345]",
            "anon_inode:[eventpoll]",
        ] {
            assert!(!is_terminal_path(Path::new(bad)), "{}", bad);
        }
    }

    /// Open a pty pair; the slave is a real `/dev/pts/N` owned by us, which is
    /// exactly what the spinner writes to in production.
    fn open_pty() -> (std::fs::File, PathBuf) {
        let mut master = 0;
        let mut slave = 0;
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "openpty failed");
        let name = unsafe { libc::ptsname(master) };
        assert!(!name.is_null(), "ptsname failed");
        let path = unsafe { std::ffi::CStr::from_ptr(name) }
            .to_string_lossy()
            .to_string();
        // The slave fd is not needed: the spinner opens the path itself.
        unsafe { libc::close(slave) };
        // Non-blocking so a silent spinner fails the test instead of hanging.
        unsafe { libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK) };
        (unsafe { std::fs::File::from_raw_fd(master) }, PathBuf::from(path))
    }

    /// Read whatever is pending on the master side, once the writer has had a
    /// moment to flush.
    fn drain(master: &mut std::fs::File) -> String {
        std::thread::sleep(Duration::from_millis(50));
        let mut seen = String::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = master.read(&mut buf) {
            if n == 0 {
                break;
            }
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        seen
    }

    /// Drain the master side for up to ~2s, until `want` shows up.
    fn read_until(master: &mut std::fs::File, want: &str) -> String {
        let mut seen = String::new();
        for _ in 0..200 {
            let mut buf = [0u8; 4096];
            match master.read(&mut buf) {
                Ok(0) => {}
                Ok(n) => seen.push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(_) => {}
            }
            if seen.contains(want) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        seen
    }

    #[test]
    fn spinner_draws_animates_and_leaves_a_final_line() {
        let (mut master, path) = open_pty();
        let sp = TtySpinner::start(path, "waiting-for-touch").expect("spinner should start");

        let seen = read_until(&mut master, "waiting-for-touch");
        assert!(seen.contains("waiting-for-touch"), "no initial message: {:?}", seen);
        assert!(seen.contains("\r\x1b[K"), "line not redrawn in place: {:?}", seen);
        assert!(
            seen.chars().any(|c| "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(c)),
            "no spinner frame: {:?}",
            seen
        );

        sp.set_message("second-message");
        let seen = read_until(&mut master, "second-message");
        assert!(seen.contains("second-message"), "message not swapped: {:?}", seen);

        // finish(Some) leaves the line behind, terminated, for the user to read.
        let (mut master2, path2) = open_pty();
        let sp2 = TtySpinner::start(path2, "pending").expect("spinner should start");
        sp2.finish(Some("final-outcome".into()));
        let seen = read_until(&mut master2, "final-outcome");
        assert!(seen.contains("final-outcome"), "no final line: {:?}", seen);
        assert!(seen.ends_with('\n'), "final line not terminated: {:?}", seen);
    }

    #[test]
    fn spinner_erases_on_finish_without_a_line() {
        let (mut master, path) = open_pty();
        let sp = TtySpinner::start(path, "transient").expect("spinner should start");
        read_until(&mut master, "transient");
        sp.finish(None);
        let tail = drain(&mut master);
        assert!(tail.ends_with("\r\x1b[K"), "line not erased: {:?}", tail);
        assert!(!tail.contains('\n'), "erase should not add a newline: {:?}", tail);
    }

    #[test]
    fn renderer_ignores_pids_that_are_not_ours() {
        let mut r = HintRenderer::default();
        // pid 1 is root's (or, as root, a pid with no terminal) — either way
        // this must not panic and must leave nothing active.
        r.apply(1, TtyHint::Waiting);
        assert!(r.active.is_none());
        r.apply(0x7fff_fffe, TtyHint::Waiting);
        assert!(r.active.is_none());
        // A final hint for an unknown pid is a no-op, not a panic.
        r.apply(1, TtyHint::Failed(TtyHintFailure::TimedOut));
        assert!(r.active.is_none());
    }
}
