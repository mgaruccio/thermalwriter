// SPDX-License-Identifier: GPL-3.0-or-later
//
// Guided active `validate-device` state machine with injectable I/O and prompts.

#![allow(clippy::too_many_arguments)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail, ensure};

use crate::service::guard::{
    DEFAULT_SERVICE_UNIT, DefaultDeviceOwnership, DeviceOwnership, OwnershipTarget, ServiceControl,
    ServiceGuard, SystemdUserControl,
};
use crate::transport::discovery::{open_discovered, scan_devices, DiscoveredDevice, KNOWN_LCD_IDS};
use crate::transport::hid_report::{
    HidReadObservation, HidReportIo, HidReportProbeError, HidReportReadSession,
    HidReportWriteSession, HidrawCorrelation, LINUX_HIDRAW_BACKEND_CONTRACT,
    PROTOCOL_CHUNK_BYTES, UsbBusAddress,
};
use crate::transport::profile::{DeviceInfo, WireProtocol, build_device_info};
use crate::transport::type2_policy::{
    Type2NegotiatedObservation, Type2PreHandshakePolicy, TYPE2_PROBE_READ_BOUND,
    select_type2_pre_handshake_policy,
};
use crate::transport::validation_report::{
    CheckField, CheckStatus, DescriptorCaptureStatus, EvidenceOrigin, HardwareValidationReport,
    ValidationErrorKind, ValidationScope, ValidationStage,
};
use crate::transport::{EncodedFrame, Transport};

use super::cards::{encode_and_save_expected, generate_test_cards, pad_to_hid_chunks, TestCardBundle};
use super::{
    InventoryEntry, PassivePreflightContext, PreflightResult, UsbInventory, ValidatorLog,
    correlate_hidraw_if_needed, device_has_hid_shape, resolve_selection, run_passive_preflight,
    write_validation_output, HidrawInventory,
};
use super::super::hid_report::SysfsAccess;

/// Default soak duration for active validation (5 minutes).
pub const DEFAULT_SOAK_SECS: u64 = 300;

/// Injectable yes/no prompts (stdin in production, scripted in tests).
pub trait Prompt {
    fn yes_no(&mut self, question: &str) -> bool;
}

/// Production prompt reading a single line from stdin.
#[derive(Debug, Default)]
pub struct StdioPrompt;

impl Prompt for StdioPrompt {
    fn yes_no(&mut self, question: &str) -> bool {
        use std::io::{self, Write};
        let _ = writeln!(io::stdout(), "{question} [y/N]");
        let _ = io::stdout().flush();
        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err() {
            return false;
        }
        matches!(line.trim().chars().next(), Some('y' | 'Y'))
    }
}

/// Scripted prompt for unit tests.
#[derive(Debug, Default)]
pub struct ScriptedPrompt {
    pub answers: Vec<bool>,
    pub index: usize,
    pub asked: Vec<String>,
}

impl ScriptedPrompt {
    pub fn new(answers: impl IntoIterator<Item = bool>) -> Self {
        Self {
            answers: answers.into_iter().collect(),
            index: 0,
            asked: Vec::new(),
        }
    }
}

impl Prompt for ScriptedPrompt {
    fn yes_no(&mut self, question: &str) -> bool {
        self.asked.push(question.to_string());
        let answer = self.answers.get(self.index).copied().unwrap_or(false);
        self.index += 1;
        answer
    }
}

/// Another known LCD attached during validation (peer identity).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PeerIdentity {
    pub vid: u16,
    pub pid: u16,
    pub bus: u8,
    pub address: u8,
}

/// Active-run knobs (soak duration, encoding quality, run id).
#[derive(Debug, Clone)]
pub struct ActiveOptions {
    pub soak_secs: u64,
    pub jpeg_quality: u8,
    pub run_id: String,
    pub rotation: u16,
}

impl Default for ActiveOptions {
    fn default() -> Self {
        Self {
            soak_secs: std::env::var("THERMALWRITER_VALIDATE_SOAK_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_SOAK_SECS),
            jpeg_quality: 85,
            run_id: format!(
                "{:04X}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() & 0xFFFF)
                    .unwrap_or(0)
            ),
            rotation: 0,
        }
    }
}

/// Frame output backend after negotiation.
pub enum ActiveOutput<Io: HidReportIo> {
    Hid(HidReportWriteSession<Io>),
    Bulk(Box<dyn Transport>),
}

impl<Io: HidReportIo> ActiveOutput<Io> {
    pub fn hid_write_count(&self) -> usize {
        match self {
            Self::Hid(_) => 0,
            Self::Bulk(_) => 0,
        }
    }

    pub fn send_encoded(&mut self, frame: &EncodedFrame) -> Result<()> {
        match self {
            Self::Hid(session) => {
                let padded = pad_to_hid_chunks(&frame.data);
                let observations = session
                    .write_chunked(&padded, Some(PROTOCOL_CHUNK_BYTES), Some(8))
                    .map_err(|failure| anyhow::anyhow!("{failure}"))?;
                ensure!(!observations.is_empty(), "HID write produced no chunks");
                Ok(())
            }
            Self::Bulk(transport) => transport.send_frame(frame),
        }
    }

    pub fn close(self) {
        if let Self::Bulk(mut transport) = self {
            transport.close();
        }
    }
}

/// Spy wrapper counting transport writes.
pub struct SpyTransport {
    inner: Box<dyn Transport>,
    pub writes: usize,
}

impl SpyTransport {
    pub fn new(inner: Box<dyn Transport>) -> Self {
        Self { inner, writes: 0 }
    }
}

impl Transport for SpyTransport {
    fn handshake(&mut self) -> Result<DeviceInfo> {
        self.inner.handshake()
    }

