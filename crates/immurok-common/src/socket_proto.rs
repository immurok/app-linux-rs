//! Unix Socket text protocol — parser and serializer.
//!
//! Wire format uses colon-separated text lines (no trailing newline in the
//! serialized string itself — callers append `\n` when sending over the socket).
//!
//! # Request wire format
//!
//! | Command             | Wire line                          |
//! |---------------------|------------------------------------|
//! | AUTH                | `AUTH:username:service`            |
//! | STATUS              | `STATUS`                           |
//! | FP:LIST             | `FP:LIST`                          |
//! | FP:ENROLL           | `FP:ENROLL:slot`                   |
//! | FP:ENROLL_CANCEL    | `FP:ENROLL_CANCEL`                 |
//! | FP:DELETE           | `FP:DELETE:slot`                   |
//! | FP:VERIFY           | `FP:VERIFY`                        |
//! | FP:STATUS           | `FP:STATUS`                        |
//! | FP:LAST_MATCH       | `FP:LAST_MATCH`                    |
//! | GATE:CANCEL         | `GATE:CANCEL`                      |
//! | PAIR:STATUS         | `PAIR:STATUS`                      |
//! | PAIR:START          | `PAIR:START`                       |
//! | PAIR:RESET          | `PAIR:RESET`                       |
//! | SET:UNLOCK_SUDO     | `SET:UNLOCK_SUDO:1` / `:0`         |
//! | SET:UNLOCK_POLKIT   | `SET:UNLOCK_POLKIT:1` / `:0`       |
//! | SET:UNLOCK_SCREEN   | `SET:UNLOCK_SCREEN:1` / `:0`       |
//! | SET:LOCK_SCREEN     | `SET:LOCK_SCREEN:1` / `:0`         |
//! | GET:SETTINGS        | `GET:SETTINGS`                     |
//! | GET:INFO            | `GET:INFO`                         |
//! | OTA:START           | `OTA:START:size:version`           |
//! | OTA:DATA            | `OTA:DATA:hex_bytes`               |
//! | OTA:FINISH          | `OTA:FINISH`                       |
//!
//! # Response wire format
//!
//! | Variant     | Wire line                                         |
//! |-------------|---------------------------------------------------|
//! | Ok          | `OK:message`                                      |
//! | Deny        | `DENY:message`                                    |
//! | Retry       | `RETRY:remaining`                                 |
//! | Error       | `ERROR:message`                                   |
//! | Status      | `STATUS:connected(0/1):name:battery:version`      |

use std::fmt;

// ── Error type ────────────────────────────────────────────────

/// Errors that can occur while parsing a request line.
#[derive(Debug, PartialEq)]
pub enum ParseError {
    /// The input line is empty.
    Empty,
    /// The command token is not recognised.
    UnknownCommand(String),
    /// A required field is missing.
    MissingField { command: &'static str, field: &'static str },
    /// A field could not be parsed as the expected type.
    InvalidField { command: &'static str, field: &'static str, detail: String },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty input"),
            Self::UnknownCommand(cmd) => write!(f, "unknown command: {cmd}"),
            Self::MissingField { command, field } => {
                write!(f, "{command}: missing field '{field}'")
            }
            Self::InvalidField { command, field, detail } => {
                write!(f, "{command}: invalid field '{field}': {detail}")
            }
        }
    }
}

impl std::error::Error for ParseError {}

// ── Request ───────────────────────────────────────────────────

/// All commands the client can send to the daemon.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Auth { user: String, service: String },
    Status,
    FpList,
    FpEnroll { slot: u8 },
    FpEnrollCancel,
    FpDelete { slot: u8 },
    FpVerify,
    FpStatus,
    FpLastMatch,
    GateCancel,
    PairStatus,
    PairStart,
    PairReset,
    SetUnlockSudo(bool),
    SetUnlockPolkit(bool),
    SetUnlockScreen(bool),
    SetLockScreen(bool),
    GetSettings,
    GetInfo,
    OtaStart { size: u32, version: String },
    OtaData(Vec<u8>),
    OtaFinish,
}

// ── Response ──────────────────────────────────────────────────

/// All responses the daemon can send back to the client.
#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    Ok(String),
    Deny(String),
    Retry { remaining: u8 },
    Error(String),
    Status {
        connected: bool,
        name: String,
        battery: u8,
        version: String,
    },
}

