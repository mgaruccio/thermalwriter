// SPDX-License-Identifier: GPL-3.0-or-later
//
// SCSI mass-storage LCD transport.
// Protocol derived from thermalright-trcc-linux at tree
// 390b880abd4cf0ed2d6eae7151493432263eff39 (project version 9.8.6, four commits after the v9.8.6 tag),
// path: src/trcc/adapters/device/scsi_lcd.py

use anyhow::{Context, Result, bail};
use log::{debug, info};
use std::fs::OpenOptions;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::profile::{WireProtocol, build_device_info};
use super::{DeviceInfo, EncodedFrame, Transport};

const BOOT_SIGNATURE: [u8; 4] = [0xA1, 0xA2, 0xA3, 0xA4];
const BOOT_WAIT: Duration = Duration::from_secs(3);
const BOOT_MAX_ATTEMPTS: u32 = 5;
const POST_INIT_DELAY: Duration = Duration::from_millis(100);

const POLL_CMD: u32 = 0xF5;
const INIT_CMD: u32 = 0x1F5;
const POLL_SIZE: u32 = 0xE100;

const FRAME_CMD_BASE: u32 = 0x101F5;
const CHUNK_SIZE_LARGE: u32 = 0x10000;
const CHUNK_SIZE_SMALL: u32 = 0xE100;
const SMALL_DISPLAY_PIXELS: u32 = 76800;

const HANDSHAKE_TIMEOUT_MS: u32 = 10_000;
const FRAME_TIMEOUT_MS: u32 = 5_000;

// Linux SG_IO ioctl.
const SG_IO: libc::c_ulong = 0x2285;
const SG_DXFER_TO_DEV: i32 = -2;
const SG_DXFER_FROM_DEV: i32 = -3;
const SG_FLAG_DIRECT_IO: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct SgIoHdr {
    interface_id: i32,
    dxfer_direction: i32,
    cmd_len: u8,
    mx_sb_len: u8,
    iovec_count: u16,
    dxfer_len: u32,
    dxferp: *mut u8,
    cmdp: *mut u8,
    sbp: *mut u8,
    timeout: u32,
    flags: u32,
    pack_id: i32,
    usr_ptr: *mut u8,
    status: u8,
    masked_status: u8,
    msg_status: u8,
    sb_len_wr: u8,
    host_status: u16,
    driver_status: u16,
    resid: i32,
    duration: u32,
    info: u32,
}

/// Build the 16-byte SCSI CDB: `[cmd:u32 LE][8 zeros][size:u32 LE]`.
pub fn build_cdb(cmd: u32, size: u32) -> [u8; 16] {
    let mut cdb = [0u8; 16];
    cdb[0..4].copy_from_slice(&cmd.to_le_bytes());
    cdb[12..16].copy_from_slice(&size.to_le_bytes());
    cdb
}

/// Compute (cmd, size) pairs for chunked RGB565 frame send. Final chunk is
/// unpadded (`min(chunk, remaining)`).
pub fn frame_chunks(width: u32, height: u32) -> Vec<(u32, u32)> {
    let pixels = width.saturating_mul(height);
    let chunk_size = if pixels <= SMALL_DISPLAY_PIXELS {
        CHUNK_SIZE_SMALL
    } else {
        CHUNK_SIZE_LARGE
    };
    let total = pixels.saturating_mul(2);
    let mut chunks = Vec::new();
    let mut offset = 0u32;
    let mut idx = 0u32;
    while offset < total {
        let size = chunk_size.min(total - offset);
        let cmd = FRAME_CMD_BASE | (idx << 24);
        chunks.push((cmd, size));
        offset += size;
        idx += 1;
    }
    chunks
}

/// Injectable SCSI CDB I/O for tests.
pub trait ScsiIo: Send {
    fn read_cdb(&mut self, cdb: &[u8; 16], size: usize) -> Result<Vec<u8>>;
    fn send_cdb(&mut self, cdb: &[u8; 16], data: &[u8]) -> Result<()>;
    fn wait(&mut self, d: Duration);
}

