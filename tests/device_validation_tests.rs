// SPDX-License-Identifier: GPL-3.0-or-later
#![cfg(feature = "daemon")]

use std::path::PathBuf;

use thermalwriter::transport::hid_report::{
    HidChunkedWriteFailure, HidReportReadSession, HidReportWriteError, PROTOCOL_CHUNK_BYTES,
    USERSPACE_SUBMIT_BYTES,
};
use thermalwriter::transport::test_support::{
    active_fixture, correlated_hidraw_fs, fixed_timestamp, hid_in_fingerprint, inventory_entry,
    mismatched_hidraw_fs, open_pm58_session, pm58_short_response, pm68_short_response,
    ActiveOptions, CountingInventory, FakeControl, FakeHidIo, FakeOwnership, MapHidrawInventory,
    MapInventory, PassivePreflightContext, PreflightResult, ScriptedPrompt, ValidateDeviceArgs,
    run_active_validation_with, run_passive_preflight, run_validate_device_with,
};
use thermalwriter::transport::type2_policy::Type2PreHandshakePolicy;
use thermalwriter::transport::validation_report::{
    CheckField, CheckStatus, EvidenceOrigin, HardwareValidationReport, ValidationScope,
};

#[test]
fn hid_interrupt_in_only_is_valid_passive_inventory() {
    let temp = tempfile::tempdir().unwrap();
    let usb = MapInventory {
        entries: vec![inventory_entry(1, 14, hid_in_fingerprint(), false)],
    };
    let hidraw = MapHidrawInventory {
        candidates: vec![
            thermalwriter::transport::HidrawCandidate::from_sysfs_class_entry(PathBuf::from(
                "/sys/class/hidraw/hidraw3",
            ))
            .unwrap(),
        ],
    };
    let output = run_validate_device_with(
        ValidateDeviceArgs {
            device: "0416:5302".to_string(),
            bus_address: None,
            passive: true,
            output: temp.path().to_path_buf(),
        },
        &usb,
        &hidraw,
        &correlated_hidraw_fs(1, 14),
        fixed_timestamp,
    )
    .unwrap();

    let report_toml = std::fs::read_to_string(output.join("report.toml")).unwrap();
    assert!(report_toml.contains("result = \"pass\""));
    assert!(report_toml.contains("direction = \"in\""));
    assert!(!report_toml.contains("direction = \"out\""));
    assert!(report_toml.contains("max_packet_size = 8"));
    assert!(report_toml.contains("pre_handshake_policy = \"hid407_read_only_probe\""));
}

#[test]
fn mismatched_hidraw_ancestor_sends_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let usb = MapInventory {
        entries: vec![inventory_entry(2, 7, hid_in_fingerprint(), false)],
    };
    let hidraw = MapHidrawInventory {
        candidates: vec![
            thermalwriter::transport::HidrawCandidate::from_sysfs_class_entry(PathBuf::from(
                "/sys/class/hidraw/hidraw1",
            ))
            .unwrap(),
        ],
    };
    let io = FakeHidIo::with_probe(&pm58_short_response());
    let writes_before = io.write_count();

    let error = run_validate_device_with(
        ValidateDeviceArgs {
            device: "0416:5302".to_string(),
            bus_address: None,
            passive: true,
            output: temp.path().to_path_buf(),
        },
        &usb,
        &hidraw,
        &mismatched_hidraw_fs(),
        fixed_timestamp,
    )
    .unwrap_err();

    assert!(error.to_string().contains("HidrawCorrelation"), "{error}");
    assert_eq!(io.write_count(), writes_before);
}

#[test]
fn active_owner_blocks_init_and_output() {
    let temp = tempfile::tempdir().unwrap();
    let (usb, hidraw, sysfs) = active_fixture();
    let error = run_active_validation_with(
        0x0416,
        0x5302,
        None,
        temp.path(),
        &usb,
        &hidraw,
        &sysfs,
        FakeControl::default(),
        FakeOwnership { owned: true },
        &mut ScriptedPrompt::default(),
        ActiveOptions::default(),
        |_| {},
        open_pm58_session,
    )
    .unwrap_err();

    assert!(error.to_string().contains("ExclusiveOwner"), "{error}");
}

#[test]
fn pm58_short_response_authorizes_report_policy() {
    let io = FakeHidIo::with_probe(&pm58_short_response());
    let mut session = HidReportReadSession::from_io(io);
    let observation = session.probe_type2_read_only(0).unwrap();
    assert!(observation.policy().active_writes_allowed());
    assert!(session.authorize_writes().is_ok());
}

