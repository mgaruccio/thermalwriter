// SPDX-License-Identifier: GPL-3.0-or-later
//
// Linux HID report transport: sysfs-correlated direct hidraw I/O with pinned
// length contracts. Output may route through interrupt OUT or control SET_REPORT
// on EP0 via the kernel hidraw driver; descriptor-level interrupt OUT is not required.

use super::type2_policy::{
    TYPE2_PROBE_READ_BOUND, Type2NegotiatedObservation, Type2PreHandshakePolicy, WINBOND_HID2_PID,
    WINBOND_HID2_VID, build_type2_init_packet, negotiate_type2_policy,
};
use anyhow::{Context, Result, bail, ensure};
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use thiserror::Error;

/// HIDAPI 0.16 source commit reviewed for write-return semantics alignment.
/// The compiled backend is direct Linux hidraw syscalls, not this HIDAPI revision.
pub const REVIEWED_HIDAPI_EVIDENCE_COMMIT: &str = "518fbd18796b0ef376f47796d1ee8dd63cc9315a";

/// Linux hidraw userspace documentation reference for report routing.
pub const KERNEL_HIDRAW_DOC_REF: &str =
    "Documentation/hid/hidraw.rst (report ID prefix, write/read byte counts)";

/// Protocol payload per HID report chunk (upstream Type 2 fixed chunk size).
pub const PROTOCOL_CHUNK_BYTES: usize = 512;

/// Report ID byte for devices with a single unnumbered output report.
pub const REPORT_ID_UNNUMBERED: u8 = 0;

/// Userspace buffer length: one report-ID byte plus one protocol chunk.
pub const USERSPACE_SUBMIT_BYTES: usize = 1 + PROTOCOL_CHUNK_BYTES;

/// Expected `write(2)` return count for a full 513-byte userspace buffer on hidraw.
pub const EXPECTED_TRANSPORT_RETURN_BYTES: usize = USERSPACE_SUBMIT_BYTES;

/// Recorded backend/version contract for shareable validation reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct HidReportBackendContract {
    pub backend: &'static str,
    pub expected_write_return_bytes: usize,
    pub kernel_hidraw_doc_ref: &'static str,
    pub reviewed_hidapi_evidence_commit: &'static str,
}

impl HidReportBackendContract {
    const fn linux_hidraw_syscall() -> Self {
        Self {
            backend: "linux-hidraw-syscall",
            expected_write_return_bytes: EXPECTED_TRANSPORT_RETURN_BYTES,
            kernel_hidraw_doc_ref: KERNEL_HIDRAW_DOC_REF,
            reviewed_hidapi_evidence_commit: REVIEWED_HIDAPI_EVIDENCE_COMMIT,
        }
    }
}

/// Current Linux direct-hidraw backend contract.
pub const LINUX_HIDRAW_BACKEND_CONTRACT: HidReportBackendContract =
    HidReportBackendContract::linux_hidraw_syscall();

/// Independent length observations for one HID output report write.
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
    pub read_timeout_ms: u32,
    pub transport_return_bytes: isize,
    pub protocol_response_bytes: usize,
}

/// Selected USB bus/address used to correlate exactly one hidraw node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbBusAddress {
    pub bus: u8,
    pub address: u8,
}

/// One hidraw sysfs entry; name is always derived from `sysfs_path` basename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidrawCandidate {
    sysfs_path: PathBuf,
}

impl HidrawCandidate {
    /// Build from a sysfs class entry; rejects basename/name mismatches.
    pub fn from_sysfs_class_entry(sysfs_path: PathBuf) -> Result<Self> {
        let name = sysfs_path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "hidraw sysfs path has no basename: {}",
                    sysfs_path.display()
                )
            })?;
        validate_hidraw_name(name)?;
        let expected = PathBuf::from(format!("/sys/class/hidraw/{name}"));
        ensure!(
            sysfs_path == expected,
            "hidraw sysfs path {} does not match trusted class entry {}",
            sysfs_path.display(),
            expected.display()
        );
        Ok(Self { sysfs_path })
    }

    pub fn name(&self) -> &str {
        self.sysfs_path
            .file_name()
            .and_then(|value| value.to_str())
            .expect("validated hidraw basename")
    }

    pub fn sysfs_path(&self) -> &Path {
        &self.sysfs_path
    }

    pub fn devnode(&self) -> PathBuf {
        PathBuf::from(format!("/dev/{}", self.name()))
    }
}

/// Result of correlating hidraw candidates to a selected USB identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidrawCorrelation {
    pub selected: HidrawCandidate,
    pub devnode: PathBuf,
}

/// Production write authorization bound to a session probe (not caller-constructible).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HidReportWriteAuthorization {
    _private: (),
}

