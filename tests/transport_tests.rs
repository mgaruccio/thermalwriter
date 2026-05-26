#![cfg(feature = "daemon")]

use anyhow::Result;
use thermalwriter::transport::{DeviceInfo, Transport, bulk_usb};

// ---------------------------------------------------------------------------
// Reconnect trait tests (plan Task 9) — TDD: written BEFORE implementation
// ---------------------------------------------------------------------------

/// A mock Transport that fails send_frame once, then succeeds.
/// Verifies that is_connected() goes false after send_frame Err, and
/// try_reconnect() returns it to a usable state.
struct MockTransport {
    send_count: usize,
    reconnect_count: usize,
    connected: bool,
}

impl MockTransport {
    fn new() -> Self {
        Self {
            send_count: 0,
            reconnect_count: 0,
            connected: true,
        }
    }
}

impl Transport for MockTransport {
    fn handshake(&mut self) -> Result<DeviceInfo> {
        Ok(DeviceInfo {
            vid: 0,
            pid: 0,
            width: 480,
            height: 480,
            pm: 4,
            sub: 5,
            use_jpeg: true,
        })
    }
    fn send_frame(&mut self, _data: &[u8]) -> Result<()> {
        self.send_count += 1;
        if self.send_count == 1 {
            self.connected = false;
            anyhow::bail!("simulated USB disconnect")
        } else {
            Ok(())
        }
    }
    fn close(&mut self) {
        self.connected = false;
    }
    fn is_connected(&self) -> bool {
        self.connected
    }
    fn try_reconnect(&mut self) -> Result<()> {
        self.reconnect_count += 1;
        self.connected = true;
        Ok(())
    }
}

/// After a send_frame failure the transport marks itself disconnected.
/// The caller detects is_connected()==false and calls try_reconnect().
/// A subsequent send_frame then succeeds.
#[test]
fn reconnect_is_called_after_send_frame_failure() {
    let mut t = MockTransport::new();

    assert!(t.is_connected(), "should start connected");

    // First send fails and marks transport disconnected
    let r1 = t.send_frame(&[0u8; 100]);
    assert!(r1.is_err(), "first send must fail");
    assert!(
        !t.is_connected(),
        "transport must be disconnected after send failure"
    );

    // Caller reconnects
    t.try_reconnect().expect("reconnect must succeed");
    assert!(
        t.is_connected(),
        "transport must be connected after try_reconnect"
    );
    assert_eq!(
        t.reconnect_count, 1,
        "try_reconnect must have been called once"
    );

    // Second send succeeds
    let r2 = t.send_frame(&[0u8; 100]);
    assert!(r2.is_ok(), "second send must succeed after reconnect");
}

// ---------------------------------------------------------------------------
// write_all helper tests (plan Task 8) — TDD: written BEFORE implementation
// ---------------------------------------------------------------------------

/// write_all must loop when the writer returns a partial write, continuing
/// from the correct offset until all bytes are sent.
#[test]
fn write_all_handles_partial_writes_by_continuing() {
    let data = vec![0u8; 16 * 1024];
    let mut call_count = 0;
    let result = bulk_usb::write_all(&data, |chunk| {
        call_count += 1;
        if call_count == 1 {
            Ok(chunk.len() / 2) // partial write of half
        } else {
            Ok(chunk.len()) // full write of remainder
        }
    });
    assert!(
        result.is_ok(),
        "write_all must succeed after partial then full write"
    );
    assert_eq!(
        call_count, 2,
        "must call writer twice: partial then remainder"
    );
}

/// write_all must bail immediately when the writer returns 0 (signals disconnection).
#[test]
fn write_all_bails_on_zero_length_write() {
    let data = vec![0u8; 100];
    let result = bulk_usb::write_all(&data, |_| Ok(0));
    assert!(
        result.is_err(),
        "write_all must return Err on zero-length write"
    );
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("zero-length") || msg.contains("disconnected"),
        "error must mention zero-length or disconnected, got: {msg}"
    );
}

#[test]
fn handshake_payload_is_64_bytes() {
    let payload = bulk_usb::handshake_payload();
    assert_eq!(payload.len(), 64);
    assert_eq!(payload[0], 0x12);
    assert_eq!(payload[1], 0x34);
    assert_eq!(payload[2], 0x56);
    assert_eq!(payload[3], 0x78);
    assert_eq!(payload[56], 0x01);
    // All other bytes are zero
    for (i, byte) in payload.iter().enumerate().take(56).skip(4) {
        assert_eq!(*byte, 0x00, "byte {} should be 0x00", i);
    }
}

#[test]
fn frame_header_is_64_bytes_with_correct_fields() {
    let header = bulk_usb::build_frame_header(2, 480, 480, 12345);
    assert_eq!(header.len(), 64);
    // Magic
    assert_eq!(&header[0..4], &[0x12, 0x34, 0x56, 0x78]);
    // cmd = 2 (JPEG), little-endian u32
    assert_eq!(&header[4..8], &2u32.to_le_bytes());
    // width = 480
    assert_eq!(&header[8..12], &480u32.to_le_bytes());
    // height = 480
    assert_eq!(&header[12..16], &480u32.to_le_bytes());
    // mode = 2 at offset 56
    assert_eq!(&header[56..60], &2u32.to_le_bytes());
    // payload length = 12345 at offset 60
    assert_eq!(&header[60..64], &12345u32.to_le_bytes());
}
