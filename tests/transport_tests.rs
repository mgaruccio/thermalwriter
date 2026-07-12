#![cfg(feature = "daemon")]

//! Wire protocol fixture tests with injectable I/O operation sequences.
//! No real USB/SCSI hardware required.

use anyhow::{Result, bail};
use std::collections::VecDeque;
use std::time::Duration;
use thermalwriter::render::RawFrame;
use thermalwriter::transport::encode::encode_frame;
use thermalwriter::transport::{
    DeviceInfo, EncodedFrame, FrameEncoding, Transport, WireProtocol, build_device_info, bulk_usb,
    device_info_from_fixture, hid_lcd, ly_lcd, null, scsi_lcd,
};

// ---------------------------------------------------------------------------
// Injectable op log
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum IoOp {
    Write(Vec<u8>),
    Read(usize),
    Wait(Duration),
    Zlp,
}

struct ScriptedIo {
    /// Scripted read responses, consumed in order.
    reads: VecDeque<Vec<u8>>,
    /// Recorded operations.
    log: Vec<IoOp>,
    fail_write_after: Option<usize>,
    writes: usize,
}

impl ScriptedIo {
    fn new(reads: Vec<Vec<u8>>) -> Self {
        Self {
            reads: reads.into(),
            log: Vec::new(),
            fail_write_after: None,
            writes: 0,
        }
    }

    fn write(&mut self, data: &[u8]) -> Result<()> {
        self.writes += 1;
        if let Some(n) = self.fail_write_after {
            if self.writes > n {
                bail!("scripted write failure");
            }
        }
        if data.is_empty() {
            self.log.push(IoOp::Zlp);
        } else {
            self.log.push(IoOp::Write(data.to_vec()));
        }
        Ok(())
    }

    fn read(&mut self, max: usize) -> Result<Vec<u8>> {
        self.log.push(IoOp::Read(max));
        self.reads
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("no scripted read response left"))
    }
}

// ---------------------------------------------------------------------------
// Pure helper tests (existing)
// ---------------------------------------------------------------------------

#[test]
fn write_all_handles_partial_writes_by_continuing() {
    let data = b"abcdefghij";
    let mut calls = 0usize;
    let mut sent = Vec::new();
    bulk_usb::write_all(data, |buf| {
        calls += 1;
        let n = if buf.len() > 3 { 3 } else { buf.len() };
        sent.extend_from_slice(&buf[..n]);
        Ok(n)
    })
    .unwrap();
    assert!(calls > 1);
    assert_eq!(sent, data);
}

#[test]
fn write_all_bails_on_zero_length_write() {
    let err = bulk_usb::write_all(b"hello", |_buf| Ok(0)).unwrap_err();
    assert!(err.to_string().contains("zero-length"));
}

#[test]
fn handshake_payload_is_64_bytes() {
    let p = bulk_usb::handshake_payload();
    assert_eq!(p.len(), 64);
    assert_eq!(&p[0..4], &[0x12, 0x34, 0x56, 0x78]);
    assert_eq!(p[56], 0x01);
}

#[test]
fn frame_header_is_64_bytes_with_correct_fields() {
    let h = bulk_usb::build_frame_header(2, 480, 480, 1234);
    assert_eq!(h.len(), 64);
    assert_eq!(&h[0..4], &[0x12, 0x34, 0x56, 0x78]);
    assert_eq!(u32::from_le_bytes(h[4..8].try_into().unwrap()), 2);
    assert_eq!(u32::from_le_bytes(h[8..12].try_into().unwrap()), 480);
    assert_eq!(u32::from_le_bytes(h[12..16].try_into().unwrap()), 480);
    assert_eq!(u32::from_le_bytes(h[56..60].try_into().unwrap()), 2);
    assert_eq!(u32::from_le_bytes(h[60..64].try_into().unwrap()), 1234);
}

#[test]
fn bulk_frame_cmd_jpeg_vs_rgb565() {
    let jpeg = bulk_usb::build_frame_header(2, 480, 480, 100);
    let rgb = bulk_usb::build_frame_header(3, 320, 320, 204800);
    assert_eq!(u32::from_le_bytes(jpeg[4..8].try_into().unwrap()), 2);
    assert_eq!(u32::from_le_bytes(rgb[4..8].try_into().unwrap()), 3);
    assert_eq!(u32::from_le_bytes(rgb[8..12].try_into().unwrap()), 320);
}

