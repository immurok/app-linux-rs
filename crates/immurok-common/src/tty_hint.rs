//! Fingerprint-gate hints for a client's terminal, as pushed from the daemon
//! to `immurok-session-agent`.
//!
//! The ssh-agent runs inside the daemon, which since privsep is the dedicated
//! `immurok` system user: it can neither `readlink /proc/<ssh pid>/fd/2` (the
//! ptrace check needs the same uid) nor open the resulting `/dev/pts/N` (owned
//! by the logged-in user). So the daemon decides *what* to say and the session
//! agent — running as that user — is what writes it.
//!
//! The wire carries an **enum code, never free text**: the agent maps it to
//! its own fixed strings. A pts interprets what is written to it, so letting
//! daemon-side text through verbatim would make any future hint that embeds
//! remote data (a key comment, a hostname) an ANSI/OSC injection into the
//! user's terminal. The only variable datum here is an attempt count, which
//! is a number.

/// Line prefix on the session-agent channel.
pub const PREFIX: &str = "TTY_HINT:";

/// Why the gate ended, for the final line left on the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtyHintFailure {
    /// The device rejected the finger outright.
    NotRecognized,
    /// No touch (or no verdict) before the gate's deadline.
    TimedOut,
    /// Gate failed for any other reason.
    Failed,
    /// Gate passed but the signature never came back.
    SigningFailed,
}

/// One state of the hint line on the client's terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtyHint {
    /// Gate open, waiting for the touch.
    Waiting,
    /// Finger did not match; `remaining` tries left before the gate closes.
    Mismatch { remaining: u8 },
    /// Touch matched; the device is producing the signature.
    Signing,
    /// Finished cleanly — erase the line (the command's own output follows).
    Done,
    /// Finished badly — leave this reason on the terminal.
    Failed(TtyHintFailure),
}

impl TtyHint {
    /// The wire code for this hint.
    fn code(&self) -> String {
        match self {
            TtyHint::Waiting => "waiting".into(),
            TtyHint::Mismatch { remaining } => format!("mismatch-{}", remaining),
            TtyHint::Signing => "signing".into(),
            TtyHint::Done => "done".into(),
            TtyHint::Failed(TtyHintFailure::NotRecognized) => "fail-unrecognized".into(),
            TtyHint::Failed(TtyHintFailure::TimedOut) => "fail-timeout".into(),
            TtyHint::Failed(TtyHintFailure::Failed) => "fail-generic".into(),
            TtyHint::Failed(TtyHintFailure::SigningFailed) => "fail-signing".into(),
        }
    }

    /// Encode as a channel line (no trailing newline).
    pub fn encode(&self, pid: i32) -> String {
        format!("{}{}:{}", PREFIX, pid, self.code())
    }

    /// Parse a channel line. `None` for anything that is not a well-formed
    /// hint — an unknown code from a newer daemon included.
    pub fn parse(line: &str) -> Option<(i32, TtyHint)> {
        let rest = line.strip_prefix(PREFIX)?;
        let (pid, code) = rest.split_once(':')?;
        // Codes are a closed set of ASCII tokens; a stray ':' means the line
        // was not produced by `encode`.
        if code.contains(':') {
            return None;
        }
        let pid: i32 = pid.parse().ok()?;
        if pid <= 0 {
            return None;
        }
        let hint = match code {
            "waiting" => TtyHint::Waiting,
            "signing" => TtyHint::Signing,
            "done" => TtyHint::Done,
            "fail-unrecognized" => TtyHint::Failed(TtyHintFailure::NotRecognized),
            "fail-timeout" => TtyHint::Failed(TtyHintFailure::TimedOut),
            "fail-generic" => TtyHint::Failed(TtyHintFailure::Failed),
            "fail-signing" => TtyHint::Failed(TtyHintFailure::SigningFailed),
            other => {
                let remaining = other.strip_prefix("mismatch-")?.parse().ok()?;
                TtyHint::Mismatch { remaining }
            }
        };
        Some((pid, hint))
    }

    /// The text to show, already coloured. `None` for [`TtyHint::Done`],
    /// which erases the line instead of writing one.
    ///
    /// Yellow while waiting (matching the PAM/sudo spinner), red on a
    /// rejection, green once the touch is in. Every string is a literal here;
    /// the attempt count is the only interpolated value.
    pub fn message(&self) -> Option<String> {
        Some(match self {
            TtyHint::Waiting => "\x1b[33mPlease verify your fingerprint...\x1b[0m".into(),
            TtyHint::Mismatch { remaining } => {
                let noun = if *remaining == 1 { "attempt" } else { "attempts" };
                format!(
                    "\x1b[31m✗ Fingerprint not recognized\x1b[0m — {} {} left, touch again...",
                    remaining, noun
                )
            }
            TtyHint::Signing => {
                "\x1b[32m✓ Verified — signing...\x1b[0m".into()
            }
            TtyHint::Done => return None,
            TtyHint::Failed(TtyHintFailure::NotRecognized) => {
                "\x1b[31m✗ Fingerprint not recognized — verification failed\x1b[0m"
                    .into()
            }
            TtyHint::Failed(TtyHintFailure::TimedOut) => {
                "\x1b[31m✗ Fingerprint verification timed out\x1b[0m".into()
            }
            TtyHint::Failed(TtyHintFailure::Failed) => {
                "\x1b[31m✗ Fingerprint verification failed\x1b[0m".into()
            }
            TtyHint::Failed(TtyHintFailure::SigningFailed) => {
                "\x1b[31m✗ Signing failed\x1b[0m".into()
            }
        })
    }

