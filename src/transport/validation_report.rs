// SPDX-License-Identifier: GPL-3.0-or-later
//
// Sanitized hardware-validation report schema for incremental CLI/cleanup workflows.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::hid_report::{
    HidChunkedWriteFailure, HidReadObservation, HidReportBackendContract, HidReportWriteError,
    HidWriteObservation, LINUX_HIDRAW_BACKEND_CONTRACT, REVIEWED_HIDAPI_EVIDENCE_COMMIT,
};
use super::profile::{DeviceInfo, WireProtocol};
use super::type2_policy::{
    HidOutputRoute, Type2NegotiatedObservation, Type2NegotiatedPolicy, Type2PreHandshakePolicy,
};
use super::usb_fingerprint::{UsbEndpointCapability, UsbFingerprint, UsbInterfaceShape};

/// Current report schema revision.
pub const SCHEMA_VERSION: u32 = 1;

/// Upstream TRCC commit reviewed for hardware-coverage evidence.
pub const UPSTREAM_REVIEWED_COMMIT: &str = "655a1acff5c86ff0f9121f9fd4a0ea14bee35447";

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
pub struct ValidationChecks {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enumerated: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passive_allowlist: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusive_owner: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handshake: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_marker: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub second_display_unchanged: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orientation: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colors: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub soak: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reconnect: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_restored: Option<CheckStatus>,
}

impl ValidationChecks {
    /// Returns `true` only when the check was explicitly recorded as pass.
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

    pub fn set(&mut self, field: CheckField, status: CheckStatus) {
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

/// Shareable USB fingerprint section (no bus/address/serial value).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportFingerprint {
    #[serde(with = "hex_u16")]
    pub vid: u16,
    #[serde(with = "hex_u16")]
    pub pid: u16,
    pub bcd_device: String,
    pub serial_present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidraw_correlated: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<ReportInterfaceShape>,
}

/// One interface alternate setting for shareable reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportInterfaceShape {
    pub number: u8,
    pub alternate_setting: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endpoints: Vec<ReportEndpointCapability>,
}

/// One endpoint capability for shareable reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportEndpointCapability {
    #[serde(with = "hex_u8")]
    pub address: u8,
    pub direction: super::usb_fingerprint::UsbDirection,
    pub transfer: super::usb_fingerprint::UsbTransferKind,
    pub max_packet_size: u16,
    pub interval: u8,
}

/// Pre-handshake policy recorded for audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportPreHandshakePolicy {
    LegacyBulkInit,
    Hid407ReadOnlyProbe,
    StopUnsupportedShape,
}

/// HID report descriptor capture status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DescriptorCaptureStatus {
    Unknown,
    Captured,
    Unavailable,
}

/// Observed HID output route (backend-observed, not descriptor-inferred).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReportHidOutputRoute {
    InterruptOut,
    ControlSetReport,
    LegacyBulk,
    HidReport,
}

/// Direct-hidraw backend provenance for shareable reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HidBackendProvenance {
    pub backend: String,
    pub expected_write_return_bytes: usize,
    pub kernel_hidraw_doc_ref: String,
    pub reviewed_hidapi_evidence_commit: String,
}

/// Independent HID read length observations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HidReadEvidence {
    pub read_capacity_bytes: usize,
    pub read_timeout_ms: u32,
    pub transport_return_bytes: isize,
    pub protocol_response_bytes: usize,
}

/// Independent HID write chunk observations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HidWriteChunkEvidence {
    pub protocol_chunk_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical_output_report_bytes: Option<usize>,
    pub report_id: u8,
    pub userspace_submit_bytes: usize,
    pub transport_return_bytes: isize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_max_packet_size: Option<u16>,
}

/// Typed HID write failure with partial-chunk evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HidWriteFailureEvidence {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_chunks: Vec<HidWriteChunkEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failing_chunk: Option<HidWriteChunkEvidence>,
    pub error_kind: HidWriteErrorKind,
    pub error_message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HidWriteErrorKind {
    NegativeReturn,
    UnexpectedCount,
    Transport,
    SessionStopped,
}

/// HID report transport evidence (length layers kept independent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HidReportEvidence {
    pub descriptor_status: DescriptorCaptureStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical_output_report_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_id: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_chunk_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub userspace_submit_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport_return_bytes: Option<isize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_max_packet_size: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_route: Option<ReportHidOutputRoute>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_write_authorized: Option<bool>,
    pub backend: HidBackendProvenance,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read: Option<HidReadEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_failure: Option<HidWriteFailureEvidence>,
}

