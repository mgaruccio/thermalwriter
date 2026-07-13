// SPDX-License-Identifier: GPL-3.0-or-later
//
// Transport layer: USB bulk / SCSI / HID / LY transfer trait and implementations.
// Protocol tables and multi-cooler wire behavior are derived from
// thermalright-trcc-linux at tree 390b880abd4cf0ed2d6eae7151493432263eff39
// (project version 9.8.6, four commits after the v9.8.6 tag).

//! Transport layer: discovery, profile resolution, and frame transfer.

pub mod bulk_usb;
pub mod discovery;
pub mod encode;
pub mod null;
pub mod profile;

// Family implementations (added as they land).
pub mod hid_lcd;
pub mod ly_lcd;
pub mod scsi_lcd;

use anyhow::Result;

pub use profile::{
    DeviceInfo, DeviceProfile, DisplayShape, FixtureProfile, FrameEncoding, KNOWN_FBL_CODES,
    WireProtocol, build_device_info, device_info_from_fixture, display_shape, fixture_by_id,
    known_fixture_profiles, oriented_dimensions, pm_to_fbl, resolve_profile, supported_resolutions,
    wire_angle,
};

/// Encoded payload ready for the wire — dimensions match the device native
/// canvas after any wire-angle rotation.
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub encoding: FrameEncoding,
}

pub trait Transport: Send {
    /// Perform device handshake and return negotiated device info.
    fn handshake(&mut self) -> Result<DeviceInfo>;
    /// Send one encoded frame.
    fn send_frame(&mut self, frame: &EncodedFrame) -> Result<()>;
    /// Release the device handle / file descriptor.
    fn close(&mut self);
    /// Whether the underlying device handle is currently usable.
    fn is_connected(&self) -> bool {
        true
    }
}
