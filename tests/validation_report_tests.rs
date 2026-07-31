// SPDX-License-Identifier: GPL-3.0-or-later
#![cfg(feature = "daemon")]

use thermalwriter::transport::hid_report::{
    HidChunkedWriteFailure, HidReadObservation, HidReportWriteError, HidWriteCountError,
    HidWriteObservation, LINUX_HIDRAW_BACKEND_CONTRACT, PROTOCOL_CHUNK_BYTES, REPORT_ID_UNNUMBERED,
    USERSPACE_SUBMIT_BYTES,
};
use thermalwriter::transport::profile::{WireProtocol, build_device_info};
use thermalwriter::transport::type2_policy::{
    Type2PreHandshakePolicy, WINBOND_HID2_PID, WINBOND_HID2_VID, negotiate_type2_policy,
};
use thermalwriter::transport::usb_fingerprint::{
    UsbDirection, UsbEndpointCapability, UsbFingerprint, UsbInterfaceShape, UsbTransferKind,
};
use thermalwriter::transport::validation_report::{
    CheckField, CheckStatus, DescriptorCaptureStatus, DisplayDimensions, EvidenceOrigin,
    FinalizeError, HardwareValidationReport, HidReadErrorKind, NegotiatedOutputRoute,
    ProfilePolicyLabel, ReportMutationError, ValidationErrorKind, ValidationResult,
    ValidationScope, ValidationStage, sanitize_free_text,
};

