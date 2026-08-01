// SPDX-License-Identifier: GPL-3.0-or-later
//
// HID LCD Type 2 / Type 3 transports.
// Protocol derived from thermalright-trcc-linux at tree
// 390b880abd4cf0ed2d6eae7151493432263eff39 (project version 9.8.6, four commits after the v9.8.6 tag),
// path: src/trcc/adapters/device/hid_lcd.py

use super::usb_device::find_device;
use anyhow::{Context, Result, bail};
use log::{debug, info, warn};
use rusb::{DeviceHandle, GlobalContext};
use std::time::Duration;

#[cfg(feature = "daemon")]
pub use super::hid_report::linux::{LinuxHidrawIo, open_correlated_read_session};
pub use super::hid_report::{
    HidChunkedWriteFailure, HidReadObservation, HidReportAuthorizeError, HidReportBackendContract,
    HidReportProbeError, HidReportReadError, HidReportReadSession, HidReportWriteAuthorization,
    HidReportWriteError, HidReportWriteSession, HidWriteObservation, HidrawCandidate,
    HidrawCorrelation, KERNEL_HIDRAW_DOC_REF, LINUX_HIDRAW_BACKEND_CONTRACT, PROTOCOL_CHUNK_BYTES,
    REPORT_ID_UNNUMBERED, REVIEWED_HIDAPI_EVIDENCE_COMMIT, USERSPACE_SUBMIT_BYTES, UsbBusAddress,
    authenticate_opened_hidraw, correlate_hidraw_to_usb,
};

use super::profile::{WireProtocol, build_device_info};
use super::type2_policy::{
    self, TYPE2_MAGIC, TYPE2_RESPONSE_SIZE, Type2NegotiatedObservation, negotiate_type2_policy,
};

use super::{DeviceInfo, EncodedFrame, FrameEncoding, Transport};
pub use type2_policy::{
    HidOutputRoute, TYPE2_PROBE_READ_BOUND, TYPE2_SHORT_RESPONSE_LEN, Type2NegotiatedPolicy,
    Type2PreHandshakePolicy, UPSTREAM_407_PM58_ISSUE, UPSTREAM_407_PM58_PR, parse_type2_pm_sub,
    select_type2_pre_handshake_policy, validate_short_response_type2,
};

const EP_READ_DEFAULT: u8 = 0x81;
const EP_WRITE_DEFAULT: u8 = 0x02;
const TYPE2_INIT_SIZE: usize = 512;

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

struct UsbHidIo<'a> {
    handle: &'a DeviceHandle<GlobalContext>,
    ep_out: u8,
    ep_in: u8,
    write_timeout: Option<Duration>,
    read_timeout: Duration,
}

impl HidIo for UsbHidIo<'_> {
    fn write(&mut self, data: &[u8]) -> Result<()> {
        let timeout = self
            .write_timeout
            .unwrap_or_else(|| frame_timeout(data.len()));
        // Prefer interrupt OUT (true HID Type2); fall back to bulk for dual-shape units.
        super::bulk_usb::write_all(data, |remaining| {
            match self.handle.write_interrupt(self.ep_out, remaining, timeout) {
                Ok(n) => Ok(n),
                Err(rusb::Error::InvalidParam) | Err(rusb::Error::NotFound) => {
                    self.handle.write_bulk(self.ep_out, remaining, timeout)
                }
                Err(e) => Err(e),
            }
        })
    }

    fn read(&mut self, max_len: usize) -> Result<Vec<u8>> {
        let mut data = vec![0; max_len];
        let len = match self
            .handle
            .read_interrupt(self.ep_in, &mut data, self.read_timeout)
        {
            Ok(n) => n,
            Err(rusb::Error::InvalidParam) | Err(rusb::Error::NotFound) => {
                self.handle
                    .read_bulk(self.ep_in, &mut data, self.read_timeout)?
            }
            Err(e) => return Err(e.into()),
        };
        data.truncate(len);
        Ok(data)
    }

    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Type2 handshake control flow over injectable I/O (retries included).
pub fn handshake_type2_with_io(io: &mut dyn HidIo, vid: u16, pid: u16) -> Result<DeviceInfo> {
    handshake_type2_legacy_with_io(io, vid, pid).map(|(info, _)| info)
}

