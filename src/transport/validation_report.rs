// SPDX-License-Identifier: GPL-3.0-or-later
//
// Sanitized hardware-validation report schema for incremental CLI/cleanup workflows.

use std::fmt;

use anyhow::{Context, Result, bail, ensure};
use serde::de::Error as DeError;
use serde::{Deserialize, Serialize};

use super::discovery::{LcdTransportRoute, protocol_for_id, resolve_known_lcd_route};
use super::hid_report::{
    EXPECTED_TRANSPORT_RETURN_BYTES, HidChunkedWriteFailure, HidReadObservation,
    HidReportBackendContract, HidReportWriteError, KERNEL_HIDRAW_DOC_REF,
    LINUX_HIDRAW_BACKEND_CONTRACT, PROTOCOL_CHUNK_BYTES, REPORT_ID_UNNUMBERED,
    REVIEWED_HIDAPI_EVIDENCE_COMMIT, USERSPACE_SUBMIT_BYTES,
};
use super::policy::{ExactDevicePolicy, PM58_RESPONSE, PM128_RESPONSE};
use super::profile::{DeviceInfo, WireProtocol, build_device_info};
use super::type2_policy::{
    BCD_DEVICE_407, HidOutputRoute, TYPE2_LEGACY_RESPONSE_MIN, Type2NegotiatedObservation,
    Type2NegotiatedPolicy, Type2PreHandshakePolicy, WINBOND_HID2_PID, WINBOND_HID2_VID,
    select_type2_pre_handshake_policy,
};
use super::usb_fingerprint::{
    UsbDirection, UsbEndpointCapability, UsbFingerprint, UsbInterfaceShape, UsbTransferKind,
};

/// Current report schema revision.
pub const SCHEMA_VERSION: u32 = 1;

/// Upstream TRCC commit reviewed for hardware-coverage evidence.
pub const UPSTREAM_REVIEWED_COMMIT: &str = "655a1acff5c86ff0f9121f9fd4a0ea14bee35447";

const REDACTED: &str = "[redacted]";

/// How validation evidence was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOrigin {
    Physical,
    Synthetic,
    Replay,
}

/// Validation depth requested for the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationScope {
    Passive,
    Full,
}

/// Incremental validation outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationResult {
    Pass,
    Fail,
    Aborted,
}

/// Stage that failed or aborted the run (empty when still in progress or passed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStage {
    Inventory,
    Selection,
    HidrawCorrelation,
    PassiveAllowlist,
    ExclusiveOwner,
    Handshake,
    Negotiation,
    ActiveWrite,
    TargetMarker,
    SecondDisplayUnchanged,
    Orientation,
    Colors,
    Soak,
    Reconnect,
    DaemonRestored,
}

/// Tri-state check result. Absent checks are never interpreted as pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Fail,
    NotApplicable,
}

/// Named validation checks written incrementally by the CLI workflow.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationChecks {
    #[serde(skip_serializing_if = "Option::is_none")]
    enumerated: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    passive_allowlist: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exclusive_owner: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    handshake: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_marker: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    second_display_unchanged: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    orientation: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    colors: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    soak: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reconnect: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon_restored: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_write: Option<CheckStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckField {
    Enumerated,
    PassiveAllowlist,
    ExclusiveOwner,
    Handshake,
    ActiveWrite,
    TargetMarker,
    SecondDisplayUnchanged,
    Orientation,
    Colors,
    Soak,
    Reconnect,
    DaemonRestored,
}

impl ValidationChecks {
    pub fn passed(&self, field: CheckField) -> bool {
        matches!(self.get(field), Some(CheckStatus::Pass))
    }

    pub fn get(&self, field: CheckField) -> Option<CheckStatus> {
        match field {
            CheckField::Enumerated => self.enumerated,
            CheckField::PassiveAllowlist => self.passive_allowlist,
            CheckField::ExclusiveOwner => self.exclusive_owner,
            CheckField::Handshake => self.handshake,
            CheckField::ActiveWrite => self.active_write,
            CheckField::TargetMarker => self.target_marker,
            CheckField::SecondDisplayUnchanged => self.second_display_unchanged,
            CheckField::Orientation => self.orientation,
            CheckField::Colors => self.colors,
            CheckField::Soak => self.soak,
            CheckField::Reconnect => self.reconnect,
            CheckField::DaemonRestored => self.daemon_restored,
        }
    }

    fn set(&mut self, field: CheckField, status: CheckStatus) {
        match field {
            CheckField::Enumerated => self.enumerated = Some(status),
            CheckField::PassiveAllowlist => self.passive_allowlist = Some(status),
            CheckField::ExclusiveOwner => self.exclusive_owner = Some(status),
            CheckField::Handshake => self.handshake = Some(status),
            CheckField::ActiveWrite => self.active_write = Some(status),
            CheckField::TargetMarker => self.target_marker = Some(status),
            CheckField::SecondDisplayUnchanged => self.second_display_unchanged = Some(status),
            CheckField::Orientation => self.orientation = Some(status),
            CheckField::Colors => self.colors = Some(status),
            CheckField::Soak => self.soak = Some(status),
            CheckField::Reconnect => self.reconnect = Some(status),
            CheckField::DaemonRestored => self.daemon_restored = Some(status),
        }
    }

    fn mandatory_full_fields() -> &'static [CheckField] {
        &[
            CheckField::Enumerated,
            CheckField::PassiveAllowlist,
            CheckField::ExclusiveOwner,
            CheckField::Handshake,
            CheckField::ActiveWrite,
            CheckField::TargetMarker,
            CheckField::SecondDisplayUnchanged,
            CheckField::Orientation,
            CheckField::Colors,
            CheckField::Soak,
            CheckField::Reconnect,
            CheckField::DaemonRestored,
        ]
    }

    fn mandatory_passive_fields() -> &'static [CheckField] {
        &[CheckField::Enumerated, CheckField::PassiveAllowlist]
    }

    fn all_mandatory_pass(&self, fields: &[CheckField]) -> bool {
        fields
            .iter()
            .all(|field| matches!(self.get(*field), Some(CheckStatus::Pass)))
    }
}