fn hid_in_fingerprint() -> UsbFingerprint {
    UsbFingerprint {
        vid: WINBOND_HID2_VID,
        pid: WINBOND_HID2_PID,
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

fn short_pm58_response() -> Vec<u8> {
    vec![0xDA, 0xDB, 0xDC, 0xDD, 0x00, 0x3A, 0x00, 0x00]
}

fn write_observation(returned: isize) -> HidWriteObservation {
    HidWriteObservation {
        protocol_chunk_bytes: PROTOCOL_CHUNK_BYTES,
        logical_output_report_bytes: Some(PROTOCOL_CHUNK_BYTES),
        report_id: REPORT_ID_UNNUMBERED,
        userspace_submit_bytes: USERSPACE_SUBMIT_BYTES,
        transport_return_bytes: returned,
        endpoint_max_packet_size: Some(8),
    }
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

fn full_mandatory_checks(report: &mut HardwareValidationReport) {
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
}

fn replay_pm58_active_report() -> HardwareValidationReport {
    let obs = negotiate_type2_policy(
        WINBOND_HID2_VID,
        WINBOND_HID2_PID,
        &short_pm58_response(),
        Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
    )
    .expect("pm58");

    let mut report =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Replay, ValidationScope::Full);
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
        .record_hid_write_observation(512, Some(512), 0, 513, Some(513), Some(8))
        .expect("write");
    report
        .set_hid_active_write_authorized(true)
        .expect("authorized");
    report
}

#[test]
fn golden_passive_in_only_hid_interrupt_without_out() {
    let report = passive_physical_report();
    let toml = report.to_private_toml().expect("serialize");

    let expected = include_str!("fixtures/validation_report/passive_in_only.toml");
    assert_eq!(
        normalize_build_section(&toml),
        normalize_build_section(expected)
    );

    assert!(toml.contains("vid = \"0416\""));
    assert!(toml.contains("direction = \"in\""));
    assert!(toml.contains("transfer = \"interrupt\""));
    assert!(toml.contains("max_packet_size = 8"));
    assert!(!toml.contains("direction = \"out\""));
    assert!(toml.contains("scope = \"passive\""));
    assert!(toml.contains("origin = \"physical\""));
    assert!(toml.contains("pre_handshake_policy = \"hid407_read_only_probe\""));

    let parsed = HardwareValidationReport::from_toml(&toml).expect("parse");
    assert_eq!(parsed.scope(), ValidationScope::Passive);
    assert_eq!(parsed.result(), Some(ValidationResult::Pass));
    assert!(!parsed.eligible_for_tested());
}

#[test]
fn golden_replay_pm58_active_evidence_in_progress() {
    let report = replay_pm58_active_report();
    let toml = report.to_private_toml().expect("serialize");

    let expected = include_str!("fixtures/validation_report/replay_pm58_active_evidence.toml");
    assert_eq!(
        normalize_build_section(&toml),
        normalize_build_section(expected)
    );

    assert!(toml.contains("origin = \"replay\""));
    assert!(toml.contains("profile_policy = \"upstream_pm58_407\""));
    assert!(toml.contains("pm = 58"));
    assert!(toml.contains("fbl = 58"));
    assert!(!toml.contains("result = "));
    assert!(!report.eligible_for_tested());
}

#[test]
fn golden_pm58_active_negotiated_policy() {
    let report = replay_pm58_active_report();
    let negotiated = report.negotiated().expect("negotiated profile");
    assert_eq!(negotiated.pm(), 58);
    assert_eq!(negotiated.fbl(), 58);
    assert_eq!(
        negotiated.profile_policy(),
        ProfilePolicyLabel::UpstreamPm58_407
    );
    assert_eq!(
        negotiated.wire_dimensions(),
        DisplayDimensions {
            width: 240,
            height: 320,
        }
    );

    let toml = report.to_private_toml().expect("serialize");
    assert!(toml.contains("userspace_submit_bytes = 513"));
    assert!(toml.contains("protocol_chunk_bytes = 512"));
    assert!(toml.contains("runtime_route = \"kernel_managed_hidraw\""));
    assert!(!toml.contains("interrupt_out"));
    assert!(!toml.contains("control_set_report"));
}

#[test]
fn golden_pm68_conservative_stop_before_active_write() {
    let mut resp = short_pm58_response();
    resp[5] = 68;
    let obs = negotiate_type2_policy(
        WINBOND_HID2_VID,
        WINBOND_HID2_PID,
        &resp,
        Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
    )
    .expect("pm68");

    let mut report =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Physical, ValidationScope::Full);
    report
        .set_fingerprint(&hid_in_fingerprint(), false, Some(true))
        .expect("fingerprint");
    report.record_negotiated_type2(&obs).expect("negotiated");
    report
        .record_check(CheckField::Handshake, CheckStatus::Pass)
        .expect("check");
    report
        .fail_at(
            ValidationStage::ActiveWrite,
            &[(
                ValidationErrorKind::Policy,
                "PM68 observed; active output not evidenced",
            )],
        )
        .expect("fail");

    let negotiated = report.negotiated().expect("negotiated");
    assert_eq!(negotiated.fbl(), 192);
    assert_eq!(
        negotiated.wire_dimensions(),
        DisplayDimensions {
            width: 1280,
            height: 480,
        }
    );
    assert_eq!(
        negotiated.profile_policy(),
        ProfilePolicyLabel::ObservedPm68ConservativeStop
    );

    let toml = report.to_private_toml().expect("serialize");
    assert!(toml.contains("profile_policy = \"observed_pm68_conservative_stop\""));
    assert!(toml.contains("active_writes_allowed = false"));
    assert!(toml.contains("result = \"fail\""));
    assert!(toml.contains("failed_step = \"active_write\""));
    assert!(toml.contains("kind = \"policy\""));
    assert!(toml.contains("fbl = 192"));
    assert!(!report.eligible_for_tested());
}

#[test]
fn pm68_cannot_finalize_full_pass() {
    let mut resp = short_pm58_response();
    resp[5] = 68;
    let obs = negotiate_type2_policy(
        WINBOND_HID2_VID,
        WINBOND_HID2_PID,
        &resp,
        Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
    )
    .expect("pm68");

    let mut report =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Physical, ValidationScope::Full);
    report
        .set_fingerprint(&hid_in_fingerprint(), false, Some(true))
        .expect("fingerprint");
    report.record_negotiated_type2(&obs).expect("negotiated");
    full_mandatory_checks(&mut report);
    report
        .set_hid_backend_contract(LINUX_HIDRAW_BACKEND_CONTRACT)
        .expect("backend");
    report
        .record_hid_write_observation(512, Some(512), 0, 513, Some(513), Some(8))
        .expect("write");
    report
        .set_hid_active_write_authorized(true)
        .expect("authorized");

    assert_eq!(
        report.finalize_full_pass().unwrap_err(),
        FinalizeError::ConservativeStopProfile
    );
}

