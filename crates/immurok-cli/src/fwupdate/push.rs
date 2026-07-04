//! OTA push engine over the daemon socket session.
//!
//! Command sequence mirrors ota/ota-update.py and commands/ota.rs:
//!   OTA:INFO → OTA:ERASE → OTA:HEADER:<b64> → OTA:WRITE:<off>:<b64> × n → OTA:END
//! The daemon holds one persistent OTA session per connection (immurok-daemon
//! src/ota.rs), so a retry needs a fresh connection — hence the `connect`
//! factory in push_with_retry.

use base64::Engine;

use immurok_common::fwupdate::imfw::{ImfwPackage, CHUNK_SIZE};

use super::error::FwUpdateError;

pub trait OtaChannel {
    fn send(&mut self, request: &str) -> Result<String, String>;
}

impl OtaChannel for crate::socket_client::DaemonClient {
    fn send(&mut self, request: &str) -> Result<String, String> {
        crate::socket_client::DaemonClient::send(self, request)
    }
}

pub enum PushEvent {
    Stage(&'static str),
    /// Raw OK payload of OTA:INFO (flag/size/block/chip) for display.
    DeviceInfo(String),
    Chunk { done: usize, total: usize },
}

fn is_ok(resp: &str) -> bool {
    let t = resp.trim();
    t == "OK" || t.starts_with("OK:")
}

fn transfer(stage: &'static str, detail: impl Into<String>) -> FwUpdateError {
    FwUpdateError::Transfer { stage, detail: detail.into() }
}

/// Device-reported ERROR mapped per stage. LOW_BATTERY (firmware refuses
/// long writes below 5%) is terminal — retrying won't charge the battery.
fn map_device_error(stage: &'static str, resp: &str) -> FwUpdateError {
    if resp.contains("LOW_BATTERY") {
        return FwUpdateError::LowBattery;
    }
    transfer(stage, resp)
}

pub fn is_retryable(e: &FwUpdateError) -> bool {
    matches!(e, FwUpdateError::Transfer { .. })
}

pub fn push_once<C: OtaChannel>(
    ch: &mut C,
    pkg: &ImfwPackage<'_>,
    progress: &mut dyn FnMut(PushEvent),
) -> Result<(), FwUpdateError> {
    let b64 = base64::engine::general_purpose::STANDARD;

    // 1. INFO — handshake, confirms the OTA channel is alive.
    progress(PushEvent::Stage("info"));
    let resp = ch.send("OTA:INFO").map_err(|e| transfer("info", e))?;
    if !is_ok(&resp) {
        return Err(map_device_error("info", &resp));
    }
    if let Some(payload) = resp.trim().strip_prefix("OK:") {
        progress(PushEvent::DeviceInfo(payload.to_string()));
    }

    // 2. ERASE — wipe Image B (~3-5s, daemon blocks until done).
    progress(PushEvent::Stage("erase"));
    let resp = ch.send("OTA:ERASE").map_err(|e| transfer("erase", e))?;
    if !is_ok(&resp) {
        return Err(map_device_error("erase", &resp));
    }

    // 3. HEADER — v2 rejection / SVN anti-rollback errors surface here.
    progress(PushEvent::Stage("header"));
    let cmd = format!("OTA:HEADER:{}", b64.encode(pkg.header));
    let resp = ch.send(&cmd).map_err(|e| transfer("header", e))?;
    if !is_ok(&resp) {
        if resp.contains("LOW_BATTERY") {
            return Err(FwUpdateError::LowBattery);
        }
        return Err(FwUpdateError::HeaderRejected(resp.trim().to_string()));
    }

    // 4. WRITE — encrypted body in 240B chunks.
    progress(PushEvent::Stage("write"));
    let fw = pkg.firmware;
    let total = fw.len().div_ceil(CHUNK_SIZE);
    for i in 0..total {
        let offset = i * CHUNK_SIZE;
        let end = (offset + CHUNK_SIZE).min(fw.len());
        let cmd = format!("OTA:WRITE:{:04x}:{}", offset, b64.encode(&fw[offset..end]));
        let resp = ch.send(&cmd).map_err(|e| transfer("write", e))?;
        if !is_ok(&resp) {
            return Err(map_device_error("write", &resp));
        }
        progress(PushEvent::Chunk { done: i + 1, total });
    }

    // 5. END — device verifies SHA256 + signature, then reboots.
    progress(PushEvent::Stage("end"));
    let resp = ch.send("OTA:END").map_err(|e| transfer("end", e))?;
    if is_ok(&resp) {
        return Ok(());
    }
    if resp.contains("SHA256") {
        return Err(FwUpdateError::Sha256Mismatch);
    }
    if resp.contains("HMAC") || resp.contains("SIG") {
        return Err(FwUpdateError::SignatureRejected);
    }
    Err(map_device_error("end", &resp))
}

/// One automatic retry (fresh connection, from ERASE) for transfer-class
/// errors; signature-class errors fail immediately (macOS pushWithRetry).
pub fn push_with_retry<C: OtaChannel>(
    mut connect: impl FnMut() -> Result<C, FwUpdateError>,
    pkg: &ImfwPackage<'_>,
    progress: &mut dyn FnMut(PushEvent),
) -> Result<(), FwUpdateError> {
    let mut ch = connect()?;
    match push_once(&mut ch, pkg, progress) {
        Err(e) if is_retryable(&e) => {
            progress(PushEvent::Stage("retry"));
            let mut ch = connect()?;
            push_once(&mut ch, pkg, progress)
        }
        r => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use immurok_common::fwupdate::imfw;

    /// Scripted channel: responds by command prefix, logs every request.
    struct MockChannel {
        /// (request-prefix, response). First match wins, consumed in order
        /// for same-prefix entries? No — static map is enough here.
        responses: Vec<(&'static str, Result<String, String>)>,
        pub log: Vec<String>,
    }

    impl MockChannel {
        fn ok_all() -> Self {
            Self {
                responses: vec![
                    ("OTA:INFO", Ok("OK:00:36000:1000:0592".into())),
                    ("OTA:ERASE", Ok("OK".into())),
                    ("OTA:HEADER", Ok("OK".into())),
                    ("OTA:WRITE", Ok("OK".into())),
                    ("OTA:END", Ok("OK".into())),
                ],
                log: Vec::new(),
            }
        }

        fn with(mut self, prefix: &'static str, resp: Result<String, String>) -> Self {
            self.responses.retain(|(p, _)| *p != prefix);
            self.responses.push((prefix, resp));
            self
        }
    }

    impl OtaChannel for MockChannel {
        fn send(&mut self, request: &str) -> Result<String, String> {
            self.log.push(request.to_string());
            for (prefix, resp) in &self.responses {
                if request.starts_with(prefix) {
                    return resp.clone();
                }
            }
            panic!("unexpected request: {request}");
        }
    }

    /// 500-byte body → 3 chunks (240+240+20).
    fn test_package_data() -> Vec<u8> {
        let mut d = vec![0u8; imfw::HEADER_SIZE_V2 + 500];
        d[0..4].copy_from_slice(&imfw::IMFW_MAGIC.to_le_bytes());
        d[4] = 2;
        d[8..12].copy_from_slice(&500u32.to_le_bytes());
        d
    }

    fn sink() -> impl FnMut(PushEvent) {
        |_| {}
    }

    #[test]
    fn happy_path_sends_full_sequence() {
        let data = test_package_data();
        let pkg = imfw::parse(&data).unwrap();
        let mut ch = MockChannel::ok_all();
        push_once(&mut ch, &pkg, &mut sink()).unwrap();
        assert_eq!(ch.log[0], "OTA:INFO");
        assert_eq!(ch.log[1], "OTA:ERASE");
        assert!(ch.log[2].starts_with("OTA:HEADER:"));
        // 3 chunks: offsets 0000, 00f0, 01e0
        assert!(ch.log[3].starts_with("OTA:WRITE:0000:"));
        assert!(ch.log[4].starts_with("OTA:WRITE:00f0:"));
        assert!(ch.log[5].starts_with("OTA:WRITE:01e0:"));
        assert_eq!(ch.log[6], "OTA:END");
        assert_eq!(ch.log.len(), 7);
    }

    #[test]
    fn chunk_progress_reported() {
        let data = test_package_data();
        let pkg = imfw::parse(&data).unwrap();
        let mut ch = MockChannel::ok_all();
        let mut chunks = Vec::new();
        let mut progress = |ev: PushEvent| {
            if let PushEvent::Chunk { done, total } = ev {
                chunks.push((done, total));
            }
        };
        push_once(&mut ch, &pkg, &mut progress).unwrap();
        assert_eq!(chunks, vec![(1, 3), (2, 3), (3, 3)]);
    }

    #[test]
    fn header_rejected_is_terminal() {
        let data = test_package_data();
        let pkg = imfw::parse(&data).unwrap();
        let mut ch = MockChannel::ok_all().with("OTA:HEADER", Ok("ERROR:HEADER_REJECTED".into()));
        let err = push_once(&mut ch, &pkg, &mut sink()).unwrap_err();
        assert!(matches!(err, FwUpdateError::HeaderRejected(_)));
        assert!(!is_retryable(&err));
    }

    #[test]
    fn end_sha256_and_signature_errors() {
        let data = test_package_data();
        let pkg = imfw::parse(&data).unwrap();
        let mut ch = MockChannel::ok_all().with("OTA:END", Ok("ERROR:SHA256_MISMATCH".into()));
        assert!(matches!(
            push_once(&mut ch, &pkg, &mut sink()).unwrap_err(),
            FwUpdateError::Sha256Mismatch
        ));
        let mut ch = MockChannel::ok_all().with("OTA:END", Ok("ERROR:HMAC_MISMATCH".into()));
        assert!(matches!(
            push_once(&mut ch, &pkg, &mut sink()).unwrap_err(),
            FwUpdateError::SignatureRejected
        ));
    }

    #[test]
    fn low_battery_is_terminal() {
        let data = test_package_data();
        let pkg = imfw::parse(&data).unwrap();
        let mut ch = MockChannel::ok_all().with("OTA:ERASE", Ok("ERROR:LOW_BATTERY".into()));
        let err = push_once(&mut ch, &pkg, &mut sink()).unwrap_err();
        assert!(matches!(err, FwUpdateError::LowBattery));
        assert!(!is_retryable(&err));
    }

    #[test]
    fn write_socket_error_retried_once_then_succeeds() {
        let data = test_package_data();
        let pkg = imfw::parse(&data).unwrap();
        // First connection: WRITE dies (socket error). Second: all OK.
        let mut attempts = 0;
        let connect = || -> Result<MockChannel, FwUpdateError> {
            attempts += 1;
            if attempts == 1 {
                Ok(MockChannel::ok_all().with("OTA:WRITE", Err("Read failed: timeout".into())))
            } else {
                Ok(MockChannel::ok_all())
            }
        };
        push_with_retry(connect, &pkg, &mut sink()).unwrap();
        assert_eq!(attempts, 2);
    }

    #[test]
    fn signature_class_not_retried() {
        let data = test_package_data();
        let pkg = imfw::parse(&data).unwrap();
        let mut attempts = 0;
        let connect = || -> Result<MockChannel, FwUpdateError> {
            attempts += 1;
            Ok(MockChannel::ok_all().with("OTA:END", Ok("ERROR:SHA256_MISMATCH".into())))
        };
        let err = push_with_retry(connect, &pkg, &mut sink()).unwrap_err();
        assert!(matches!(err, FwUpdateError::Sha256Mismatch));
        assert_eq!(attempts, 1);
    }
}
