// SPDX-License-Identifier: GPL-3.0-or-later
//
// LY / LY1 bulk LCD transport (Trofeo Vision 9.16).
// Protocol derived from thermalright-trcc-linux at tree
// 390b880abd4cf0ed2d6eae7151493432263eff39 (project version 9.8.6, four commits after the v9.8.6 tag),
// path: src/trcc/adapters/device/ly_lcd.py

use super::usb_device::find_device;
use anyhow::{Context, Result, bail};
use log::{info, warn};
use rusb::{DeviceHandle, GlobalContext};
use std::time::Duration;

use super::profile::{WireProtocol, build_device_info, pm_to_fbl};
use super::{DeviceInfo, EncodedFrame, Transport};

const PID_LY: u16 = 0x5408;
const PID_LY1: u16 = 0x5409;

const HANDSHAKE_READ_SIZE: usize = 512;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(1);

const CHUNK_SIZE: usize = 512;
const CHUNK_HEADER_SIZE: usize = 16;
const CHUNK_DATA_SIZE: usize = 496;
const USB_WRITE_SIZE: usize = 4096;

/// 2048-byte 02FF handshake with byte8=1.
pub fn handshake_payload() -> Vec<u8> {
    let mut payload = vec![0u8; 2048];
    payload[0] = 0x02;
    payload[1] = 0xFF;
    payload[8] = 0x01;
    payload
}

/// Pack JPEG payload into LY/LY1 chunk records.
///
/// `num_chunks = floor(n/496) + 1` (retains zero-data terminal when divisible).
/// 5408 (command 1) pads record count to multiple of 4; 5409 (command 2) does not.
pub fn pack_ly_payload(payload: &[u8], pid: u16) -> Result<Vec<u8>> {
    let total_size = payload.len();
    let num_chunks = total_size / CHUNK_DATA_SIZE + 1;
    let last_chunk_data = total_size % CHUNK_DATA_SIZE;
    let chunk_cmd: u8 = if pid == PID_LY { 1 } else { 2 };

    let mut chunks = vec![0u8; num_chunks * CHUNK_SIZE];
    for i in 0..num_chunks {
        let offset = i * CHUNK_SIZE;
        let is_last = i + 1 == num_chunks;
        let data_len = if is_last {
            last_chunk_data
        } else {
            CHUNK_DATA_SIZE
        };

        chunks[offset] = 0x01;
        chunks[offset + 1] = 0xFF;
        chunks[offset + 2..offset + 6].copy_from_slice(&(total_size as u32).to_le_bytes());
        chunks[offset + 6..offset + 8].copy_from_slice(&(data_len as u16).to_le_bytes());
        chunks[offset + 8] = chunk_cmd;
        chunks[offset + 9..offset + 11].copy_from_slice(&(num_chunks as u16).to_le_bytes());
        chunks[offset + 11..offset + 13].copy_from_slice(&(i as u16).to_le_bytes());

        let src_offset = i * CHUNK_DATA_SIZE;
        if data_len > 0 {
            chunks[offset + CHUNK_HEADER_SIZE..offset + CHUNK_HEADER_SIZE + data_len]
                .copy_from_slice(&payload[src_offset..src_offset + data_len]);
        }
    }

    let pad_multiple = if pid == PID_LY { 4 } else { 1 };
    let mut padded_chunks = num_chunks;
    let remainder = padded_chunks % pad_multiple;
    if remainder != 0 {
        padded_chunks += pad_multiple - remainder;
    }
    let total_bytes = padded_chunks * CHUNK_SIZE;
    chunks.resize(total_bytes, 0);
    Ok(chunks)
}

/// Compute write burst sizes for a packed buffer.
pub fn ly_write_plan(total_bytes: usize, pid: u16) -> Vec<usize> {
    let mut plan = Vec::new();
    let mut pos = 0;
    while pos < total_bytes {
        let remaining = total_bytes - pos;
        let write_size = if remaining >= USB_WRITE_SIZE {
            USB_WRITE_SIZE
        } else if pid == PID_LY {
            remaining.min(2048)
        } else {
            remaining
        };
        plan.push(write_size);
        // Upstream advances by USB_WRITE_SIZE even when the tail write is shorter.
        pos += USB_WRITE_SIZE;
        if write_size < USB_WRITE_SIZE {
            break;
        }
    }
    // Fix plan to cover exact buffer for LY1 actual tails / LY 2048 tail.
    // Recompute without the buggy advance for safety:
    plan.clear();
    let mut pos = 0;
    while pos < total_bytes {
        let remaining = total_bytes - pos;
        let write_size = if remaining >= USB_WRITE_SIZE {
            USB_WRITE_SIZE
        } else if pid == PID_LY {
            remaining.min(2048)
        } else {
            remaining
        };
        plan.push(write_size);
        pos += write_size;
    }
    plan
}

/// Injectable LY bulk I/O.
pub trait LyIo: Send {
    fn write(&mut self, data: &[u8]) -> Result<()>;
    fn read(&mut self, max_len: usize) -> Result<Vec<u8>>;
}