#[test]
fn golden_direct_hidraw_short_read_count() {
    let mut report = HardwareValidationReport::new_in_progress(
        EvidenceOrigin::Physical,
        ValidationScope::Passive,
    );
    report
        .set_fingerprint(&hid_in_fingerprint(), false, Some(true))
        .expect("fingerprint");
    report
        .set_hid_backend_contract(LINUX_HIDRAW_BACKEND_CONTRACT)
        .expect("backend");
    report
        .record_hid_read(&HidReadObservation {
            read_capacity_bytes: 512,
            read_timeout_ms: 500,
            transport_return_bytes: 8,
            protocol_response_bytes: 8,
        })
        .expect("read");

    let toml = report.to_private_toml().expect("serialize");
    assert!(toml.contains("read_capacity_bytes = 512"));
    assert!(toml.contains("transport_return_bytes = 8"));
    assert!(toml.contains("protocol_response_bytes = 8"));
    assert!(!toml.contains("logical_output_report_bytes = 8"));

    let parsed = HardwareValidationReport::from_toml(&toml).expect("parse");
    let read = parsed.hid_report().unwrap().read().unwrap();
    assert_eq!(read.transport_return_bytes(), Some(8));
    assert_eq!(read.read_capacity_bytes(), 512);
}

#[test]
fn golden_hid_read_failure_distinguishes_none_from_zero() {
    let mut report = HardwareValidationReport::new_in_progress(
        EvidenceOrigin::Physical,
        ValidationScope::Passive,
    );
    report
        .record_hid_read_failure(512, 500, None, HidReadErrorKind::Timeout, "read timed out")
        .expect("read failure");

    let toml = report.to_private_toml().expect("serialize");
    assert!(!toml.contains("transport_return_bytes"));

    report
        .record_hid_read_failure(
            512,
            500,
            Some(0),
            HidReadErrorKind::ShortCount,
            "short read",
        )
        .expect("read failure");
    let toml = report.to_private_toml().expect("serialize");
    assert!(toml.contains("transport_return_bytes = 0"));
}

#[test]
fn golden_partial_chunk_write_failure() {
    let completed = write_observation(513);
    let failing = write_observation(8);
    let failure = HidChunkedWriteFailure {
        completed: vec![completed],
        error: HidReportWriteError::UnexpectedCount(HidWriteCountError {
            submitted: USERSPACE_SUBMIT_BYTES,
            returned: 8,
            expected: USERSPACE_SUBMIT_BYTES,
            observation: failing,
        }),
    };

    let mut report =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Physical, ValidationScope::Full);
    report
        .set_fingerprint(&hid_in_fingerprint(), false, Some(true))
        .expect("fingerprint");
    report
        .record_hid_chunked_write_failure(&failure)
        .expect("write failure");
    report
        .fail_at(
            ValidationStage::ActiveWrite,
            &[(ValidationErrorKind::Transport, "unexpected HID write count")],
        )
        .expect("fail");

    let toml = report.to_private_toml().expect("serialize");
    assert!(toml.contains("error_kind = \"unexpected_count\""));
    assert!(toml.contains("kind = \"transport\""));
    assert!(toml.contains("completed_chunks"));
    assert!(toml.contains("failing_chunk"));
    assert!(toml.contains("transport_return_bytes = 513"));

    let parsed = HardwareValidationReport::from_toml(&toml).expect("parse");
    let write_failure = parsed.hid_report().unwrap().write_failure().unwrap();
    assert_eq!(write_failure.completed_chunks().len(), 1);
    assert_eq!(
        write_failure.completed_chunks()[0].transport_return_bytes(),
        Some(513)
    );
    assert_eq!(
        write_failure
            .failing_chunk()
            .unwrap()
            .transport_return_bytes(),
        Some(8)
    );
}

#[test]
fn write_observation_allows_missing_transport_return() {
    let mut report = HardwareValidationReport::new_in_progress(
        EvidenceOrigin::Physical,
        ValidationScope::Passive,
    );
    report
        .record_hid_write_observation(512, Some(512), 0, 513, None, Some(8))
        .expect("write");

    let toml = report.to_private_toml().expect("serialize");
    assert!(!toml.contains("transport_return_bytes"));
}