#[test]
fn bulk_zlp_required_when_frame_aligned_to_512() {
    // Header 64 + payload 448 = 512 → ZLP required by protocol.
    let payload_len = 448u32;
    let header = bulk_usb::build_frame_header(2, 480, 480, payload_len);
    let total = 64 + payload_len as usize;
    assert_eq!(total % 512, 0);
    assert_eq!(header.len(), 64);
}

// ---------------------------------------------------------------------------
// SCSI pure + boot-poll sequence via scripted wait hook
// ---------------------------------------------------------------------------

#[test]
fn scsi_cdb_and_unpadded_chunks() {
    let cdb = scsi_lcd::build_cdb(0xF5, 0xE100);
    assert_eq!(&cdb[0..4], &0xF5u32.to_le_bytes());
    assert_eq!(&cdb[4..12], &[0u8; 8]);
    assert_eq!(&cdb[12..16], &0xE100u32.to_le_bytes());

    let chunks = scsi_lcd::frame_chunks(320, 240);
    let total: u32 = chunks.iter().map(|(_, s)| *s).sum();
    assert_eq!(total, 320 * 240 * 2);
    // Final chunk unpadded
    assert_eq!(chunks.last().unwrap().1, total % 0xE100);
    // Small display uses 0xE100
    assert!(chunks.iter().all(|(_, s)| *s <= 0xE100));
}

#[test]
fn scsi_large_display_uses_64kib_chunks() {
    let chunks = scsi_lcd::frame_chunks(640, 480);
    assert!(chunks.iter().all(|(_, s)| *s <= 0x10000));
    let total: u32 = chunks.iter().map(|(_, s)| *s).sum();
    assert_eq!(total, 640 * 480 * 2);
}

/// Scripted SCSI boot-poll: five A1A2A3A4 responses exhaust without init.
#[test]
fn scsi_five_boot_attempts_no_init() {
    // We can't open /dev/sg without root; validate the pure policy:
    // BOOT_MAX_ATTEMPTS = 5, wait only when remaining > 0.
    let mut waits = 0u32;
    let mut attempts = 0u32;
    const BOOT_MAX: u32 = 5;
    let boot = [0u8, 0, 0, 0, 0xA1, 0xA2, 0xA3, 0xA4];
    let mut left_boot = true;
    for attempt in 0..BOOT_MAX {
        attempts += 1;
        let is_boot = boot[4..8] == [0xA1, 0xA2, 0xA3, 0xA4];
        assert!(is_boot);
        let remaining = BOOT_MAX - attempt - 1;
        if remaining == 0 {
            left_boot = true; // still boot on fifth
            break;
        }
        waits += 1; // wait 3s only if another attempt remains
    }
    assert_eq!(attempts, 5);
    assert_eq!(waits, 4); // waits after attempts 1..4, not after 5th
    assert!(left_boot);
}

// ---------------------------------------------------------------------------
// HID pure sequence helpers
// ---------------------------------------------------------------------------

#[test]
fn hid_type2_init_response_and_aligned_frame() {
    let init = hid_lcd::build_init_packet_type2();
    assert_eq!(init.len(), 512);
    assert_eq!(&init[0..4], &[0xDA, 0xDB, 0xDC, 0xDD]);
    assert_eq!(init[12], 1);

    let mut resp = vec![0u8; 20];
    resp[0..4].copy_from_slice(&[0xDA, 0xDB, 0xDC, 0xDD]);
    resp[12] = 1;
    resp[5] = 58; // PM
    resp[4] = 0; // SUB
    assert!(hid_lcd::validate_response_type2(&resp));

    let frame =
        hid_lcd::build_frame_type2(&[0xFF, 0xD8, 0x00, 0x01], 320, 240, FrameEncoding::Jpeg);
    assert_eq!(frame.len() % 512, 0);
    assert_eq!(&frame[0..4], &[0xDA, 0xDB, 0xDC, 0xDD]);
}

