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

fn passive_replay_report() -> HardwareValidationReport {
    let mut report =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Replay, ValidationScope::Passive);
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
    let report = passive_replay_report();
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
    assert!(toml.contains("direction = \"out\""));
    assert!(toml.contains("scope = \"passive\""));
    assert!(toml.contains("origin = \"replay\""));
    assert!(toml.contains("pre_handshake_policy = \"hid407_read_only_probe\""));

    let parsed = HardwareValidationReport::from_toml(&toml).expect("parse");
    assert_eq!(parsed.scope(), ValidationScope::Passive);
    assert_eq!(parsed.origin(), EvidenceOrigin::Replay);
    assert_eq!(parsed.result(), Some(ValidationResult::Pass));
    assert!(!parsed.eligible_for_tested());
}

#[test]
fn golden_replay_pm58_active_evidence_in_progress() {
    let report = replay_pm58_active_report();
    let toml = report.to_private_toml().expect("serialize");

    let expected = include_str!("fixtures/validation_report/replay_pm58_active_evidence.toml");
    assert!(expected.contains("commit = \"unknown\""));
    assert!(expected.contains("dirty = true"));
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

    let parsed = HardwareValidationReport::from_toml(&toml).expect("parse pm58 replay");
    assert_eq!(parsed.origin(), EvidenceOrigin::Replay);
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
    report
        .set_pre_handshake_policy(Type2PreHandshakePolicy::Hid407ReadOnlyProbe)
        .expect("policy");
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

    let parsed = HardwareValidationReport::from_toml(&toml).expect("parse pm68 fail");
    assert_eq!(parsed.result(), Some(ValidationResult::Fail));
    assert!(
        parsed
            .negotiated()
            .unwrap()
            .negotiated_output_route()
            .is_none()
    );
}

#[test]
fn pm68_in_progress_round_trips() {
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
    report
        .set_pre_handshake_policy(Type2PreHandshakePolicy::Hid407ReadOnlyProbe)
        .expect("policy");
    report.record_negotiated_type2(&obs).expect("negotiated");

    let toml = report.to_private_toml().expect("serialize");
    assert!(!toml.contains("negotiated_output_route = "));
    let parsed = HardwareValidationReport::from_toml(&toml).expect("parse");
    let negotiated = parsed.negotiated().expect("negotiated");
    assert_eq!(
        negotiated.profile_policy(),
        ProfilePolicyLabel::ObservedPm68ConservativeStop
    );
    assert!(!negotiated.active_writes_allowed());
    assert!(negotiated.negotiated_output_route().is_none());
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
    report
        .set_pre_handshake_policy(Type2PreHandshakePolicy::Hid407ReadOnlyProbe)
        .expect("policy");
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
fn privacy_detector_matches_assignment_forms() {
    let outcome = sanitize_free_text("bus=2 address=7 addr:7 user=mike username=alice uid=1000");
    assert!(!outcome.provably_safe);
    assert_eq!(outcome.text, "[redacted]");
}

#[test]
fn missing_checks_never_count_as_pass() {
    let mut report =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Physical, ValidationScope::Full);
    report
        .set_fingerprint(&hid_in_fingerprint(), false, Some(true))
        .expect("fingerprint");
    report
        .set_pre_handshake_policy(Type2PreHandshakePolicy::Hid407ReadOnlyProbe)
        .expect("policy");
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
    let report = passive_replay_report();
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
    let mut input = passive_replay_report()
        .to_private_toml()
        .expect("serialize");
    input = input.replace("schema = 1", "schema = 99");
    let error = HardwareValidationReport::from_toml(&input).unwrap_err();
    assert!(error.to_string().contains("unsupported schema version"));
}

