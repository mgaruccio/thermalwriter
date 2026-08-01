// SPDX-License-Identifier: GPL-3.0-or-later
//
// Injectable fakes and workflow entry points for integration tests (no hardware).

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::service::guard::{DeviceOwnership, OwnershipTarget, ServiceControl};
use crate::transport::hid_report::{
    HidReportIo, HidReportReadSession, HidrawCandidate, PROTOCOL_CHUNK_BYTES, SysfsAccess,
    UsbBusAddress,
};
use crate::transport::usb_fingerprint::{
    UsbDirection, UsbEndpointCapability, UsbFingerprint, UsbInterfaceShape, UsbRunIdentity,
    UsbTransferKind,
};

pub use super::active::{
    run_active_validation_with, resolve_reconnect, snapshot_peers, ActiveOptions, PeerIdentity,
    ScriptedPrompt,
};
pub use super::{
    run_passive_preflight, run_validate_device_with, PassiveOutcome, PassivePreflightContext,
    PreflightResult, ValidateDeviceArgs, HidrawInventory, InventoryEntry, UsbInventory,
};

/// USB inventory backed by an in-memory device list.
pub struct MapInventory {
    pub entries: Vec<InventoryEntry>,
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

/// Hidraw inventory backed by an in-memory candidate list.
pub struct MapHidrawInventory {
    pub candidates: Vec<HidrawCandidate>,
}

impl HidrawInventory for MapHidrawInventory {
    fn list_hidraw_candidates(&self) -> anyhow::Result<Vec<HidrawCandidate>> {
        Ok(self.candidates.clone())
    }
}

/// Sysfs view backed by in-memory files and canonical path links.
#[derive(Default)]
pub struct MapSysfs {
    pub files: BTreeMap<PathBuf, String>,
    pub canonical: BTreeMap<PathBuf, PathBuf>,
}

impl MapSysfs {
    pub fn insert_file(mut self, path: impl Into<PathBuf>, contents: impl Into<String>) -> Self {
        self.files.insert(path.into(), contents.into());
        self
    }

    pub fn link(mut self, from: impl Into<PathBuf>, to: impl Into<PathBuf>) -> Self {
        self.canonical.insert(from.into(), to.into());
        self
    }
}

impl SysfsAccess for MapSysfs {
    fn canonicalize(&self, path: &Path) -> anyhow::Result<PathBuf> {
        self.canonical
            .get(path)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no canonical mapping for {}", path.display()))
    }

    fn read_trimmed(&self, path: &Path) -> anyhow::Result<String> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing {}", path.display()))
    }

    fn exists(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }
}

/// Inventory that returns different entries on successive scans (reconnect tests).
pub struct CountingInventory {
    pub first: Vec<InventoryEntry>,
    pub later: Vec<InventoryEntry>,
    calls: RefCell<usize>,
}

impl CountingInventory {
    pub fn new(first: Vec<InventoryEntry>, later: Vec<InventoryEntry>) -> Self {
        Self {
            first,
            later,
            calls: RefCell::new(0),
        }
    }

    pub fn inventory_calls(&self) -> usize {
        *self.calls.borrow()
    }
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

/// Scripted systemd user-service control for validator guard tests.
pub struct FakeControl {
    pub active: bool,
    pub stop_fail: bool,
    pub start_fail: bool,
    pub calls: Rc<RefCell<Vec<&'static str>>>,
}

impl Default for FakeControl {
    fn default() -> Self {
        Self {
            active: false,
            stop_fail: false,
            start_fail: false,
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
        if self.stop_fail {
            Err(anyhow::anyhow!("stop failed"))
        } else {
            Ok(())
        }
    }

    fn start(&self, _unit: &str) -> anyhow::Result<()> {
        self.calls.borrow_mut().push("start");
        if self.start_fail {
            Err(anyhow::anyhow!("start failed"))
        } else {
            Ok(())
        }
    }
}

/// Scripted concurrent device ownership probe.
#[derive(Default)]
pub struct FakeOwnership {
    pub owned: bool,
}

impl DeviceOwnership for FakeOwnership {
    fn is_concurrently_owned(&self, _target: &OwnershipTarget) -> anyhow::Result<bool> {
        Ok(self.owned)
    }
}

/// In-memory HID report I/O with scripted read/write returns.
pub struct FakeHidIo {
    pub read_data: VecDeque<Vec<u8>>,
    pub read_returns: VecDeque<anyhow::Result<isize>>,
    pub writes: Arc<Mutex<Vec<Vec<u8>>>>,
    pub write_returns: VecDeque<anyhow::Result<isize>>,
}

impl FakeHidIo {
    pub fn with_probe(response: &[u8]) -> Self {
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

    pub fn write_count(&self) -> usize {
        self.writes.lock().unwrap().len()
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

pub fn endpoint(
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

pub fn iface(number: u8, class: u8, endpoints: Vec<UsbEndpointCapability>) -> UsbInterfaceShape {
    UsbInterfaceShape {
        number,
        alternate_setting: 0,
        class,
        subclass: 0,
        protocol: 0,
        endpoints,
    }
}

pub fn inventory_entry(
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

/// Winbond HID Type2 descriptor with interrupt-IN only (no interrupt OUT).
pub fn hid_in_fingerprint() -> UsbFingerprint {
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

pub fn bulk_fingerprint() -> UsbFingerprint {
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

pub fn pm58_short_response() -> Vec<u8> {
    vec![0xDA, 0xDB, 0xDC, 0xDD, 0x00, 0x3A, 0x00, 0x00]
}

pub fn pm68_short_response() -> Vec<u8> {
    let mut response = pm58_short_response();
    response[5] = 0x44;
    response
}

pub fn correlated_hidraw_fs(bus: u8, address: u8) -> MapSysfs {
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

pub fn mismatched_hidraw_fs() -> MapSysfs {
    MapSysfs::default()
        .link(
            "/sys/class/hidraw/hidraw1/device",
            "/sys/devices/pci0/usb1/1-1/1-1:1.0",
        )
        .insert_file("/sys/devices/pci0/usb1/1-1/1-1:1.0/busnum", "9")
        .insert_file("/sys/devices/pci0/usb1/1-1/1-1:1.0/devnum", "3")
}

pub fn active_fixture() -> (MapInventory, MapHidrawInventory, MapSysfs) {
    (
        MapInventory {
            entries: vec![inventory_entry(1, 14, hid_in_fingerprint(), false)],
        },
        MapHidrawInventory {
            candidates: vec![
                HidrawCandidate::from_sysfs_class_entry(PathBuf::from("/sys/class/hidraw/hidraw3"))
                    .unwrap(),
            ],
        },
        correlated_hidraw_fs(1, 14),
    )
}

pub fn open_pm58_session(
    _selector: UsbBusAddress,
    _correlation: &crate::transport::hid_report::HidrawCorrelation,
) -> anyhow::Result<HidReportReadSession<FakeHidIo>> {
    Ok(HidReportReadSession::from_io(FakeHidIo::with_probe(
        &pm58_short_response(),
    )))
}

pub fn open_pm68_session(
    _selector: UsbBusAddress,
    _correlation: &crate::transport::hid_report::HidrawCorrelation,
) -> anyhow::Result<HidReportReadSession<FakeHidIo>> {
    Ok(HidReportReadSession::from_io(FakeHidIo::with_probe(
        &pm68_short_response(),
    )))
}

pub fn fixed_timestamp() -> String {
    "fixed-ts".to_string()
}