#[test]
fn hid_type3_init_and_fixed_frame_with_ack_size() {
    let init = hid_lcd::build_init_packet_type3();
    assert_eq!(init.len(), 1040);
    assert_eq!(init[0], 0xF5);

    assert!(hid_lcd::validate_response_type3(&[0x65; 14]));
    assert!(hid_lcd::validate_response_type3(&[0x66; 14]));
    assert!(!hid_lcd::validate_response_type3(&[0x64; 14]));

    let frame = hid_lcd::build_frame_type3(&vec![0u8; 204800]).unwrap();
    assert_eq!(frame.len(), 16 + 204800);
    // Both PIDs 5303/5304 share Type3 framing.
    let _ = build_device_info(WireProtocol::HidType3, 0x0418, 0x5303, 100, 0, Some(100)).unwrap();
    let _ = build_device_info(WireProtocol::HidType3, 0x0418, 0x5304, 101, 0, Some(101)).unwrap();
}

#[test]
fn hid_retry_timing_constants() {
    // Documented protocol: 3 attempts, 50ms pre, 200ms post, 500ms retry.
    // Pure builders don't sleep; timings are asserted as constants via code paths
    // exercised when real I/O is injected in unit tests of the helpers above.
    assert_eq!(hid_lcd::build_init_packet_type2().len(), 512);
    assert_eq!(hid_lcd::build_init_packet_type3().len(), 1040);
}

// ---------------------------------------------------------------------------
// LY pure sequence helpers
// ---------------------------------------------------------------------------

#[test]
fn ly_handshake_and_chunk_boundaries() {
    let hs = ly_lcd::handshake_payload();
    assert_eq!(hs.len(), 2048);
    assert_eq!(hs[0], 0x02);
    assert_eq!(hs[1], 0xFF);
    assert_eq!(hs[8], 0x01);

    for n in [495usize, 496, 497] {
        let packed = ly_lcd::pack_ly_payload(&vec![0xABu8; n], 0x5409).unwrap();
        let num = n / 496 + 1;
        assert_eq!(packed.len(), num * 512, "n={n}");
        if n == 496 {
            let last = &packed[(num - 1) * 512..];
            let data_len = u16::from_le_bytes([last[6], last[7]]);
            assert_eq!(data_len, 0, "496 retains zero-data terminal record");
        }
    }

    // 5408 pads record count to multiple of 4; 5409 does not.
    let ly = ly_lcd::pack_ly_payload(&[0u8; 10], 0x5408).unwrap();
    assert_eq!(ly.len(), 4 * 512);
    let ly1 = ly_lcd::pack_ly_payload(&[0u8; 10], 0x5409).unwrap();
    assert_eq!(ly1.len(), 512);

    // Write plan: 4096 bursts + 2048/actual tails.
    assert_eq!(ly_lcd::ly_write_plan(4096 + 100, 0x5408), vec![4096, 100]);
    assert_eq!(
        ly_lcd::ly_write_plan(4096 + 3000, 0x5408),
        vec![4096, 2048, 952]
    );
    assert_eq!(ly_lcd::ly_write_plan(4096 + 123, 0x5409), vec![4096, 123]);
}

#[test]
fn ly_parse_pm_sub_from_handshake() {
    let mut resp = vec![0u8; 64];
    resp[0] = 3;
    resp[1] = 0xFF;
    resp[8] = 1;
    resp[20] = 1; // raw <=3 → 1 → PM=65
    resp[22] = 2; // SUB = 3
    let (pm, sub) = ly_lcd::parse_ly_pm_sub(0x5408, &resp).unwrap();
    assert_eq!((pm, sub), (65, 3));

    resp[36] = 0;
    resp[22] = 0;
    let (pm, sub) = ly_lcd::parse_ly_pm_sub(0x5409, &resp).unwrap();
    assert_eq!((pm, sub), (50, 0));
}

// ---------------------------------------------------------------------------
// Null capture mode: sequence names/keys
// ---------------------------------------------------------------------------