/// Shareable USB fingerprint section (no bus/address/serial value).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportFingerprint {
    #[serde(with = "hex_u16")]
    vid: u16,
    #[serde(with = "hex_u16")]
    pid: u16,
    bcd_device: String,
    serial_present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    hidraw_correlated: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    interfaces: Vec<ReportInterfaceShape>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportInterfaceShape {
    number: u8,
    alternate_setting: u8,
    class: u8,
    subclass: u8,
    protocol: u8,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    endpoints: Vec<ReportEndpointCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportEndpointCapability {
    #[serde(with = "hex_u8")]
    address: u8,
    direction: super::usb_fingerprint::UsbDirection,
    transfer: super::usb_fingerprint::UsbTransferKind,
    max_packet_size: u16,
    interval: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportPreHandshakePolicy {
    LegacyBulkInit,
    Hid407ReadOnlyProbe,
    StopUnsupportedShape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DescriptorCaptureStatus {
    Unknown,
    Captured,
    Unavailable,
}

/// Negotiated output route from handshake/policy (not runtime syscall backend).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NegotiatedOutputRoute {
    LegacyBulk,
    HidReport,
    HidInterrupt,
    ScsiCommand,
}

/// Runtime transport backend route (direct hidraw cannot distinguish interrupt-OUT vs SET_REPORT).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeBackendRoute {
    KernelManagedHidraw,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolFamily {
    Bulk,
    Scsi,
    HidType2,
    HidType3,
    Ly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfilePolicyLabel {
    UpstreamPm58_407,
    ObservedPm68ConservativeStop,
    LegacyBulk,
    ActivePmSub { pm: u8, sub: u8 },
    ObservedInactive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayDimensions {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HidBackendProvenance {
    backend: String,
    runtime_route: RuntimeBackendRoute,
    expected_write_return_bytes: usize,
    kernel_hidraw_doc_ref: String,
    reviewed_hidapi_semantics_commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HidReadEvidence {
    read_capacity_bytes: usize,
    read_timeout_ms: u32,
    /// `None` when no transport return was observed; `Some(0)` when zero bytes returned.
    transport_return_bytes: Option<isize>,
    protocol_response_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HidReadFailureEvidence {
    read_capacity_bytes: usize,
    read_timeout_ms: u32,
    transport_return_bytes: Option<isize>,
    error_kind: HidReadErrorKind,
    message: SafeMessage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HidReadErrorKind {
    Timeout,
    NegativeReturn,
    ShortCount,
    Transport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HidWriteChunkEvidence {
    protocol_chunk_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    logical_output_report_bytes: Option<usize>,
    report_id: u8,
    userspace_submit_bytes: usize,
    /// `None` when no transport return was observed; `Some(0)` when zero bytes returned.
    transport_return_bytes: Option<isize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    endpoint_max_packet_size: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HidWriteErrorKind {
    NegativeReturn,
    UnexpectedCount,
    Transport,
    SessionStopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HidWriteFailureEvidence {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    completed_chunks: Vec<HidWriteChunkEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failing_chunk: Option<HidWriteChunkEvidence>,
    error_kind: HidWriteErrorKind,
    message: SafeMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HidReportEvidence {
    descriptor_status: DescriptorCaptureStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    logical_output_report_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    report_id: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    protocol_chunk_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    userspace_submit_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport_return_bytes: Option<isize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    endpoint_max_packet_size: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_write_authorized: Option<bool>,
    backend: HidBackendProvenance,
    #[serde(skip_serializing_if = "Option::is_none")]
    read: Option<HidReadEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    read_failure: Option<HidReadFailureEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    write_failure: Option<HidWriteFailureEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NegotiatedProfile {
    response_bytes: usize,
    protocol_family: ProtocolFamily,
    pm: u8,
    sub: u8,
    fbl: u8,
    native_dimensions: DisplayDimensions,
    wire_dimensions: DisplayDimensions,
    profile_policy: ProfilePolicyLabel,
    negotiated_output_route: Option<NegotiatedOutputRoute>,
    active_writes_allowed: bool,
    keep_single_session: bool,
    /// Type2 portrait-native wire rotation; absent on generic negotiated profiles.
    #[serde(skip_serializing_if = "Option::is_none")]
    portrait_native: Option<bool>,
    /// Generic panel rotation policy; absent on Type2 negotiated profiles.
    #[serde(skip_serializing_if = "Option::is_none")]
    rotate_panel: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationErrorKind {
    Error,
    Transport,
    Policy,
    Device,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationErrorLink {
    kind: ValidationErrorKind,
    message: SafeMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationFailure {
    stage: ValidationStage,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    errors: Vec<ValidationErrorLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildProvenance {
    version: String,
    commit: String,
    dirty: bool,
}

/// Sanitized free-form text; hostile input is redacted at construction and rejected on deserialize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
struct SafeMessage(String);

impl SafeMessage {
    fn from_raw(input: &str) -> (Self, bool) {
        let sanitized = sanitize_free_text(input);
        (Self(sanitized.text), sanitized.provably_safe)
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SafeMessage {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if contains_privacy_signal(&value) {
            return Err(DeError::custom(
                "hostile or sensitive string rejected in report message field",
            ));
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportDocument {
    schema: u32,
    origin: EvidenceOrigin,
    scope: ValidationScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<ValidationResult>,
    shareable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    failed_step: Option<ValidationStage>,
    build: BuildProvenance,
    upstream_reviewed_commit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    fingerprint: Option<ReportFingerprint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pre_handshake_policy: Option<ReportPreHandshakePolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hid_report: Option<HidReportEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    negotiated: Option<NegotiatedProfile>,
    #[serde(default)]
    checks: ValidationChecks,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<ValidationFailure>,
}

/// Canonical hardware-validation report (`report.toml`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareValidationReport {
    doc: ReportDocument,
    redaction_permanent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedText {
    pub text: String,
    pub provably_safe: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeError {
    MissingMandatoryChecks,
    MissingFingerprint,
    MissingNegotiated,
    UncleanBuild,
    UnknownCommit,
    PriorFailure,
    WrongScope,
    WrongOrigin,
    NotShareable,
    ConservativeStopProfile,
    InvariantViolation,
    AlreadyFinalized,
}

impl fmt::Display for FinalizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingMandatoryChecks => write!(f, "mandatory checks incomplete"),
            Self::MissingFingerprint => write!(f, "fingerprint missing"),
            Self::MissingNegotiated => write!(f, "negotiated profile missing"),
            Self::UncleanBuild => write!(f, "build tree is dirty"),
            Self::UnknownCommit => write!(f, "build commit unknown"),
            Self::PriorFailure => write!(f, "report already failed or aborted"),
            Self::WrongScope => write!(f, "scope mismatch for finalization"),
            Self::WrongOrigin => write!(f, "origin not eligible for this finalization"),
            Self::NotShareable => write!(f, "report marked non-shareable"),
            Self::ConservativeStopProfile => {
                write!(f, "negotiated profile blocks active full pass")
            }
            Self::InvariantViolation => write!(f, "report semantic invariants violated"),
            Self::AlreadyFinalized => write!(f, "report is already finalized"),
        }
    }
}

/// Returned when mutating a report that already has a terminal result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportMutationError {
    AlreadyFinalized,
}

impl fmt::Display for ReportMutationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyFinalized => write!(f, "report is already finalized"),
        }
    }
}

impl std::error::Error for ReportMutationError {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SemanticError {
    ResultStageMismatch,
    MissingFailureDetails,
    UnexpectedFailureDetails,
    MissingFingerprint,
    MissingNegotiated,
    MissingMandatoryChecks,
    ConservativeStopProfile,
    MissingNegotiatedRoute,
    MissingHidEvidence,
    HidWriteNotAuthorized,
    IncompleteHidWriteEvidence,
    InvalidHidWriteReturn,
    InvalidHidReadEvidence,
    InvalidHidBackend,
    RedactionBlocksShareable,
    ShareableStringViolation,
    HostileString,
    InvalidOrigin,
    NotShareable,
    UncleanBuild,
    UnknownBuildCommit,
    ProtocolRouteMismatch,
    NegotiatedProfileMismatch,
    Hid407BindingViolation,
}

impl fmt::Display for SemanticError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResultStageMismatch => write!(f, "result/failed_step/failure are inconsistent"),
            Self::MissingFailureDetails => write!(f, "terminal result missing failure details"),
            Self::UnexpectedFailureDetails => write!(f, "in-progress report has failure details"),
            Self::MissingFingerprint => write!(f, "fingerprint missing"),
            Self::MissingNegotiated => write!(f, "negotiated profile missing"),
            Self::MissingMandatoryChecks => write!(f, "mandatory checks incomplete"),
            Self::ConservativeStopProfile => {
                write!(f, "conservative-stop profile blocks full pass")
            }
            Self::MissingNegotiatedRoute => write!(f, "negotiated output route missing"),
            Self::MissingHidEvidence => write!(f, "HID report evidence missing"),
            Self::HidWriteNotAuthorized => write!(f, "HID active write not authorized"),
            Self::IncompleteHidWriteEvidence => write!(f, "HID write evidence incomplete"),
            Self::InvalidHidWriteReturn => write!(f, "HID write transport return invalid"),
            Self::InvalidHidReadEvidence => write!(f, "HID read evidence inconsistent"),
            Self::InvalidHidBackend => write!(f, "HID backend contract invalid"),
            Self::RedactionBlocksShareable => write!(f, "redaction permanently blocks shareable"),
            Self::ShareableStringViolation => write!(f, "shareable string field invalid"),
            Self::HostileString => write!(f, "hostile string in serialized field"),
            Self::InvalidOrigin => write!(f, "full pass requires physical origin"),
            Self::NotShareable => write!(f, "report is not shareable"),
            Self::UncleanBuild => write!(f, "build tree is dirty"),
            Self::UnknownBuildCommit => write!(f, "build commit unknown or invalid"),
            Self::ProtocolRouteMismatch => write!(f, "protocol family and output route disagree"),
            Self::NegotiatedProfileMismatch => {
                write!(f, "negotiated profile inconsistent with device facts")
            }
            Self::Hid407BindingViolation => {
                write!(f, "Type2 Hid407 context fingerprint binding violated")
            }
        }
    }
}

impl std::error::Error for SemanticError {}

impl HardwareValidationReport {
    pub fn new_in_progress(origin: EvidenceOrigin, scope: ValidationScope) -> Self {
        Self {
            doc: ReportDocument {
                schema: SCHEMA_VERSION,
                origin,
                scope,
                result: None,
                shareable: true,
                failed_step: None,
                build: current_build_provenance(),
                upstream_reviewed_commit: UPSTREAM_REVIEWED_COMMIT.to_string(),
                fingerprint: None,
                pre_handshake_policy: None,
                hid_report: None,
                negotiated: None,
                checks: ValidationChecks::default(),
                failure: None,
            },
            redaction_permanent: false,
        }
    }

    pub fn origin(&self) -> EvidenceOrigin {
        self.doc.origin
    }

    pub fn scope(&self) -> ValidationScope {
        self.doc.scope
    }

    pub fn result(&self) -> Option<ValidationResult> {
        self.doc.result
    }

    pub fn shareable(&self) -> bool {
        self.doc.shareable && !self.redaction_permanent
    }

    pub fn failed_step(&self) -> Option<ValidationStage> {
        self.doc.failed_step
    }

    pub fn checks(&self) -> &ValidationChecks {
        &self.doc.checks
    }

    pub fn fingerprint(&self) -> Option<&ReportFingerprint> {
        self.doc.fingerprint.as_ref()
    }

    pub fn negotiated(&self) -> Option<&NegotiatedProfile> {
        self.doc.negotiated.as_ref()
    }

    pub fn hid_report(&self) -> Option<&HidReportEvidence> {
        self.doc.hid_report.as_ref()
    }

    pub fn failure(&self) -> Option<&ValidationFailure> {
        self.doc.failure.as_ref()
    }

    pub fn build_provenance(&self) -> &BuildProvenance {
        &self.doc.build
    }

    fn ensure_mutable(&self) -> Result<(), ReportMutationError> {
        if self.doc.result.is_some() {
            Err(ReportMutationError::AlreadyFinalized)
        } else {
            Ok(())
        }
    }

    pub fn set_fingerprint(
        &mut self,
        fingerprint: &UsbFingerprint,
        serial_present: bool,
        hidraw_correlated: Option<bool>,
    ) -> Result<(), ReportMutationError> {
        self.ensure_mutable()?;
        self.doc.fingerprint = Some(ReportFingerprint::from_usb_fingerprint(
            fingerprint,
            serial_present,
            hidraw_correlated,
        ));
        Ok(())
    }

    pub fn set_pre_handshake_policy(
        &mut self,
        policy: Type2PreHandshakePolicy,
    ) -> Result<(), ReportMutationError> {
        self.ensure_mutable()?;
        self.doc.pre_handshake_policy = Some(ReportPreHandshakePolicy::from(policy));
        Ok(())
    }

    pub fn record_check(
        &mut self,
        field: CheckField,
        status: CheckStatus,
    ) -> Result<(), ReportMutationError> {
        self.ensure_mutable()?;
        self.doc.checks.set(field, status);
        Ok(())
    }

    pub fn record_negotiated_type2(
        &mut self,
        observation: &Type2NegotiatedObservation,
    ) -> Result<()> {
        self.ensure_mutable().context("report already finalized")?;
        ensure!(
            observation.policy().output() != Some(HidOutputRoute::LegacyBulk),
            "legacy Type2 output policy is fixture-only and cannot be recorded for production",
        );
        let exact_descriptor = self.doc.fingerprint.as_ref().is_some_and(|fingerprint| {
            matches!(
                super::policy::exact_descriptor_policy(&fingerprint.to_usb_fingerprint()),
                Ok(super::policy::ExactDescriptorPolicy::Type2)
            )
        });
        let device_info = if exact_descriptor && observation.response() == PM58_RESPONSE {
            ExactDevicePolicy::Type2Pm58.device_info()
        } else if exact_descriptor && observation.response() == PM128_RESPONSE {
            ExactDevicePolicy::Type2Pm128.device_info()
        } else {
            build_device_info(
                WireProtocol::HidType2,
                WINBOND_HID2_VID,
                WINBOND_HID2_PID,
                observation.pm(),
                observation.sub(),
                None,
            )
            .context("derive negotiated DeviceInfo from Type2 observation")?
        };
        let candidate = NegotiatedProfile::from_type2(observation, &device_info)?;
        let mut provisional = self.doc.clone();
        provisional.negotiated = Some(candidate.clone());
        validate_negotiated_profile_consistency(&provisional)
            .map_err(|err| anyhow::anyhow!("negotiated profile failed validation: {err}"))?;
        self.doc.negotiated = Some(candidate);
        Ok(())
    }

    /// Record negotiated profile for bulk/SCSI/HID3/LY devices from resolved [`DeviceInfo`].
    ///
    /// Output route is derived from [`WireProtocol`] (bulk/HID3/LY → legacy bulk, SCSI → scsi command).
    pub fn record_negotiated_device(
        &mut self,
        device_info: &DeviceInfo,
        response_bytes: usize,
    ) -> Result<()> {
        self.ensure_mutable().context("report already finalized")?;
        let fingerprint = self
            .doc
            .fingerprint
            .as_ref()
            .context("fingerprint required before recording negotiated generic device")?;
        ensure!(
            device_info.vid == fingerprint.vid && device_info.pid == fingerprint.pid,
            "device_info VID:PID {:04x}:{:04x} does not match report fingerprint {:04x}:{:04x}",
            device_info.vid,
            device_info.pid,
            fingerprint.vid,
            fingerprint.pid,
        );
        ensure!(
            !matches!(device_info.protocol, WireProtocol::HidType2),
            "use record_negotiated_type2 for HID Type2"
        );
        let candidate = NegotiatedProfile::from_device_info(device_info, response_bytes)?;
        let mut provisional = self.doc.clone();
        provisional.negotiated = Some(candidate.clone());
        validate_negotiated_profile_consistency(&provisional)
            .map_err(|err| anyhow::anyhow!("negotiated profile failed validation: {err}"))?;
        self.doc.negotiated = Some(candidate);
        Ok(())
    }

    pub fn set_hid_backend_contract(
        &mut self,
        contract: HidReportBackendContract,
    ) -> Result<(), ReportMutationError> {
        self.ensure_mutable()?;
        let evidence = self
            .doc
            .hid_report
            .get_or_insert_with(|| HidReportEvidence::empty_with_backend(contract));
        evidence.backend = HidBackendProvenance::from(contract);
        Ok(())
    }

    pub fn record_hid_read(
        &mut self,
        observation: &HidReadObservation,
    ) -> Result<(), ReportMutationError> {
        self.ensure_mutable()?;
        let evidence = self.doc.hid_report.get_or_insert_with(|| {
            HidReportEvidence::empty_with_backend(LINUX_HIDRAW_BACKEND_CONTRACT)
        });
        evidence.read = Some(HidReadEvidence::from(observation));
        Ok(())
    }

    pub fn record_hid_read_failure(
        &mut self,
        read_capacity_bytes: usize,
        read_timeout_ms: u32,
        transport_return_bytes: Option<isize>,
        error_kind: HidReadErrorKind,
        message: &str,
    ) -> Result<(), ReportMutationError> {
        self.ensure_mutable()?;
        let (message, safe) = SafeMessage::from_raw(message);
        if !safe {
            self.mark_redacted();
        }
        let evidence = self.doc.hid_report.get_or_insert_with(|| {
            HidReportEvidence::empty_with_backend(LINUX_HIDRAW_BACKEND_CONTRACT)
        });
        evidence.read_failure = Some(HidReadFailureEvidence {
            read_capacity_bytes,
            read_timeout_ms,
            transport_return_bytes,
            error_kind,
            message,
        });
        Ok(())
    }

    pub fn set_hid_descriptor_status(
        &mut self,
        status: DescriptorCaptureStatus,
    ) -> Result<(), ReportMutationError> {
        self.ensure_mutable()?;
        let evidence = self.doc.hid_report.get_or_insert_with(|| {
            HidReportEvidence::empty_with_backend(LINUX_HIDRAW_BACKEND_CONTRACT)
        });
        evidence.descriptor_status = status;
        Ok(())
    }

    pub fn set_hid_active_write_authorized(
        &mut self,
        authorized: bool,
    ) -> Result<(), ReportMutationError> {
        self.ensure_mutable()?;
        let evidence = self.doc.hid_report.get_or_insert_with(|| {
            HidReportEvidence::empty_with_backend(LINUX_HIDRAW_BACKEND_CONTRACT)
        });
        evidence.active_write_authorized = Some(authorized);
        Ok(())
    }

    pub fn record_hid_write_observation(
        &mut self,
        protocol_chunk_bytes: usize,
        logical_output_report_bytes: Option<usize>,
        report_id: u8,
        userspace_submit_bytes: usize,
        transport_return_bytes: Option<isize>,
        endpoint_max_packet_size: Option<u16>,
    ) -> Result<(), ReportMutationError> {
        self.ensure_mutable()?;
        let evidence = self.doc.hid_report.get_or_insert_with(|| {
            HidReportEvidence::empty_with_backend(LINUX_HIDRAW_BACKEND_CONTRACT)
        });
        evidence.protocol_chunk_bytes = Some(protocol_chunk_bytes);
        evidence.logical_output_report_bytes = logical_output_report_bytes;
        evidence.report_id = Some(report_id);
        evidence.userspace_submit_bytes = Some(userspace_submit_bytes);
        evidence.transport_return_bytes = transport_return_bytes;
        evidence.endpoint_max_packet_size = endpoint_max_packet_size;
        Ok(())
    }

    pub fn record_hid_chunked_write_failure(
        &mut self,
        failure: &HidChunkedWriteFailure,
    ) -> Result<(), ReportMutationError> {
        self.ensure_mutable()?;
        let evidence = self.doc.hid_report.get_or_insert_with(|| {
            HidReportEvidence::empty_with_backend(LINUX_HIDRAW_BACKEND_CONTRACT)
        });
        let (write_failure, safe) = HidWriteFailureEvidence::from_failure(failure);
        evidence.write_failure = Some(write_failure);
        if !safe {
            self.mark_redacted();
        }
        Ok(())
    }

    pub fn fail_at(
        &mut self,
        stage: ValidationStage,
        errors: &[(ValidationErrorKind, &str)],
    ) -> Result<(), ReportMutationError> {
        self.record_terminal(ValidationResult::Fail, stage, errors)
    }

    pub fn abort_at(
        &mut self,
        stage: ValidationStage,
        errors: &[(ValidationErrorKind, &str)],
    ) -> Result<(), ReportMutationError> {
        self.record_terminal(ValidationResult::Aborted, stage, errors)
    }

    fn record_terminal(
        &mut self,
        result: ValidationResult,
        stage: ValidationStage,
        errors: &[(ValidationErrorKind, &str)],
    ) -> Result<(), ReportMutationError> {
        self.ensure_mutable()?;
        self.doc.result = Some(result);
        self.doc.failed_step = Some(stage);
        let (failure, safe) = ValidationFailure::from_typed_messages(stage, errors);
        self.doc.failure = Some(failure);
        if !safe {
            self.mark_redacted();
        }
        Ok(())
    }

    pub fn finalize_passive_pass(&mut self) -> Result<(), FinalizeError> {
        ensure_scope(self, ValidationScope::Passive)?;
        if self.doc.result.is_some() {
            return Err(FinalizeError::AlreadyFinalized);
        }
        if self.doc.fingerprint.is_none() {
            return Err(FinalizeError::MissingFingerprint);
        }
        if !self
            .doc
            .checks
            .all_mandatory_pass(ValidationChecks::mandatory_passive_fields())
        {
            return Err(FinalizeError::MissingMandatoryChecks);
        }
        let mut prospective = self.doc.clone();
        prospective.result = Some(ValidationResult::Pass);
        validate_semantics(&prospective, self.redaction_permanent)
            .map_err(|_| FinalizeError::InvariantViolation)?;
        self.doc.result = Some(ValidationResult::Pass);
        Ok(())
    }

    pub fn finalize_full_pass(&mut self) -> Result<(), FinalizeError> {
        ensure_scope(self, ValidationScope::Full)?;
        if self.doc.origin != EvidenceOrigin::Physical {
            return Err(FinalizeError::WrongOrigin);
        }
        if self.doc.result.is_some() {
            return Err(FinalizeError::AlreadyFinalized);
        }
        if self.doc.fingerprint.is_none() {
            return Err(FinalizeError::MissingFingerprint);
        }
        if self.doc.negotiated.is_none() {
            return Err(FinalizeError::MissingNegotiated);
        }
        if !self
            .doc
            .checks
            .all_mandatory_pass(ValidationChecks::mandatory_full_fields())
        {
            return Err(FinalizeError::MissingMandatoryChecks);
        }
        if let Some(negotiated) = &self.doc.negotiated {
            if negotiated.profile_policy == ProfilePolicyLabel::ObservedPm68ConservativeStop
                || !negotiated.active_writes_allowed
            {
                return Err(FinalizeError::ConservativeStopProfile);
            }
        }
        if !build_commit_known(&self.doc.build.commit) {
            return Err(FinalizeError::UnknownCommit);
        }
        if self.doc.build.dirty {
            return Err(FinalizeError::UncleanBuild);
        }
        if !self.shareable() {
            return Err(FinalizeError::NotShareable);
        }
        let mut prospective = self.doc.clone();
        prospective.result = Some(ValidationResult::Pass);
        validate_semantics(&prospective, self.redaction_permanent)
            .map_err(map_semantic_to_finalize_error)?;
        validate_shareable_strings(&prospective).map_err(map_semantic_to_finalize_error)?;
        self.doc.result = Some(ValidationResult::Pass);
        Ok(())
    }

    /// Recompute Tested-badge eligibility; never trust a stored flag.
    pub fn eligible_for_tested(&self) -> bool {
        self.doc.scope == ValidationScope::Full
            && matches!(self.doc.result, Some(ValidationResult::Pass))
            && validate_semantics(&self.doc, self.redaction_permanent).is_ok()
            && validate_shareable_strings(&self.doc).is_ok()
    }

    pub fn to_private_toml(&self) -> Result<String, toml::ser::Error> {
        let mut doc = self.doc.clone();
        doc.canonicalize();
        toml::to_string_pretty(&doc)
    }

    pub fn to_shareable_toml(&self) -> Result<String> {
        validate_semantics(&self.doc, self.redaction_permanent)
            .context("report failed semantic validation")?;
        ensure!(self.shareable(), "report is not shareable");
        validate_shareable_strings(&self.doc).context("shareable string validation failed")?;
        self.to_private_toml()
            .map_err(|err| anyhow::anyhow!("shareable serialization failed: {err}"))
    }

    pub fn from_toml(input: &str) -> Result<Self, toml::de::Error> {
        let value: toml::Value = toml::from_str(input)?;
        reject_unknown_root_fields(&value)?;
        let doc: ReportDocument = value.try_into()?;
        if doc.schema != SCHEMA_VERSION {
            return Err(toml::de::Error::custom(format!(
                "unsupported schema version {} (expected {SCHEMA_VERSION})",
                doc.schema
            )));
        }
        let report = Self {
            doc,
            redaction_permanent: false,
        };
        validate_build_provenance(&report.doc).map_err(|err| {
            toml::de::Error::custom(format!("build provenance failed validation: {err}"))
        })?;
        if let Some(negotiated) = &report.doc.negotiated {
            validate_protocol_route_consistency(negotiated).map_err(|err| {
                toml::de::Error::custom(format!("negotiated profile failed validation: {err}"))
            })?;
            validate_negotiated_profile_consistency(&report.doc).map_err(|err| {
                toml::de::Error::custom(format!("negotiated profile failed validation: {err}"))
            })?;
        }
        if report.doc.result.is_some() {
            validate_semantics(&report.doc, report.redaction_permanent).map_err(|err| {
                toml::de::Error::custom(format!(
                    "completed report failed semantic validation: {err}"
                ))
            })?;
        }
        Ok(report)
    }

    fn mark_redacted(&mut self) {
        self.redaction_permanent = true;
        self.doc.shareable = false;
    }
}

fn reject_unknown_root_fields(value: &toml::Value) -> Result<(), toml::de::Error> {
    const ALLOWED: &[&str] = &[
        "schema",
        "origin",
        "scope",
        "result",
        "shareable",
        "failed_step",
        "build",
        "upstream_reviewed_commit",
        "fingerprint",
        "pre_handshake_policy",
        "hid_report",
        "negotiated",
        "checks",
        "failure",
    ];
    let table = value
        .as_table()
        .ok_or_else(|| toml::de::Error::custom("report root must be a TOML table"))?;
    for key in table.keys() {
        if !ALLOWED.contains(&key.as_str()) {
            return Err(toml::de::Error::custom(format!(
                "unknown report field `{key}`"
            )));
        }
    }
    Ok(())
}

fn ensure_scope(
    report: &HardwareValidationReport,
    scope: ValidationScope,
) -> Result<(), FinalizeError> {
    if report.doc.scope == scope {
        Ok(())
    } else {
        Err(FinalizeError::WrongScope)
    }
}

fn validate_semantics(
    doc: &ReportDocument,
    redaction_permanent: bool,
) -> Result<(), SemanticError> {
    if doc.shareable && redaction_permanent {
        return Err(SemanticError::RedactionBlocksShareable);
    }

    validate_build_provenance(doc)?;

    if let Some(negotiated) = &doc.negotiated {
        validate_protocol_route_consistency(negotiated)?;
        validate_negotiated_profile_consistency(doc)?;
    }

    match doc.result {
        None => {
            if doc.failed_step.is_some() || doc.failure.is_some() {
                return Err(SemanticError::UnexpectedFailureDetails);
            }
        }
        Some(ValidationResult::Pass) => {
            if doc.failed_step.is_some() || doc.failure.is_some() {
                return Err(SemanticError::ResultStageMismatch);
            }
            if doc.fingerprint.is_none() {
                return Err(SemanticError::MissingFingerprint);
            }
            match doc.scope {
                ValidationScope::Passive => {
                    if !doc
                        .checks
                        .all_mandatory_pass(ValidationChecks::mandatory_passive_fields())
                    {
                        return Err(SemanticError::MissingMandatoryChecks);
                    }
                }
                ValidationScope::Full => {
                    if doc.negotiated.is_none() {
                        return Err(SemanticError::MissingNegotiated);
                    }
                    if !doc
                        .checks
                        .all_mandatory_pass(ValidationChecks::mandatory_full_fields())
                    {
                        return Err(SemanticError::MissingMandatoryChecks);
                    }
                    validate_full_pass_completion_requirements(doc, redaction_permanent)?;
                    validate_full_pass_evidence(doc)?;
                }
            }
        }
        Some(ValidationResult::Fail) | Some(ValidationResult::Aborted) => {
            let stage = doc
                .failed_step
                .ok_or(SemanticError::MissingFailureDetails)?;
            let failure = doc
                .failure
                .as_ref()
                .ok_or(SemanticError::MissingFailureDetails)?;
            if failure.stage != stage {
                return Err(SemanticError::ResultStageMismatch);
            }
        }
    }

    Ok(())
}

fn validate_full_pass_completion_requirements(
    doc: &ReportDocument,
    redaction_permanent: bool,
) -> Result<(), SemanticError> {
    if doc.origin != EvidenceOrigin::Physical {
        return Err(SemanticError::InvalidOrigin);
    }
    if !doc.shareable || redaction_permanent {
        return Err(SemanticError::NotShareable);
    }
    if !build_commit_known(&doc.build.commit) {
        return Err(SemanticError::UnknownBuildCommit);
    }
    if doc.build.dirty {
        return Err(SemanticError::UncleanBuild);
    }
    Ok(())
}

fn map_semantic_to_finalize_error(err: SemanticError) -> FinalizeError {
    match err {
        SemanticError::ConservativeStopProfile => FinalizeError::ConservativeStopProfile,
        SemanticError::UnknownBuildCommit => FinalizeError::UnknownCommit,
        SemanticError::UncleanBuild => FinalizeError::UncleanBuild,
        SemanticError::NotShareable
        | SemanticError::RedactionBlocksShareable
        | SemanticError::ShareableStringViolation
        | SemanticError::HostileString => FinalizeError::NotShareable,
        SemanticError::InvalidOrigin => FinalizeError::WrongOrigin,
        _ => FinalizeError::InvariantViolation,
    }
}

fn validate_full_pass_evidence(doc: &ReportDocument) -> Result<(), SemanticError> {
    let negotiated = doc
        .negotiated
        .as_ref()
        .ok_or(SemanticError::MissingNegotiated)?;

    if negotiated.profile_policy == ProfilePolicyLabel::ObservedPm68ConservativeStop
        || !negotiated.active_writes_allowed
    {
        return Err(SemanticError::ConservativeStopProfile);
    }

    if !doc.checks.passed(CheckField::ActiveWrite) {
        return Err(SemanticError::MissingMandatoryChecks);
    }

    validate_protocol_route_consistency(negotiated)?;

    let route = negotiated
        .negotiated_output_route
        .ok_or(SemanticError::MissingNegotiatedRoute)?;

    match route {
        NegotiatedOutputRoute::HidReport => validate_hid_report_route_evidence(doc)?,
        NegotiatedOutputRoute::HidInterrupt
        | NegotiatedOutputRoute::LegacyBulk
        | NegotiatedOutputRoute::ScsiCommand => {}
    }

    Ok(())
}

fn validate_protocol_route_consistency(
    negotiated: &NegotiatedProfile,
) -> Result<(), SemanticError> {
    if !negotiated.active_writes_allowed {
        if negotiated.negotiated_output_route.is_some() {
            return Err(SemanticError::ProtocolRouteMismatch);
        }
        return Ok(());
    }
    let route = negotiated
        .negotiated_output_route
        .ok_or(SemanticError::MissingNegotiatedRoute)?;
    let expected = match negotiated.protocol_family {
        ProtocolFamily::Bulk | ProtocolFamily::HidType3 | ProtocolFamily::Ly => {
            NegotiatedOutputRoute::LegacyBulk
        }
        ProtocolFamily::Scsi => NegotiatedOutputRoute::ScsiCommand,
        ProtocolFamily::HidType2 => {
            if matches!(
                route,
                NegotiatedOutputRoute::HidReport
                    | NegotiatedOutputRoute::HidInterrupt
                    | NegotiatedOutputRoute::LegacyBulk
            ) {
                return Ok(());
            }
            return Err(SemanticError::ProtocolRouteMismatch);
        }
    };
    if route != expected {
        return Err(SemanticError::ProtocolRouteMismatch);
    }
    Ok(())
}

fn validate_build_provenance(doc: &ReportDocument) -> Result<(), SemanticError> {
    if doc.build.commit == "unknown" && !doc.build.dirty {
        return Err(SemanticError::ShareableStringViolation);
    }
    Ok(())
}

fn wire_protocol_from_family(family: ProtocolFamily) -> WireProtocol {
    match family {
        ProtocolFamily::Bulk => WireProtocol::Bulk,
        ProtocolFamily::Scsi => WireProtocol::Scsi,
        ProtocolFamily::HidType2 => WireProtocol::HidType2,
        ProtocolFamily::HidType3 => WireProtocol::HidType3,
        ProtocolFamily::Ly => WireProtocol::Ly,
    }
}

fn expected_wire_dimensions(
    device_info: &DeviceInfo,
    portrait_native: bool,
) -> Result<DisplayDimensions, SemanticError> {
    if portrait_native && device_info.protocol == WireProtocol::HidType2 {
        return Ok(DisplayDimensions {
            width: device_info.width(),
            height: device_info.height(),
        });
    }
    let (width, height) = device_info
        .wire_dimensions()
        .map_err(|_| SemanticError::NegotiatedProfileMismatch)?;
    Ok(DisplayDimensions { width, height })
}

fn negotiated_route_from_lcd(route: LcdTransportRoute) -> NegotiatedOutputRoute {
    match route {
        LcdTransportRoute::LegacyBulk => NegotiatedOutputRoute::LegacyBulk,
        LcdTransportRoute::ScsiCommand => NegotiatedOutputRoute::ScsiCommand,
    }
}

fn fbl_input_for_validation(family: ProtocolFamily, serialized_fbl: u8) -> Option<u8> {
    match family {
        ProtocolFamily::Bulk | ProtocolFamily::HidType2 | ProtocolFamily::Ly => None,
        ProtocolFamily::Scsi | ProtocolFamily::HidType3 => Some(serialized_fbl),
    }
}

fn validate_fingerprint_route_binding(
    fingerprint: &ReportFingerprint,
    negotiated: &NegotiatedProfile,
) -> Result<(), SemanticError> {
    let usb_fp = fingerprint.to_usb_fingerprint();
    let (expected_protocol, expected_lcd_route) =
        resolve_known_lcd_route(fingerprint.vid, fingerprint.pid, &usb_fp)
            .map_err(|_| SemanticError::NegotiatedProfileMismatch)?;

    if wire_protocol_from_family(negotiated.protocol_family) != expected_protocol {
        return Err(SemanticError::NegotiatedProfileMismatch);
    }

    if negotiated.active_writes_allowed {
        let expected_route = negotiated_route_from_lcd(expected_lcd_route);
        if negotiated.negotiated_output_route != Some(expected_route) {
            return Err(SemanticError::ProtocolRouteMismatch);
        }
    }

    Ok(())
}

fn validate_fingerprint_protocol_matrix(
    fingerprint: &ReportFingerprint,
    negotiated: &NegotiatedProfile,
) -> Result<(), SemanticError> {
    match negotiated.protocol_family {
        ProtocolFamily::HidType2 => {
            if protocol_for_id(fingerprint.vid, fingerprint.pid) != Some(WireProtocol::HidType2) {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
            Ok(())
        }
        ProtocolFamily::Bulk
        | ProtocolFamily::Scsi
        | ProtocolFamily::HidType3
        | ProtocolFamily::Ly => validate_fingerprint_route_binding(fingerprint, negotiated),
    }
}

fn requires_hid407_binding(doc: &ReportDocument, negotiated: &NegotiatedProfile) -> bool {
    if negotiated.protocol_family != ProtocolFamily::HidType2 {
        return false;
    }
    doc.pre_handshake_policy == Some(ReportPreHandshakePolicy::Hid407ReadOnlyProbe)
        || negotiated.negotiated_output_route == Some(NegotiatedOutputRoute::HidReport)
}

fn validate_type2_pre_handshake_binding(
    doc: &ReportDocument,
    fingerprint: &ReportFingerprint,
) -> Result<(), SemanticError> {
    let usb_fp = fingerprint.to_usb_fingerprint();
    let selected =
        select_type2_pre_handshake_policy(&usb_fp, fingerprint.hidraw_correlated.unwrap_or(false));
    if selected == Type2PreHandshakePolicy::StopUnsupportedShape {
        return Err(SemanticError::NegotiatedProfileMismatch);
    }
    let recorded = doc
        .pre_handshake_policy
        .ok_or(SemanticError::NegotiatedProfileMismatch)?;
    let expected = ReportPreHandshakePolicy::from(selected);
    if recorded != expected {
        return Err(SemanticError::NegotiatedProfileMismatch);
    }
    Ok(())
}

fn validate_hid407_fingerprint_binding(
    doc: &ReportDocument,
    fingerprint: &ReportFingerprint,
) -> Result<(), SemanticError> {
    if fingerprint.vid != WINBOND_HID2_VID || fingerprint.pid != WINBOND_HID2_PID {
        return Err(SemanticError::Hid407BindingViolation);
    }
    if fingerprint.bcd_device != BCD_DEVICE_407 {
        return Err(SemanticError::Hid407BindingViolation);
    }
    if fingerprint.hidraw_correlated != Some(true) {
        return Err(SemanticError::Hid407BindingViolation);
    }
    if doc.pre_handshake_policy != Some(ReportPreHandshakePolicy::Hid407ReadOnlyProbe) {
        return Err(SemanticError::Hid407BindingViolation);
    }
    let has_hid_interrupt_in = fingerprint.interfaces.iter().any(|iface| {
        iface.class == 3
            && iface.endpoints.iter().any(|ep| {
                ep.direction == UsbDirection::In
                    && ep.transfer == UsbTransferKind::Interrupt
                    && ep.max_packet_size == 8
            })
    });
    if !has_hid_interrupt_in {
        return Err(SemanticError::Hid407BindingViolation);
    }
    Ok(())
}

fn validate_negotiated_profile_consistency(doc: &ReportDocument) -> Result<(), SemanticError> {
    let negotiated = doc
        .negotiated
        .as_ref()
        .ok_or(SemanticError::MissingNegotiated)?;
    let fingerprint = doc
        .fingerprint
        .as_ref()
        .ok_or(SemanticError::MissingFingerprint)?;

    validate_fingerprint_protocol_matrix(fingerprint, negotiated)?;

    let exact_descriptor = matches!(
        super::policy::exact_descriptor_policy(&fingerprint.to_usb_fingerprint()),
        Ok(super::policy::ExactDescriptorPolicy::Type2)
    );
    let wire_protocol = wire_protocol_from_family(negotiated.protocol_family);
    let device_info = if exact_descriptor
        && negotiated.protocol_family == ProtocolFamily::HidType2
        && negotiated.pm == 58
        && negotiated.sub == 0
        && negotiated.response_bytes == 8
    {
        ExactDevicePolicy::Type2Pm58.device_info()
    } else if exact_descriptor
        && negotiated.protocol_family == ProtocolFamily::HidType2
        && negotiated.pm == 128
        && negotiated.sub == 1
        && negotiated.response_bytes == PM128_RESPONSE.len()
    {
        ExactDevicePolicy::Type2Pm128.device_info()
    } else {
        build_device_info(
            wire_protocol,
            fingerprint.vid,
            fingerprint.pid,
            negotiated.pm,
            negotiated.sub,
            fbl_input_for_validation(negotiated.protocol_family, negotiated.fbl),
        )
        .map_err(|_| SemanticError::NegotiatedProfileMismatch)?
    };

    if device_info.fbl != negotiated.fbl {
        return Err(SemanticError::NegotiatedProfileMismatch);
    }

    if negotiated.native_dimensions.width != device_info.width()
        || negotiated.native_dimensions.height != device_info.height()
    {
        return Err(SemanticError::NegotiatedProfileMismatch);
    }

    if let ProfilePolicyLabel::ActivePmSub { pm, sub } = negotiated.profile_policy {
        if pm != negotiated.pm || sub != negotiated.sub {
            return Err(SemanticError::NegotiatedProfileMismatch);
        }
    }

    match negotiated.protocol_family {
        ProtocolFamily::HidType2 => {
            validate_type2_pre_handshake_binding(doc, fingerprint)?;
            let portrait = negotiated
                .portrait_native
                .ok_or(SemanticError::NegotiatedProfileMismatch)?;
            if negotiated.rotate_panel.is_some() {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
            let expected_wire = expected_wire_dimensions(&device_info, portrait)?;
            if negotiated.wire_dimensions != expected_wire {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
            validate_type2_negotiated_profile(doc, negotiated)?;
        }
        ProtocolFamily::Bulk => {
            validate_generic_lifecycle_flags(negotiated, &device_info)?;
            validate_generic_wire_dimensions(negotiated, &device_info)?;
            if negotiated.profile_policy != ProfilePolicyLabel::LegacyBulk {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
        }
        ProtocolFamily::Scsi | ProtocolFamily::HidType3 | ProtocolFamily::Ly => {
            validate_generic_lifecycle_flags(negotiated, &device_info)?;
            validate_generic_wire_dimensions(negotiated, &device_info)?;
            let ProfilePolicyLabel::ActivePmSub { pm, sub } = negotiated.profile_policy else {
                return Err(SemanticError::NegotiatedProfileMismatch);
            };
            if pm != negotiated.pm || sub != negotiated.sub {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
        }
    }

    if requires_hid407_binding(doc, negotiated) {
        validate_hid407_fingerprint_binding(doc, fingerprint)?;
    }

    Ok(())
}

fn validate_type2_negotiated_profile(
    doc: &ReportDocument,
    negotiated: &NegotiatedProfile,
) -> Result<(), SemanticError> {
    match negotiated.profile_policy {
        ProfilePolicyLabel::UpstreamPm58_407 => {
            ensure_type2_tuple(negotiated, 58, 0, 58)?;
            if negotiated.response_bytes != 8 {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
            if negotiated.negotiated_output_route != Some(NegotiatedOutputRoute::HidReport) {
                return Err(SemanticError::ProtocolRouteMismatch);
            }
            if !negotiated.active_writes_allowed
                || !negotiated.keep_single_session
                || negotiated.portrait_native != Some(true)
                || negotiated.rotate_panel.is_some()
            {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
            if negotiated.native_dimensions
                != (DisplayDimensions {
                    width: 240,
                    height: 320,
                })
                || negotiated.wire_dimensions
                    != (DisplayDimensions {
                        width: 240,
                        height: 320,
                    })
            {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
            if doc.pre_handshake_policy != Some(ReportPreHandshakePolicy::Hid407ReadOnlyProbe) {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
        }
        ProfilePolicyLabel::ObservedPm68ConservativeStop => {
            ensure_type2_tuple(negotiated, 68, negotiated.sub, 192)?;
            if negotiated.response_bytes != 8 {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
            if negotiated.negotiated_output_route.is_some()
                || negotiated.active_writes_allowed
                || negotiated.keep_single_session
                || negotiated.portrait_native != Some(false)
                || negotiated.rotate_panel.is_some()
            {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
            if negotiated.native_dimensions
                != (DisplayDimensions {
                    width: 1280,
                    height: 480,
                })
                || negotiated.wire_dimensions
                    != (DisplayDimensions {
                        width: 1280,
                        height: 480,
                    })
            {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
            if doc.pre_handshake_policy != Some(ReportPreHandshakePolicy::Hid407ReadOnlyProbe) {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
        }
        ProfilePolicyLabel::ObservedInactive => {
            if doc.pre_handshake_policy != Some(ReportPreHandshakePolicy::Hid407ReadOnlyProbe) {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
            // Short (PM58-class) or legacy-shaped (init-elicited) inactive replies.
            let ok_len = negotiated.response_bytes == 8
                || negotiated.response_bytes >= TYPE2_LEGACY_RESPONSE_MIN;
            if !ok_len {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
            if negotiated.negotiated_output_route.is_some()
                || negotiated.active_writes_allowed
                || negotiated.keep_single_session
                || negotiated.portrait_native != Some(false)
                || negotiated.rotate_panel.is_some()
            {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
        }
        ProfilePolicyLabel::LegacyBulk => {
            if doc.pre_handshake_policy != Some(ReportPreHandshakePolicy::LegacyBulkInit) {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
            if negotiated.negotiated_output_route != Some(NegotiatedOutputRoute::LegacyBulk) {
                return Err(SemanticError::ProtocolRouteMismatch);
            }
            if !negotiated.active_writes_allowed
                || negotiated.keep_single_session
                || negotiated.portrait_native != Some(false)
                || negotiated.rotate_panel.is_some()
            {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
            if negotiated.response_bytes < TYPE2_LEGACY_RESPONSE_MIN {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
        }
        ProfilePolicyLabel::ActivePmSub { pm, sub } => {
            if pm != 128
                || sub != 1
                || negotiated.response_bytes != PM128_RESPONSE.len()
                || negotiated.negotiated_output_route != Some(NegotiatedOutputRoute::HidInterrupt)
                || !negotiated.active_writes_allowed
                || negotiated.keep_single_session
                || negotiated.portrait_native != Some(false)
                || negotiated.rotate_panel.is_some()
                || doc.pre_handshake_policy != Some(ReportPreHandshakePolicy::Hid407ReadOnlyProbe)
            {
                return Err(SemanticError::NegotiatedProfileMismatch);
            }
        }
    }
    Ok(())
}

fn validate_generic_lifecycle_flags(
    negotiated: &NegotiatedProfile,
    device_info: &DeviceInfo,
) -> Result<(), SemanticError> {
    if !negotiated.active_writes_allowed {
        return Err(SemanticError::NegotiatedProfileMismatch);
    }
    if negotiated.keep_single_session {
        return Err(SemanticError::NegotiatedProfileMismatch);
    }
    if negotiated.portrait_native.is_some() {
        return Err(SemanticError::NegotiatedProfileMismatch);
    }
    if negotiated.rotate_panel != Some(device_info.profile.rotate_panel) {
        return Err(SemanticError::NegotiatedProfileMismatch);
    }
    Ok(())
}

fn validate_generic_wire_dimensions(
    negotiated: &NegotiatedProfile,
    device_info: &DeviceInfo,
) -> Result<(), SemanticError> {
    let (width, height) = device_info
        .wire_dimensions()
        .map_err(|_| SemanticError::NegotiatedProfileMismatch)?;
    if negotiated.wire_dimensions != (DisplayDimensions { width, height }) {
        return Err(SemanticError::NegotiatedProfileMismatch);
    }
    Ok(())
}

fn ensure_type2_tuple(
    negotiated: &NegotiatedProfile,
    pm: u8,
    sub: u8,
    fbl: u8,
) -> Result<(), SemanticError> {
    if negotiated.pm != pm || negotiated.sub != sub || negotiated.fbl != fbl {
        return Err(SemanticError::NegotiatedProfileMismatch);
    }
    Ok(())
}

fn validate_hid_report_route_evidence(doc: &ReportDocument) -> Result<(), SemanticError> {
    let negotiated = doc
        .negotiated
        .as_ref()
        .ok_or(SemanticError::MissingNegotiated)?;
    let hid = doc
        .hid_report
        .as_ref()
        .ok_or(SemanticError::MissingHidEvidence)?;

    validate_shareable_hid_backend(&hid.backend)?;
    if hid.backend.runtime_route != RuntimeBackendRoute::KernelManagedHidraw {
        return Err(SemanticError::InvalidHidBackend);
    }
    if hid.read_failure.is_some() || hid.write_failure.is_some() {
        return Err(SemanticError::IncompleteHidWriteEvidence);
    }

    let read = hid
        .read
        .as_ref()
        .ok_or(SemanticError::InvalidHidReadEvidence)?;
    if read.protocol_response_bytes != negotiated.response_bytes {
        return Err(SemanticError::InvalidHidReadEvidence);
    }
    let returned = read
        .transport_return_bytes
        .ok_or(SemanticError::InvalidHidReadEvidence)?;
    if returned < 0 {
        return Err(SemanticError::InvalidHidReadEvidence);
    }
    let returned_usize = returned as usize;
    if returned_usize != read.protocol_response_bytes {
        return Err(SemanticError::InvalidHidReadEvidence);
    }
    if read.protocol_response_bytes > read.read_capacity_bytes {
        return Err(SemanticError::InvalidHidReadEvidence);
    }

    if hid.active_write_authorized != Some(true) {
        return Err(SemanticError::HidWriteNotAuthorized);
    }
    if hid.report_id != Some(REPORT_ID_UNNUMBERED) {
        return Err(SemanticError::IncompleteHidWriteEvidence);
    }
    if hid.protocol_chunk_bytes != Some(PROTOCOL_CHUNK_BYTES) {
        return Err(SemanticError::IncompleteHidWriteEvidence);
    }
    if hid.userspace_submit_bytes != Some(USERSPACE_SUBMIT_BYTES) {
        return Err(SemanticError::IncompleteHidWriteEvidence);
    }
    if hid.logical_output_report_bytes != Some(PROTOCOL_CHUNK_BYTES) {
        return Err(SemanticError::IncompleteHidWriteEvidence);
    }
    if hid.transport_return_bytes != Some(hid.backend.expected_write_return_bytes as isize) {
        return Err(SemanticError::InvalidHidWriteReturn);
    }

    Ok(())
}

fn validate_shareable_strings(doc: &ReportDocument) -> Result<(), SemanticError> {
    if doc.upstream_reviewed_commit != UPSTREAM_REVIEWED_COMMIT {
        return Err(SemanticError::ShareableStringViolation);
    }
    if !is_valid_shareable_version(&doc.build.version) {
        return Err(SemanticError::ShareableStringViolation);
    }
    if !is_valid_shareable_commit(&doc.build.commit) {
        return Err(SemanticError::ShareableStringViolation);
    }
    if contains_privacy_signal(&doc.build.version) || contains_privacy_signal(&doc.build.commit) {
        return Err(SemanticError::HostileString);
    }
    if let Some(fingerprint) = &doc.fingerprint {
        if !is_valid_bcd_device(&fingerprint.bcd_device) {
            return Err(SemanticError::ShareableStringViolation);
        }
        if contains_privacy_signal(&fingerprint.bcd_device) {
            return Err(SemanticError::HostileString);
        }
    }
    if let Some(hid) = &doc.hid_report {
        validate_shareable_hid_backend(&hid.backend)?;
        for link in hid
            .read_failure
            .iter()
            .map(|failure| failure.message.as_str())
        {
            if contains_privacy_signal(link) {
                return Err(SemanticError::HostileString);
            }
        }
        if let Some(write_failure) = &hid.write_failure {
            if contains_privacy_signal(write_failure.message.as_str()) {
                return Err(SemanticError::HostileString);
            }
        }
    }
    if let Some(failure) = &doc.failure {
        for link in &failure.errors {
            if contains_privacy_signal(link.message.as_str()) {
                return Err(SemanticError::HostileString);
            }
        }
    }
    Ok(())
}

fn is_valid_shareable_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 64
        && version
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '+' | '-'))
}

fn is_valid_shareable_commit(commit: &str) -> bool {
    commit == "unknown" || build_commit_known(commit)
}

fn is_valid_bcd_device(bcd_device: &str) -> bool {
    let Some((major, minor)) = bcd_device.split_once('.') else {
        return false;
    };
    !major.is_empty()
        && major.len() <= 4
        && minor.len() <= 4
        && !minor.is_empty()
        && major.chars().all(|ch| ch.is_ascii_digit())
        && minor.chars().all(|ch| ch.is_ascii_digit())
}

fn validate_shareable_hid_backend(backend: &HidBackendProvenance) -> Result<(), SemanticError> {
    if backend.backend != LINUX_HIDRAW_BACKEND_CONTRACT.backend {
        return Err(SemanticError::ShareableStringViolation);
    }
    if backend.expected_write_return_bytes != EXPECTED_TRANSPORT_RETURN_BYTES {
        return Err(SemanticError::InvalidHidBackend);
    }
    if backend.kernel_hidraw_doc_ref != KERNEL_HIDRAW_DOC_REF {
        return Err(SemanticError::ShareableStringViolation);
    }
    if backend.reviewed_hidapi_semantics_commit != REVIEWED_HIDAPI_EVIDENCE_COMMIT {
        return Err(SemanticError::ShareableStringViolation);
    }
    if contains_privacy_signal(&backend.backend)
        || contains_privacy_signal(&backend.kernel_hidraw_doc_ref)
        || contains_privacy_signal(&backend.reviewed_hidapi_semantics_commit)
    {
        return Err(SemanticError::HostileString);
    }
    Ok(())
}

pub fn current_build_provenance() -> BuildProvenance {
    BuildProvenance {
        version: env!("CARGO_PKG_VERSION").to_string(),
        commit: env!("THERMALWRITER_GIT_COMMIT").to_string(),
        dirty: env!("THERMALWRITER_GIT_DIRTY") == "1",
    }
}

pub fn build_commit_known(commit: &str) -> bool {
    commit.len() == 40 && commit.chars().all(|ch| ch.is_ascii_hexdigit()) && commit != "unknown"
}

pub fn sanitize_free_text(input: &str) -> SanitizedText {
    if contains_privacy_signal(input) {
        return SanitizedText {
            text: REDACTED.to_string(),
            provably_safe: false,
        };
    }
    SanitizedText {
        text: input.to_string(),
        provably_safe: true,
    }
}

fn contains_privacy_signal(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    const HOSTILE: &[&str] = &[
        "/sys/",
        "/dev/hidraw",
        "/dev/bus/usb",
        "iserial",
        "usbmon",
        "xhci",
        "ehci",
        "ohci",
        "pci",
        "topology",
        "/home/",
        "/users/",
        "busnum",
        "devnum",
        "serial=",
        "serial:",
        "serial ",
        "i serial",
        "bus=",
        "bus:",
        "address=",
        "address:",
        "addr=",
        "addr:",
        "user=",
        "username=",
        "uid=",
    ];
    if HOSTILE.iter().any(|pattern| lower.contains(pattern)) {
        return true;
    }
    if lower.contains("bus ") && lower.contains("addr") {
        return true;
    }
    false
}

impl ReportDocument {
    fn canonicalize(&mut self) {
        if let Some(fingerprint) = &mut self.fingerprint {
            fingerprint
                .interfaces
                .sort_by_key(|iface| (iface.number, iface.alternate_setting));
            for iface in &mut fingerprint.interfaces {
                iface.endpoints.sort_by_key(|ep| ep.address);
            }
        }
    }
}

impl ReportFingerprint {
    fn from_usb_fingerprint(
        fingerprint: &UsbFingerprint,
        serial_present: bool,
        hidraw_correlated: Option<bool>,
    ) -> Self {
        let mut interfaces = fingerprint
            .interfaces
            .iter()
            .map(ReportInterfaceShape::from)
            .collect::<Vec<_>>();
        interfaces.sort_by_key(|iface| (iface.number, iface.alternate_setting));
        for iface in &mut interfaces {
            iface.endpoints.sort_by_key(|ep| ep.address);
        }
        Self {
            vid: fingerprint.vid,
            pid: fingerprint.pid,
            bcd_device: fingerprint.bcd_device.clone(),
            serial_present,
            hidraw_correlated,
            interfaces,
        }
    }

    fn to_usb_fingerprint(&self) -> UsbFingerprint {
        UsbFingerprint {
            vid: self.vid,
            pid: self.pid,
            bcd_device: self.bcd_device.clone(),
            interfaces: self
                .interfaces
                .iter()
                .map(|iface| UsbInterfaceShape {
                    number: iface.number,
                    alternate_setting: iface.alternate_setting,
                    class: iface.class,
                    subclass: iface.subclass,
                    protocol: iface.protocol,
                    endpoints: iface
                        .endpoints
                        .iter()
                        .map(|ep| UsbEndpointCapability {
                            address: ep.address,
                            direction: ep.direction,
                            transfer: ep.transfer,
                            max_packet_size: ep.max_packet_size,
                            interval: ep.interval,
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

impl From<&UsbInterfaceShape> for ReportInterfaceShape {
    fn from(shape: &UsbInterfaceShape) -> Self {
        let mut endpoints = shape
            .endpoints
            .iter()
            .map(ReportEndpointCapability::from)
            .collect::<Vec<_>>();
        endpoints.sort_by_key(|ep| ep.address);
        Self {
            number: shape.number,
            alternate_setting: shape.alternate_setting,
            class: shape.class,
            subclass: shape.subclass,
            protocol: shape.protocol,
            endpoints,
        }
    }
}

impl From<&UsbEndpointCapability> for ReportEndpointCapability {
    fn from(endpoint: &UsbEndpointCapability) -> Self {
        Self {
            address: endpoint.address,
            direction: endpoint.direction,
            transfer: endpoint.transfer,
            max_packet_size: endpoint.max_packet_size,
            interval: endpoint.interval,
        }
    }
}

impl From<Type2PreHandshakePolicy> for ReportPreHandshakePolicy {
    fn from(policy: Type2PreHandshakePolicy) -> Self {
        match policy {
            Type2PreHandshakePolicy::LegacyBulkInit => Self::LegacyBulkInit,
            Type2PreHandshakePolicy::Hid407ReadOnlyProbe => Self::Hid407ReadOnlyProbe,
            Type2PreHandshakePolicy::StopUnsupportedShape => Self::StopUnsupportedShape,
        }
    }
}

impl From<HidOutputRoute> for NegotiatedOutputRoute {
    fn from(route: HidOutputRoute) -> Self {
        match route {
            HidOutputRoute::LegacyBulk => Self::LegacyBulk,
            HidOutputRoute::HidReport => Self::HidReport,
            HidOutputRoute::Interrupt => Self::HidInterrupt,
        }
    }
}

impl From<WireProtocol> for ProtocolFamily {
    fn from(protocol: WireProtocol) -> Self {
        match protocol {
            WireProtocol::Bulk => Self::Bulk,
            WireProtocol::Scsi => Self::Scsi,
            WireProtocol::HidType2 => Self::HidType2,
            WireProtocol::HidType3 => Self::HidType3,
            WireProtocol::Ly => Self::Ly,
        }
    }
}

impl From<HidReportBackendContract> for HidBackendProvenance {
    fn from(contract: HidReportBackendContract) -> Self {
        Self {
            backend: contract.backend.to_string(),
            runtime_route: RuntimeBackendRoute::KernelManagedHidraw,
            expected_write_return_bytes: contract.expected_write_return_bytes,
            kernel_hidraw_doc_ref: contract.kernel_hidraw_doc_ref.to_string(),
            reviewed_hidapi_semantics_commit: contract.reviewed_hidapi_evidence_commit.to_string(),
        }
    }
}

impl From<&HidReadObservation> for HidReadEvidence {
    fn from(observation: &HidReadObservation) -> Self {
        Self {
            read_capacity_bytes: observation.read_capacity_bytes,
            read_timeout_ms: observation.read_timeout_ms,
            transport_return_bytes: Some(observation.transport_return_bytes),
            protocol_response_bytes: observation.protocol_response_bytes,
        }
    }
}

impl HidReportEvidence {
    fn empty_with_backend(contract: HidReportBackendContract) -> Self {
        Self {
            descriptor_status: DescriptorCaptureStatus::Unknown,
            logical_output_report_bytes: None,
            report_id: None,
            protocol_chunk_bytes: None,
            userspace_submit_bytes: None,
            transport_return_bytes: None,
            endpoint_max_packet_size: None,
            active_write_authorized: None,
            backend: HidBackendProvenance::from(contract),
            read: None,
            read_failure: None,
            write_failure: None,
        }
    }
}

impl NegotiatedProfile {
    fn from_type2(
        observation: &Type2NegotiatedObservation,
        device_info: &DeviceInfo,
    ) -> Result<Self> {
        ensure!(
            device_info.pm == observation.pm() && device_info.sub == observation.sub(),
            "DeviceInfo PM/SUB mismatch with Type2 observation"
        );
        let policy = observation.policy();
        let native = DisplayDimensions {
            width: device_info.profile.width,
            height: device_info.profile.height,
        };
        let wire = if policy.portrait_native() {
            DisplayDimensions {
                width: device_info.profile.width,
                height: device_info.profile.height,
            }
        } else {
            native
        };
        let profile_policy = profile_policy_label(policy, observation.pm(), observation.sub());
        validate_negotiated_dimensions(observation.pm(), device_info.fbl, &wire)?;
        Ok(Self {
            response_bytes: observation.response().len(),
            protocol_family: ProtocolFamily::from(device_info.protocol),
            pm: observation.pm(),
            sub: observation.sub(),
            fbl: device_info.fbl,
            native_dimensions: native,
            wire_dimensions: wire,
            profile_policy,
            negotiated_output_route: policy.output().map(NegotiatedOutputRoute::from),
            active_writes_allowed: policy.active_writes_allowed(),
            keep_single_session: policy.keep_single_session(),
            portrait_native: Some(policy.portrait_native()),
            rotate_panel: None,
        })
    }

    fn from_device_info(device_info: &DeviceInfo, response_bytes: usize) -> Result<Self> {
        let route = negotiated_route_for_protocol(device_info.protocol)?;
        let native = DisplayDimensions {
            width: device_info.width(),
            height: device_info.height(),
        };
        let (wire_w, wire_h) = device_info.wire_dimensions()?;
        let wire = DisplayDimensions {
            width: wire_w,
            height: wire_h,
        };
        let profile_policy = match device_info.protocol {
            WireProtocol::Bulk => ProfilePolicyLabel::LegacyBulk,
            WireProtocol::Scsi | WireProtocol::HidType3 | WireProtocol::Ly => {
                ProfilePolicyLabel::ActivePmSub {
                    pm: device_info.pm,
                    sub: device_info.sub,
                }
            }
            WireProtocol::HidType2 => bail!("use record_negotiated_type2 for HID Type2"),
        };
        Ok(Self {
            response_bytes,
            protocol_family: ProtocolFamily::from(device_info.protocol),
            pm: device_info.pm,
            sub: device_info.sub,
            fbl: device_info.fbl,
            native_dimensions: native,
            wire_dimensions: wire,
            profile_policy,
            negotiated_output_route: Some(route),
            active_writes_allowed: true,
            keep_single_session: false,
            portrait_native: None,
            rotate_panel: Some(device_info.profile.rotate_panel),
        })
    }
}

fn negotiated_route_for_protocol(protocol: WireProtocol) -> Result<NegotiatedOutputRoute> {
    match protocol {
        WireProtocol::Bulk | WireProtocol::HidType3 | WireProtocol::Ly => {
            Ok(NegotiatedOutputRoute::LegacyBulk)
        }
        WireProtocol::Scsi => Ok(NegotiatedOutputRoute::ScsiCommand),
        WireProtocol::HidType2 => bail!("use record_negotiated_type2 for HID Type2"),
    }
}

fn validate_negotiated_dimensions(pm: u8, fbl: u8, wire: &DisplayDimensions) -> Result<()> {
    match (pm, fbl) {
        (58, 58) => ensure!(
            wire.width == 240 && wire.height == 320,
            "PM58/FBL58 must report wire dimensions 240x320, got {}x{}",
            wire.width,
            wire.height
        ),
        (68, 192) => ensure!(
            wire.width == 1280 && wire.height == 480,
            "PM68/FBL192 must report wire dimensions 1280x480, got {}x{}",
            wire.width,
            wire.height
        ),
        _ => {}
    }
    Ok(())
}

fn profile_policy_label(policy: Type2NegotiatedPolicy, pm: u8, sub: u8) -> ProfilePolicyLabel {
    if policy.active_writes_allowed() {
        if policy.output() == Some(HidOutputRoute::HidReport) && pm == 58 && sub == 0 {
            return ProfilePolicyLabel::UpstreamPm58_407;
        }
        if policy.output() == Some(HidOutputRoute::LegacyBulk) {
            return ProfilePolicyLabel::LegacyBulk;
        }
        return ProfilePolicyLabel::ActivePmSub { pm, sub };
    }
    if pm == 68 {
        return ProfilePolicyLabel::ObservedPm68ConservativeStop;
    }
    ProfilePolicyLabel::ObservedInactive
}

impl ValidationFailure {
    fn from_typed_messages(
        stage: ValidationStage,
        errors: &[(ValidationErrorKind, &str)],
    ) -> (Self, bool) {
        let mut safe = true;
        let errors = errors
            .iter()
            .map(|(kind, message)| {
                let (message, provably_safe) = SafeMessage::from_raw(message);
                if !provably_safe {
                    safe = false;
                }
                ValidationErrorLink {
                    kind: *kind,
                    message,
                }
            })
            .collect();
        (Self { stage, errors }, safe)
    }
}

impl HidWriteFailureEvidence {
    fn from_failure(failure: &HidChunkedWriteFailure) -> (Self, bool) {
        let (error_kind, raw_message, failing_chunk) = match &failure.error {
            HidReportWriteError::NegativeReturn { observation, .. } => (
                HidWriteErrorKind::NegativeReturn,
                failure.error.to_string(),
                Some(HidWriteChunkEvidence::from_observation(observation)),
            ),
            HidReportWriteError::UnexpectedCount(err) => (
                HidWriteErrorKind::UnexpectedCount,
                err.to_string(),
                Some(HidWriteChunkEvidence::from_observation(&err.observation)),
            ),
            HidReportWriteError::Transport {
                message,
                observation,
            } => (
                HidWriteErrorKind::Transport,
                format!("HID report write transport error: {message}"),
                Some(HidWriteChunkEvidence {
                    protocol_chunk_bytes: observation.protocol_chunk_bytes,
                    logical_output_report_bytes: observation.logical_output_report_bytes,
                    report_id: observation.report_id,
                    userspace_submit_bytes: observation.userspace_submit_bytes,
                    transport_return_bytes: None,
                    endpoint_max_packet_size: observation.endpoint_max_packet_size,
                }),
            ),
            HidReportWriteError::SessionStopped => (
                HidWriteErrorKind::SessionStopped,
                failure.error.to_string(),
                None,
            ),
        };
        let (message, provably_safe) = SafeMessage::from_raw(&raw_message);
        (
            Self {
                completed_chunks: failure
                    .completed
                    .iter()
                    .map(HidWriteChunkEvidence::from_observation)
                    .collect(),
                failing_chunk,
                error_kind,
                message,
            },
            provably_safe,
        )
    }
}

impl HidWriteChunkEvidence {
    fn from_observation(observation: &super::hid_report::HidWriteObservation) -> Self {
        Self {
            protocol_chunk_bytes: observation.protocol_chunk_bytes,
            logical_output_report_bytes: observation.logical_output_report_bytes,
            report_id: observation.report_id,
            userspace_submit_bytes: observation.userspace_submit_bytes,
            transport_return_bytes: Some(observation.transport_return_bytes),
            endpoint_max_packet_size: observation.endpoint_max_packet_size,
        }
    }
}

mod hex_u16 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &u16, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{value:04x}"))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u16, D::Error> {
        let value = String::deserialize(deserializer)?;
        u16::from_str_radix(value.trim_start_matches("0x"), 16).map_err(serde::de::Error::custom)
    }
}

mod hex_u8 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &u8, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("0x{value:02x}"))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
        let value = String::deserialize(deserializer)?;
        u8::from_str_radix(value.trim_start_matches("0x"), 16).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for HardwareValidationReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "hardware-validation-report(schema={}, scope={:?})",
            self.doc.schema, self.doc.scope
        )
    }
}

// Public read accessors for shareable report sections used in tests/downstream.
impl ReportFingerprint {
    pub fn vid(&self) -> u16 {
        self.vid
    }

    pub fn pid(&self) -> u16 {
        self.pid
    }

    pub fn serial_present(&self) -> bool {
        self.serial_present
    }

    pub fn interfaces(&self) -> &[ReportInterfaceShape] {
        &self.interfaces
    }
}

impl ReportInterfaceShape {
    pub fn endpoints(&self) -> &[ReportEndpointCapability] {
        &self.endpoints
    }
}

impl ReportEndpointCapability {
    pub fn direction(&self) -> super::usb_fingerprint::UsbDirection {
        self.direction
    }

    pub fn max_packet_size(&self) -> u16 {
        self.max_packet_size
    }
}

impl NegotiatedProfile {
    pub fn pm(&self) -> u8 {
        self.pm
    }

    pub fn fbl(&self) -> u8 {
        self.fbl
    }

    pub fn profile_policy(&self) -> ProfilePolicyLabel {
        self.profile_policy
    }

    pub fn wire_dimensions(&self) -> DisplayDimensions {
        self.wire_dimensions
    }

    pub fn negotiated_output_route(&self) -> Option<NegotiatedOutputRoute> {
        self.negotiated_output_route
    }

    pub fn active_writes_allowed(&self) -> bool {
        self.active_writes_allowed
    }

    pub fn portrait_native(&self) -> Option<bool> {
        self.portrait_native
    }

    pub fn rotate_panel(&self) -> Option<bool> {
        self.rotate_panel
    }
}

impl ValidationFailure {
    pub fn errors(&self) -> &[ValidationErrorLink] {
        &self.errors
    }
}

impl ValidationErrorLink {
    pub fn message(&self) -> &str {
        self.message.as_str()
    }

    pub fn kind(&self) -> ValidationErrorKind {
        self.kind
    }
}

impl BuildProvenance {
    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn commit(&self) -> &str {
        &self.commit
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }
}

impl HidReportEvidence {
    pub fn descriptor_status(&self) -> DescriptorCaptureStatus {
        self.descriptor_status
    }

    pub fn runtime_route(&self) -> RuntimeBackendRoute {
        self.backend.runtime_route
    }

    pub fn read(&self) -> Option<&HidReadEvidence> {
        self.read.as_ref()
    }

    pub fn write_failure(&self) -> Option<&HidWriteFailureEvidence> {
        self.write_failure.as_ref()
    }
}

impl HidReadEvidence {
    pub fn transport_return_bytes(&self) -> Option<isize> {
        self.transport_return_bytes
    }

    pub fn read_capacity_bytes(&self) -> usize {
        self.read_capacity_bytes
    }
}

impl HidWriteFailureEvidence {
    pub fn completed_chunks(&self) -> &[HidWriteChunkEvidence] {
        &self.completed_chunks
    }

    pub fn failing_chunk(&self) -> Option<&HidWriteChunkEvidence> {
        self.failing_chunk.as_ref()
    }

    pub fn message(&self) -> &str {
        self.message.as_str()
    }
}

impl HidWriteChunkEvidence {
    pub fn transport_return_bytes(&self) -> Option<isize> {
        self.transport_return_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::hid_report::HidWriteObservation;
    use crate::transport::type2_policy::{
        Type2PreHandshakePolicy, WINBOND_HID2_PID, WINBOND_HID2_VID, negotiate_type2_policy,
    };
    use crate::transport::usb_fingerprint::{
        UsbDirection, UsbEndpointCapability, UsbInterfaceShape, UsbTransferKind,
    };

    const KNOWN_COMMIT: &str = "cccccccccccccccccccccccccccccccccccccccc";

    #[cfg(test)]
    fn with_clean_build(
        mut report: HardwareValidationReport,
        commit: &str,
    ) -> HardwareValidationReport {
        report.doc.build.commit = commit.to_string();
        report.doc.build.dirty = false;
        report
    }

    #[test]
    fn from_toml_rejects_unknown_root_field() {
        let input = passive_physical_report()
            .to_private_toml()
            .expect("serialize");
        let input = format!("unknown_field = true\n{input}");
        let error = HardwareValidationReport::from_toml(&input).unwrap_err();
        assert!(error.to_string().contains("unknown report field"));
    }

    fn passive_physical_report() -> HardwareValidationReport {
        let mut report = HardwareValidationReport::new_in_progress(
            EvidenceOrigin::Physical,
            ValidationScope::Passive,
        );
        report
            .set_fingerprint(&hid_in_fingerprint(), false, Some(true))
            .expect("fingerprint");
        report
            .set_pre_handshake_policy(Type2PreHandshakePolicy::Hid407ReadOnlyProbe)
            .expect("policy");
        report
            .record_check(CheckField::Enumerated, CheckStatus::Pass)
            .expect("check");
        report
            .record_check(CheckField::PassiveAllowlist, CheckStatus::Pass)
            .expect("check");
        report.finalize_passive_pass().expect("passive pass");
        report
    }

    fn full_physical_pass_report() -> HardwareValidationReport {
        let report = full_physical_pass_report_parts();
        let mut report = with_clean_build(report, KNOWN_COMMIT);
        report.finalize_full_pass().expect("full pass");
        report
    }

    fn full_physical_pass_report_parts() -> HardwareValidationReport {
        let obs = negotiate_type2_policy(
            WINBOND_HID2_VID,
            WINBOND_HID2_PID,
            &short_pm58_response(),
            Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
        )
        .expect("pm58");

        let mut report = HardwareValidationReport::new_in_progress(
            EvidenceOrigin::Physical,
            ValidationScope::Full,
        );
        report
            .set_fingerprint(&hid_in_fingerprint(), false, Some(true))
            .expect("fingerprint");
        report
            .set_pre_handshake_policy(Type2PreHandshakePolicy::Hid407ReadOnlyProbe)
            .expect("policy");
        report.record_negotiated_type2(&obs).expect("negotiated");
        report
            .set_hid_backend_contract(LINUX_HIDRAW_BACKEND_CONTRACT)
            .expect("backend");
        report
            .set_hid_descriptor_status(DescriptorCaptureStatus::Captured)
            .expect("descriptor");
        report
            .record_hid_read(&HidReadObservation {
                read_capacity_bytes: 512,
                read_timeout_ms: 500,
                transport_return_bytes: 8,
                protocol_response_bytes: 8,
            })
            .expect("read");
        report
            .record_hid_write_observation(512, Some(512), 0, 513, Some(513), Some(8))
            .expect("write");
        report
            .set_hid_active_write_authorized(true)
            .expect("authorized");
        for field in [
            CheckField::Enumerated,
            CheckField::PassiveAllowlist,
            CheckField::ExclusiveOwner,
            CheckField::Handshake,
            CheckField::ActiveWrite,
            CheckField::TargetMarker,
            CheckField::SecondDisplayUnchanged,
            CheckField::Orientation,
            CheckField::Colors,
            CheckField::Soak,
            CheckField::Reconnect,
            CheckField::DaemonRestored,
        ] {
            report
                .record_check(field, CheckStatus::Pass)
                .expect("check");
        }
        report
    }

    fn short_pm58_response() -> Vec<u8> {
        vec![0xDA, 0xDB, 0xDC, 0xDD, 0x00, 0x3A, 0x00, 0x00]
    }

    fn hid_in_fingerprint() -> UsbFingerprint {
        UsbFingerprint {
            vid: 0x0416,
            pid: 0x5302,
            bcd_device: "4.07".to_string(),
            interfaces: vec![
                UsbInterfaceShape {
                    number: 0,
                    alternate_setting: 0,
                    class: 3,
                    subclass: 0,
                    protocol: 0,
                    endpoints: vec![
                        UsbEndpointCapability {
                            address: 0x83,
                            direction: UsbDirection::In,
                            transfer: UsbTransferKind::Interrupt,
                            max_packet_size: 8,
                            interval: 1,
                        },
                        UsbEndpointCapability {
                            address: 0x02,
                            direction: UsbDirection::Out,
                            transfer: UsbTransferKind::Interrupt,
                            max_packet_size: 512,
                            interval: 1,
                        },
                    ],
                },
                UsbInterfaceShape {
                    number: 1,
                    alternate_setting: 0,
                    class: 255,
                    subclass: 255,
                    protocol: 255,
                    endpoints: vec![],
                },
            ],
        }
    }

    #[test]
    fn absent_check_is_not_pass() {
        let checks = ValidationChecks::default();
        assert!(!checks.passed(CheckField::Handshake));
        assert_eq!(checks.get(CheckField::Handshake), None);
    }

    #[test]
    fn sanitize_hostile_paths_fully_redacts() {
        let outcome = sanitize_free_text(
            "opened /dev/hidraw3 on busnum=1 devnum=4 serial=ABC /home/mike/sys/class/hidraw/hidraw3 bus=1 address=/dev/foo user=alice uid=1000",
        );
        assert!(!outcome.provably_safe);
        assert_eq!(outcome.text, REDACTED);
    }

    #[test]
    fn privacy_detector_matches_assignment_forms() {
        let outcome =
            sanitize_free_text("bus=2 address=7 addr:7 user=mike username=alice uid=1000");
        assert!(!outcome.provably_safe);
        assert_eq!(outcome.text, REDACTED);
    }

    #[test]
    fn benign_error_stays_shareable() {
        let outcome = sanitize_free_text("unexpected HID write count: submitted=513 returned=8");
        assert!(outcome.provably_safe);
        assert!(outcome.text.contains("returned=8"));
    }

    #[test]
    fn build_commit_known_rejects_unknown() {
        assert!(!build_commit_known("unknown"));
        assert!(build_commit_known(KNOWN_COMMIT));
    }

    #[test]
    fn passive_finalize_never_tested_eligible() {
        let report = passive_physical_report();
        assert!(!report.eligible_for_tested());
    }

    #[test]
    fn eligible_for_tested_only_on_complete_clean_physical_full_pass() {
        let report = full_physical_pass_report();
        assert!(report.eligible_for_tested());
        assert!(report.to_shareable_toml().is_ok());
    }

    #[test]
    fn mutation_after_finalize_rejected() {
        let report = passive_physical_report();
        let mut report = report;
        assert_eq!(
            report
                .record_check(CheckField::Enumerated, CheckStatus::Pass)
                .unwrap_err(),
            ReportMutationError::AlreadyFinalized
        );
    }

    #[test]
    fn completed_report_with_contradictory_failure_rejected_on_load() {
        let mut toml = passive_physical_report()
            .to_private_toml()
            .expect("serialize");
        toml = toml.replace("result = \"pass\"", "result = \"fail\"");
        let error = HardwareValidationReport::from_toml(&toml).unwrap_err();
        assert!(error.to_string().contains("semantic validation"));
    }

    #[test]
    fn physical_full_pass_rejects_short_write_return() {
        let mut report = full_physical_pass_report_parts();
        report
            .record_hid_write_observation(512, Some(512), 0, 513, Some(8), Some(8))
            .expect("write");
        let mut report = with_clean_build(report, KNOWN_COMMIT);
        assert_eq!(
            report.finalize_full_pass().unwrap_err(),
            FinalizeError::InvariantViolation
        );
        assert!(report.result().is_none());
    }

    #[test]
    fn physical_full_pass_rejects_missing_transport_return() {
        let mut report = full_physical_pass_report_parts();
        report
            .record_hid_write_observation(512, Some(512), 0, 513, None, Some(8))
            .expect("write");
        let mut report = with_clean_build(report, KNOWN_COMMIT);
        assert_eq!(
            report.finalize_full_pass().unwrap_err(),
            FinalizeError::InvariantViolation
        );
        assert!(report.result().is_none());
    }

    #[test]
    fn physical_full_pass_rejects_read_transport_protocol_mismatch() {
        let mut report = full_physical_pass_report_parts();
        report
            .record_hid_read(&HidReadObservation {
                read_capacity_bytes: 512,
                read_timeout_ms: 500,
                transport_return_bytes: 9,
                protocol_response_bytes: 8,
            })
            .expect("read");
        let mut report = with_clean_build(report, KNOWN_COMMIT);
        assert_eq!(
            report.finalize_full_pass().unwrap_err(),
            FinalizeError::InvariantViolation
        );
        assert!(report.result().is_none());
    }

    #[test]
    fn physical_full_pass_rejects_read_protocol_exceeds_capacity() {
        let mut report = full_physical_pass_report_parts();
        report
            .record_hid_read(&HidReadObservation {
                read_capacity_bytes: 8,
                read_timeout_ms: 500,
                transport_return_bytes: 512,
                protocol_response_bytes: 512,
            })
            .expect("read");
        let mut report = with_clean_build(report, KNOWN_COMMIT);
        assert_eq!(
            report.finalize_full_pass().unwrap_err(),
            FinalizeError::InvariantViolation
        );
        assert!(report.result().is_none());
    }

    #[test]
    fn physical_full_pass_rejects_malformed_build_version_on_finalize() {
        let report = full_physical_pass_report_parts();
        let mut toml = with_clean_build(report, KNOWN_COMMIT)
            .to_private_toml()
            .expect("serialize");
        toml = toml.replace("version = \"0.1.4\"", "version = \"/tmp/leak\"");
        let mut report = HardwareValidationReport::from_toml(&toml).expect("parse");
        assert_eq!(
            report.finalize_full_pass().unwrap_err(),
            FinalizeError::NotShareable
        );
        assert!(report.result().is_none());
    }

    #[test]
    fn from_toml_rejects_tampered_backend_expected_return() {
        let report = full_physical_pass_report();
        let mut toml = report.to_private_toml().expect("serialize");
        toml = toml.replace(
            "expected_write_return_bytes = 513",
            "expected_write_return_bytes = 8",
        );
        let error = HardwareValidationReport::from_toml(&toml).unwrap_err();
        assert!(error.to_string().contains("semantic validation"));
    }

    #[test]
    fn physical_full_pass_rejects_read_failure_evidence() {
        let mut report = full_physical_pass_report_parts();
        report
            .record_hid_read_failure(
                512,
                500,
                Some(8),
                HidReadErrorKind::ShortCount,
                "short read",
            )
            .expect("read failure");
        let mut report = with_clean_build(report, KNOWN_COMMIT);
        assert_eq!(
            report.finalize_full_pass().unwrap_err(),
            FinalizeError::InvariantViolation
        );
        assert!(report.result().is_none());
    }

    #[test]
    fn transport_write_failure_omits_failing_chunk_return() {
        let failure = HidChunkedWriteFailure {
            completed: vec![],
            error: HidReportWriteError::Transport {
                message: "EIO".to_string(),
                observation: HidWriteObservation {
                    protocol_chunk_bytes: PROTOCOL_CHUNK_BYTES,
                    logical_output_report_bytes: Some(PROTOCOL_CHUNK_BYTES),
                    report_id: REPORT_ID_UNNUMBERED,
                    userspace_submit_bytes: USERSPACE_SUBMIT_BYTES,
                    transport_return_bytes: 0,
                    endpoint_max_packet_size: Some(8),
                },
            },
        };
        let (evidence, _) = HidWriteFailureEvidence::from_failure(&failure);
        assert_eq!(
            evidence
                .failing_chunk
                .as_ref()
                .unwrap()
                .transport_return_bytes,
            None
        );
    }
}
