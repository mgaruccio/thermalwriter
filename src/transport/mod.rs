// SPDX-License-Identifier: GPL-3.0-or-later
//
// Transport layer: USB bulk / SCSI / HID / LY transfer trait and implementations.
// Protocol tables and multi-cooler wire behavior are derived from
// thermalright-trcc-linux at tree 390b880abd4cf0ed2d6eae7151493432263eff39
// (project version 9.8.6, four commits after the v9.8.6 tag).

//! Transport layer: discovery, profile resolution, and frame transfer.

pub mod bulk_usb;
pub mod policy;
#[cfg(feature = "daemon")]
pub use discovery::{LcdTransportRoute, resolve_known_lcd_route};
pub mod discovery;
pub mod encode;
pub mod null;
pub mod profile;
pub mod usb_device;
pub mod usb_fingerprint;

// Family implementations (added as they land).
pub mod hid_lcd;
pub mod hid_report;
pub mod ly_lcd;
pub mod scsi_lcd;
pub mod type2_policy;
pub mod validate_device;
pub mod validation_report;

use anyhow::Result;

pub use hid_report::{
    HidChunkedWriteFailure, HidReadObservation, HidReportAuthorizeError, HidReportBackendContract,
    HidReportProbeError, HidReportReadError, HidReportReadSession, HidReportWriteAuthorization,
    HidReportWriteError, HidReportWriteSession, HidWriteObservation, HidrawCandidate,
    HidrawCorrelation, KERNEL_HIDRAW_DOC_REF, LINUX_HIDRAW_BACKEND_CONTRACT, PROTOCOL_CHUNK_BYTES,
    REPORT_ID_UNNUMBERED, REVIEWED_HIDAPI_EVIDENCE_COMMIT, USERSPACE_SUBMIT_BYTES, UsbBusAddress,
    authenticate_opened_hidraw, correlate_hidraw_to_usb,
};
pub use policy::{
    ExactDescriptorPolicy, ExactDevicePolicy, ProbePolicy, exact_descriptor_policy,
    negotiate_response, select_probe_policy,
};
pub use profile::{
    DeviceInfo, DeviceProfile, DisplayShape, FixtureProfile, FrameEncoding, KNOWN_FBL_CODES,
    WireProtocol, build_device_info, device_info_from_fixture, display_shape, fixture_by_id,
    known_fixture_profiles, oriented_dimensions, pm_to_fbl, resolve_profile, supported_resolutions,
    wire_angle,
};
pub use type2_policy::{
    HidOutputRoute, Type2NegotiatedObservation, Type2NegotiatedPolicy, Type2PreHandshakePolicy,
    UPSTREAM_407_PM58_ISSUE, UPSTREAM_407_PM58_PR, authorize_hid_report_writes,
    negotiate_type2_policy, select_type2_pre_handshake_policy, validate_short_response_type2,
};
#[cfg(feature = "daemon")]
pub use usb_fingerprint::fingerprint_from_device;
pub use usb_fingerprint::{
    DerivedBulkPair, HidInterruptIn, UsbDirection, UsbEndpointCapability, UsbFingerprint,
    UsbInterfaceShape, UsbRunIdentity, UsbTransferKind, derive_bulk_pair, derive_vendor_bulk_pair,
    format_bcd_device, hid_interrupt_in_endpoints, unsupported_known_shape_message,
};
#[doc(hidden)]
pub use validate_device::test_support;
pub use validate_device::{ValidateDeviceArgs, run_validate_device};
pub use validation_report::{
    BuildProvenance, CheckField, CheckStatus, DescriptorCaptureStatus, DisplayDimensions,
    EvidenceOrigin, FinalizeError, HardwareValidationReport, HidBackendProvenance,
    HidReadErrorKind, HidReadEvidence, HidReadFailureEvidence, HidReportEvidence,
    HidWriteChunkEvidence, HidWriteErrorKind, HidWriteFailureEvidence, NegotiatedOutputRoute,
    NegotiatedProfile, ProfilePolicyLabel, ProtocolFamily, ReportEndpointCapability,
    ReportFingerprint, ReportInterfaceShape, ReportMutationError, ReportPreHandshakePolicy,
    RuntimeBackendRoute, SCHEMA_VERSION, SanitizedText, UPSTREAM_REVIEWED_COMMIT, ValidationChecks,
    ValidationErrorKind, ValidationErrorLink, ValidationFailure, ValidationResult, ValidationScope,
    ValidationStage, build_commit_known, current_build_provenance, sanitize_free_text,
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