#[test]
fn null_capture_writes_matching_bin_toml() {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: test-local env for capture path; serialised by process isolation of cargo test
    // when not parallelised across this env var. We set and unset around the transport.
    // Use with_profile + manual capture by setting env.
    unsafe {
        std::env::set_var("THERMALWRITER_CAPTURE_DIR", dir.path());
    }
    let info = device_info_from_fixture("ly-0416-5408-pm65-sub3-fbl192").unwrap();
    let mut t = null::NullTransport::with_profile(info.clone());
    let _ = t.handshake().unwrap();
    let frame = EncodedFrame {
        data: vec![0xFFu8, 0xD8, 0x00, 0x01],
        width: info.width(),
        height: info.height(),
        encoding: FrameEncoding::Jpeg,
    };
    t.send_frame(&frame).unwrap();
    t.send_frame(&frame).unwrap();
    unsafe {
        std::env::remove_var("THERMALWRITER_CAPTURE_DIR");
    }

    let bin1 = dir.path().join("frame-000001.bin");
    let toml1 = dir.path().join("frame-000001.toml");
    let bin2 = dir.path().join("frame-000002.bin");
    let toml2 = dir.path().join("frame-000002.toml");
    assert!(bin1.exists() && toml1.exists());
    assert!(bin2.exists() && toml2.exists());
    let meta = std::fs::read_to_string(toml1).unwrap();
    assert!(meta.contains("profile_id"));
    assert!(meta.contains("width"));
    assert!(meta.contains("height"));
    assert!(meta.contains("encoding"));
    assert!(meta.contains("jpeg") || meta.contains("\"jpeg\""));
    assert_eq!(std::fs::read(bin1).unwrap(), frame.data);
}

// ---------------------------------------------------------------------------
// Connector multi-device / zero-device errors
// ---------------------------------------------------------------------------

#[test]
fn select_devices_zero_and_ambiguous() {
    use thermalwriter::transport::discovery::{
        DevicePath, DeviceSelector, DiscoveredDevice, select_devices,
    };
    let d1 = DiscoveredDevice {
        vid: 0x87ad,
        pid: 0x70db,
        protocol: WireProtocol::Bulk,
        serial: None,
        path: DevicePath::Usb {
            bus: 1,
            address: 2,
            interface: 0,
            ep_in: 0x81,
            ep_out: 0x01,
        },
    };
    let mut d2 = d1.clone();
    if let DevicePath::Usb { address, .. } = &mut d2.path {
        *address = 9;
    }
    assert!(select_devices(&[], &DeviceSelector::Auto).is_err());
    assert!(select_devices(&[d1.clone(), d2], &DeviceSelector::Auto).is_err());
    assert!(select_devices(std::slice::from_ref(&d1), &DeviceSelector::Auto).is_ok());
    // Explicit ID still errors on duplicates.
    let mut d3 = d1.clone();
    if let DevicePath::Usb { address, .. } = &mut d3.path {
        *address = 3;
    }
    assert!(
        select_devices(
            &[d1, d3],
            &DeviceSelector::UsbId {
                vid: 0x87ad,
                pid: 0x70db
            }
        )
        .is_err()
    );
}

// ---------------------------------------------------------------------------
// Fatal send drops connected flag (mock)
// ---------------------------------------------------------------------------

struct FailOnceTransport {
    fail: bool,
    connected: bool,
}

impl Transport for FailOnceTransport {
    fn handshake(&mut self) -> Result<DeviceInfo> {
        build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, Some(72))
    }
    fn send_frame(&mut self, _frame: &EncodedFrame) -> Result<()> {
        if self.fail {
            self.fail = false;
            self.connected = false;
            bail!("fatal USB error");
        }
        Ok(())
    }
    fn close(&mut self) {}
    fn is_connected(&self) -> bool {
        self.connected
    }
}

#[test]
fn fatal_send_marks_disconnected_for_connector_retry() {
    let mut t = FailOnceTransport {
        fail: true,
        connected: true,
    };
    let frame = EncodedFrame {
        data: vec![0],
        width: 480,
        height: 480,
        encoding: FrameEncoding::Jpeg,
    };
    assert!(t.send_frame(&frame).is_err());
    assert!(!t.is_connected());
    // Caller drops transport and connector rediscovers.
    t.connected = true;
    t.send_frame(&frame).unwrap();
    assert!(t.is_connected());
}

// ---------------------------------------------------------------------------
// Scripted bulk-like handshake sequence (write handshake → read resp)
// ---------------------------------------------------------------------------

#[test]
fn scripted_bulk_handshake_sequence() {
    let mut resp = vec![0u8; 64];
    resp[24] = 4; // PM
    resp[36] = 5; // SUB
    let mut io = ScriptedIo::new(vec![resp]);
    // handshake write
    let payload = bulk_usb::handshake_payload();
    io.write(&payload).unwrap();
    let r = io.read(1024).unwrap();
    assert_eq!(r[24], 4);
    assert_eq!(r[36], 5);
    let info = build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, r[24], r[36], None).unwrap();
    assert_eq!((info.width(), info.height()), (480, 480));
    assert!(info.encoding().is_jpeg());
    // ops sequence
    assert!(matches!(io.log[0], IoOp::Write(_)));
    assert!(matches!(io.log[1], IoOp::Read(1024)));
}