impl HidReportWriteAuthorization {
    fn from_session_probe(obs: &Type2NegotiatedObservation) -> Result<Self> {
        super::type2_policy::authorize_hid_report_writes(obs)?;
        Ok(Self { _private: () })
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error(
    "unexpected HID write count: submitted={submitted} returned={returned} expected={expected}"
)]
pub struct HidWriteCountError {
    pub submitted: usize,
    pub returned: isize,
    pub expected: usize,
    pub observation: HidWriteObservation,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum HidReportWriteError {
    #[error("HID report write returned error ({returned})")]
    NegativeReturn {
        returned: isize,
        observation: HidWriteObservation,
    },
    #[error(transparent)]
    UnexpectedCount(#[from] HidWriteCountError),
    #[error("HID report write transport error: {message}")]
    Transport {
        message: String,
        observation: HidWriteObservation,
    },
    #[error("HID report session stopped after prior error")]
    SessionStopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidChunkedWriteFailure {
    pub completed: Vec<HidWriteObservation>,
    pub error: HidReportWriteError,
}

impl std::fmt::Display for HidChunkedWriteFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "HID chunked write failed after {} completed chunk(s): {}",
            self.completed.len(),
            self.error
        )
    }
}

impl std::error::Error for HidChunkedWriteFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum HidReportProbeError {
    #[error(transparent)]
    Read(#[from] HidReportReadError),
    #[error("Type2 negotiation failed: {0}")]
    Negotiate(String),
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum HidReportAuthorizeError {
    #[error("HID report session probe not performed")]
    ProbeNotPerformed,
    #[error("HID report write authorization requires PM58/SUB0 probe on this session")]
    ProbeNotAuthorized,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum HidReportReadError {
    #[error("HID report read returned error ({returned})")]
    NegativeReturn {
        returned: isize,
        observation: HidReadObservation,
    },
    #[error("HID report read returned {returned} bytes, exceeding capacity {capacity}")]
    ExceedsCapacity {
        returned: usize,
        capacity: usize,
        observation: HidReadObservation,
    },
    #[error("HID report read transport error: {message}")]
    Transport {
        message: String,
        observation: HidReadObservation,
    },
    #[error("HID report session stopped after prior error")]
    SessionStopped,
}

/// Injectable sysfs access for correlation tests.
pub trait SysfsAccess {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf>;
    fn read_trimmed(&self, path: &Path) -> Result<String>;
    fn exists(&self, path: &Path) -> bool;
}

/// Injectable character-device identity for post-open authentication tests.
pub trait CharDeviceIdentity {
    fn rdev(&self) -> Result<(u32, u32)>;
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

fn validate_hidraw_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && !name.contains('/') && !name.contains('\0'),
        "invalid hidraw name {name:?}"
    );
    let Some(suffix) = name.strip_prefix("hidraw") else {
        bail!("invalid hidraw name {name:?}: expected hidraw prefix");
    };
    ensure!(
        !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()),
        "invalid hidraw name {name:?}: suffix must be decimal digits"
    );
    Ok(())
}

fn hidraw_name_from_char_sysfs(char_sysfs: &Path) -> Option<String> {
    let file_name = char_sysfs.file_name()?.to_str()?;
    if file_name.starts_with("hidraw") {
        validate_hidraw_name(file_name).ok()?;
        return Some(file_name.to_string());
    }
    None
}

/// Authenticate an opened character device against the selected hidraw name and USB ancestor.
pub fn authenticate_opened_hidraw(
    char_rdev: (u32, u32),
    expected_name: &str,
    expected_usb: UsbBusAddress,
    fs: &dyn SysfsAccess,
) -> Result<()> {
    validate_hidraw_name(expected_name)?;
    let char_path = PathBuf::from(format!("/sys/dev/char/{}:{}", char_rdev.0, char_rdev.1));
    let resolved = fs
        .canonicalize(&char_path)
        .with_context(|| format!("resolve {}", char_path.display()))?;
    let resolved_name = hidraw_name_from_char_sysfs(&resolved).ok_or_else(|| {
        anyhow::anyhow!(
            "opened character device {}:{} resolves to {}, not a hidraw node",
            char_rdev.0,
            char_rdev.1,
            resolved.display()
        )
    })?;
    ensure!(
        resolved_name == expected_name,
        "opened hidraw node {resolved_name} does not match selected {expected_name}"
    );
    let hidraw_sysfs = PathBuf::from(format!("/sys/class/hidraw/{expected_name}"));
    let Some(resolved_usb) = resolve_usb_bus_address_from_hidraw_sysfs(&hidraw_sysfs, fs) else {
        bail!("cannot resolve USB bus/address for opened hidraw node {expected_name}");
    };
    ensure!(
        resolved_usb.bus == expected_usb.bus && resolved_usb.address == expected_usb.address,
        "opened hidraw USB bus={} address={} does not match selected bus={} address={}",
        resolved_usb.bus,
        resolved_usb.address,
        expected_usb.bus,
        expected_usb.address
    );
    Ok(())
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
        let Some(resolved) = resolve_usb_bus_address_from_hidraw_sysfs(candidate.sysfs_path(), fs)
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
                devnode: selected.devnode(),
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

/// Injectable HID report I/O (direct hidraw semantics without hardware).
pub trait HidReportIo: Send {
    fn write(&mut self, data: &[u8]) -> Result<isize>;
    /// Read with an explicit millisecond timeout (0 = immediate return when no data).
    fn read_timeout(&mut self, buf: &mut [u8], timeout_ms: u32) -> Result<isize>;
}

pub(crate) struct HidReportSessionCore<Io: HidReportIo> {
    io: Io,
    contract: &'static HidReportBackendContract,
    stopped: bool,
}

impl<Io: HidReportIo> HidReportSessionCore<Io> {
    fn new(io: Io, contract: &'static HidReportBackendContract) -> Self {
        Self {
            io,
            contract,
            stopped: false,
        }
    }

    fn contract(&self) -> &'static HidReportBackendContract {
        self.contract
    }

    fn is_stopped(&self) -> bool {
        self.stopped
    }

    fn stop_on_error(&mut self) {
        self.stopped = true;
    }

    fn read_bounded(
        &mut self,
        capacity: usize,
        timeout_ms: u32,
    ) -> Result<(HidReadObservation, Vec<u8>), HidReportReadError> {
        if self.stopped {
            return Err(HidReportReadError::SessionStopped);
        }
        if capacity == 0 {
            return Err(HidReportReadError::Transport {
                message: "HID report read capacity must be > 0".into(),
                observation: HidReadObservation {
                    read_capacity_bytes: capacity,
                    read_timeout_ms: timeout_ms,
                    transport_return_bytes: 0,
                    protocol_response_bytes: 0,
                },
            });
        }

        let mut buf = vec![0_u8; capacity];
        let returned = match self.io.read_timeout(&mut buf, timeout_ms) {
            Ok(value) => value,
            Err(error) => {
                self.stop_on_error();
                return Err(HidReportReadError::Transport {
                    message: error.to_string(),
                    observation: HidReadObservation {
                        read_capacity_bytes: capacity,
                        read_timeout_ms: timeout_ms,
                        transport_return_bytes: 0,
                        protocol_response_bytes: 0,
                    },
                });
            }
        };

        if returned < 0 {
            self.stop_on_error();
            return Err(HidReportReadError::NegativeReturn {
                returned,
                observation: HidReadObservation {
                    read_capacity_bytes: capacity,
                    read_timeout_ms: timeout_ms,
                    transport_return_bytes: returned,
                    protocol_response_bytes: 0,
                },
            });
        }

        let returned_usize = returned as usize;
        if returned_usize > capacity {
            self.stop_on_error();
            return Err(HidReportReadError::ExceedsCapacity {
                returned: returned_usize,
                capacity,
                observation: HidReadObservation {
                    read_capacity_bytes: capacity,
                    read_timeout_ms: timeout_ms,
                    transport_return_bytes: returned,
                    protocol_response_bytes: 0,
                },
            });
        }

        buf.truncate(returned_usize);
        let observation = HidReadObservation {
            read_capacity_bytes: capacity,
            read_timeout_ms: timeout_ms,
            transport_return_bytes: returned,
            protocol_response_bytes: returned_usize,
        };
        Ok((observation, buf))
    }
}

/// Read-only HID report session (bounded reads, no writes).
pub struct HidReportReadSession<Io: HidReportIo> {
    #[cfg(test)]
    pub(crate) core: HidReportSessionCore<Io>,
    #[cfg(not(test))]
    core: HidReportSessionCore<Io>,
    session_auth: Option<HidReportWriteAuthorization>,
    probe_performed: bool,
}

impl<Io: HidReportIo> HidReportReadSession<Io> {
    #[cfg(test)]
    pub(crate) fn new_for_test(io: Io) -> Self {
        Self::new(io)
    }

    fn new(io: Io) -> Self {
        Self::from_io(io)
    }