/// Legacy Type2 handshake returning the raw response bytes for policy negotiation.
pub fn handshake_type2_legacy_with_io(
    io: &mut dyn HidIo,
    vid: u16,
    pid: u16,
) -> Result<(DeviceInfo, Vec<u8>)> {
    let init = build_init_packet_type2();
    let mut last_err = None;
    for attempt in 1..=HANDSHAKE_MAX_RETRIES {
        io.sleep(DELAY_PRE_INIT);
        if let Err(e) = io.write(&init) {
            last_err = Some(anyhow::anyhow!("HID init write failed: {e}"));
            if attempt < HANDSHAKE_MAX_RETRIES {
                io.sleep(HANDSHAKE_RETRY_DELAY);
            }
            continue;
        }
        io.sleep(DELAY_POST_INIT);
        match io.read(TYPE2_RESPONSE_SIZE) {
            Ok(resp) if validate_response_type2(&resp) => {
                let pm = resp[5];
                let sub = resp[4];
                let info = build_device_info(WireProtocol::HidType2, vid, pid, pm, sub, None)?;
                return Ok((info, resp));
            }
            Ok(resp) => {
                last_err = Some(anyhow::anyhow!(
                    "invalid HID Type2 response attempt {attempt} len={}",
                    resp.len()
                ));
            }
            Err(e) => last_err = Some(anyhow::anyhow!("HID init read failed: {e}")),
        }
        if attempt < HANDSHAKE_MAX_RETRIES {
            io.sleep(HANDSHAKE_RETRY_DELAY);
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("HID Type2 handshake failed")))
}

/// 4.07 probe: prefer a silent bounded read; if empty, elicit with one Type2 init.
pub fn handshake_type2_read_only_probe_with_io(
    io: &mut dyn HidIo,
    vid: u16,
    pid: u16,
    pre: Type2PreHandshakePolicy,
) -> Result<Type2NegotiatedObservation> {
    let Type2PreHandshakePolicy::Hid407ReadOnlyProbe = pre else {
        bail!("read-only probe requires Hid407ReadOnlyProbe pre-handshake policy");
    };
    let mut response = io
        .read(TYPE2_PROBE_READ_BOUND)
        .context("HID Type2 4.07 probe read failed")?;
    if response.is_empty() {
        let init = build_init_packet_type2();
        io.write(&init)
            .context("HID Type2 4.07 probe init elicit write failed")?;
        io.sleep(DELAY_POST_INIT);
        response = io
            .read(TYPE2_PROBE_READ_BOUND)
            .context("HID Type2 4.07 probe read after init elicit failed")?;
    }
    negotiate_type2_policy(vid, pid, &response, pre)
}

/// Handshake using the selected pre-handshake policy and return negotiated observation.
pub fn handshake_type2_with_policy(
    io: &mut dyn HidIo,
    vid: u16,
    pid: u16,
    pre: Type2PreHandshakePolicy,
) -> Result<(DeviceInfo, Type2NegotiatedObservation)> {
    match pre {
        Type2PreHandshakePolicy::LegacyBulkInit => {
            let (info, response) = handshake_type2_legacy_with_io(io, vid, pid)?;
            let observation = negotiate_type2_policy(vid, pid, &response, pre)?;
            Ok((info, observation))
        }
        Type2PreHandshakePolicy::Hid407ReadOnlyProbe => {
            let observation = handshake_type2_read_only_probe_with_io(io, vid, pid, pre)?;
            let info = build_device_info(
                WireProtocol::HidType2,
                vid,
                pid,
                observation.pm(),
                observation.sub(),
                None,
            )?;
            Ok((info, observation))
        }
        Type2PreHandshakePolicy::StopUnsupportedShape => {
            bail!("unsupported Type2 interface shape; handshake refused");
        }
    }
}

