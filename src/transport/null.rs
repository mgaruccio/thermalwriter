// Null transport: discards frames instead of talking to real USB hardware.
// Lets the full daemon (D-Bus, tick loop, sensors, rendering) run headlessly
// for profiling, with no cooler attached. Always compiled (no feature gate)
// so every build variant and CI run exercises the same code path.

use anyhow::Result;
use log::info;
use std::time::Duration;

use super::{DeviceInfo, Transport};

/// Which transport to construct, selected from `THERMALWRITER_TRANSPORT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// Real USB device (`BulkUsb`) — the default.
    Usb,
    /// `NullTransport` — headless, no hardware required.
    Null,
}

/// Select a `TransportKind` from the raw env var value. Pure function (takes
/// the value rather than reading the environment itself) so callers can unit
/// test the selection logic without mutating process-global env state.
pub fn transport_from_env(value: Option<&str>) -> TransportKind {
    match value {
        Some("null") => TransportKind::Null,
        _ => TransportKind::Usb,
    }
}

/// A `Transport` that discards every frame. Used to run the daemon headless
/// (no cooler attached) for profiling: `handshake()` returns a synthetic
/// 480x480 JPEG-capable device, `send_frame` counts frames and drops them.
pub struct NullTransport {
    frames_sent: u64,
    /// Optional artificial per-frame delay from `THERMALWRITER_NULL_LATENCY_MS`,
    /// for simulating USB send cost in headless profiling runs. Off by default.
    latency: Option<Duration>,
}

impl NullTransport {
    pub fn new() -> Self {
        let latency = std::env::var("THERMALWRITER_NULL_LATENCY_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|&ms| ms > 0)
            .map(Duration::from_millis);
        Self {
            frames_sent: 0,
            latency,
        }
    }
}

impl Default for NullTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for NullTransport {
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
        // Caller (tick loop) invokes send_frame via block_in_place, so a
        // blocking sleep here is safe — it won't stall the async runtime.
        if let Some(latency) = self.latency {
            std::thread::sleep(latency);
        }
        self.frames_sent += 1;
        Ok(())
    }

    fn close(&mut self) {
        info!("NullTransport closed: {} frames sent", self.frames_sent);
    }

    // is_connected() (always true) and try_reconnect() (bails) use the trait
    // defaults — NullTransport never disconnects, so reconnect never fires.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_from_env_selects_null_on_exact_match() {
        assert_eq!(transport_from_env(Some("null")), TransportKind::Null);
    }

    #[test]
    fn transport_from_env_defaults_to_usb_when_unset() {
        assert_eq!(transport_from_env(None), TransportKind::Usb);
    }

    #[test]
    fn transport_from_env_defaults_to_usb_on_unrecognized_value() {
        assert_eq!(transport_from_env(Some("bogus")), TransportKind::Usb);
        assert_eq!(transport_from_env(Some("")), TransportKind::Usb);
        assert_eq!(transport_from_env(Some("NULL")), TransportKind::Usb); // case-sensitive, exact match only
    }

    #[test]
    fn handshake_returns_synthetic_480x480_jpeg_device() {
        let mut t = NullTransport::new();
        let info = t.handshake().expect("handshake never fails");
        assert_eq!(info.width, 480);
        assert_eq!(info.height, 480);
        assert!(info.use_jpeg);
    }

    #[test]
    fn send_frame_counts_frames_and_always_succeeds() {
        let mut t = NullTransport {
            frames_sent: 0,
            latency: None,
        };
        for _ in 0..5 {
            t.send_frame(&[0u8; 16]).expect("send_frame never fails");
        }
        assert_eq!(t.frames_sent, 5);
    }

    #[test]
    fn is_connected_and_try_reconnect_use_trait_defaults() {
        let mut t = NullTransport::new();
        assert!(t.is_connected());
        assert!(t.try_reconnect().is_err());
    }
}