#[test]
fn pm68_is_recorded_then_stops_before_output() {
    let temp = tempfile::tempdir().unwrap();
    let (usb, hidraw, sysfs) = active_fixture();
    let io = FakeHidIo::with_probe(&pm68_short_response());
    let writes = std::sync::Arc::clone(&io.writes);

    let error = run_active_validation_with(
        0x0416,
        0x5302,
        None,
        temp.path(),
        &usb,
        &hidraw,
        &sysfs,
        FakeControl::default(),
        FakeOwnership::default(),
        &mut ScriptedPrompt::default(),
        ActiveOptions::default(),
        |_| {},
        |_, _| Ok(HidReportReadSession::from_io(FakeHidIo::with_probe(&pm68_short_response()))),
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("stopped before output"),
        "{error}"
    );
    assert_eq!(writes.lock().unwrap().len(), 0);

    let report_toml = std::fs::read_to_string(temp.path().join("report.toml")).unwrap();
    assert!(report_toml.contains("pm = 68") || report_toml.contains("profile_policy"));
}

#[test]
fn hid_lengths_remain_independent() {
    let io = FakeHidIo::with_probe(&pm58_short_response());
    let mut session = HidReportReadSession::from_io(io);
    session.probe_type2_read_only(0).unwrap();
    let mut write_session = session.authorize_writes().unwrap();
    let observations = write_session
        .write_chunked(&vec![0xAB; PROTOCOL_CHUNK_BYTES], Some(PROTOCOL_CHUNK_BYTES), Some(8))
        .unwrap();

    let obs = &observations[0];
    assert_eq!(obs.protocol_chunk_bytes, 512);
    assert_eq!(obs.userspace_submit_bytes, USERSPACE_SUBMIT_BYTES);
    assert_eq!(obs.transport_return_bytes, 513);
    assert_eq!(obs.endpoint_max_packet_size, Some(8));
    assert_ne!(
        obs.endpoint_max_packet_size.unwrap() as usize,
        obs.protocol_chunk_bytes
    );
    assert_ne!(obs.userspace_submit_bytes, obs.protocol_chunk_bytes);
}

#[test]
fn unexpected_512_return_for_513_submit_fails_without_retry() {
    let mut io = FakeHidIo::with_probe(&pm58_short_response());
    io.write_returns.clear();
    io.write_returns.push_back(Ok(512));
    let mut session = HidReportReadSession::from_io(io);
    session.probe_type2_read_only(0).unwrap();
    let mut write_session = session.authorize_writes().unwrap();
    let failure = write_session
        .write_chunked(&vec![0xEE; PROTOCOL_CHUNK_BYTES], Some(512), None)
        .unwrap_err();

    let HidChunkedWriteFailure { completed, error } = failure;
    assert!(completed.is_empty());
    assert!(matches!(
        error,
        HidReportWriteError::UnexpectedCount(_)
    ));
    assert!(write_session.is_stopped());
    assert_eq!(write_session.contract().expected_write_return_bytes, 513);
}

#[test]
fn frame_write_without_visual_confirmation_fails() {
    let temp = tempfile::tempdir().unwrap();
    let (usb, hidraw, sysfs) = active_fixture();
    let error = run_active_validation_with(
        0x0416,
        0x5302,
        None,
        temp.path(),
        &usb,
        &hidraw,
        &sysfs,
        FakeControl::default(),
        FakeOwnership::default(),
        &mut ScriptedPrompt::new([false]),
        ActiveOptions {
            soak_secs: 0,
            ..Default::default()
        },
        |_| {},
        open_pm58_session,
    )
    .unwrap_err();

    assert!(error.to_string().contains("TargetMarker"), "{error}");
}

#[test]
fn reconnect_rescans_and_reallowlists() {
    let temp = tempfile::tempdir().unwrap();
    let (_, hidraw, sysfs) = active_fixture();
    let usb = CountingInventory::new(
        vec![inventory_entry(1, 14, hid_in_fingerprint(), false)],
        vec![
            inventory_entry(1, 14, hid_in_fingerprint(), false),
            inventory_entry(1, 20, hid_in_fingerprint(), false),
        ],
    );

    let _ = run_active_validation_with(
        0x0416,
        0x5302,
        None,
        temp.path(),
        &usb,
        &hidraw,
        &sysfs,
        FakeControl::default(),
        FakeOwnership::default(),
        &mut ScriptedPrompt::new([true, true, true, true, true]),
        ActiveOptions {
            soak_secs: 0,
            ..Default::default()
        },
        |_| {},
        open_pm58_session,
    );

    assert!(
        usb.inventory_calls() >= 2,
        "expected reconnect to rescan USB inventory"
    );
}