/// LY handshake control flow over injectable I/O.
pub fn handshake_ly_with_io(io: &mut dyn LyIo, vid: u16, pid: u16) -> Result<DeviceInfo> {
    let payload = handshake_payload();
    io.write(&payload).context("LY handshake write failed")?;
    let resp = io
        .read(HANDSHAKE_READ_SIZE)
        .context("LY handshake read failed")?;
    let (pm, sub) = parse_ly_pm_sub(pid, &resp)?;
    let fbl = pm_to_fbl(pm, sub);
    build_device_info(WireProtocol::Ly, vid, pid, pm, sub, Some(fbl))
}

fn validate_rgb565_payload(frame: &EncodedFrame) -> Result<()> {
    if !frame.encoding.is_rgb565() {
        return Ok(());
    }
    let expected_len = usize::try_from(frame.width)
        .ok()
        .and_then(|width| {
            usize::try_from(frame.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(2))
        .context("LY RGB565 wire payload size overflow")?;
    if frame.data.len() != expected_len {
        bail!(
            "RGB565 payload length {} does not match {}x{} wire frame ({} bytes)",
            frame.data.len(),
            frame.width,
            frame.height,
            expected_len
        );
    }
    Ok(())
}

/// LY frame send control flow over injectable I/O.
pub fn send_ly_with_io(io: &mut dyn LyIo, pid: u16, frame: &EncodedFrame) -> Result<()> {
    validate_rgb565_payload(frame)?;
    let send_buf = pack_ly_payload(&frame.data, pid)?;
    let plan = ly_write_plan(send_buf.len(), pid);
    let mut pos = 0usize;
    for write_size in plan {
        let end = pos + write_size;
        io.write(&send_buf[pos..end])
            .context("LY frame write failed")?;
        pos = end;
    }
    let _ack = io.read(HANDSHAKE_READ_SIZE).context("LY ACK read failed")?;
    Ok(())
}

fn validate_ly_frame(info: &DeviceInfo, frame: &EncodedFrame) -> Result<()> {
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
    validate_rgb565_payload(frame)?;
    Ok(())
}

pub struct LyLcd {
    handle: Option<DeviceHandle<GlobalContext>>,
    vid: u16,
    pid: u16,
    interface: u8,
    ep_out: u8,
    ep_in: u8,
    info: Option<DeviceInfo>,
}

impl LyLcd {
    pub fn open(
        pid: u16,
        bus: u8,
        address: u8,
        interface: u8,
        ep_in: u8,
        ep_out: u8,
    ) -> Result<Self> {
        if !matches!(pid, PID_LY | PID_LY1) {
            bail!("unsupported LY PID {pid:04x}");
        }
        let device = find_device(bus, address)?;
        let desc = device.device_descriptor().context("device descriptor")?;
        let vid = desc.vendor_id();
        let actual_pid = desc.product_id();
        if actual_pid != pid {
            bail!("LY PID mismatch: expected {pid:04x}, got {actual_pid:04x}");
        }

        let handle = device.open().with_context(|| {
            format!("Failed to open LY {vid:04x}:{pid:04x} (check udev rules and replug)")
        })?;
        handle
            .set_auto_detach_kernel_driver(true)
            .context("auto-detach kernel driver")?;
        handle
            .claim_interface(interface)
            .with_context(|| format!("claim interface {interface}"))?;

        info!(
            "Opened LY {:04x}:{:04x} bus={} addr={} OUT=0x{:02x} IN=0x{:02x}",
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
        })
    }

    fn mark_disconnected(&mut self) {
        self.handle = None;
        self.info = None;
    }

    fn mark_disconnected_if_fatal(&mut self, err: &anyhow::Error) {
        let is_fatal = super::bulk_usb::is_fatal_usb_transfer(err);
        if is_fatal {
            warn!("Fatal LY USB error — disconnecting: {err}");
            self.mark_disconnected();
        }
    }
}

/// Extract PM/SUB from a validated LY handshake response.
pub fn parse_ly_pm_sub(pid: u16, resp: &[u8]) -> Result<(u8, u8)> {
    if resp.len() < 37 || resp[0] != 3 || resp[1] != 0xFF || resp[8] != 1 {
        bail!(
            "LY handshake validation failed (len={}, [0]={}, [1]={}, [8]={})",
            resp.len(),
            resp.first().map(|b| *b as i32).unwrap_or(-1),
            resp.get(1).map(|b| *b as i32).unwrap_or(-1),
            resp.get(8).map(|b| *b as i32).unwrap_or(-1)
        );
    }
    if pid == PID_LY {
        let mut raw = resp[20];
        if raw <= 3 {
            raw = 1;
        }
        let pm = 64u8.saturating_add(raw);
        let sub = resp.get(22).copied().unwrap_or(0).saturating_add(1);
        Ok((pm, sub))
    } else {
        let pm = 50u8.saturating_add(resp[36]);
        let sub = resp.get(22).copied().unwrap_or(0);
        Ok((pm, sub))
    }
}

impl Transport for LyLcd {
    fn handshake(&mut self) -> Result<DeviceInfo> {
        let handle = self.handle.as_ref().context("LY device not open")?;
        let payload = handshake_payload();
        handle
            .write_bulk(self.ep_out, &payload, HANDSHAKE_TIMEOUT)
            .context("LY handshake write failed")?;

        let mut resp = vec![0u8; HANDSHAKE_READ_SIZE];
        let n = handle
            .read_bulk(self.ep_in, &mut resp, HANDSHAKE_TIMEOUT)
            .context("LY handshake read failed")?;
        resp.truncate(n);

        let (pm, sub) = parse_ly_pm_sub(self.pid, &resp)?;
        let fbl = pm_to_fbl(pm, sub);
        let info = build_device_info(WireProtocol::Ly, self.vid, self.pid, pm, sub, Some(fbl))?;

        info!(
            "LY handshake OK: PM={} SUB={} FBL={} {}x{} (pid=0x{:04x})",
            pm,
            sub,
            info.fbl,
            info.width(),
            info.height(),
            self.pid
        );
        self.info = Some(info.clone());
        Ok(info)
    }

    fn send_frame(&mut self, _frame: &EncodedFrame) -> Result<()> {
        anyhow::bail!("LY has no evidence-backed exact production output policy");
    }

    fn close(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.release_interface(self.interface);
            info!("LyLcd closed");
        }
        self.info = None;
    }

    fn is_connected(&self) -> bool {
        self.handle.is_some() && self.info.is_some()
    }
}