// ── Parsing helpers ───────────────────────────────────────────

/// Require that `parts[idx]` exists; return it or a `MissingField` error.
fn require<'a>(
    parts: &[&'a str],
    idx: usize,
    command: &'static str,
    field: &'static str,
) -> Result<&'a str, ParseError> {
    parts
        .get(idx)
        .copied()
        .ok_or(ParseError::MissingField { command, field })
}

/// Parse `parts[idx]` as `u8`; propagate `MissingField` or `InvalidField`.
fn parse_u8(
    parts: &[&str],
    idx: usize,
    command: &'static str,
    field: &'static str,
) -> Result<u8, ParseError> {
    let s = require(parts, idx, command, field)?;
    s.parse::<u8>().map_err(|e| ParseError::InvalidField {
        command,
        field,
        detail: e.to_string(),
    })
}

/// Parse `parts[idx]` as `u32`.
fn parse_u32(
    parts: &[&str],
    idx: usize,
    command: &'static str,
    field: &'static str,
) -> Result<u32, ParseError> {
    let s = require(parts, idx, command, field)?;
    s.parse::<u32>().map_err(|e| ParseError::InvalidField {
        command,
        field,
        detail: e.to_string(),
    })
}

/// Parse `parts[idx]` as a boolean `"1"` / `"0"`.
fn parse_bool(
    parts: &[&str],
    idx: usize,
    command: &'static str,
    field: &'static str,
) -> Result<bool, ParseError> {
    let s = require(parts, idx, command, field)?;
    match s {
        "1" => Ok(true),
        "0" => Ok(false),
        other => Err(ParseError::InvalidField {
            command,
            field,
            detail: format!("expected '1' or '0', got '{other}'"),
        }),
    }
}

// ── parse_request ─────────────────────────────────────────────

/// Parse a single text line (without the trailing newline) into a [`Request`].
pub fn parse_request(line: &str) -> Result<Request, ParseError> {
    let line = line.trim();
    if line.is_empty() {
        return Err(ParseError::Empty);
    }

    // Split on ':' but keep the rest of the line intact for fields that may
    // contain colons (e.g. OTA version strings like "v1.2.3", OTA data hex).
    // We collect into a Vec so we can index freely.
    let parts: Vec<&str> = line.splitn(10, ':').collect();
    let cmd = parts[0];

    match cmd {
        "AUTH" => {
            let user = require(&parts, 1, "AUTH", "user")?.to_string();
            let service = require(&parts, 2, "AUTH", "service")?.to_string();
            Ok(Request::Auth { user, service })
        }

        "STATUS" => Ok(Request::Status),

        "FP" => {
            let sub = require(&parts, 1, "FP", "subcommand")?;
            match sub {
                "LIST" => Ok(Request::FpList),
                "ENROLL" => {
                    let slot = parse_u8(&parts, 2, "FP:ENROLL", "slot")?;
                    Ok(Request::FpEnroll { slot })
                }
                "ENROLL_CANCEL" => Ok(Request::FpEnrollCancel),
                "DELETE" => {
                    let slot = parse_u8(&parts, 2, "FP:DELETE", "slot")?;
                    Ok(Request::FpDelete { slot })
                }
                "VERIFY" => Ok(Request::FpVerify),
                "STATUS" => Ok(Request::FpStatus),
                "LAST_MATCH" => Ok(Request::FpLastMatch),
                other => Err(ParseError::UnknownCommand(format!("FP:{other}"))),
            }
        }

        "GATE" => {
            let sub = require(&parts, 1, "GATE", "subcommand")?;
            match sub {
                "CANCEL" => Ok(Request::GateCancel),
                other => Err(ParseError::UnknownCommand(format!("GATE:{other}"))),
            }
        }

        "PAIR" => {
            let sub = require(&parts, 1, "PAIR", "subcommand")?;
            match sub {
                "STATUS" => Ok(Request::PairStatus),
                "START" => Ok(Request::PairStart),
                "RESET" => Ok(Request::PairReset),
                other => Err(ParseError::UnknownCommand(format!("PAIR:{other}"))),
            }
        }

        "SET" => {
            let sub = require(&parts, 1, "SET", "subcommand")?;
            match sub {
                "UNLOCK_SUDO" => {
                    let v = parse_bool(&parts, 2, "SET:UNLOCK_SUDO", "value")?;
                    Ok(Request::SetUnlockSudo(v))
                }
                "UNLOCK_POLKIT" => {
                    let v = parse_bool(&parts, 2, "SET:UNLOCK_POLKIT", "value")?;
                    Ok(Request::SetUnlockPolkit(v))
                }
                "UNLOCK_SCREEN" => {
                    let v = parse_bool(&parts, 2, "SET:UNLOCK_SCREEN", "value")?;
                    Ok(Request::SetUnlockScreen(v))
                }
                "LOCK_SCREEN" => {
                    let v = parse_bool(&parts, 2, "SET:LOCK_SCREEN", "value")?;
                    Ok(Request::SetLockScreen(v))
                }
                other => Err(ParseError::UnknownCommand(format!("SET:{other}"))),
            }
        }

        "GET" => {
            let sub = require(&parts, 1, "GET", "subcommand")?;
            match sub {
                "SETTINGS" => Ok(Request::GetSettings),
                "INFO" => Ok(Request::GetInfo),
                other => Err(ParseError::UnknownCommand(format!("GET:{other}"))),
            }
        }

        "OTA" => {
            let sub = require(&parts, 1, "OTA", "subcommand")?;
            match sub {
                "START" => {
                    let size = parse_u32(&parts, 2, "OTA:START", "size")?;
                    let version = require(&parts, 3, "OTA:START", "version")?.to_string();
                    Ok(Request::OtaStart { size, version })
                }
                "DATA" => {
                    let hex = require(&parts, 2, "OTA:DATA", "hex_bytes")?;
                    let data = hex::decode(hex).map_err(|e| ParseError::InvalidField {
                        command: "OTA:DATA",
                        field: "hex_bytes",
                        detail: e.to_string(),
                    })?;
                    Ok(Request::OtaData(data))
                }
                "FINISH" => Ok(Request::OtaFinish),
                other => Err(ParseError::UnknownCommand(format!("OTA:{other}"))),
            }
        }

        other => Err(ParseError::UnknownCommand(other.to_string())),
    }
}