/// SCSI poll+init handshake control flow over injectable I/O.
pub fn handshake_scsi_with_io(io: &mut dyn ScsiIo, vid: u16, pid: u16) -> Result<DeviceInfo> {
    let poll_cdb = build_cdb(POLL_CMD, POLL_SIZE);
    let mut response = Vec::new();
    let mut booted = false;
    for attempt in 0..BOOT_MAX_ATTEMPTS {
        response = io
            .read_cdb(&poll_cdb, POLL_SIZE as usize)
            .with_context(|| format!("SCSI poll attempt {} failed", attempt + 1))?;
        let is_boot = response.len() >= 8 && response[4..8] == BOOT_SIGNATURE;
        if is_boot {
            let remaining = BOOT_MAX_ATTEMPTS - attempt - 1;
            if remaining == 0 {
                bail!(
                    "SCSI device still booting after {BOOT_MAX_ATTEMPTS} attempts; not initializing"
                );
            }
            io.wait(BOOT_WAIT);
            continue;
        }
        booted = true;
        break;
    }
    if !booted {
        bail!("SCSI poll never left boot state");
    }
    if response.is_empty() {
        bail!("SCSI poll returned empty response");
    }
    let fbl = response[0];
    if fbl == 0 {
        bail!("SCSI poll FBL byte0 is zero");
    }
    let init_cdb = build_cdb(INIT_CMD, POLL_SIZE);
    let zeros = vec![0u8; POLL_SIZE as usize];
    io.send_cdb(&init_cdb, &zeros)
        .context("SCSI init command failed")?;
    io.wait(POST_INIT_DELAY);
    build_device_info(WireProtocol::Scsi, vid, pid, fbl, 0, Some(fbl))
}

/// Send one encoded SCSI frame through injectable CDB I/O.
pub fn send_frame_scsi_with_io(
    io: &mut dyn ScsiIo,
    info: &DeviceInfo,
    frame: &EncodedFrame,
) -> Result<()> {
    if frame.encoding != info.encoding() {
        bail!(
            "frame encoding {} does not match device {}",
            frame.encoding,
            info.encoding()
        );
    }
    if !frame.encoding.is_rgb565() {
        bail!("SCSI requires RGB565 encoding, got {}", frame.encoding);
    }
    let (wire_width, wire_height) = info.wire_dimensions()?;
    if frame.width != wire_width || frame.height != wire_height {
        bail!(
            "frame {}x{} does not match wire dimensions {}x{}",
            frame.width,
            frame.height,
            wire_width,
            wire_height
        );
    }
    let expected = (frame.width as usize)
        .checked_mul(frame.height as usize)
        .and_then(|pixels| pixels.checked_mul(2))
        .context("RGB565 size overflow")?;
    if frame.data.len() != expected {
        bail!(
            "RGB565 payload length {} != expected {expected} for {}x{}",
            frame.data.len(),
            frame.width,
            frame.height
        );
    }

    let chunks = frame_chunks(frame.width, frame.height);
    let mut offset = 0usize;
    for (cmd, size) in chunks {
        let size = size as usize;
        let end = offset + size;
        if end > frame.data.len() {
            bail!("chunk overruns RGB565 payload at offset {offset}");
        }
        io.send_cdb(&build_cdb(cmd, size as u32), &frame.data[offset..end])
            .with_context(|| format!("SCSI frame chunk failed at offset {offset}"))?;
        offset = end;
    }
    if offset != frame.data.len() {
        bail!(
            "SCSI frame chunks sent {offset} bytes but payload is {}",
            frame.data.len()
        );
    }
    Ok(())
}

pub struct ScsiLcd {
    fd: Option<OwnedFd>,
    path: PathBuf,
    vid: u16,
    pid: u16,
    info: Option<DeviceInfo>,
    /// Injected wait hook for tests (defaults to thread::sleep).
    wait: Box<dyn Fn(Duration) + Send>,
}

impl ScsiLcd {
    pub fn open(path: &Path, vid: u16, pid: u16) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .with_context(|| {
                format!(
                    "failed to open SCSI node {} for {:04x}:{:04x} (check udev/scsi_generic access)",
                    path.display(),
                    vid,
                    pid
                )
            })?;
        // Convert File into OwnedFd.
        use std::os::fd::IntoRawFd;
        let raw = file.into_raw_fd();
        // SAFETY: raw comes from a just-opened File; we own it exclusively.
        let fd = unsafe { OwnedFd::from_raw_fd_checked(raw) };

