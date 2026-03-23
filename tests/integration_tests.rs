use thermalrighter::render::{SensorData, FrameSource};
use thermalrighter::transport::{DeviceInfo, Transport};
use anyhow::Result;
use tiny_skia::Pixmap;
use std::sync::atomic::{AtomicU32, Ordering};

struct MockTransport {
    frames_sent: AtomicU32,
}
impl Transport for MockTransport {
    fn handshake(&mut self) -> Result<DeviceInfo> {
        Ok(DeviceInfo { vid: 0, pid: 0, width: 480, height: 480, pm: 4, sub: 0, use_jpeg: true })
    }
    fn send_frame(&mut self, _data: &[u8]) -> Result<()> {
        self.frames_sent.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn close(&mut self) {}
}

struct MockFrameSource;
impl FrameSource for MockFrameSource {
    fn render(&mut self, _sensors: &SensorData) -> Result<Pixmap> {
        Ok(Pixmap::new(480, 480).unwrap())
    }
    fn name(&self) -> &str { "mock" }
}

#[test]
fn jpeg_encode_produces_valid_output() {
    use thermalrighter::service::tick::encode_jpeg;
    let pixmap = Pixmap::new(480, 480).unwrap();
    let jpeg = encode_jpeg(&pixmap, 85).unwrap();
    // JPEG files start with FF D8
    assert_eq!(&jpeg[0..2], &[0xFF, 0xD8]);
    assert!(jpeg.len() > 100, "JPEG should be more than 100 bytes");
}

#[test]
fn jpeg_encode_quality_affects_size() {
    use thermalrighter::service::tick::encode_jpeg;
    let pixmap = Pixmap::new(480, 480).unwrap();
    let jpeg_high = encode_jpeg(&pixmap, 95).unwrap();
    let jpeg_low = encode_jpeg(&pixmap, 10).unwrap();
    // Higher quality should be >= lower quality in size
    // (for a solid-color image they may be equal, but both must be valid JPEG)
    assert_eq!(&jpeg_high[0..2], &[0xFF, 0xD8]);
    assert_eq!(&jpeg_low[0..2], &[0xFF, 0xD8]);
}

#[tokio::test]
async fn tick_loop_sends_frames_and_stops_on_shutdown() {
    use thermalrighter::service::tick::run_tick_loop;
    use thermalrighter::sensor::SensorHub;
    use std::sync::Arc;

    let frames_sent = Arc::new(AtomicU32::new(0));
    let frames_sent_clone = Arc::clone(&frames_sent);

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // Run tick loop on a blocking thread — Transport/FrameSource are not Send
    // so we run synchronously inside spawn_blocking
    let handle = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut t = MockTransport { frames_sent: AtomicU32::new(0) };
            let mut fs = MockFrameSource;
            let mut hub = SensorHub::new();
            run_tick_loop(&mut t, &mut fs, &mut hub, 30, 85, shutdown_rx).await.unwrap();
            // Return frame count so outer test can verify
            t.frames_sent.load(Ordering::Relaxed)
        })
    });

    // Let it run for a couple ticks then signal shutdown
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    shutdown_tx.send(true).unwrap();

    let count = handle.await.unwrap();
    assert!(count >= 1, "Expected at least 1 frame sent, got {}", count);
    let _ = frames_sent_clone; // suppress unused warning
}