// ── serialize_response ────────────────────────────────────────

/// Serialize a [`Response`] into a single text line (no trailing newline).
pub fn serialize_response(resp: &Response) -> String {
    match resp {
        Response::Ok(msg) => format!("OK:{msg}"),
        Response::Deny(msg) => format!("DENY:{msg}"),
        Response::Retry { remaining } => format!("RETRY:{remaining}"),
        Response::Error(msg) => format!("ERROR:{msg}"),
        Response::Status {
            connected,
            name,
            battery,
            version,
        } => {
            let conn_flag = if *connected { 1u8 } else { 0u8 };
            format!("STATUS:{conn_flag}:{name}:{battery}:{version}")
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Request parsing ───────────────────────────────────────

    #[test]
    fn test_parse_auth() {
        let req = parse_request("AUTH:alice:sudo").expect("parse failed");
        match req {
            Request::Auth { user, service } => {
                assert_eq!(user, "alice");
                assert_eq!(service, "sudo");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn test_parse_status() {
        let req = parse_request("STATUS").expect("parse failed");
        assert_eq!(req, Request::Status);
    }

    #[test]
    fn test_parse_fp_enroll() {
        let req = parse_request("FP:ENROLL:2").expect("parse failed");
        match req {
            Request::FpEnroll { slot } => assert_eq!(slot, 2),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn test_parse_fp_enroll_cancel() {
        let req = parse_request("FP:ENROLL_CANCEL").expect("parse failed");
        assert_eq!(req, Request::FpEnrollCancel);
    }

    #[test]
    fn test_parse_set_toggle() {
        let req_true = parse_request("SET:UNLOCK_SUDO:1").expect("parse failed");
        assert_eq!(req_true, Request::SetUnlockSudo(true));

        let req_false = parse_request("SET:UNLOCK_SUDO:0").expect("parse failed");
        assert_eq!(req_false, Request::SetUnlockSudo(false));
    }

    #[test]
    fn test_parse_gate_cancel() {
        let req = parse_request("GATE:CANCEL").expect("parse failed");
        assert_eq!(req, Request::GateCancel);
    }

    #[test]
    fn test_parse_ota_start() {
        let req = parse_request("OTA:START:1024:v1.2.3").expect("parse failed");
        match req {
            Request::OtaStart { size, version } => {
                assert_eq!(size, 1024);
                assert_eq!(version, "v1.2.3");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    // ── Response serialization ────────────────────────────────

    #[test]
    fn test_serialize_ok() {
        let s = serialize_response(&Response::Ok("done".to_string()));
        assert_eq!(s, "OK:done");
    }

    #[test]
    fn test_serialize_retry() {
        let s = serialize_response(&Response::Retry { remaining: 2 });
        assert_eq!(s, "RETRY:2");
    }

    #[test]
    fn test_serialize_status() {
        let resp = Response::Status {
            connected: true,
            name: "immurok-AB12".to_string(),
            battery: 85,
            version: "v1.0.0".to_string(),
        };
        let s = serialize_response(&resp);
        assert_eq!(s, "STATUS:1:immurok-AB12:85:v1.0.0");

        // disconnected variant
        let resp_off = Response::Status {
            connected: false,
            name: String::new(),
            battery: 0,
            version: String::new(),
        };
        let s_off = serialize_response(&resp_off);
        assert_eq!(s_off, "STATUS:0::0:");
    }

    #[test]
    fn test_parse_invalid() {
        let err = parse_request("GARBAGE").expect_err("should have failed");
        assert!(
            matches!(err, ParseError::UnknownCommand(_)),
            "expected UnknownCommand, got {err:?}"
        );
    }

    // ── Additional coverage ───────────────────────────────────

    #[test]
    fn test_parse_fp_list() {
        assert_eq!(parse_request("FP:LIST").unwrap(), Request::FpList);
    }

    #[test]
    fn test_parse_fp_delete() {
        let req = parse_request("FP:DELETE:3").unwrap();
        assert_eq!(req, Request::FpDelete { slot: 3 });
    }

    #[test]
    fn test_parse_fp_verify() {
        assert_eq!(parse_request("FP:VERIFY").unwrap(), Request::FpVerify);
    }

    #[test]
    fn test_parse_fp_status() {
        assert_eq!(parse_request("FP:STATUS").unwrap(), Request::FpStatus);
    }

    #[test]
    fn test_parse_fp_last_match() {
        assert_eq!(parse_request("FP:LAST_MATCH").unwrap(), Request::FpLastMatch);
    }

    #[test]
    fn test_parse_pair_commands() {
        assert_eq!(parse_request("PAIR:STATUS").unwrap(), Request::PairStatus);
        assert_eq!(parse_request("PAIR:START").unwrap(), Request::PairStart);
        assert_eq!(parse_request("PAIR:RESET").unwrap(), Request::PairReset);
    }

    #[test]
    fn test_parse_set_polkit_and_screen() {
        assert_eq!(
            parse_request("SET:UNLOCK_POLKIT:1").unwrap(),
            Request::SetUnlockPolkit(true)
        );
        assert_eq!(
            parse_request("SET:UNLOCK_SCREEN:0").unwrap(),
            Request::SetUnlockScreen(false)
        );
    }

    #[test]
    fn test_parse_get_commands() {
        assert_eq!(parse_request("GET:SETTINGS").unwrap(), Request::GetSettings);
        assert_eq!(parse_request("GET:INFO").unwrap(), Request::GetInfo);
    }

    #[test]
    fn test_parse_ota_data() {
        let req = parse_request("OTA:DATA:deadbeef").unwrap();
        assert_eq!(req, Request::OtaData(vec![0xde, 0xad, 0xbe, 0xef]));
    }

    #[test]
    fn test_parse_ota_finish() {
        assert_eq!(parse_request("OTA:FINISH").unwrap(), Request::OtaFinish);
    }

    #[test]
    fn test_serialize_deny_and_error() {
        assert_eq!(serialize_response(&Response::Deny("no".into())), "DENY:no");
        assert_eq!(
            serialize_response(&Response::Error("oops".into())),
            "ERROR:oops"
        );
    }

    #[test]
    fn test_parse_empty() {
        assert_eq!(parse_request(""), Err(ParseError::Empty));
        assert_eq!(parse_request("   "), Err(ParseError::Empty));
    }

    #[test]
    fn test_parse_auth_with_whitespace_trim() {
        // Leading/trailing whitespace (e.g. Windows-style \r\n lines) must be handled.
        let req = parse_request("  AUTH:bob:polkit  ").expect("parse failed");
        assert_eq!(req, Request::Auth { user: "bob".into(), service: "polkit".into() });
    }
}