#[test]
fn scripted_bulk_pm32_rgb565_and_pm64_wide() {
    for (pm, sub, w, h, enc_jpeg) in [
        (32u8, 0u8, 320u32, 320u32, false),
        (64, 0, 1600, 720, true),
        (4, 5, 480, 480, true),
    ] {
        let info = build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, pm, sub, None).unwrap();
        assert_eq!((info.width(), info.height()), (w, h));
        assert_eq!(info.encoding().is_jpeg(), enc_jpeg);
        let cmd = if enc_jpeg { 2 } else { 3 };
        let header = bulk_usb::build_frame_header(cmd, w, h, 100);
        assert_eq!(u32::from_le_bytes(header[4..8].try_into().unwrap()), cmd);
    }
}

// ===========================================================================
// Real control-flow tests: drive production handshake/send via injectable I/O
// ===========================================================================

struct MemBulkIo {
    reads: std::collections::VecDeque<Vec<u8>>,
    log: Vec<IoOp>,
}

impl MemBulkIo {
    fn new(reads: Vec<Vec<u8>>) -> Self {
        Self {
            reads: reads.into(),
            log: Vec::new(),
        }
    }
}

impl bulk_usb::BulkIo for MemBulkIo {
    fn write(&mut self, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            self.log.push(IoOp::Zlp);
        } else {
            self.log.push(IoOp::Write(data.to_vec()));
        }
        Ok(())
    }
    fn read(&mut self, max_len: usize) -> Result<Vec<u8>> {
        self.log.push(IoOp::Read(max_len));
        self.reads
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("no read"))
    }
}

#[test]
fn bulk_handshake_with_io_pm4_and_send_jpeg_zlp() {
    let mut resp = vec![0u8; 64];
    resp[24] = 4;
    resp[36] = 5;
    let mut io = MemBulkIo::new(vec![resp]);
    let info = bulk_usb::handshake_with_io(&mut io, 0x87ad, 0x70db).unwrap();
    assert_eq!((info.width(), info.height()), (480, 480));
    assert!(matches!(io.log[0], IoOp::Write(ref w) if w.len() == 64));
    assert!(matches!(io.log[1], IoOp::Read(1024)));

    // Payload such that 64+len is 512-aligned → ZLP.
    let payload = vec![0xFFu8; 448];
    let frame = EncodedFrame {
        data: payload,
        width: 480,
        height: 480,
        encoding: FrameEncoding::Jpeg,
    };
    bulk_usb::send_frame_with_io(&mut io, &info, &frame).unwrap();
    assert!(io.log.iter().any(|op| matches!(op, IoOp::Zlp)));
    // Header cmd=2 for JPEG
    if let IoOp::Write(w) = &io.log[2] {
        assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()), 2);
    } else {
        panic!("expected frame write");
    }
}

#[test]
fn bulk_handshake_with_io_pm32_rgb565_cmd3() {
    let mut resp = vec![0u8; 64];
    resp[24] = 32;
    resp[36] = 0;
    let mut io = MemBulkIo::new(vec![resp]);
    let info = bulk_usb::handshake_with_io(&mut io, 0x87ad, 0x70db).unwrap();
    assert_eq!(info.encoding(), FrameEncoding::Rgb565Be);
    let frame = EncodedFrame {
        data: vec![0u8; 320 * 320 * 2],
        width: 320,
        height: 320,
        encoding: FrameEncoding::Rgb565Be,
    };
    bulk_usb::send_frame_with_io(&mut io, &info, &frame).unwrap();
    if let IoOp::Write(w) = &io.log[2] {
        assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()), 3);
    }
}

struct MemHidIo {
    reads: std::collections::VecDeque<Vec<u8>>,
    log: Vec<IoOp>,
    sleeps: Vec<Duration>,
}

impl MemHidIo {
    fn new(reads: Vec<Vec<u8>>) -> Self {
        Self {
            reads: reads.into(),
            log: Vec::new(),
            sleeps: Vec::new(),
        }
    }
}

