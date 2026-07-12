// SPDX-License-Identifier: GPL-3.0-or-later
//
// HID LCD Type 2 / Type 3 transports.
// Protocol derived from thermalright-trcc-linux at tree
// 390b880abd4cf0ed2d6eae7151493432263eff39 (project version 9.8.6, four commits after the v9.8.6 tag),
// path: src/trcc/adapters/device/hid_lcd.py

use anyhow::{Context, Result, bail};
use log::{debug, info, warn};
use rusb::{DeviceHandle, GlobalContext};
use std::time::Duration;

use super::profile::{WireProtocol, build_device_info};
use super::{DeviceInfo, EncodedFrame, FrameEncoding, Transport};

const EP_READ_DEFAULT: u8 = 0x81;
const EP_WRITE_DEFAULT: u8 = 0x02;

const TYPE2_MAGIC: [u8; 4] = [0xDA, 0xDB, 0xDC, 0xDD];
const TYPE2_INIT_SIZE: usize = 512;
const TYPE2_RESPONSE_SIZE: usize = 512;

const TYPE3_CMD_PREFIX: [u8; 8] = [0xF5, 0x00, 0x01, 0x00, 0xBC, 0xFF, 0xB6, 0xC8];
const TYPE3_FRAME_PREFIX: [u8; 8] = [0xF5, 0x01, 0x01, 0x00, 0xBC, 0xFF, 0xB6, 0xC8];
const TYPE3_INIT_SIZE: usize = 1040;
const TYPE3_RESPONSE_SIZE: usize = 1024;
const TYPE3_DATA_SIZE: usize = 204_800; // 320*320*2
const TYPE3_ACK_SIZE: usize = 16;

const USB_BULK_ALIGNMENT: usize = 512;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_MAX_RETRIES: u32 = 3;
const HANDSHAKE_RETRY_DELAY: Duration = Duration::from_millis(500);
const DELAY_PRE_INIT: Duration = Duration::from_millis(50);
const DELAY_POST_INIT: Duration = Duration::from_millis(200);
const DELAY_FRAME_TYPE2: Duration = Duration::from_millis(1);
const DEFAULT_FRAME_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HidType {
    Type2,
    Type3,
}

/// Injectable HID bulk I/O.
pub trait HidIo: Send {
    fn write(&mut self, data: &[u8]) -> Result<()>;
    fn read(&mut self, max_len: usize) -> Result<Vec<u8>>;
    fn sleep(&mut self, d: std::time::Duration);
}

/// Type2 handshake control flow over injectable I/O (retries included).
pub fn handshake_type2_with_io(io: &mut dyn HidIo, vid: u16, pid: u16) -> Result<DeviceInfo> {
    let init = build_init_packet_type2();
    let mut last_err = None;
    for attempt in 1..=HANDSHAKE_MAX_RETRIES {
        io.sleep(DELAY_PRE_INIT);
        if let Err(e) = io.write(&init) {
            last_err = Some(anyhow::anyhow!("HID init write failed: {e}"));
            io.sleep(HANDSHAKE_RETRY_DELAY);
            continue;
        }
        io.sleep(DELAY_POST_INIT);
        match io.read(TYPE2_RESPONSE_SIZE) {
            Ok(resp) if validate_response_type2(&resp) => {
                let pm = resp[5];
                let sub = resp[4];
                return build_device_info(WireProtocol::HidType2, vid, pid, pm, sub, None);
            }
            Ok(resp) => {
                last_err = Some(anyhow::anyhow!(
                    "invalid HID Type2 response attempt {attempt} len={}",
                    resp.len()
                ));
            }
            Err(e) => last_err = Some(anyhow::anyhow!("HID init read failed: {e}")),
        }
        io.sleep(HANDSHAKE_RETRY_DELAY);
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("HID Type2 handshake failed")))
}

pub struct HidLcd {
    handle: Option<DeviceHandle<GlobalContext>>,
    vid: u16,
    pid: u16,
    interface: u8,
    ep_out: u8,
    ep_in: u8,
    kind: HidType,
    info: Option<DeviceInfo>,
}

