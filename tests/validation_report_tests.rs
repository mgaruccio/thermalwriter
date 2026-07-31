// SPDX-License-Identifier: GPL-3.0-or-later
#![cfg(feature = "daemon")]

use thermalwriter::transport::hid_report::{
    HidChunkedWriteFailure, HidReadObservation, HidReportWriteError, HidWriteCountError,
    HidWriteObservation, LINUX_HIDRAW_BACKEND_CONTRACT, PROTOCOL_CHUNK_BYTES, REPORT_ID_UNNUMBERED,
    USERSPACE_SUBMIT_BYTES,
};
use thermalwriter::transport::type2_policy::{
    Type2PreHandshakePolicy, WINBOND_HID2_PID, WINBOND_HID2_VID, negotiate_type2_policy,
};
use thermalwriter::transport::usb_fingerprint::{
    UsbDirection, UsbEndpointCapability, UsbFingerprint, UsbInterfaceShape, UsbTransferKind,
};
use thermalwriter::transport::validation_report::{
    CheckField, CheckStatus, DescriptorCaptureStatus, DisplayDimensions, EvidenceOrigin,
    FinalizeError, HardwareValidationReport, HidReadErrorKind, ProfilePolicyLabel,
    ValidationResult, ValidationScope, ValidationStage, build_commit_known, sanitize_free_text,
};

const KNOWN_COMMIT: &str = "655a1acff5c86ff0f9121f9fd4a0ea14bee35447";

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
    report.set_fingerprint(&hid_in_fingerprint(), false, Some(true));
    report.set_pre_handshake_policy(Type2PreHandshakePolicy::Hid407ReadOnlyProbe);
    report.record_check(CheckField::Enumerated, CheckStatus::Pass);
    report.record_check(CheckField::PassiveAllowlist, CheckStatus::Pass);
    report.finalize_passive_pass().expect("passive pass");
    report
}

fn full_mandatory_checks(report: &mut HardwareValidationReport) {
    for field in [
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
    ] {
        report.record_check(field, CheckStatus::Pass);
    }
}

fn full_physical_report_from_toml() -> HardwareValidationReport {
    let input = include_str!("fixtures/validation_report/full_physical_pass.toml");
    HardwareValidationReport::from_toml(input).expect("parse full physical fixture")
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
fn golden_pm58_active_negotiated_policy() {
    let obs = negotiate_type2_policy(
        WINBOND_HID2_VID,
        WINBOND_HID2_PID,
        &short_pm58_response(),
        Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
    )
    .expect("pm58");

    let mut report =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Physical, ValidationScope::Full);
    report.set_fingerprint(&hid_in_fingerprint(), false, Some(true));
    report.set_pre_handshake_policy(Type2PreHandshakePolicy::Hid407ReadOnlyProbe);
    report.record_negotiated_type2(&obs).expect("negotiated");
    report.set_hid_backend_contract(LINUX_HIDRAW_BACKEND_CONTRACT);
    report.set_hid_descriptor_status(DescriptorCaptureStatus::Captured);
    report.record_hid_write_observation(512, Some(512), 0, 513, 513, Some(8));
    report.set_hid_active_write_authorized(true);
    report.record_check(CheckField::Handshake, CheckStatus::Pass);

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
    assert!(toml.contains("profile_policy = \"upstream_pm58_407\""));
    assert!(toml.contains("pm = 58"));
    assert!(toml.contains("fbl = 58"));
    assert!(toml.contains("response_bytes = 8"));
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
    report.set_fingerprint(&hid_in_fingerprint(), false, Some(true));
    report.record_negotiated_type2(&obs).expect("negotiated");
    report.record_check(CheckField::Handshake, CheckStatus::Pass);
    report.fail_at(
        ValidationStage::ActiveWrite,
        &["PM68 observed; active output not evidenced"],
    );

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
    assert!(toml.contains("fbl = 192"));
    assert!(!report.eligible_for_tested());
}

#[test]
fn golden_direct_hidraw_short_read_count() {
    let mut report = HardwareValidationReport::new_in_progress(
        EvidenceOrigin::Physical,
        ValidationScope::Passive,
    );
    report.set_fingerprint(&hid_in_fingerprint(), false, Some(true));
    report.set_hid_backend_contract(LINUX_HIDRAW_BACKEND_CONTRACT);
    report.record_hid_read(&HidReadObservation {
        read_capacity_bytes: 512,
        read_timeout_ms: 500,
        transport_return_bytes: 8,
        protocol_response_bytes: 8,
    });

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
    report.record_hid_read_failure(512, 500, None, HidReadErrorKind::Timeout, "read timed out");

    let toml = report.to_private_toml().expect("serialize");
    assert!(!toml.contains("transport_return_bytes"));

    report.record_hid_read_failure(
        512,
        500,
        Some(0),
        HidReadErrorKind::ShortCount,
        "short read",
    );
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
    report.set_fingerprint(&hid_in_fingerprint(), false, Some(true));
    report.record_hid_chunked_write_failure(&failure);
    report.fail_at(
        ValidationStage::ActiveWrite,
        &["unexpected HID write count"],
    );

    let toml = report.to_private_toml().expect("serialize");
    assert!(toml.contains("error_kind = \"unexpected_count\""));
    assert!(toml.contains("completed_chunks"));
    assert!(toml.contains("failing_chunk"));
    assert!(toml.contains("transport_return_bytes = 513"));

    let parsed = HardwareValidationReport::from_toml(&toml).expect("parse");
    let write_failure = parsed.hid_report().unwrap().write_failure().unwrap();
    assert_eq!(write_failure.completed_chunks().len(), 1);
    assert_eq!(
        write_failure.completed_chunks()[0].transport_return_bytes(),
        513
    );
    assert_eq!(
        write_failure
            .failing_chunk()
            .unwrap()
            .transport_return_bytes(),
        8
    );
}

