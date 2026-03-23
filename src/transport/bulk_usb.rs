use std::time::Duration;
use anyhow::{Context, Result, bail};
use log::{debug, info};
use rusb::{DeviceHandle, GlobalContext};

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
        10 | 11 | 12 | 13 | 14 | 15 | 16 | 17 => (320, 240),
        32 => (480, 480),
        50 => (240, 320),
        64 | 65 | 66 => (320, 320),
        68 | 69 => (480, 480),
        _ => (480, 480), // Default for unknown PMs (including PM=4)
    }
}

pub struct BulkUsb {
    handle: Option<DeviceHandle<GlobalContext>>,
    ep_out: u8,
    ep_in: u8,
    info: Option<DeviceInfo>,
}

impl BulkUsb {
    pub fn new() -> Result<Self> {
        let handle = rusb::open_device_with_vid_pid(VID, PID)
            .context("USB device 87AD:70DB not found")?;

        handle.set_auto_detach_kernel_driver(true)
            .context("Failed to set auto-detach kernel driver")?;

        handle.claim_interface(0)
            .context("Failed to claim USB interface 0")?;

        // Discover bulk endpoints
        let device = handle.device();
        let config = device.active_config_descriptor()
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

        info!("Opened BulkUSB device {:04x}:{:04x} (EP OUT=0x{:02x}, EP IN=0x{:02x})",
              VID, PID, ep_out, ep_in);

        Ok(Self {
            handle: Some(handle),
            ep_out,
            ep_in,
            info: None,
        })
    }
}

impl Transport for BulkUsb {
    fn handshake(&mut self) -> Result<DeviceInfo> {
        let handle = self.handle.as_ref().context("Device not open")?;

        // Write handshake
        let payload = handshake_payload();
        handle.write_bulk(self.ep_out, &payload, TIMEOUT)
            .context("Handshake write failed")?;
        debug!("Handshake sent ({} bytes)", payload.len());

        // Read response
        let mut resp = [0u8; HANDSHAKE_READ_SIZE];
        let n = handle.read_bulk(self.ep_in, &mut resp, TIMEOUT)
            .context("Handshake read failed")?;
        info!("Handshake response: {} bytes", n);

        if n < 41 || resp[24] == 0 {
            bail!("Handshake failed: resp[24]={} (expected non-zero)", resp[24]);
        }

        let pm = resp[24];
        let sub = resp[36];
        let (width, height) = pm_to_resolution(pm);
        let use_jpeg = pm != 32;

        info!("Handshake OK: PM={}, SUB={}, resolution={}x{}, jpeg={}",
              pm, sub, width, height, use_jpeg);

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
        let handle = self.handle.as_ref().context("Device not open")?;
        let info = self.info.as_ref().context("Handshake not performed")?;

        let cmd: u32 = if info.use_jpeg { 2 } else { 3 };
        let payload_len = u32::try_from(data.len()).context("frame too large")?;
        let header = build_frame_header(cmd, info.width, info.height, payload_len);

        // Concatenate header + payload
        let mut frame = Vec::with_capacity(64 + data.len());
        frame.extend_from_slice(&header);
        frame.extend_from_slice(data);

        // Send in 16KB chunks
        for chunk in frame.chunks(CHUNK_SIZE) {
            handle.write_bulk(self.ep_out, chunk, WRITE_TIMEOUT)
                .context("Bulk write failed")?;
        }

        // ZLP if total is 512-aligned
        if frame.len() % 512 == 0 {
            handle.write_bulk(self.ep_out, &[], WRITE_TIMEOUT)
                .context("ZLP write failed")?;
        }

        debug!("Frame sent: {}x{}, cmd={}, {} bytes",
               info.width, info.height, cmd, data.len());
        Ok(())
    }

    fn close(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.release_interface(0);
            info!("BulkUSB device closed");
        }
        self.info = None;
    }
}

impl Drop for BulkUsb {
    fn drop(&mut self) {
        self.close();
    }
}