#[test]
fn aborted_result_serializes_incrementally() {
    let mut report = HardwareValidationReport::new_in_progress(
        EvidenceOrigin::Physical,
        ValidationScope::Passive,
    );
    report
        .set_fingerprint(&hid_in_fingerprint(), true, None)
        .expect("fingerprint");
    report
        .record_check(CheckField::Enumerated, CheckStatus::Pass)
        .expect("check");
    report
        .abort_at(
            ValidationStage::Selection,
            &[(ValidationErrorKind::Device, "ambiguous duplicate VID:PID")],
        )
        .expect("abort");

    let toml = report.to_private_toml().expect("serialize");
    assert!(toml.contains("result = \"aborted\""));
    assert!(toml.contains("serial_present = true"));
    assert!(!toml.contains("serial ="));

    let parsed = HardwareValidationReport::from_toml(&toml).expect("parse");
    assert_eq!(parsed.result(), Some(ValidationResult::Aborted));
    assert!(parsed.fingerprint().unwrap().serial_present());
}

#[test]
fn hostile_error_fully_redacts_and_blocks_shareable() {
    let mut report = HardwareValidationReport::new_in_progress(
        EvidenceOrigin::Physical,
        ValidationScope::Passive,
    );
    report
        .fail_at(
            ValidationStage::HidrawCorrelation,
            &[(
                ValidationErrorKind::Error,
                "correlation failed for /dev/hidraw3 busnum=2 devnum=7 /home/mike/sys",
            )],
        )
        .expect("fail");

    assert!(!report.shareable());
    let message = report.failure().unwrap().errors()[0].message();
    assert_eq!(message, "[redacted]");
    assert!(!message.contains("/home/mike"));
    assert!(!message.contains("/dev/hidraw3"));

    assert!(report.to_shareable_toml().is_err());
}

#[test]
fn sanitize_hostile_redaction_golden() {
    let outcome = sanitize_free_text(
        "opened /dev/hidraw3 busnum=1 devnum=4 serial=SECRET /home/mike/.config bus=1 user=mike uid=1000",
    );
    assert!(!outcome.provably_safe);
    assert_eq!(outcome.text, "[redacted]");
    assert!(!outcome.text.contains("SECRET"));
    assert!(!outcome.text.contains("/home/mike"));
}

#[test]
fn missing_checks_never_count_as_pass() {
    let mut report =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Physical, ValidationScope::Full);
    report
        .set_fingerprint(&hid_in_fingerprint(), false, Some(true))
        .expect("fingerprint");
    let obs = negotiate_type2_policy(
        WINBOND_HID2_VID,
        WINBOND_HID2_PID,
        &short_pm58_response(),
        Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
    )
    .unwrap();
    report.record_negotiated_type2(&obs).unwrap();
    report
        .record_check(CheckField::Enumerated, CheckStatus::Pass)
        .expect("check");

    assert!(report.checks().passed(CheckField::Enumerated));
    assert!(!report.checks().passed(CheckField::Handshake));
    assert!(!report.checks().passed(CheckField::ActiveWrite));
    assert_eq!(
        report.finalize_full_pass().unwrap_err(),
        FinalizeError::MissingMandatoryChecks
    );
}

#[test]
fn mutation_after_finalize_is_rejected() {
    let mut report = passive_physical_report();
    assert_eq!(
        report
            .record_check(CheckField::Enumerated, CheckStatus::Pass)
            .unwrap_err(),
        ReportMutationError::AlreadyFinalized
    );
}

#[test]
fn endpoint_packet_size_not_serialized_as_report_length() {
    let mut report = HardwareValidationReport::new_in_progress(
        EvidenceOrigin::Physical,
        ValidationScope::Passive,
    );
    report
        .set_fingerprint(&hid_in_fingerprint(), false, None)
        .expect("fingerprint");
    report
        .set_hid_backend_contract(LINUX_HIDRAW_BACKEND_CONTRACT)
        .expect("backend");
    report
        .record_hid_write_observation(512, Some(512), 0, 513, Some(513), Some(8))
        .expect("write");

    let toml = report.to_private_toml().expect("serialize");
    assert!(toml.contains("max_packet_size = 8"));
    assert!(toml.contains("logical_output_report_bytes = 512"));
    assert!(!toml.contains("logical_output_report_bytes = 8"));
}

