//! Synchronous Unix socket client for communicating with the daemon.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// Client for the immurok daemon Unix socket.
pub struct DaemonClient {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl DaemonClient {
    /// Connect to the daemon socket at `$XDG_RUNTIME_DIR/immurok/pam.sock`.
    pub fn connect() -> Result<Self, String> {
        let sock_path = if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            format!("{}/immurok/{}", runtime_dir, immurok_common::protocol::PAM_SOCKET_NAME)
        } else {
            let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
            format!("{}/{}/{}", home, immurok_common::protocol::IMMUROK_DIR, immurok_common::protocol::PAM_SOCKET_NAME)
        };

        let stream = UnixStream::connect(&sock_path).map_err(|_| {
            "Cannot connect to daemon. Is immurok-daemon running?".to_string()
        })?;

        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
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

}