        info!(
            "Opened SCSI LCD {:04x}:{:04x} at {}",
            vid,
            pid,
            path.display()
        );
        Ok(Self {
            fd: Some(fd),
            path: path.to_path_buf(),
            vid,
            pid,
            info: None,
            wait: Box::new(std::thread::sleep),
        })
    }

    /// Test helper: inject wait implementation.
    pub fn set_wait_hook<F>(&mut self, f: F)
    where
        F: Fn(Duration) + Send + 'static,
    {
        self.wait = Box::new(f);
    }

    fn mark_disconnected(&mut self) {
        self.fd = None;
        self.info = None;
    }

    fn sg_io(
        &self,
        cdb: &[u8; 16],
        dxfer_direction: i32,
        data: &mut [u8],
        timeout_ms: u32,
    ) -> Result<()> {
        let fd = self.fd.as_ref().context("SCSI device not open")?;
        let mut cmd = *cdb;
        let mut sense = [0u8; 32];
        let mut hdr = SgIoHdr {
            interface_id: i32::from(b'S'),
            dxfer_direction,
            cmd_len: 16,
            mx_sb_len: sense.len() as u8,
            iovec_count: 0,
            dxfer_len: data.len() as u32,
            dxferp: data.as_mut_ptr(),
            cmdp: cmd.as_mut_ptr(),
            sbp: sense.as_mut_ptr(),
            timeout: timeout_ms,
            flags: SG_FLAG_DIRECT_IO,
            pack_id: 0,
            usr_ptr: std::ptr::null_mut(),
            status: 0,
            masked_status: 0,
            msg_status: 0,
            sb_len_wr: 0,
            host_status: 0,
            driver_status: 0,
            resid: 0,
            duration: 0,
            info: 0,
        };

        // SAFETY: hdr points at valid stack buffers for the duration of ioctl.
        let rc = unsafe { libc::ioctl(fd.as_raw_fd(), SG_IO, &mut hdr) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            match err.raw_os_error() {
                Some(libc::ENODEV) | Some(libc::EIO) | Some(libc::ETIMEDOUT) => {
                    bail!(
                        "SCSI SG_IO fatal I/O error on {}: {err}",
                        self.path.display()
                    );
                }
                _ => bail!("SCSI SG_IO failed on {}: {err}", self.path.display()),
            }
        }

        if hdr.status != 0 || hdr.host_status != 0 || hdr.driver_status != 0 {
            bail!(
                "SCSI command failed status={} host={} driver={} on {}",
                hdr.status,
                hdr.host_status,
                hdr.driver_status,
                self.path.display()
            );
        }

        if dxfer_direction == SG_DXFER_TO_DEV && hdr.resid != 0 {
            bail!(
                "SCSI data-out residual {} on {} (expected full transfer)",
                hdr.resid,
                self.path.display()
            );
        }
        // data-in residual is allowed when the fields we need were read.
        Ok(())
    }

    fn read_cdb(&self, cdb: &[u8; 16], size: usize, timeout_ms: u32) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; size];
        self.sg_io(cdb, SG_DXFER_FROM_DEV, &mut buf, timeout_ms)?;
        Ok(buf)
    }

    fn send_cdb(&self, cdb: &[u8; 16], data: &[u8], timeout_ms: u32) -> Result<()> {
        let mut buf = data.to_vec();
        self.sg_io(cdb, SG_DXFER_TO_DEV, &mut buf, timeout_ms)
    }
}

// OwnedFd helpers for older/stable patterns.
trait FromRawFdChecked: Sized {
    unsafe fn from_raw_fd_checked(fd: std::os::fd::RawFd) -> Self;
}

impl FromRawFdChecked for OwnedFd {
    unsafe fn from_raw_fd_checked(fd: std::os::fd::RawFd) -> Self {
        // SAFETY: caller guarantees fd is open and uniquely owned.
        unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) }
    }
}

impl ScsiIo for ScsiLcd {
    fn read_cdb(&mut self, cdb: &[u8; 16], size: usize) -> Result<Vec<u8>> {
        ScsiLcd::read_cdb(self, cdb, size, HANDSHAKE_TIMEOUT_MS)
    }

    fn send_cdb(&mut self, cdb: &[u8; 16], data: &[u8]) -> Result<()> {
        ScsiLcd::send_cdb(self, cdb, data, FRAME_TIMEOUT_MS)
    }