    /// Construct a read session from an opened I/O handle.
    pub fn from_io(io: Io) -> Self {
        Self {
            core: HidReportSessionCore::new(io, &LINUX_HIDRAW_BACKEND_CONTRACT),
            session_auth: None,
            probe_performed: false,
        }
    }

    pub fn contract(&self) -> &'static HidReportBackendContract {
        self.core.contract()
    }

    pub fn is_stopped(&self) -> bool {
        self.core.is_stopped()
    }

    pub fn read_bounded(
        &mut self,
        capacity: usize,
        timeout_ms: u32,
    ) -> Result<(HidReadObservation, Vec<u8>), HidReportReadError> {
        self.core.read_bounded(capacity, timeout_ms)
    }

    /// Read exactly one passive Type2 observation without negotiating or writing.
    pub fn read_type2_passive(
        &mut self,
        timeout_ms: u32,
    ) -> Result<(HidReadObservation, Vec<u8>), HidReportReadError> {
        self.session_auth = None;
        self.probe_performed = true;
        let (observation, response) = self.core.read_bounded(TYPE2_PROBE_READ_BOUND, timeout_ms)?;
        // Authorization is derived only from this same passive read and exact PM58 bytes.
        if response == super::policy::PM58_RESPONSE {
            if let Ok(negotiated) = negotiate_type2_policy(
                WINBOND_HID2_VID,
                WINBOND_HID2_PID,
                &response,
                Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
            ) {
                self.session_auth =
                    HidReportWriteAuthorization::from_session_probe(&negotiated).ok();
            }
        }
        Ok((observation, response))
    }

    /// Perform the 4.07 read-only Type2 probe on this session's I/O handle for the exact
    /// Winbond `0416:5302` firmware-4.07 policy.
    ///
    /// Clears any prior probe authorization, then reads passively. For the
    /// legacy validation helper, an empty read may elicit the legacy response;
    /// production Type2 output uses the HidLcd state machine instead.
    ///
    /// Negotiates with [`WINBOND_HID2_VID`]/[`WINBOND_HID2_PID`]. Stores write
    /// authorization only when the same session yields the exact PM58/SUB0 short
    /// response. Returns the observation for reporting.
    pub fn probe_type2_read_only(
        &mut self,
        timeout_ms: u32,
    ) -> Result<Type2NegotiatedObservation, HidReportProbeError> {
        self.session_auth = None;
        self.probe_performed = true;
        let (_, mut response) = self
            .core
            .read_bounded(TYPE2_PROBE_READ_BOUND, timeout_ms)
            .map_err(HidReportProbeError::Read)?;
        if response.is_empty() {
            let init = build_type2_init_packet();
            let mut api_buffer = [0_u8; USERSPACE_SUBMIT_BYTES];
            api_buffer[0] = REPORT_ID_UNNUMBERED;
            api_buffer[1..].copy_from_slice(&init);
            if let Err(error) = self.core.io.write(&api_buffer) {
                self.core.stop_on_error();
                return Err(HidReportProbeError::Read(HidReportReadError::Transport {
                    message: format!("legacy probe init elicit write failed: {error}"),
                    observation: HidReadObservation {
                        read_capacity_bytes: TYPE2_PROBE_READ_BOUND,
                        read_timeout_ms: timeout_ms,
                        transport_return_bytes: 0,
                        protocol_response_bytes: 0,
                    },
                }));
            }
            let (_, elicited) = self
                .core
                .read_bounded(TYPE2_PROBE_READ_BOUND, timeout_ms)
                .map_err(HidReportProbeError::Read)?;
            response = elicited;
        }
        let observation = negotiate_type2_policy(
            WINBOND_HID2_VID,
            WINBOND_HID2_PID,
            &response,
            Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
        )
        .map_err(|error| HidReportProbeError::Negotiate(error.to_string()))?;
        self.session_auth = HidReportWriteAuthorization::from_session_probe(&observation).ok();
        Ok(observation)
    }

    /// Production passive probe: exactly one read and no hidraw write, including
    /// when the device has no queued response. PM128 re-elicit is performed only
    /// after this session is closed and libusb has claimed the exact descriptor.
    pub fn probe_type2_passive(
        &mut self,
        timeout_ms: u32,
    ) -> Result<Type2NegotiatedObservation, HidReportProbeError> {
        let (_, response) = self
            .read_type2_passive(timeout_ms)
            .map_err(HidReportProbeError::Read)?;
        let observation = negotiate_type2_policy(
            WINBOND_HID2_VID,
            WINBOND_HID2_PID,
            &response,
            Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
        )
        .map_err(|error| HidReportProbeError::Negotiate(error.to_string()))?;
        self.session_auth = HidReportWriteAuthorization::from_session_probe(&observation).ok();
        Ok(observation)
    }

    /// Promote this read-capable session to write-authorized use on the same I/O handle.
    ///
    /// Requires a prior [`probe_type2_read_only`] that stored PM58/SUB0 authorization from
    /// bytes read on this session. Caller-supplied observations cannot authorize writes.
    pub fn authorize_writes(self) -> Result<HidReportWriteSession<Io>, HidReportAuthorizeError> {
        if !self.probe_performed {
            return Err(HidReportAuthorizeError::ProbeNotPerformed);
        }
        let auth = self
            .session_auth
            .ok_or(HidReportAuthorizeError::ProbeNotAuthorized)?;
        Ok(HidReportWriteSession {
            core: self.core,
            _auth: auth,
        })
    }
}

/// Write-authorized HID report session (requires negotiated PM58 HID-report policy).
pub struct HidReportWriteSession<Io: HidReportIo> {
    #[cfg(test)]
    pub(crate) core: HidReportSessionCore<Io>,
    #[cfg(not(test))]
    core: HidReportSessionCore<Io>,
    _auth: HidReportWriteAuthorization,
}

impl<Io: HidReportIo> HidReportWriteSession<Io> {
    #[cfg(test)]
    pub(crate) fn new_for_test(io: Io, auth: HidReportWriteAuthorization) -> Self {
        Self {
            core: HidReportSessionCore::new(io, &LINUX_HIDRAW_BACKEND_CONTRACT),
            _auth: auth,
        }
    }

    pub fn contract(&self) -> &'static HidReportBackendContract {
        self.core.contract()
    }

    pub fn is_stopped(&self) -> bool {
        self.core.is_stopped()
    }

    pub fn read_bounded(
        &mut self,
        capacity: usize,
        timeout_ms: u32,
    ) -> Result<(HidReadObservation, Vec<u8>), HidReportReadError> {
        self.core.read_bounded(capacity, timeout_ms)
    }

