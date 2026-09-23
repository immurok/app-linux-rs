//! Shared client for the immurok daemon socket.
//!
//! The daemon answers one request per connection and closes it, so every
//! helper here opens its own connection. Everything is synchronous std I/O:
//! the TUI calls it from `thread::spawn`, the GUI from `gio::spawn_blocking`.

pub mod daemon;
pub mod enroll_hint;
pub mod enroll_session;
pub mod fingerprint;
pub mod fwupdate;
pub mod hosts;
pub mod keys;
pub mod keys_import;
pub mod pam;
pub mod ssh_import;
pub mod status;

pub use daemon::{fetch_key_cache, open_log_stream, probe_isolation, DaemonClient, Isolation};
