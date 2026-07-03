use anyhow::{Context, Result, bail};
use log::{debug, info, warn};
use rusb::{DeviceHandle, GlobalContext};
use std::time::Duration;

use super::{DeviceInfo, Transport};

const VID: u16 = 0x87AD;
const PID: u16 = 0x70DB;
const HANDSHAKE_READ_SIZE: usize = 1024;
const TIMEOUT: Duration = Duration::from_secs(1);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const CHUNK_SIZE: usize = 16 * 1024; // 16 KiB per USB bulk write

/// The 64-byte handshake payload from USBLCDNew protocol.
pub fn handshake_payload() -> [u8; 64] {
    let mut payload = [0u8; 64];
    payload[0] = 0x12;
    payload[1] = 0x34;
    payload[2] = 0x56;
    payload[3] = 0x78;
    payload[56] = 0x01;
    payload
}

/// Build the 64-byte frame header for a bulk frame send.
///
/// Layout:
///   [0..4]:   magic 0x12345678 (LE)
///   [4..8]:   cmd (2=JPEG, 3=RGB565) (LE u32)
///   [8..12]:  width (LE u32)
///   [12..16]: height (LE u32)
///   [16..56]: zeros
///   [56..60]: mode = 2 (LE u32)
///   [60..64]: payload length (LE u32)
pub fn build_frame_header(cmd: u32, width: u32, height: u32, payload_len: u32) -> [u8; 64] {
    let mut header = [0u8; 64];
    header[0..4].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);
    header[4..8].copy_from_slice(&cmd.to_le_bytes());
    header[8..12].copy_from_slice(&width.to_le_bytes());
    header[12..16].copy_from_slice(&height.to_le_bytes());
    header[56..60].copy_from_slice(&2u32.to_le_bytes());
    header[60..64].copy_from_slice(&payload_len.to_le_bytes());
    header
}

/// Resolve PM byte to (width, height). Defaults to 480x480 for unknown PMs.
fn pm_to_resolution(pm: u8) -> (u32, u32) {
    match pm {
        5 => (240, 240),
        7 | 9 => (320, 320),
        10..=17 => (320, 240),
        32 => (480, 480),
        50 => (240, 320),
        64..=66 => (320, 320),
        68 | 69 => (480, 480),
        _ => (480, 480), // Default for unknown PMs (including PM=4)
    }
}

/// Write all bytes in `data` by calling `write` repeatedly until the buffer is
/// exhausted. Handles partial writes by advancing the offset and retrying.
/// Returns `Err` immediately if `write` returns `Ok(0)` (signals disconnection)
/// or propagates any `Err` from `write`.
pub fn write_all<W>(data: &[u8], mut write: W) -> Result<()>
where
    W: FnMut(&[u8]) -> rusb::Result<usize>,
{
    let mut sent = 0;
    while sent < data.len() {
        let n = write(&data[sent..]).context("Bulk write failed")?;
        if n == 0 {
            bail!(
                "device returned zero-length write — likely disconnected (after {} bytes)",
                sent
            );
        }
        sent += n;
    }
    Ok(())
}

pub struct BulkUsb {
    handle: Option<DeviceHandle<GlobalContext>>,
    ep_out: u8,
    ep_in: u8,
    info: Option<DeviceInfo>,
    /// Total frames successfully sent over this transport's lifetime —
    /// mirrors NullTransport's counter so profiling can compute
    /// cpu-per-frame on real hardware too. Persists across reconnects
    /// (try_reconnect transplants handle/ep_out/ep_in/info but not this).
    frames_sent: u64,
}

impl BulkUsb {
    pub fn new() -> Result<Self> {
        let handle =
            rusb::open_device_with_vid_pid(VID, PID).context("USB device 87AD:70DB not found")?;

        handle
            .set_auto_detach_kernel_driver(true)
            .context("Failed to set auto-detach kernel driver")?;

        handle
            .claim_interface(0)
            .context("Failed to claim USB interface 0")?;

        // Discover bulk endpoints
        let device = handle.device();
        let config = device
            .active_config_descriptor()
            .context("Failed to get active config descriptor")?;

        let mut ep_out = 0u8;
        let mut ep_in = 0u8;

        for iface in config.interfaces() {
            for desc in iface.descriptors() {
                // Prefer vendor-specific interface (class 255)
                if desc.class_code() == 255 || desc.class_code() == 0 {
                    for ep in desc.endpoint_descriptors() {
                        if ep.transfer_type() == rusb::TransferType::Bulk {
                            if ep.direction() == rusb::Direction::Out {
                                ep_out = ep.address();
                            } else {
                                ep_in = ep.address();
                            }
                        }
                    }
                }
            }
        }

        if ep_out == 0 || ep_in == 0 {
            let _ = handle.release_interface(0);
            bail!("Could not find bulk IN/OUT endpoints");
        }

        info!(
            "Opened BulkUSB device {:04x}:{:04x} (EP OUT=0x{:02x}, EP IN=0x{:02x})",
            VID, PID, ep_out, ep_in
        );

        Ok(Self {
            handle: Some(handle),
            ep_out,
            ep_in,
            info: None,
            frames_sent: 0,
        })
    }

