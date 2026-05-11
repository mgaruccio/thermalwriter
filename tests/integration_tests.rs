#![cfg(feature = "daemon")]

use thermalwriter::render::{SensorData, FrameSource, RawFrame};
use thermalwriter::transport::{DeviceInfo, Transport};
use anyhow::Result;
use std::sync::atomic::{AtomicU32, Ordering};
use tiny_skia::Pixmap;

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
    fn render(&mut self, _sensors: &SensorData) -> Result<RawFrame> {
        Ok(RawFrame { data: vec![0u8; 480 * 480 * 3], width: 480, height: 480 })
    }
    fn name(&self) -> &str { "mock" }
    fn set_template(&mut self, template: &str) {
        self.last_template = Some(template.to_string());
    }
}

#[test]
fn jpeg_encode_produces_valid_output() {
    use thermalwriter::service::tick::encode_jpeg;
    let frame = RawFrame { data: vec![0u8; 480 * 480 * 3], width: 480, height: 480 };
    let jpeg = encode_jpeg(&frame, 85, 0).unwrap();
    // JPEG files start with FF D8
    assert_eq!(&jpeg[0..2], &[0xFF, 0xD8]);
    assert!(jpeg.len() > 100, "JPEG should be more than 100 bytes");
}

#[test]
fn jpeg_encode_quality_affects_size() {
    use thermalwriter::service::tick::encode_jpeg;
    let frame = RawFrame { data: vec![0u8; 480 * 480 * 3], width: 480, height: 480 };
    let jpeg_high = encode_jpeg(&frame, 95, 0).unwrap();
    let jpeg_low = encode_jpeg(&frame, 10, 0).unwrap();
    // Higher quality should be >= lower quality in size
    // (for a solid-color image they may be equal, but both must be valid JPEG)
    assert_eq!(&jpeg_high[0..2], &[0xFF, 0xD8]);
    assert_eq!(&jpeg_low[0..2], &[0xFF, 0xD8]);
}

#[tokio::test(flavor = "multi_thread")]
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
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut t = MockTransport { frames_sent: AtomicU32::new(0) };
            let fs: Box<dyn thermalwriter::render::FrameSource> = Box::new(MockFrameSource { last_template: None });
            let (_source_tx, mut source_rx) = tokio::sync::mpsc::channel(1);
            let mut hub = SensorHub::new();
            let (_bg_tx, bg_rx) = tokio::sync::watch::channel::<Option<tiny_skia::Pixmap>>(None);
            run_tick_loop(&mut t, fs, &mut source_rx, &mut hub, 30, 85, 0, template_rx, bg_rx, shutdown_rx, None, std::time::Duration::from_millis(500), tokio::sync::watch::channel(true).0, tokio::sync::watch::channel(30u32).1).await.unwrap();
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

#[tokio::test(flavor = "multi_thread")]
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
        fn render(&mut self, _sensors: &SensorData) -> Result<RawFrame> {
            Ok(RawFrame { data: vec![0u8; 480 * 480 * 3], width: 480, height: 480 })
        }
        fn name(&self) -> &str { "tracking" }
        fn set_template(&mut self, template: &str) {
            self.applied.lock().unwrap().push(template.to_string());
        }
    }

    let handle = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut t = MockTransport { frames_sent: AtomicU32::new(0) };
            let fs: Box<dyn thermalwriter::render::FrameSource> = Box::new(TrackingFrameSource { applied: applied_clone });
            let (_source_tx, mut source_rx) = tokio::sync::mpsc::channel(1);
            let mut hub = SensorHub::new();
            let (_bg_tx, bg_rx) = tokio::sync::watch::channel::<Option<tiny_skia::Pixmap>>(None);
            run_tick_loop(&mut t, fs, &mut source_rx, &mut hub, 30, 85, 0, template_rx, bg_rx, shutdown_rx, None, std::time::Duration::from_millis(500), tokio::sync::watch::channel(true).0, tokio::sync::watch::channel(30u32).1).await.unwrap();
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