#[test]
fn rejects_unknown_fields() {
    let input = passive_replay_report()
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
    let passive = passive_replay_report();
    assert!(!passive.eligible_for_tested());

    let replay = replay_pm58_active_report();
    assert!(!replay.eligible_for_tested());

    let mut synthetic =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Synthetic, ValidationScope::Full);
    full_mandatory_checks(&mut synthetic);
    synthetic
        .set_fingerprint(&hid_in_fingerprint(), false, Some(true))
        .expect("fingerprint");
    synthetic
        .set_pre_handshake_policy(Type2PreHandshakePolicy::Hid407ReadOnlyProbe)
        .expect("policy");
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

fn bulk_fingerprint() -> UsbFingerprint {
    UsbFingerprint {
        vid: 0x87ad,
        pid: 0x70db,
        bcd_device: "1.00".to_string(),
        interfaces: vec![UsbInterfaceShape {
            number: 0,
            alternate_setting: 0,
            class: 0xff,
            subclass: 0,
            protocol: 0,
            endpoints: vec![
                UsbEndpointCapability {
                    address: 0x01,
                    direction: UsbDirection::Out,
                    transfer: UsbTransferKind::Bulk,
                    max_packet_size: 512,
                    interval: 0,
                },
                UsbEndpointCapability {
                    address: 0x81,
                    direction: UsbDirection::In,
                    transfer: UsbTransferKind::Bulk,
                    max_packet_size: 512,
                    interval: 0,
                },
            ],
        }],
    }
}

fn scsi_fingerprint() -> UsbFingerprint {
    UsbFingerprint {
        vid: 0x87cd,
        pid: 0x70db,
        bcd_device: "1.00".to_string(),
        interfaces: vec![UsbInterfaceShape {
            number: 0,
            alternate_setting: 0,
            class: 8,
            subclass: 0,
            protocol: 0,
            endpoints: vec![
                UsbEndpointCapability {
                    address: 0x01,
                    direction: UsbDirection::Out,
                    transfer: UsbTransferKind::Bulk,
                    max_packet_size: 512,
                    interval: 0,
                },
                UsbEndpointCapability {
                    address: 0x81,
                    direction: UsbDirection::In,
                    transfer: UsbTransferKind::Bulk,
                    max_packet_size: 512,
                    interval: 0,
                },
            ],
        }],
    }
}

fn legacy_type2_bulk_fingerprint() -> UsbFingerprint {
    UsbFingerprint {
        vid: WINBOND_HID2_VID,
        pid: WINBOND_HID2_PID,
        bcd_device: "1.00".to_string(),
        interfaces: vec![UsbInterfaceShape {
            number: 1,
            alternate_setting: 0,
            class: 0xff,
            subclass: 0,
            protocol: 0,
            endpoints: vec![
                UsbEndpointCapability {
                    address: 0x02,
                    direction: UsbDirection::Out,
                    transfer: UsbTransferKind::Bulk,
                    max_packet_size: 512,
                    interval: 0,
                },
                UsbEndpointCapability {
                    address: 0x81,
                    direction: UsbDirection::In,
                    transfer: UsbTransferKind::Bulk,
                    max_packet_size: 512,
                    interval: 0,
                },
            ],
        }],
    }
}

fn legacy_type2_full_response(pm: u8, sub: u8) -> Vec<u8> {
    let mut resp = vec![0u8; 20];
    resp[0..4].copy_from_slice(&[0xDA, 0xDB, 0xDC, 0xDD]);
    resp[12] = 0x01;
    resp[5] = pm;
    resp[4] = sub;
    resp
}

fn record_negotiated_device_report(
    fingerprint: &UsbFingerprint,
    device_info: &thermalwriter::transport::profile::DeviceInfo,
    response_bytes: usize,
) -> HardwareValidationReport {
    let mut report =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Replay, ValidationScope::Full);
    report
        .set_fingerprint(fingerprint, false, None)
        .expect("fingerprint");
    report
        .record_negotiated_device(device_info, response_bytes)
        .expect("negotiated");
    report
}