#[test]
fn aborted_result_serializes_incrementally() {
    let mut report = HardwareValidationReport::new_in_progress(
        EvidenceOrigin::Physical,
        ValidationScope::Passive,
    );
    report.set_fingerprint(&hid_in_fingerprint(), true, None);
    report.record_check(CheckField::Enumerated, CheckStatus::Pass);
    report.abort_at(ValidationStage::Selection, &["ambiguous duplicate VID:PID"]);

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
    report.fail_at(
        ValidationStage::HidrawCorrelation,
        &["correlation failed for /dev/hidraw3 busnum=2 devnum=7 /home/mike/sys"],
    );

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
        "opened /dev/hidraw3 busnum=1 devnum=4 serial=SECRET /home/mike/.config",
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
    report.set_fingerprint(&hid_in_fingerprint(), false, Some(true));
    let obs = negotiate_type2_policy(
        WINBOND_HID2_VID,
        WINBOND_HID2_PID,
        &short_pm58_response(),
        Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
    )
    .unwrap();
    report.record_negotiated_type2(&obs).unwrap();
    report.record_check(CheckField::Enumerated, CheckStatus::Pass);

    assert!(report.checks().passed(CheckField::Enumerated));
    assert!(!report.checks().passed(CheckField::Handshake));
    assert!(!report.checks().passed(CheckField::Soak));
    assert_eq!(
        report.finalize_full_pass().unwrap_err(),
        FinalizeError::MissingMandatoryChecks
    );
}

#[test]
fn endpoint_packet_size_not_serialized_as_report_length() {
    let mut report = HardwareValidationReport::new_in_progress(
        EvidenceOrigin::Physical,
        ValidationScope::Passive,
    );
    report.set_fingerprint(&hid_in_fingerprint(), false, None);
    report.set_hid_backend_contract(LINUX_HIDRAW_BACKEND_CONTRACT);
    report.record_hid_write_observation(512, Some(512), 0, 513, 513, Some(8));

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
fn eligible_for_tested_only_on_complete_clean_physical_full_pass() {
    let passive = passive_physical_report();
    assert!(!passive.eligible_for_tested());

    let mut synthetic =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Synthetic, ValidationScope::Full);
    full_mandatory_checks(&mut synthetic);
    synthetic.set_fingerprint(&hid_in_fingerprint(), false, Some(true));
    let obs = negotiate_type2_policy(
        WINBOND_HID2_VID,
        WINBOND_HID2_PID,
        &short_pm58_response(),
        Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
    )
    .unwrap();
    synthetic.record_negotiated_type2(&obs).unwrap();
    synthetic.doc_result_override_for_test(Some(ValidationResult::Pass));
    assert!(!synthetic.eligible_for_tested());

    let mut incomplete = full_physical_report_from_toml();
    incomplete.doc_checks_clear_for_test();
    assert!(!incomplete.eligible_for_tested());

    let mut failed = full_physical_report_from_toml();
    failed.fail_at(ValidationStage::Soak, &["soak failed"]);
    assert!(!failed.eligible_for_tested());

    let mut dirty = full_physical_report_from_toml();
    dirty.doc_build_dirty_for_test(true);
    assert!(!dirty.eligible_for_tested());

    let mut unknown_commit = full_physical_report_from_toml();
    unknown_commit.doc_build_commit_for_test("unknown");
    assert!(!unknown_commit.eligible_for_tested());

    let clean = full_physical_report_from_toml();
    assert!(clean.shareable());
    assert!(build_commit_known(clean.build_provenance().commit()));
    assert!(!clean.build_provenance().dirty());
    assert!(clean.eligible_for_tested());
}

#[test]
fn shareable_toml_rejects_deserialized_hostile_message() {
    let mut report = HardwareValidationReport::new_in_progress(
        EvidenceOrigin::Physical,
        ValidationScope::Passive,
    );
    report.fail_at(ValidationStage::Inventory, &["benign inventory error"]);
    let mut toml = report.to_private_toml().expect("serialize");
    toml = toml.replace("benign inventory error", "/dev/hidraw0 leaked");
    let loaded = HardwareValidationReport::from_toml(&toml).expect("parse");
    assert!(loaded.to_shareable_toml().is_err());
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
}

#[test]
#[ignore = "one-shot fixture generator"]
fn write_golden_fixtures() {
    use std::fs;
    use std::path::PathBuf;

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/validation_report");
    fs::create_dir_all(&dir).expect("mkdir");

    let passive = passive_physical_report();
    fs::write(
        dir.join("passive_in_only.toml"),
        passive.to_private_toml().expect("passive"),
    )
    .expect("write passive");

    let obs = negotiate_type2_policy(
        WINBOND_HID2_VID,
        WINBOND_HID2_PID,
        &short_pm58_response(),
        Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
    )
    .expect("pm58");
    let mut full =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Physical, ValidationScope::Full);
    full.set_fingerprint(&hid_in_fingerprint(), false, Some(true));
    full.set_pre_handshake_policy(Type2PreHandshakePolicy::Hid407ReadOnlyProbe);
    full.record_negotiated_type2(&obs).expect("negotiated");
    full.set_hid_backend_contract(LINUX_HIDRAW_BACKEND_CONTRACT);
    full_mandatory_checks(&mut full);
    full.doc_build_commit_for_test(KNOWN_COMMIT);
    full.doc_build_dirty_for_test(false);
    full.doc_result_override_for_test(Some(ValidationResult::Pass));
    fs::write(
        dir.join("full_physical_pass.toml"),
        full.to_private_toml().expect("full"),
    )
    .expect("write full");
}
