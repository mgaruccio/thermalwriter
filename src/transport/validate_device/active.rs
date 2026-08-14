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
use crate::transport::discovery::{
    DiscoveredDevice, KNOWN_LCD_IDS, discovered_exact_square_from_fingerprint, open_discovered,
};
use crate::transport::hid_lcd::{HidLcd, Type2PassiveObservation, classify_type2_passive_response};
use crate::transport::hid_report::{
    HidReadObservation, HidReportIo, HidReportReadSession, HidReportWriteSession,
    HidrawCorrelation, LINUX_HIDRAW_BACKEND_CONTRACT, PROTOCOL_CHUNK_BYTES, UsbBusAddress,
};
use crate::transport::policy::ExactDevicePolicy;
use crate::transport::profile::DeviceInfo;
use crate::transport::type2_policy::{
    TYPE2_PROBE_READ_BOUND, Type2PreHandshakePolicy, select_type2_pre_handshake_policy,
};
use crate::transport::validation_report::{
    CheckField, CheckStatus, DescriptorCaptureStatus, EvidenceOrigin, HardwareValidationReport,
    ValidationErrorKind, ValidationScope, ValidationStage,
};
use crate::transport::{EncodedFrame, Transport};

use super::super::hid_report::SysfsAccess;
use super::cards::{
    TestCardBundle, encode_and_save_expected, generate_test_cards, pad_to_hid_chunks,
};
use super::{
    HidrawInventory, InventoryEntry, PassivePreflightContext, PreflightResult, UsbInventory,
    ValidatorLog, correlate_hidraw_if_needed, device_has_hid_shape, resolve_selection,
    run_passive_preflight, write_validation_output,
};

/// Default soak duration for active validation (5 minutes).
pub const DEFAULT_SOAK_SECS: u64 = 300;
const SOAK_PROGRESS_INTERVAL_SECS: u64 = 10;

/// Pause between card refresh sends while waiting for an operator answer.
const CARD_PROMPT_HOLD_MS: u64 = 200;

/// Injectable yes/no prompts (stdin in production, scripted in tests).
pub trait Prompt {
    fn yes_no(&mut self, question: &str) -> bool;

    /// Send frames via `hold` until the operator answers the question.
    fn yes_no_with_hold(
        &mut self,
        question: &str,
        mut hold: impl FnMut() -> Result<(), ValidationStage>,
        _sleep: &mut impl FnMut(Duration),
    ) -> Result<bool, ValidationStage> {
        hold()?;
        Ok(self.yes_no(question))
    }
}

/// One durable stdin line source for the whole validation session.
struct StdinLineSource {
    lines: std::sync::mpsc::Receiver<String>,
    reader_thread: Option<std::thread::JoinHandle<()>>,
}