#[test]
fn record_negotiated_device_supports_bulk_87ad70db() {
    let device_info =
        build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, Some(72)).expect("bulk info");
    let report = record_negotiated_device_report(&bulk_fingerprint(), &device_info, 64);

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
    assert!(toml.contains("rotate_panel = false"));
    assert!(!toml.contains("portrait_native"));
}

#[test]
fn record_negotiated_device_derives_scsi_route() {
    let device_info =
        build_device_info(WireProtocol::Scsi, 0x87cd, 0x70db, 100, 0, Some(72)).expect("scsi info");
    let report = record_negotiated_device_report(&scsi_fingerprint(), &device_info, 64);

    assert_eq!(
        report.negotiated().unwrap().negotiated_output_route(),
        Some(NegotiatedOutputRoute::ScsiCommand)
    );
    let toml = report.to_private_toml().expect("serialize");
    assert!(toml.contains("negotiated_output_route = \"scsi_command\""));
}

#[test]
fn from_toml_rejects_bulk_protocol_with_scsi_route() {
    let device_info =
        build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, Some(72)).expect("bulk info");
    let report = record_negotiated_device_report(&bulk_fingerprint(), &device_info, 64);
    let mut toml = report.to_private_toml().expect("serialize");
    toml = toml.replace(
        "negotiated_output_route = \"legacy_bulk\"",
        "negotiated_output_route = \"scsi_command\"",
    );
    let error = HardwareValidationReport::from_toml(&toml).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("protocol family and output route disagree")
    );
}