/// Negotiated PM/SUB/FBL/profile policy from observed handshake bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NegotiatedProfile {
    pub response_bytes: usize,
    pub wire_protocol: String,
    pub pm: u8,
    pub sub: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fbl: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    pub profile_policy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_writes_allowed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_route: Option<ReportHidOutputRoute>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_single_session: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portrait_native: Option<bool>,
}

/// Typed failure chain entry with sanitized message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationErrorLink {
    pub kind: String,
    pub message: String,
}

/// Failure details for a stopped stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationFailure {
    pub stage: ValidationStage,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<ValidationErrorLink>,
}

/// Canonical shareable hardware-validation report (`report.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HardwareValidationReport {
    pub schema: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ValidationResult>,
    pub shareable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_step: Option<ValidationStage>,
    pub thermalwriter_version: String,
    pub thermalwriter_commit: String,
    pub upstream_reviewed_commit: String,
    pub hidapi_commit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<ReportFingerprint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_handshake_policy: Option<ReportPreHandshakePolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hid_report: Option<HidReportEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub negotiated: Option<NegotiatedProfile>,
    #[serde(default)]
    pub checks: ValidationChecks,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<ValidationFailure>,
}

impl HardwareValidationReport {
    /// Start a new in-progress report with build-time version/commit metadata.
    pub fn new_in_progress() -> Self {
        Self {
            schema: SCHEMA_VERSION,
            result: None,
            shareable: true,
            failed_step: None,
            thermalwriter_version: env!("CARGO_PKG_VERSION").to_string(),
            thermalwriter_commit: env!("THERMALWRITER_GIT_COMMIT").to_string(),
            upstream_reviewed_commit: UPSTREAM_REVIEWED_COMMIT.to_string(),
            hidapi_commit: REVIEWED_HIDAPI_EVIDENCE_COMMIT.to_string(),
            fingerprint: None,
            pre_handshake_policy: None,
            hid_report: None,
            negotiated: None,
            checks: ValidationChecks::default(),
            failure: None,
        }
    }

    pub fn set_fingerprint(
        &mut self,
        fingerprint: &UsbFingerprint,
        serial_present: bool,
        hidraw_correlated: Option<bool>,
    ) {
        self.fingerprint = Some(ReportFingerprint::from_usb_fingerprint(
            fingerprint,
            serial_present,
            hidraw_correlated,
        ));
    }

    pub fn set_pre_handshake_policy(&mut self, policy: Type2PreHandshakePolicy) {
        self.pre_handshake_policy = Some(ReportPreHandshakePolicy::from(policy));
    }

    pub fn record_check(&mut self, field: CheckField, status: CheckStatus) {
        self.checks.set(field, status);
    }

    pub fn record_negotiated_type2(
        &mut self,
        observation: &Type2NegotiatedObservation,
        device_info: Option<&DeviceInfo>,
    ) {
        self.negotiated = Some(NegotiatedProfile::from_type2_observation(
            observation,
            device_info,
        ));
    }

    pub fn set_hid_backend_contract(&mut self, contract: HidReportBackendContract) {
        let evidence = self
            .hid_report
            .get_or_insert_with(|| HidReportEvidence::empty_with_backend(contract));
        evidence.backend = HidBackendProvenance::from(contract);
    }

    pub fn record_hid_read(&mut self, observation: &HidReadObservation) {
        let evidence = self.hid_report.get_or_insert_with(|| {
            HidReportEvidence::empty_with_backend(LINUX_HIDRAW_BACKEND_CONTRACT)
        });
        evidence.read = Some(HidReadEvidence::from(observation));
    }

    pub fn record_hid_write_observation(&mut self, observation: &HidWriteObservation) {
        let evidence = self.hid_report.get_or_insert_with(|| {
            HidReportEvidence::empty_with_backend(LINUX_HIDRAW_BACKEND_CONTRACT)
        });
        evidence.protocol_chunk_bytes = Some(observation.protocol_chunk_bytes);
        evidence.logical_output_report_bytes = observation.logical_output_report_bytes;
        evidence.report_id = Some(observation.report_id);
        evidence.userspace_submit_bytes = Some(observation.userspace_submit_bytes);
        evidence.transport_return_bytes = Some(observation.transport_return_bytes);
        evidence.endpoint_max_packet_size = observation.endpoint_max_packet_size;
    }

