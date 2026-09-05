//! Synchronous Unix socket client for communicating with the daemon.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// Default read timeout. Long enough for any single device round-trip,
/// short enough that a wedged daemon surfaces within a minute.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Client for the immurok daemon Unix socket.
pub struct DaemonClient {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl DaemonClient {
    /// Connect to the daemon socket (`/run/immurok/pam.sock` unless overridden
    /// — see `immurok_common::paths`).
    pub fn connect() -> Result<Self, String> {
        let sock_path = immurok_common::paths::pam_socket();

        let stream = UnixStream::connect(&sock_path).map_err(|_| {
            "Cannot connect to daemon. Is immurok-daemon running?".to_string()
        })?;

        stream
            .set_read_timeout(Some(DEFAULT_READ_TIMEOUT))
            .map_err(|e| format!("Failed to set read timeout: {}", e))?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("Failed to set write timeout: {}", e))?;

        let reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);

        Ok(Self { stream, reader })
    }

    /// Send a request line and read a single response line.
    pub fn send(&mut self, request: &str) -> Result<String, String> {
        let msg = format!("{}\n", request);
        self.stream
            .write_all(msg.as_bytes())
            .map_err(|e| format!("Send failed: {}", e))?;

        let mut line = String::new();
        self.reader
            .read_line(&mut line)
            .map_err(|e| format!("Read failed: {}", e))?;

        Ok(line.trim().to_string())
    }

    /// Send a request and wait for the answer with a per-call read timeout.
    ///
    /// Binding a second host is two independent human actions — a 30 s
    /// fingerprint gate then a 30 s button window — and blows past the
    /// default. Without this the daemon would still finish the pairing while
    /// the CLI had already printed a failure.
    ///
    /// The default deliberately stays 60 s for everything else: raising it
    /// globally would make a wedged daemon take minutes to surface.
    ///
    /// `self.reader` wraps a dup of `self.stream`; dup'd descriptors share
    /// the socket's SO_RCVTIMEO, so setting it here does apply to the read.
    pub fn send_with_timeout(
        &mut self,
        request: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        self.stream
            .set_read_timeout(Some(timeout))
            .map_err(|e| format!("Failed to set read timeout: {}", e))?;
        let result = self.send(request);
        let _ = self.stream.set_read_timeout(Some(DEFAULT_READ_TIMEOUT));
        result
    }
}

/// Read one of the daemon's key caches (`ssh` or `names`) as JSON.
///
/// These used to be files under `~/.immurok` that the CLI read directly. The
/// daemon's state moved to a 0700 directory owned by its own system user, so
/// it hands the JSON over instead. An unreachable daemon yields an empty list
/// — same as a missing cache file did.
pub fn fetch_key_cache(kind: &str) -> Vec<serde_json::Value> {
    let mut client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let reply = match client.send(&format!("KEY:CACHE:{}", kind)) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    reply
        .strip_prefix("OK:")
        .and_then(|json| serde_json::from_str(json.trim()).ok())
        .unwrap_or_default()
}

/// Open a live log stream. Returns the raw socket; the caller reads lines
/// until EOF. The daemon sends its buffered tail first, then live lines.
pub fn open_log_stream() -> Result<UnixStream, String> {
    let path = immurok_common::paths::pam_socket();
    let mut stream = UnixStream::connect(&path)
        .map_err(|_| "Cannot connect to daemon. Is immurok-daemon running?".to_string())?;
    stream
        .write_all(b"SUBSCRIBE:LOG\n")
        .map_err(|e| format!("Failed to subscribe: {}", e))?;
    Ok(stream)
}

/// Where the daemon is running from, as it reports in `GET:INFO`.
pub struct Isolation {
    /// True when the daemon runs as its own system user with the socket in
    /// the machine-wide runtime directory.
    pub isolated: bool,
    pub daemon_uid: Option<u32>,
    pub socket: String,
}

/// Ask the daemon whether it is privilege-separated.
///
/// Worth surfacing rather than hiding: on a daemon that still runs as the
/// logged-in user, any process of that user can stop it and answer PAM in its
/// place — zero-interaction sudo. That is the hole the separation closes.
pub fn probe_isolation() -> Option<Isolation> {
    let mut client = DaemonClient::connect().ok()?;
    let rsp = client.send("GET:INFO").ok()?;
    let body = rsp.strip_prefix("OK:")?;

    let mut daemon_uid = None;
    let mut socket = String::new();
    for field in body.split(':') {
        match field.split_once('=') {
            Some(("uid", v)) => daemon_uid = v.trim().parse::<u32>().ok(),
            Some(("sock", v)) => socket = v.trim().to_string(),
            _ => {}
        }
    }

    // An older daemon reports neither field; treat that as "not isolated",
    // which is exactly what it is.
    let own = unsafe { libc::getuid() };
    let isolated = daemon_uid.is_some_and(|u| u != own && u != 0)
        && socket.starts_with(immurok_common::protocol::SYSTEM_RUNTIME_DIR);

    Some(Isolation {
        isolated,
        daemon_uid,
        socket,
    })
}