    fn write_protocol_chunk(
        &mut self,
        chunk: &[u8],
        logical_output_report_bytes: Option<usize>,
        endpoint_max_packet_size: Option<u16>,
    ) -> Result<HidWriteObservation, HidReportWriteError> {
        if self.core.stopped {
            return Err(HidReportWriteError::SessionStopped);
        }

        if chunk.len() != PROTOCOL_CHUNK_BYTES {
            return Err(HidReportWriteError::Transport {
                message: format!(
                    "expected one {PROTOCOL_CHUNK_BYTES}-byte protocol chunk, got {}",
                    chunk.len()
                ),
                observation: HidWriteObservation {
                    protocol_chunk_bytes: chunk.len(),
                    logical_output_report_bytes,
                    report_id: REPORT_ID_UNNUMBERED,
                    userspace_submit_bytes: 0,
                    transport_return_bytes: 0,
                    endpoint_max_packet_size,
                },
            });
        }

        let mut api_buffer = [0_u8; USERSPACE_SUBMIT_BYTES];
        api_buffer[0] = REPORT_ID_UNNUMBERED;
        api_buffer[1..].copy_from_slice(chunk);

        let returned = match self.core.io.write(&api_buffer) {
            Ok(value) => value,
            Err(error) => {
                self.core.stop_on_error();
                return Err(HidReportWriteError::Transport {
                    message: error.to_string(),
                    observation: HidWriteObservation {
                        protocol_chunk_bytes: PROTOCOL_CHUNK_BYTES,
                        logical_output_report_bytes,
                        report_id: REPORT_ID_UNNUMBERED,
                        userspace_submit_bytes: api_buffer.len(),
                        transport_return_bytes: 0,
                        endpoint_max_packet_size,
                    },
                });
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
            self.core.stop_on_error();
            return Err(HidReportWriteError::NegativeReturn {
                returned,
                observation,
            });
        }

        if returned as usize != self.core.contract.expected_write_return_bytes {
            self.core.stop_on_error();
            return Err(HidReportWriteError::UnexpectedCount(HidWriteCountError {
                submitted: api_buffer.len(),
                returned,
                expected: self.core.contract.expected_write_return_bytes,
                observation,
            }));
        }

        Ok(observation)
    }

    /// Write a multi-chunk payload as sequential 512-byte HID report chunks.
    pub fn write_chunked(
        &mut self,
        payload: &[u8],
        logical_output_report_bytes: Option<usize>,
        endpoint_max_packet_size: Option<u16>,
    ) -> Result<Vec<HidWriteObservation>, HidChunkedWriteFailure> {
        if payload.is_empty() {
            return Err(HidChunkedWriteFailure {
                completed: Vec::new(),
                error: HidReportWriteError::Transport {
                    message: "HID report chunked write requires non-empty payload".into(),
                    observation: HidWriteObservation {
                        protocol_chunk_bytes: 0,
                        logical_output_report_bytes,
                        report_id: REPORT_ID_UNNUMBERED,
                        userspace_submit_bytes: 0,
                        transport_return_bytes: 0,
                        endpoint_max_packet_size,
                    },
                },
            });
        }
        if payload.len() % PROTOCOL_CHUNK_BYTES != 0 {
            return Err(HidChunkedWriteFailure {
                completed: Vec::new(),
                error: HidReportWriteError::Transport {
                    message: format!(
                        "HID report payload length {} is not a multiple of {PROTOCOL_CHUNK_BYTES}",
                        payload.len()
                    ),
                    observation: HidWriteObservation {
                        protocol_chunk_bytes: payload.len(),
                        logical_output_report_bytes,
                        report_id: REPORT_ID_UNNUMBERED,
                        userspace_submit_bytes: 0,
                        transport_return_bytes: 0,
                        endpoint_max_packet_size,
                    },
                },
            });
        }

        let mut observations = Vec::new();
        for chunk in payload.chunks(PROTOCOL_CHUNK_BYTES) {
            match self.write_protocol_chunk(
                chunk,
                logical_output_report_bytes,
                endpoint_max_packet_size,
            ) {
                Ok(observation) => observations.push(observation),
                Err(error) => {
                    return Err(HidChunkedWriteFailure {
                        completed: observations,
                        error,
                    });
                }
            }
        }
        Ok(observations)
    }
}

/// Outcome of polling a hidraw fd before `read(2)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HidrawPollOutcome {
    Ready,
    Timeout,
}

/// Interpret `poll(2)` results for hidraw reads (testable without hardware).
pub(crate) fn interpret_hidraw_poll(poll_result: i32, revents: i16) -> Result<HidrawPollOutcome> {
    if poll_result < 0 {
        let err = std::io::Error::last_os_error();
        bail!("hidraw poll(2) failed: {err}");
    }
    if revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
        bail!("hidraw poll(2) reported error revents=0x{revents:x} (POLLERR|POLLHUP|POLLNVAL)");
    }
    if poll_result == 0 {
        return Ok(HidrawPollOutcome::Timeout);
    }
    if revents & libc::POLLIN == 0 {
        bail!("hidraw poll(2) returned without POLLIN (revents=0x{revents:x})");
    }
    Ok(HidrawPollOutcome::Ready)
}

/// One `poll(2)` invocation (injectable for deadline/EINTR tests).
pub(crate) fn poll_hidraw_once(fd: libc::c_int, timeout_ms: i32) -> std::io::Result<(i32, i16)> {
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let poll_result = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
    if poll_result < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((poll_result, poll_fd.revents))
}

/// Poll one hidraw fd with a monotonic deadline; always invoked (including `timeout_ms == 0`).
///
/// Zero timeout: one `poll(0)` only; `EINTR` fails immediately (no retry that could extend wait).
/// Non-zero: recompute remaining time from the deadline after `EINTR` so repeated signals cannot
/// extend the operation beyond the original timeout.
pub(crate) fn poll_hidraw_for_read_deadline(
    fd: libc::c_int,
    timeout_ms: u32,
    mut poll_once: impl FnMut(libc::c_int, i32) -> std::io::Result<(i32, i16)>,
    now: impl Fn() -> Instant,
) -> Result<HidrawPollOutcome> {
    if timeout_ms == 0 {
        let (poll_result, revents) = match poll_once(fd, 0) {
            Ok(value) => value,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {
                bail!("hidraw poll(2) interrupted with zero timeout: {err}");
            }
            Err(err) => bail!("hidraw poll(2) failed: {err}"),
        };
        return interpret_hidraw_poll(poll_result, revents);
    }

    let deadline = now() + Duration::from_millis(timeout_ms as u64);
    loop {
        let remaining = deadline.saturating_duration_since(now());
        if remaining.is_zero() {
            return Ok(HidrawPollOutcome::Timeout);
        }
        let remaining_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        let (poll_result, revents) = match poll_once(fd, remaining_ms) {
            Ok(value) => value,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => bail!("hidraw poll(2) failed: {err}"),
        };
        if poll_result == 0 && now() >= deadline {
            return Ok(HidrawPollOutcome::Timeout);
        }
        return interpret_hidraw_poll(poll_result, revents);
    }
}

/// Poll one hidraw fd using the production `poll(2)` backend.
pub(crate) fn poll_hidraw_for_read(fd: libc::c_int, timeout_ms: u32) -> Result<HidrawPollOutcome> {
    poll_hidraw_for_read_deadline(fd, timeout_ms, poll_hidraw_once, Instant::now)
}