/// Type3 handshake control flow over injectable I/O (retries included).
pub fn handshake_type3_with_io(io: &mut dyn HidIo, vid: u16, pid: u16) -> Result<DeviceInfo> {
    let init = build_init_packet_type3();
    let mut last_err = None;
    for attempt in 1..=HANDSHAKE_MAX_RETRIES {
        io.sleep(DELAY_PRE_INIT);
        if let Err(error) = io.write(&init) {
            last_err = Some(anyhow::anyhow!("HID init write failed: {error}"));
            if attempt < HANDSHAKE_MAX_RETRIES {
                io.sleep(HANDSHAKE_RETRY_DELAY);
            }
            continue;
        }
        io.sleep(DELAY_POST_INIT);
        match io.read(TYPE3_RESPONSE_SIZE) {
            Ok(response) if validate_response_type3(&response) => {
                let fbl = response[0].saturating_sub(1);
                return build_device_info(WireProtocol::HidType3, vid, pid, fbl, 0, Some(fbl));
            }
            Ok(response) => {
                last_err = Some(anyhow::anyhow!(
                    "invalid HID Type3 response attempt {attempt} len={}",
                    response.len()
                ));
            }
            Err(error) => last_err = Some(anyhow::anyhow!("HID init read failed: {error}")),
        }
        if attempt < HANDSHAKE_MAX_RETRIES {
            io.sleep(HANDSHAKE_RETRY_DELAY);
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("HID Type3 handshake failed")))
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
pub fn build_frame_type2(
    image_data: &[u8],
    width: u32,
    height: u32,
    encoding: FrameEncoding,
) -> Vec<u8> {
    let is_jpeg = encoding.is_jpeg();
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
pub fn build_frame_type3(image_data: &[u8]) -> Result<Vec<u8>> {
    if image_data.len() != TYPE3_DATA_SIZE {
        bail!(
            "HID Type3 RGB565 payload length {} does not match fixed size {}",
            image_data.len(),
            TYPE3_DATA_SIZE
        );
    }
    let mut pkt = Vec::with_capacity(16 + TYPE3_DATA_SIZE);
    pkt.extend_from_slice(&TYPE3_FRAME_PREFIX);
    pkt.extend_from_slice(&[0, 0, 0, 0]);
    pkt.extend_from_slice(&(TYPE3_DATA_SIZE as u32).to_le_bytes());
    pkt.extend_from_slice(image_data);
    Ok(pkt)
}

fn validate_hid_frame(kind: HidType, info: &DeviceInfo, frame: &EncodedFrame) -> Result<()> {
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
    if kind == HidType::Type3 && !frame.encoding.is_rgb565() {
        bail!("HID Type3 requires RGB565, got {}", frame.encoding);
    }
    if frame.encoding.is_rgb565() {
        let expected_len = usize::try_from(wire_width)
            .ok()
            .and_then(|width| {
                usize::try_from(wire_height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(2))
            .context("HID RGB565 wire payload size overflow")?;
        if frame.data.len() != expected_len {
            bail!(
                "RGB565 payload length {} does not match {}x{} wire frame ({} bytes)",
                frame.data.len(),
                wire_width,
                wire_height,
                expected_len
            );
        }
    }
    Ok(())
}

fn send_frame_with_io(
    io: &mut dyn HidIo,
    kind: HidType,
    info: &DeviceInfo,
    frame: &EncodedFrame,
) -> Result<usize> {
    validate_hid_frame(kind, info, frame)?;
    let packet = match kind {
        HidType::Type2 => build_frame_type2(&frame.data, frame.width, frame.height, frame.encoding),
        HidType::Type3 => build_frame_type3(&frame.data)?,
    };
    io.write(&packet).context("HID frame write failed")?;
    match kind {
        HidType::Type2 => io.sleep(DELAY_FRAME_TYPE2),
        HidType::Type3 => {
            let ack = io
                .read(TYPE3_ACK_SIZE)
                .context("HID Type3 ACK read failed")?;
            if ack.is_empty() {
                bail!("HID Type3 ACK empty");
            }
        }
    }
    Ok(packet.len())
}

/// Type3 frame send control flow over injectable I/O.
pub fn send_frame_type3_with_io(
    io: &mut dyn HidIo,
    info: &DeviceInfo,
    frame: &EncodedFrame,
) -> Result<()> {
    send_frame_with_io(io, HidType::Type3, info, frame).map(|_| ())
}

fn frame_timeout(packet_size: usize) -> Duration {
    let ms = (packet_size / 4 + 100).max(100) as u64;
    Duration::from_millis(ms)
}

impl Transport for HidLcd {
    fn handshake(&mut self) -> Result<DeviceInfo> {
        let result = {
            let handle = self.handle.as_ref().context("HID device not open")?;
            let mut io = UsbHidIo {
                handle,
                ep_out: self.ep_out,
                ep_in: self.ep_in,
                write_timeout: Some(HANDSHAKE_TIMEOUT),
                read_timeout: HANDSHAKE_TIMEOUT,
            };
            match self.kind {
                HidType::Type2 => handshake_type2_with_io(&mut io, self.vid, self.pid),
                HidType::Type3 => handshake_type3_with_io(&mut io, self.vid, self.pid),
            }
        };

        match result {
            Ok(info) => {
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
                Ok(info)
            }
            Err(error) => {
                self.mark_disconnected();
                Err(error)
            }
        }
    }

    fn send_frame(&mut self, frame: &EncodedFrame) -> Result<()> {
        let send_result = {
            let info = self.info.as_ref().context("Handshake not performed")?;
            let handle = self.handle.as_ref().context("HID device not open")?;
            let mut io = UsbHidIo {
                handle,
                ep_out: self.ep_out,
                ep_in: self.ep_in,
                write_timeout: None,
                read_timeout: DEFAULT_FRAME_TIMEOUT,
            };
            send_frame_with_io(&mut io, self.kind, info, frame)
        };

        match send_result {
            Ok(packet_len) => {
                debug!("HID {:?} frame sent ({} bytes)", self.kind, packet_len);
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
    fn type2_frame_uses_negotiated_encoding_not_payload_magic() {
        let data = vec![0xFFu8, 0xD8, 0x00];
        let jpeg = build_frame_type2(&data, 320, 240, FrameEncoding::Jpeg);
        assert_eq!(jpeg.len() % 512, 0);
        assert_eq!(&jpeg[0..4], &TYPE2_MAGIC);
        assert_eq!(&jpeg[6..8], &[0x00, 0x00]);

        let rgb565 = build_frame_type2(&data, 320, 240, FrameEncoding::Rgb565Le);
        assert_eq!(
            &rgb565[6..8],
            &[0x01, 0x00],
            "RGB565 bytes beginning with JPEG SOI must keep raw framing"
        );
    }

    #[test]
    fn type3_frame_requires_fixed_size() {
        let exact = vec![0u8; TYPE3_DATA_SIZE];
        let pkt = build_frame_type3(&exact).expect("exact Type3 payload");
        assert_eq!(pkt.len(), 16 + TYPE3_DATA_SIZE);

        for invalid_len in [TYPE3_DATA_SIZE - 1, TYPE3_DATA_SIZE + 1] {
            let error = build_frame_type3(&vec![0; invalid_len]).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("payload length {invalid_len}")),
                "{error:#}"
            );
        }
    }

    #[test]
    fn hid_frame_validation_enforces_encoding_and_rgb565_size() {
        let cases = [
            (
                HidType::Type2,
                build_device_info(WireProtocol::HidType2, 0x0416, 0x5302, 58, 0, None).unwrap(),
            ),
            (
                HidType::Type2,
                build_device_info(WireProtocol::HidType2, 0x0416, 0x5302, 49, 0, None).unwrap(),
            ),
            (
                HidType::Type3,
                build_device_info(WireProtocol::HidType3, 0x0418, 0x5303, 100, 0, Some(100))
                    .unwrap(),
            ),
        ];
        for (kind, info) in cases {
            let (width, height) = info.wire_dimensions().unwrap();
            let expected_len = width as usize * height as usize * 2;
            let matching = EncodedFrame {
                data: vec![0; expected_len],
                width,
                height,
                encoding: info.encoding(),
            };
            validate_hid_frame(kind, &info, &matching).expect("matching frame");

            let mut opposite = matching.clone();
            opposite.encoding = match info.encoding() {
                FrameEncoding::Rgb565Le => FrameEncoding::Rgb565Be,
                FrameEncoding::Rgb565Be => FrameEncoding::Rgb565Le,
                other => panic!("expected RGB565 profile, got {other}"),
            };
            let error = validate_hid_frame(kind, &info, &opposite).unwrap_err();
            assert!(
                error.to_string().contains("does not match device"),
                "{error:#}"
            );

            for invalid_len in [expected_len - 1, expected_len + 1] {
                let mut invalid = matching.clone();
                invalid.data.resize(invalid_len, 0);
                let error = validate_hid_frame(kind, &info, &invalid).unwrap_err();
                assert!(
                    error
                        .to_string()
                        .contains(&format!("RGB565 payload length {invalid_len}")),
                    "{error:#}"
                );
            }
        }
    }

    #[test]
    fn type2_keeps_jpeg_variable_length_while_type3_rejects_it() {
        let info = build_device_info(WireProtocol::HidType2, 0x0416, 0x5302, 65, 3, None).unwrap();
        assert_eq!(info.encoding(), FrameEncoding::Jpeg);
        let (width, height) = info.wire_dimensions().unwrap();
        let frame = EncodedFrame {
            data: vec![0xff, 0xd8, 0xff, 0xd9],
            width,
            height,
            encoding: FrameEncoding::Jpeg,
        };

        validate_hid_frame(HidType::Type2, &info, &frame)
            .expect("Type2 JPEG payload length is variable");
        let error = validate_hid_frame(HidType::Type3, &info, &frame).unwrap_err();
        assert!(
            error.to_string().contains("Type3 requires RGB565"),
            "{error:#}"
        );
    }
}