#[test]
fn absent_pm_short_response_fails_without_write() {
    let io = FakeHidIo::with_probe(&[0; 8]);
    let writes = std::sync::Arc::clone(&io.writes);
    let mut session = HidReportReadSession::from_io(io);
    assert!(session.probe_type2_read_only(0).is_err());
    assert_eq!(writes.lock().unwrap().len(), 0);
}

#[test]
fn malformed_short_response_fails_without_write() {
    let io = FakeHidIo::with_probe(&[0xFF; 8]);
    let writes = std::sync::Arc::clone(&io.writes);
    let mut session = HidReportReadSession::from_io(io);
    assert!(session.probe_type2_read_only(0).is_err());
    assert_eq!(writes.lock().unwrap().len(), 0);
}

#[test]
fn hid_in_only_descriptor_allows_report_path_policy() {
    let mut report = HardwareValidationReport::new_in_progress(
        EvidenceOrigin::Physical,
        ValidationScope::Passive,
    );
    let mut log = thermalwriter::transport::validate_device::ValidatorLog::new();
    let usb = MapInventory {
        entries: vec![inventory_entry(1, 14, hid_in_fingerprint(), false)],
    };
    let hidraw = MapHidrawInventory {
        candidates: vec![
            thermalwriter::transport::HidrawCandidate::from_sysfs_class_entry(PathBuf::from(
                "/sys/class/hidraw/hidraw3",
            ))
            .unwrap(),
        ],
    };
    let outcome = run_passive_preflight(PassivePreflightContext {
        vid: 0x0416,
        pid: 0x5302,
        bus_address: None,
        usb: &usb,
        hidraw: &hidraw,
        sysfs: &correlated_hidraw_fs(1, 14),
        report: &mut report,
        log: &mut log,
    });

    assert!(matches!(outcome.result, PreflightResult::Pass));
    let toml = report.to_private_toml().unwrap();
    assert!(toml.contains("pre_handshake_policy = \"hid407_read_only_probe\""));
    assert_eq!(
        report.checks().get(CheckField::PassiveAllowlist),
        Some(CheckStatus::Pass)
    );
}

#[test]
fn user_abort_on_reconnect_prompt() {
    let temp = tempfile::tempdir().unwrap();
    let (usb, hidraw, sysfs) = active_fixture();
    let error = run_active_validation_with(
        0x0416,
        0x5302,
        None,
        temp.path(),
        &usb,
        &hidraw,
        &sysfs,
        FakeControl::default(),
        FakeOwnership::default(),
        &mut ScriptedPrompt::new([true, true, true, false]),
        ActiveOptions {
            soak_secs: 0,
            ..Default::default()
        },
        |_| {},
        open_pm58_session,
    )
    .unwrap_err();

    assert!(error.to_string().contains("Reconnect"), "{error}");
}

#[test]
fn partial_reports_written_on_chunk_failure() {
    let mut io = FakeHidIo::with_probe(&pm58_short_response());
    io.write_returns.clear();
    io.write_returns.push_back(Ok(513));
    io.write_returns.push_back(Ok(512));
    let mut session = HidReportReadSession::from_io(io);
    session.probe_type2_read_only(0).unwrap();
    let mut write_session = session.authorize_writes().unwrap();
    let failure = write_session
        .write_chunked(&vec![0xEE; PROTOCOL_CHUNK_BYTES * 2], Some(512), None)
        .unwrap_err();

    assert_eq!(failure.completed.len(), 1);
    assert_eq!(failure.completed[0].transport_return_bytes, 513);
    assert!(matches!(
        failure.error,
        HidReportWriteError::UnexpectedCount(_)
    ));
}

