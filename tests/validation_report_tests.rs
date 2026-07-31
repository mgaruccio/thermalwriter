// SPDX-License-Identifier: GPL-3.0-or-later
#![cfg(feature = "daemon")]

use thermalwriter::transport::hid_report::{
    HidChunkedWriteFailure, HidReadObservation, HidReportWriteError, HidWriteCountError,
    HidWriteObservation, LINUX_HIDRAW_BACKEND_CONTRACT, PROTOCOL_CHUNK_BYTES, REPORT_ID_UNNUMBERED,
    USERSPACE_SUBMIT_BYTES,
};
use thermalwriter::transport::profile::{DeviceInfo, DeviceProfile, FrameEncoding, WireProtocol};
use thermalwriter::transport::type2_policy::{
    Type2PreHandshakePolicy, WINBOND_HID2_PID, WINBOND_HID2_VID, negotiate_type2_policy,
};
use thermalwriter::transport::usb_fingerprint::{
    UsbDirection, UsbEndpointCapability, UsbFingerprint, UsbInterfaceShape, UsbTransferKind,
};
use thermalwriter::transport::validation_report::{
    CheckField, CheckStatus, DescriptorCaptureStatus, HardwareValidationReport,
    ReportHidOutputRoute, ValidationResult, ValidationStage, sanitize_free_text,
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

fn pm68_device_info() -> DeviceInfo {
    DeviceInfo {
        vid: WINBOND_HID2_VID,
        pid: WINBOND_HID2_PID,
        pm: 68,
        sub: 0,
        fbl: 192,
        protocol: WireProtocol::HidType2,
        profile: DeviceProfile {
            width: 1920,
            height: 480,
            encoding: FrameEncoding::Jpeg,
            rotate_panel: false,
            widescreen: true,
            encode_baseline: 0,
            encode_base: 0,
            encode_invert: false,
        },
    }
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

#[test]
fn golden_passive_in_only_hid_interrupt_without_out() {
    let mut report = HardwareValidationReport::new_in_progress();
    report.set_fingerprint(&hid_in_fingerprint(), false, Some(true));
    report.set_pre_handshake_policy(Type2PreHandshakePolicy::Hid407ReadOnlyProbe);
    report.record_check(CheckField::Enumerated, CheckStatus::Pass);
    report.record_check(CheckField::PassiveAllowlist, CheckStatus::Pass);
    report.set_result(ValidationResult::Pass);

    let toml = report.to_toml().expect("serialize");
    assert!(toml.contains("vid = \"0416\""));
    assert!(toml.contains("direction = \"in\""));
    assert!(toml.contains("transfer = \"interrupt\""));
    assert!(toml.contains("max_packet_size = 8"));
    assert!(!toml.contains("direction = \"out\""));
    assert!(toml.contains("pre_handshake_policy = \"hid407_read_only_probe\""));

    let parsed = HardwareValidationReport::from_toml(&toml).expect("parse");
    assert_eq!(parsed, report);
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

    let mut report = HardwareValidationReport::new_in_progress();
    report.set_fingerprint(&hid_in_fingerprint(), false, Some(true));
    report.set_pre_handshake_policy(Type2PreHandshakePolicy::Hid407ReadOnlyProbe);
    report.record_negotiated_type2(&obs, None);
    report.set_hid_backend_contract(LINUX_HIDRAW_BACKEND_CONTRACT);

    if let Some(hid) = report.hid_report.as_mut() {
        hid.descriptor_status = DescriptorCaptureStatus::Captured;
        hid.logical_output_report_bytes = Some(512);
        hid.report_id = Some(0);
        hid.protocol_chunk_bytes = Some(512);
        hid.userspace_submit_bytes = Some(513);
        hid.transport_return_bytes = Some(513);
        hid.output_route = Some(ReportHidOutputRoute::ControlSetReport);
        hid.active_write_authorized = Some(true);
    }

    report.record_check(CheckField::Handshake, CheckStatus::Pass);
    report.set_result(ValidationResult::Pass);

    let toml = report.to_toml().expect("serialize");
    assert!(toml.contains("profile_policy = \"upstream-pm58-4.07\""));
    assert!(toml.contains("pm = 58"));
    assert!(toml.contains("response_bytes = 8"));
    assert!(toml.contains("userspace_submit_bytes = 513"));
    assert!(toml.contains("protocol_chunk_bytes = 512"));

    let parsed = HardwareValidationReport::from_toml(&toml).expect("parse");
    assert_eq!(parsed.negotiated.as_ref().unwrap().pm, 58);
    assert_eq!(
        parsed.negotiated.as_ref().unwrap().profile_policy,
        "upstream-pm58-4.07"
    );
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

    let mut report = HardwareValidationReport::new_in_progress();
    report.set_fingerprint(&hid_in_fingerprint(), false, Some(true));
    report.record_negotiated_type2(&obs, Some(&pm68_device_info()));
    report.record_check(CheckField::Handshake, CheckStatus::Pass);
    report.fail_at(
        ValidationStage::ActiveWrite,
        ["PM68 observed; active output not evidenced"],
    );

    let toml = report.to_toml().expect("serialize");
    assert!(toml.contains("profile_policy = \"observed-pm68-conservative-stop\""));
    assert!(toml.contains("active_writes_allowed = false"));
    assert!(toml.contains("result = \"fail\""));
    assert!(toml.contains("failed_step = \"active_write\""));
    assert!(toml.contains("fbl = 192"));

    let parsed = HardwareValidationReport::from_toml(&toml).expect("parse");
    assert_eq!(parsed.result, Some(ValidationResult::Fail));
    assert!(!parsed.checks.passed(CheckField::TargetMarker));
}

#[test]
fn golden_direct_hidraw_short_read_count() {
    let mut report = HardwareValidationReport::new_in_progress();
    report.set_fingerprint(&hid_in_fingerprint(), false, Some(true));
    report.set_hid_backend_contract(LINUX_HIDRAW_BACKEND_CONTRACT);
    report.record_hid_read(&HidReadObservation {
        read_capacity_bytes: 512,
        read_timeout_ms: 500,
        transport_return_bytes: 8,
        protocol_response_bytes: 8,
    });

    let toml = report.to_toml().expect("serialize");
    assert!(toml.contains("read_capacity_bytes = 512"));
    assert!(toml.contains("transport_return_bytes = 8"));
    assert!(toml.contains("protocol_response_bytes = 8"));
    assert!(!toml.contains("logical_output_report_bytes = 8"));

    let parsed = HardwareValidationReport::from_toml(&toml).expect("parse");
    let read = parsed.hid_report.unwrap().read.unwrap();
    assert_eq!(read.transport_return_bytes, 8);
    assert_eq!(read.read_capacity_bytes, 512);
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

    let mut report = HardwareValidationReport::new_in_progress();
    report.set_fingerprint(&hid_in_fingerprint(), false, Some(true));
    report.record_hid_chunked_write_failure(&failure);
    report.fail_at(ValidationStage::ActiveWrite, ["unexpected HID write count"]);

    let toml = report.to_toml().expect("serialize");
    assert!(toml.contains("error_kind = \"unexpected_count\""));
    assert!(toml.contains("completed_chunks"));
    assert!(toml.contains("failing_chunk"));
    assert!(toml.contains("transport_return_bytes = 513"));

    let parsed = HardwareValidationReport::from_toml(&toml).expect("parse");
    let write_failure = parsed.hid_report.unwrap().write_failure.unwrap();
    assert_eq!(write_failure.completed_chunks.len(), 1);
    assert_eq!(
        write_failure.completed_chunks[0].transport_return_bytes,
        513
    );
    assert_eq!(
        write_failure.failing_chunk.unwrap().transport_return_bytes,
        8
    );
}

#[test]
fn aborted_result_serializes_incrementally() {
    let mut report = HardwareValidationReport::new_in_progress();
    report.set_fingerprint(&hid_in_fingerprint(), true, None);
    report.record_check(CheckField::Enumerated, CheckStatus::Pass);
    report.abort_at(ValidationStage::Selection, ["ambiguous duplicate VID:PID"]);

    let toml = report.to_toml().expect("serialize");
    assert!(toml.contains("result = \"aborted\""));
    assert!(toml.contains("serial_present = true"));
    assert!(!toml.contains("serial ="));

    let parsed = HardwareValidationReport::from_toml(&toml).expect("parse");
    assert_eq!(parsed.result, Some(ValidationResult::Aborted));
    assert!(parsed.fingerprint.unwrap().serial_present);
}

#[test]
fn hostile_error_forces_shareable_false() {
    let mut report = HardwareValidationReport::new_in_progress();
    report.fail_at(
        ValidationStage::HidrawCorrelation,
        ["correlation failed for /dev/hidraw3 busnum=2 devnum=7 /home/mike/sys"],
    );

    assert!(!report.shareable);
    let message = &report.failure.as_ref().unwrap().errors[0].message;
    assert!(!message.contains("/home/mike"));
    assert!(!message.contains("/dev/hidraw3"));
}

#[test]
fn sanitize_hostile_redaction_golden() {
    let outcome = sanitize_free_text(
        "opened /dev/hidraw3 busnum=1 devnum=4 serial=SECRET /home/mike/.config",
    );
    assert!(!outcome.provably_safe);
    assert!(!outcome.text.contains("SECRET"));
    assert!(!outcome.text.contains("/home/mike"));
}

#[test]
fn missing_checks_never_count_as_pass() {
    let mut report = HardwareValidationReport::new_in_progress();
    report.record_check(CheckField::Enumerated, CheckStatus::Pass);
    report.set_result(ValidationResult::Pass);

    assert!(report.checks.passed(CheckField::Enumerated));
    assert!(!report.checks.passed(CheckField::Handshake));
    assert!(!report.checks.passed(CheckField::Soak));
}

#[test]
fn endpoint_packet_size_not_serialized_as_report_length() {
    let mut report = HardwareValidationReport::new_in_progress();
    report.set_fingerprint(&hid_in_fingerprint(), false, None);
    report.set_hid_backend_contract(LINUX_HIDRAW_BACKEND_CONTRACT);
    if let Some(hid) = report.hid_report.as_mut() {
        hid.endpoint_max_packet_size = Some(8);
        hid.logical_output_report_bytes = Some(512);
    }

    let toml = report.to_toml().expect("serialize");
    assert!(toml.contains("max_packet_size = 8"));
    assert!(toml.contains("logical_output_report_bytes = 512"));
    assert!(!toml.contains("logical_output_report_bytes = 8"));
}

#[test]
fn deterministic_round_trip_preserves_schema_fields() {
    let mut report = HardwareValidationReport::new_in_progress();
    report.set_fingerprint(&hid_in_fingerprint(), false, Some(true));
    report.set_result(ValidationResult::Pass);

    let first = report.to_toml().expect("serialize");
    let second = HardwareValidationReport::from_toml(&first)
        .expect("parse")
        .to_toml()
        .expect("re-serialize");
    assert_eq!(first, second);
    assert!(first.contains("schema = 1"));
    assert!(first.contains("thermalwriter_version = \"0.1.4\""));
}
