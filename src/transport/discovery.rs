// SPDX-License-Identifier: GPL-3.0-or-later
//
// Deterministic multi-family device discovery and TransportConnector.
// Device IDs and recognition rules derived from thermalright-trcc-linux
// at tree 390b880abd4cf0ed2d6eae7151493432263eff39 (project version 9.8.6, four commits after the v9.8.6 tag).

//! Scan for Thermalright full-pixel LCDs and connect the selected device.

use anyhow::{Context, Result, bail};
use log::info;
use std::fmt;
use std::path::{Path, PathBuf};

use super::Transport;
use super::bulk_usb::BulkUsb;
use super::hid_lcd::HidLcd;
use super::ly_lcd::LyLcd;
use super::null::{NullTransport, TransportKind, transport_from_env};
use super::profile::{DeviceInfo, WireProtocol, device_info_from_fixture, fixture_by_id};
use super::scsi_lcd::ScsiLcd;
use super::usb_device::find_device;
use super::usb_fingerprint::{
    DerivedBulkPair, UsbDirection, UsbFingerprint, UsbTransferKind, derive_bulk_pair,
    derive_vendor_bulk_pair, fingerprint_from_device,
};

/// How the user selects which LCD to open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DeviceSelector {
    /// Exactly one physical LCD must be present.
    #[default]
    Auto,
    /// Mirror every discovered supported LCD (including identical VID:PID units).
    All,
    /// Open the unique device with this USB id.
    UsbId { vid: u16, pid: u16 },
}

impl DeviceSelector {
    /// Parse `auto`, `all`, or `VID:PID` (hex, optional `0x` prefix).
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("auto") {
            return Ok(Self::Auto);
        }
        if s.eq_ignore_ascii_case("all") {
            return Ok(Self::All);
        }
        let (vid_s, pid_s) = s.split_once(':').ok_or_else(|| {
            anyhow::anyhow!("device selector must be 'auto', 'all', or 'VID:PID', got {s:?}")
        })?;
        let vid = parse_hex_u16(vid_s).with_context(|| format!("invalid VID in {s:?}"))?;
        let pid = parse_hex_u16(pid_s).with_context(|| format!("invalid PID in {s:?}"))?;
        Ok(Self::UsbId { vid, pid })
    }

    /// Whether this selector drives every matching display in mirror mode.
    pub fn is_mirror_all(&self) -> bool {
        matches!(self, Self::All)
    }
}

impl fmt::Display for DeviceSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::All => write!(f, "all"),
            Self::UsbId { vid, pid } => write!(f, "{vid:04x}:{pid:04x}"),
        }
    }
}

fn parse_hex_u16(s: &str) -> Result<u16> {
    let s = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    u16::from_str_radix(s, 16).with_context(|| format!("not a hex u16: {s:?}"))
}

/// Physical path used to open a discovered device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevicePath {
    Usb {
        bus: u8,
        address: u8,
        interface: u8,
        ep_in: u8,
        ep_out: u8,
    },
    Scsi {
        devnode: PathBuf,
        sysfs_device: PathBuf,
        usb_bus: Option<u8>,
        usb_address: Option<u8>,
    },
}

/// One discovered full-pixel LCD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredDevice {
    pub vid: u16,
    pub pid: u16,
    pub protocol: WireProtocol,
    pub serial: Option<String>,
    pub path: DevicePath,
}

impl DiscoveredDevice {
    pub fn identity(&self) -> String {
        match &self.path {
            DevicePath::Usb { bus, address, .. } => {
                format!(
                    "{:04x}:{:04x} {} bus={} addr={}",
                    self.vid, self.pid, self.protocol, bus, address
                )
            }
            DevicePath::Scsi { devnode, .. } => {
                format!(
                    "{:04x}:{:04x} {} {}",
                    self.vid,
                    self.pid,
                    self.protocol,
                    devnode.display()
                )
            }
        }
    }
}

/// Known full-pixel LCD USB IDs (LED/segment 0416:8001 excluded).
pub const KNOWN_LCD_IDS: &[(u16, u16, WireProtocol)] = &[
    (0x87cd, 0x70db, WireProtocol::Scsi),
    (0x0402, 0x3922, WireProtocol::Scsi),
    (0x87ad, 0x70db, WireProtocol::Bulk),
    (0x0416, 0x5302, WireProtocol::HidType2),
    (0x0418, 0x5303, WireProtocol::HidType3),
    (0x0418, 0x5304, WireProtocol::HidType3),
    (0x0416, 0x5408, WireProtocol::Ly),
    (0x0416, 0x5409, WireProtocol::Ly),
    // 0416:5406 is dual-shape (bulk preferred, else SCSI) — special-cased.
    (0x0416, 0x5406, WireProtocol::Bulk),
];

const SCSI_ONLY_LCD_IDS: &[(u16, u16)] = &[(0x87cd, 0x70db), (0x0402, 0x3922)];
const DUAL_PATH_LCD_ID: (u16, u16) = (0x0416, 0x5406);

pub(crate) fn protocol_for_id(vid: u16, pid: u16) -> Option<WireProtocol> {
    KNOWN_LCD_IDS
        .iter()
        .find(|(v, p, _)| *v == vid && *p == pid)
        .map(|(_, _, proto)| *proto)
}

fn scsi_protocol_for_id(vid: u16, pid: u16, bulk_claimed: bool) -> Option<WireProtocol> {
    if SCSI_ONLY_LCD_IDS.contains(&(vid, pid)) {
        return Some(WireProtocol::Scsi);
    }
    if (vid, pid) == DUAL_PATH_LCD_ID && !bulk_claimed {
        return Some(WireProtocol::Scsi);
    }
    None
}

/// Output route for generic (non-Type2) known LCD validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LcdTransportRoute {
    LegacyBulk,
    ScsiCommand,
}

const USB_CLASS_MASS_STORAGE: u8 = 8;

fn mass_storage_bulk_pair(fingerprint: &UsbFingerprint) -> Option<DerivedBulkPair> {
    for shape in &fingerprint.interfaces {
        if shape.class != USB_CLASS_MASS_STORAGE {
            continue;
        }
        let mut ep_in = 0u8;
        let mut ep_out = 0u8;
        for endpoint in &shape.endpoints {
            if endpoint.transfer != UsbTransferKind::Bulk {
                continue;
            }
            match endpoint.direction {
                UsbDirection::In => ep_in = endpoint.address,
                UsbDirection::Out => ep_out = endpoint.address,
            }
        }
        if ep_in != 0 && ep_out != 0 {
            return Some(DerivedBulkPair {
                interface: shape.number,
                ep_in,
                ep_out,
                vendor_class: false,
            });
        }
    }
    None
}

