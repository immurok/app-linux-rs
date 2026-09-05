//! An in-memory tail of the daemon's own log, served over the socket.
//!
//! The log file now lives in `/var/log/immurok` (0640, owned by the daemon
//! user): the person running `immurok-cli` cannot open it, and making it
//! world-readable would publish every authentication event on a multi-user
//! machine. So the daemon keeps the last few hundred lines itself and hands
//! them out over the socket, where the same active-session check as every
//! other management command applies.
//!
//! Installed as a `tracing` writer, so it sees exactly what the file sees.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::broadcast;

/// How much history a newly attached viewer gets.
const RING_CAPACITY: usize = 500;
/// Slow readers past this many pending lines get dropped rather than stalling
/// the daemon's logging path.
const BROADCAST_CAPACITY: usize = 256;

pub struct LogSink {
    file: Option<Mutex<std::fs::File>>,
    ring: Mutex<VecDeque<String>>,
    tx: broadcast::Sender<String>,
}

impl LogSink {
    fn new(file: Option<std::fs::File>) -> Self {
        Self {
            file: file.map(Mutex::new),
            ring: Mutex::new(VecDeque::with_capacity(RING_CAPACITY)),
            tx: broadcast::channel(BROADCAST_CAPACITY).0,
        }
    }

    /// The lines a viewer should see before the live stream starts.
    pub fn snapshot(&self) -> Vec<String> {
        self.ring
            .lock()
            .map(|r| r.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    fn emit(&self, bytes: &[u8]) {
        if let Some(file) = &self.file {
            if let Ok(mut f) = file.lock() {
                use std::io::Write;
                let _ = f.write_all(bytes);
            }
        }
        for line in String::from_utf8_lossy(bytes).lines() {
            if line.is_empty() {
                continue;
            }
            if let Ok(mut ring) = self.ring.lock() {
                if ring.len() == RING_CAPACITY {
                    ring.pop_front();
                }
                ring.push_back(line.to_string());
            }
            // Err just means nobody is watching right now.
            let _ = self.tx.send(line.to_string());
        }
    }
}

static SINK: OnceLock<Arc<LogSink>> = OnceLock::new();

/// Install the process-wide sink. Returns the `MakeWriter` for
/// `tracing_subscriber`. Idempotent; a second call keeps the first sink.
pub fn install(file: Option<std::fs::File>) -> LogMaker {
    let _ = SINK.set(Arc::new(LogSink::new(file)));
    LogMaker
}

pub fn global() -> Option<&'static Arc<LogSink>> {
    SINK.get()
}

#[derive(Clone)]
pub struct LogMaker;

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogMaker {
    type Writer = LogWriter;
    fn make_writer(&'a self) -> Self::Writer {
        LogWriter { buf: Vec::new() }
    }
}

/// One formatted event. `tracing` builds the whole line through `write`
/// calls and drops the writer at the end of the event, which is where we
/// hand it over — that way a line never reaches the ring half-written.
pub struct LogWriter {
    buf: Vec<u8>,
}

impl io::Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        if let Some(sink) = global() {
            sink.emit(&self.buf);
        }
    }
}