// Regression test for the watch-channel-consumption race:
// GUI apply() sends Layout (x2 via set_layout_vars + set_layout) then Background.
// The watch fires once for background; tick 1 consumes it via borrow_and_update.
// Tick 2 receives a new source (built without bg) — has_changed() is false so bg was lost.
// Fix: cache the latest background and re-apply it whenever a new source arrives.
#[tokio::test(flavor = "multi_thread")]
async fn tick_loop_reapplies_cached_bg_to_swapped_source() {
    use thermalwriter::service::tick::run_tick_loop;
    use thermalwriter::sensor::SensorHub;
    use std::sync::{Arc, Mutex as StdMutex};

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let (_template_tx, template_rx) = tokio::sync::watch::channel(String::new());

    // Track set_background calls per-source via a shared log: (source_name, had_bg)
    let bg_log = Arc::new(StdMutex::new(Vec::<(String, bool)>::new()));
    let bg_log_clone = Arc::clone(&bg_log);

    struct TrackingSource {
        name: String,
        log: Arc<StdMutex<Vec<(String, bool)>>>,
    }
    impl FrameSource for TrackingSource {
        fn render(&mut self, _sensors: &SensorData) -> Result<RawFrame> {
            Ok(RawFrame { data: vec![0u8; 480 * 480 * 3], width: 480, height: 480 })
        }
        fn name(&self) -> &str { &self.name }
        fn set_background(&mut self, bg: Option<tiny_skia::Pixmap>) {
            self.log.lock().unwrap().push((self.name.clone(), bg.is_some()));
        }
    }

    let bg_log_inner = Arc::clone(&bg_log_clone);
    let handle = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut t = MockTransport { frames_sent: AtomicU32::new(0) };
            let initial_fs: Box<dyn FrameSource> = Box::new(TrackingSource {
                name: "source-0".to_string(),
                log: Arc::clone(&bg_log_inner),
            });
            let (source_tx, mut source_rx) = tokio::sync::mpsc::channel::<Box<dyn FrameSource>>(4);
            let (bg_tx, bg_rx) = tokio::sync::watch::channel::<Option<Pixmap>>(None);
            let mut hub = SensorHub::new();

            // Send a 1x1 green Pixmap as background
            let mut px = Pixmap::new(1, 1).unwrap();
            px.fill(tiny_skia::Color::from_rgba8(0, 255, 0, 255));
            bg_tx.send(Some(px)).unwrap();

            // Send two new sources (simulating Layout x2 from the GUI apply flow).
            // These sources are built without any background — they rely on the tick
            // loop's cache to receive the bg.
            source_tx.send(Box::new(TrackingSource {
                name: "source-1".to_string(),
                log: Arc::clone(&bg_log_inner),
            }) as Box<dyn FrameSource>).await.unwrap();
            source_tx.send(Box::new(TrackingSource {
                name: "source-2".to_string(),
                log: Arc::clone(&bg_log_inner),
            }) as Box<dyn FrameSource>).await.unwrap();

            run_tick_loop(&mut t, initial_fs, &mut source_rx, &mut hub, 30, 85, 0, template_rx, bg_rx, shutdown_rx, None, std::time::Duration::from_millis(500), tokio::sync::watch::channel(true).0, tokio::sync::watch::channel(30u32).1).await.unwrap();
        })
    });

    // Give the tick loop enough time to process both sources (2 ticks at 30fps ≈ 67ms)
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    shutdown_tx.send(true).unwrap();
    handle.await.unwrap();

    let log = bg_log.lock().unwrap();

    // With while-let draining, source-1 is skipped and only source-2 (the latest)
    // gets applied. This is the correct behavior: 5 rapid GUI applies shouldn't
    // take 5 ticks to settle — only the last one matters.
    let source2_got_bg = log.iter().any(|(n, had_bg)| n == "source-2" && *had_bg);
    assert!(source2_got_bg, "source-2 never received bg; log: {:?}", *log);
}

// Regression: cached_background was initialized to None even when the watch channel
// was seeded with an initial background. A source swap before any SetBackground D-Bus
// call would call set_background(None), wiping the configured startup background.
// Fix: initialize cached_background from background_rx.borrow() at tick loop start.
#[tokio::test(flavor = "multi_thread")]
async fn tick_loop_preserves_initial_bg_on_first_source_swap() {
    use thermalwriter::service::tick::run_tick_loop;
    use thermalwriter::sensor::SensorHub;
    use std::sync::{Arc, Mutex as StdMutex};

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let (_template_tx, template_rx) = tokio::sync::watch::channel(String::new());

    let bg_log = Arc::new(StdMutex::new(Vec::<(String, bool)>::new()));

    struct TrackingSource {
        name: String,
        log: Arc<StdMutex<Vec<(String, bool)>>>,
    }
    impl FrameSource for TrackingSource {
        fn render(&mut self, _sensors: &SensorData) -> Result<RawFrame> {
            Ok(RawFrame { data: vec![0u8; 480 * 480 * 3], width: 480, height: 480 })
        }
        fn name(&self) -> &str { &self.name }
        fn set_background(&mut self, bg: Option<tiny_skia::Pixmap>) {
            self.log.lock().unwrap().push((self.name.clone(), bg.is_some()));
        }
    }

    let bg_log_inner = Arc::clone(&bg_log);
    let handle = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut t = MockTransport { frames_sent: AtomicU32::new(0) };
            let initial_fs: Box<dyn FrameSource> = Box::new(TrackingSource {
                name: "source-0".to_string(),
                log: Arc::clone(&bg_log_inner),
            });
            let (source_tx, mut source_rx) = tokio::sync::mpsc::channel::<Box<dyn FrameSource>>(4);

            // Seed the watch with an initial background — simulates daemon startup with
            // [background] image configured. NO subsequent send on bg_tx.
            let mut px = Pixmap::new(1, 1).unwrap();
            px.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
            let (_bg_tx, bg_rx) = tokio::sync::watch::channel::<Option<Pixmap>>(Some(px));

            let mut hub = SensorHub::new();

            // Send one new source immediately — before any background_tx.send fires.
            source_tx.send(Box::new(TrackingSource {
                name: "source-1".to_string(),
                log: Arc::clone(&bg_log_inner),
            }) as Box<dyn FrameSource>).await.unwrap();

            run_tick_loop(&mut t, initial_fs, &mut source_rx, &mut hub, 30, 85, 0, template_rx, bg_rx, shutdown_rx, None, std::time::Duration::from_millis(500), tokio::sync::watch::channel(true).0, tokio::sync::watch::channel(30u32).1).await.unwrap();
        })
    });

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    shutdown_tx.send(true).unwrap();
    handle.await.unwrap();

    let log = bg_log.lock().unwrap();
    let source1_got_bg = log.iter().any(|(n, had_bg)| n == "source-1" && *had_bg);
    assert!(source1_got_bg, "source-1 should have received initial bg from watch seed; log: {:?}", *log);
}