#[test]
fn from_toml_rejects_replay_full_pass_with_clean_build() {
    let mut report = replay_pm58_active_report();
    full_mandatory_checks(&mut report);
    report
        .record_hid_read(&HidReadObservation {
            read_capacity_bytes: 512,
            read_timeout_ms: 500,
            transport_return_bytes: 8,
            protocol_response_bytes: 8,
        })
        .expect("read");
    let mut toml = report.to_private_toml().expect("serialize");
    toml = toml
        .lines()
        .map(|line| {
            if line.starts_with("commit = ") {
                "commit = \"655a1acff5c86ff0f9121f9fd4a0ea14bee35447\""
            } else if line.starts_with("dirty = ") {
                "dirty = false"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    toml = format!("result = \"pass\"\n\n{toml}");
    let error = HardwareValidationReport::from_toml(&toml).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("physical origin") || message.contains("full pass requires physical"),
        "unexpected error: {message}"
    );
}

#[test]
fn shareable_toml_rejects_malformed_version_path() {
    let report = passive_physical_report();
    let mut toml = report.to_private_toml().expect("serialize");
    toml = toml.replace("version = \"0.1.4\"", "version = \"/tmp/leak\"");
    let loaded = HardwareValidationReport::from_toml(&toml).expect("parse");
    assert!(loaded.to_shareable_toml().is_err());
}

#[test]
fn replay_origin_cannot_finalize_full_pass() {
    let mut report = replay_pm58_active_report();
    full_mandatory_checks(&mut report);
    report
        .record_hid_read(&HidReadObservation {
            read_capacity_bytes: 512,
            read_timeout_ms: 500,
            transport_return_bytes: 8,
            protocol_response_bytes: 8,
        })
        .expect("read");
    assert_eq!(
        report.finalize_full_pass().unwrap_err(),
        FinalizeError::WrongOrigin
    );
    assert!(report.result().is_none());
}

#[test]
fn transport_write_failure_omits_failing_chunk_return() {
    let failure = HidChunkedWriteFailure {
        completed: vec![],
        error: HidReportWriteError::Transport {
            message: "EIO".to_string(),
            observation: write_observation(0),
        },
    };

    let mut report = HardwareValidationReport::new_in_progress(
        EvidenceOrigin::Physical,
        ValidationScope::Passive,
    );
    report
        .record_hid_chunked_write_failure(&failure)
        .expect("write failure");

    let toml = report.to_private_toml().expect("serialize");
    assert!(!toml.contains("failing_chunk.transport_return_bytes"));
    let parsed = HardwareValidationReport::from_toml(&toml).expect("parse");
    let chunk = parsed
        .hid_report()
        .unwrap()
        .write_failure()
        .unwrap()
        .failing_chunk()
        .expect("failing chunk");
    assert_eq!(chunk.transport_return_bytes(), None);
}

#[test]
fn from_toml_rejects_unknown_commit_without_dirty_flag() {
    let report = passive_physical_report();
    let mut toml = report.to_private_toml().expect("serialize");
    toml = toml
        .lines()
        .map(|line| {
            if line.starts_with("commit = ") {
                "commit = \"unknown\""
            } else if line.starts_with("dirty = ") {
                "dirty = false"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let error = HardwareValidationReport::from_toml(&toml).unwrap_err();
    assert!(error.to_string().contains("build provenance"));
}

#[test]
fn from_toml_rejects_bulk_tampered_active_writes_false() {
    let device_info =
        build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, Some(72)).expect("bulk info");
    let report = record_negotiated_device_report(&bulk_fingerprint(), &device_info, 64);
    let mut toml = report.to_private_toml().expect("serialize");
    toml = toml.replace(
        "active_writes_allowed = true",
        "active_writes_allowed = false",
    );
    let error = HardwareValidationReport::from_toml(&toml).unwrap_err();
    assert!(error.to_string().contains("negotiated profile"));
}

#[test]
fn from_toml_rejects_bulk_tampered_keep_single_session_true() {
    let device_info =
        build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, Some(72)).expect("bulk info");
    let report = record_negotiated_device_report(&bulk_fingerprint(), &device_info, 64);
    let mut toml = report.to_private_toml().expect("serialize");
    toml = toml.replace("keep_single_session = false", "keep_single_session = true");
    let error = HardwareValidationReport::from_toml(&toml).unwrap_err();
    assert!(error.to_string().contains("negotiated profile"));
}

#[test]
fn from_toml_rejects_bulk_tampered_rotate_panel_true() {
    let device_info =
        build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, Some(72)).expect("bulk info");
    let report = record_negotiated_device_report(&bulk_fingerprint(), &device_info, 64);
    let mut toml = report.to_private_toml().expect("serialize");
    toml = toml.replace("rotate_panel = false", "rotate_panel = true");
    let error = HardwareValidationReport::from_toml(&toml).unwrap_err();
    assert!(error.to_string().contains("negotiated profile"));
}

#[test]
fn from_toml_rejects_bulk_tampered_fbl_derived_mismatch() {
    let device_info =
        build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, Some(72)).expect("bulk info");
    let report = record_negotiated_device_report(&bulk_fingerprint(), &device_info, 64);
    let mut toml = report.to_private_toml().expect("serialize");
    toml = toml.replace("fbl = 72", "fbl = 36");
    let error = HardwareValidationReport::from_toml(&toml).unwrap_err();
    assert!(error.to_string().contains("negotiated profile"));
}

#[test]
fn from_toml_rejects_bulk_tampered_vid_pid() {
    let device_info =
        build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, Some(72)).expect("bulk info");
    let report = record_negotiated_device_report(&bulk_fingerprint(), &device_info, 64);
    let mut toml = report.to_private_toml().expect("serialize");
    toml = toml.replace("vid = \"87ad\"", "vid = \"0416\"");
    let error = HardwareValidationReport::from_toml(&toml).unwrap_err();
    assert!(error.to_string().contains("negotiated profile"));
}

#[test]
fn from_toml_rejects_bulk_missing_out_endpoint() {
    let device_info =
        build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, Some(72)).expect("bulk info");
    let report = record_negotiated_device_report(&bulk_fingerprint(), &device_info, 64);
    let mut toml = report.to_private_toml().expect("serialize");
    toml = toml.replace("transfer = \"bulk\"", "transfer = \"interrupt\"");
    let error = HardwareValidationReport::from_toml(&toml).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("negotiated profile") || message.contains("bulk route"),
        "unexpected error: {message}"
    );
}