    fn wait(&mut self, duration: Duration) {
        (self.wait)(duration);
    }
}

impl Transport for ScsiLcd {
    fn handshake(&mut self) -> Result<DeviceInfo> {
        let poll_cdb = build_cdb(POLL_CMD, POLL_SIZE);
        let mut response = Vec::new();
        let mut booted = false;

        for attempt in 0..BOOT_MAX_ATTEMPTS {
            response = ScsiLcd::read_cdb(self, &poll_cdb, POLL_SIZE as usize, HANDSHAKE_TIMEOUT_MS)
                .with_context(|| format!("SCSI poll attempt {} failed", attempt + 1))?;

            let is_boot = response.len() >= 8 && response[4..8] == BOOT_SIGNATURE;
            if is_boot {
                let remaining = BOOT_MAX_ATTEMPTS - attempt - 1;
                if remaining == 0 {
                    self.mark_disconnected();
                    bail!(
                        "SCSI device still booting after {BOOT_MAX_ATTEMPTS} attempts; not initializing"
                    );
                }
                info!(
                    "SCSI device still booting (attempt {}/{}), waiting {}s",
                    attempt + 1,
                    BOOT_MAX_ATTEMPTS,
                    BOOT_WAIT.as_secs()
                );
                (self.wait)(BOOT_WAIT);
                continue;
            }
            booted = true;
            break;
        }

        if !booted {
            self.mark_disconnected();
            bail!("SCSI poll never left boot state");
        }
        if response.is_empty() {
            self.mark_disconnected();
            bail!("SCSI poll returned empty response");
        }

        let fbl = response[0];
        if fbl == 0 {
            self.mark_disconnected();
            bail!("SCSI poll FBL byte0 is zero");
        }
        debug!("SCSI poll byte[0] = {fbl} (FBL)");

        let init_cdb = build_cdb(INIT_CMD, POLL_SIZE);
        let zeros = vec![0u8; POLL_SIZE as usize];
        ScsiLcd::send_cdb(self, &init_cdb, &zeros, HANDSHAKE_TIMEOUT_MS)
            .context("SCSI init command failed")?;
        (self.wait)(POST_INIT_DELAY);

        let info = build_device_info(WireProtocol::Scsi, self.vid, self.pid, fbl, 0, Some(fbl))?;
        info!(
            "SCSI handshake OK: FBL={}, resolution={}x{}, encoding={}",
            fbl,
            info.width(),
            info.height(),
            info.encoding()
        );
        self.info = Some(info.clone());
        Ok(info)
    }

    fn send_frame(&mut self, _frame: &EncodedFrame) -> Result<()> {
        anyhow::bail!("SCSI has no evidence-backed exact production output policy");
    }

    fn close(&mut self) {
        if self.fd.take().is_some() {
            info!("ScsiLcd closed {}", self.path.display());
        }
        self.info = None;
    }

    fn is_connected(&self) -> bool {
        self.fd.is_some() && self.info.is_some()
    }
}

impl Drop for ScsiLcd {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdb_layout_is_cmd_zeros_size() {
        let cdb = build_cdb(0xF5, 0xE100);
        assert_eq!(&cdb[0..4], &0xF5u32.to_le_bytes());
        assert_eq!(&cdb[4..12], &[0u8; 8]);
        assert_eq!(&cdb[12..16], &0xE100u32.to_le_bytes());
    }

    #[test]
    fn small_display_uses_small_chunks_and_unpadded_tail() {
        let chunks = frame_chunks(320, 240);
        let total: u32 = chunks.iter().map(|(_, s)| s).sum();
        assert_eq!(total, 320 * 240 * 2);
        assert!(chunks.iter().all(|(_, s)| *s <= CHUNK_SIZE_SMALL));
        assert_eq!(chunks.last().unwrap().1, total % CHUNK_SIZE_SMALL);
    }

    #[test]
    fn large_display_uses_64kib_chunks() {
        let chunks = frame_chunks(640, 480);
        assert!(chunks.iter().all(|(_, s)| *s <= CHUNK_SIZE_LARGE));
        let total: u32 = chunks.iter().map(|(_, s)| s).sum();
        assert_eq!(total, 640 * 480 * 2);
    }
}