impl HidLcd {
    pub fn open_type2(bus: u8, address: u8, interface: u8, ep_in: u8, ep_out: u8) -> Result<Self> {
        Self::open(HidType::Type2, 0, bus, address, interface, ep_in, ep_out)
    }

    pub fn open_type3(
        pid: u16,
        bus: u8,
        address: u8,
        interface: u8,
        ep_in: u8,
        ep_out: u8,
    ) -> Result<Self> {
        Self::open(HidType::Type3, pid, bus, address, interface, ep_in, ep_out)
    }

    fn open(
        kind: HidType,
        expected_pid: u16,
        bus: u8,
        address: u8,
        interface: u8,
        ep_in: u8,
        ep_out: u8,
    ) -> Result<Self> {
        let device = find_device(bus, address)?;
        let desc = device.device_descriptor().context("device descriptor")?;
        let vid = desc.vendor_id();
        let pid = desc.product_id();
        if expected_pid != 0 && pid != expected_pid {
            bail!("HID PID mismatch: expected {expected_pid:04x}, got {pid:04x}");
        }

        let handle = device.open().with_context(|| {
            format!(
                "Failed to open HID {:04x}:{pid:04x} (check udev rules and replug)",
                vid
            )
        })?;
        handle
            .set_auto_detach_kernel_driver(true)
            .context("auto-detach kernel driver")?;
        handle
            .claim_interface(interface)
            .with_context(|| format!("claim interface {interface}"))?;

        let ep_out = if ep_out == 0 {
            EP_WRITE_DEFAULT
        } else {
            ep_out
        };
        let ep_in = if ep_in == 0 { EP_READ_DEFAULT } else { ep_in };

        info!(
            "Opened HID {:?} {:04x}:{:04x} bus={} addr={} OUT=0x{:02x} IN=0x{:02x}",
            kind, vid, pid, bus, address, ep_out, ep_in
        );

        Ok(Self {
            handle: Some(handle),
            vid,
            pid,
            interface,
            ep_out,
            ep_in,
            kind,
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
            warn!("Fatal HID USB error — disconnecting: {err}");
            self.mark_disconnected();
        }
    }
}

fn find_device(bus: u8, address: u8) -> Result<rusb::Device<GlobalContext>> {
    let list = rusb::devices().context("libusb device list failed")?;
    for device in list.iter() {
        if device.bus_number() == bus && device.address() == address {
            return Ok(device);
        }
    }
    bail!("no USB device at bus={bus} address={address}")
}

/// Type 2 512-byte handshake: magic + command=1, zero-padded.
pub fn build_init_packet_type2() -> Vec<u8> {
    let mut pkt = vec![0u8; TYPE2_INIT_SIZE];
    pkt[0..4].copy_from_slice(&TYPE2_MAGIC);
    pkt[12] = 0x01; // command = 1 at offset 12 (after magic + 8 zeros)
    pkt
}

/// Type 3 1040-byte handshake: F5 prefix + 16-byte header + 1024 zeros.
pub fn build_init_packet_type3() -> Vec<u8> {
    let mut pkt = vec![0u8; TYPE3_INIT_SIZE];
    pkt[0..8].copy_from_slice(&TYPE3_CMD_PREFIX);
    // bytes 8..12 zeros, 12..16 = 0x00000400 LE (1024)
    pkt[12..16].copy_from_slice(&1024u32.to_le_bytes());
    pkt
}

pub fn validate_response_type2(resp: &[u8]) -> bool {
    resp.len() >= 20 && resp[0..4] == TYPE2_MAGIC && resp[12] == 0x01
}

pub fn validate_response_type3(resp: &[u8]) -> bool {
    resp.len() >= 14 && matches!(resp[0], 0x65 | 0x66)
}

/// Type 2 frame: 20-byte header + image data, 512-aligned.
pub fn build_frame_type2(image_data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let is_jpeg = image_data.len() >= 2 && image_data[0] == 0xFF && image_data[1] == 0xD8;
    let mut header = Vec::with_capacity(20);
    header.extend_from_slice(&TYPE2_MAGIC);
    header.extend_from_slice(&[0x02, 0x00]); // PICTURE
    if is_jpeg {
        header.extend_from_slice(&[0x00, 0x00]);
        header.extend_from_slice(&(width as u16).to_le_bytes());
        header.extend_from_slice(&(height as u16).to_le_bytes());
    } else {
        header.extend_from_slice(&[0x01, 0x00]);
        header.extend_from_slice(&240u16.to_le_bytes());
        header.extend_from_slice(&320u16.to_le_bytes());
    }
    header.extend_from_slice(&[0x02, 0x00, 0x00, 0x00]);
    header.extend_from_slice(&(image_data.len() as u32).to_le_bytes());

    let mut raw = header;
    raw.extend_from_slice(image_data);
    let aligned = raw.len().div_ceil(USB_BULK_ALIGNMENT) * USB_BULK_ALIGNMENT;
    raw.resize(aligned, 0);
    raw
}

/// Type 3 frame: 16-byte prefix + exactly 204800 RGB565 bytes.
pub fn build_frame_type3(image_data: &[u8]) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(16 + TYPE3_DATA_SIZE);
    pkt.extend_from_slice(&TYPE3_FRAME_PREFIX);
    pkt.extend_from_slice(&[0, 0, 0, 0]);
    pkt.extend_from_slice(&(TYPE3_DATA_SIZE as u32).to_le_bytes());
    if image_data.len() < TYPE3_DATA_SIZE {
        pkt.extend_from_slice(image_data);
        pkt.resize(16 + TYPE3_DATA_SIZE, 0);
    } else {
        pkt.extend_from_slice(&image_data[..TYPE3_DATA_SIZE]);
    }
    pkt
}