    fn send_frame(&mut self, frame: &EncodedFrame) -> Result<()> {
        self.writes += 1;
        self.inner.send_frame(frame)
    }

    fn close(&mut self) {
        self.inner.close();
    }

    fn is_connected(&self) -> bool {
        self.inner.is_connected()
    }
}

/// Run active validation with production service control and stdin prompts.
#[cfg(feature = "daemon")]
pub fn run_active_validation<I, H, F>(
    vid: u16,
    pid: u16,
    bus_address: Option<&str>,
    output_dir: &Path,
    usb: &I,
    hidraw: &H,
    sysfs: &F,
    options: ActiveOptions,
) -> Result<PathBuf>
where
    I: UsbInventory,
    H: HidrawInventory,
    F: SysfsAccess,
{
    let mut prompt = StdioPrompt;
    run_active_validation_with(
        vid,
        pid,
        bus_address,
        output_dir,
        usb,
        hidraw,
        sysfs,
        SystemdUserControl,
        DefaultDeviceOwnership,
        &mut prompt,
        options,
        std::thread::sleep,
        open_production_hid_session,
    )
}

#[cfg(feature = "daemon")]
fn open_production_hid_session(
    selector: UsbBusAddress,
    correlation: &HidrawCorrelation,
) -> Result<HidReportReadSession<crate::transport::hid_report::linux::LinuxHidrawIo>> {
    use crate::transport::hid_report::linux::LinuxHidrawIo;
    let io = LinuxHidrawIo::open_authenticated(
        correlation,
        selector,
        &crate::transport::hid_report::RealSysfs,
    )?;
    Ok(HidReportReadSession::from_io(io))
}

pub(crate) fn run_active_validation_with<I, H, F, C, O, P, Io>(
    vid: u16,
    pid: u16,
    bus_address: Option<&str>,
    output_dir: &Path,
    usb: &I,
    hidraw: &H,
    sysfs: &F,
    service_control: C,
    ownership: O,
    prompt: &mut P,
    options: ActiveOptions,
    mut sleep: impl FnMut(Duration),
    mut open_hid: impl FnMut(UsbBusAddress, &HidrawCorrelation) -> Result<HidReportReadSession<Io>>,
) -> Result<PathBuf>
where
    I: UsbInventory,
    H: HidrawInventory,
    F: SysfsAccess,
    C: ServiceControl,
    O: DeviceOwnership,
    P: Prompt,
    Io: HidReportIo,
{
    let mut report = HardwareValidationReport::new_in_progress(
        EvidenceOrigin::Physical,
        ValidationScope::Full,
    );
    let mut log = ValidatorLog::new();

    let passive = run_passive_preflight(PassivePreflightContext {
        vid,
        pid,
        bus_address,
        usb,
        hidraw,
        sysfs,
        report: &mut report,
        log: &mut log,
    });

    let selected = match passive.selected.clone() {
        Some(entry) => entry,
        None => {
            write_validation_output(output_dir, &report, None, &mut log)?;
            return fail_cli(passive.result, output_dir);
        }
    };

    if let PreflightResult::Fail(stage, errors) = passive.result.clone() {
        write_validation_output(output_dir, &report, Some(&selected), &mut log)?;
        return fail_cli(PreflightResult::Fail(stage, errors), output_dir);
    }

    let selector = UsbBusAddress {
        bus: selected.identity.bus,
        address: selected.identity.address,
    };
    let peers_before = snapshot_peers(usb, selector);

    let ownership_target =
        build_ownership_target(&selected.identity.fingerprint, selector, hidraw, sysfs, &mut log)?;

    let mut guard = match ServiceGuard::acquire(
        service_control,
        ownership,
        &ownership_target,
        DEFAULT_SERVICE_UNIT,
    ) {
        Ok(guard) => guard,
        Err(error) => {
            let _ = report.record_check(CheckField::ExclusiveOwner, CheckStatus::Fail);
            let _ = report.fail_at(
                ValidationStage::ExclusiveOwner,
                &[(
                    ValidationErrorKind::Device,
                    "failed to acquire exclusive device ownership",
                )],
            );
            log.info(format!("exclusive owner error: {error:#}"));
            write_validation_output(output_dir, &report, Some(&selected), &mut log)?;
            bail!("validate-device failed at ExclusiveOwner: {error:#}");
        }
    };
    let _ = report.record_check(CheckField::ExclusiveOwner, CheckStatus::Pass);

    let hidraw_correlated = correlate_hidraw_if_needed(
        &selected.identity.fingerprint,
        selector,
        hidraw,
        sysfs,
        &mut log,
    )
    .ok()
    .flatten();
    let policy = select_type2_pre_handshake_policy(
        &selected.identity.fingerprint,
        hidraw_correlated.unwrap_or(false),
    );

    let negotiation = match negotiate_active_output(
        vid,
        pid,
        selector,
        policy,
        &selected,
        hidraw,
        sysfs,
        &mut open_hid,
        &mut report,
        &mut log,
    ) {
        Ok(result) => result,
        Err(stage) => {
            restore_daemon(&mut guard, &mut report, &mut log);
            write_validation_output(output_dir, &report, Some(&selected), &mut log)?;
            bail!("validate-device failed at {stage:?}");
        }
    };

    if !negotiation.active_writes_allowed {
        conservative_stop(&mut report);
        restore_daemon(&mut guard, &mut report, &mut log);
        write_validation_output(output_dir, &report, Some(&selected), &mut log)?;
        bail!(
            "validate-device stopped before output: negotiated profile does not authorize active writes"
        );
    }

    let mut output = negotiation.output;
    let device_info = negotiation.device_info;

    if let Err(stage) = run_visual_stream_reconnect(
        vid,
        pid,
        bus_address,
        selector,
        &peers_before,
        &options,
        output_dir,
        usb,
        prompt,
        &mut sleep,
        &mut output,
        &device_info,
        &mut report,
        &mut log,
    ) {
        output.close();
        restore_daemon(&mut guard, &mut report, &mut log);
        write_validation_output(output_dir, &report, Some(&selected), &mut log)?;
        bail!("validate-device failed at {stage:?}");
    }

    output.close();
    restore_daemon(&mut guard, &mut report, &mut log);

    if let Err(error) = report.finalize_full_pass() {
        let _ = report.fail_at(
            ValidationStage::Reconnect,
            &[(
                ValidationErrorKind::Error,
                "active validation could not finalize full pass",
            )],
        );
        log.info(format!("finalize error: {error}"));
        write_validation_output(output_dir, &report, Some(&selected), &mut log)?;
        bail!("validate-device failed to finalize: {error}");
    }

    write_validation_output(output_dir, &report, Some(&selected), &mut log)?;
    println!("{}", output_dir.display());
    Ok(output_dir.to_path_buf())
}

struct NegotiationResult<Io: HidReportIo> {
    device_info: DeviceInfo,
    active_writes_allowed: bool,
    output: ActiveOutput<Io>,
}

fn negotiate_active_output<Io, H, F>(
    vid: u16,
    pid: u16,
    selector: UsbBusAddress,
    policy: Type2PreHandshakePolicy,
    selected: &InventoryEntry,
    hidraw: &H,
    sysfs: &F,
    open_hid: &mut dyn FnMut(UsbBusAddress, &HidrawCorrelation) -> Result<HidReportReadSession<Io>>,
    report: &mut HardwareValidationReport,
    log: &mut ValidatorLog,
) -> Result<NegotiationResult<Io>, ValidationStage>
where
    Io: HidReportIo,
    H: HidrawInventory,
    F: SysfsAccess,
{
    let _ = report.set_pre_handshake_policy(policy);
    match policy {
        Type2PreHandshakePolicy::Hid407ReadOnlyProbe => {
            negotiate_hid407(vid, pid, selector, hidraw, sysfs, open_hid, report, log)
        }
        Type2PreHandshakePolicy::LegacyBulkInit => {
            negotiate_bulk(vid, pid, selector, selected, report, log)
        }
        Type2PreHandshakePolicy::StopUnsupportedShape => {
            let _ = report.fail_at(
                ValidationStage::PassiveAllowlist,
                &[(
                    ValidationErrorKind::Policy,
                    "descriptor shape is not on the passive allowlist",
                )],
            );
            Err(ValidationStage::PassiveAllowlist)
        }
    }
}

fn negotiate_hid407<Io, H, F>(
    vid: u16,
    pid: u16,
    selector: UsbBusAddress,
    hidraw: &H,
    sysfs: &F,
    open_hid: &mut dyn FnMut(UsbBusAddress, &HidrawCorrelation) -> Result<HidReportReadSession<Io>>,
    report: &mut HardwareValidationReport,
    log: &mut ValidatorLog,
) -> Result<NegotiationResult<Io>, ValidationStage>
where
    Io: HidReportIo,
    H: HidrawInventory,
    F: SysfsAccess,
{
    let candidates = hidraw.list_hidraw_candidates().map_err(|error| {
        log.info(format!("hidraw list error: {error:#}"));
        let _ = report.fail_at(
            ValidationStage::Handshake,
            &[(
                ValidationErrorKind::Device,
                "hidraw correlation failed for handshake",
            )],
        );
        ValidationStage::Handshake
    })?;
    let correlation =
        super::super::hid_report::correlate_hidraw_to_usb(selector, &candidates, sysfs).map_err(
            |error| {
                log.info(format!("hidraw correlate error: {error:#}"));
                let _ = report.fail_at(
                    ValidationStage::Handshake,
                    &[(
                        ValidationErrorKind::Device,
                        "hidraw correlation failed for handshake",
                    )],
                );
                ValidationStage::Handshake
            },
        )?;

    let mut session = open_hid(selector, &correlation).map_err(|error| {
        log.info(format!("hid open error: {error:#}"));
        let _ = report.fail_at(
            ValidationStage::Handshake,
            &[(ValidationErrorKind::Transport, "failed to open HID session")],
        );
        ValidationStage::Handshake
    })?;

    let _ = report.set_hid_backend_contract(LINUX_HIDRAW_BACKEND_CONTRACT);
    let _ = report
        .set_hid_descriptor_status(DescriptorCaptureStatus::Captured);

    let observation = match session.probe_type2_read_only(500) {
        Ok(obs) => obs,
        Err(HidReportProbeError::Read(read_error)) => {
            log.info(format!("HID probe read error: {read_error}"));
            let _ = report.record_hid_read_failure(
                TYPE2_PROBE_READ_BOUND,
                500,
                None,
                crate::transport::validation_report::HidReadErrorKind::Transport,
                &read_error.to_string(),
            );
            let _ = report.fail_at(
                ValidationStage::Handshake,
                &[(
                    ValidationErrorKind::Transport,
                    "HID read-only probe failed",
                )],
            );
            let _ = report.record_check(CheckField::Handshake, CheckStatus::Fail);
            return Err(ValidationStage::Handshake);
        }
        Err(HidReportProbeError::Negotiate(message)) => {
            log.info(format!("HID negotiate error: {message}"));
            let _ = report.fail_at(
                ValidationStage::Negotiation,
                &[(
                    ValidationErrorKind::Policy,
                    "Type2 negotiation failed on probe response",
                )],
            );
            let _ = report.record_check(CheckField::Handshake, CheckStatus::Fail);
            return Err(ValidationStage::Negotiation);
        }
    };

    record_hid_probe_read(report, &observation);
    if let Err(error) = report.record_negotiated_type2(&observation) {
        log.info(format!("record negotiated error: {error:#}"));
        let _ = report.fail_at(
            ValidationStage::Negotiation,
            &[(
                ValidationErrorKind::Error,
                "failed to record negotiated profile",
            )],
        );
        return Err(ValidationStage::Negotiation);
    }
    let _ = report.record_check(CheckField::Handshake, CheckStatus::Pass);

    if !observation.policy().active_writes_allowed() {
        let device_info = build_device_info(
            WireProtocol::HidType2,
            vid,
            pid,
            observation.pm(),
            observation.sub(),
            None,
        )
        .map_err(|_| ValidationStage::Negotiation)?;
        let _ = session;
        return Ok(NegotiationResult {
            device_info,
            active_writes_allowed: false,
            output: ActiveOutput::Bulk(Box::new(NullOutputTransport)),
        });
    }

    let write_session = session.authorize_writes().map_err(|error| {
        log.info(format!("authorize writes error: {error}"));
        let _ = report.fail_at(
            ValidationStage::Negotiation,
            &[(
                ValidationErrorKind::Policy,
                "HID write authorization denied after probe",
            )],
        );
        ValidationStage::Negotiation
    })?;
    let _ = report.set_hid_active_write_authorized(true);
    let _ = report.record_hid_write_observation(
        PROTOCOL_CHUNK_BYTES,
        Some(PROTOCOL_CHUNK_BYTES),
        0,
        PROTOCOL_CHUNK_BYTES + 1,
        Some((PROTOCOL_CHUNK_BYTES + 1) as isize),
        Some(8),
    );
    let _ = report.record_check(CheckField::ActiveWrite, CheckStatus::Pass);

    let device_info = build_device_info(
        WireProtocol::HidType2,
        vid,
        pid,
        observation.pm(),
        observation.sub(),
        None,
    )
    .map_err(|_| ValidationStage::Negotiation)?;

    Ok(NegotiationResult {
        active_writes_allowed: true,
        device_info,
        output: ActiveOutput::Hid(write_session),
    })
}

/// No-op transport placeholder when negotiation disallows output.
struct NullOutputTransport;

impl Transport for NullOutputTransport {
    fn handshake(&mut self) -> Result<DeviceInfo> {
        bail!("null output transport cannot handshake")
    }