#[test]
fn record_negotiated_device_requires_fingerprint() {
    let device_info =
        build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, Some(72)).expect("bulk info");
    let mut report =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Replay, ValidationScope::Full);
    let error = report
        .record_negotiated_device(&device_info, 64)
        .unwrap_err();
    assert!(error.to_string().contains("fingerprint required"));
}

#[test]
fn record_negotiated_device_rejects_vid_pid_mismatch() {
    let device_info =
        build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, Some(72)).expect("bulk info");
    let mut report =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Replay, ValidationScope::Full);
    report
        .set_fingerprint(&scsi_fingerprint(), false, None)
        .expect("fingerprint");
    let error = report
        .record_negotiated_device(&device_info, 64)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not match report fingerprint")
    );
}

#[test]
fn from_toml_rejects_bulk_tampered_portrait_native_true() {
    let device_info =
        build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, Some(72)).expect("bulk info");
    let report = record_negotiated_device_report(&bulk_fingerprint(), &device_info, 64);
    let mut toml = report.to_private_toml().expect("serialize");
    toml = toml.replace(
        "rotate_panel = false",
        "rotate_panel = false\nportrait_native = true",
    );
    let error = HardwareValidationReport::from_toml(&toml).unwrap_err();
    assert!(error.to_string().contains("negotiated profile"));
}

#[test]
fn from_toml_rejects_bulk_tampered_wire_dimensions() {
    let device_info =
        build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, Some(72)).expect("bulk info");
    let report = record_negotiated_device_report(&bulk_fingerprint(), &device_info, 64);
    let mut toml = report.to_private_toml().expect("serialize");
    toml = toml.replace("width = 480", "width = 320");
    let error = HardwareValidationReport::from_toml(&toml).unwrap_err();
    assert!(error.to_string().contains("negotiated profile"));
}

fn short_pm49_response() -> Vec<u8> {
    vec![0xDA, 0xDB, 0xDC, 0xDD, 0x00, 0x31, 0x00, 0x00]
}

#[test]
fn observed_inactive_pm49_in_progress_round_trips() {
    let obs = negotiate_type2_policy(
        WINBOND_HID2_VID,
        WINBOND_HID2_PID,
        &short_pm49_response(),
        Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
    )
    .expect("pm49 inactive");

    let mut report =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Physical, ValidationScope::Full);
    report
        .set_fingerprint(&hid_in_fingerprint(), false, Some(true))
        .expect("fingerprint");
    report
        .set_pre_handshake_policy(Type2PreHandshakePolicy::Hid407ReadOnlyProbe)
        .expect("policy");
    report.record_negotiated_type2(&obs).expect("negotiated");

    let toml = report.to_private_toml().expect("serialize");
    assert!(!toml.contains("negotiated_output_route = "));
    let parsed = HardwareValidationReport::from_toml(&toml).expect("parse");
    let negotiated = parsed.negotiated().expect("negotiated");
    assert_eq!(
        negotiated.profile_policy(),
        ProfilePolicyLabel::ObservedInactive
    );
    assert!(!negotiated.active_writes_allowed());
    assert!(negotiated.negotiated_output_route().is_none());
}

#[test]
fn from_toml_rejects_observed_inactive_without_hid407_pre_policy() {
    let obs = negotiate_type2_policy(
        WINBOND_HID2_VID,
        WINBOND_HID2_PID,
        &short_pm49_response(),
        Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
    )
    .expect("pm49 inactive");

    let mut report =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Physical, ValidationScope::Full);
    report
        .set_fingerprint(&hid_in_fingerprint(), false, Some(true))
        .expect("fingerprint");
    report
        .set_pre_handshake_policy(Type2PreHandshakePolicy::Hid407ReadOnlyProbe)
        .expect("policy");
    report.record_negotiated_type2(&obs).expect("negotiated");

    let mut toml = report.to_private_toml().expect("serialize");
    toml = toml.replace(
        "pre_handshake_policy = \"hid407_read_only_probe\"",
        "pre_handshake_policy = \"legacy_bulk_init\"",
    );
    let error = HardwareValidationReport::from_toml(&toml).unwrap_err();
    assert!(error.to_string().contains("negotiated profile"));
}