    pub fn record_hid_chunked_write_failure(&mut self, failure: &HidChunkedWriteFailure) {
        let evidence = self.hid_report.get_or_insert_with(|| {
            HidReportEvidence::empty_with_backend(LINUX_HIDRAW_BACKEND_CONTRACT)
        });
        let (write_failure, safe) = HidWriteFailureEvidence::from_failure(failure);
        evidence.write_failure = Some(write_failure);
        if !safe {
            self.shareable = false;
        }
    }

    pub fn set_result(&mut self, result: ValidationResult) {
        self.result = Some(result);
    }

    pub fn fail_at(
        &mut self,
        stage: ValidationStage,
        errors: impl IntoIterator<Item = impl Into<String>>,
    ) {
        self.result = Some(ValidationResult::Fail);
        self.failed_step = Some(stage);
        let (failure, safe) =
            ValidationFailure::from_messages(stage, errors.into_iter().map(Into::into));
        self.failure = Some(failure);
        if !safe {
            self.shareable = false;
        }
    }

    pub fn abort_at(
        &mut self,
        stage: ValidationStage,
        errors: impl IntoIterator<Item = impl Into<String>>,
    ) {
        self.result = Some(ValidationResult::Aborted);
        self.failed_step = Some(stage);
        let (failure, safe) =
            ValidationFailure::from_messages(stage, errors.into_iter().map(Into::into));
        self.failure = Some(failure);
        if !safe {
            self.shareable = false;
        }
    }

    /// Serialize to deterministic TOML (struct field order).
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    /// Parse a report from TOML.
    pub fn from_toml(input: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(input)
    }

    /// Re-evaluate shareability after mutating free-text fields.
    pub fn apply_text_safety(&mut self) {
        if !self.text_fields_are_safe() {
            self.shareable = false;
        }
    }

    fn text_fields_are_safe(&self) -> bool {
        let mut safe = true;
        if let Some(failure) = &self.failure {
            for link in &failure.errors {
                if !sanitize_free_text(&link.message).provably_safe {
                    safe = false;
                }
            }
        }
        if let Some(hid) = &self.hid_report {
            if let Some(write_failure) = &hid.write_failure {
                if !sanitize_free_text(&write_failure.error_message).provably_safe {
                    safe = false;
                }
            }
        }
        safe
    }
}

impl ReportFingerprint {
    pub fn from_usb_fingerprint(
        fingerprint: &UsbFingerprint,
        serial_present: bool,
        hidraw_correlated: Option<bool>,
    ) -> Self {
        Self {
            vid: fingerprint.vid,
            pid: fingerprint.pid,
            bcd_device: fingerprint.bcd_device.clone(),
            serial_present,
            hidraw_correlated,
            interfaces: fingerprint
                .interfaces
                .iter()
                .map(ReportInterfaceShape::from)
                .collect(),
        }
    }
}