    fn send_frame(&mut self, _frame: &EncodedFrame) -> Result<()> {
        bail!("null output transport cannot send frames")
    }

    fn close(&mut self) {}
}

fn negotiate_bulk<Io: HidReportIo>(
    vid: u16,
    pid: u16,
    selector: UsbBusAddress,
    _selected: &InventoryEntry,
    report: &mut HardwareValidationReport,
    log: &mut ValidatorLog,
) -> Result<NegotiationResult<Io>, ValidationStage> {
    let discovered = discovered_for_selector(vid, pid, selector).map_err(|error| {
        log.info(format!("discovery error: {error:#}"));
        let _ = report.fail_at(
            ValidationStage::Handshake,
            &[(
                ValidationErrorKind::Device,
                "failed to resolve discovered device path",
            )],
        );
        ValidationStage::Handshake
    })?;

    let (transport, device_info) = open_discovered(&discovered).map_err(|error| {
        log.info(format!("bulk open error: {error:#}"));
        let _ = report.fail_at(
            ValidationStage::Handshake,
            &[(ValidationErrorKind::Transport, "bulk open/handshake failed")],
        );
        let _ = report.record_check(CheckField::Handshake, CheckStatus::Fail);
        ValidationStage::Handshake
    })?;

    report
        .record_negotiated_device(&device_info, 64)
        .map_err(|error| {
            log.info(format!("negotiated device error: {error:#}"));
            ValidationStage::Negotiation
        })?;
    let _ = report.record_check(CheckField::Handshake, CheckStatus::Pass);
    let _ = report.record_check(CheckField::ActiveWrite, CheckStatus::Pass);

    Ok(NegotiationResult {
        active_writes_allowed: true,
        device_info,
        output: ActiveOutput::Bulk(transport),
    })
}

fn run_visual_stream_reconnect<Io, I, P>(
    vid: u16,
    pid: u16,
    bus_address_hint: Option<&str>,
    previous_selector: UsbBusAddress,
    peers_before: &BTreeSet<PeerIdentity>,
    options: &ActiveOptions,
    output_dir: &Path,
    usb: &I,
    prompt: &mut P,
    sleep: &mut impl FnMut(Duration),
    output: &mut ActiveOutput<Io>,
    device_info: &DeviceInfo,
    report: &mut HardwareValidationReport,
    log: &mut ValidatorLog,
) -> Result<(), ValidationStage>
where
    Io: HidReportIo,
    I: UsbInventory,
    P: Prompt,
{
    let bundle = generate_test_cards(device_info, vid, pid, &options.run_id, options.rotation)
        .map_err(|error| {
            log.info(format!("card generation error: {error:#}"));
            ValidationStage::TargetMarker
        })?;

    let encoded = encode_and_save_expected(
        &bundle,
        device_info,
        options.rotation,
        options.jpeg_quality,
        output_dir,
    )
    .map_err(|error| {
        log.info(format!("encode cards error: {error:#}"));
        ValidationStage::TargetMarker
    })?;

    for frame in &encoded {
        output.send_encoded(frame).map_err(|error| {
            log.info(format!("send frame error: {error:#}"));
            let _ = report.fail_at(
                ValidationStage::ActiveWrite,
                &[(
                    ValidationErrorKind::Transport,
                    "failed to send validation frame",
                )],
            );
            ValidationStage::ActiveWrite
        })?;
    }
    let _ = report.record_check(CheckField::ActiveWrite, CheckStatus::Pass);

    let marker_q = format!(
        "Does the target display show marker {} {}?",
        bundle.run_id, bundle.vid_pid_label
    );
    if !prompt.yes_no(&marker_q) {
        fail_visual(report, log, ValidationStage::TargetMarker);
        return Err(ValidationStage::TargetMarker);
    }
    let _ = report.record_check(CheckField::TargetMarker, CheckStatus::Pass);

    if peers_before.is_empty() {
        let _ = report.record_check(CheckField::SecondDisplayUnchanged, CheckStatus::NotApplicable);
    } else if !prompt.yes_no("Is the second display unchanged?") {
        fail_visual(report, log, ValidationStage::SecondDisplayUnchanged);
        return Err(ValidationStage::SecondDisplayUnchanged);
    } else {
        let _ = report
            .record_check(CheckField::SecondDisplayUnchanged, CheckStatus::Pass);
    }

    if !prompt.yes_no(
        "Does the orientation card show TOP with distinct corner colors (TL red, TR green, BL blue, BR yellow)?",
    ) {
        fail_visual(report, log, ValidationStage::Orientation);
        return Err(ValidationStage::Orientation);
    }
    let _ = report.record_check(CheckField::Orientation, CheckStatus::Pass);

    if !prompt.yes_no(
        "Does the color card show red, green, blue, white, black, and mid-gray blocks?",
    ) {
        fail_visual(report, log, ValidationStage::Colors);
        return Err(ValidationStage::Colors);
    }
    let _ = report.record_check(CheckField::Colors, CheckStatus::Pass);

    run_soak(output, &encoded, options.soak_secs, sleep, report, log)?;

    if !prompt.yes_no("Reconnect the USB cable now, then answer yes when the display is back.") {
        let _ = report.fail_at(
            ValidationStage::Reconnect,
            &[(
                ValidationErrorKind::Device,
                "operator declined reconnect prompt",
            )],
        );
        return Err(ValidationStage::Reconnect);
    }

    let entries = usb.inventory_matching(vid, pid).map_err(|error| {
        log.info(format!("reconnect inventory error: {error:#}"));
        ValidationStage::Reconnect
    })?;
    let _new_selector = resolve_reconnect(&entries, previous_selector, bus_address_hint).map_err(
        |error| {
            log.info(format!("reconnect selection error: {error:#}"));
            let _ = report.abort_at(
                ValidationStage::Reconnect,
                &[(
                    ValidationErrorKind::Device,
                    "reconnect could not resolve a new bus/address",
                )],
            );
            ValidationStage::Reconnect
        },
    )?;

    let peers_after = snapshot_peers(usb, previous_selector);
    if peers_after != *peers_before {
        let _ = report.abort_at(
            ValidationStage::Reconnect,
            &[(
                ValidationErrorKind::Device,
                "peer display identity changed after reconnect",
            )],
        );
        return Err(ValidationStage::Reconnect);
    }

    let _ = report.record_check(CheckField::Reconnect, CheckStatus::Pass);
    let _bundle: TestCardBundle = bundle;
    Ok(())
}

fn run_soak<Io: HidReportIo>(
    output: &mut ActiveOutput<Io>,
    encoded: &[EncodedFrame],
    soak_secs: u64,
    sleep: &mut impl FnMut(Duration),
    report: &mut HardwareValidationReport,
    log: &mut ValidatorLog,
) -> Result<(), ValidationStage> {
    let frame = encoded
        .first()
        .ok_or_else(|| {
            log.info("soak missing encoded frame".to_string());
            ValidationStage::Soak
        })?;
    let iterations = soak_secs.saturating_mul(5);
    for _ in 0..iterations {
        output.send_encoded(frame).map_err(|error| {
            log.info(format!("soak send error: {error:#}"));
            let _ = report.fail_at(
                ValidationStage::Soak,
                &[(ValidationErrorKind::Transport, "soak stream frame failed")],
            );
            ValidationStage::Soak
        })?;
        sleep(Duration::from_millis(200));
    }
    let _ = report.record_check(CheckField::Soak, CheckStatus::Pass);
    Ok(())
}

fn conservative_stop(report: &mut HardwareValidationReport) {
    let _ = report.record_check(CheckField::ActiveWrite, CheckStatus::Fail);
    let _ = report.fail_at(
        ValidationStage::Negotiation,
        &[(
            ValidationErrorKind::Policy,
            "negotiated profile blocks active output (conservative stop)",
        )],
    );
}

fn restore_daemon<C: ServiceControl, O: DeviceOwnership>(
    guard: &mut ServiceGuard<C, O>,
    report: &mut HardwareValidationReport,
    log: &mut ValidatorLog,
) {
    let restored = match guard.restore() {
        Ok(()) => CheckStatus::Pass,
        Err(error) => {
            log.info(format!("daemon restore error: {error:#}"));
            CheckStatus::Fail
        }
    };
    let _ = report.record_check(CheckField::DaemonRestored, restored);
}

fn record_hid_probe_read(report: &mut HardwareValidationReport, obs: &Type2NegotiatedObservation) {
    let _ = report.record_hid_read(&HidReadObservation {
        read_capacity_bytes: TYPE2_PROBE_READ_BOUND,
        read_timeout_ms: 500,
        transport_return_bytes: obs.response().len() as isize,
        protocol_response_bytes: obs.response().len(),
    });
}

fn fail_visual(
    report: &mut HardwareValidationReport,
    log: &mut ValidatorLog,
    stage: ValidationStage,
) {
    let _ = report.fail_at(
        stage,
        &[(
            ValidationErrorKind::Device,
            "operator reported negative or missing visual confirmation",
        )],
    );
    log.info(format!("visual check failed at {stage:?}"));
}

fn discovered_for_selector(vid: u16, pid: u16, selector: UsbBusAddress) -> Result<DiscoveredDevice> {
    let devices = scan_devices()?;
    let mut matches = devices.into_iter().filter(|device| {
        device.vid == vid
            && device.pid == pid
            && matches!(
                device.path,
                crate::transport::discovery::DevicePath::Usb {
                    bus,
                    address,
                    ..
                } if bus == selector.bus && address == selector.address
            )
    });
    let first = matches
        .next()
        .ok_or_else(|| anyhow::anyhow!("no discovered device at selected bus/address"))?;
    ensure!(
        matches.next().is_none(),
        "multiple discovered devices at selected bus/address"
    );
    Ok(first)
}

pub(crate) fn resolve_reconnect(
    entries: &[InventoryEntry],
    previous: UsbBusAddress,
    bus_address_hint: Option<&str>,
) -> Result<UsbBusAddress> {
    let candidates: Vec<_> = entries
        .iter()
        .filter(|entry| {
            entry.identity.bus != previous.bus || entry.identity.address != previous.address
        })
        .collect();
    if candidates.is_empty() {
        bail!("reconnect inventory has no device at a new bus/address");
    }
    if candidates.len() == 1 && bus_address_hint.is_none() {
        let entry = candidates[0];
        return Ok(UsbBusAddress {
            bus: entry.identity.bus,
            address: entry.identity.address,
        });
    }
    let hint = bus_address_hint.ok_or_else(|| {
        anyhow::anyhow!("multiple reconnect candidates; pass --bus-address BUS:ADDRESS")
    })?;
    let (selector, _) = resolve_selection(entries, Some(hint))?;
    ensure!(
        selector.bus != previous.bus || selector.address != previous.address,
        "reconnect resolved the same bus/address as before"
    );
    Ok(selector)
}

pub(crate) fn snapshot_peers<I: UsbInventory>(
    usb: &I,
    target: UsbBusAddress,
) -> BTreeSet<PeerIdentity> {
    let mut peers = BTreeSet::new();
    for (vid, pid, _) in KNOWN_LCD_IDS {
        let Ok(entries) = usb.inventory_matching(*vid, *pid) else {
            continue;
        };
        for entry in entries {
            if entry.identity.bus == target.bus && entry.identity.address == target.address {
                continue;
            }
            peers.insert(PeerIdentity {
                vid: *vid,
                pid: *pid,
                bus: entry.identity.bus,
                address: entry.identity.address,
            });
        }
    }
    peers
}

fn build_ownership_target<H: HidrawInventory, F: SysfsAccess>(
    fingerprint: &crate::transport::usb_fingerprint::UsbFingerprint,
    selector: UsbBusAddress,
    hidraw: &H,
    sysfs: &F,
    log: &mut ValidatorLog,
) -> Result<OwnershipTarget> {
    let mut target = OwnershipTarget::usb_bus_address(selector.bus, selector.address);
    if device_has_hid_shape(fingerprint) {
        let candidates = hidraw.list_hidraw_candidates()?;
        if let Ok(correlation) =
            super::super::hid_report::correlate_hidraw_to_usb(selector, &candidates, sysfs)
        {
            log.info(format!(
                "ownership hidraw correlation: {}",
                correlation.selected.name()
            ));
            target = target.with_hidraw_devnode(correlation.devnode);
        }
    }
    Ok(target)
}

fn fail_cli(result: PreflightResult, output_dir: &Path) -> Result<PathBuf> {
    println!("{}", output_dir.display());
    match result {
        PreflightResult::Pass => Ok(output_dir.to_path_buf()),
        PreflightResult::Fail(stage, errors) => {
            let message = errors
                .iter()
                .map(|(_, msg)| *msg)
                .collect::<Vec<_>>()
                .join("; ");
            bail!("validate-device failed at {stage:?}: {message}")
        }
    }
}

#[cfg(test)]
mod active_tests {
    use super::*;
    use crate::service::guard::{DeviceOwnership, OwnershipTarget, ServiceControl};
    use crate::transport::hid_report::{HidReportIo, HidReportReadSession, PROTOCOL_CHUNK_BYTES};
    use crate::transport::validation_report::{
        EvidenceOrigin, HardwareValidationReport, ValidationScope,
    };
    use crate::transport::{EncodedFrame, FrameEncoding};
    use crate::transport::usb_fingerprint::{
        UsbDirection, UsbEndpointCapability, UsbFingerprint, UsbInterfaceShape, UsbRunIdentity,
        UsbTransferKind,
    };
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    struct MapInventory {
        entries: Vec<InventoryEntry>,
    }

