//! Protocol constants — mirrors firmware + docs/protocol.md

// GATT UUIDs (string form for bluer). Updated to firmware 1.2.28+ random
// v4 UUIDs (commit 556e137 — production BLE asset compliance). The old
// 12340010-…-00805f9b34fb pattern piggybacked on the SIG Bluetooth Base
// UUID, and 0xFEE0/FEE1 collided with another SIG member range.
pub const SERVICE_UUID_STR: &str = "45529919-7668-48f9-b9fe-e4eabe6595d9";
pub const CMD_CHAR_UUID_STR: &str = "8a537e1f-3992-4b2c-8b77-8d4e778186e1";
pub const RSP_CHAR_UUID_STR: &str = "76a1660d-8cf6-44d1-b3fc-70486028e289";
pub const OTA_SERVICE_UUID_STR: &str = "d29005de-1391-4a54-8168-bf4e3c080430";
pub const OTA_CHAR_UUID_STR: &str = "c75f4c30-9a2d-4445-92e0-0e034c53d092";
pub const FW_REVISION_UUID: u16 = 0x2A26;

pub const DEVICE_NAME_PREFIX: &str = "immurok";

// Command codes (App → Device)
pub const CMD_GET_STATUS: u8 = 0x01;
pub const CMD_ENROLL_START: u8 = 0x10;
pub const CMD_ENROLL_CANCEL: u8 = 0x11;
pub const CMD_DELETE_FP: u8 = 0x12;
pub const CMD_FP_LIST: u8 = 0x13;
pub const CMD_FP_MATCH_ACK: u8 = 0x22;
pub const CMD_PAIR_INIT: u8 = 0x30;
pub const CMD_PAIR_CONFIRM: u8 = 0x31;
pub const CMD_PAIR_STATUS: u8 = 0x32;
pub const CMD_AUTH_REQUEST: u8 = 0x33;
pub const CMD_PAIR_BUTTON: u8 = 0x34;
pub const CMD_FACTORY_RESET: u8 = 0x36;
pub const CMD_GATE_CANCEL: u8 = 0x37;
pub const CMD_CHALLENGE: u8 = 0x38;
pub const CMD_KEY_COUNT: u8 = 0x60;
pub const CMD_KEY_READ: u8 = 0x61;
pub const CMD_KEY_WRITE: u8 = 0x62;
pub const CMD_KEY_DELETE: u8 = 0x63;
pub const CMD_KEY_COMMIT: u8 = 0x64;
pub const CMD_KEY_SIGN: u8 = 0x65;
pub const CMD_KEY_GETPUB: u8 = 0x66;
pub const CMD_KEY_GENERATE: u8 = 0x67;
pub const CMD_KEY_RESULT: u8 = 0x68;
pub const CMD_KEY_OTP_GET: u8 = 0x69;

// Response codes
pub const RSP_OK: u8 = 0x00;
pub const RSP_ERR_TIMEOUT: u8 = 0x06;
pub const RSP_ERR_FP_NOT_MATCH: u8 = 0x07;
pub const RSP_FP_GATE_APPROVED: u8 = 0x10;
pub const RSP_WAIT_FP: u8 = 0x11;
pub const RSP_BUSY: u8 = 0xFD;
pub const RSP_INVALID_PARAM: u8 = 0xFE;
pub const RSP_ERROR: u8 = 0xFF;

// PAIR_INIT-specific status codes (second byte of [0x30, status])
pub const RSP_PAIR_WAIT_BUTTON: u8 = 0xF0;
pub const RSP_PAIR_NEEDS_RESET: u8 = 0xF1;

// Firmware 1.3.1+ refuses long-write commands (KEY_WRITE / KEY_COMMIT /
// KEY_DELETE / KEY_GENERATE / OTA) when battery < 5% — half-erased EEPROM
// from a brown-out would corrupt the keystore or leave OTA Image B in a
// state that JumpIAP can't reason about. KEY_SIGN / AUTH_REQUEST /
// KEY_OTP_GET / GET_STATUS are NOT gated; auth still works on a dying
// battery. Mirrors firmware commit fe984db.
pub const RSP_ERR_LOW_BATTERY: u8 = 0xF4;

// CMD_PAIR_BUTTON notification statuses (second byte of [0x34, status])
pub const PAIR_BUTTON_TIMEOUT: u8 = 0x00;
pub const PAIR_BUTTON_CONFIRMED: u8 = 0x01;
pub const PAIR_BUTTON_CANCELLED: u8 = 0x02;

// Notification types (Device → App)
pub const NOTIFY_FP_MATCH_SIGNED: u8 = 0x21;
pub const NOTIFY_LOCK_REQUEST: u8 = 0x23;
pub const NOTIFY_ENROLL_PROGRESS: u8 = 0x11;
// Firmware 1.2.31+ emits this every ~3s during enroll step 1 polling to
// keep the BLE link active (commit 25a2f19). 4-byte payload
// `[0x12, 0x01, fp_powered, capture]`. App should silently ignore.
pub const NOTIFY_ENROLL_KEEPALIVE: u8 = 0x12;
pub const NOTIFY_CONN_PARAM_UPDATE: u8 = 0xF0;