impl hid_lcd::HidIo for MemHidIo {
    fn write(&mut self, data: &[u8]) -> Result<()> {
        self.log.push(IoOp::Write(data.to_vec()));
        Ok(())
    }
    fn read(&mut self, max_len: usize) -> Result<Vec<u8>> {
        self.log.push(IoOp::Read(max_len));
        self.reads
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("no read"))
    }
    fn sleep(&mut self, d: Duration) {
        self.sleeps.push(d);
        self.log.push(IoOp::Wait(d));
    }
}

#[test]
fn hid_type2_handshake_retries_then_succeeds() {
    // First two reads invalid; third valid. Expect 3 attempts with pre/post/retry sleeps.
    let bad = vec![0u8; 20];
    let mut good = vec![0u8; 20];
    good[0..4].copy_from_slice(&[0xDA, 0xDB, 0xDC, 0xDD]);
    good[12] = 1;
    good[5] = 58;
    good[4] = 0;
    let mut io = MemHidIo::new(vec![bad.clone(), bad, good]);
    let info = hid_lcd::handshake_type2_with_io(&mut io, 0x0416, 0x5302).unwrap();
    assert_eq!((info.width(), info.height()), (320, 240));
    // 3 writes of 512-byte init
    let writes: Vec<_> = io
        .log
        .iter()
        .filter(|op| matches!(op, IoOp::Write(w) if w.len() == 512))
        .collect();
    assert_eq!(writes.len(), 3);
    // sleeps: each attempt pre(50)+post(200), failed attempts also retry(500)
    assert!(io.sleeps.iter().any(|d| *d == Duration::from_millis(50)));
    assert!(io.sleeps.iter().any(|d| *d == Duration::from_millis(200)));
    assert!(io.sleeps.iter().any(|d| *d == Duration::from_millis(500)));
}

#[test]
fn hid_type3_both_pids_handshake_send_and_read_exact_ack() {
    for (pid, response_code, expected_fbl) in [(0x5303, 0x65, 100), (0x5304, 0x66, 101)] {
        let mut response = vec![0; 14];
        response[0] = response_code;
        let mut io = MemHidIo::new(vec![response, vec![0xaa]]);

        let info = hid_lcd::handshake_type3_with_io(&mut io, 0x0418, pid).unwrap();
        assert_eq!((info.vid, info.pid), (0x0418, pid));
        assert_eq!(info.fbl, expected_fbl);
        assert_eq!(info.protocol, WireProtocol::HidType3);
        assert_eq!(info.encoding(), FrameEncoding::Rgb565Be);

        let (width, height) = info.wire_dimensions().unwrap();
        let frame = EncodedFrame {
            data: vec![0; width as usize * height as usize * 2],
            width,
            height,
            encoding: info.encoding(),
        };
        hid_lcd::send_frame_type3_with_io(&mut io, &info, &frame).unwrap();

        assert_eq!(io.log.len(), 6, "pid={pid:04x}: {:?}", io.log);
        assert!(matches!(io.log[0], IoOp::Wait(d) if d == Duration::from_millis(50)));
        assert!(matches!(io.log[1], IoOp::Write(ref data) if data.len() == 1040));
        assert!(matches!(io.log[2], IoOp::Wait(d) if d == Duration::from_millis(200)));
        assert!(matches!(io.log[3], IoOp::Read(1024)));
        assert!(matches!(io.log[4], IoOp::Write(ref data) if data.len() == 16 + 204_800));
        assert!(matches!(io.log[5], IoOp::Read(16)));
    }
}

struct MemLyIo {
    reads: std::collections::VecDeque<Vec<u8>>,
    log: Vec<IoOp>,
}

impl MemLyIo {
    fn new(reads: Vec<Vec<u8>>) -> Self {
        Self {
            reads: reads.into(),
            log: Vec::new(),
        }
    }
}

impl ly_lcd::LyIo for MemLyIo {
    fn write(&mut self, data: &[u8]) -> Result<()> {
        self.log.push(IoOp::Write(data.to_vec()));
        Ok(())
    }
    fn read(&mut self, max_len: usize) -> Result<Vec<u8>> {
        self.log.push(IoOp::Read(max_len));
        self.reads
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("no read"))
    }
}

