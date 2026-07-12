// SPDX-License-Identifier: GPL-3.0-or-later
//
// Deterministic multi-family device discovery and TransportConnector.
// Device IDs and recognition rules derived from thermalright-trcc-linux
// at tree 390b880abd4cf0ed2d6eae7151493432263eff39 (project version 9.8.6, four commits after the v9.8.6 tag).

//! Scan for Thermalright full-pixel LCDs and connect the selected device.

use anyhow::{Context, Result, bail};
use log::{info, warn};
use std::fmt;
use std::path::{Path, PathBuf};

use super::Transport;
use super::bulk_usb::BulkUsb;
use super::hid_lcd::HidLcd;
use super::ly_lcd::LyLcd;
use super::null::{NullTransport, TransportKind, transport_from_env};
use super::profile::{DeviceInfo, WireProtocol, device_info_from_fixture, fixture_by_id};
use super::scsi_lcd::ScsiLcd;

/// How the user selects which LCD to open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DeviceSelector {
    /// Exactly one physical LCD must be present.
    #[default]
    Auto,
    /// Open the unique device with this USB id.
    UsbId { vid: u16, pid: u16 },
}

impl DeviceSelector {
    /// Parse `auto` or `VID:PID` (hex, optional `0x` prefix).
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("auto") {
            return Ok(Self::Auto);
        }
        let (vid_s, pid_s) = s.split_once(':').ok_or_else(|| {
            anyhow::anyhow!("device selector must be 'auto' or 'VID:PID', got {s:?}")
        })?;
        let vid = parse_hex_u16(vid_s).with_context(|| format!("invalid VID in {s:?}"))?;
        let pid = parse_hex_u16(pid_s).with_context(|| format!("invalid PID in {s:?}"))?;
        Ok(Self::UsbId { vid, pid })
    }
}

impl fmt::Display for DeviceSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
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

fn protocol_for_id(vid: u16, pid: u16) -> Option<WireProtocol> {
    KNOWN_LCD_IDS
        .iter()
        .find(|(v, p, _)| *v == vid && *p == pid)
        .map(|(_, _, proto)| *proto)
}

/// Scan libusb + scsi_generic for full-pixel LCDs.
pub fn scan_devices() -> Result<Vec<DiscoveredDevice>> {
    let mut devices = Vec::new();
    scan_usb(&mut devices)?;
    scan_scsi(&mut devices)?;
    Ok(devices)
}

fn scan_usb(out: &mut Vec<DiscoveredDevice>) -> Result<()> {
    let list = match rusb::devices() {
        Ok(l) => l,
        Err(e) => {
            warn!("libusb device list failed: {e}");
            return Ok(());
        }
    };
    for device in list.iter() {
        let desc = match device.device_descriptor() {
            Ok(d) => d,
            Err(_) => continue,
        };
        let vid = desc.vendor_id();
        let pid = desc.product_id();
        let Some(mut protocol) = protocol_for_id(vid, pid) else {
            continue;
        };

        let bus = device.bus_number();
        let address = device.address();

        // 0416:5406 — prefer vendor bulk endpoints; else leave for SCSI scan.
        if vid == 0x0416 && pid == 0x5406 {
            match find_bulk_endpoints(&device) {
                Ok(Some((iface, ep_in, ep_out))) => {
                    out.push(DiscoveredDevice {
                        vid,
                        pid,
                        protocol: WireProtocol::Bulk,
                        serial: None,
                        path: DevicePath::Usb {
                            bus,
                            address,
                            interface: iface,
                            ep_in,
                            ep_out,
                        },
                    });
                }
                Ok(None) => {
                    // No vendor bulk pair — SCSI path may claim it.
                }
                Err(e) => warn!("0416:5406 endpoint probe failed: {e:#}"),
            }
            continue;
        }

        // SCSI IDs are claimed via /dev/sg* (scan_scsi), not raw bulk.
        if matches!(protocol, WireProtocol::Scsi) {
            continue;
        }

        match find_bulk_endpoints(&device) {
            Ok(Some((iface, ep_in, ep_out))) => {
                // LY/HID claim interface 0 with descriptor endpoints.
                if matches!(protocol, WireProtocol::HidType2 | WireProtocol::HidType3) {
                    // HID uses fixed OUT02/IN81 when present; still record descriptor pair.
                }
                if matches!(protocol, WireProtocol::Ly) {
                    protocol = WireProtocol::Ly;
                }
                out.push(DiscoveredDevice {
                    vid,
                    pid,
                    protocol,
                    serial: None,
                    path: DevicePath::Usb {
                        bus,
                        address,
                        interface: iface,
                        ep_in,
                        ep_out,
                    },
                });
            }
            Ok(None) => {
                warn!(
                    "device {:04x}:{:04x} bus={} addr={} has no bulk endpoints",
                    vid, pid, bus, address
                );
            }
            Err(e) => warn!("failed to read config for {:04x}:{:04x}: {e:#}", vid, pid),
        }
    }
    Ok(())
}

fn find_bulk_endpoints<T: rusb::UsbContext>(
    device: &rusb::Device<T>,
) -> Result<Option<(u8, u8, u8)>> {
    let config = device
        .active_config_descriptor()
        .context("active config descriptor")?;
    let mut best: Option<(u8, u8, u8, bool)> = None; // iface, in, out, vendor
    for iface in config.interfaces() {
        for desc in iface.descriptors() {
            let mut ep_in = 0u8;
            let mut ep_out = 0u8;
            for ep in desc.endpoint_descriptors() {
                if ep.transfer_type() == rusb::TransferType::Bulk {
                    if ep.direction() == rusb::Direction::Out {
                        ep_out = ep.address();
                    } else {
                        ep_in = ep.address();
                    }
                }
            }
            if ep_in != 0 && ep_out != 0 {
                let vendor = desc.class_code() == 255 || desc.class_code() == 0;
                let candidate = (desc.interface_number(), ep_in, ep_out, vendor);
                match best {
                    None => best = Some(candidate),
                    Some((_, _, _, was_vendor)) if vendor && !was_vendor => best = Some(candidate),
                    _ => {}
                }
            }
        }
    }
    Ok(best.map(|(i, inn, out, _)| (i, inn, out)))
}