fn frame_timeout(packet_size: usize) -> Duration {
    let ms = (packet_size / 4 + 100).max(100) as u64;
    Duration::from_millis(ms)
}

impl Transport for HidLcd {
    fn handshake(&mut self) -> Result<DeviceInfo> {
        let init = match self.kind {
            HidType::Type2 => build_init_packet_type2(),
            HidType::Type3 => build_init_packet_type3(),
        };
        let response_size = match self.kind {
            HidType::Type2 => TYPE2_RESPONSE_SIZE,
            HidType::Type3 => TYPE3_RESPONSE_SIZE,
        };

        let mut last_err = None;
        for attempt in 1..=HANDSHAKE_MAX_RETRIES {
            let handle = self.handle.as_ref().context("HID device not open")?;
            std::thread::sleep(DELAY_PRE_INIT);
            if let Err(e) = handle.write_bulk(self.ep_out, &init, HANDSHAKE_TIMEOUT) {
                last_err = Some(anyhow::anyhow!("HID init write failed: {e}"));
                std::thread::sleep(HANDSHAKE_RETRY_DELAY);
                continue;
            }
            std::thread::sleep(DELAY_POST_INIT);
            let mut resp = vec![0u8; response_size];
            let n = match handle.read_bulk(self.ep_in, &mut resp, HANDSHAKE_TIMEOUT) {
                Ok(n) => n,
                Err(e) => {
                    last_err = Some(anyhow::anyhow!("HID init read failed: {e}"));
                    std::thread::sleep(HANDSHAKE_RETRY_DELAY);
                    continue;
                }
            };
            resp.truncate(n);

            let valid = match self.kind {
                HidType::Type2 => validate_response_type2(&resp),
                HidType::Type3 => validate_response_type3(&resp),
            };
            if !valid {
                last_err = Some(anyhow::anyhow!(
                    "invalid HID {:?} handshake response (attempt {attempt}/{HANDSHAKE_MAX_RETRIES}, len={n})",
                    self.kind
                ));
                std::thread::sleep(HANDSHAKE_RETRY_DELAY);
                continue;
            }

            let info = match self.kind {
                HidType::Type2 => {
                    let pm = resp[5];
                    let sub = resp[4];
                    build_device_info(WireProtocol::HidType2, self.vid, self.pid, pm, sub, None)?
                }
                HidType::Type3 => {
                    let fbl = resp[0].saturating_sub(1);
                    build_device_info(
                        WireProtocol::HidType3,
                        self.vid,
                        self.pid,
                        fbl,
                        0,
                        Some(fbl),
                    )?
                }
            };

            info!(
                "HID {:?} handshake OK: PM={} SUB={} {}x{} {}",
                self.kind,
                info.pm,
                info.sub,
                info.width(),
                info.height(),
                info.encoding()
            );
            self.info = Some(info.clone());
            return Ok(info);
        }

        self.mark_disconnected();
        Err(last_err.unwrap_or_else(|| {
            anyhow::anyhow!("HID handshake failed after {HANDSHAKE_MAX_RETRIES} attempts")
        }))
    }