#[test]
fn from_toml_rejects_hid407_vendor_class_fingerprint() {
    let report = replay_pm58_active_report();
    let mut toml = report.to_private_toml().expect("serialize");
    toml = toml.replace("class = 3", "class = 255");
    let error = HardwareValidationReport::from_toml(&toml).unwrap_err();
    assert!(error.to_string().contains("negotiated profile"));
}

#[test]
fn from_toml_rejects_407_tampered_to_legacy_bulk_init() {
    let report = replay_pm58_active_report();
    let mut toml = report.to_private_toml().expect("serialize");
    toml = toml.replace(
        "pre_handshake_policy = \"hid407_read_only_probe\"",
        "pre_handshake_policy = \"legacy_bulk_init\"",
    );
    let error = HardwareValidationReport::from_toml(&toml).unwrap_err();
    assert!(error.to_string().contains("negotiated profile"));
}

#[test]
fn from_toml_rejects_non_407_legacy_without_bulk_pair() {
    let obs = negotiate_type2_policy(
        WINBOND_HID2_VID,
        WINBOND_HID2_PID,
        &legacy_type2_full_response(49, 0),
        Type2PreHandshakePolicy::LegacyBulkInit,
    )
    .expect("legacy obs");

    let mut report =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Replay, ValidationScope::Full);
    report
        .set_fingerprint(&legacy_type2_bulk_fingerprint(), false, None)
        .expect("fingerprint");
    report
        .set_pre_handshake_policy(Type2PreHandshakePolicy::LegacyBulkInit)
        .expect("policy");
    let error = report.record_negotiated_type2(&obs).unwrap_err();
    assert!(error.to_string().contains("fixture-only"));
}

#[test]
fn legacy_type2_bulk_profile_is_rejected_as_fixture_only() {
    let obs = negotiate_type2_policy(
        WINBOND_HID2_VID,
        WINBOND_HID2_PID,
        &legacy_type2_full_response(49, 0),
        Type2PreHandshakePolicy::LegacyBulkInit,
    )
    .expect("legacy obs");
    let mut report =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Replay, ValidationScope::Full);
    report
        .set_fingerprint(&legacy_type2_bulk_fingerprint(), false, None)
        .expect("fingerprint");
    report
        .set_pre_handshake_policy(Type2PreHandshakePolicy::LegacyBulkInit)
        .expect("policy");
    let error = report.record_negotiated_type2(&obs).unwrap_err();
    assert!(error.to_string().contains("fixture-only"));
}

#[test]
fn record_negotiated_type2_rejects_policy_mismatch_with_fingerprint() {
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
        .set_pre_handshake_policy(Type2PreHandshakePolicy::LegacyBulkInit)
        .expect("policy");
    let error = report.record_negotiated_type2(&obs).unwrap_err();
    assert!(error.to_string().contains("negotiated profile"));
    assert!(report.negotiated().is_none());
}

#[test]
fn record_negotiated_device_rejects_invalid_scsi_shape() {
    let device_info =
        build_device_info(WireProtocol::Scsi, 0x87cd, 0x70db, 100, 0, Some(72)).expect("scsi info");
    let empty_scsi = UsbFingerprint {
        vid: 0x87cd,
        pid: 0x70db,
        bcd_device: "1.00".to_string(),
        interfaces: vec![],
    };
    let mut report =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Replay, ValidationScope::Full);
    report
        .set_fingerprint(&empty_scsi, false, None)
        .expect("fingerprint");
    let error = report
        .record_negotiated_device(&device_info, 64)
        .unwrap_err();
    assert!(error.to_string().contains("negotiated profile"));
    assert!(report.negotiated().is_none());
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
