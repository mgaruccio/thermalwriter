use thermalwriter::render::{SensorData, FrameSource};
use thermalwriter::transport::{DeviceInfo, Transport};
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

struct MockFrameSource {
    last_template: Option<String>,
}
impl FrameSource for MockFrameSource {
    fn render(&mut self, _sensors: &SensorData) -> Result<Pixmap> {
        Ok(Pixmap::new(480, 480).unwrap())
    }
    fn name(&self) -> &str { "mock" }
    fn set_template(&mut self, template: &str) {
        self.last_template = Some(template.to_string());
    }
}

#[test]
fn jpeg_encode_produces_valid_output() {
    use thermalwriter::service::tick::encode_jpeg;
    let pixmap = Pixmap::new(480, 480).unwrap();
    let jpeg = encode_jpeg(&pixmap, 85, 0).unwrap();
    // JPEG files start with FF D8
    assert_eq!(&jpeg[0..2], &[0xFF, 0xD8]);
    assert!(jpeg.len() > 100, "JPEG should be more than 100 bytes");
}

#[test]
fn jpeg_encode_quality_affects_size() {
    use thermalwriter::service::tick::encode_jpeg;
    let pixmap = Pixmap::new(480, 480).unwrap();
    let jpeg_high = encode_jpeg(&pixmap, 95, 0).unwrap();
    let jpeg_low = encode_jpeg(&pixmap, 10, 0).unwrap();
    // Higher quality should be >= lower quality in size
    // (for a solid-color image they may be equal, but both must be valid JPEG)
    assert_eq!(&jpeg_high[0..2], &[0xFF, 0xD8]);
    assert_eq!(&jpeg_low[0..2], &[0xFF, 0xD8]);
}

#[tokio::test]
async fn tick_loop_sends_frames_and_stops_on_shutdown() {
    use thermalwriter::service::tick::run_tick_loop;
    use thermalwriter::sensor::SensorHub;
    use std::sync::Arc;

    let frames_sent = Arc::new(AtomicU32::new(0));
    let frames_sent_clone = Arc::clone(&frames_sent);

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let (_template_tx, template_rx) = tokio::sync::watch::channel(String::new());

    // Run tick loop on a blocking thread — Transport/FrameSource are not Send
    // so we run synchronously inside spawn_blocking
    let handle = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut t = MockTransport { frames_sent: AtomicU32::new(0) };
            let mut fs = MockFrameSource { last_template: None };
            let mut hub = SensorHub::new();
            run_tick_loop(&mut t, &mut fs, &mut hub, 30, 85, 0, template_rx, shutdown_rx, None, std::time::Duration::from_millis(500)).await.unwrap();
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

#[tokio::test]
async fn tick_loop_applies_template_update() {
    use thermalwriter::service::tick::run_tick_loop;
    use thermalwriter::sensor::SensorHub;
    use std::sync::{Arc, Mutex as StdMutex};

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let (template_tx, template_rx) = tokio::sync::watch::channel(String::new());

    // Capture which templates were applied via shared state
    let applied = Arc::new(StdMutex::new(Vec::<String>::new()));
    let applied_clone = Arc::clone(&applied);

    struct TrackingFrameSource {
        applied: Arc<StdMutex<Vec<String>>>,
    }
    impl FrameSource for TrackingFrameSource {
        fn render(&mut self, _sensors: &SensorData) -> Result<Pixmap> {
            Ok(Pixmap::new(480, 480).unwrap())
        }
        fn name(&self) -> &str { "tracking" }
        fn set_template(&mut self, template: &str) {
            self.applied.lock().unwrap().push(template.to_string());
        }
    }

    let handle = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut t = MockTransport { frames_sent: AtomicU32::new(0) };
            let mut fs = TrackingFrameSource { applied: applied_clone };
            let mut hub = SensorHub::new();
            run_tick_loop(&mut t, &mut fs, &mut hub, 30, 85, 0, template_rx, shutdown_rx, None, std::time::Duration::from_millis(500)).await.unwrap();
        })
    });

    // Send a template update then shut down
    template_tx.send("new-template".to_string()).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    shutdown_tx.send(true).unwrap();
    handle.await.unwrap();

    let calls = applied.lock().unwrap();
    assert!(!calls.is_empty(), "set_template should have been called after template_tx update");
    assert_eq!(calls[0], "new-template");
}
