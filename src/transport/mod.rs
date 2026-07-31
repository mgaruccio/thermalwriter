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
pub mod usb_device;
pub mod usb_fingerprint;

// Family implementations (added as they land).
pub mod hid_lcd;
pub mod ly_lcd;
pub mod scsi_lcd;
pub mod type2_policy;

use anyhow::Result;

pub use profile::{
    DeviceInfo, DeviceProfile, DisplayShape, FixtureProfile, FrameEncoding, KNOWN_FBL_CODES,
    WireProtocol, build_device_info, device_info_from_fixture, display_shape, fixture_by_id,
    known_fixture_profiles, oriented_dimensions, pm_to_fbl, resolve_profile, supported_resolutions,
    wire_angle,
};
pub use type2_policy::{
    HidOutputRoute, Type2NegotiatedObservation, Type2NegotiatedPolicy, Type2PreHandshakePolicy,
    UPSTREAM_407_PM58_ISSUE, UPSTREAM_407_PM58_PR, negotiate_type2_policy,
    select_type2_pre_handshake_policy, validate_short_response_type2,
};
#[cfg(feature = "daemon")]
pub use usb_fingerprint::fingerprint_from_device;
pub use usb_fingerprint::{
    DerivedBulkPair, HidInterruptIn, UsbDirection, UsbEndpointCapability, UsbFingerprint,
    UsbInterfaceShape, UsbRunIdentity, UsbTransferKind, derive_bulk_pair, derive_vendor_bulk_pair,
    format_bcd_device, hid_interrupt_in_endpoints, unsupported_known_shape_message,
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