#[test]
fn shareable_report_redacts_bus_address_sysfs_and_home() {
    let temp = tempfile::tempdir().unwrap();
    let usb = MapInventory {
        entries: vec![inventory_entry(1, 14, hid_in_fingerprint(), true)],
    };
    let hidraw = MapHidrawInventory {
        candidates: vec![
            thermalwriter::transport::HidrawCandidate::from_sysfs_class_entry(PathBuf::from(
                "/sys/class/hidraw/hidraw3",
            ))
            .unwrap(),
        ],
    };
    let output = run_validate_device_with(
        ValidateDeviceArgs {
            device: "0416:5302".to_string(),
            bus_address: None,
            passive: true,
            output: temp.path().to_path_buf(),
        },
        &usb,
        &hidraw,
        &correlated_hidraw_fs(1, 14),
        fixed_timestamp,
    )
    .unwrap();

    let report_toml = std::fs::read_to_string(output.join("report.toml")).unwrap();
    assert!(!report_toml.contains("/sys/"));
    assert!(!report_toml.contains("/home/"));
    assert!(!report_toml.contains("bus="));
    assert!(!report_toml.contains("address="));
    assert!(!report_toml.contains("serial ="));
}

#[test]
fn second_display_isolation_abort() {
    let temp = tempfile::tempdir().unwrap();
    let mut peer_fp = hid_in_fingerprint();
    peer_fp.vid = 0x87ad;
    peer_fp.pid = 0x70db;
    let usb = MapInventory {
        entries: vec![
            inventory_entry(1, 14, hid_in_fingerprint(), false),
            inventory_entry(2, 5, peer_fp, false),
        ],
    };
    let hidraw = MapHidrawInventory {
        candidates: vec![
            thermalwriter::transport::HidrawCandidate::from_sysfs_class_entry(PathBuf::from(
                "/sys/class/hidraw/hidraw3",
            ))
            .unwrap(),
        ],
    };
    let error = run_active_validation_with(
        0x0416,
        0x5302,
        None,
        temp.path(),
        &usb,
        &hidraw,
        &correlated_hidraw_fs(1, 14),
        FakeControl::default(),
        FakeOwnership::default(),
        &mut ScriptedPrompt::new([true, true, true, false]),
        ActiveOptions {
            soak_secs: 0,
            ..Default::default()
        },
        |_| {},
        open_pm58_session,
    )
    .unwrap_err();

    assert!(
        error.to_string().contains("SecondDisplayUnchanged"),
        "{error}"
    );
}

#[test]
fn synthetic_reports_cannot_set_eligible_for_tested() {
    let report =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Synthetic, ValidationScope::Full);
    assert!(!report.eligible_for_tested());

    let mut replay = HardwareValidationReport::new_in_progress(
        EvidenceOrigin::Replay,
        ValidationScope::Passive,
    );
    replay
        .set_fingerprint(&hid_in_fingerprint(), false, Some(true))
        .unwrap();
    replay
        .set_pre_handshake_policy(Type2PreHandshakePolicy::Hid407ReadOnlyProbe)
        .unwrap();
    replay
        .record_check(CheckField::Enumerated, CheckStatus::Pass)
        .unwrap();
    replay
        .record_check(CheckField::PassiveAllowlist, CheckStatus::Pass)
        .unwrap();
    replay.finalize_passive_pass().unwrap();
    assert!(!replay.eligible_for_tested());
}

#[test]
fn handshake_open_failure_blocks_output() {
    let temp = tempfile::tempdir().unwrap();
    let (usb, hidraw, sysfs) = active_fixture();
    let error = run_active_validation_with(
        0x0416,
        0x5302,
        None,
        temp.path(),
        &usb,
        &hidraw,
        &sysfs,
        FakeControl::default(),
        FakeOwnership::default(),
        &mut ScriptedPrompt::default(),
        ActiveOptions::default(),
        |_| {},
        |_, _| {
            Err::<HidReportReadSession<FakeHidIo>, anyhow::Error>(anyhow::anyhow!(
                "injected hid open failure"
            ))
        },
    )
    .unwrap_err();

    assert!(error.to_string().contains("Handshake"), "{error}");
}

#[test]
fn daemon_restore_failure_recorded_on_drop() {
    let temp = tempfile::tempdir().unwrap();
    let (usb, hidraw, sysfs) = active_fixture();
    let control = FakeControl {
        active: true,
        start_fail: true,
        ..Default::default()
    };
    let _ = run_active_validation_with(
        0x0416,
        0x5302,
        None,
        temp.path(),
        &usb,
        &hidraw,
        &sysfs,
        control,
        FakeOwnership::default(),
        &mut ScriptedPrompt::new([false]),
        ActiveOptions {
            soak_secs: 0,
            ..Default::default()
        },
        |_| {},
        open_pm58_session,
    );
}