    fn send_frame(&mut self, frame: &EncodedFrame) -> Result<()> {
        let info = self.info.as_ref().context("Handshake not performed")?;
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
        let packet = match self.kind {
            HidType::Type2 => build_frame_type2(&frame.data, frame.width, frame.height),
            HidType::Type3 => {
                if !matches!(
                    frame.encoding,
                    FrameEncoding::Rgb565Le | FrameEncoding::Rgb565Be
                ) {
                    bail!("HID Type3 requires RGB565, got {}", frame.encoding);
                }
                build_frame_type3(&frame.data)
            }
        };
        let timeout = frame_timeout(packet.len());

        let send_result: Result<()> = {
            let handle = self.handle.as_ref().context("HID device not open")?;
            super::bulk_usb::write_all(&packet, |remaining| {
                handle.write_bulk(self.ep_out, remaining, timeout)
            })
            .context("HID frame write failed")?;
            match self.kind {
                HidType::Type2 => {
                    std::thread::sleep(DELAY_FRAME_TYPE2);
                    Ok(())
                }
                HidType::Type3 => {
                    let mut ack = [0u8; TYPE3_ACK_SIZE];
                    let an = handle
                        .read_bulk(self.ep_in, &mut ack, DEFAULT_FRAME_TIMEOUT)
                        .context("HID Type3 ACK read failed")?;
                    if an == 0 {
                        bail!("HID Type3 ACK empty");
                    }
                    Ok(())
                }
            }
        };

        if let Err(e) = &send_result {
            self.mark_disconnected_if_fatal(e);
        } else {
            debug!("HID {:?} frame sent ({} bytes)", self.kind, packet.len());
        }
        send_result
    }

    fn close(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.release_interface(self.interface);
            info!("HidLcd closed");
        }
        self.info = None;
    }

    fn is_connected(&self) -> bool {
        self.handle.is_some() && self.info.is_some()
    }
}

impl Drop for HidLcd {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type2_init_has_magic_and_cmd() {
        let pkt = build_init_packet_type2();
        assert_eq!(pkt.len(), 512);
        assert_eq!(&pkt[0..4], &TYPE2_MAGIC);
        assert_eq!(pkt[12], 1);
    }

    #[test]
    fn type3_init_size_and_prefix() {
        let pkt = build_init_packet_type3();
        assert_eq!(pkt.len(), 1040);
        assert_eq!(&pkt[0..8], &TYPE3_CMD_PREFIX);
    }

    #[test]
    fn type2_frame_aligns_to_512() {
        let data = vec![0xFFu8, 0xD8, 0x00];
        let pkt = build_frame_type2(&data, 320, 240);
        assert_eq!(pkt.len() % 512, 0);
        assert_eq!(&pkt[0..4], &TYPE2_MAGIC);
    }

    #[test]
    fn type3_frame_is_fixed_size() {
        let data = vec![0u8; 100];
        let pkt = build_frame_type3(&data);
        assert_eq!(pkt.len(), 16 + TYPE3_DATA_SIZE);
    }
}