#[test]
fn deterministic_round_trip_preserves_schema_fields() {
    let report = passive_physical_report();
    let first = report.to_private_toml().expect("serialize");
    let second = HardwareValidationReport::from_toml(&first)
        .expect("parse")
        .to_private_toml()
        .expect("re-serialize");
    assert_eq!(first, second);
    assert!(first.contains("schema = 1"));
    assert!(first.contains("version = \"0.1.4\""));
}

#[test]
fn rejects_unsupported_schema_version() {
    let mut input = passive_physical_report()
        .to_private_toml()
        .expect("serialize");
    input = input.replace("schema = 1", "schema = 99");
    let error = HardwareValidationReport::from_toml(&input).unwrap_err();
    assert!(error.to_string().contains("unsupported schema version"));
}

#[test]
fn rejects_unknown_fields() {
    let input = passive_physical_report()
        .to_private_toml()
        .expect("serialize");
    let input = format!("unknown_field = true\n{input}");
    let error = HardwareValidationReport::from_toml(&input).unwrap_err();
    assert!(error.to_string().contains("unknown report field"));
}

#[test]
fn hostile_message_rejected_on_deserialize() {
    let mut report = HardwareValidationReport::new_in_progress(
        EvidenceOrigin::Physical,
        ValidationScope::Passive,
    );
    report
        .fail_at(
            ValidationStage::Inventory,
            &[(ValidationErrorKind::Error, "benign inventory error")],
        )
        .expect("fail");
    let mut toml = report.to_private_toml().expect("serialize");
    toml = toml.replace("benign inventory error", "/dev/hidraw0 leaked");
    let error = HardwareValidationReport::from_toml(&toml).unwrap_err();
    assert!(error.to_string().contains("hostile"));
}

#[test]
fn eligible_for_tested_rejects_replay_and_synthetic_origins() {
    let passive = passive_physical_report();
    assert!(!passive.eligible_for_tested());

    let replay = replay_pm58_active_report();
    assert!(!replay.eligible_for_tested());

    let mut synthetic =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Synthetic, ValidationScope::Full);
    full_mandatory_checks(&mut synthetic);
    synthetic
        .set_fingerprint(&hid_in_fingerprint(), false, Some(true))
        .expect("fingerprint");
    let obs = negotiate_type2_policy(
        WINBOND_HID2_VID,
        WINBOND_HID2_PID,
        &short_pm58_response(),
        Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
    )
    .unwrap();
    synthetic.record_negotiated_type2(&obs).unwrap();
    synthetic
        .set_hid_backend_contract(LINUX_HIDRAW_BACKEND_CONTRACT)
        .expect("backend");
    synthetic
        .record_hid_write_observation(512, Some(512), 0, 513, Some(513), Some(8))
        .expect("write");
    synthetic
        .set_hid_active_write_authorized(true)
        .expect("authorized");
    assert_eq!(
        synthetic.finalize_full_pass().unwrap_err(),
        FinalizeError::WrongOrigin
    );
    assert!(!synthetic.eligible_for_tested());
}

#[test]
fn record_negotiated_device_supports_bulk_87ad70db() {
    let device_info =
        build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, Some(72)).expect("bulk info");
    let mut report =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Replay, ValidationScope::Full);
    report
        .record_negotiated_device(&device_info, NegotiatedOutputRoute::LegacyBulk, 64)
        .expect("negotiated");

    let negotiated = report.negotiated().expect("negotiated");
    assert_eq!(negotiated.pm(), 4);
    assert_eq!(negotiated.fbl(), 72);
    assert_eq!(negotiated.profile_policy(), ProfilePolicyLabel::LegacyBulk);
    assert_eq!(
        negotiated.negotiated_output_route(),
        Some(NegotiatedOutputRoute::LegacyBulk)
    );

    let toml = report.to_private_toml().expect("serialize");
    assert!(toml.contains("protocol_family = \"bulk\""));
    assert!(toml.contains("negotiated_output_route = \"legacy_bulk\""));
}

/// Strip volatile `[build]` lines so golden fixtures stay stable across commits.
fn normalize_build_section(toml: &str) -> String {
    toml.lines()
        .filter(|line| {
            !line.starts_with("commit = ")
                && !line.starts_with("dirty = ")
                && *line != "[build]"
                && !line.starts_with("version = ")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end_matches('\n')
        .to_string()
}
