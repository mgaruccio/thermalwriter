// SPDX-License-Identifier: GPL-3.0-or-later
//
// Linux HID report transport: sysfs-correlated hidraw/HIDAPI I/O with pinned
// length contracts. HIDAPI may route output through interrupt OUT or control
// SET_REPORT on EP0; descriptor-level interrupt OUT is not required.
//
// Pinned HIDAPI evidence: libusb/hidapi commit
// 518fbd18796b0ef376f47796d1ee8dd63cc9315a — Linux hidraw and libusb backends
// return the caller buffer length (513) for a successful `[0x00] + 512` write.

use anyhow::{Context, Result, bail, ensure};
use std::path::{Path, PathBuf};

/// HIDAPI source commit pinned for Linux return-count contract evidence.
pub const HIDAPI_EVIDENCE_COMMIT: &str = "518fbd18796b0ef376f47796d1ee8dd63cc9315a";

/// Protocol payload per HID report chunk (upstream Type 2 fixed chunk size).
pub const PROTOCOL_CHUNK_BYTES: usize = 512;

/// Report ID byte for devices with a single unnumbered output report.
pub const REPORT_ID_UNNUMBERED: u8 = 0;

/// Userspace buffer length: one report-ID byte plus one protocol chunk.
pub const USERSPACE_SUBMIT_BYTES: usize = 1 + PROTOCOL_CHUNK_BYTES;

/// Expected HIDAPI `write` return count for a full 513-byte userspace buffer.
pub const EXPECTED_TRANSPORT_RETURN_BYTES: usize = USERSPACE_SUBMIT_BYTES;

/// Recorded backend/version contract for shareable validation reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidReportBackendContract {
    pub hidapi_commit: &'static str,
    pub backend: &'static str,
    pub expected_return_bytes: usize,
}

/// Current Linux hidraw backend contract (statically linked via `linux-static-hidraw`).
pub const LINUX_HIDRAW_BACKEND_CONTRACT: HidReportBackendContract = HidReportBackendContract {
    hidapi_commit: HIDAPI_EVIDENCE_COMMIT,
    backend: "linux-static-hidraw",
    expected_return_bytes: EXPECTED_TRANSPORT_RETURN_BYTES,
};

/// Independent length observations for one HID output report write.
///
/// Endpoint `wMaxPacketSize` is an unrelated USB packet fact and must never be
/// conflated with protocol chunk or userspace buffer lengths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidWriteObservation {
    pub protocol_chunk_bytes: usize,
    pub logical_output_report_bytes: Option<usize>,
    pub report_id: u8,
    pub userspace_submit_bytes: usize,
    pub transport_return_bytes: isize,
    pub endpoint_max_packet_size: Option<u16>,
}

/// Independent length observations for one HID input report read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidReadObservation {
    pub read_capacity_bytes: usize,
    pub transport_return_bytes: isize,
    pub protocol_response_bytes: usize,
}

/// Selected USB bus/address used to correlate exactly one hidraw node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbBusAddress {
    pub bus: u8,
    pub address: u8,
}

/// One hidraw sysfs entry discovered under `/sys/class/hidraw/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidrawCandidate {
    pub name: String,
    pub sysfs_path: PathBuf,
}

/// Result of correlating hidraw candidates to a selected USB identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidrawCorrelation {
    pub selected: HidrawCandidate,
    pub devnode: PathBuf,
}

/// Injectable sysfs access for correlation tests.
pub trait SysfsAccess {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf>;
    fn read_trimmed(&self, path: &Path) -> Result<String>;
    fn exists(&self, path: &Path) -> bool;
}

/// Production sysfs reader.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealSysfs;

impl SysfsAccess for RealSysfs {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf> {
        std::fs::canonicalize(path).with_context(|| format!("canonicalize {}", path.display()))
    }

