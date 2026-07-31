// SPDX-License-Identifier: GPL-3.0-or-later
//
// Sanitized hardware-validation report schema for incremental CLI/cleanup workflows.

use std::fmt;

use anyhow::{Context, Result, ensure};
use serde::de::Error as DeError;
use serde::{Deserialize, Serialize};

use super::hid_report::{
    HidChunkedWriteFailure, HidReadObservation, HidReportBackendContract, HidReportWriteError,
    LINUX_HIDRAW_BACKEND_CONTRACT,
};
use super::profile::{DeviceInfo, WireProtocol, build_device_info};
use super::type2_policy::{
    HidOutputRoute, Type2NegotiatedObservation, Type2NegotiatedPolicy, Type2PreHandshakePolicy,
};
use super::usb_fingerprint::{UsbEndpointCapability, UsbFingerprint, UsbInterfaceShape};

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckField {
    Enumerated,
    PassiveAllowlist,
    ExclusiveOwner,
    Handshake,
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
            CheckField::ExclusiveOwner,
            CheckField::Handshake,
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
    transport_return_bytes: isize,
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
    portrait_native: bool,
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

/// Sanitized free-form text; only constructible via [`sanitize_free_text`] or safe constants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
        }
    }
}

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

    pub fn set_fingerprint(
        &mut self,
        fingerprint: &UsbFingerprint,
        serial_present: bool,
        hidraw_correlated: Option<bool>,
    ) {
        self.doc.fingerprint = Some(ReportFingerprint::from_usb_fingerprint(
            fingerprint,
            serial_present,
            hidraw_correlated,
        ));
    }

    pub fn set_pre_handshake_policy(&mut self, policy: Type2PreHandshakePolicy) {
        self.doc.pre_handshake_policy = Some(ReportPreHandshakePolicy::from(policy));
    }

    pub fn record_check(&mut self, field: CheckField, status: CheckStatus) {
        self.doc.checks.set(field, status);
    }

    pub fn record_negotiated_type2(
        &mut self,
        observation: &Type2NegotiatedObservation,
    ) -> Result<()> {
        let device_info = build_device_info(
            WireProtocol::HidType2,
            WINBOND_HID2_VID,
            WINBOND_HID2_PID,
            observation.pm(),
            observation.sub(),
            None,
        )
        .context("derive negotiated DeviceInfo from Type2 observation")?;
        self.doc.negotiated = Some(NegotiatedProfile::from_type2(observation, &device_info)?);
        Ok(())
    }

    pub fn record_negotiated_device(
        &mut self,
        device_info: &DeviceInfo,
        observation: &Type2NegotiatedObservation,
    ) -> Result<()> {
        ensure!(
            device_info.pm == observation.pm() && device_info.sub == observation.sub(),
            "DeviceInfo PM/SUB does not match negotiated observation"
        );
        self.doc.negotiated = Some(NegotiatedProfile::from_type2(observation, device_info)?);
        Ok(())
    }

    pub fn set_hid_backend_contract(&mut self, contract: HidReportBackendContract) {
        let evidence = self
            .doc
            .hid_report
            .get_or_insert_with(|| HidReportEvidence::empty_with_backend(contract));
        evidence.backend = HidBackendProvenance::from(contract);
    }

    pub fn record_hid_read(&mut self, observation: &HidReadObservation) {
        let evidence = self.doc.hid_report.get_or_insert_with(|| {
            HidReportEvidence::empty_with_backend(LINUX_HIDRAW_BACKEND_CONTRACT)
        });
        evidence.read = Some(HidReadEvidence::from(observation));
    }

    pub fn record_hid_read_failure(
        &mut self,
        read_capacity_bytes: usize,
        read_timeout_ms: u32,
        transport_return_bytes: Option<isize>,
        error_kind: HidReadErrorKind,
        message: &str,
    ) {
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
    }

    pub fn set_hid_descriptor_status(&mut self, status: DescriptorCaptureStatus) {
        let evidence = self.doc.hid_report.get_or_insert_with(|| {
            HidReportEvidence::empty_with_backend(LINUX_HIDRAW_BACKEND_CONTRACT)
        });
        evidence.descriptor_status = status;
    }

    pub fn set_hid_active_write_authorized(&mut self, authorized: bool) {
        let evidence = self.doc.hid_report.get_or_insert_with(|| {
            HidReportEvidence::empty_with_backend(LINUX_HIDRAW_BACKEND_CONTRACT)
        });
        evidence.active_write_authorized = Some(authorized);
    }

    pub fn record_hid_write_observation(
        &mut self,
        protocol_chunk_bytes: usize,
        logical_output_report_bytes: Option<usize>,
        report_id: u8,
        userspace_submit_bytes: usize,
        transport_return_bytes: isize,
        endpoint_max_packet_size: Option<u16>,
    ) {
        let evidence = self.doc.hid_report.get_or_insert_with(|| {
            HidReportEvidence::empty_with_backend(LINUX_HIDRAW_BACKEND_CONTRACT)
        });
        evidence.protocol_chunk_bytes = Some(protocol_chunk_bytes);
        evidence.logical_output_report_bytes = logical_output_report_bytes;
        evidence.report_id = Some(report_id);
        evidence.userspace_submit_bytes = Some(userspace_submit_bytes);
        evidence.transport_return_bytes = Some(transport_return_bytes);
        evidence.endpoint_max_packet_size = endpoint_max_packet_size;
    }

    pub fn record_hid_chunked_write_failure(&mut self, failure: &HidChunkedWriteFailure) {
        let evidence = self.doc.hid_report.get_or_insert_with(|| {
            HidReportEvidence::empty_with_backend(LINUX_HIDRAW_BACKEND_CONTRACT)
        });
        let (write_failure, safe) = HidWriteFailureEvidence::from_failure(failure);
        evidence.write_failure = Some(write_failure);
        if !safe {
            self.mark_redacted();
        }
    }

    pub fn fail_at(&mut self, stage: ValidationStage, errors: &[&str]) {
        self.doc.result = Some(ValidationResult::Fail);
        self.doc.failed_step = Some(stage);
        let (failure, safe) = ValidationFailure::from_messages(stage, errors);
        self.doc.failure = Some(failure);
        if !safe {
            self.mark_redacted();
        }
    }

    pub fn abort_at(&mut self, stage: ValidationStage, errors: &[&str]) {
        self.doc.result = Some(ValidationResult::Aborted);
        self.doc.failed_step = Some(stage);
        let (failure, safe) = ValidationFailure::from_messages(stage, errors);
        self.doc.failure = Some(failure);
        if !safe {
            self.mark_redacted();
        }
    }

    pub fn finalize_passive_pass(&mut self) -> Result<(), FinalizeError> {
        ensure_scope(self, ValidationScope::Passive)?;
        if self.doc.result.is_some() {
            return Err(FinalizeError::PriorFailure);
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
        self.doc.result = Some(ValidationResult::Pass);
        Ok(())
    }

    pub fn finalize_full_pass(&mut self) -> Result<(), FinalizeError> {
        ensure_scope(self, ValidationScope::Full)?;
        if self.doc.origin != EvidenceOrigin::Physical {
            return Err(FinalizeError::WrongOrigin);
        }
        if self.doc.result.is_some() {
            return Err(FinalizeError::PriorFailure);
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
        if !build_commit_known(&self.doc.build.commit) {
            return Err(FinalizeError::UnknownCommit);
        }
        if self.doc.build.dirty {
            return Err(FinalizeError::UncleanBuild);
        }
        if !self.shareable() {
            return Err(FinalizeError::NotShareable);
        }
        self.doc.result = Some(ValidationResult::Pass);
        Ok(())
    }

    /// Recompute Tested-badge eligibility; never trust a stored flag.
    pub fn eligible_for_tested(&self) -> bool {
        self.doc.origin == EvidenceOrigin::Physical
            && self.doc.scope == ValidationScope::Full
            && matches!(self.doc.result, Some(ValidationResult::Pass))
            && self.doc.failed_step.is_none()
            && self.doc.failure.is_none()
            && self.doc.fingerprint.is_some()
            && self.doc.negotiated.is_some()
            && self
                .doc
                .checks
                .all_mandatory_pass(ValidationChecks::mandatory_full_fields())
            && build_commit_known(&self.doc.build.commit)
            && !self.doc.build.dirty
            && self.shareable()
            && self.doc.shareable
            && self.text_fields_are_safe()
    }

    pub fn to_private_toml(&self) -> Result<String, toml::ser::Error> {
        let mut doc = self.doc.clone();
        doc.canonicalize();
        toml::to_string_pretty(&doc)
    }

    pub fn to_shareable_toml(&self) -> Result<String> {
        self.validate_shareable()?;
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
        Ok(Self {
            doc,
            redaction_permanent: false,
        })
    }

    /// Test-only mutation hooks for eligibility fixtures (integration tests).
    #[doc(hidden)]
    pub fn doc_result_override_for_test(&mut self, result: Option<ValidationResult>) {
        self.doc.result = result;
    }

    #[doc(hidden)]
    pub fn doc_checks_clear_for_test(&mut self) {
        self.doc.checks = ValidationChecks::default();
    }

    #[doc(hidden)]
    pub fn doc_build_dirty_for_test(&mut self, dirty: bool) {
        self.doc.build.dirty = dirty;
    }

    #[doc(hidden)]
    pub fn doc_build_commit_for_test(&mut self, commit: &str) {
        self.doc.build.commit = commit.to_string();
    }

    fn validate_shareable(&self) -> Result<()> {
        ensure!(self.shareable(), "report is not shareable");
        ensure!(
            self.text_fields_are_safe(),
            "report contains unsafe free-form text"
        );
        if let Some(fingerprint) = &self.doc.fingerprint {
            ensure!(
                validate_shareable_fingerprint(fingerprint),
                "fingerprint contains unsafe values"
            );
        }
        Ok(())
    }

    fn mark_redacted(&mut self) {
        self.redaction_permanent = true;
        self.doc.shareable = false;
    }

    fn text_fields_are_safe(&self) -> bool {
        if let Some(failure) = &self.doc.failure {
            for link in &failure.errors {
                if contains_privacy_signal(link.message.as_str()) {
                    return false;
                }
            }
        }
        if let Some(hid) = &self.doc.hid_report {
            if let Some(write_failure) = &hid.write_failure {
                if contains_privacy_signal(write_failure.message.as_str()) {
                    return false;
                }
            }
            if let Some(read_failure) = &hid.read_failure {
                if contains_privacy_signal(read_failure.message.as_str()) {
                    return false;
                }
            }
        }
        true
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
    ];
    if HOSTILE.iter().any(|pattern| lower.contains(pattern)) {
        return true;
    }
    if lower.contains("bus ") && lower.contains("addr") {
        return true;
    }
    false
}

fn validate_shareable_fingerprint(fingerprint: &ReportFingerprint) -> bool {
    !contains_privacy_signal(&fingerprint.bcd_device)
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
                width: device_info.profile.height,
                height: device_info.profile.width,
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
            portrait_native: policy.portrait_native(),
        })
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
    fn from_messages(stage: ValidationStage, errors: &[&str]) -> (Self, bool) {
        let mut safe = true;
        let errors = errors
            .iter()
            .map(|message| {
                let (message, provably_safe) = SafeMessage::from_raw(message);
                if !provably_safe {
                    safe = false;
                }
                ValidationErrorLink {
                    kind: ValidationErrorKind::Error,
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
                Some(HidWriteChunkEvidence::from_observation(observation)),
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
            transport_return_bytes: observation.transport_return_bytes,
            endpoint_max_packet_size: observation.endpoint_max_packet_size,
        }
    }
}

use super::type2_policy::{WINBOND_HID2_PID, WINBOND_HID2_VID};

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
    pub fn transport_return_bytes(&self) -> isize {
        self.transport_return_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::type2_policy::Type2PreHandshakePolicy;
    use crate::transport::usb_fingerprint::{
        UsbDirection, UsbEndpointCapability, UsbInterfaceShape, UsbTransferKind,
    };

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
        report.set_fingerprint(&hid_in_fingerprint(), false, Some(true));
        report.set_pre_handshake_policy(Type2PreHandshakePolicy::Hid407ReadOnlyProbe);
        report.record_check(CheckField::Enumerated, CheckStatus::Pass);
        report.record_check(CheckField::PassiveAllowlist, CheckStatus::Pass);
        report.finalize_passive_pass().expect("passive pass");
        report
    }

    fn hid_in_fingerprint() -> UsbFingerprint {
        UsbFingerprint {
            vid: 0x0416,
            pid: 0x5302,
            bcd_device: "4.07".to_string(),
            interfaces: vec![UsbInterfaceShape {
                number: 0,
                alternate_setting: 0,
                class: 3,
                subclass: 0,
                protocol: 0,
                endpoints: vec![UsbEndpointCapability {
                    address: 0x81,
                    direction: UsbDirection::In,
                    transfer: UsbTransferKind::Interrupt,
                    max_packet_size: 8,
                    interval: 1,
                }],
            }],
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
            "opened /dev/hidraw3 on busnum=1 devnum=4 serial=ABC /home/mike/sys/class/hidraw/hidraw3",
        );
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
        assert!(build_commit_known(
            "655a1acff5c86ff0f9121f9fd4a0ea14bee35447"
        ));
    }

    #[test]
    fn passive_finalize_never_tested_eligible() {
        let mut report = HardwareValidationReport::new_in_progress(
            EvidenceOrigin::Physical,
            ValidationScope::Passive,
        );
        report.set_fingerprint(&hid_in_fingerprint(), false, Some(true));
        report.record_check(CheckField::Enumerated, CheckStatus::Pass);
        report.record_check(CheckField::PassiveAllowlist, CheckStatus::Pass);
        report.finalize_passive_pass().expect("passive pass");
        assert!(!report.eligible_for_tested());
    }
}