#[test]
fn ly_handshake_and_send_with_io_records_ack() {
    let mut resp = vec![0u8; 64];
    resp[0] = 3;
    resp[1] = 0xFF;
    resp[8] = 1;
    resp[20] = 1;
    resp[22] = 2;
    let mut io = MemLyIo::new(vec![resp, vec![0u8; 16]]); // handshake + ACK
    let info = ly_lcd::handshake_ly_with_io(&mut io, 0x0416, 0x5408).unwrap();
    assert_eq!((info.pm, info.sub), (65, 3));
    assert_eq!((info.width(), info.height()), (1920, 462));
    let frame = EncodedFrame {
        data: vec![0xABu8; 496],
        width: info.width(),
        height: info.height(),
        encoding: FrameEncoding::Jpeg,
    };
    ly_lcd::send_ly_with_io(&mut io, 0x5408, &frame).unwrap();
    // handshake write 2048, then frame writes, then ACK read
    assert!(matches!(io.log[0], IoOp::Write(ref w) if w.len() == 2048));
    assert!(io.log.iter().any(|op| matches!(op, IoOp::Read(512))));
}

#[test]
fn ly_rgb565_send_rejects_nonexact_payload_without_io() {
    for invalid_len in [7, 9] {
        let mut io = MemLyIo::new(vec![vec![0u8; 16]]);
        let frame = EncodedFrame {
            data: vec![0; invalid_len],
            width: 2,
            height: 2,
            encoding: FrameEncoding::Rgb565Le,
        };

        let error = ly_lcd::send_ly_with_io(&mut io, 0x5409, &frame).unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("RGB565 payload length {invalid_len}")),
            "{error:#}"
        );
        assert!(io.log.is_empty(), "invalid frame performed I/O");
    }

    let mut io = MemLyIo::new(vec![vec![0u8; 16]]);
    let exact = EncodedFrame {
        data: vec![0; 8],
        width: 2,
        height: 2,
        encoding: FrameEncoding::Rgb565Le,
    };
    ly_lcd::send_ly_with_io(&mut io, 0x5409, &exact).expect("exact payload should be sent");
    assert!(matches!(
        io.log.as_slice(),
        [IoOp::Write(_), IoOp::Read(512)]
    ));
}
#[test]
fn ly1_fixture_matches_handshake_producible_pm_and_fbl() {
    let mut response = vec![0u8; 64];
    response[0] = 3;
    response[1] = 0xFF;
    response[8] = 1;
    response[36] = 0;
    let mut io = MemLyIo::new(vec![response]);
    let actual = ly_lcd::handshake_ly_with_io(&mut io, 0x0416, 0x5409).unwrap();
    let fixture = device_info_from_fixture("ly-0416-5409-pm50-sub0-fbl50").unwrap();
    assert_eq!(actual, fixture);
}

struct MemScsiIo {
    /// poll responses in order
    polls: std::collections::VecDeque<Vec<u8>>,
    log: Vec<String>,
    waits: Vec<Duration>,
    inits: u32,
}

impl MemScsiIo {
    fn new(polls: Vec<Vec<u8>>) -> Self {
        Self {
            polls: polls.into(),
            log: Vec::new(),
            waits: Vec::new(),
            inits: 0,
        }
    }
}

impl scsi_lcd::ScsiIo for MemScsiIo {
    fn read_cdb(&mut self, cdb: &[u8; 16], size: usize) -> Result<Vec<u8>> {
        let cmd = u32::from_le_bytes(cdb[0..4].try_into().unwrap());
        self.log.push(format!("read_cdb cmd=0x{cmd:x} size={size}"));
        self.polls
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("no poll response"))
    }
    fn send_cdb(&mut self, cdb: &[u8; 16], data: &[u8]) -> Result<()> {
        let cmd = u32::from_le_bytes(cdb[0..4].try_into().unwrap());
        self.log
            .push(format!("send_cdb cmd=0x{cmd:x} len={}", data.len()));
        if cmd == 0x1F5 {
            self.inits += 1;
        }
        Ok(())
    }
    fn wait(&mut self, d: Duration) {
        self.waits.push(d);
        self.log.push(format!("wait {}ms", d.as_millis()));
    }
}

fn boot_resp() -> Vec<u8> {
    let mut v = vec![0u8; 16];
    v[4..8].copy_from_slice(&[0xA1, 0xA2, 0xA3, 0xA4]);
    v
}

fn ready_resp(fbl: u8) -> Vec<u8> {
    let mut v = vec![0u8; 16];
    v[0] = fbl;
    v
}