fn scan_scsi(out: &mut Vec<DiscoveredDevice>) -> Result<()> {
    let sg_root = Path::new("/sys/class/scsi_generic");
    if !sg_root.exists() {
        return Ok(());
    }
    let entries = match std::fs::read_dir(sg_root) {
        Ok(e) => e,
        Err(e) => {
            warn!("failed to read {}: {e}", sg_root.display());
            return Ok(());
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("sg") {
            continue;
        }
        let sysfs = entry.path();
        let devnode = PathBuf::from(format!("/dev/{name}"));
        if !devnode.exists() {
            continue;
        }
        let Some(ancestor) = resolve_usb_ancestor(&sysfs) else {
            continue;
        };
        let (vid, pid) = (ancestor.vid, ancestor.pid);
        let protocol = match (vid, pid) {
            (0x87cd, 0x70db) | (0x0402, 0x3922) => WireProtocol::Scsi,
            (0x0416, 0x5406) => {
                // Prefer the bulk interface only when it belongs to this same
                // physical USB device. Identical coolers remain distinct.
                let same_physical_device = out
                    .iter()
                    .any(|device| belongs_to_usb_ancestor(device, ancestor));
                if same_physical_device {
                    continue;
                }
                WireProtocol::Scsi
            }
            _ => continue,
        };
        out.push(DiscoveredDevice {
            vid,
            pid,
            protocol,
            serial: None,
            path: DevicePath::Scsi {
                devnode,
                sysfs_device: sysfs,
                usb_bus: ancestor.bus,
                usb_address: ancestor.address,
            },
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct UsbAncestor {
    vid: u16,
    pid: u16,
    bus: Option<u8>,
    address: Option<u8>,
}

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

/// Select devices matching `selector`. Errors on zero or ambiguous matches.
pub fn select_devices(
    devices: &[DiscoveredDevice],
    selector: &DeviceSelector,
) -> Result<DiscoveredDevice> {
    let matched: Vec<&DiscoveredDevice> = match selector {
        DeviceSelector::Auto => devices.iter().collect(),
        DeviceSelector::UsbId { vid, pid } => devices
            .iter()
            .filter(|d| d.vid == *vid && d.pid == *pid)
            .collect(),
    };

    match matched.as_slice() {
        [] => {
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
                "no matching LCD for selector '{selector}' (available: {available}). \
                 Check udev rules, cable access, and that the cooler is plugged in."
            );
        }
        [one] => Ok((*one).clone()),
        many => {
            let list = many
                .iter()
                .map(|d| d.identity())
                .collect::<Vec<_>>()
                .join("; ");
            bail!(
                "ambiguous LCD selection for '{selector}': found {} devices: {list}. \
                 Set display.device to a specific VID:PID or unplug extras.",
                many.len()
            );
        }
    }
}

/// Opens, handshakes, and returns a negotiated transport pair.
pub struct TransportConnector {
    pub selector: DeviceSelector,
}

impl TransportConnector {
    pub fn new(selector: DeviceSelector) -> Self {
        Self { selector }
    }

    pub fn from_config_device(device: &str) -> Result<Self> {
        Ok(Self::new(DeviceSelector::parse(device)?))
    }

    /// Discover + construct + handshake. Honors null transport and fixture env.
    ///
    /// `THERMALWRITER_PROFILE` is resolved first (immutable after process start).
    /// With `THERMALWRITER_TRANSPORT=null` (or profile without hardware), the
    /// fixture's `DeviceInfo` is passed to `NullTransport::with_profile`.
    pub fn connect(&self) -> Result<(Box<dyn Transport>, DeviceInfo)> {
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
            let info = if let Some(id) = &profile_id {
                device_info_from_fixture(id)?
            } else {
                device_info_from_fixture("bulk-87ad-70db-pm4-sub5-fbl72")?
            };
            let mut t = NullTransport::with_profile(info.clone());
            let _ = t.handshake()?;
            if let Some(id) = &profile_id {
                info!("Null transport with THERMALWRITER_PROFILE={id}");
            } else {
                info!("Null transport with default bulk 480x480 fixture");
            }
            return Ok((Box::new(t), info));
        }

        if let Some(id) = &profile_id {
            // Prefer real hardware when present; otherwise synthetic fixture.
            match self.connect_hardware() {
                Ok(pair) => return Ok(pair),
                Err(_) => {
                    let info = device_info_from_fixture(id)?;
                    let mut t = NullTransport::with_profile(info.clone());
                    let _ = t.handshake()?;
                    info!("THERMALWRITER_PROFILE={id}: using fixture profile without hardware");
                    return Ok((Box::new(t), info));
                }
            }
        }

        self.connect_hardware()
    }

    fn connect_hardware(&self) -> Result<(Box<dyn Transport>, DeviceInfo)> {
        let devices = scan_devices().context("device scan failed")?;
        let selected = select_devices(&devices, &self.selector)?;
        info!("Selected device {}", selected.identity());
        open_discovered(&selected)
    }
}

fn open_discovered(dev: &DiscoveredDevice) -> Result<(Box<dyn Transport>, DeviceInfo)> {
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

    #[test]
    fn parse_auto_and_usb_id() {
        assert_eq!(DeviceSelector::parse("auto").unwrap(), DeviceSelector::Auto);
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
}