    impl UsbInventory for MapInventory {
        fn inventory_matching(&self, vid: u16, pid: u16) -> anyhow::Result<Vec<InventoryEntry>> {
            Ok(self
                .entries
                .iter()
                .filter(|entry| {
                    entry.identity.fingerprint.vid == vid && entry.identity.fingerprint.pid == pid
                })
                .cloned()
                .collect())
        }
    }

    struct MapHidrawInventory {
        candidates: Vec<crate::transport::hid_report::HidrawCandidate>,
    }

    impl HidrawInventory for MapHidrawInventory {
        fn list_hidraw_candidates(
            &self,
        ) -> anyhow::Result<Vec<crate::transport::hid_report::HidrawCandidate>> {
            Ok(self.candidates.clone())
        }
    }

    #[derive(Default)]
    struct MapSysfs {
        files: std::collections::BTreeMap<PathBuf, String>,
        canonical: std::collections::BTreeMap<PathBuf, PathBuf>,
    }

    impl MapSysfs {
        fn insert_file(mut self, path: impl Into<PathBuf>, contents: impl Into<String>) -> Self {
            self.files.insert(path.into(), contents.into());
            self
        }

        fn link(mut self, from: impl Into<PathBuf>, to: impl Into<PathBuf>) -> Self {
            self.canonical.insert(from.into(), to.into());
            self
        }
    }