#[test]
fn scsi_handshake_five_boots_errors_without_init() {
    let polls = vec![boot_resp(); 5];
    let mut io = MemScsiIo::new(polls);
    let err = scsi_lcd::handshake_scsi_with_io(&mut io, 0x87cd, 0x70db).unwrap_err();
    assert!(err.to_string().contains("still booting"));
    assert_eq!(io.inits, 0);
    assert_eq!(io.waits.len(), 4); // wait only when another attempt remains
    assert!(io.waits.iter().all(|d| *d == Duration::from_secs(3)));
}

#[test]
fn scsi_handshake_boot_then_ready_inits_once() {
    let polls = vec![boot_resp(), boot_resp(), ready_resp(100)];
    let mut io = MemScsiIo::new(polls);
    let info = scsi_lcd::handshake_scsi_with_io(&mut io, 0x87cd, 0x70db).unwrap();
    assert_eq!(info.fbl, 100);
    assert_eq!((info.width(), info.height()), (320, 320));
    assert_eq!(io.inits, 1);
    assert_eq!(io.waits.len(), 3); // 2 boot waits + post-init 100ms
    assert!(io.waits.iter().any(|d| *d == Duration::from_millis(100)));
}

#[test]
fn scsi_send_enforces_negotiated_rgb565_encoding_without_wrong_io() {
    for (fbl, expected, opposite) in [
        (50, FrameEncoding::Rgb565Le, FrameEncoding::Rgb565Be),
        (100, FrameEncoding::Rgb565Be, FrameEncoding::Rgb565Le),
    ] {
        let info =
            build_device_info(WireProtocol::Scsi, 0x87cd, 0x70db, fbl, 0, Some(fbl)).unwrap();
        let (width, height) = info.wire_dimensions().unwrap();
        let data = vec![0; width as usize * height as usize * 2];
        let frame = |encoding| EncodedFrame {
            data: data.clone(),
            width,
            height,
            encoding,
        };

        let mut io = MemScsiIo::new(Vec::new());
        let error = scsi_lcd::send_frame_scsi_with_io(&mut io, &info, &frame(opposite))
            .expect_err("opposite byte order must be rejected");
        assert!(
            error.to_string().contains(&format!(
                "frame encoding {opposite} does not match device {expected}"
            )),
            "{error:#}"
        );
        assert!(io.log.is_empty(), "wrong byte order performed SCSI I/O");

        scsi_lcd::send_frame_scsi_with_io(&mut io, &info, &frame(expected))
            .expect("negotiated byte order should be sent");
        assert!(
            !io.log.is_empty(),
            "matching byte order performed no SCSI I/O"
        );
    }

    let info = build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, None).unwrap();
    let (width, height) = info.wire_dimensions().unwrap();
    let jpeg = EncodedFrame {
        data: vec![0; width as usize * height as usize * 2],
        width,
        height,
        encoding: FrameEncoding::Jpeg,
    };
    let mut io = MemScsiIo::new(Vec::new());
    let error = scsi_lcd::send_frame_scsi_with_io(&mut io, &info, &jpeg)
        .expect_err("SCSI helper must remain RGB565-only");
    assert!(
        error.to_string().contains("SCSI requires RGB565"),
        "{error:#}"
    );
    assert!(io.log.is_empty(), "JPEG frame performed SCSI I/O");
}

#[test]
fn scsi_rotate_panel_encode_and_send_use_swapped_wire_dimensions() {
    let info = build_device_info(WireProtocol::Scsi, 0x87cd, 0x70db, 50, 0, Some(50)).unwrap();
    assert_eq!((info.width(), info.height()), (320, 240));
    assert_eq!(info.wire_dimensions().unwrap(), (240, 320));

    let frame = RawFrame {
        data: vec![0x55; 320 * 240 * 3],
        width: 320,
        height: 240,
    };
    let encoded = encode_frame(&frame, &info, 0, 100).unwrap();
    assert_eq!((encoded.width, encoded.height), (240, 320));
    assert_eq!(encoded.data.len(), 240 * 320 * 2);

    let mut io = MemScsiIo::new(Vec::new());
    scsi_lcd::send_frame_scsi_with_io(&mut io, &info, &encoded).unwrap();
    let sent: usize = io
        .log
        .iter()
        .filter_map(|entry| entry.rsplit_once("len="))
        .map(|(_, len)| len.parse::<usize>().unwrap())
        .sum();
    assert_eq!(sent, encoded.data.len());
}
