//! Firmware update pure logic — ported from macOS FirmwareUpdateKit.
//!
//! Behavioral reference: app-macos/FirmwareUpdateKit/*.swift and its unit
//! tests in app-macos/Tests/FirmwareUpdateKitTests/.

pub mod version;
pub mod planner;
pub mod manifest;
pub mod imfw;