impl Drop for LyLcd {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_payload_is_2048_with_markers() {
        let p = handshake_payload();
        assert_eq!(p.len(), 2048);
        assert_eq!(p[0], 0x02);
        assert_eq!(p[1], 0xFF);
        assert_eq!(p[8], 0x01);
    }

    #[test]
    fn payload_boundaries_include_zero_terminal_on_496() {
        for n in [495usize, 496, 497] {
            let data = vec![0xABu8; n];
            let packed = pack_ly_payload(&data, PID_LY1).unwrap();
            let num_chunks = n / 496 + 1;
            // LY1 no padding
            assert_eq!(packed.len(), num_chunks * 512, "n={n}");
            if n == 496 {
                // terminal record has data_len 0
                let last = &packed[(num_chunks - 1) * 512..];
                let data_len = u16::from_le_bytes([last[6], last[7]]);
                assert_eq!(data_len, 0);
            }
        }
    }

    #[test]
    fn ly_pads_record_count_to_multiple_of_4() {
        let data = vec![0u8; 10];
        let packed = pack_ly_payload(&data, PID_LY).unwrap();
        // 1 data chunk -> pad to 4
        assert_eq!(packed.len(), 4 * 512);
    }

    #[test]
    fn ly1_no_record_padding() {
        let data = vec![0u8; 10];
        let packed = pack_ly_payload(&data, PID_LY1).unwrap();
        assert_eq!(packed.len(), 512);
    }

    #[test]
    fn write_plan_ly_uses_4096_and_2048_tail() {
        let plan = ly_write_plan(4096 + 100, PID_LY);
        assert_eq!(plan, vec![4096, 100]);
        let plan = ly_write_plan(4096 + 3000, PID_LY);
        assert_eq!(plan, vec![4096, 2048, 952]);
    }

    #[test]
    fn rgb565_frame_requires_exact_wire_payload_size() {
        let info =
            crate::transport::device_info_from_fixture("ly-0416-5409-pm50-sub0-fbl50").unwrap();
        let (width, height) = info.wire_dimensions().unwrap();
        let expected_len = width as usize * height as usize * 2;
        let frame = |len| EncodedFrame {
            data: vec![0; len],
            width,
            height,
            encoding: info.encoding(),
        };

        validate_ly_frame(&info, &frame(expected_len)).expect("exact payload should be accepted");
        for invalid_len in [expected_len - 1, expected_len + 1] {
            let error = validate_ly_frame(&info, &frame(invalid_len)).unwrap_err();
            assert!(
                error.to_string().contains(&format!(
                    "RGB565 payload length {invalid_len} does not match {width}x{height} wire frame"
                )),
                "{error:#}"
            );
        }
    }

    #[test]
    fn jpeg_frame_remains_variable_length() {
        let info =
            crate::transport::device_info_from_fixture("ly-0416-5408-pm65-sub3-fbl192").unwrap();
        let (width, height) = info.wire_dimensions().unwrap();
        let frame = EncodedFrame {
            data: vec![0xff],
            width,
            height,
            encoding: info.encoding(),
        };

        validate_ly_frame(&info, &frame).expect("JPEG payload length is variable");
    }

    #[test]
    fn write_plan_ly1_uses_actual_tail() {
        let plan = ly_write_plan(4096 + 123, PID_LY1);
        assert_eq!(plan, vec![4096, 123]);
    }
}