    fn read_trimmed(&self, path: &Path) -> Result<String> {
        let value =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        Ok(value.trim().to_string())
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolvedUsbBusAddress {
    bus: u8,
    address: u8,
}

fn resolve_usb_bus_address_from_hidraw_sysfs(
    hidraw_sysfs: &Path,
    fs: &dyn SysfsAccess,
) -> Option<ResolvedUsbBusAddress> {
    let device_link = hidraw_sysfs.join("device");
    let mut current = fs.canonicalize(&device_link).ok()?;
    for _ in 0..12 {
        let bus_path = current.join("busnum");
        let addr_path = current.join("devnum");
        if fs.exists(&bus_path) && fs.exists(&addr_path) {
            let bus = fs.read_trimmed(&bus_path).ok()?.parse().ok()?;
            let address = fs.read_trimmed(&addr_path).ok()?.parse().ok()?;
            return Some(ResolvedUsbBusAddress { bus, address });
        }
        current = current.parent()?.to_path_buf();
    }
    None
}

/// Correlate hidraw candidates to exactly one node whose USB ancestor matches
/// `selector`. Returns an error when zero or multiple nodes match.
pub fn correlate_hidraw_to_usb(
    selector: UsbBusAddress,
    candidates: &[HidrawCandidate],
    fs: &dyn SysfsAccess,
) -> Result<HidrawCorrelation> {
    let mut matches = Vec::new();
    for candidate in candidates {
        let Some(resolved) = resolve_usb_bus_address_from_hidraw_sysfs(&candidate.sysfs_path, fs)
        else {
            continue;
        };
        if resolved.bus == selector.bus && resolved.address == selector.address {
            matches.push(candidate.clone());
        }
    }

    match matches.len() {
        0 => bail!(
            "no hidraw node correlates to USB bus={} address={}",
            selector.bus,
            selector.address
        ),
        1 => {
            let selected = matches.remove(0);
            Ok(HidrawCorrelation {
                devnode: PathBuf::from(format!("/dev/{}", selected.name)),
                selected,
            })
        }
        count => bail!(
            "ambiguous hidraw correlation for USB bus={} address={}: {count} matches",
            selector.bus,
            selector.address
        ),
    }
}

/// Injectable HID report I/O (HIDAPI/hidraw semantics without hardware).
pub trait HidReportIo: Send {
    fn write(&mut self, data: &[u8]) -> Result<isize>;
    fn read(&mut self, buf: &mut [u8]) -> Result<isize>;
}

/// One open HID report session. Errors stop further I/O; no automatic reopen.
pub struct HidReportSession<Io: HidReportIo> {
    io: Io,
    contract: HidReportBackendContract,
    stopped: bool,
}

impl<Io: HidReportIo> HidReportSession<Io> {
    pub fn new(io: Io, contract: HidReportBackendContract) -> Self {
        Self {
            io,
            contract,
            stopped: false,
        }
    }

    pub fn contract(&self) -> &HidReportBackendContract {
        &self.contract
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    fn stop_on_error(&mut self) {
        self.stopped = true;
    }

    /// Write one 512-byte protocol chunk as `[report_id] + chunk` with pinned return contract.
    pub fn write_protocol_chunk(
        &mut self,
        chunk: &[u8],
        logical_output_report_bytes: Option<usize>,
        endpoint_max_packet_size: Option<u16>,
    ) -> Result<HidWriteObservation> {
        if self.stopped {
            bail!("HID report session stopped after prior error");
        }

        ensure!(
            chunk.len() == PROTOCOL_CHUNK_BYTES,
            "expected one {PROTOCOL_CHUNK_BYTES}-byte protocol chunk, got {}",
            chunk.len()
        );

        let mut api_buffer = [0_u8; USERSPACE_SUBMIT_BYTES];
        api_buffer[0] = REPORT_ID_UNNUMBERED;
        api_buffer[1..].copy_from_slice(chunk);

        let returned = match self.io.write(&api_buffer) {
            Ok(value) => value,
            Err(error) => {
                self.stop_on_error();
                return Err(error.context("HID report write failed"));
            }
        };

        let observation = HidWriteObservation {
            protocol_chunk_bytes: PROTOCOL_CHUNK_BYTES,
            logical_output_report_bytes,
            report_id: REPORT_ID_UNNUMBERED,
            userspace_submit_bytes: api_buffer.len(),
            transport_return_bytes: returned,
            endpoint_max_packet_size,
        };

        if returned < 0 {
            self.stop_on_error();
            bail!("HID report write returned error ({returned})");
        }

        if returned as usize != self.contract.expected_return_bytes {
            self.stop_on_error();
            bail!(
                "unexpected HIDAPI write count: submitted={} returned={returned} (expected {})",
                api_buffer.len(),
                self.contract.expected_return_bytes
            );
        }

        Ok(observation)
    }

    /// Write a multi-chunk payload as sequential 512-byte HID report chunks.
    pub fn write_chunked(
        &mut self,
        payload: &[u8],
        logical_output_report_bytes: Option<usize>,
        endpoint_max_packet_size: Option<u16>,
    ) -> Result<Vec<HidWriteObservation>> {
        if payload.is_empty() {
            bail!("HID report chunked write requires non-empty payload");
        }
        let mut observations = Vec::new();
        for chunk in payload.chunks(PROTOCOL_CHUNK_BYTES) {
            if chunk.len() != PROTOCOL_CHUNK_BYTES {
                bail!(
                    "HID report payload length {} is not a multiple of {PROTOCOL_CHUNK_BYTES}",
                    payload.len()
                );
            }
            observations.push(self.write_protocol_chunk(
                chunk,
                logical_output_report_bytes,
                endpoint_max_packet_size,
            )?);
        }
        Ok(observations)
    }

    /// Read up to `capacity` bytes; record capacity, transport return, and protocol length separately.
    pub fn read_bounded(&mut self, capacity: usize) -> Result<(HidReadObservation, Vec<u8>)> {
        if self.stopped {
            bail!("HID report session stopped after prior error");
        }
        if capacity == 0 {
            bail!("HID report read capacity must be > 0");
        }

        let mut buf = vec![0_u8; capacity];
        let returned = match self.io.read(&mut buf) {
            Ok(value) => value,
            Err(error) => {
                self.stop_on_error();
                return Err(error.context("HID report read failed"));
            }
        };

        if returned < 0 {
            self.stop_on_error();
            bail!("HID report read returned error ({returned})");
        }

        let returned_usize = returned as usize;
        buf.truncate(returned_usize);
        let observation = HidReadObservation {
            read_capacity_bytes: capacity,
            transport_return_bytes: returned,
            protocol_response_bytes: returned_usize,
        };
        Ok((observation, buf))
    }
}

#[cfg(feature = "daemon")]
pub mod linux {
    use super::*;
    use anyhow::Context;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    pub struct LinuxHidReportIo {
        device: hidapi::HidDevice,
    }

    impl LinuxHidReportIo {
        pub fn open_path(devnode: &Path) -> Result<Self> {
            let api = hidapi::HidApi::new().context("initialize HIDAPI")?;
            let path = CString::new(devnode.as_os_str().as_bytes())
                .context("hidraw devnode path contains interior NUL")?;
            let device = api
                .open_path(&path)
                .with_context(|| format!("open HID device {}", devnode.display()))?;
            Ok(Self { device })
        }
    }

    impl HidReportIo for LinuxHidReportIo {
        fn write(&mut self, data: &[u8]) -> Result<isize> {
            let count = self.device.write(data).context("hidapi write")?;
            isize::try_from(count).context("hidapi write count overflow")
        }

        fn read(&mut self, buf: &mut [u8]) -> Result<isize> {
            let count = self.device.read(buf).context("hidapi read")?;
            isize::try_from(count).context("hidapi read count overflow")
        }
    }

    /// Open a correlated hidraw devnode for the selected USB bus/address.
    pub fn open_correlated_session(
        selector: UsbBusAddress,
        candidates: &[HidrawCandidate],
        contract: HidReportBackendContract,
    ) -> Result<HidReportSession<LinuxHidReportIo>> {
        let correlation = correlate_hidraw_to_usb(selector, candidates, &RealSysfs)?;
        let io = LinuxHidReportIo::open_path(&correlation.devnode)?;
        Ok(HidReportSession::new(io, contract))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Debug, Default)]
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

    struct MemHidReportIo {
        write_returns: std::collections::VecDeque<Result<isize>>,
        read_returns: std::collections::VecDeque<Result<isize>>,
        read_data: std::collections::VecDeque<Vec<u8>>,
        writes: Vec<Vec<u8>>,
        reads: Vec<usize>,
        fail_after_write: bool,
    }

    impl MemHidReportIo {
        fn new() -> Self {
            Self {
                write_returns: std::collections::VecDeque::new(),
                read_returns: std::collections::VecDeque::new(),
                read_data: std::collections::VecDeque::new(),
                writes: Vec::new(),
                reads: Vec::new(),
                fail_after_write: false,
            }
        }

        fn with_write_ok(returned: isize) -> Self {
            let mut io = Self::new();
            io.write_returns.push_back(Ok(returned));
            io
        }
    }

    impl HidReportIo for MemHidReportIo {
        fn write(&mut self, data: &[u8]) -> Result<isize> {
            self.writes.push(data.to_vec());
            if self.fail_after_write {
                return Err(anyhow::anyhow!("injected write transport error"));
            }
            self.write_returns
                .pop_front()
                .unwrap_or(Ok(data.len() as isize))
        }

        fn read(&mut self, buf: &mut [u8]) -> Result<isize> {
            self.reads.push(buf.len());
            let data = self
                .read_data
                .pop_front()
                .unwrap_or_else(|| vec![0; buf.len()]);
            let len = data.len().min(buf.len());
            buf[..len].copy_from_slice(&data[..len]);
            self.read_returns.pop_front().unwrap_or(Ok(len as isize))
        }
    }

    fn sample_chunk(byte: u8) -> [u8; PROTOCOL_CHUNK_BYTES] {
        [byte; PROTOCOL_CHUNK_BYTES]
    }

    #[test]
    fn length_constants_are_distinct() {
        assert_eq!(PROTOCOL_CHUNK_BYTES, 512);
        assert_eq!(USERSPACE_SUBMIT_BYTES, 513);
        assert_eq!(EXPECTED_TRANSPORT_RETURN_BYTES, 513);
        assert_ne!(PROTOCOL_CHUNK_BYTES, USERSPACE_SUBMIT_BYTES);
    }

    #[test]
    fn write_observation_fields_are_independent() {
        let obs = HidWriteObservation {
            protocol_chunk_bytes: 512,
            logical_output_report_bytes: Some(512),
            report_id: 0,
            userspace_submit_bytes: 513,
            transport_return_bytes: 513,
            endpoint_max_packet_size: Some(8),
        };
        assert_eq!(obs.protocol_chunk_bytes, 512);
        assert_eq!(obs.logical_output_report_bytes, Some(512));
        assert_eq!(obs.userspace_submit_bytes, 513);
        assert_eq!(obs.transport_return_bytes, 513);
        assert_eq!(obs.endpoint_max_packet_size, Some(8));
        assert_ne!(
            obs.endpoint_max_packet_size.unwrap() as usize,
            obs.protocol_chunk_bytes
        );
    }

    #[test]
    fn successful_write_requires_exact_513_return() {
        let mut session = HidReportSession::new(
            MemHidReportIo::with_write_ok(513),
            LINUX_HIDRAW_BACKEND_CONTRACT,
        );
        let obs = session
            .write_protocol_chunk(&sample_chunk(0xAB), Some(512), Some(8))
            .unwrap();
        assert_eq!(obs.transport_return_bytes, 513);
        assert_eq!(obs.userspace_submit_bytes, 513);
        assert_eq!(obs.report_id, 0);
        assert!(!session.is_stopped());
    }

    #[test]
    fn unexpected_positive_write_count_stops_without_retry() {
        let io = MemHidReportIo::with_write_ok(512);
        let mut session = HidReportSession::new(io, LINUX_HIDRAW_BACKEND_CONTRACT);
        let error = session
            .write_protocol_chunk(&sample_chunk(1), Some(512), None)
            .unwrap_err();
        assert!(
            error.to_string().contains("unexpected HIDAPI write count"),
            "{error:#}"
        );
        assert!(session.is_stopped());
        let follow_up = session
            .write_protocol_chunk(&sample_chunk(2), None, None)
            .unwrap_err();
        assert!(
            follow_up.to_string().contains("session stopped"),
            "{follow_up:#}"
        );
    }

    #[test]
    fn write_transport_error_stops_session() {
        let mut io = MemHidReportIo::new();
        io.fail_after_write = true;
        let mut session = HidReportSession::new(io, LINUX_HIDRAW_BACKEND_CONTRACT);
        let error = session
            .write_protocol_chunk(&sample_chunk(3), None, None)
            .unwrap_err();
        assert!(
            error.to_string().contains("HID report write failed"),
            "{error:#}"
        );
        assert!(session.is_stopped());
    }

    #[test]
    fn write_buffer_prefixes_report_id_zero() {
        let io = MemHidReportIo::with_write_ok(513);
        let mut session = HidReportSession::new(io, LINUX_HIDRAW_BACKEND_CONTRACT);
        session
            .write_protocol_chunk(&sample_chunk(0xCD), None, None)
            .unwrap();
        assert_eq!(session.io.writes.len(), 1);
        assert_eq!(session.io.writes[0].len(), 513);
        assert_eq!(session.io.writes[0][0], 0);
        assert_eq!(session.io.writes[0][1], 0xCD);
    }

    #[test]
    fn chunked_write_splits_on_512_boundaries() {
        let mut io = MemHidReportIo::new();
        io.write_returns.push_back(Ok(513));
        io.write_returns.push_back(Ok(513));
        let mut session = HidReportSession::new(io, LINUX_HIDRAW_BACKEND_CONTRACT);
        let payload = vec![0xEE; 1024];
        let obs = session.write_chunked(&payload, Some(512), None).unwrap();
        assert_eq!(obs.len(), 2);
        assert_eq!(session.io.writes.len(), 2);
        assert!(session.io.writes.iter().all(|write| write.len() == 513));
    }

    #[test]
    fn chunked_write_rejects_non_multiple_payload() {
        let mut session =
            HidReportSession::new(MemHidReportIo::new(), LINUX_HIDRAW_BACKEND_CONTRACT);
        let error = session
            .write_chunked(&vec![0; 600], None, None)
            .unwrap_err();
        assert!(
            error.to_string().contains("not a multiple of 512"),
            "{error:#}"
        );
    }

    #[test]
    fn read_records_capacity_return_and_protocol_lengths_separately() {
        let mut io = MemHidReportIo::new();
        io.read_data
            .push_back(vec![0xDA, 0xDB, 0xDC, 0xDD, 0, 0x3A, 0, 0]);
        io.read_returns.push_back(Ok(8));
        let mut session = HidReportSession::new(io, LINUX_HIDRAW_BACKEND_CONTRACT);
        let (obs, data) = session.read_bounded(512).unwrap();
        assert_eq!(obs.read_capacity_bytes, 512);
        assert_eq!(obs.transport_return_bytes, 8);
        assert_eq!(obs.protocol_response_bytes, 8);
        assert_eq!(data.len(), 8);
        assert_eq!(session.io.reads, vec![512]);
    }

    #[test]
    fn read_error_stops_session() {
        let mut io = MemHidReportIo::new();
        io.read_returns.push_back(Ok(-1));
        let mut session = HidReportSession::new(io, LINUX_HIDRAW_BACKEND_CONTRACT);
        let error = session.read_bounded(64).unwrap_err();
        assert!(
            error.to_string().contains("read returned error"),
            "{error:#}"
        );
        assert!(session.is_stopped());
    }

    #[test]
    fn no_out_interrupt_shape_uses_report_path_with_control_ep0_semantics() {
        // Interrupt IN only (wMaxPacketSize=8) with no OUT endpoint: report output still
        // flows through HIDAPI (SET_REPORT on EP0 when no interrupt OUT exists).
        let mut session = HidReportSession::new(
            MemHidReportIo::with_write_ok(513),
            LINUX_HIDRAW_BACKEND_CONTRACT,
        );
        let obs = session
            .write_protocol_chunk(&sample_chunk(0x5A), Some(512), Some(8))
            .unwrap();
        assert_eq!(obs.endpoint_max_packet_size, Some(8));
        assert_eq!(obs.protocol_chunk_bytes, 512);
        assert_eq!(obs.userspace_submit_bytes, 513);
    }

    #[test]
    fn correlate_hidraw_unique_match() {
        let fs = MapSysfs::default()
            .link(
                "/sys/class/hidraw/hidraw3/device",
                "/sys/devices/pci0/usb1/1-2/1-2:1.0",
            )
            .insert_file("/sys/devices/pci0/usb1/1-2/1-2:1.0/busnum", "1")
            .insert_file("/sys/devices/pci0/usb1/1-2/1-2:1.0/devnum", "14");
        let candidates = vec![HidrawCandidate {
            name: "hidraw3".into(),
            sysfs_path: PathBuf::from("/sys/class/hidraw/hidraw3"),
        }];
        let correlation = correlate_hidraw_to_usb(
            UsbBusAddress {
                bus: 1,
                address: 14,
            },
            &candidates,
            &fs,
        )
        .unwrap();
        assert_eq!(correlation.selected.name, "hidraw3");
        assert_eq!(correlation.devnode, PathBuf::from("/dev/hidraw3"));
    }

    #[test]
    fn correlate_hidraw_mismatch_is_error() {
        let fs = MapSysfs::default()
            .link(
                "/sys/class/hidraw/hidraw1/device",
                "/sys/devices/pci0/usb1/1-1/1-1:1.0",
            )
            .insert_file("/sys/devices/pci0/usb1/1-1/1-1:1.0/busnum", "1")
            .insert_file("/sys/devices/pci0/usb1/1-1/1-1:1.0/devnum", "5");
        let candidates = vec![HidrawCandidate {
            name: "hidraw1".into(),
            sysfs_path: PathBuf::from("/sys/class/hidraw/hidraw1"),
        }];
        let error = correlate_hidraw_to_usb(
            UsbBusAddress {
                bus: 1,
                address: 99,
            },
            &candidates,
            &fs,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("no hidraw node correlates"),
            "{error:#}"
        );
    }

    #[test]
    fn correlate_hidraw_ambiguous_is_error() {
        let fs = MapSysfs::default()
            .link(
                "/sys/class/hidraw/hidraw1/device",
                "/sys/devices/pci0/usb1/1-2/1-2:1.0",
            )
            .link(
                "/sys/class/hidraw/hidraw2/device",
                "/sys/devices/pci0/usb1/1-2/1-2:1.1",
            )
            .insert_file("/sys/devices/pci0/usb1/1-2/1-2:1.0/busnum", "2")
            .insert_file("/sys/devices/pci0/usb1/1-2/1-2:1.0/devnum", "7")
            .insert_file("/sys/devices/pci0/usb1/1-2/1-2:1.1/busnum", "2")
            .insert_file("/sys/devices/pci0/usb1/1-2/1-2:1.1/devnum", "7");
        let candidates = vec![
            HidrawCandidate {
                name: "hidraw1".into(),
                sysfs_path: PathBuf::from("/sys/class/hidraw/hidraw1"),
            },
            HidrawCandidate {
                name: "hidraw2".into(),
                sysfs_path: PathBuf::from("/sys/class/hidraw/hidraw2"),
            },
        ];
        let error = correlate_hidraw_to_usb(UsbBusAddress { bus: 2, address: 7 }, &candidates, &fs)
            .unwrap_err();
        assert!(
            error.to_string().contains("ambiguous hidraw correlation"),
            "{error:#}"
        );
    }

    #[test]
    fn backend_contract_pins_hidapi_commit() {
        assert_eq!(
            LINUX_HIDRAW_BACKEND_CONTRACT.hidapi_commit,
            HIDAPI_EVIDENCE_COMMIT
        );
        assert_eq!(
            LINUX_HIDRAW_BACKEND_CONTRACT.expected_return_bytes,
            EXPECTED_TRANSPORT_RETURN_BYTES
        );
    }
}