impl From<&UsbInterfaceShape> for ReportInterfaceShape {
    fn from(shape: &UsbInterfaceShape) -> Self {
        Self {
            number: shape.number,
            alternate_setting: shape.alternate_setting,
            class: shape.class,
            subclass: shape.subclass,
            protocol: shape.protocol,
            endpoints: shape
                .endpoints
                .iter()
                .map(ReportEndpointCapability::from)
                .collect(),
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

impl From<HidOutputRoute> for ReportHidOutputRoute {
    fn from(route: HidOutputRoute) -> Self {
        match route {
            HidOutputRoute::LegacyBulk => Self::LegacyBulk,
            HidOutputRoute::HidReport => Self::HidReport,
        }
    }
}

impl From<HidReportBackendContract> for HidBackendProvenance {
    fn from(contract: HidReportBackendContract) -> Self {
        Self {
            backend: contract.backend.to_string(),
            expected_write_return_bytes: contract.expected_write_return_bytes,
            kernel_hidraw_doc_ref: contract.kernel_hidraw_doc_ref.to_string(),
            reviewed_hidapi_evidence_commit: contract.reviewed_hidapi_evidence_commit.to_string(),
        }
    }
}

impl From<&HidReadObservation> for HidReadEvidence {
    fn from(observation: &HidReadObservation) -> Self {
        Self {
            read_capacity_bytes: observation.read_capacity_bytes,
            read_timeout_ms: observation.read_timeout_ms,
            transport_return_bytes: observation.transport_return_bytes,
            protocol_response_bytes: observation.protocol_response_bytes,
        }
    }
}

impl From<&HidWriteObservation> for HidWriteChunkEvidence {
    fn from(observation: &HidWriteObservation) -> Self {
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
            output_route: None,
            active_write_authorized: None,
            backend: HidBackendProvenance::from(contract),
            read: None,
            write_failure: None,
        }
    }
}

impl NegotiatedProfile {
    fn from_type2_observation(
        observation: &Type2NegotiatedObservation,
        device_info: Option<&DeviceInfo>,
    ) -> Self {
        let policy = observation.policy();
        Self {
            response_bytes: observation.response().len(),
            wire_protocol: wire_protocol_label(device_info.map(|info| info.protocol)),
            pm: observation.pm(),
            sub: observation.sub(),
            fbl: device_info.map(|info| info.fbl),
            width: device_info.map(|info| info.width()),
            height: device_info.map(|info| info.height()),
            profile_policy: profile_policy_label(policy, observation.pm(), observation.sub()),
            active_writes_allowed: Some(policy.active_writes_allowed()),
            output_route: policy.output().map(ReportHidOutputRoute::from),
            keep_single_session: Some(policy.keep_single_session()),
            portrait_native: Some(policy.portrait_native()),
        }
    }
}

fn wire_protocol_label(protocol: Option<WireProtocol>) -> String {
    match protocol {
        Some(WireProtocol::HidType2) => "hid-type2-report".to_string(),
        Some(WireProtocol::HidType3) => "hid-type3".to_string(),
        Some(WireProtocol::Bulk) => "bulk".to_string(),
        Some(WireProtocol::Scsi) => "scsi".to_string(),
        Some(WireProtocol::Ly) => "ly".to_string(),
        None => "unknown".to_string(),
    }
}

fn profile_policy_label(policy: Type2NegotiatedPolicy, pm: u8, sub: u8) -> String {
    if policy.active_writes_allowed() {
        if policy.output() == Some(HidOutputRoute::HidReport) && pm == 58 && sub == 0 {
            return "upstream-pm58-4.07".to_string();
        }
        if policy.output() == Some(HidOutputRoute::LegacyBulk) {
            return "legacy-bulk".to_string();
        }
        return format!("active-pm{pm}-sub{sub}");
    }
    if pm == 68 {
        return "observed-pm68-conservative-stop".to_string();
    }
    "observed-inactive".to_string()
}

impl ValidationFailure {
    fn from_messages(
        stage: ValidationStage,
        errors: impl IntoIterator<Item = String>,
    ) -> (Self, bool) {
        let mut safe = true;
        let errors = errors
            .into_iter()
            .map(|message| {
                let sanitized = sanitize_free_text(&message);
                if !sanitized.provably_safe {
                    safe = false;
                }
                ValidationErrorLink {
                    kind: "error".to_string(),
                    message: sanitized.text,
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
                Some(HidWriteChunkEvidence::from(observation)),
            ),
            HidReportWriteError::UnexpectedCount(err) => (
                HidWriteErrorKind::UnexpectedCount,
                err.to_string(),
                Some(HidWriteChunkEvidence::from(&err.observation)),
            ),
            HidReportWriteError::Transport {
                message,
                observation,
            } => (
                HidWriteErrorKind::Transport,
                format!("HID report write transport error: {message}"),
                Some(HidWriteChunkEvidence::from(observation)),
            ),
            HidReportWriteError::SessionStopped => (
                HidWriteErrorKind::SessionStopped,
                failure.error.to_string(),
                None,
            ),
        };
        let sanitized = sanitize_free_text(&raw_message);
        let safe = sanitized.provably_safe;
        (
            Self {
                completed_chunks: failure
                    .completed
                    .iter()
                    .map(HidWriteChunkEvidence::from)
                    .collect(),
                failing_chunk,
                error_kind,
                error_message: sanitized.text,
            },
            safe,
        )
    }
}

/// Outcome of sanitizing a free-form error/log string for shareable output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedText {
    pub text: String,
    pub provably_safe: bool,
}

const REDACTED: &str = "[redacted]";

/// Sanitize hostile or private content from free-form strings.
pub fn sanitize_free_text(input: &str) -> SanitizedText {
    let mut text = input.to_string();
    let mut touched = false;

    for pattern in HOSTILE_SUBSTRINGS {
        if text.to_ascii_lowercase().contains(pattern) {
            text = redact_substring(&text, pattern);
            touched = true;
        }
    }

    for key in ["serial", "busnum", "devnum"] {
        let (updated, key_touched) = redact_assignment_value(&text, key);
        text = updated;
        touched |= key_touched;
    }

    if text.contains("/home/") || text.contains("/Users/") {
        text = redact_path_like(&text, "/home/");
        text = redact_path_like(&text, "/Users/");
        touched = true;
    }

    if still_contains_hostile(&text) {
        return SanitizedText {
            text: REDACTED.to_string(),
            provably_safe: false,
        };
    }

    SanitizedText {
        text,
        provably_safe: !touched,
    }
}

const HOSTILE_SUBSTRINGS: &[&str] = &[
    "/sys/",
    "/dev/hidraw",
    "iserial",
    "usbmon",
    "xhci",
    "ehci",
    "ohci",
];

fn still_contains_hostile(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    HOSTILE_SUBSTRINGS
        .iter()
        .any(|pattern| lower.contains(pattern))
        || lower.contains("/home/")
        || lower.contains("/users/")
        || lower.contains("busnum")
        || lower.contains("devnum")
        || lower.contains("serial=")
}

fn redact_substring(input: &str, pattern: &str) -> String {
    let mut output = input.to_string();
    while let Some(index) = output.to_ascii_lowercase().find(pattern) {
        let end = index + pattern.len();
        output.replace_range(index..end, REDACTED);
    }
    output
}

fn redact_path_like(input: &str, prefix: &str) -> String {
    let mut output = input.to_string();
    while let Some(start) = output.find(prefix) {
        let end = output[start..]
            .find(|c: char| c.is_whitespace() || c == ',' || c == ';' || c == ')')
            .map(|offset| start + offset)
            .unwrap_or(output.len());
        output.replace_range(start..end, REDACTED);
    }
    output
}

fn redact_assignment_value(input: &str, key: &str) -> (String, bool) {
    let needle = format!("{key}=");
    let mut output = input.to_string();
    let mut touched = false;
    let mut search = 0usize;
    loop {
        let lower = output.to_ascii_lowercase();
        let Some(rel) = lower[search..].find(&needle) else {
            break;
        };
        let start = search + rel;
        let value_start = start + needle.len();
        let value_end = output[value_start..]
            .find(|c: char| c.is_whitespace() || [',', ';', ')', ']'].contains(&c))
            .map(|offset| value_start + offset)
            .unwrap_or(output.len());
        if value_start < value_end {
            output.replace_range(value_start..value_end, REDACTED);
            touched = true;
        }
        search = value_start.saturating_add(REDACTED.len());
        if search >= output.len() {
            break;
        }
    }
    (output, touched)
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
        write!(f, "hardware-validation-report(schema={})", self.schema)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::type2_policy::Type2PreHandshakePolicy;
    use crate::transport::usb_fingerprint::{
        UsbDirection, UsbEndpointCapability, UsbInterfaceShape, UsbTransferKind,
    };

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
    fn sanitize_hostile_paths_redacts_and_marks_unsafe() {
        let outcome = sanitize_free_text(
            "opened /dev/hidraw3 on busnum=1 devnum=4 serial=ABC /home/mike/sys/class/hidraw/hidraw3",
        );
        assert!(!outcome.provably_safe);
        assert!(!outcome.text.contains("/home/mike"));
        assert!(!outcome.text.contains("serial=ABC"));
        assert!(!outcome.text.contains("/dev/hidraw3"));
    }

    #[test]
    fn benign_error_stays_shareable() {
        let outcome = sanitize_free_text("unexpected HID write count: submitted=513 returned=8");
        assert!(outcome.provably_safe);
        assert!(outcome.text.contains("returned=8"));
    }

    #[test]
    fn round_trip_passive_in_only() {
        let mut report = HardwareValidationReport::new_in_progress();
        report.set_fingerprint(&hid_in_fingerprint(), false, Some(true));
        report.set_pre_handshake_policy(Type2PreHandshakePolicy::Hid407ReadOnlyProbe);
        report.record_check(CheckField::Enumerated, CheckStatus::Pass);
        report.record_check(CheckField::PassiveAllowlist, CheckStatus::Pass);
        report.set_result(ValidationResult::Pass);
        let toml = report.to_toml().expect("serialize");
        let parsed = HardwareValidationReport::from_toml(&toml).expect("parse");
        assert_eq!(parsed, report);
        assert!(parsed.shareable);
        let iface = &parsed.fingerprint.as_ref().unwrap().interfaces[0];
        assert_eq!(iface.endpoints.len(), 1);
        assert_eq!(iface.endpoints[0].max_packet_size, 8);
        assert!(
            iface
                .endpoints
                .iter()
                .all(|ep| ep.direction == UsbDirection::In)
        );
    }
}
