// SPDX-License-Identifier: GPL-3.0-or-later
//
// `thermalwriter validate-device`: active guided validation by default; `--passive` for descriptor-only preflight.

mod active;
mod cards;

#[doc(hidden)]
pub mod test_support;

pub use active::{
    ActiveOptions, ActiveOutput, DEFAULT_SOAK_SECS, PeerIdentity, Prompt, ScriptedPrompt,
    SpyTransport, StdioPrompt,
};
pub use cards::{TestCardBundle, encode_and_save_expected, generate_test_cards, pad_to_hid_chunks};

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};

use super::discovery::DeviceSelector;
use super::hid_report::{
    HidrawCandidate, RealSysfs, SysfsAccess, UsbBusAddress, correlate_hidraw_to_usb,
};
use super::type2_policy::{Type2PreHandshakePolicy, select_type2_pre_handshake_policy};
use super::usb_fingerprint::{
    UsbFingerprint, UsbRunIdentity, fingerprint_from_device, hid_interrupt_in_endpoints,
};
use super::validation_report::{
    CheckField, CheckStatus, EvidenceOrigin, HardwareValidationReport, ValidationErrorKind,
    ValidationScope, ValidationStage,
};

/// CLI arguments for `thermalwriter validate-device`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidateDeviceArgs {
    pub device: String,
    pub bus_address: Option<String>,
    pub passive: bool,
    pub output: PathBuf,
}

/// One inventoried USB device with shareable fingerprint and serial presence flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryEntry {
    pub identity: UsbRunIdentity,
    pub serial_present: bool,
}

/// Injectable USB inventory (production uses libusb enumeration).
pub trait UsbInventory {
    fn inventory_matching(&self, vid: u16, pid: u16) -> Result<Vec<InventoryEntry>>;
}