    pub fn disconnected() -> Self {
        Self {
            handle: None,
            ep_out: 0,
            ep_in: 0,
            info: None,
            frames_sent: 0,
        }
    }
}

impl BulkUsb {
    /// If `err` represents a fatal USB error (device gone, pipe stall, etc.),
    /// drop the handle so is_connected() returns false and the tick loop retries.
    /// Uses root_cause() because write_all wraps rusb::Error with anyhow::context.
    fn mark_disconnected_if_fatal(&mut self, err: &anyhow::Error) {
        // Any rusb error from a bulk write means the device is in a bad state.
        // NoDevice/Io/Access/Pipe all indicate the handle is no longer usable.
        // Timeout is excluded — a slow write is not a disconnect.
        // Use chain() not root_cause(): write_all wraps rusb::Error with context,
        // so the rusb::Error appears at chain[1], not as the root cause.
        let is_fatal = err
            .chain()
            .find_map(|cause| cause.downcast_ref::<rusb::Error>())
            .map(|e| !matches!(e, rusb::Error::Timeout))
            .unwrap_or(false);
        if is_fatal {
            warn!("Fatal USB error — marking device disconnected: {}", err);
            self.handle = None;
        }
    }
}

impl Transport for BulkUsb {
    fn handshake(&mut self) -> Result<DeviceInfo> {
        let handle = self.handle.as_ref().context("Device not open")?;

        // Write handshake
        let payload = handshake_payload();
        handle
            .write_bulk(self.ep_out, &payload, TIMEOUT)
            .context("Handshake write failed")?;
        debug!("Handshake sent ({} bytes)", payload.len());

        // Read response
        let mut resp = [0u8; HANDSHAKE_READ_SIZE];
        let n = handle
            .read_bulk(self.ep_in, &mut resp, TIMEOUT)
            .context("Handshake read failed")?;
        info!("Handshake response: {} bytes", n);

        if n < 41 || resp[24] == 0 {
            bail!(
                "Handshake failed: resp[24]={} (expected non-zero)",
                resp[24]
            );
        }

        let pm = resp[24];
        let sub = resp[36];
        let (width, height) = pm_to_resolution(pm);
        let use_jpeg = pm != 32;

        info!(
            "Handshake OK: PM={}, SUB={}, resolution={}x{}, jpeg={}",
            pm, sub, width, height, use_jpeg
        );

        let info = DeviceInfo {
            vid: VID,
            pid: PID,
            width,
            height,
            pm,
            sub,
            use_jpeg,
        };
        self.info = Some(info.clone());
        Ok(info)
    }

    fn send_frame(&mut self, data: &[u8]) -> Result<()> {
        // Build the frame (header + payload) before borrowing the handle for writes.
        let (frame, log_info) = {
            let info = self.info.as_ref().context("Handshake not performed")?;
            let cmd: u32 = if info.use_jpeg { 2 } else { 3 };
            let payload_len = u32::try_from(data.len()).context("frame too large")?;
            let header = build_frame_header(cmd, info.width, info.height, payload_len);
            let mut frame = Vec::with_capacity(64 + data.len());
            frame.extend_from_slice(&header);
            frame.extend_from_slice(data);
            let log = (info.width, info.height, cmd, data.len());
            (frame, log)
        };

        // All writes in a scoped block so the handle borrow ends before any
        // mark_disconnected_if_fatal call (which needs &mut self).
        let send_result: Result<()> = {
            let handle = self.handle.as_ref().context("Device not open")?;
            let ep_out = self.ep_out;

            let chunk_result: Result<()> = frame.chunks(CHUNK_SIZE).try_for_each(|chunk| {
                write_all(chunk, |buf| handle.write_bulk(ep_out, buf, WRITE_TIMEOUT))
            });

            if chunk_result.is_ok() && frame.len() % 512 == 0 {
                handle
                    .write_bulk(ep_out, &[], WRITE_TIMEOUT)
                    .context("ZLP write failed")?;
            }
            chunk_result
        };

        if let Err(ref e) = send_result {
            self.mark_disconnected_if_fatal(e);
        } else {
            self.frames_sent += 1;
            debug!(
                "Frame sent: {}x{}, cmd={}, {} bytes",
                log_info.0, log_info.1, log_info.2, log_info.3
            );
        }
        send_result
    }

    fn close(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.release_interface(0);
            info!("BulkUsb closed: {} frames sent", self.frames_sent);
        }
        self.info = None;
    }

    fn is_connected(&self) -> bool {
        self.handle.is_some() && self.info.is_some()
    }

    fn try_reconnect(&mut self) -> Result<DeviceInfo> {
        self.close();
        let mut new = BulkUsb::new()?;
        let info = new.handshake()?;
        self.handle = new.handle.take(); // take() avoids moving out of Drop type
        self.ep_out = new.ep_out;
        self.ep_in = new.ep_in;
        self.info = Some(info.clone());
        Ok(info)
    }
}

impl Drop for BulkUsb {
    fn drop(&mut self) {
        self.close();
    }
}