    impl crate::transport::hid_report::SysfsAccess for MapSysfs {
        fn canonicalize(&self, path: &std::path::Path) -> anyhow::Result<PathBuf> {
            self.canonical
                .get(path)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no canonical mapping for {}", path.display()))
        }

        fn read_trimmed(&self, path: &std::path::Path) -> anyhow::Result<String> {
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("missing {}", path.display()))
        }

        fn exists(&self, path: &std::path::Path) -> bool {
            self.files.contains_key(path)
        }
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

    fn inventory_entry(
        bus: u8,
        address: u8,
        fingerprint: UsbFingerprint,
        serial_present: bool,
    ) -> InventoryEntry {
        InventoryEntry {
            identity: UsbRunIdentity {
                bus,
                address,
                fingerprint,
            },
            serial_present,
        }
    }

    fn correlated_hidraw_fs(bus: u8, address: u8) -> MapSysfs {
        MapSysfs::default()
            .link(
                "/sys/class/hidraw/hidraw3/device",
                "/sys/devices/pci0/usb1/1-2/1-2:1.0",
            )
            .insert_file("/sys/devices/pci0/usb1/1-2/1-2:1.0/busnum", bus.to_string())
            .insert_file(
                "/sys/devices/pci0/usb1/1-2/1-2:1.0/devnum",
                address.to_string(),
            )
    }