#[cfg(feature = "daemon")]
pub mod linux {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::os::linux::fs::MetadataExt;

    pub struct LinuxHidrawIo {
        file: File,
    }

    impl LinuxHidrawIo {
        pub fn open_authenticated(
            correlation: &HidrawCorrelation,
            selector: UsbBusAddress,
            fs: &dyn SysfsAccess,
        ) -> Result<Self> {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&correlation.devnode)
                .with_context(|| format!("open hidraw {}", correlation.devnode.display()))?;
            let metadata = file.metadata().context("fstat hidraw device")?;
            let (major, minor) = linux_rdev_from_st_rdev(metadata.st_rdev());
            authenticate_opened_hidraw((major, minor), correlation.selected.name(), selector, fs)?;
            Ok(Self { file })
        }
    }

    impl CharDeviceIdentity for File {
        fn rdev(&self) -> Result<(u32, u32)> {
            let metadata = self.metadata().context("fstat hidraw device")?;
            Ok(linux_rdev_from_st_rdev(metadata.st_rdev()))
        }
    }

    fn linux_rdev_from_st_rdev(st_rdev: u64) -> (u32, u32) {
        let major = libc::major(st_rdev as libc::dev_t) as u32;
        let minor = libc::minor(st_rdev as libc::dev_t) as u32;
        (major, minor)
    }

    impl HidReportIo for LinuxHidrawIo {
        fn write(&mut self, data: &[u8]) -> Result<isize> {
            let count = self.file.write(data).context("hidraw write(2)")?;
            isize::try_from(count).context("hidraw write count overflow")
        }

        fn read_timeout(&mut self, buf: &mut [u8], timeout_ms: u32) -> Result<isize> {
            match poll_hidraw_for_read(self.file.as_raw_fd(), timeout_ms)? {
                HidrawPollOutcome::Timeout => return Ok(0),
                HidrawPollOutcome::Ready => {}
            }
            let count = self.file.read(buf).context("hidraw read(2)")?;
            isize::try_from(count).context("hidraw read count overflow")
        }
    }

    /// Open a correlated hidraw devnode once (O_RDWR); probe via
    /// [`HidReportReadSession::probe_type2_read_only`], then promote via
    /// [`HidReportReadSession::authorize_writes`].
    pub fn open_correlated_read_session(
        selector: UsbBusAddress,
        candidates: &[HidrawCandidate],
    ) -> Result<HidReportReadSession<LinuxHidrawIo>> {
        let correlation = correlate_hidraw_to_usb(selector, candidates, &RealSysfs)?;
        let io = LinuxHidrawIo::open_authenticated(&correlation, selector, &RealSysfs)?;
        Ok(HidReportReadSession::new(io))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::type2_policy::{
        Type2PreHandshakePolicy, WINBOND_HID2_PID, WINBOND_HID2_VID, negotiate_type2_policy,
    };
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_MEM_IO_ID: AtomicU64 = AtomicU64::new(1);

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
        id: u64,
        write_returns: std::collections::VecDeque<Result<isize>>,
        read_returns: std::collections::VecDeque<Result<isize>>,
        read_data: std::collections::VecDeque<Vec<u8>>,
        read_timeouts: Vec<u32>,
        writes: Vec<Vec<u8>>,
        reads: Vec<usize>,
        fail_after_write: bool,
    }

    impl MemHidReportIo {
        fn new() -> Self {
            Self {
                id: NEXT_MEM_IO_ID.fetch_add(1, Ordering::Relaxed),
                write_returns: std::collections::VecDeque::new(),
                read_returns: std::collections::VecDeque::new(),
                read_data: std::collections::VecDeque::new(),
                read_timeouts: Vec::new(),
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

        fn id(&self) -> u64 {
            self.id
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

        fn read_timeout(&mut self, buf: &mut [u8], timeout_ms: u32) -> Result<isize> {
            self.read_timeouts.push(timeout_ms);
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

    fn pm58_short_response() -> [u8; 8] {
        [0xDA, 0xDB, 0xDC, 0xDD, 0x00, 0x3A, 0x00, 0x00]
    }

    fn pm68_short_response() -> [u8; 8] {
        let mut resp = pm58_short_response();
        resp[5] = 0x44;
        resp
    }

    fn setup_probe_read(io: &mut MemHidReportIo, response: &[u8]) {
        io.read_data.push_back(response.to_vec());
        io.read_returns.push_back(Ok(response.len() as isize));
    }

    fn pm58_auth() -> HidReportWriteAuthorization {
        let obs = negotiate_type2_policy(
            WINBOND_HID2_VID,
            WINBOND_HID2_PID,
            &pm58_short_response(),
            Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
        )
        .unwrap();
        HidReportWriteAuthorization::from_session_probe(&obs).unwrap()
    }

    fn expect_authorize_error(
        session: HidReportReadSession<MemHidReportIo>,
    ) -> HidReportAuthorizeError {
        match session.authorize_writes() {
            Err(error) => error,
            Ok(_) => panic!("expected authorize_writes to fail"),
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
    fn write_authorization_requires_pm58_hid_report_policy() {
        let obs = negotiate_type2_policy(
            WINBOND_HID2_VID,
            WINBOND_HID2_PID,
            &pm68_short_response(),
            Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
        )
        .unwrap();
        let error = HidReportWriteAuthorization::from_session_probe(&obs).unwrap_err();
        assert!(
            error.to_string().contains("PM58/SUB0")
                || error
                    .to_string()
                    .contains("active HID report writes not authorized"),
            "{error:#}"
        );
    }

    #[test]
    fn successful_write_requires_exact_513_return() {
        let mut session =
            HidReportWriteSession::new_for_test(MemHidReportIo::with_write_ok(513), pm58_auth());
        let obs = session
            .write_chunked(sample_chunk(0xAB).as_ref(), Some(512), Some(8))
            .unwrap();
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].transport_return_bytes, 513);
        assert!(!session.is_stopped());
    }

    #[test]
    fn unexpected_positive_write_count_stops_without_retry() {
        let io = MemHidReportIo::with_write_ok(512);
        let mut session = HidReportWriteSession::new_for_test(io, pm58_auth());
        let failure = session
            .write_chunked(sample_chunk(1).as_ref(), Some(512), None)
            .unwrap_err();
        assert!(matches!(
            failure.error,
            HidReportWriteError::UnexpectedCount(_)
        ));
        assert!(failure.completed.is_empty());
        assert!(session.is_stopped());
    }

    #[test]
    fn chunked_write_second_chunk_failure_retains_first_observation() {
        let mut io = MemHidReportIo::new();
        io.write_returns.push_back(Ok(513));
        io.write_returns.push_back(Ok(512));
        let mut session = HidReportWriteSession::new_for_test(io, pm58_auth());
        let failure = session
            .write_chunked(&vec![0xEE; 1024], Some(512), None)
            .unwrap_err();
        assert_eq!(failure.completed.len(), 1);
        assert_eq!(failure.completed[0].transport_return_bytes, 513);
        assert!(matches!(
            failure.error,
            HidReportWriteError::UnexpectedCount(_)
        ));
        assert!(session.is_stopped());
        assert_eq!(session.core.io.writes.len(), 2);
    }

    #[test]
    fn chunked_write_transport_error_on_second_chunk_retains_evidence() {
        let mut io = MemHidReportIo::new();
        io.write_returns.push_back(Ok(513));
        io.write_returns
            .push_back(Err(anyhow::anyhow!("injected write transport error")));
        let mut session = HidReportWriteSession::new_for_test(io, pm58_auth());
        let failure = session
            .write_chunked(&vec![0xEE; 1024], None, None)
            .unwrap_err();
        assert_eq!(failure.completed.len(), 1);
        assert!(matches!(
            failure.error,
            HidReportWriteError::Transport { .. }
        ));
        assert_eq!(failure.error.observation_submitted_bytes(), Some(513));
        assert!(session.is_stopped());
        assert_eq!(session.core.io.writes.len(), 2);
    }

    #[test]
    fn chunked_write_negative_second_chunk_retains_first_observation() {
        let mut io = MemHidReportIo::new();
        io.write_returns.push_back(Ok(513));
        io.write_returns.push_back(Ok(-1));
        let mut session = HidReportWriteSession::new_for_test(io, pm58_auth());
        let failure = session
            .write_chunked(&vec![0xEE; 1024], None, None)
            .unwrap_err();
        assert_eq!(failure.completed.len(), 1);
        assert!(matches!(
            failure.error,
            HidReportWriteError::NegativeReturn { .. }
        ));
        assert!(session.is_stopped());
    }

    #[test]
    fn write_transport_error_stops_session() {
        let mut io = MemHidReportIo::new();
        io.fail_after_write = true;
        let mut session = HidReportWriteSession::new_for_test(io, pm58_auth());
        let failure = session
            .write_chunked(sample_chunk(3).as_ref(), None, None)
            .unwrap_err();
        assert!(matches!(
            failure.error,
            HidReportWriteError::Transport { .. }
        ));
        assert!(session.is_stopped());
    }

    #[test]
    fn write_buffer_prefixes_report_id_zero() {
        let io = MemHidReportIo::with_write_ok(513);
        let mut session = HidReportWriteSession::new_for_test(io, pm58_auth());
        session
            .write_chunked(sample_chunk(0xCD).as_ref(), None, None)
            .unwrap();
        assert_eq!(session.core.io.writes.len(), 1);
        assert_eq!(session.core.io.writes[0].len(), 513);
        assert_eq!(session.core.io.writes[0][0], 0);
        assert_eq!(session.core.io.writes[0][1], 0xCD);
    }

    #[test]
    fn chunked_write_splits_on_512_boundaries() {
        let mut io = MemHidReportIo::new();
        io.write_returns.push_back(Ok(513));
        io.write_returns.push_back(Ok(513));
        let mut session = HidReportWriteSession::new_for_test(io, pm58_auth());
        let payload = vec![0xEE; 1024];
        let obs = session.write_chunked(&payload, Some(512), None).unwrap();
        assert_eq!(obs.len(), 2);
        assert_eq!(session.core.io.writes.len(), 2);
        assert!(
            session
                .core
                .io
                .writes
                .iter()
                .all(|write| write.len() == 513)
        );
    }

    #[test]
    fn chunked_write_rejects_non_multiple_payload_without_partial_writes() {
        let mut io = MemHidReportIo::new();
        io.write_returns.push_back(Ok(513));
        io.write_returns.push_back(Ok(513));
        let mut session = HidReportWriteSession::new_for_test(io, pm58_auth());
        let failure = session
            .write_chunked(&vec![0; 1025], None, None)
            .unwrap_err();
        assert!(
            failure.error.to_string().contains("not a multiple of 512"),
            "{:#}",
            failure.error
        );
        assert_eq!(session.core.io.writes.len(), 0);
    }

    #[test]
    fn read_records_capacity_return_and_protocol_lengths_separately() {
        let mut io = MemHidReportIo::new();
        io.read_data
            .push_back(vec![0xDA, 0xDB, 0xDC, 0xDD, 0, 0x3A, 0, 0]);
        io.read_returns.push_back(Ok(8));
        let mut session = HidReportReadSession::new_for_test(io);
        let (obs, data) = session.read_bounded(512, 250).unwrap();
        assert_eq!(obs.read_capacity_bytes, 512);
        assert_eq!(obs.read_timeout_ms, 250);
        assert_eq!(obs.transport_return_bytes, 8);
        assert_eq!(obs.protocol_response_bytes, 8);
        assert_eq!(data.len(), 8);
        assert_eq!(session.core.io.reads, vec![512]);
        assert_eq!(session.core.io.read_timeouts, vec![250]);
    }

    #[test]
    fn read_timeout_zero_returns_without_data() {
        let mut io = MemHidReportIo::new();
        io.read_returns.push_back(Ok(0));
        let mut session = HidReportReadSession::new_for_test(io);
        let (obs, data) = session.read_bounded(64, 0).unwrap();
        assert_eq!(obs.read_timeout_ms, 0);
        assert_eq!(obs.transport_return_bytes, 0);
        assert_eq!(obs.protocol_response_bytes, 0);
        assert_eq!(data.len(), 0);
    }

    #[test]
    fn read_rejects_return_count_larger_than_capacity_and_preserves_evidence() {
        let mut io = MemHidReportIo::new();
        io.read_data.push_back(vec![0xAA; 100]);
        io.read_returns.push_back(Ok(100));
        let mut session = HidReportReadSession::new_for_test(io);
        let error = session.read_bounded(64, 100).unwrap_err();
        assert!(matches!(
            error,
            HidReportReadError::ExceedsCapacity {
                returned: 100,
                capacity: 64,
                ..
            }
        ));
        if let HidReportReadError::ExceedsCapacity { observation, .. } = error {
            assert_eq!(observation.transport_return_bytes, 100);
            assert_eq!(observation.protocol_response_bytes, 0);
        }
        assert!(session.is_stopped());
    }

    #[test]
    fn read_error_stops_session_and_preserves_evidence() {
        let mut io = MemHidReportIo::new();
        io.read_returns.push_back(Ok(-1));
        let mut session = HidReportReadSession::new_for_test(io);
        let error = session.read_bounded(64, 50).unwrap_err();
        assert!(matches!(error, HidReportReadError::NegativeReturn { .. }));
        assert!(session.is_stopped());
    }

    #[test]
    fn no_out_interrupt_shape_uses_report_path_with_control_ep0_semantics() {
        let mut session =
            HidReportWriteSession::new_for_test(MemHidReportIo::with_write_ok(513), pm58_auth());
        let obs = session
            .write_chunked(sample_chunk(0x5A).as_ref(), Some(512), Some(8))
            .unwrap();
        assert_eq!(obs[0].endpoint_max_packet_size, Some(8));
        assert_eq!(obs[0].protocol_chunk_bytes, 512);
        assert_eq!(obs[0].userspace_submit_bytes, 513);
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
        let candidates = vec![
            HidrawCandidate::from_sysfs_class_entry(PathBuf::from("/sys/class/hidraw/hidraw3"))
                .unwrap(),
        ];
        let correlation = correlate_hidraw_to_usb(
            UsbBusAddress {
                bus: 1,
                address: 14,
            },
            &candidates,
            &fs,
        )
        .unwrap();
        assert_eq!(correlation.selected.name(), "hidraw3");
        assert_eq!(correlation.devnode, PathBuf::from("/dev/hidraw3"));
    }

    #[test]
    fn correlate_hidraw_rejects_untrusted_sysfs_path() {
        let error =
            HidrawCandidate::from_sysfs_class_entry(PathBuf::from("/var/lib/fake/hidraw/hidraw3"))
                .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not match trusted class entry"),
            "{error:#}"
        );
    }

    #[test]
    fn authenticate_opened_hidraw_rejects_reassigned_node() {
        let fs = MapSysfs::default()
            .link("/sys/dev/char/239:9", "/sys/class/hidraw/hidraw9")
            .link(
                "/sys/class/hidraw/hidraw3/device",
                "/sys/devices/pci0/usb1/1-2/1-2:1.0",
            )
            .insert_file("/sys/devices/pci0/usb1/1-2/1-2:1.0/busnum", "1")
            .insert_file("/sys/devices/pci0/usb1/1-2/1-2:1.0/devnum", "14");
        let error = authenticate_opened_hidraw(
            (239, 9),
            "hidraw3",
            UsbBusAddress {
                bus: 1,
                address: 14,
            },
            &fs,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not match selected hidraw3"),
            "{error:#}"
        );
    }

    #[test]
    fn authenticate_opened_hidraw_accepts_matching_node() {
        let fs = MapSysfs::default()
            .link("/sys/dev/char/239:3", "/sys/class/hidraw/hidraw3")
            .link(
                "/sys/class/hidraw/hidraw3/device",
                "/sys/devices/pci0/usb1/1-2/1-2:1.0",
            )
            .insert_file("/sys/devices/pci0/usb1/1-2/1-2:1.0/busnum", "1")
            .insert_file("/sys/devices/pci0/usb1/1-2/1-2:1.0/devnum", "14");
        authenticate_opened_hidraw(
            (239, 3),
            "hidraw3",
            UsbBusAddress {
                bus: 1,
                address: 14,
            },
            &fs,
        )
        .unwrap();
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
        let candidates = vec![
            HidrawCandidate::from_sysfs_class_entry(PathBuf::from("/sys/class/hidraw/hidraw1"))
                .unwrap(),
        ];
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
            HidrawCandidate::from_sysfs_class_entry(PathBuf::from("/sys/class/hidraw/hidraw1"))
                .unwrap(),
            HidrawCandidate::from_sysfs_class_entry(PathBuf::from("/sys/class/hidraw/hidraw2"))
                .unwrap(),
        ];
        let error = correlate_hidraw_to_usb(UsbBusAddress { bus: 2, address: 7 }, &candidates, &fs)
            .unwrap_err();
        assert!(
            error.to_string().contains("ambiguous hidraw correlation"),
            "{error:#}"
        );
    }

    #[test]
    fn correlate_hidraw_rejects_malformed_name() {
        let error =
            HidrawCandidate::from_sysfs_class_entry(PathBuf::from("/sys/class/hidraw/../hidraw3"))
                .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not match trusted class entry")
                || error.to_string().contains("invalid hidraw"),
            "{error:#}"
        );
    }

    #[test]
    fn authorize_writes_promotes_same_io_handle_without_reopen() {
        let io = MemHidReportIo::new();
        let io_id = io.id();
        let mut read_session = HidReportReadSession::new_for_test(io);
        setup_probe_read(&mut read_session.core.io, &pm58_short_response());
        read_session.probe_type2_read_only(0).unwrap();
        let write_session = read_session.authorize_writes().unwrap();
        assert_eq!(write_session.core.io.id(), io_id);
    }

    #[test]
    fn passive_empty_read_is_write_free_and_unauthorized() {
        let mut io = MemHidReportIo::new();
        io.read_data.push_back(Vec::new());
        io.read_returns.push_back(Ok(0));
        let mut session = HidReportReadSession::new_for_test(io);
        let (observation, response) = session.read_type2_passive(0).unwrap();
        assert_eq!(observation.protocol_response_bytes, 0);
        assert!(response.is_empty());
        assert!(session.core.io.writes.is_empty());
        assert!(session.authorize_writes().is_err());
    }

    #[test]
    fn silent_probe_elicits_init_then_negotiates_legacy_response() {
        let mut io = MemHidReportIo::new();
        // First read empty; write init returns 513; second read is legacy PM128.
        // Push an empty data buffer so the timeout read does not consume the reply.
        io.read_data.push_back(Vec::new());
        io.read_returns.push_back(Ok(0));
        io.write_returns.push_back(Ok(513));
        let mut legacy = vec![0_u8; 36];
        legacy[0..4].copy_from_slice(&[0xDA, 0xDB, 0xDC, 0xDD]);
        legacy[4] = 1;
        legacy[5] = 128;
        legacy[12] = 0x01;
        io.read_data.push_back(legacy.clone());
        io.read_returns.push_back(Ok(legacy.len() as isize));
        let mut session = HidReportReadSession::new_for_test(io);
        let obs = session.probe_type2_read_only(100).unwrap();
        assert_eq!(obs.pm(), 128);
        assert_eq!(obs.sub(), 1);
        assert!(!obs.policy().active_writes_allowed());
        assert_eq!(session.core.io.writes.len(), 1);
        assert_eq!(session.core.io.writes[0].len(), 513);
        assert_eq!(session.core.io.writes[0][0], 0); // report id
        assert_eq!(&session.core.io.writes[0][1..5], &[0xDA, 0xDB, 0xDC, 0xDD]);
    }

    #[test]
    fn authorize_writes_before_probe_fails() {
        let read_session = HidReportReadSession::new_for_test(MemHidReportIo::new());
        let error = expect_authorize_error(read_session);
        assert_eq!(error, HidReportAuthorizeError::ProbeNotPerformed);
    }

    #[test]
    fn fabricated_negotiated_observation_cannot_authorize_session() {
        let mut read_session = HidReportReadSession::new_for_test(MemHidReportIo::new());
        setup_probe_read(&mut read_session.core.io, &pm68_short_response());
        let obs = read_session.probe_type2_read_only(0).unwrap();
        assert_eq!(obs.pm(), 68);
        let error = expect_authorize_error(read_session);
        assert_eq!(error, HidReportAuthorizeError::ProbeNotAuthorized);
    }

    #[test]
    fn pm68_probe_cannot_promote_even_if_external_negotiation_is_pm58() {
        let mut read_session = HidReportReadSession::new_for_test(MemHidReportIo::new());
        setup_probe_read(&mut read_session.core.io, &pm68_short_response());
        read_session.probe_type2_read_only(0).unwrap();
        let error = expect_authorize_error(read_session);
        assert_eq!(error, HidReportAuthorizeError::ProbeNotAuthorized);
    }

    #[test]
    fn failed_probe_clears_stale_authorization() {
        let mut read_session = HidReportReadSession::new_for_test(MemHidReportIo::new());
        setup_probe_read(&mut read_session.core.io, &pm58_short_response());
        read_session.probe_type2_read_only(0).unwrap();
        read_session.core.io.read_returns.push_back(Ok(-1));
        let probe_error = read_session.probe_type2_read_only(0).unwrap_err();
        assert!(matches!(probe_error, HidReportProbeError::Read(_)));
        let error = expect_authorize_error(read_session);
        assert_eq!(error, HidReportAuthorizeError::ProbeNotAuthorized);
    }

    #[test]
    fn repeated_pm68_probe_cannot_retain_pm58_authorization() {
        let mut read_session = HidReportReadSession::new_for_test(MemHidReportIo::new());
        setup_probe_read(&mut read_session.core.io, &pm58_short_response());
        read_session.probe_type2_read_only(0).unwrap();
        setup_probe_read(&mut read_session.core.io, &pm68_short_response());
        read_session.probe_type2_read_only(0).unwrap();
        let error = expect_authorize_error(read_session);
        assert_eq!(error, HidReportAuthorizeError::ProbeNotAuthorized);
    }

    #[test]
    fn chunked_write_failure_implements_error_with_source() {
        let io = MemHidReportIo::with_write_ok(512);
        let mut session = HidReportWriteSession::new_for_test(io, pm58_auth());
        let failure = session
            .write_chunked(sample_chunk(1).as_ref(), None, None)
            .unwrap_err();
        let rendered = failure.to_string();
        assert!(rendered.contains("completed chunk"));
        assert!(rendered.contains("unexpected HID write count"));
        assert!(std::error::Error::source(&failure).is_some());
    }

    #[test]
    fn interpret_poll_zero_timeout_is_immediate_without_pollin() {
        assert_eq!(
            interpret_hidraw_poll(0, 0).unwrap(),
            HidrawPollOutcome::Timeout
        );
    }

    #[test]
    fn interpret_poll_pollin_is_ready() {
        assert_eq!(
            interpret_hidraw_poll(1, libc::POLLIN).unwrap(),
            HidrawPollOutcome::Ready
        );
    }

    #[test]
    fn interpret_poll_error_revents_fail_closed() {
        for revents in [libc::POLLERR, libc::POLLHUP, libc::POLLNVAL] {
            let error = interpret_hidraw_poll(1, revents).unwrap_err();
            assert!(error.to_string().contains("error revents"), "{error:#}");
        }
    }

    #[test]
    fn interpret_poll_positive_without_pollin_fails() {
        let error = interpret_hidraw_poll(1, libc::POLLOUT).unwrap_err();
        assert!(error.to_string().contains("without POLLIN"), "{error:#}");
    }

    #[test]
    fn poll_zero_timeout_eintr_fails_immediately_without_retry() {
        let mut calls = 0;
        let error = poll_hidraw_for_read_deadline(
            7,
            0,
            |_fd, timeout| {
                calls += 1;
                assert_eq!(timeout, 0);
                Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
            },
            Instant::now,
        )
        .unwrap_err();
        assert_eq!(calls, 1);
        assert!(
            error.to_string().contains("interrupted with zero timeout"),
            "{error:#}"
        );
    }

    #[test]
    fn poll_nonzero_eintr_recomputes_remaining_deadline() {
        let start = Instant::now();
        let elapsed_ms = std::cell::Cell::new(0_u64);
        let mut poll_timeouts = Vec::new();
        let outcome = poll_hidraw_for_read_deadline(
            7,
            100,
            |_fd, timeout| {
                poll_timeouts.push(timeout);
                if poll_timeouts.len() == 1 {
                    elapsed_ms.set(40);
                    return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
                }
                Ok((1, libc::POLLIN))
            },
            || start + Duration::from_millis(elapsed_ms.get()),
        )
        .unwrap();
        assert_eq!(outcome, HidrawPollOutcome::Ready);
        assert_eq!(poll_timeouts.len(), 2);
        assert!(poll_timeouts[0] >= 90 && poll_timeouts[0] <= 100);
        assert!(poll_timeouts[1] >= 50 && poll_timeouts[1] <= 70);
    }

    #[test]
    fn poll_nonzero_deadline_expiry_returns_timeout_without_extra_poll() {
        let start = Instant::now();
        let now_calls = std::cell::Cell::new(0_u32);
        let mut poll_calls = 0;
        let outcome = poll_hidraw_for_read_deadline(
            7,
            40,
            |_fd, _timeout| {
                poll_calls += 1;
                Ok((0, 0))
            },
            || {
                let n = now_calls.get();
                now_calls.set(n + 1);
                start + Duration::from_millis(if n == 0 { 0 } else { 40 })
            },
        )
        .unwrap();
        assert_eq!(outcome, HidrawPollOutcome::Timeout);
        assert_eq!(poll_calls, 0);
    }

    #[test]
    fn backend_contract_records_syscall_backend_and_reviewed_evidence() {
        assert_eq!(
            LINUX_HIDRAW_BACKEND_CONTRACT.backend,
            "linux-hidraw-syscall"
        );
        assert_eq!(
            LINUX_HIDRAW_BACKEND_CONTRACT.reviewed_hidapi_evidence_commit,
            REVIEWED_HIDAPI_EVIDENCE_COMMIT
        );
        assert_eq!(
            LINUX_HIDRAW_BACKEND_CONTRACT.expected_write_return_bytes,
            EXPECTED_TRANSPORT_RETURN_BYTES
        );
    }

    trait ObservationSubmittedBytes {
        fn observation_submitted_bytes(&self) -> Option<usize>;
    }

    impl ObservationSubmittedBytes for HidReportWriteError {
        fn observation_submitted_bytes(&self) -> Option<usize> {
            match self {
                Self::Transport { observation, .. } => Some(observation.userspace_submit_bytes),
                Self::UnexpectedCount(error) => Some(error.observation.userspace_submit_bytes),
                Self::NegativeReturn { observation, .. } => {
                    Some(observation.userspace_submit_bytes)
                }
                Self::SessionStopped => None,
            }
        }
    }
}