/// Resolve protocol and transport route from [`KNOWN_LCD_IDS`] and observed USB shape.
///
/// Mirrors `scan_usb` / `scan_scsi` routing: dual-path `0416:5406` prefers vendor bulk,
/// else SCSI; SCSI-only IDs and mass-storage bulk defer to `ScsiCommand`; other bulk/HID3/LY
/// IDs require a bulk IN+OUT pair suitable for discovery.
pub fn resolve_known_lcd_route(
    vid: u16,
    pid: u16,
    fingerprint: &UsbFingerprint,
) -> Result<(WireProtocol, LcdTransportRoute)> {
    let Some(base_protocol) = protocol_for_id(vid, pid) else {
        bail!("unknown LCD VID:PID {vid:04x}:{pid:04x}");
    };

    let bulk_claimed = vendor_bulk_endpoints(derive_vendor_bulk_pair(fingerprint)).is_some();

    match base_protocol {
        WireProtocol::Bulk if (vid, pid) == DUAL_PATH_LCD_ID => {
            if bulk_claimed {
                derive_vendor_bulk_pair(fingerprint).ok_or_else(|| {
                    anyhow::anyhow!("0416:5406 vendor bulk route missing IN+OUT pair")
                })?;
                Ok((WireProtocol::Bulk, LcdTransportRoute::LegacyBulk))
            } else if scsi_protocol_for_id(vid, pid, false) == Some(WireProtocol::Scsi) {
                mass_storage_bulk_pair(fingerprint).ok_or_else(|| {
                    anyhow::anyhow!("0416:5406 SCSI route missing mass-storage bulk IN+OUT pair")
                })?;
                Ok((WireProtocol::Scsi, LcdTransportRoute::ScsiCommand))
            } else {
                bail!("0416:5406 shape has no supported bulk or SCSI route");
            }
        }
        WireProtocol::Bulk => {
            derive_bulk_pair(fingerprint).ok_or_else(|| {
                anyhow::anyhow!(
                    "bulk route missing same-interface bulk IN+OUT pair for {vid:04x}:{pid:04x}"
                )
            })?;
            Ok((WireProtocol::Bulk, LcdTransportRoute::LegacyBulk))
        }
        WireProtocol::Scsi => {
            mass_storage_bulk_pair(fingerprint).ok_or_else(|| {
                anyhow::anyhow!(
                    "SCSI route missing mass-storage bulk IN+OUT pair for {vid:04x}:{pid:04x}"
                )
            })?;
            Ok((WireProtocol::Scsi, LcdTransportRoute::ScsiCommand))
        }
        WireProtocol::HidType3 | WireProtocol::Ly => {
            derive_bulk_pair(fingerprint).ok_or_else(|| {
                anyhow::anyhow!(
                    "{} route missing bulk IN+OUT pair for {vid:04x}:{pid:04x}",
                    base_protocol.as_str()
                )
            })?;
            Ok((base_protocol, LcdTransportRoute::LegacyBulk))
        }
        WireProtocol::HidType2 => {
            bail!("HID Type2 uses record_negotiated_type2, not generic negotiated device");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScsiUsbCandidate {
    vid: u16,
    pid: u16,
    bus: u8,
    address: u8,
}

fn ensure_scsi_candidates_resolved(
    candidates: &[ScsiUsbCandidate],
    devices: &[DiscoveredDevice],
) -> Result<()> {
    for candidate in candidates {
        let resolved = devices.iter().any(|device| {
            device.vid == candidate.vid
                && device.pid == candidate.pid
                && matches!(
                    device.path,
                    DevicePath::Scsi {
                        usb_bus: Some(bus),
                        usb_address: Some(address),
                        ..
                    } if bus == candidate.bus && address == candidate.address
                )
        });
        if !resolved {
            bail!(
                "SCSI LCD {:04x}:{:04x} bus={} addr={} was detected over USB but no usable scsi_generic device was found",
                candidate.vid,
                candidate.pid,
                candidate.bus,
                candidate.address
            );
        }
    }
    Ok(())
}

fn vendor_bulk_endpoints(pair: Option<DerivedBulkPair>) -> Option<(u8, u8, u8)> {
    pair.filter(|pair| pair.vendor_class)
        .map(|pair| (pair.interface, pair.ep_in, pair.ep_out))
}

fn bulk_endpoints(pair: Option<DerivedBulkPair>) -> Option<(u8, u8, u8)> {
    pair.map(|pair| (pair.interface, pair.ep_in, pair.ep_out))
}

/// Outcome of routing a known USB LCD through bulk-endpoint discovery.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsbBulkDiscoveryOutcome {
    /// Bulk IN+OUT pair suitable for `DevicePath::Usb`.
    Endpoints(u8, u8, u8),
    /// No supported route for this protocol/shape.
    Unsupported,
}

#[allow(dead_code)]
fn usb_bulk_discovery_outcome(
    protocol: WireProtocol,
    fingerprint: &UsbFingerprint,
) -> UsbBulkDiscoveryOutcome {
    if let Some(endpoints) = bulk_endpoints(derive_bulk_pair(fingerprint)) {
        return UsbBulkDiscoveryOutcome::Endpoints(endpoints.0, endpoints.1, endpoints.2);
    }
    // HID Type2 panels often expose interrupt IN/OUT instead of bulk.
    if protocol == WireProtocol::HidType2 {
        if let Some((iface, ep_in, ep_out)) =
            crate::transport::usb_fingerprint::derive_hid_interrupt_pair(fingerprint)
        {
            return UsbBulkDiscoveryOutcome::Endpoints(iface, ep_in, ep_out);
        }
    }
    UsbBulkDiscoveryOutcome::Unsupported
}

/// Build a bulk [`DiscoveredDevice`] from a passive inventory fingerprint.
///
/// Does not scan libusb; peer displays cannot affect the result.
pub fn discovered_bulk_from_fingerprint(
    vid: u16,
    pid: u16,
    bus: u8,
    address: u8,
    fingerprint: &UsbFingerprint,
) -> Result<DiscoveredDevice> {
    let protocol = protocol_for_id(vid, pid)
        .filter(|p| *p == WireProtocol::Bulk)
        .ok_or_else(|| anyhow::anyhow!("{vid:04x}:{pid:04x} is not a bulk LCD in KNOWN_LCD_IDS"))?;
    let (interface, ep_in, ep_out) =
        bulk_endpoints(derive_bulk_pair(fingerprint)).ok_or_else(|| {
            anyhow::anyhow!(
                "bulk route missing same-interface bulk IN+OUT pair for {vid:04x}:{pid:04x}"
            )
        })?;
    Ok(DiscoveredDevice {
        vid,
        pid,
        protocol,
        serial: None,
        path: DevicePath::Usb {
            bus,
            address,
            interface,
            ep_in,
            ep_out,
        },
    })
}

/// Build the only production bulk route from a complete exact fingerprint.
pub fn discovered_exact_square_from_fingerprint(
    bus: u8,
    address: u8,
    fingerprint: &UsbFingerprint,
) -> Result<DiscoveredDevice> {
    anyhow::ensure!(
        matches!(
            crate::transport::policy::exact_descriptor_policy(fingerprint),
            Ok(crate::transport::policy::ExactDescriptorPolicy::Square87ad)
        ),
        "fingerprint is not the exact square production descriptor"
    );
    Ok(DiscoveredDevice {
        vid: fingerprint.vid,
        pid: fingerprint.pid,
        protocol: WireProtocol::Bulk,
        serial: None,
        path: DevicePath::Usb {
            bus,
            address,
            interface: 0,
            ep_in: 0x81,
            ep_out: 0x01,
        },
    })
}

fn read_fingerprint<T: rusb::UsbContext>(
    device: &rusb::Device<T>,
    bus: u8,
    address: u8,
) -> Result<UsbFingerprint> {
    fingerprint_from_device(device)
        .with_context(|| format!("failed to read USB fingerprint at bus={bus} addr={address}"))
}

/// Scan libusb + scsi_generic for full-pixel LCDs.
pub fn scan_devices() -> Result<Vec<DiscoveredDevice>> {
    let mut devices = Vec::new();
    let mut scsi_candidates = Vec::new();
    scan_usb(&mut devices, &mut scsi_candidates)?;
    scan_scsi(&mut devices)?;
    ensure_scsi_candidates_resolved(&scsi_candidates, &devices)?;
    Ok(devices)
}

fn scan_usb(
    out: &mut Vec<DiscoveredDevice>,
    _scsi_candidates: &mut Vec<ScsiUsbCandidate>,
) -> Result<()> {
    let list = rusb::devices().context("libusb device list failed")?;
    for device in list.iter() {
        let bus = device.bus_number();
        let address = device.address();
        let desc = device.device_descriptor().with_context(|| {
            format!("failed to read USB descriptor at bus={bus} addr={address}")
        })?;
        let vid = desc.vendor_id();
        let pid = desc.product_id();
        if protocol_for_id(vid, pid).is_none() {
            continue;
        }

        let fingerprint = read_fingerprint(&device, bus, address)?;
        let descriptor = match crate::transport::policy::exact_descriptor_policy(&fingerprint) {
            Ok(descriptor) => descriptor,
            Err(error) => {
                log::warn!(
                    "ignoring unsupported known LCD {:04x}:{:04x}: {error:#}",
                    vid,
                    pid
                );
                continue;
            }
        };
        let (protocol, interface, ep_in, ep_out) = match descriptor {
            crate::transport::policy::ExactDescriptorPolicy::Square87ad => {
                (WireProtocol::Bulk, 0, 0x81, 0x01)
            }
            crate::transport::policy::ExactDescriptorPolicy::Type2 => {
                (WireProtocol::HidType2, 0, 0x83, 0x02)
            }
        };
        if protocol_for_id(vid, pid) != Some(protocol) {
            bail!("production descriptor/protocol mismatch for {vid:04x}:{pid:04x}");
        }
        out.push(DiscoveredDevice {
            vid,
            pid,
            protocol,
            serial: None,
            path: DevicePath::Usb {
                bus,
                address,
                interface,
                ep_in,
                ep_out,
            },
        });
    }
    Ok(())
}

fn scan_scsi(_out: &mut Vec<DiscoveredDevice>) -> Result<()> {
    // No SCSI identity has an evidence-backed production row.
    Ok(())
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
struct UsbAncestor {
    vid: u16,
    pid: u16,
    bus: Option<u8>,
    address: Option<u8>,
}

#[allow(dead_code)]
fn belongs_to_usb_ancestor(device: &DiscoveredDevice, ancestor: UsbAncestor) -> bool {
    let (Some(expected_bus), Some(expected_address)) = (ancestor.bus, ancestor.address) else {
        return false;
    };
    match &device.path {
        DevicePath::Usb { bus, address, .. } => {
            *bus == expected_bus && *address == expected_address
        }
        DevicePath::Scsi {
            usb_bus,
            usb_address,
            ..
        } => *usb_bus == Some(expected_bus) && *usb_address == Some(expected_address),
    }
}
#[allow(dead_code)]
fn resolve_usb_ancestor(sysfs_sg: &Path) -> Option<UsbAncestor> {
    // Walk device symlink parents looking for idVendor/idProduct.
    let device_link = sysfs_sg.join("device");
    let mut current = std::fs::canonicalize(&device_link).ok()?;
    for _ in 0..12 {
        let vid_path = current.join("idVendor");
        let pid_path = current.join("idProduct");
        if vid_path.exists() && pid_path.exists() {
            let vid = std::fs::read_to_string(vid_path).ok()?;
            let pid = std::fs::read_to_string(pid_path).ok()?;
            let bus = std::fs::read_to_string(current.join("busnum"))
                .ok()
                .and_then(|value| value.trim().parse().ok());
            let address = std::fs::read_to_string(current.join("devnum"))
                .ok()
                .and_then(|value| value.trim().parse().ok());
            return Some(UsbAncestor {
                vid: u16::from_str_radix(vid.trim(), 16).ok()?,
                pid: u16::from_str_radix(pid.trim(), 16).ok()?,
                bus,
                address,
            });
        }
        current = current.parent()?.to_path_buf();
    }
    None
}

fn selector_matches(device: &DiscoveredDevice, selector: &DeviceSelector) -> bool {
    match selector {
        DeviceSelector::Auto | DeviceSelector::All => true,
        DeviceSelector::UsbId { vid, pid } => device.vid == *vid && device.pid == *pid,
    }
}

fn device_path_sort_key(path: &DevicePath) -> (u8, u8, String) {
    match path {
        DevicePath::Usb { bus, address, .. } => (0, *bus, format!("{address:03}")),
        DevicePath::Scsi { devnode, .. } => (1, 0, devnode.to_string_lossy().into_owned()),
    }
}

/// Stable ordering for multi-display mirror mode (identical VID:PID units included).
pub fn sort_discovered(devices: &mut [DiscoveredDevice]) {
    devices.sort_by(|left, right| {
        left.vid
            .cmp(&right.vid)
            .then(left.pid.cmp(&right.pid))
            .then(device_path_sort_key(&left.path).cmp(&device_path_sort_key(&right.path)))
    });
}

fn no_matching_device_error(
    devices: &[DiscoveredDevice],
    selector: &DeviceSelector,
) -> anyhow::Error {
    let available = if devices.is_empty() {
        "none".to_string()
    } else {
        devices
            .iter()
            .map(|d| d.identity())
            .collect::<Vec<_>>()
            .join("; ")
    };
    anyhow::anyhow!(
        "no matching LCD for selector '{selector}' (available: {available}). \
         Check udev rules, cable access, and that the cooler is plugged in."
    )
}

/// Select devices matching `selector`. Errors on zero or ambiguous matches.
pub fn select_devices(
    devices: &[DiscoveredDevice],
    selector: &DeviceSelector,
) -> Result<DiscoveredDevice> {
    if matches!(selector, DeviceSelector::All) {
        bail!("select_devices does not support selector 'all'; use select_all_devices instead");
    }

    let matched: Vec<&DiscoveredDevice> = devices
        .iter()
        .filter(|device| selector_matches(device, selector))
        .collect();

    match matched.as_slice() {
        [] => Err(no_matching_device_error(devices, selector)),
        [one] => Ok((*one).clone()),
        many => {
            let list = many
                .iter()
                .map(|d| d.identity())
                .collect::<Vec<_>>()
                .join("; ");
            bail!(
                "ambiguous LCD selection for '{selector}': found {} devices: {list}. \
                 Set display.device to a specific VID:PID, use 'all' to mirror every display, or unplug extras.",
                many.len()
            );
        }
    }
}

/// Select one device per target `VID:PID` in config order.
///
/// Each target consumes the first unused match in stable discovery order.
/// Missing or exhausted matches error with the failing selector. Duplicate
/// targets in `targets` are rejected.
pub fn select_target_devices(
    devices: &[DiscoveredDevice],
    targets: &[(u16, u16)],
) -> Result<Vec<DiscoveredDevice>> {
    if targets.is_empty() {
        bail!("select_target_devices requires at least one VID:PID target");
    }
    let mut seen_targets = std::collections::HashSet::new();
    for &(vid, pid) in targets {
        if !seen_targets.insert((vid, pid)) {
            bail!(
                "duplicate independent target {:04x}:{:04x}; use distinct VID:PIDs or display.device=\"all\" to mirror duplicates",
                vid,
                pid
            );
        }
    }

    let mut pool = devices.to_vec();
    sort_discovered(&mut pool);
    let mut selected = Vec::with_capacity(targets.len());
    let mut used = vec![false; pool.len()];

    for &(vid, pid) in targets {
        let mut found = None;
        for (idx, candidate) in pool.iter().enumerate() {
            if used[idx] {
                continue;
            }
            if candidate.vid == vid && candidate.pid == pid {
                found = Some(idx);
                break;
            }
        }
        match found {
            Some(idx) => {
                used[idx] = true;
                selected.push(pool[idx].clone());
            }
            None => {
                let available = if devices.is_empty() {
                    "none".to_string()
                } else {
                    devices
                        .iter()
                        .map(|d| d.identity())
                        .collect::<Vec<_>>()
                        .join("; ")
                };
                bail!(
                    "no matching LCD for independent target {:04x}:{:04x} (available: {available}). \
                     Check udev rules, cable access, and that the cooler is plugged in.",
                    vid,
                    pid
                );
            }
        }
    }
    Ok(selected)
}

/// Select every device matching `selector` in deterministic order.
///
/// For `all`, returns all supported displays (including duplicate VID:PID units).
/// For `auto` and `usb_id`, behaves like [`select_devices`] but returns a one-element vec.
pub fn select_all_devices(
    devices: &[DiscoveredDevice],
    selector: &DeviceSelector,
) -> Result<Vec<DiscoveredDevice>> {
    let mut matched: Vec<DiscoveredDevice> = devices
        .iter()
        .filter(|device| selector_matches(device, selector))
        .cloned()
        .collect();
    if matched.is_empty() {
        return Err(no_matching_device_error(devices, selector));
    }

    sort_discovered(&mut matched);

    match selector {
        DeviceSelector::All => Ok(matched),
        DeviceSelector::Auto | DeviceSelector::UsbId { .. } => {
            if matched.len() > 1 {
                let list = matched
                    .iter()
                    .map(|d| d.identity())
                    .collect::<Vec<_>>()
                    .join("; ");
                bail!(
                    "ambiguous LCD selection for '{selector}': found {} devices: {list}. \
                     Set display.device to a specific VID:PID, use 'all' to mirror every display, or unplug extras.",
                    matched.len()
                );
            }
            Ok(matched)
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum HardwareConnectError {
    #[error(transparent)]
    NoDevice(anyhow::Error),
    #[error(transparent)]
    Failed(anyhow::Error),
}

impl HardwareConnectError {
    fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::NoDevice(error) | Self::Failed(error) => error,
        }
    }
}

#[cfg(test)]
fn select_scanned_device(
    scan: Result<Vec<DiscoveredDevice>>,
    selector: &DeviceSelector,
) -> std::result::Result<DiscoveredDevice, HardwareConnectError> {
    let devices = scan.map_err(HardwareConnectError::Failed)?;
    if !devices
        .iter()
        .any(|device| selector_matches(device, selector))
    {
        let error = select_devices(&devices, selector)
            .expect_err("zero matching devices must produce a selection error");
        return Err(HardwareConnectError::NoDevice(error));
    }
    select_devices(&devices, selector).map_err(HardwareConnectError::Failed)
}

#[cfg(test)]
fn connect_scanned_with<T, F>(
    scan: Result<Vec<DiscoveredDevice>>,
    selector: &DeviceSelector,
    open: F,
) -> std::result::Result<T, HardwareConnectError>
where
    F: FnOnce(&DiscoveredDevice) -> Result<T>,
{
    let selected = select_scanned_device(scan, selector)?;
    open(&selected).map_err(HardwareConnectError::Failed)
}

fn hardware_or_no_device<T>(
    result: std::result::Result<T, HardwareConnectError>,
) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(HardwareConnectError::NoDevice(_)) => Ok(None),
        Err(HardwareConnectError::Failed(error)) => Err(error),
    }
}

/// One opened display transport plus negotiated profile.
pub struct OpenedDisplay {
    pub transport: Box<dyn Transport>,
    pub info: DeviceInfo,
}

/// All displays opened for the active mirror group. The first entry is primary.
pub struct ConnectedOutputs {
    pub outputs: Vec<OpenedDisplay>,
}

impl ConnectedOutputs {
    pub fn primary(&self) -> &DeviceInfo {
        &self.outputs[0].info
    }

    pub fn display_count(&self) -> u32 {
        self.outputs.len() as u32
    }

    pub fn from_single(transport: Box<dyn Transport>, info: DeviceInfo) -> Self {
        Self {
            outputs: vec![OpenedDisplay { transport, info }],
        }
    }
}

fn open_all_discovered(devices: &[DiscoveredDevice]) -> Result<Vec<OpenedDisplay>> {
    let mut opened = Vec::with_capacity(devices.len());
    for device in devices {
        match open_discovered(device) {
            Ok((transport, info)) => {
                info!("Selected device {}", device.identity());
                opened.push(OpenedDisplay { transport, info });
            }
            Err(error) => {
                for mut output in opened {
                    output.transport.close();
                }
                return Err(error);
            }
        }
    }
    Ok(opened)
}

fn null_connected_outputs(info: DeviceInfo) -> Result<ConnectedOutputs> {
    let mut transport = NullTransport::with_profile(info.clone());
    let _ = transport.handshake()?;
    Ok(ConnectedOutputs::from_single(Box::new(transport), info))
}

/// Opens, handshakes, and returns a negotiated transport pair.
pub struct TransportConnector {
    pub selector: DeviceSelector,
    /// When set, `connect_all` opens these ordered VID:PID targets (independent mode)
    /// instead of using `selector`.
    pub targets: Option<Vec<(u16, u16)>>,
}

impl TransportConnector {
    pub fn new(selector: DeviceSelector) -> Self {
        Self {
            selector,
            targets: None,
        }
    }

    pub fn with_targets(targets: Vec<(u16, u16)>) -> Self {
        Self {
            selector: DeviceSelector::Auto,
            targets: Some(targets),
        }
    }

    pub fn from_config_device(device: &str) -> Result<Self> {
        Ok(Self::new(DeviceSelector::parse(device)?))
    }

    /// Discover + construct + handshake. Honors null transport and fixture env.
    ///
    /// `THERMALWRITER_PROFILE` is resolved first (immutable after process start).
    /// With `THERMALWRITER_TRANSPORT=null` (or profile without hardware), the
    /// fixture's `DeviceInfo` is passed to `NullTransport::with_profile`.
    ///
    /// For `display.device = "all"`, use [`Self::connect_all`] instead.
    pub fn connect(&self) -> Result<(Box<dyn Transport>, DeviceInfo)> {
        if self.selector.is_mirror_all() {
            bail!("connect() does not support selector 'all'; use connect_all() instead");
        }
        let connected = self.connect_all()?;
        let OpenedDisplay { transport, info } = connected
            .outputs
            .into_iter()
            .next()
            .expect("non-all connect always returns exactly one output");
        Ok((transport, info))
    }

    /// Discover, open, and handshake every display selected by the connector.
    ///
    /// Mirror mode (`all`) opens every supported device in deterministic order.
    /// `auto` and `VID:PID` return a single output. Null transport and fixture
    /// fallback always yield one output.
    pub fn connect_all(&self) -> Result<ConnectedOutputs> {
        let owned = self.targets.as_deref();
        self.connect_all_inner(owned)
    }

    /// Open an ordered list of distinct `VID:PID` targets (independent multi-display).
    ///
    /// Null transport / fixture env still yield a single output (existing constraint).
    pub fn connect_targets(&self, targets: &[(u16, u16)]) -> Result<ConnectedOutputs> {
        if targets.is_empty() {
            bail!("connect_targets requires at least one VID:PID");
        }
        self.connect_all_inner(Some(targets))
    }

    fn connect_all_inner(&self, targets: Option<&[(u16, u16)]>) -> Result<ConnectedOutputs> {
        let profile_id = std::env::var("THERMALWRITER_PROFILE")
            .ok()
            .filter(|s| !s.is_empty());
        if let Some(id) = &profile_id {
            // Validate fixture id early so bad env fails fast.
            let _ = fixture_by_id(id)?;
        }

        let force_null = matches!(
            transport_from_env(std::env::var("THERMALWRITER_TRANSPORT").ok().as_deref()),
            TransportKind::Null
        );

        if force_null {
            if targets.map(|t| t.len()).unwrap_or(0) > 1 {
                bail!(
                    "independent multi-display with null transport is not supported (got {} targets)",
                    targets.map(|t| t.len()).unwrap_or(0)
                );
            }
            let info = if let Some(id) = &profile_id {
                device_info_from_fixture(id)?
            } else {
                device_info_from_fixture("bulk-87ad-70db-pm4-sub5-fbl72")?
            };
            if let Some(id) = &profile_id {
                info!("Null transport with THERMALWRITER_PROFILE={id}");
            } else {
                info!("Null transport with default bulk 480x480 fixture");
            }
            return null_connected_outputs(info);
        }

        if let Some(id) = &profile_id {
            // Prefer real hardware when present. A fixture fallback is valid only
            // after a successful scan positively establishes no selector match.
            if let Some(connected) = hardware_or_no_device(self.connect_hardware_all(targets))? {
                return Ok(connected);
            }
            if targets.map(|t| t.len()).unwrap_or(0) > 1 {
                bail!(
                    "THERMALWRITER_PROFILE fixture fallback cannot satisfy {} independent targets",
                    targets.map(|t| t.len()).unwrap_or(0)
                );
            }
            let info = device_info_from_fixture(id)?;
            info!("THERMALWRITER_PROFILE={id}: using fixture profile without hardware");
            return null_connected_outputs(info);
        }

        self.connect_hardware_all(targets)
            .map_err(HardwareConnectError::into_anyhow)
    }

    fn connect_hardware_all(
        &self,
        targets: Option<&[(u16, u16)]>,
    ) -> std::result::Result<ConnectedOutputs, HardwareConnectError> {
        let devices = scan_devices().map_err(HardwareConnectError::Failed)?;
        let selected = if let Some(targets) = targets {
            match select_target_devices(&devices, targets) {
                Ok(selected) => selected,
                Err(error) => {
                    // Treat total absence of any target match as NoDevice for fixture fallback.
                    let any_match = targets
                        .iter()
                        .any(|(vid, pid)| devices.iter().any(|d| d.vid == *vid && d.pid == *pid));
                    if !any_match {
                        return Err(HardwareConnectError::NoDevice(error));
                    }
                    return Err(HardwareConnectError::Failed(error));
                }
            }
        } else {
            if !devices
                .iter()
                .any(|device| selector_matches(device, &self.selector))
            {
                let error = select_all_devices(&devices, &self.selector)
                    .expect_err("zero matching devices must produce a selection error");
                return Err(HardwareConnectError::NoDevice(error));
            }
            select_all_devices(&devices, &self.selector).map_err(HardwareConnectError::Failed)?
        };
        open_all_discovered(&selected)
            .map(|outputs| ConnectedOutputs { outputs })
            .map_err(HardwareConnectError::Failed)
    }
}

pub(crate) fn open_discovered(dev: &DiscoveredDevice) -> Result<(Box<dyn Transport>, DeviceInfo)> {
    if (dev.vid, dev.pid)
        != (
            crate::transport::policy::SQUARE_VID,
            crate::transport::policy::SQUARE_PID,
        )
        && (dev.vid, dev.pid)
            != (
                crate::transport::policy::WINBOND_VID,
                crate::transport::policy::WINBOND_PID,
            )
    {
        bail!(
            "unsupported exact production identity {:04x}:{:04x}; no output route",
            dev.vid,
            dev.pid
        );
    }
    if !matches!(dev.protocol, WireProtocol::Bulk | WireProtocol::HidType2) {
        bail!(
            "unsupported exact production identity {:04x}:{:04x}; no output route",
            dev.vid,
            dev.pid
        );
    }
    let (bus, address) = match &dev.path {
        DevicePath::Usb { bus, address, .. } => (*bus, *address),
        _ => bail!("production LCD requires an exact USB descriptor path"),
    };
    let device = find_device(bus, address)?;
    let fingerprint = fingerprint_from_device(&device)?;
    let descriptor = crate::transport::policy::exact_descriptor_policy(&fingerprint)?;
    let expected_protocol = match descriptor {
        crate::transport::policy::ExactDescriptorPolicy::Square87ad => WireProtocol::Bulk,
        crate::transport::policy::ExactDescriptorPolicy::Type2 => WireProtocol::HidType2,
    };
    if dev.protocol != expected_protocol || fingerprint.vid != dev.vid || fingerprint.pid != dev.pid
    {
        bail!("discovered device does not match exact production identity");
    }
    if let DevicePath::Usb {
        interface,
        ep_in,
        ep_out,
        ..
    } = &dev.path
    {
        let valid_path = match descriptor {
            crate::transport::policy::ExactDescriptorPolicy::Square87ad => {
                (*interface, *ep_in, *ep_out) == (0, 0x81, 0x01)
            }
            crate::transport::policy::ExactDescriptorPolicy::Type2 => {
                (*interface, *ep_in, *ep_out) == (0, 0x83, 0x02)
            }
        };
        if !valid_path {
            bail!("discovered endpoint path is not the exact policy path");
        }
    }
    match (&dev.path, dev.protocol) {
        (
            DevicePath::Usb {
                bus,
                address,
                interface,
                ep_in,
                ep_out,
            },
            WireProtocol::Bulk,
        ) => {
            let mut t = BulkUsb::open_at(*bus, *address, *interface, *ep_in, *ep_out)
                .with_context(|| {
                    format!(
                        "failed to open bulk USB {:04x}:{:04x} (check udev rules and replug)",
                        dev.vid, dev.pid
                    )
                })?;
            let info = t.handshake().with_context(|| {
                format!("bulk handshake failed for {:04x}:{:04x}", dev.vid, dev.pid)
            })?;
            Ok((Box::new(t), info))
        }
        (
            DevicePath::Usb {
                bus,
                address,
                interface,
                ep_in,
                ep_out,
            },
            WireProtocol::HidType2,
        ) => {
            let mut t = HidLcd::open_type2(*bus, *address, *interface, *ep_in, *ep_out)?;
            let info = t.handshake()?;
            Ok((Box::new(t), info))
        }
        (
            DevicePath::Usb {
                bus,
                address,
                interface,
                ep_in,
                ep_out,
            },
            WireProtocol::HidType3,
        ) => {
            let mut t = HidLcd::open_type3(dev.pid, *bus, *address, *interface, *ep_in, *ep_out)?;
            let info = t.handshake()?;
            Ok((Box::new(t), info))
        }
        (
            DevicePath::Usb {
                bus,
                address,
                interface,
                ep_in,
                ep_out,
            },
            WireProtocol::Ly,
        ) => {
            let mut t = LyLcd::open(dev.pid, *bus, *address, *interface, *ep_in, *ep_out)?;
            let info = t.handshake()?;
            Ok((Box::new(t), info))
        }
        (DevicePath::Scsi { devnode, .. }, WireProtocol::Scsi) => {
            let mut t = ScsiLcd::open(devnode, dev.vid, dev.pid).with_context(|| {
                format!(
                    "failed to open SCSI node {} for {:04x}:{:04x} (check udev/scsi_generic access)",
                    devnode.display(),
                    dev.vid,
                    dev.pid
                )
            })?;
            let info = t.handshake()?;
            Ok((Box::new(t), info))
        }
        (path, protocol) => {
            bail!("unsupported path/protocol combination: path={path:?} protocol={protocol}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::usb_fingerprint::{UsbEndpointCapability, UsbInterfaceShape};

    fn udev_hex_attr(line: &str, attr: &str) -> Option<u16> {
        let prefix = format!("ATTRS{{{attr}}}==\"");
        let start = line.find(&prefix)? + prefix.len();
        let value = line[start..].split('"').next()?;
        u16::from_str_radix(value, 16).ok()
    }

    fn udev_rule(rules: &'static str, subsystem: &str, vid: u16, pid: u16) -> &'static str {
        let subsystem = format!("SUBSYSTEM==\"{subsystem}\"");
        let vendor = format!("idVendor}}==\"{vid:04x}\"");
        let product = format!("idProduct}}==\"{pid:04x}\"");
        let matching: Vec<_> = rules
            .lines()
            .filter(|line| {
                line.starts_with(&subsystem) && line.contains(&vendor) && line.contains(&product)
            })
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "expected exactly one {subsystem} udev rule for {vid:04x}:{pid:04x}, found {matching:?}"
        );
        matching[0]
    }

    #[test]
    fn scsi_discovery_matrix_covers_permitted_ids_and_dual_path() {
        let expected_scsi_only = [(0x87cd, 0x70db), (0x0402, 0x3922)];
        assert_eq!(SCSI_ONLY_LCD_IDS, expected_scsi_only);

        let mut known_scsi_ids: Vec<_> = KNOWN_LCD_IDS
            .iter()
            .filter_map(|(vid, pid, protocol)| {
                (*protocol == WireProtocol::Scsi).then_some((*vid, *pid))
            })
            .collect();
        known_scsi_ids.sort_unstable();
        let mut routed_scsi_ids = SCSI_ONLY_LCD_IDS.to_vec();
        routed_scsi_ids.sort_unstable();
        assert_eq!(
            known_scsi_ids, routed_scsi_ids,
            "USB-declared SCSI IDs and scan_scsi routing matrix drifted"
        );

        for (vid, pid) in expected_scsi_only {
            assert_eq!(
                protocol_for_id(vid, pid),
                Some(WireProtocol::Scsi),
                "USB discovery matrix omitted {vid:04x}:{pid:04x}"
            );
            assert_eq!(
                scsi_protocol_for_id(vid, pid, false),
                Some(WireProtocol::Scsi),
                "SCSI discovery matrix omitted {vid:04x}:{pid:04x}"
            );
            assert_eq!(
                scsi_protocol_for_id(vid, pid, true),
                Some(WireProtocol::Scsi),
                "SCSI-only ID must not be suppressed by unrelated bulk devices"
            );
        }

        let (vid, pid) = DUAL_PATH_LCD_ID;
        assert_eq!(protocol_for_id(vid, pid), Some(WireProtocol::Bulk));
        let vendor_pair = DerivedBulkPair {
            interface: 1,
            ep_in: 0x81,
            ep_out: 0x02,
            vendor_class: true,
        };
        assert_eq!(
            vendor_bulk_endpoints(Some(vendor_pair)),
            Some((1, 0x81, 0x02)),
            "vendor interface must select bulk"
        );
        let non_vendor_pair = DerivedBulkPair {
            interface: 1,
            ep_in: 0x81,
            ep_out: 0x02,
            vendor_class: false,
        };
        assert_eq!(
            vendor_bulk_endpoints(Some(non_vendor_pair)),
            None,
            "mass-storage bulk endpoints must defer to SCSI"
        );
        assert_eq!(
            vendor_bulk_endpoints(None),
            None,
            "missing bulk endpoints must defer to SCSI"
        );
        assert_eq!(
            scsi_protocol_for_id(vid, pid, false),
            Some(WireProtocol::Scsi),
            "0416:5406 without a same-device bulk claim must fall back to SCSI"
        );
        assert_eq!(
            scsi_protocol_for_id(vid, pid, true),
            None,
            "0416:5406 with a same-device bulk claim must prefer bulk"
        );

        for (vid, pid) in expected_scsi_only.into_iter().chain([DUAL_PATH_LCD_ID]) {
            let info = crate::transport::build_device_info(
                WireProtocol::Scsi,
                vid,
                pid,
                100,
                0,
                Some(100),
            )
            .unwrap_or_else(|error| {
                panic!("SCSI profile resolution failed for {vid:04x}:{pid:04x}: {error:#}")
            });
            assert_eq!((info.vid, info.pid), (vid, pid));
            assert_eq!(info.protocol, WireProtocol::Scsi);
            assert_eq!((info.width(), info.height()), (320, 320));
            assert_eq!(info.encoding(), crate::transport::FrameEncoding::Rgb565Be);
        }

        let udev_rules = include_str!("../../packaging/udev/99-thermalwriter-rapl.rules");
        let mut udev_scsi_ids = Vec::new();
        for line in udev_rules
            .lines()
            .filter(|line| line.trim_start().starts_with("SUBSYSTEM==\"scsi_generic\""))
        {
            assert!(
                line.contains("TAG+=\"uaccess\""),
                "SCSI udev rule does not grant active-session access: {line}"
            );
            let vid = udev_hex_attr(line, "idVendor")
                .unwrap_or_else(|| panic!("SCSI udev rule lacks a valid vendor: {line}"));
            let pid = udev_hex_attr(line, "idProduct")
                .unwrap_or_else(|| panic!("SCSI udev rule lacks a valid product: {line}"));
            udev_scsi_ids.push((vid, pid));
        }
        udev_scsi_ids.sort_unstable();

        let mut expected_udev_ids = expected_scsi_only.to_vec();
        expected_udev_ids.push(DUAL_PATH_LCD_ID);
        expected_udev_ids.sort_unstable();
        assert_eq!(
            udev_scsi_ids, expected_udev_ids,
            "SCSI discovery and permission matrices drifted"
        );

        for (vid, pid) in expected_scsi_only {
            let usb_rule = udev_rule(udev_rules, "usb", vid, pid);
            let expected_power_attrs: &[&str] = match (vid, pid) {
                (0x87cd, 0x70db) => &[r#"ATTR{power/control}="on""#],
                (0x0402, 0x3922) => &[
                    r#"ATTR{power/control}="auto""#,
                    r#"ATTR{power/autosuspend_delay_ms}="10000""#,
                ],
                _ => unreachable!("unexpected SCSI-only ID"),
            };
            for attr in expected_power_attrs {
                assert!(
                    usb_rule.contains(attr),
                    "SCSI-only USB parent lost power attribute {attr}: {usb_rule}"
                );
            }
            assert!(
                !usb_rule.contains("TAG+=\"uaccess\""),
                "SCSI-only USB parent must not grant direct access: {usb_rule}"
            );
            let sg_rule = udev_rule(udev_rules, "scsi_generic", vid, pid);
            assert!(
                sg_rule.contains("TAG+=\"uaccess\""),
                "SCSI-only sg node must grant active-session access: {sg_rule}"
            );
        }

        let (dual_vid, dual_pid) = DUAL_PATH_LCD_ID;
        let dual_usb_rule = udev_rule(udev_rules, "usb", dual_vid, dual_pid);
        assert!(dual_usb_rule.contains(r#"ATTR{power/control}="auto""#));
        assert!(dual_usb_rule.contains(r#"ATTR{power/autosuspend_delay_ms}="10000""#));
        assert!(dual_usb_rule.contains("TAG+=\"uaccess\""));
        let dual_sg_rule = udev_rule(udev_rules, "scsi_generic", dual_vid, dual_pid);
        assert!(dual_sg_rule.contains("TAG+=\"uaccess\""));
    }

    fn scsi_only_fingerprint_with_unrelated_vendor_bulk() -> UsbFingerprint {
        UsbFingerprint {
            vid: 0x87cd,
            pid: 0x70db,
            bcd_device: "1.00".to_string(),
            interfaces: vec![
                UsbInterfaceShape {
                    number: 0,
                    alternate_setting: 0,
                    class: USB_CLASS_MASS_STORAGE,
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
                },
                UsbInterfaceShape {
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
                            address: 0x82,
                            direction: UsbDirection::In,
                            transfer: UsbTransferKind::Bulk,
                            max_packet_size: 512,
                            interval: 0,
                        },
                    ],
                },
            ],
        }
    }

    #[test]
    fn resolve_known_lcd_route_scsi_only_ignores_unrelated_vendor_bulk() {
        let fingerprint = scsi_only_fingerprint_with_unrelated_vendor_bulk();
        assert!(
            vendor_bulk_endpoints(derive_vendor_bulk_pair(&fingerprint)).is_some(),
            "fixture must include an unrelated vendor bulk pair"
        );
        assert!(
            mass_storage_bulk_pair(&fingerprint).is_some(),
            "fixture must include a mass-storage bulk pair"
        );

        let (protocol, route) =
            resolve_known_lcd_route(0x87cd, 0x70db, &fingerprint).expect("SCSI-only route");
        assert_eq!(protocol, WireProtocol::Scsi);
        assert_eq!(route, LcdTransportRoute::ScsiCommand);
    }

    fn udev_hidraw_ids(rules: &'static str) -> Vec<(u16, u16)> {
        let mut ids = Vec::new();
        for line in rules
            .lines()
            .filter(|line| line.trim_start().starts_with("SUBSYSTEM==\"hidraw\""))
        {
            assert!(
                line.contains("TAG+=\"uaccess\""),
                "hidraw udev rule does not grant active-session access: {line}"
            );
            assert!(
                line.contains("ATTRS{idVendor}"),
                "hidraw rule must use parent-scoped ATTRS, not ATTR: {line}"
            );
            assert!(
                line.contains("ATTRS{idProduct}"),
                "hidraw rule must use parent-scoped ATTRS, not ATTR: {line}"
            );
            assert!(
                !line.contains("MODE="),
                "hidraw rule must not use world-writable MODE: {line}"
            );
            let vid = udev_hex_attr(line, "idVendor")
                .unwrap_or_else(|| panic!("hidraw udev rule lacks a valid vendor: {line}"));
            let pid = udev_hex_attr(line, "idProduct")
                .unwrap_or_else(|| panic!("hidraw udev rule lacks a valid product: {line}"));
            ids.push((vid, pid));
        }
        ids
    }

    #[test]
    fn hid2_udev_hidraw_rule_scoped_to_0416_5302() {
        const HID2_ID: (u16, u16) = (0x0416, 0x5302);

        let udev_rules = include_str!("../../packaging/udev/99-thermalwriter-rapl.rules");
        let hidraw_ids = udev_hidraw_ids(udev_rules);
        assert_eq!(
            hidraw_ids,
            vec![HID2_ID],
            "only 0416:5302 may receive hidraw uaccess"
        );

        let hidraw_rule = udev_rule(udev_rules, "hidraw", HID2_ID.0, HID2_ID.1);
        assert!(hidraw_rule.contains("ATTRS{idVendor}"));
        assert!(hidraw_rule.contains("ATTRS{idProduct}"));
        assert!(hidraw_rule.contains("TAG+=\"uaccess\""));
        assert!(
            !hidraw_rule.contains("MODE="),
            "hidraw rule must not loosen permissions via MODE: {hidraw_rule}"
        );

        let usb_rule = udev_rule(udev_rules, "usb", HID2_ID.0, HID2_ID.1);
        assert!(usb_rule.contains(r#"ATTR{power/control}="on""#));
        assert!(
            !usb_rule.contains(r#"ATTR{power/control}="auto""#),
            "0416:5302 must not autosuspend: {usb_rule}"
        );
        assert!(usb_rule.contains("TAG+=\"uaccess\""));

        let unrelated_usb_ids = [
            (0x87ad, 0x70db),
            (0x87cd, 0x70db),
            (0x0402, 0x3922),
            DUAL_PATH_LCD_ID,
            (0x0418, 0x5303),
            (0x0418, 0x5304),
            (0x0416, 0x5408),
            (0x0416, 0x5409),
        ];
        for (vid, pid) in unrelated_usb_ids {
            assert!(
                !udev_rules.contains(&format!(
                    "SUBSYSTEM==\"hidraw\", ATTRS{{idVendor}}==\"{vid:04x}\", ATTRS{{idProduct}}==\"{pid:04x}\""
                )),
                "{vid:04x}:{pid:04x} must not inherit hidraw uaccess"
            );
        }

        // SCSI matrix from the companion test must still hold.
        let mut udev_scsi_ids = Vec::new();
        for line in udev_rules
            .lines()
            .filter(|line| line.trim_start().starts_with("SUBSYSTEM==\"scsi_generic\""))
        {
            let vid = udev_hex_attr(line, "idVendor").unwrap();
            let pid = udev_hex_attr(line, "idProduct").unwrap();
            udev_scsi_ids.push((vid, pid));
        }
        udev_scsi_ids.sort_unstable();
        let mut expected_scsi_ids = [(0x87cd, 0x70db), (0x0402, 0x3922), DUAL_PATH_LCD_ID].to_vec();
        expected_scsi_ids.sort_unstable();
        assert_eq!(
            udev_scsi_ids, expected_scsi_ids,
            "existing SCSI udev rules must remain intact"
        );
    }

    #[test]
    fn parse_auto_all_and_usb_id() {
        assert_eq!(DeviceSelector::parse("auto").unwrap(), DeviceSelector::Auto);
        assert_eq!(DeviceSelector::parse("ALL").unwrap(), DeviceSelector::All);
        assert_eq!(
            DeviceSelector::parse("87AD:70DB").unwrap(),
            DeviceSelector::UsbId {
                vid: 0x87ad,
                pid: 0x70db
            }
        );
        assert_eq!(
            DeviceSelector::parse("0x0416:0x5408").unwrap(),
            DeviceSelector::UsbId {
                vid: 0x0416,
                pid: 0x5408
            }
        );
        assert!(DeviceSelector::parse("nope").is_err());
    }

    fn sample_device(address: u8) -> DiscoveredDevice {
        DiscoveredDevice {
            vid: 0x87ad,
            pid: 0x70db,
            protocol: WireProtocol::Bulk,
            serial: None,
            path: DevicePath::Usb {
                bus: 1,
                address,
                interface: 0,
                ep_in: 0x81,
                ep_out: 0x01,
            },
        }
    }

    #[test]
    fn select_targets_binds_in_config_order() {
        let a = sample_device(2);
        let mut b = sample_device(3);
        b.vid = 0x0416;
        b.pid = 0x5302;
        let selected = select_target_devices(
            &[b.clone(), a.clone()],
            &[(0x87ad, 0x70db), (0x0416, 0x5302)],
        )
        .expect("targets");
        assert_eq!(selected[0].pid, 0x70db);
        assert_eq!(selected[1].pid, 0x5302);
    }

    #[test]
    fn select_targets_errors_when_missing() {
        let a = sample_device(2);
        let err = select_target_devices(&[a], &[(0x0416, 0x5302)]).unwrap_err();
        assert!(err.to_string().contains("0416:5302"), "{err}");
    }

    #[test]
    fn select_all_returns_every_matching_device_in_order() {
        let devices = vec![sample_device(9), sample_device(2), sample_device(3)];
        let selected = select_all_devices(&devices, &DeviceSelector::All).expect("all selector");
        assert_eq!(selected.len(), 3);
        assert_eq!(selected[0].path, sample_device(2).path);
        assert_eq!(selected[1].path, sample_device(3).path);
        assert_eq!(selected[2].path, sample_device(9).path);
    }

    #[test]
    fn select_all_includes_identical_vid_pid_units() {
        let first = sample_device(2);
        let second = sample_device(3);
        let selected = select_all_devices(&[first.clone(), second.clone()], &DeviceSelector::All)
            .expect("duplicate VID:PID units");
        assert_eq!(selected, vec![first, second]);
        assert!(
            select_devices(&[sample_device(2), sample_device(3)], &DeviceSelector::Auto).is_err()
        );
    }

    #[test]
    fn select_auto_requires_exactly_one() {
        let d = DiscoveredDevice {
            vid: 0x87ad,
            pid: 0x70db,
            protocol: WireProtocol::Bulk,
            serial: None,
            path: DevicePath::Usb {
                bus: 1,
                address: 2,
                interface: 0,
                ep_in: 0x81,
                ep_out: 0x01,
            },
        };
        assert_eq!(
            select_devices(std::slice::from_ref(&d), &DeviceSelector::Auto).unwrap(),
            d
        );
        assert!(select_devices(&[], &DeviceSelector::Auto).is_err());
        let mut d2 = d.clone();
        if let DevicePath::Usb { address, .. } = &mut d2.path {
            *address = 9;
        }
        assert!(select_devices(&[d, d2], &DeviceSelector::Auto).is_err());
    }

    #[test]
    fn fixture_fallback_classification_requires_successful_zero_match_scan() {
        let device = |address| DiscoveredDevice {
            vid: 0x87ad,
            pid: 0x70db,
            protocol: WireProtocol::Bulk,
            serial: None,
            path: DevicePath::Usb {
                bus: 1,
                address,
                interface: 0,
                ep_in: 0x81,
                ep_out: 0x01,
            },
        };

        let scan_error = select_scanned_device(
            Err(anyhow::anyhow!("permission denied")),
            &DeviceSelector::Auto,
        )
        .unwrap_err();
        assert!(matches!(scan_error, HardwareConnectError::Failed(_)));

        let no_devices = select_scanned_device(Ok(Vec::new()), &DeviceSelector::Auto).unwrap_err();
        assert!(matches!(no_devices, HardwareConnectError::NoDevice(_)));

        let no_selector_match = select_scanned_device(
            Ok(vec![device(2)]),
            &DeviceSelector::UsbId {
                vid: 0x0416,
                pid: 0x5408,
            },
        )
        .unwrap_err();
        assert!(matches!(
            no_selector_match,
            HardwareConnectError::NoDevice(_)
        ));

        let ambiguous =
            select_scanned_device(Ok(vec![device(2), device(3)]), &DeviceSelector::Auto)
                .unwrap_err();
        assert!(matches!(ambiguous, HardwareConnectError::Failed(_)));

        let selected = select_scanned_device(Ok(vec![device(2)]), &DeviceSelector::Auto).unwrap();
        assert_eq!(selected, device(2));

        let open_error = connect_scanned_with(
            Ok(vec![device(2)]),
            &DeviceSelector::Auto,
            |_| -> Result<()> { Err(anyhow::anyhow!("permission/open/handshake failure")) },
        )
        .unwrap_err();
        assert!(matches!(open_error, HardwareConnectError::Failed(_)));
        let propagated = hardware_or_no_device::<()>(Err(open_error)).unwrap_err();
        assert!(
            propagated
                .to_string()
                .contains("permission/open/handshake failure"),
            "{propagated:#}"
        );

        let fallback = hardware_or_no_device::<()>(Err(HardwareConnectError::NoDevice(
            anyhow::anyhow!("no matching device"),
        )))
        .unwrap();
        assert!(fallback.is_none(), "only NoDevice should permit fallback");
    }

    #[test]
    fn detected_scsi_usb_device_requires_resolved_generic_node() {
        let candidate = ScsiUsbCandidate {
            vid: 0x87cd,
            pid: 0x70db,
            bus: 1,
            address: 2,
        };
        let error = ensure_scsi_candidates_resolved(&[candidate], &[]).unwrap_err();
        assert!(
            error.to_string().contains("no usable scsi_generic"),
            "{error:#}"
        );

        let resolved = DiscoveredDevice {
            vid: candidate.vid,
            pid: candidate.pid,
            protocol: WireProtocol::Scsi,
            serial: None,
            path: DevicePath::Scsi {
                devnode: PathBuf::from("/dev/sg0"),
                sysfs_device: PathBuf::from("/sys/class/scsi_generic/sg0"),
                usb_bus: Some(candidate.bus),
                usb_address: Some(candidate.address),
            },
        };
        ensure_scsi_candidates_resolved(&[candidate], &[resolved])
            .expect("matching scsi_generic node should resolve candidate");
    }

    #[test]
    fn dual_shape_dedup_uses_physical_usb_identity_not_vid_pid() {
        let first = DiscoveredDevice {
            vid: 0x0416,
            pid: 0x5406,
            protocol: WireProtocol::Bulk,
            serial: None,
            path: DevicePath::Usb {
                bus: 1,
                address: 2,
                interface: 0,
                ep_in: 0x81,
                ep_out: 0x01,
            },
        };
        let same = UsbAncestor {
            vid: 0x0416,
            pid: 0x5406,
            bus: Some(1),
            address: Some(2),
        };
        let second = UsbAncestor {
            address: Some(3),
            ..same
        };
        let unknown_path = UsbAncestor {
            bus: None,
            address: None,
            ..same
        };

        assert!(belongs_to_usb_ancestor(&first, same));
        assert!(!belongs_to_usb_ancestor(&first, second));
        assert!(!belongs_to_usb_ancestor(&first, unknown_path));
    }

    fn hid2_interrupt_only_fingerprint() -> UsbFingerprint {
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

    fn bulk_peer_fingerprint() -> UsbFingerprint {
        UsbFingerprint {
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
        }
    }

    #[test]
    fn usb_bulk_discovery_includes_hid2_interrupt_shape() {
        let fp = hid2_interrupt_only_fingerprint();
        // IN-only HID Type2 is still discovered (ep_out=0 → SET_REPORT / default).
        assert_eq!(
            usb_bulk_discovery_outcome(WireProtocol::HidType2, &fp),
            UsbBulkDiscoveryOutcome::Endpoints(0, 0x81, 0)
        );
    }

    #[test]
    fn usb_bulk_discovery_returns_bulk_endpoints_for_peer_display() {
        let fp = bulk_peer_fingerprint();
        assert_eq!(
            usb_bulk_discovery_outcome(WireProtocol::Bulk, &fp),
            UsbBulkDiscoveryOutcome::Endpoints(1, 0x81, 0x02)
        );
    }

    #[test]
    fn discovered_bulk_from_fingerprint_builds_usb_path_without_scan() {
        let fp = bulk_peer_fingerprint();
        let device =
            discovered_bulk_from_fingerprint(0x87ad, 0x70db, 3, 21, &fp).expect("bulk device");
        assert_eq!(device.protocol, WireProtocol::Bulk);
        assert_eq!(
            device.path,
            DevicePath::Usb {
                bus: 3,
                address: 21,
                interface: 1,
                ep_in: 0x81,
                ep_out: 0x02,
            }
        );
    }
}