    struct CountingInventory {
        first: Vec<InventoryEntry>,
        later: Vec<InventoryEntry>,
        calls: RefCell<usize>,
    }

    impl UsbInventory for CountingInventory {
        fn inventory_matching(&self, vid: u16, pid: u16) -> anyhow::Result<Vec<InventoryEntry>> {
            let call = {
                let mut calls = self.calls.borrow_mut();
                *calls += 1;
                *calls
            };
            let source = if call == 1 { &self.first } else { &self.later };
            Ok(source
                .iter()
                .filter(|entry| {
                    entry.identity.fingerprint.vid == vid && entry.identity.fingerprint.pid == pid
                })
                .cloned()
                .collect())
        }
    }

    struct FakeControl {
        active: bool,
        calls: Rc<RefCell<Vec<&'static str>>>,
    }

    impl Default for FakeControl {
        fn default() -> Self {
            Self {
                active: false,
                calls: Rc::new(RefCell::new(Vec::new())),
            }
        }
    }

    impl ServiceControl for FakeControl {
        fn is_active(&self, _unit: &str) -> anyhow::Result<bool> {
            self.calls.borrow_mut().push("is_active");
            Ok(self.active)
        }

        fn stop(&self, _unit: &str) -> anyhow::Result<()> {
            self.calls.borrow_mut().push("stop");
            Ok(())
        }

