// SPDX-License-Identifier: GPL-3.0-or-later
//
// Null transport: discards frames instead of talking to real USB hardware.
// Lets the full daemon (D-Bus, tick loop, sensors, rendering) run headlessly
// for profiling, with no cooler attached. Always compiled (no feature gate)
// so every build variant and CI run exercises the same code path.

use anyhow::{Context, Result};
use log::{info, warn};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use super::profile::{DeviceInfo, device_info_from_fixture};
use super::{EncodedFrame, Transport};

/// Which transport to construct, selected from `THERMALWRITER_TRANSPORT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// Real USB/SCSI/HID/LY device — the default.
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
/// (no cooler attached) for profiling or fixture-driven E2E.
pub struct NullTransport {
    frames_sent: u64,
    /// Optional artificial per-frame delay from `THERMALWRITER_NULL_LATENCY_MS`.
    latency: Option<Duration>,
    /// Synthetic negotiated device info.
    info: DeviceInfo,
    /// Optional capture directory for frame dumps (`THERMALWRITER_CAPTURE_DIR`).
    capture_dir: Option<PathBuf>,
}

impl NullTransport {
    pub fn new() -> Self {
        let info = device_info_from_fixture("bulk-87ad-70db-pm4-sub5-fbl72")
            .expect("built-in bulk fixture must resolve");
        Self::with_profile(info)
    }

    pub fn with_profile(info: DeviceInfo) -> Self {
        let latency = std::env::var("THERMALWRITER_NULL_LATENCY_MS")
            .ok()
            .and_then(|v| match v.parse::<u64>() {
                Ok(ms) => Some(ms),
                Err(_) => {
                    warn!(
                        "THERMALWRITER_NULL_LATENCY_MS={v:?} is not a valid u64; ignoring (no artificial latency)"
                    );
                    None
                }
            })
            .filter(|&ms| ms > 0)
            .map(Duration::from_millis);

        let capture_dir = std::env::var("THERMALWRITER_CAPTURE_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);

        Self {
            frames_sent: 0,
            latency,
            info,
            capture_dir,
        }
    }

    fn capture_frame(&self, sequence: u64, frame: &EncodedFrame) -> Result<()> {
        let Some(dir) = &self.capture_dir else {
            return Ok(());
        };
        fs::create_dir_all(dir).with_context(|| format!("create capture dir {}", dir.display()))?;

        let stem = format!("frame-{sequence:06}");
        let bin_path = dir.join(format!("{stem}.bin"));
        let toml_path = dir.join(format!("{stem}.toml"));
        let bin_tmp = dir.join(format!(".{stem}.bin.tmp"));
        let toml_tmp = dir.join(format!(".{stem}.toml.tmp"));

        {
            let mut f = fs::File::create(&bin_tmp)
                .with_context(|| format!("create {}", bin_tmp.display()))?;
            f.write_all(&frame.data)
                .with_context(|| format!("write {}", bin_tmp.display()))?;
            f.sync_all()?;
        }

        let profile_id = self.info.fixture_id();
        let toml = format!(
            "profile_id = {:?}\nwidth = {}\nheight = {}\nencoding = {:?}\n",
            profile_id,
            frame.width,
            frame.height,
            frame.encoding.as_str()
        );
        {
            let mut f = fs::File::create(&toml_tmp)
                .with_context(|| format!("create {}", toml_tmp.display()))?;
            f.write_all(toml.as_bytes())
                .with_context(|| format!("write {}", toml_tmp.display()))?;
            f.sync_all()?;
        }

        fs::rename(&bin_tmp, &bin_path)
            .with_context(|| format!("rename {} -> {}", bin_tmp.display(), bin_path.display()))?;
        fs::rename(&toml_tmp, &toml_path)
            .with_context(|| format!("rename {} -> {}", toml_tmp.display(), toml_path.display()))?;
        Ok(())
    }
}

impl Default for NullTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for NullTransport {
    fn handshake(&mut self) -> Result<DeviceInfo> {
        Ok(self.info.clone())
    }

    fn send_frame(&mut self, frame: &EncodedFrame) -> Result<()> {
        if let Some(latency) = self.latency {
            std::thread::sleep(latency);
        }
        self.frames_sent += 1;
        if self.frames_sent == 1 {
            info!("NullTransport: first frame sent");
        }
        self.capture_frame(self.frames_sent, frame)?;
        Ok(())
    }

    fn close(&mut self) {
        info!("NullTransport closed: {} frames sent", self.frames_sent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::FrameEncoding;

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
        assert_eq!(transport_from_env(Some("NULL")), TransportKind::Usb);
    }

    #[test]
    fn handshake_returns_synthetic_480x480_jpeg_device() {
        let mut t = NullTransport::new();
        let info = t.handshake().expect("handshake never fails");
        assert_eq!(info.width(), 480);
        assert_eq!(info.height(), 480);
        assert!(info.encoding().is_jpeg());
    }

    #[test]
    fn send_frame_counts_frames_and_always_succeeds() {
        let mut t = NullTransport::new();
        let frame = EncodedFrame {
            data: vec![0u8; 16],
            width: 480,
            height: 480,
            encoding: FrameEncoding::Jpeg,
        };
        for _ in 0..5 {
            t.send_frame(&frame).expect("send_frame never fails");
        }
        assert_eq!(t.frames_sent, 5);
    }

    #[test]
    fn is_connected_uses_trait_default() {
        let t = NullTransport::new();
        assert!(t.is_connected());
    }
}
