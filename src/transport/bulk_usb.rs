// SPDX-License-Identifier: GPL-3.0-or-later
//
// Raw bulk USBLCDNew transport for Thermalright LCD coolers.
// Protocol framing derived from thermalright-trcc-linux at tree
// 390b880abd4cf0ed2d6eae7151493432263eff39 (project version 9.8.6, four commits after the v9.8.6 tag),
// path: src/trcc/adapters/device/bulk_lcd.py

use super::usb_device::find_device;
use anyhow::{Context, Result, bail};
use log::{debug, info, warn};
use rusb::{DeviceHandle, GlobalContext};
use std::time::Duration;

use super::profile::{WireProtocol, build_device_info};
use super::{DeviceInfo, EncodedFrame, FrameEncoding, Transport};

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

#[derive(Debug, thiserror::Error)]
#[error("device returned zero-length USB write after {bytes_written} bytes")]
pub(crate) struct ZeroLengthTransfer {
    bytes_written: usize,
}

pub(crate) fn is_fatal_usb_transfer(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause.is::<ZeroLengthTransfer>()
            || cause
                .downcast_ref::<rusb::Error>()
                .is_some_and(|error| !matches!(error, rusb::Error::Timeout))
    })
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
            return Err(ZeroLengthTransfer {
                bytes_written: sent,
            }
            .into());
        }
        sent += n;
    }
    Ok(())
}

/// Injectable count-aware bulk endpoint I/O for tests and the rusb backend.
pub trait BulkIo: Send {
    fn write(&mut self, data: &[u8]) -> Result<usize>;
    fn read(&mut self, max_len: usize) -> Result<Vec<u8>>;
}

fn write_all_with_io(io: &mut dyn BulkIo, data: &[u8]) -> Result<()> {
    let mut sent = 0;
    while sent < data.len() {
        let remaining = &data[sent..];
        let written = io.write(remaining).context("Bulk write failed")?;
        if written == 0 {
            return Err(ZeroLengthTransfer {
                bytes_written: sent,
            }
            .into());
        }
        if written > remaining.len() {
            bail!(
                "bulk writer reported {written} bytes for a {}-byte buffer",
                remaining.len()
            );
        }
        sent += written;
    }
    Ok(())
}

struct UsbBulkIo<'a> {
    handle: &'a DeviceHandle<GlobalContext>,
    ep_out: u8,
    ep_in: u8,
    write_timeout: Duration,
}

impl BulkIo for UsbBulkIo<'_> {
    fn write(&mut self, data: &[u8]) -> Result<usize> {
        self.handle
            .write_bulk(self.ep_out, data, self.write_timeout)
            .context("USB bulk write failed")
    }

    fn read(&mut self, max_len: usize) -> Result<Vec<u8>> {
        let mut data = vec![0; max_len];
        let read = self
            .handle
            .read_bulk(self.ep_in, &mut data, TIMEOUT)
            .context("USB bulk read failed")?;
        data.truncate(read);
        Ok(data)
    }
}

/// Pure handshake over injectable I/O — production and tests share this control flow.
pub fn handshake_with_io(io: &mut dyn BulkIo, vid: u16, pid: u16) -> Result<DeviceInfo> {
    let payload = handshake_payload();
    write_all_with_io(io, &payload).context("Handshake write failed")?;
    let resp = io
        .read(HANDSHAKE_READ_SIZE)
        .context("Handshake read failed")?;
    if resp.len() < 41 || resp[24] == 0 {
        bail!(
            "Handshake failed: resp[24]={} (expected non-zero)",
            resp.get(24).copied().unwrap_or(0)
        );
    }
    let pm = resp[24];
    let sub = resp[36];
    build_device_info(WireProtocol::Bulk, vid, pid, pm, sub, None)
}

/// Pure frame send over injectable I/O.
pub fn send_frame_with_io(
    io: &mut dyn BulkIo,
    info: &DeviceInfo,
    frame: &EncodedFrame,
) -> Result<()> {
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
    if frame.encoding != info.encoding() {
        bail!(
            "frame encoding {} does not match device {}",
            frame.encoding,
            info.encoding()
        );
    }
    let cmd: u32 = match frame.encoding {
        FrameEncoding::Jpeg => 2,
        FrameEncoding::Rgb565Le | FrameEncoding::Rgb565Be => 3,
    };
    let payload_len = u32::try_from(frame.data.len()).context("frame too large")?;
    let header = build_frame_header(cmd, frame.width, frame.height, payload_len);
    let mut wire = Vec::with_capacity(64 + frame.data.len());
    wire.extend_from_slice(&header);
    wire.extend_from_slice(&frame.data);
    for chunk in wire.chunks(CHUNK_SIZE) {
        write_all_with_io(io, chunk)?;
    }
    if wire.len() % 512 == 0 {
        io.write(&[])?; // ZLP
    }
    Ok(())
}