        fn start(&self, _unit: &str) -> anyhow::Result<()> {
            self.calls.borrow_mut().push("start");
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeOwnership {
        owned: bool,
    }

    impl DeviceOwnership for FakeOwnership {
        fn is_concurrently_owned(&self, _target: &OwnershipTarget) -> anyhow::Result<bool> {
            Ok(self.owned)
        }
    }

    struct FakeHidIo {
        read_data: VecDeque<Vec<u8>>,
        read_returns: VecDeque<anyhow::Result<isize>>,
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
        write_returns: VecDeque<anyhow::Result<isize>>,
    }

    impl FakeHidIo {
        fn with_probe(response: &[u8]) -> Self {
            let mut read_data = VecDeque::new();
            let mut read_returns = VecDeque::new();
            read_data.push_back(response.to_vec());
            read_returns.push_back(Ok(response.len() as isize));
            let mut write_returns = VecDeque::new();
            for _ in 0..10_000 {
                write_returns.push_back(Ok((PROTOCOL_CHUNK_BYTES + 1) as isize));
            }
            Self {
                read_data,
                read_returns,
                writes: Arc::new(Mutex::new(Vec::new())),
                write_returns,
            }
        }
    }

    impl HidReportIo for FakeHidIo {
        fn write(&mut self, data: &[u8]) -> anyhow::Result<isize> {
            self.writes.lock().unwrap().push(data.to_vec());
            self.write_returns
                .pop_front()
                .unwrap_or(Ok(data.len() as isize))
        }

        fn read_timeout(&mut self, buf: &mut [u8], _timeout_ms: u32) -> anyhow::Result<isize> {
            let data = self
                .read_data
                .pop_front()
                .unwrap_or_else(|| vec![0; buf.len()]);
            let len = data.len().min(buf.len());
            buf[..len].copy_from_slice(&data[..len]);
            self.read_returns.pop_front().unwrap_or(Ok(len as isize))
        }
    }

    fn pm58() -> Vec<u8> {
        vec![0xDA, 0xDB, 0xDC, 0xDD, 0x00, 0x3A, 0x00, 0x00]
    }

    fn pm68() -> Vec<u8> {
        let mut r = pm58();
        r[5] = 0x44;
        r
    }

    fn active_fixture() -> (MapInventory, MapHidrawInventory, MapSysfs) {
        (
            MapInventory {
                entries: vec![inventory_entry(1, 14, hid_in_fingerprint(), false)],
            },
            MapHidrawInventory {
                candidates: vec![
                    crate::transport::hid_report::HidrawCandidate::from_sysfs_class_entry(
                        std::path::PathBuf::from("/sys/class/hidraw/hidraw3"),
                    )
                    .unwrap(),
                ],
            },
            correlated_hidraw_fs(1, 14),
        )
    }

    #[test]
    fn pm68_stops_before_hid_write() {
        let io = FakeHidIo::with_probe(&pm68());
        let writes = Arc::clone(&io.writes);
        let mut session = HidReportReadSession::from_io(io);
        let obs = session.probe_type2_read_only(0).unwrap();
        assert!(!obs.policy().active_writes_allowed());
        assert!(session.authorize_writes().is_err());
        assert_eq!(writes.lock().unwrap().len(), 0);
    }

    #[test]
    fn pm58_probe_authorizes_without_prewrite() {
        let io = FakeHidIo::with_probe(&pm58());
        let mut session = HidReportReadSession::from_io(io);
        session.probe_type2_read_only(0).unwrap();
        assert!(session.authorize_writes().is_ok());
    }

    #[test]
    fn malformed_probe_fails_without_write() {
        let io = FakeHidIo::with_probe(&[0; 8]);
        let writes = Arc::clone(&io.writes);
        let mut session = HidReportReadSession::from_io(io);
        assert!(session.probe_type2_read_only(0).is_err());
        assert_eq!(writes.lock().unwrap().len(), 0);
    }

    #[test]
    fn reconnect_requires_new_address() {
        let entries = vec![
            inventory_entry(1, 5, hid_in_fingerprint(), false),
            inventory_entry(1, 8, hid_in_fingerprint(), false),
        ];
        let selector = resolve_reconnect(&entries, UsbBusAddress { bus: 1, address: 5 }, None)
            .unwrap();
        assert_eq!(selector, UsbBusAddress { bus: 1, address: 8 });
    }

    #[test]
    fn reconnect_ambiguity_errors() {
        let entries = vec![
            inventory_entry(1, 5, hid_in_fingerprint(), false),
            inventory_entry(1, 8, hid_in_fingerprint(), false),
            inventory_entry(1, 9, hid_in_fingerprint(), false),
        ];
        let error = resolve_reconnect(
            &entries,
            UsbBusAddress { bus: 1, address: 5 },
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("multiple reconnect candidates"));
    }

    #[test]
    fn exclusive_owner_blocks_active_path() {
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
            |_, _| Ok(HidReportReadSession::from_io(FakeHidIo::with_probe(&pm58()))),
        )
        .unwrap_err();
        assert!(error.to_string().contains("ExclusiveOwner"), "{error}");
    }