// Key categories
pub const KEY_CAT_SSH: u8 = 0;
pub const KEY_CAT_OTP: u8 = 1;
pub const KEY_CAT_API: u8 = 2;

// Per-category capacity (must mirror firmware/APP/immurok_keystore.h —
// KEYSTORE_SSH_MAX / OTP_MAX / API_MAX). Used by the CLI to refuse import
// before round-tripping to the device.
pub const KEY_MAX_SSH: u8 = 32;
pub const KEY_MAX_OTP: u8 = 128;
pub const KEY_MAX_API: u8 = 50;

// Per-category entry name field length (firmware structs):
//   ssh_entry_t.name[16]
//   otp_entry_t.name[30]   ← Rust used to read 16 here, truncating long names
//   api_entry_t.name[32]   ← same bug
pub const NAME_LEN_SSH: usize = 16;
pub const NAME_LEN_OTP: usize = 30;
pub const NAME_LEN_API: usize = 32;

// Enroll status values
pub const ENROLL_WAITING: u8 = 0x00;
pub const ENROLL_CAPTURED: u8 = 0x01;
pub const ENROLL_PROCESSING: u8 = 0x02;
pub const ENROLL_LIFT_FINGER: u8 = 0x03;
pub const ENROLL_COMPLETE: u8 = 0x04;
pub const ENROLL_FAILED: u8 = 0xFF;

// Packet format
pub const MAX_PACKET_SIZE: usize = 64;
pub const MAX_PAYLOAD_SIZE: usize = 62;

// Timing
pub const BLE_COMMAND_TIMEOUT_SECS: u64 = 5;
pub const BLE_RECONNECT_INTERVAL_SECS: u64 = 1;
pub const BLE_CONNECTING_TIMEOUT_SECS: u64 = 10;
pub const BLE_FP_GATE_TIMEOUT_SECS: u64 = 30;
pub const BLE_AUTH_TIMEOUT_SECS: u64 = 30;
// 30s 等按钮 + 2×2s ECC + 余量
pub const BLE_PAIR_BUTTON_TIMEOUT_SECS: u64 = 35;
// Pre-auth window (after FP match → screen unlock or PAM approve) for
// follow-up PAM requests like polkit prompts. Bound to a service set; an
// out-of-set request still requires a fresh fingerprint. Tightened from
// the original 10s to align with macOS 1.2.6 hardening (commit 2f26dbf):
// PAM follow-ups typically arrive within ~1s of the unlock.
pub const PRE_AUTH_DURATION_SECS: u64 = 3;
// Suppress 0x23 long-press lock if it arrives in the tail of an actual auth
// flow (PAM approve or screen unlock). Firmware sends 0x21 (FP match) and
// then 0x23 1.6s later regardless of match outcome, so a successful auth
// where the user lingers on the pad would otherwise immediately re-lock.
pub const LOCK_SUPPRESS_WINDOW_SECS: u64 = 3;
pub const FP_GATE_MAX_FAILURES: u8 = 3;
pub const MAX_FINGERPRINT_SLOTS: u8 = 5;

// Security
pub const COMPRESSED_PUBKEY_LEN: usize = 33;
pub const SHARED_KEY_LEN: usize = 32;
pub const HMAC_TRUNCATED_LEN: usize = 8;
pub const HKDF_SALT: &[u8] = b"immurok-pairing-salt";
pub const HKDF_INFO: &[u8] = b"immurok-shared-key";

// OTA
pub const OTA_IMAGE_B_BLOCKS: u32 = 54;
pub const OTA_READ_POLL_INTERVAL_MS: u64 = 200;
pub const OTA_ERASE_TIMEOUT_SECS: u64 = 15;

// Paths
pub const IMMUROK_DIR: &str = ".immurok";
pub const PAIRING_FILE: &str = "pairing.json";
pub const SETTINGS_FILE: &str = "settings.json";
pub const SSH_KEYS_FILE: &str = "ssh_keys.json";
pub const KEY_NAMES_FILE: &str = "key_names.json";
// Per-category (count, checksum) digests for sync_ssh_keys to short-circuit
// full BLE re-reads. Internal to daemon — CLI/SSH-agent never read this.
// Format: { "ssh": {"count":N,"checksum":U32}, "otp": {...}, "api": {...} }
pub const KEYSTORE_DIGESTS_FILE: &str = "keystore_digests.json";
pub const PAM_SOCKET_NAME: &str = "pam.sock";
pub const AGENT_SOCKET_NAME: &str = "agent.sock";