pub struct BulkUsb {
    handle: Option<DeviceHandle<GlobalContext>>,
    vid: u16,
    pid: u16,
    interface: u8,
    ep_out: u8,
    ep_in: u8,
    info: Option<DeviceInfo>,
    /// Total frames successfully sent over this transport's lifetime.
    frames_sent: u64,
}

impl BulkUsb {
    /// Open a bulk USB device at the given bus/address with discovered endpoints.
    pub fn open_at(bus: u8, address: u8, interface: u8, ep_in: u8, ep_out: u8) -> Result<Self> {
        let device = find_device(bus, address)
            .with_context(|| format!("USB device bus={bus} address={address} not found"))?;
        let desc = device
            .device_descriptor()
            .context("Failed to read USB device descriptor")?;
        let vid = desc.vendor_id();
        let pid = desc.product_id();

        let handle = device.open().with_context(|| {
            format!(
                "Failed to open USB {:04x}:{:04x} (check udev rules and replug)",
                vid, pid
            )
        })?;

        handle
            .set_auto_detach_kernel_driver(true)
            .context("Failed to set auto-detach kernel driver")?;
        handle
            .claim_interface(interface)
            .with_context(|| format!("Failed to claim USB interface {interface}"))?;

        if ep_out == 0 || ep_in == 0 {
            let _ = handle.release_interface(interface);
            bail!("Could not find bulk IN/OUT endpoints");
        }

        info!(
            "Opened BulkUSB device {:04x}:{:04x} bus={} addr={} (EP OUT=0x{:02x}, EP IN=0x{:02x})",
            vid, pid, bus, address, ep_out, ep_in
        );

        Ok(Self {
            handle: Some(handle),
            vid,
            pid,
            interface,
            ep_out,
            ep_in,
            info: None,
            frames_sent: 0,
        })
    }

    /// If `err` is a fatal USB error, drop the handle so is_connected() is false.
    fn mark_disconnected_if_fatal(&mut self, err: &anyhow::Error) {
        let is_fatal = is_fatal_usb_transfer(err);
        if is_fatal {
            warn!("Fatal USB error — marking device disconnected: {err}");
            self.handle = None;
            self.info = None;
        }
    }
}

impl Transport for BulkUsb {
    fn handshake(&mut self) -> Result<DeviceInfo> {
        let result = {
            let handle = self.handle.as_ref().context("Device not open")?;
            let mut io = UsbBulkIo {
                handle,
                ep_out: self.ep_out,
                ep_in: self.ep_in,
                write_timeout: TIMEOUT,
            };
            handshake_with_io(&mut io, self.vid, self.pid)
        };

        match result {
            Ok(info) => {
                info!(
                    "Handshake OK: PM={}, SUB={}, FBL={}, resolution={}x{}, encoding={}",
                    info.pm,
                    info.sub,
                    info.fbl,
                    info.width(),
                    info.height(),
                    info.encoding()
                );
                self.info = Some(info.clone());
                Ok(info)
            }
            Err(error) => {
                self.mark_disconnected_if_fatal(&error);
                Err(error)
            }
        }
    }

    fn send_frame(&mut self, frame: &EncodedFrame) -> Result<()> {
        let send_result = {
            let info = self.info.as_ref().context("Handshake not performed")?;
            let handle = self.handle.as_ref().context("Device not open")?;
            let mut io = UsbBulkIo {
                handle,
                ep_out: self.ep_out,
                ep_in: self.ep_in,
                write_timeout: WRITE_TIMEOUT,
            };
            send_frame_with_io(&mut io, info, frame)
        };

        match send_result {
            Ok(()) => {
                self.frames_sent += 1;
                debug!(
                    "Frame sent: {}x{}, encoding={}, {} bytes",
                    frame.width,
                    frame.height,
                    frame.encoding,
                    frame.data.len()
                );
                Ok(())
            }
            Err(error) => {
                self.mark_disconnected_if_fatal(&error);
                Err(error)
            }
        }
    }

    fn close(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.release_interface(self.interface);
            info!("BulkUsb closed: {} frames sent", self.frames_sent);
        }
        self.info = None;
    }

    fn is_connected(&self) -> bool {
        self.handle.is_some() && self.info.is_some()
    }
}

impl Drop for BulkUsb {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_length_write_is_typed_and_fatal() {
        let error = write_all(&[1], |_| Ok(0)).unwrap_err();
        assert!(error.downcast_ref::<ZeroLengthTransfer>().is_some());
        assert!(is_fatal_usb_transfer(&error));
    }

    #[test]
    fn timeout_is_not_a_fatal_transfer_error() {
        assert!(!is_fatal_usb_transfer(&rusb::Error::Timeout.into()));
        assert!(is_fatal_usb_transfer(&rusb::Error::NoDevice.into()));
    }
}