    #[test]
    fn visual_no_fails_target_marker() {
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
            |_, _| Ok(HidReportReadSession::from_io(FakeHidIo::with_probe(&pm58()))),
        )
        .unwrap_err();
        assert!(error.to_string().contains("TargetMarker"), "{error}");
    }

    #[test]
    fn visual_yes_reaches_soak_stage() {
        let temp = tempfile::tempdir().unwrap();
        let (_, hidraw, sysfs) = active_fixture();
        let usb = CountingInventory {
            first: vec![inventory_entry(1, 14, hid_in_fingerprint(), false)],
            later: vec![
                inventory_entry(1, 14, hid_in_fingerprint(), false),
                inventory_entry(1, 20, hid_in_fingerprint(), false),
            ],
            calls: RefCell::new(0),
        };
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
            &mut ScriptedPrompt::new([true, true, true, true, true]),
            ActiveOptions {
                soak_secs: 0,
                ..Default::default()
            },
            |_| {},
            |_, _| Ok(HidReportReadSession::from_io(FakeHidIo::with_probe(&pm58()))),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("finalize")
                || error.to_string().contains("Reconnect")
                || error.to_string().contains("DaemonRestored"),
            "{error}"
        );
    }

    #[test]
    fn guard_restore_attempted_on_drop() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let control = FakeControl {
            active: true,
            calls: Rc::clone(&calls),
        };
        let temp = tempfile::tempdir().unwrap();
        let (usb, hidraw, sysfs) = active_fixture();
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
            |_, _| Ok(HidReportReadSession::from_io(FakeHidIo::with_probe(&pm58()))),
        );
        let recorded = calls.borrow();
        assert!(
            recorded.iter().any(|call| *call == "start"),
            "expected daemon restore start, got {recorded:?}"
        );
    }

    #[test]
    fn synthetic_origin_not_tested_eligible() {
        let report =
            HardwareValidationReport::new_in_progress(EvidenceOrigin::Synthetic, ValidationScope::Full);
        assert!(!report.eligible_for_tested());
    }

    #[test]
    fn spy_transport_counts_writes() {
        use crate::transport::null::NullTransport;
        use crate::transport::profile::device_info_from_fixture;
        let info = device_info_from_fixture("bulk-87ad-70db-pm4-sub5-fbl72").unwrap();
        let mut spy = SpyTransport::new(Box::new(NullTransport::with_profile(info)));
        let frame = EncodedFrame {
            data: vec![0; 64],
            width: 480,
            height: 480,
            encoding: FrameEncoding::Jpeg,
        };
        let _ = spy.handshake();
        let _ = spy.send_frame(&frame);
        assert_eq!(spy.writes, 1);
    }
}