/// Injectable hidraw class-dir scan.
pub trait HidrawInventory {
    fn list_hidraw_candidates(&self) -> Result<Vec<HidrawCandidate>>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RusbInventory;

impl UsbInventory for RusbInventory {
    fn inventory_matching(&self, vid: u16, pid: u16) -> Result<Vec<InventoryEntry>> {
        let devices = rusb::devices().context("libusb device list failed")?;
        let mut entries = Vec::new();
        for device in devices.iter() {
            let desc = match device.device_descriptor() {
                Ok(desc) => desc,
                Err(_) => continue,
            };
            if desc.vendor_id() != vid || desc.product_id() != pid {
                continue;
            }
            let fingerprint = fingerprint_from_device(&device)?;
            entries.push(InventoryEntry {
                identity: UsbRunIdentity {
                    bus: device.bus_number(),
                    address: device.address(),
                    fingerprint,
                },
                serial_present: desc
                    .serial_number_string_index()
                    .is_some_and(|index| index != 0),
            });
        }
        Ok(entries)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SysfsHidrawInventory;

impl HidrawInventory for SysfsHidrawInventory {
    fn list_hidraw_candidates(&self) -> Result<Vec<HidrawCandidate>> {
        scan_hidraw_class_entries(Path::new("/sys/class/hidraw"))
    }
}

/// Run validate-device using production inventory backends (active by default).
pub fn run_validate_device(args: ValidateDeviceArgs) -> Result<PathBuf> {
    run_validate_device_with(
        args,
        &RusbInventory,
        &SysfsHidrawInventory,
        &RealSysfs,
        validation_timestamp,
    )
}

/// Run validate-device with injectable inventory backends (test hook).
#[doc(hidden)]
pub fn run_validate_device_with<I, H, F>(
    args: ValidateDeviceArgs,
    usb: &I,
    hidraw: &H,
    sysfs: &F,
    timestamp: impl FnOnce() -> String,
) -> Result<PathBuf>
where
    I: UsbInventory,
    H: HidrawInventory,
    F: SysfsAccess,
{
    let (vid, pid) = parse_device_vid_pid(&args.device)?;

    if args.passive {
        return run_passive_validate_device(args, usb, hidraw, sysfs, timestamp, vid, pid);
    }

    #[cfg(feature = "daemon")]
    {
        let selected_bcd = usb
            .inventory_matching(vid, pid)
            .ok()
            .and_then(|entries| {
                resolve_selection(&entries, args.bus_address.as_deref())
                    .ok()
                    .map(|(_, entry)| entry.identity.fingerprint.bcd_device.clone())
            });
        let output_dir = build_output_dir(
            &args.output,
            vid,
            pid,
            selected_bcd.as_deref(),
            timestamp(),
        )?;
        active::run_active_validation(
            vid,
            pid,
            args.bus_address.as_deref(),
            &output_dir,
            usb,
            hidraw,
            sysfs,
            ActiveOptions::from_user_config(),
        )
    }

    #[cfg(not(feature = "daemon"))]
    {
        let _ = (args, usb, hidraw, sysfs, timestamp, vid, pid);
        bail!("active validate-device requires the daemon feature (hardware I/O)");
    }
}

fn run_passive_validate_device<I, H, F>(
    args: ValidateDeviceArgs,
    usb: &I,
    hidraw: &H,
    sysfs: &F,
    timestamp: impl FnOnce() -> String,
    vid: u16,
    pid: u16,
) -> Result<PathBuf>
where
    I: UsbInventory,
    H: HidrawInventory,
    F: SysfsAccess,
{
    let mut report = HardwareValidationReport::new_in_progress(
        EvidenceOrigin::Physical,
        ValidationScope::Passive,
    );
    let mut log = ValidatorLog::new();

    let outcome = run_passive_preflight(PassivePreflightContext {
        vid,
        pid,
        bus_address: args.bus_address.as_deref(),
        usb,
        hidraw,
        sysfs,
        report: &mut report,
        log: &mut log,
    });

    let selected = outcome.selected.as_ref();
    let output_dir = build_output_dir(
        &args.output,
        vid,
        pid,
        selected.map(|entry| entry.identity.fingerprint.bcd_device.as_str()),
        timestamp(),
    )?;
    write_validation_output(&output_dir, &report, selected, &mut log)?;

    match outcome.result {
        PreflightResult::Pass => {
            println!("{}", output_dir.display());
            Ok(output_dir)
        }
        PreflightResult::Fail(stage, errors) => {
            println!("{}", output_dir.display());
            let message = errors
                .iter()
                .map(|(_, msg)| *msg)
                .collect::<Vec<_>>()
                .join("; ");
            bail!("validate-device failed at {stage:?}: {message}")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Passive preflight outcome for integration tests.
#[doc(hidden)]
pub struct PassiveOutcome {
    pub result: PreflightResult,
    pub selected: Option<InventoryEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub enum PreflightResult {
    Pass,
    Fail(ValidationStage, Vec<(ValidationErrorKind, &'static str)>),
}

#[derive(Debug, Default)]
#[doc(hidden)]
pub struct ValidatorLog {
    lines: Vec<String>,
}

impl ValidatorLog {
    pub fn new() -> Self {
        Self { lines: Vec::new() }
    }

    pub(crate) fn info(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }
}

#[doc(hidden)]
pub struct PassivePreflightContext<'a, I, H, F> {
    pub vid: u16,
    pub pid: u16,
    pub bus_address: Option<&'a str>,
    pub usb: &'a I,
    pub hidraw: &'a H,
    pub sysfs: &'a F,
    pub report: &'a mut HardwareValidationReport,
    pub log: &'a mut ValidatorLog,
}

#[doc(hidden)]
pub fn run_passive_preflight<I, H, F>(ctx: PassivePreflightContext<'_, I, H, F>) -> PassiveOutcome
where
    I: UsbInventory,
    H: HidrawInventory,
    F: SysfsAccess,
{
    let PassivePreflightContext {
        vid,
        pid,
        bus_address,
        usb,
        hidraw,
        sysfs,
        report,
        log,
    } = ctx;
    let entries = match usb.inventory_matching(vid, pid) {
        Ok(entries) => entries,
        Err(error) => {
            let _ = report.fail_at(
                ValidationStage::Inventory,
                &[(
                    ValidationErrorKind::Transport,
                    "failed to inventory USB devices",
                )],
            );
            log.info(format!("inventory error: {error:#}"));
            return PassiveOutcome {
                result: PreflightResult::Fail(
                    ValidationStage::Inventory,
                    vec![(
                        ValidationErrorKind::Transport,
                        "failed to inventory USB devices",
                    )],
                ),
                selected: None,
            };
        }
    };

    log.info(format!(
        "inventory: found {} candidate(s) for {vid:04x}:{pid:04x}",
        entries.len()
    ));
    for entry in &entries {
        log.info(format!(
            "  bus={} address={} bcd_device={}",
            entry.identity.bus, entry.identity.address, entry.identity.fingerprint.bcd_device
        ));
    }

    let selected = match resolve_selection(&entries, bus_address) {
        Ok((selector, entry)) => {
            log.info(format!(
                "selected explicit bus={} address={}",
                selector.bus, selector.address
            ));
            entry.clone()
        }
        Err(error) => {
            let stage = if entries.is_empty() {
                ValidationStage::Inventory
            } else {
                ValidationStage::Selection
            };
            let kind = ValidationErrorKind::Device;
            let message = selection_error_message(&error);
            let _ = report.fail_at(stage, &[(kind, message)]);
            log.info(format!("selection error: {error:#}"));
            return PassiveOutcome {
                result: PreflightResult::Fail(stage, vec![(kind, message)]),
                selected: None,
            };
        }
    };

    let selector = UsbBusAddress {
        bus: selected.identity.bus,
        address: selected.identity.address,
    };

    let hidraw_correlated = match correlate_hidraw_if_needed(
        &selected.identity.fingerprint,
        selector,
        hidraw,
        sysfs,
        log,
    ) {
        Ok(value) => value,
        Err(error) => {
            let _ = report.fail_at(
                ValidationStage::HidrawCorrelation,
                &[(
                    ValidationErrorKind::Device,
                    "hidraw correlation failed for selected USB device",
                )],
            );
            log.info(format!("hidraw correlation error: {error:#}"));
            return PassiveOutcome {
                result: PreflightResult::Fail(
                    ValidationStage::HidrawCorrelation,
                    vec![(
                        ValidationErrorKind::Device,
                        "hidraw correlation failed for selected USB device",
                    )],
                ),
                selected: Some(selected),
            };
        }
    };

    if let Err(error) = report.set_fingerprint(
        &selected.identity.fingerprint,
        selected.serial_present,
        hidraw_correlated,
    ) {
        log.info(format!("report fingerprint error: {error}"));
        return PassiveOutcome {
            result: PreflightResult::Fail(
                ValidationStage::Inventory,
                vec![(ValidationErrorKind::Error, "failed to record fingerprint")],
            ),
            selected: Some(selected),
        };
    }

    let policy = select_type2_pre_handshake_policy(
        &selected.identity.fingerprint,
        hidraw_correlated.unwrap_or(false),
    );
    if let Err(error) = report.set_pre_handshake_policy(policy) {
        log.info(format!("report policy error: {error}"));
        return PassiveOutcome {
            result: PreflightResult::Fail(
                ValidationStage::Inventory,
                vec![(ValidationErrorKind::Error, "failed to record policy")],
            ),
            selected: Some(selected),
        };
    }

    let _ = report.record_check(CheckField::Enumerated, CheckStatus::Pass);
    let allowlist = passive_allowlist_status(policy);
    let _ = report.record_check(CheckField::PassiveAllowlist, allowlist);
    if report.scope() == ValidationScope::Passive {
        record_passive_not_applicable_checks(report);
    }

    if allowlist == CheckStatus::Fail {
        let _ = report.fail_at(
            ValidationStage::PassiveAllowlist,
            &[(
                ValidationErrorKind::Policy,
                "descriptor shape is not on the passive allowlist",
            )],
        );
        return PassiveOutcome {
            result: PreflightResult::Fail(
                ValidationStage::PassiveAllowlist,
                vec![(
                    ValidationErrorKind::Policy,
                    "descriptor shape is not on the passive allowlist",
                )],
            ),
            selected: Some(selected),
        };
    }

    if report.scope() == ValidationScope::Passive {
        if let Err(error) = report.finalize_passive_pass() {
            log.info(format!("finalize error: {error}"));
            return PassiveOutcome {
                result: PreflightResult::Fail(
                    ValidationStage::PassiveAllowlist,
                    vec![(ValidationErrorKind::Error, "passive finalization failed")],
                ),
                selected: Some(selected),
            };
        }
    }

    PassiveOutcome {
        result: PreflightResult::Pass,
        selected: Some(selected),
    }
}

pub(crate) fn parse_device_vid_pid(device: &str) -> Result<(u16, u16)> {
    match DeviceSelector::parse(device)? {
        DeviceSelector::UsbId { vid, pid } => Ok((vid, pid)),
        other => bail!("device must be VID:PID (for example 0416:5302), got {other}"),
    }
}

pub(crate) fn parse_bus_address(input: &str) -> Result<UsbBusAddress> {
    let input = input.trim();
    let (bus_s, address_s) = input.split_once(':').ok_or_else(|| {
        anyhow::anyhow!("bus-address must be BUS:ADDRESS (for example 1:14), got {input:?}")
    })?;
    let bus = bus_s
        .trim()
        .parse::<u8>()
        .with_context(|| format!("invalid bus in bus-address {input:?}"))?;
    let address = address_s
        .trim()
        .parse::<u8>()
        .with_context(|| format!("invalid address in bus-address {input:?}"))?;
    Ok(UsbBusAddress { bus, address })
}

pub(crate) fn resolve_selection<'a>(
    entries: &'a [InventoryEntry],
    bus_address: Option<&str>,
) -> Result<(UsbBusAddress, &'a InventoryEntry)> {
    match entries.len() {
        0 => bail!("no USB device matching VID:PID"),
        1 => {
            let entry = &entries[0];
            if let Some(bus_address) = bus_address {
                let selector = parse_bus_address(bus_address)?;
                ensure!(
                    entry.identity.bus == selector.bus
                        && entry.identity.address == selector.address,
                    "no USB device at bus={} address={}",
                    selector.bus,
                    selector.address
                );
                Ok((selector, entry))
            } else {
                Ok((
                    UsbBusAddress {
                        bus: entry.identity.bus,
                        address: entry.identity.address,
                    },
                    entry,
                ))
            }
        }
        _ => {
            let bus_address = bus_address.ok_or_else(|| {
                anyhow::anyhow!(
                    "multiple USB devices match VID:PID; pass --bus-address BUS:ADDRESS"
                )
            })?;
            let selector = parse_bus_address(bus_address)?;
            let mut matches = entries.iter().filter(|entry| {
                entry.identity.bus == selector.bus && entry.identity.address == selector.address
            });
            let first = matches.next().ok_or_else(|| {
                anyhow::anyhow!(
                    "no USB device at bus={} address={}",
                    selector.bus,
                    selector.address
                )
            })?;
            ensure!(
                matches.next().is_none(),
                "internal duplicate bus/address inventory entries"
            );
            Ok((selector, first))
        }
    }
}

fn selection_error_message(error: &anyhow::Error) -> &'static str {
    let message = error.to_string();
    if message.contains("multiple USB devices match") {
        "ambiguous duplicate VID:PID"
    } else if message.contains("no USB device matching") {
        "no USB device matching VID:PID"
    } else if message.contains("no USB device at bus=") {
        "selected bus/address not present"
    } else if message.contains("bus-address must be") || message.contains("invalid bus") {
        "malformed bus-address"
    } else {
        "device selection failed"
    }
}

pub(crate) fn device_has_hid_shape(fingerprint: &UsbFingerprint) -> bool {
    fingerprint.interfaces.iter().any(|iface| iface.class == 3)
        || !hid_interrupt_in_endpoints(fingerprint).is_empty()
}

fn correlate_hidraw_if_needed<H, F>(
    fingerprint: &UsbFingerprint,
    selector: UsbBusAddress,
    hidraw: &H,
    sysfs: &F,
    log: &mut ValidatorLog,
) -> Result<Option<bool>>
where
    H: HidrawInventory,
    F: SysfsAccess,
{
    if !device_has_hid_shape(fingerprint) {
        log.info("hidraw correlation skipped: bulk/non-HID descriptor shape");
        return Ok(None);
    }

    let candidates = hidraw.list_hidraw_candidates()?;
    log.info(format!(
        "hidraw scan: {} candidate node(s)",
        candidates.len()
    ));
    correlate_hidraw_to_usb(selector, &candidates, sysfs)?;
    Ok(Some(true))
}

pub(crate) fn passive_allowlist_status(policy: Type2PreHandshakePolicy) -> CheckStatus {
    if policy == Type2PreHandshakePolicy::StopUnsupportedShape {
        CheckStatus::Fail
    } else {
        CheckStatus::Pass
    }
}

pub(crate) fn record_passive_not_applicable_checks(report: &mut HardwareValidationReport) {
    for field in [
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
        let _ = report.record_check(field, CheckStatus::NotApplicable);
    }
}

pub(crate) fn scan_hidraw_class_entries(root: &Path) -> Result<Vec<HidrawCandidate>> {
    let mut candidates = Vec::new();
    let entries = std::fs::read_dir(root)
        .with_context(|| format!("read hidraw class dir {}", root.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if let Ok(candidate) = HidrawCandidate::from_sysfs_class_entry(path) {
            candidates.push(candidate);
        }
    }
    candidates.sort_by(|left, right| left.name().cmp(right.name()));
    Ok(candidates)
}

pub(crate) fn format_descriptor_text(fingerprint: &UsbFingerprint) -> String {
    let mut text = String::new();
    let _ = writeln!(
        text,
        "vid={:04x} pid={:04x} bcd_device={}",
        fingerprint.vid, fingerprint.pid, fingerprint.bcd_device
    );
    for iface in &fingerprint.interfaces {
        let endpoint_summary: Vec<String> = iface
            .endpoints
            .iter()
            .map(|ep| {
                format!(
                    "0x{:02x} {:?}/{:?} mps={} interval={}",
                    ep.address, ep.transfer, ep.direction, ep.max_packet_size, ep.interval
                )
            })
            .collect();
        let _ = writeln!(
            text,
            "interface {} alt={} class={} subclass={} protocol={} endpoints=[{}]",
            iface.number,
            iface.alternate_setting,
            iface.class,
            iface.subclass,
            iface.protocol,
            endpoint_summary.join(", ")
        );
    }
    text
}

fn build_output_dir(
    output_root: &Path,
    vid: u16,
    pid: u16,
    bcd_device: Option<&str>,
    timestamp: String,
) -> Result<PathBuf> {
    let bcd = bcd_device.unwrap_or("unknown").replace('.', "-");
    let dir = output_root.join(format!("{vid:04x}-{pid:04x}-{bcd}-{timestamp}"));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create validation output dir {}", dir.display()))?;
    restrict_dir_permissions(&dir)?;
    Ok(dir)
}

pub(crate) fn write_validation_output(
    output_dir: &Path,
    report: &HardwareValidationReport,
    selected: Option<&InventoryEntry>,
    log: &mut ValidatorLog,
) -> Result<()> {
    if let Some(entry) = selected {
        log.info(format!(
            "fingerprint bcd_device={} serial_present={}",
            entry.identity.fingerprint.bcd_device, entry.serial_present
        ));
    }

    let report_path = output_dir.join("report.toml");
    let (report_body, shareable) = match report.to_shareable_toml() {
        Ok(body) => (body, true),
        Err(error) => {
            log.info(format!("shareable report unavailable: {error:#}"));
            (
                report
                    .to_private_toml()
                    .context("serialize private validation report")?,
                false,
            )
        }
    };
    write_restricted_file(
        &report_path,
        if shareable {
            report_body
        } else {
            format!("# shareable serialization failed; private report follows\n{report_body}")
        },
    )?;

    let descriptor = selected
        .map(|entry| format_descriptor_text(&entry.identity.fingerprint))
        .unwrap_or_else(|| "descriptor unavailable\n".to_string());
    write_restricted_file(output_dir.join("descriptor.txt"), descriptor)?;

    if !shareable {
        log.info("report.toml written in private form because shareable serialization failed");
    }
    write_restricted_file(output_dir.join("validator.log"), log.lines.join("\n"))?;
    Ok(())
}

fn write_restricted_file(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Result<()> {
    let path = path.as_ref();
    std::fs::write(path, contents.as_ref()).with_context(|| format!("write {}", path.display()))?;
    restrict_file_permissions(path)?;
    Ok(())
}

fn restrict_dir_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)
            .with_context(|| format!("stat {}", path.display()))?
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions)
            .with_context(|| format!("chmod 0700 {}", path.display()))?;
    }
    Ok(())
}

fn restrict_file_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)
            .with_context(|| format!("stat {}", path.display()))?
            .permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(path, permissions)
            .with_context(|| format!("chmod 0600 {}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn validation_timestamp() -> String {
    format_utc_timestamp(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0),
    )
}

pub(crate) fn format_utc_timestamp(secs: u64) -> String {
    let (year, month, day) = civil_from_days(secs / 86_400);
    let time_of_day = secs % 86_400;
    let hour = time_of_day / 3_600;
    let minute = (time_of_day % 3_600) / 60;
    let second = time_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}-{minute:02}-{second:02}")
}

fn civil_from_days(mut days: u64) -> (u64, u64, u64) {
    days = days.wrapping_add(719_468);
    let era = days / 146_097;
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::type2_policy::Type2PreHandshakePolicy;
    use crate::transport::usb_fingerprint::{
        UsbDirection, UsbEndpointCapability, UsbInterfaceShape, UsbTransferKind,
    };
    use std::collections::BTreeMap;

    struct MapInventory {
        entries: Vec<InventoryEntry>,
    }

    impl UsbInventory for MapInventory {
        fn inventory_matching(&self, vid: u16, pid: u16) -> Result<Vec<InventoryEntry>> {
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
        candidates: Vec<HidrawCandidate>,
    }

    impl HidrawInventory for MapHidrawInventory {
        fn list_hidraw_candidates(&self) -> Result<Vec<HidrawCandidate>> {
            Ok(self.candidates.clone())
        }
    }

    #[derive(Default)]
    struct MapSysfs {
        files: BTreeMap<PathBuf, String>,
        canonical: BTreeMap<PathBuf, PathBuf>,
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

    impl SysfsAccess for MapSysfs {
        fn canonicalize(&self, path: &Path) -> Result<PathBuf> {
            self.canonical
                .get(path)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no canonical mapping for {}", path.display()))
        }

        fn read_trimmed(&self, path: &Path) -> Result<String> {
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("missing {}", path.display()))
        }

        fn exists(&self, path: &Path) -> bool {
            self.files.contains_key(path)
        }
    }

    fn endpoint(
        address: u8,
        direction: UsbDirection,
        transfer: UsbTransferKind,
        max_packet_size: u16,
    ) -> UsbEndpointCapability {
        UsbEndpointCapability {
            address,
            direction,
            transfer,
            max_packet_size,
            interval: 1,
        }
    }

    fn iface(number: u8, class: u8, endpoints: Vec<UsbEndpointCapability>) -> UsbInterfaceShape {
        UsbInterfaceShape {
            number,
            alternate_setting: 0,
            class,
            subclass: 0,
            protocol: 0,
            endpoints,
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

    fn hid_in_fingerprint() -> UsbFingerprint {
        UsbFingerprint {
            vid: 0x0416,
            pid: 0x5302,
            bcd_device: "4.07".to_string(),
            interfaces: vec![iface(
                0,
                3,
                vec![endpoint(
                    0x81,
                    UsbDirection::In,
                    UsbTransferKind::Interrupt,
                    8,
                )],
            )],
        }
    }

    fn bulk_fingerprint() -> UsbFingerprint {
        UsbFingerprint {
            vid: 0x87ad,
            pid: 0x70db,
            bcd_device: "1.00".to_string(),
            interfaces: vec![iface(
                1,
                255,
                vec![
                    endpoint(0x81, UsbDirection::In, UsbTransferKind::Bulk, 512),
                    endpoint(0x02, UsbDirection::Out, UsbTransferKind::Bulk, 512),
                ],
            )],
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

    #[test]
    fn parse_bus_address_accepts_decimal_pair() {
        let selector = parse_bus_address("1:14").unwrap();
        assert_eq!(selector.bus, 1);
        assert_eq!(selector.address, 14);
    }

    #[test]
    fn parse_bus_address_rejects_malformed_input() {
        let error = parse_bus_address("not-a-bus-address").unwrap_err();
        assert!(error.to_string().contains("BUS:ADDRESS"));
    }

    #[test]
    fn unique_match_resolves_to_explicit_bus_address() {
        let entries = vec![inventory_entry(2, 9, hid_in_fingerprint(), false)];
        let (selector, _) = resolve_selection(&entries, None).unwrap();
        assert_eq!(selector, UsbBusAddress { bus: 2, address: 9 });
    }

    #[test]
    fn duplicate_vid_pid_without_bus_address_errors() {
        let entries = vec![
            inventory_entry(1, 5, hid_in_fingerprint(), false),
            inventory_entry(1, 6, hid_in_fingerprint(), false),
        ];
        let error = resolve_selection(&entries, None).unwrap_err();
        assert!(error.to_string().contains("multiple USB devices match"));
    }

    #[test]
    fn duplicate_vid_pid_with_bus_address_selects() {
        let entries = vec![
            inventory_entry(1, 5, hid_in_fingerprint(), false),
            inventory_entry(1, 6, hid_in_fingerprint(), false),
        ];
        let (selector, entry) = resolve_selection(&entries, Some("1:6")).unwrap();
        assert_eq!(selector, UsbBusAddress { bus: 1, address: 6 });
        assert_eq!(entry.identity.address, 6);
    }

    #[test]
    fn absent_device_errors() {
        let error = resolve_selection(&[], None).unwrap_err();
        assert!(error.to_string().contains("no USB device matching"));
    }

    #[test]
    fn malformed_bus_address_errors() {
        let entries = vec![inventory_entry(1, 5, hid_in_fingerprint(), false)];
        let error = resolve_selection(&entries, Some("bad")).unwrap_err();
        assert!(error.to_string().contains("BUS:ADDRESS"));
    }

    #[test]
    fn correlation_mismatch_fails_without_active_io() {
        let mut log = ValidatorLog::new();
        let fs = MapSysfs::default()
            .link(
                "/sys/class/hidraw/hidraw1/device",
                "/sys/devices/pci0/usb1/1-1/1-1:1.0",
            )
            .insert_file("/sys/devices/pci0/usb1/1-1/1-1:1.0/busnum", "9")
            .insert_file("/sys/devices/pci0/usb1/1-1/1-1:1.0/devnum", "3");
        let hidraw = MapHidrawInventory {
            candidates: vec![
                HidrawCandidate::from_sysfs_class_entry(PathBuf::from("/sys/class/hidraw/hidraw1"))
                    .unwrap(),
            ],
        };
        let error = correlate_hidraw_if_needed(
            &hid_in_fingerprint(),
            UsbBusAddress { bus: 2, address: 7 },
            &hidraw,
            &fs,
            &mut log,
        )
        .unwrap_err();
        assert!(error.to_string().contains("no hidraw node correlates"));
    }

    #[test]
    fn no_hidraw_case_is_allowed_for_bulk_shape() {
        let mut log = ValidatorLog::new();
        let hidraw = MapHidrawInventory { candidates: vec![] };
        let correlated = correlate_hidraw_if_needed(
            &bulk_fingerprint(),
            UsbBusAddress { bus: 1, address: 3 },
            &hidraw,
            &MapSysfs::default(),
            &mut log,
        )
        .unwrap();
        assert_eq!(correlated, None);
    }

    #[test]
    fn passive_report_preserves_hid_in_only_fingerprint() {
        let mut report = HardwareValidationReport::new_in_progress(
            EvidenceOrigin::Physical,
            ValidationScope::Passive,
        );
        report
            .set_fingerprint(&hid_in_fingerprint(), false, Some(true))
            .unwrap();
        report
            .set_pre_handshake_policy(Type2PreHandshakePolicy::Hid407ReadOnlyProbe)
            .unwrap();
        report
            .record_check(CheckField::Enumerated, CheckStatus::Pass)
            .unwrap();
        report
            .record_check(CheckField::PassiveAllowlist, CheckStatus::Pass)
            .unwrap();
        record_passive_not_applicable_checks(&mut report);
        report.finalize_passive_pass().unwrap();

        let iface = report.fingerprint().unwrap().interfaces().first().unwrap();
        let ep = iface.endpoints().first().unwrap();
        assert_eq!(ep.direction(), UsbDirection::In);
        assert_eq!(ep.max_packet_size(), 8);
        assert!(report.checks().get(CheckField::Handshake) == Some(CheckStatus::NotApplicable));
    }

    #[test]
    fn passive_success_report_is_shareable_without_private_paths() {
        let temp = tempfile::tempdir().unwrap();
        let usb = MapInventory {
            entries: vec![inventory_entry(1, 14, hid_in_fingerprint(), true)],
        };
        let hidraw = MapHidrawInventory {
            candidates: vec![
                HidrawCandidate::from_sysfs_class_entry(PathBuf::from("/sys/class/hidraw/hidraw3"))
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
            || "fixed-ts".to_string(),
        )
        .unwrap();

        let report_toml = std::fs::read_to_string(output.join("report.toml")).unwrap();
        assert!(report_toml.contains("result = \"pass\""));
        assert!(!report_toml.contains("/sys/"));
        assert!(!report_toml.contains("/home/"));
        assert!(!report_toml.contains("bus="));
        assert!(!report_toml.contains("address="));
        assert!(!report_toml.contains("serial ="));
        assert!(report_toml.contains("direction = \"in\""));

        let descriptor = std::fs::read_to_string(output.join("descriptor.txt")).unwrap();
        assert!(descriptor.contains("class=3"));
        assert!(!descriptor.contains("/sys/"));
    }

    #[test]
    fn passive_allowlist_fails_for_unsupported_shape() {
        let temp = tempfile::tempdir().unwrap();
        let unsupported = UsbFingerprint {
            vid: 0x1234,
            pid: 0x5678,
            bcd_device: "1.00".to_string(),
            interfaces: vec![],
        };
        let usb = MapInventory {
            entries: vec![inventory_entry(1, 2, unsupported, false)],
        };
        let error = run_validate_device_with(
            ValidateDeviceArgs {
                device: "1234:5678".to_string(),
                bus_address: None,
                passive: true,
                output: temp.path().to_path_buf(),
            },
            &usb,
            &MapHidrawInventory { candidates: vec![] },
            &MapSysfs::default(),
            || "fixed-ts".to_string(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("PassiveAllowlist"));
    }

    #[test]
    fn active_mode_fails_when_inventory_empty() {
        let temp = tempfile::tempdir().unwrap();
        let error = run_validate_device_with(
            ValidateDeviceArgs {
                device: "0416:5302".to_string(),
                bus_address: None,
                passive: false,
                output: temp.path().to_path_buf(),
            },
            &MapInventory { entries: vec![] },
            &MapHidrawInventory { candidates: vec![] },
            &MapSysfs::default(),
            || "fixed-ts".to_string(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("Inventory"));
    }

    #[test]
    fn format_utc_timestamp_is_stable() {
        assert_eq!(format_utc_timestamp(0), "1970-01-01T00-00-00");
    }
}