    /// Whether this hint stops the spinner.
    pub fn is_final(&self) -> bool {
        matches!(self, TtyHint::Done | TtyHint::Failed(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[TtyHint] = &[
        TtyHint::Waiting,
        TtyHint::Mismatch { remaining: 1 },
        TtyHint::Mismatch { remaining: 4 },
        TtyHint::Signing,
        TtyHint::Done,
        TtyHint::Failed(TtyHintFailure::NotRecognized),
        TtyHint::Failed(TtyHintFailure::TimedOut),
        TtyHint::Failed(TtyHintFailure::Failed),
        TtyHint::Failed(TtyHintFailure::SigningFailed),
    ];

    #[test]
    fn round_trips_every_variant() {
        for h in ALL {
            let line = h.encode(4242);
            assert_eq!(
                TtyHint::parse(&line),
                Some((4242, *h)),
                "round-trip failed for {:?} (line {:?})",
                h,
                line
            );
        }
    }

    #[test]
    fn encoded_lines_are_single_line_and_prefixed() {
        for h in ALL {
            let line = h.encode(7);
            assert!(line.starts_with(PREFIX), "missing prefix: {:?}", line);
            assert!(!line.contains('\n'), "embedded newline: {:?}", line);
        }
    }

    #[test]
    fn rejects_malformed_lines() {
        for bad in [
            "",
            "TTY_HINT:",
            "TTY_HINT:12",
            "TTY_HINT:abc:waiting",
            "TTY_HINT:12:waiting:extra",
            "TTY_HINT:12:nonsense",
            // A code a newer daemon might add: unknown, so ignored.
            "TTY_HINT:12:enrolling",
            "TTY_HINT:12:mismatch-",
            "TTY_HINT:12:mismatch-x",
            "UI:DIALOG:AUTH",
            "NOTIFY:hello",
        ] {
            assert_eq!(TtyHint::parse(bad), None, "should not parse: {:?}", bad);
        }
    }

    #[test]
    fn negative_pid_is_rejected() {
        assert_eq!(TtyHint::parse("TTY_HINT:-1:waiting"), None);
        assert_eq!(TtyHint::parse("TTY_HINT:0:waiting"), None);
    }

    #[test]
    fn done_erases_everything_else_speaks() {
        assert_eq!(TtyHint::Done.message(), None);
        for h in ALL.iter().filter(|h| **h != TtyHint::Done) {
            let m = h.message().expect("should carry text");
            assert!(!m.is_empty(), "empty message for {:?}", h);
        }
    }

    #[test]
    fn mismatch_counts_attempts_in_english() {
        let one = TtyHint::Mismatch { remaining: 1 }.message().unwrap();
        assert!(one.contains("1 attempt left"), "{:?}", one);
        let many = TtyHint::Mismatch { remaining: 3 }.message().unwrap();
        assert!(many.contains("3 attempts left"), "{:?}", many);
    }

    #[test]
    fn only_final_states_stop_the_spinner() {
        assert!(!TtyHint::Waiting.is_final());
        assert!(!TtyHint::Mismatch { remaining: 2 }.is_final());
        assert!(!TtyHint::Signing.is_final());
        assert!(TtyHint::Done.is_final());
        assert!(TtyHint::Failed(TtyHintFailure::TimedOut).is_final());
    }

    /// The guard that justifies the enum-code wire format: whatever reaches a
    /// pts carries only SGR colour escapes — no OSC/CSI that could retitle a
    /// window, no newline that could fake a prompt, no carriage return that
    /// could overwrite one.
    #[test]
    fn messages_carry_only_colour_escapes() {
        for h in ALL {
            let Some(m) = h.message() else { continue };
            assert!(!m.contains("\x1b]"), "OSC escape in {:?}: {:?}", h, m);
            assert!(!m.contains('\x07'), "BEL in {:?}: {:?}", h, m);
            for (i, c) in m.char_indices() {
                if c == '\x1b' {
                    let rest = &m[i + 1..];
                    assert!(
                        rest.starts_with('['),
                        "non-CSI escape in {:?}: {:?}",
                        h,
                        m
                    );
                    let seq: String = rest[1..]
                        .chars()
                        .take_while(|c| !c.is_ascii_alphabetic())
                        .collect();
                    let end = rest[1 + seq.len()..].chars().next();
                    assert_eq!(end, Some('m'), "non-SGR CSI in {:?}: {:?}", h, m);
                    assert!(
                        seq.chars().all(|c| c.is_ascii_digit() || c == ';'),
                        "odd SGR params in {:?}: {:?}",
                        h,
                        m
                    );
                } else {
                    assert!(
                        !c.is_control(),
                        "control char {:?} in {:?}: {:?}",
                        c,
                        h,
                        m
                    );
                }
            }
        }
    }
}