impl StdinLineSource {
    fn new() -> Self {
        use std::io::{self, BufRead};
        use std::sync::mpsc;
        let (tx, lines) = mpsc::channel();
        let reader_thread = std::thread::spawn(move || {
            let stdin = io::stdin();
            let mut reader = stdin.lock();
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if tx.send(line.clone()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            lines,
            reader_thread: Some(reader_thread),
        }
    }

    fn read_line_blocking(&mut self) -> String {
        self.lines.recv().unwrap_or_default()
    }

    fn try_read_line(&self) -> Option<String> {
        self.lines.try_recv().ok()
    }
}

impl Drop for StdinLineSource {
    fn drop(&mut self) {
        // Detach rather than join: the reader may block on stdin between prompts.
        self.reader_thread.take();
    }
}

/// Production prompt reading lines from one shared stdin reader.
pub struct StdioPrompt {
    stdin: StdinLineSource,
}

impl StdioPrompt {
    pub fn new() -> Self {
        Self {
            stdin: StdinLineSource::new(),
        }
    }
}

impl Default for StdioPrompt {
    fn default() -> Self {
        Self::new()
    }
}

impl Prompt for StdioPrompt {
    fn yes_no(&mut self, question: &str) -> bool {
        use std::io::{self, Write};
        let _ = writeln!(io::stdout(), "{question} [y/N]");
        let _ = io::stdout().flush();
        parse_yes_no_line(&self.stdin.read_line_blocking())
    }

    fn yes_no_with_hold(
        &mut self,
        question: &str,
        mut hold: impl FnMut() -> Result<(), ValidationStage>,
        sleep: &mut impl FnMut(Duration),
    ) -> Result<bool, ValidationStage> {
        use std::io::{self, Write};
        let _ = writeln!(io::stdout(), "{question} [y/N]");
        let _ = io::stdout().flush();

        loop {
            hold()?;
            sleep(Duration::from_millis(CARD_PROMPT_HOLD_MS));
            if let Some(line) = self.stdin.try_read_line() {
                return Ok(parse_yes_no_line(&line));
            }
        }
    }
}

fn parse_yes_no_line(line: &str) -> bool {
    matches!(line.trim().chars().next(), Some('y' | 'Y'))
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

/// Scripted prompt that only answers after a minimum number of hold callbacks.
#[derive(Debug)]
pub struct HoldScriptedPrompt {
    pub answers: Vec<bool>,
    pub index: usize,
    pub asked: Vec<String>,
    pub min_holds: usize,
    holds_this_question: usize,
}

impl HoldScriptedPrompt {
    pub fn new(answers: impl IntoIterator<Item = bool>, min_holds: usize) -> Self {
        Self {
            answers: answers.into_iter().collect(),
            index: 0,
            asked: Vec::new(),
            min_holds,
            holds_this_question: 0,
        }
    }
}

impl Prompt for HoldScriptedPrompt {
    fn yes_no(&mut self, question: &str) -> bool {
        self.asked.push(question.to_string());
        let answer = self.answers.get(self.index).copied().unwrap_or(false);
        self.index += 1;
        answer
    }

    fn yes_no_with_hold(
        &mut self,
        question: &str,
        mut hold: impl FnMut() -> Result<(), ValidationStage>,
        sleep: &mut impl FnMut(Duration),
    ) -> Result<bool, ValidationStage> {
        self.asked.push(question.to_string());
        self.holds_this_question = 0;
        loop {
            hold()?;
            self.holds_this_question += 1;
            sleep(Duration::from_millis(CARD_PROMPT_HOLD_MS));
            if self.holds_this_question >= self.min_holds {
                let answer = self.answers.get(self.index).copied().unwrap_or(false);
                self.index += 1;
                return Ok(answer);
            }
        }
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
            rotation: 180,
        }
    }
}

impl ActiveOptions {
    /// Load rotation and JPEG quality from the user's on-disk config when available.
    pub fn from_user_config() -> Self {
        let mut opts = Self::default();
        if let Ok(config) = crate::config::Config::load(&crate::config::Config::default_path()) {
            opts.rotation = config.display.rotation;
            opts.jpeg_quality = config.display.jpeg_quality;
        }
        opts
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
    let mut prompt = StdioPrompt::new();
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

#[doc(hidden)]
pub fn run_active_validation_with<I, H, F, C, O, P, Io>(
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
    let mut report =
        HardwareValidationReport::new_in_progress(EvidenceOrigin::Physical, ValidationScope::Full);
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

    let ownership_target = build_ownership_target(
        &selected.identity.fingerprint,
        selector,
        hidraw,
        sysfs,
        &mut log,
    )?;

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
            println!("=== Stage: restore daemon ===");
            restore_daemon(&mut guard, &mut report, &mut log);
            println!("=== Stage: write report ===");
            write_validation_output(output_dir, &report, Some(&selected), &mut log)?;
            println!("Done: {}", output_dir.display());
            bail!("validate-device failed at {stage:?}");
        }
    };

    if !negotiation.active_writes_allowed {
        conservative_stop(&mut report);
        println!("=== Stage: restore daemon ===");
        restore_daemon(&mut guard, &mut report, &mut log);
        println!("=== Stage: write report ===");
        write_validation_output(output_dir, &report, Some(&selected), &mut log)?;
        println!("Done: {}", output_dir.display());
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
        println!("=== Stage: restore daemon ===");
        restore_daemon(&mut guard, &mut report, &mut log);
        println!("=== Stage: write report ===");
        write_validation_output(output_dir, &report, Some(&selected), &mut log)?;
        println!("Done: {}", output_dir.display());
        bail!("validate-device failed at {stage:?}");
    }

    output.close();
    println!("=== Stage: restore daemon ===");
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
        println!("=== Stage: write report ===");
        write_validation_output(output_dir, &report, Some(&selected), &mut log)?;
        println!("Done: {}", output_dir.display());
        bail!("validate-device failed to finalize: {error}");
    }

    println!("=== Stage: write report ===");
    write_validation_output(output_dir, &report, Some(&selected), &mut log)?;
    println!("Done: {}", output_dir.display());
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
            if !matches!(
                crate::transport::policy::exact_descriptor_policy(&selected.identity.fingerprint),
                Ok(crate::transport::policy::ExactDescriptorPolicy::Type2)
            ) {
                let _ = report.fail_at(
                    ValidationStage::PassiveAllowlist,
                    &[(
                        ValidationErrorKind::Policy,
                        "descriptor is not the exact Type2 production shape",
                    )],
                );
                return Err(ValidationStage::PassiveAllowlist);
            }
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
    let _ = report.set_hid_descriptor_status(DescriptorCaptureStatus::Captured);

    let (read_observation, response) = match session.read_type2_passive(500) {
        Ok(value) => value,
        Err(read_error) => {
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
                &[(ValidationErrorKind::Transport, "HID read-only probe failed")],
            );
            let _ = report.record_check(CheckField::Handshake, CheckStatus::Fail);
            return Err(ValidationStage::Handshake);
        }
    };
    record_hid_probe_read(report, &read_observation);
    let passive_kind = match classify_type2_passive_response(&response) {
        Ok(kind) => kind,
        Err(error) => {
            log.info(format!("HID passive response rejected: {error:#}"));
            let _ = report.fail_at(
                ValidationStage::Negotiation,
                &[(
                    ValidationErrorKind::Policy,
                    "Type2 passive response was malformed",
                )],
            );
            let _ = report.record_check(CheckField::Handshake, CheckStatus::Fail);
            return Err(ValidationStage::Negotiation);
        }
    };
    let observation = match passive_kind {
        Type2PassiveObservation::Empty => None,
        Type2PassiveObservation::Pm58 => Some(
            crate::transport::type2_policy::negotiate_type2_policy(
                vid,
                pid,
                &response,
                Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
            )
            .map_err(|error| {
                log.info(format!("HID PM58 negotiate error: {error}"));
                ValidationStage::Negotiation
            })?,
        ),
        // A well-formed full observation is only a transition hint. The fresh PM128
        // libusb response remains the sole authorization and may prove an unknown PM/SUB.
        Type2PassiveObservation::Full => crate::transport::type2_policy::negotiate_type2_policy(
            vid,
            pid,
            &response,
            Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
        )
        .ok(),
    };
    if let Some(observation) = &observation {
        if let Err(error) = report.record_negotiated_type2(observation) {
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
    }
    let _ = report.record_check(CheckField::Handshake, CheckStatus::Pass);
    if matches!(
        passive_kind,
        Type2PassiveObservation::Empty | Type2PassiveObservation::Full
    ) {
        drop(session);
        let mut transport =
            HidLcd::open_type2_libusb(selector.bus, selector.address, 0, 0x83, 0x02).map_err(
                |error| {
                    log.info(format!("PM128 libusb open error: {error:#}"));
                    ValidationStage::Negotiation
                },
            )?;
        let device_info = transport.handshake_type2_pm128_session().map_err(|error| {
            log.info(format!("PM128 libusb negotiation error: {error:#}"));
            ValidationStage::Negotiation
        })?;
        if device_info.authorized_policy() != Some(ExactDevicePolicy::Type2Pm128) {
            return Err(ValidationStage::Negotiation);
        }
        let _ = report.record_check(CheckField::ActiveWrite, CheckStatus::Pass);
        return Ok(NegotiationResult {
            device_info,
            active_writes_allowed: true,
            output: ActiveOutput::Bulk(Box::new(transport)),
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
    Ok(NegotiationResult {
        active_writes_allowed: true,
        device_info: ExactDevicePolicy::Type2Pm58.device_info(),
        output: ActiveOutput::Hid(write_session),
    })
}

fn negotiate_bulk<Io: HidReportIo>(
    vid: u16,
    pid: u16,
    selector: UsbBusAddress,
    selected: &InventoryEntry,
    report: &mut HardwareValidationReport,
    log: &mut ValidatorLog,
) -> Result<NegotiationResult<Io>, ValidationStage> {
    let discovered = discovered_for_selector(vid, pid, selector, selected).map_err(|error| {
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

struct VisualCardStep {
    label: &'static str,
    stage: ValidationStage,
    check: CheckField,
    png_name: &'static str,
}

const VISUAL_CARD_STEPS: [VisualCardStep; 3] = [
    VisualCardStep {
        label: "target marker",
        stage: ValidationStage::TargetMarker,
        check: CheckField::TargetMarker,
        png_name: "expected-target-marker.png",
    },
    VisualCardStep {
        label: "orientation",
        stage: ValidationStage::Orientation,
        check: CheckField::Orientation,
        png_name: "expected-orientation.png",
    },
    VisualCardStep {
        label: "colors",
        stage: ValidationStage::Colors,
        check: CheckField::Colors,
        png_name: "expected-colors.png",
    },
];

fn send_card_frame<Io: HidReportIo>(
    output: &mut ActiveOutput<Io>,
    frame: &EncodedFrame,
    report: &mut HardwareValidationReport,
    log: &mut ValidatorLog,
) -> Result<(), ValidationStage> {
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
    })
}

fn build_card_prompt(
    label: &str,
    vid: u16,
    pid: u16,
    bundle: &TestCardBundle,
    expected_png: &Path,
) -> String {
    let vid_pid = format!("{vid:04X}:{pid:04X}");
    let abs_png = expected_png
        .canonicalize()
        .unwrap_or_else(|_| expected_png.to_path_buf());
    let expected_desc = match label {
        "target marker" => format!(
            "dark background, white top bar, text like \"{} {}\", magenta rectangle in the middle",
            bundle.run_id, bundle.vid_pid_label
        ),
        "orientation" => "dark gray background, white TOP bar at the top, colored corner markers (top-left red, top-right green, bottom-left blue, bottom-right yellow)".to_string(),
        "colors" => "six color blocks — top row red, green, blue; bottom row white, black, mid-gray".to_string(),
        _ => "validation test pattern".to_string(),
    };
    format!(
        "CARD: {label}\n\
        Look at the SELECTED cooler LCD only (VID:PID {vid_pid}).\n\
        Expected: {expected_desc}.\n\
        Reference image: {}\n\
        Open on desktop: xdg-open \"{}\"\n\
        Does the selected display match this card?",
        abs_png.display(),
        abs_png.display(),
    )
}

fn second_display_prompt() -> String {
    "If you have another supported cooler LCD attached, check it now.\n\
    Is the OTHER display unchanged (still showing its normal content, not these test cards)?"
        .to_string()
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

    println!("=== Stage: visual cards ===");

    let mut active_write_recorded = false;
    for (step, frame) in VISUAL_CARD_STEPS.iter().zip(encoded.iter()) {
        if !active_write_recorded {
            let _ = report.record_check(CheckField::ActiveWrite, CheckStatus::Pass);
            active_write_recorded = true;
        }

        let expected_png = output_dir.join(step.png_name);
        let question = build_card_prompt(step.label, vid, pid, &bundle, &expected_png);
        if !prompt.yes_no_with_hold(
            &question,
            || send_card_frame(output, frame, report, log),
            sleep,
        )? {
            fail_visual(report, log, step.stage);
            return Err(step.stage);
        }
        let _ = report.record_check(step.check, CheckStatus::Pass);
    }

    let colors_frame = encoded.last().ok_or_else(|| {
        log.info("second-display prompt missing colors frame".to_string());
        ValidationStage::SecondDisplayUnchanged
    })?;

    if peers_before.is_empty() {
        let _ = report.record_check(
            CheckField::SecondDisplayUnchanged,
            CheckStatus::NotApplicable,
        );
    } else {
        println!("Second display check — verify the other cooler LCD is unchanged.");
        if !prompt.yes_no_with_hold(
            &second_display_prompt(),
            || send_card_frame(output, colors_frame, report, log),
            sleep,
        )? {
            fail_visual(report, log, ValidationStage::SecondDisplayUnchanged);
            return Err(ValidationStage::SecondDisplayUnchanged);
        }
        let _ = report.record_check(CheckField::SecondDisplayUnchanged, CheckStatus::Pass);
    }

    let colors_frame = encoded.last().ok_or_else(|| {
        log.info("soak missing colors frame".to_string());
        ValidationStage::Soak
    })?;
    run_soak(
        output,
        std::slice::from_ref(colors_frame),
        options.soak_secs,
        sleep,
        report,
        log,
    )?;

    println!();
    println!("=== Stage: reconnect ===");
    println!("Reconnect test — unplug and replug the USB cable when ready.");
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

    println!("Re-scanning USB for {vid:04X}:{pid:04X}...");
    let entries = match usb.inventory_matching(vid, pid) {
        Ok(entries) => entries,
        Err(error) => {
            println!("USB re-scan failed: {error:#}");
            log.info(format!("reconnect inventory error: {error:#}"));
            let _ = report.fail_at(
                ValidationStage::Reconnect,
                &[(
                    ValidationErrorKind::Device,
                    "USB re-scan failed after reconnect",
                )],
            );
            return Err(ValidationStage::Reconnect);
        }
    };
    for entry in &entries {
        println!(
            "  found bus {} address {}",
            entry.identity.bus, entry.identity.address
        );
    }
    if entries.is_empty() {
        println!("No matching devices found after reconnect.");
    }

    let new_selector = match resolve_reconnect(&entries, previous_selector, bus_address_hint) {
        Ok(selector) => selector,
        Err(error) => {
            println!("Reconnect failed: {error:#}");
            log.info(format!("reconnect selection error: {error:#}"));
            let _ = report.abort_at(
                ValidationStage::Reconnect,
                &[(
                    ValidationErrorKind::Device,
                    "reconnect could not resolve target bus/address",
                )],
            );
            return Err(ValidationStage::Reconnect);
        }
    };
    println!(
        "Reconnect OK: bus {} address {}",
        new_selector.bus, new_selector.address
    );

    // Exclude the reconnected target identity, not the pre-unplug address.
    let peers_after = snapshot_peers(usb, new_selector);
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
    let frame = encoded.first().ok_or_else(|| {
        log.info("soak missing encoded frame".to_string());
        ValidationStage::Soak
    })?;
    println!("=== Stage: soak ({soak_secs} seconds, keeps last card on screen) ===");
    let progress_every = SOAK_PROGRESS_INTERVAL_SECS.saturating_mul(5);
    let iterations = soak_secs.saturating_mul(5);
    for i in 0..iterations {
        output.send_encoded(frame).map_err(|error| {
            log.info(format!("soak send error: {error:#}"));
            let _ = report.fail_at(
                ValidationStage::Soak,
                &[(ValidationErrorKind::Transport, "soak stream frame failed")],
            );
            ValidationStage::Soak
        })?;
        sleep(Duration::from_millis(200));
        let elapsed_secs = (i + 1) / 5;
        if progress_every > 0 && elapsed_secs % SOAK_PROGRESS_INTERVAL_SECS == 0 {
            println!("Soak progress: {elapsed_secs}/{soak_secs}s");
        }
    }
    println!("Soak complete.");
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
        Ok(()) => {
            println!("Daemon restored.");
            CheckStatus::Pass
        }
        Err(error) => {
            println!("Daemon restore failed: {error:#}");
            log.info(format!("daemon restore error: {error:#}"));
            CheckStatus::Fail
        }
    };
    let _ = report.record_check(CheckField::DaemonRestored, restored);
}

fn record_hid_probe_read(report: &mut HardwareValidationReport, obs: &HidReadObservation) {
    let _ = report.record_hid_read(obs);
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

fn discovered_for_selector(
    vid: u16,
    pid: u16,
    selector: UsbBusAddress,
    selected: &InventoryEntry,
) -> Result<DiscoveredDevice> {
    ensure!(
        selected.identity.bus == selector.bus && selected.identity.address == selector.address,
        "selected inventory entry does not match bus/address selector"
    );
    ensure!(
        selected.identity.fingerprint.vid == vid && selected.identity.fingerprint.pid == pid,
        "selected inventory VID:PID does not match validate target"
    );
    match discovered_exact_square_from_fingerprint(
        selector.bus,
        selector.address,
        &selected.identity.fingerprint,
    ) {
        Ok(device) => Ok(device),
        Err(error) => {
            #[cfg(test)]
            {
                return crate::transport::discovery::discovered_bulk_from_fingerprint(
                    vid,
                    pid,
                    selector.bus,
                    selector.address,
                    &selected.identity.fingerprint,
                );
            }
            #[cfg(not(test))]
            {
                Err(error)
            }
        }
    }
}

#[doc(hidden)]
pub fn resolve_reconnect(
    entries: &[InventoryEntry],
    previous: UsbBusAddress,
    bus_address_hint: Option<&str>,
) -> Result<UsbBusAddress> {
    if entries.is_empty() {
        bail!("reconnect inventory found no matching devices; is the cooler plugged in?");
    }

    if let Some(hint) = bus_address_hint {
        let (selector, _) = resolve_selection(entries, Some(hint))?;
        return Ok(selector);
    }

    let new_address_candidates: Vec<_> = entries
        .iter()
        .filter(|entry| {
            entry.identity.bus != previous.bus || entry.identity.address != previous.address
        })
        .collect();

    if new_address_candidates.len() == 1 {
        let entry = new_address_candidates[0];
        return Ok(UsbBusAddress {
            bus: entry.identity.bus,
            address: entry.identity.address,
        });
    }

    if new_address_candidates.len() > 1 {
        bail!("multiple reconnect candidates at new bus/addresses; pass --bus-address BUS:ADDRESS");
    }

    // Device renumerated to the same bus/address (common on replug).
    if entries.len() == 1 {
        let entry = &entries[0];
        return Ok(UsbBusAddress {
            bus: entry.identity.bus,
            address: entry.identity.address,
        });
    }

    bail!(
        "reconnect inventory has {} devices but none at a new bus/address; pass --bus-address BUS:ADDRESS",
        entries.len()
    );
}

#[doc(hidden)]
pub fn snapshot_peers<I: UsbInventory>(usb: &I, target: UsbBusAddress) -> BTreeSet<PeerIdentity> {
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
    use crate::transport::profile::WireProtocol;
    use crate::transport::usb_fingerprint::{
        UsbDirection, UsbEndpointCapability, UsbFingerprint, UsbInterfaceShape, UsbRunIdentity,
        UsbTransferKind,
    };
    use crate::transport::validation_report::{
        EvidenceOrigin, HardwareValidationReport, ValidationScope,
    };
    use crate::transport::{EncodedFrame, FrameEncoding};
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
    fn reconnect_prefers_new_address_when_available() {
        let entries = vec![
            inventory_entry(1, 5, hid_in_fingerprint(), false),
            inventory_entry(1, 8, hid_in_fingerprint(), false),
        ];
        let selector =
            resolve_reconnect(&entries, UsbBusAddress { bus: 1, address: 5 }, None).unwrap();
        assert_eq!(selector, UsbBusAddress { bus: 1, address: 8 });
    }

    #[test]
    fn reconnect_allows_same_address_when_single_device() {
        let entries = vec![inventory_entry(1, 5, hid_in_fingerprint(), false)];
        let selector =
            resolve_reconnect(&entries, UsbBusAddress { bus: 1, address: 5 }, None).unwrap();
        assert_eq!(selector, UsbBusAddress { bus: 1, address: 5 });
    }

    #[test]
    fn reconnect_empty_inventory_fails_fast() {
        let error = resolve_reconnect(&[], UsbBusAddress { bus: 1, address: 5 }, None).unwrap_err();
        assert!(error.to_string().contains("no matching devices"));
    }

    #[test]
    fn reconnect_ambiguity_errors() {
        let entries = vec![
            inventory_entry(1, 5, hid_in_fingerprint(), false),
            inventory_entry(1, 8, hid_in_fingerprint(), false),
            inventory_entry(1, 9, hid_in_fingerprint(), false),
        ];
        let error =
            resolve_reconnect(&entries, UsbBusAddress { bus: 1, address: 5 }, None).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("multiple reconnect candidates at new bus/addresses")
        );
    }

    #[test]
    fn parse_yes_no_line_accepts_y_prefix() {
        assert!(parse_yes_no_line("y"));
        assert!(parse_yes_no_line("Y\n"));
        assert!(parse_yes_no_line("  yes please"));
        assert!(!parse_yes_no_line("n"));
        assert!(!parse_yes_no_line(""));
    }

    #[test]
    fn snapshot_peers_excludes_reconnected_target_not_stale_address() {
        let mut peer_fp = hid_in_fingerprint();
        peer_fp.vid = 0x87ad;
        peer_fp.pid = 0x70db;
        let usb = MapInventory {
            entries: vec![
                inventory_entry(1, 10, peer_fp, false),
                // stale pre-unplug address still present in some inventories is treated as peer
                inventory_entry(3, 20, hid_in_fingerprint(), false),
                inventory_entry(3, 21, hid_in_fingerprint(), false),
            ],
        };
        let peers = snapshot_peers(
            &usb,
            UsbBusAddress {
                bus: 3,
                address: 21,
            },
        );
        assert!(peers.contains(&PeerIdentity {
            vid: 0x87ad,
            pid: 0x70db,
            bus: 1,
            address: 10,
        }));
        assert!(peers.contains(&PeerIdentity {
            vid: 0x0416,
            pid: 0x5302,
            bus: 3,
            address: 20,
        }));
        assert!(!peers.iter().any(|p| p.bus == 3 && p.address == 21));
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
            |_, _| {
                Ok(HidReportReadSession::from_io(
                    FakeHidIo::with_probe(&pm58()),
                ))
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("ExclusiveOwner"), "{error}");
    }

    #[test]
    fn visual_prompt_holds_card_until_answer() {
        let temp = tempfile::tempdir().unwrap();
        let (usb, hidraw, sysfs) = active_fixture();
        let mut io = Some(FakeHidIo::with_probe(&pm58()));
        let writes = Arc::clone(&io.as_ref().unwrap().writes);
        let writes_before = writes.lock().unwrap().len();
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
            &mut HoldScriptedPrompt::new([false], 5),
            ActiveOptions {
                soak_secs: 0,
                ..Default::default()
            },
            |_| {},
            |_, _| Ok(HidReportReadSession::from_io(io.take().unwrap())),
        )
        .unwrap_err();
        assert!(error.to_string().contains("TargetMarker"), "{error}");
        let writes_after = writes.lock().unwrap().len();
        assert!(
            writes_after >= writes_before + 5,
            "expected at least 5 hold sends before answer, got {} -> {}",
            writes_before,
            writes_after
        );
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
            |_, _| {
                Ok(HidReportReadSession::from_io(
                    FakeHidIo::with_probe(&pm58()),
                ))
            },
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
            |_, _| {
                Ok(HidReportReadSession::from_io(
                    FakeHidIo::with_probe(&pm58()),
                ))
            },
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
            |_, _| {
                Ok(HidReportReadSession::from_io(
                    FakeHidIo::with_probe(&pm58()),
                ))
            },
        );
        let recorded = calls.borrow();
        assert!(
            recorded.contains(&"start"),
            "expected daemon restore start, got {recorded:?}"
        );
    }

    #[test]
    fn synthetic_origin_not_tested_eligible() {
        let report = HardwareValidationReport::new_in_progress(
            EvidenceOrigin::Synthetic,
            ValidationScope::Full,
        );
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

    #[test]
    fn bulk_discovered_from_inventory_ignores_peer_hid_presence() {
        let bulk_fp = UsbFingerprint {
            vid: 0x87ad,
            pid: 0x70db,
            bcd_device: "1.00".to_string(),
            interfaces: vec![UsbInterfaceShape {
                number: 1,
                alternate_setting: 0,
                class: 0xff,
                subclass: 0,
                protocol: 0,
                endpoints: vec![
                    UsbEndpointCapability {
                        address: 0x81,
                        direction: UsbDirection::In,
                        transfer: UsbTransferKind::Bulk,
                        max_packet_size: 512,
                        interval: 0,
                    },
                    UsbEndpointCapability {
                        address: 0x02,
                        direction: UsbDirection::Out,
                        transfer: UsbTransferKind::Bulk,
                        max_packet_size: 512,
                        interval: 0,
                    },
                ],
            }],
        };
        let bulk_entry = inventory_entry(1, 10, bulk_fp, false);
        let selector = UsbBusAddress {
            bus: 1,
            address: 10,
        };
        let discovered =
            discovered_for_selector(0x87ad, 0x70db, selector, &bulk_entry).expect("bulk path");
        assert_eq!(discovered.protocol, WireProtocol::Bulk);
        assert_eq!(
            discovered.path,
            crate::transport::discovery::DevicePath::Usb {
                bus: 1,
                address: 10,
                interface: 1,
                ep_in: 0x81,
                ep_out: 0x02,
            }
        );
    }
}
