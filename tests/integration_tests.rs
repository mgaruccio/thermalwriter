#![cfg(feature = "daemon")]

use anyhow::Result;
use serial_test::serial;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use thermalwriter::render::background::BackgroundImage;
use thermalwriter::render::{FrameSource, RawFrame, SensorData};
use thermalwriter::service::mode_handler::RuntimeDisplayDimensions;
use thermalwriter::service::tick::{
    BackgroundApply, SourceBuildRequest, SourceBuildResult, SourceRevisionApply,
};
use thermalwriter::transport::discovery::{OpenedDisplay, TransportConnector};
use thermalwriter::transport::{
    DeviceInfo, EncodedFrame, Transport, WireProtocol, build_device_info,
};

fn bulk_info() -> DeviceInfo {
    build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, Some(72)).unwrap()
}

fn hid2_1280_info() -> DeviceInfo {
    build_device_info(WireProtocol::HidType2, 0x0416, 0x5302, 68, 0, Some(192)).unwrap()
}

struct MockTransport {
    frames_sent: Arc<AtomicU32>,
    connected: bool,
}

impl Transport for MockTransport {
    fn handshake(&mut self) -> Result<DeviceInfo> {
        Ok(bulk_info())
    }
    fn send_frame(&mut self, _frame: &EncodedFrame) -> Result<()> {
        self.frames_sent.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn close(&mut self) {}
    fn is_connected(&self) -> bool {
        self.connected
    }
}

struct MockFrameSource;

impl FrameSource for MockFrameSource {
    fn render(&mut self, _sensors: &SensorData) -> Result<RawFrame> {
        Ok(RawFrame {
            data: vec![0u8; 480 * 480 * 3],
            width: 480,
            height: 480,
        })
    }
    fn name(&self) -> &str {
        "mock"
    }
    fn set_template(&mut self, _template: &str) {}
}

struct StreamingMockSource;

impl FrameSource for StreamingMockSource {
    fn render(&mut self, _sensors: &SensorData) -> Result<RawFrame> {
        Ok(RawFrame {
            data: vec![0u8; 480 * 480 * 3],
            width: 480,
            height: 480,
        })
    }

    fn name(&self) -> &str {
        "streaming-mock"
    }

    fn is_streaming(&self) -> bool {
        true
    }
}

struct TemplateTrackingSource {
    applied_tx: Option<tokio::sync::oneshot::Sender<String>>,
}

impl FrameSource for TemplateTrackingSource {
    fn render(&mut self, _sensors: &SensorData) -> Result<RawFrame> {
        Ok(RawFrame {
            data: vec![0u8; 480 * 480 * 3],
            width: 480,
            height: 480,
        })
    }

    fn name(&self) -> &str {
        "template-tracking"
    }

    fn set_template(&mut self, template: &str) {
        if let Some(applied_tx) = self.applied_tx.take() {
            let _ = applied_tx.send(template.to_owned());
        }
    }
}

struct RuntimeDirGuard {
    original: Option<std::ffi::OsString>,
}

impl RuntimeDirGuard {
    fn set(path: &std::path::Path) -> Self {
        let original = std::env::var_os("XDG_RUNTIME_DIR");
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", path);
        }
        Self { original }
    }
}

impl Drop for RuntimeDirGuard {
    fn drop(&mut self) {
        match self.original.take() {
            Some(value) => unsafe { std::env::set_var("XDG_RUNTIME_DIR", value) },
            None => unsafe { std::env::remove_var("XDG_RUNTIME_DIR") },
        }
    }
}

struct BackgroundTrackingSource {
    applied_tx: Option<tokio::sync::oneshot::Sender<Option<Arc<BackgroundImage>>>>,
    release_rx: Option<std::sync::mpsc::Receiver<()>>,
}

impl FrameSource for BackgroundTrackingSource {
    fn render(&mut self, _sensors: &SensorData) -> Result<RawFrame> {
        Ok(RawFrame {
            data: vec![0u8; 480 * 480 * 3],
            width: 480,
            height: 480,
        })
    }

    fn name(&self) -> &str {
        "background-tracking"
    }

    fn set_background(&mut self, background: Option<Arc<BackgroundImage>>) -> Result<()> {
        if let Some(applied_tx) = self.applied_tx.take() {
            let _ = applied_tx.send(background);
        }
        if let Some(release_rx) = self.release_rx.take() {
            release_rx.recv().expect("background apply release");
        }
        Ok(())
    }
}

struct SizedMockSource {
    width: u32,
    height: u32,
}

impl FrameSource for SizedMockSource {
    fn render(&mut self, _sensors: &SensorData) -> Result<RawFrame> {
        let n = (self.width as usize)
            .saturating_mul(self.height as usize)
            .saturating_mul(3);
        Ok(RawFrame {
            data: vec![0u8; n],
            width: self.width,
            height: self.height,
        })
    }
    fn name(&self) -> &str {
        "sized-mock"
    }
}

/// Counts renders and exposes a controllable content fingerprint so the tick
/// loop's dirty-frame skip can be observed without real sensor data.
struct CountingFrameSource {
    renders: Arc<AtomicU32>,
    /// When Some, returned as the content fingerprint. Tests mutate via interior
    /// Arc to force a dirty frame mid-run.
    fingerprint: Arc<std::sync::atomic::AtomicU64>,
    /// When true, content_fingerprint returns None (always dirty).
    always_dirty: bool,
}

impl FrameSource for CountingFrameSource {
    fn render(&mut self, _sensors: &SensorData) -> Result<RawFrame> {
        self.renders.fetch_add(1, Ordering::Relaxed);
        Ok(RawFrame {
            data: vec![0u8; 480 * 480 * 3],
            width: 480,
            height: 480,
        })
    }

    fn name(&self) -> &str {
        "counting"
    }

    fn content_fingerprint(&self, _sensors: &SensorData) -> Option<u64> {
        if self.always_dirty {
            None
        } else {
            Some(self.fingerprint.load(Ordering::Relaxed))
        }
    }
}

fn test_connector() -> TransportConnector {
    TransportConnector::from_config_device("auto").expect("auto selector")
}

/// Signals shutdown when dropped so panic paths cannot leak the tick task.
struct ShutdownOnDrop(tokio::sync::watch::Sender<bool>);
impl Drop for ShutdownOnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(true);
    }
}

async fn source_build_helper(
    mut req_rx: tokio::sync::mpsc::Receiver<SourceBuildRequest>,
    result_tx: tokio::sync::mpsc::Sender<SourceBuildResult>,
) {
    while let Some(req) = req_rx.recv().await {
        let (width, height) = req.canvases.first().copied().unwrap_or((480, 480));
        let sources: Result<Vec<Box<dyn FrameSource>>, String> =
            Ok(vec![Box::new(SizedMockSource { width, height })]);
        if result_tx
            .send(SourceBuildResult {
                generation: req.generation,
                sources,
                source_revision: 0,
                commit: None,
            })
            .await
            .is_err()
        {
            break;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_mock_tick(
    frames_sent: Arc<AtomicU32>,
    frame_source: Box<dyn FrameSource>,
    source_build_tx: tokio::sync::mpsc::Sender<SourceBuildRequest>,
    mut source_result_rx: tokio::sync::mpsc::Receiver<SourceBuildResult>,
    template_rx: tokio::sync::watch::Receiver<String>,
    bg_rx: tokio::sync::watch::Receiver<Option<Arc<BackgroundImage>>>,
    mut background_apply_rx: tokio::sync::mpsc::Receiver<BackgroundApply>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    connected_tx: tokio::sync::watch::Sender<bool>,
    display_tx: tokio::sync::watch::Sender<RuntimeDisplayDimensions>,
    display_count_tx: tokio::sync::watch::Sender<u32>,
    generation_tx: tokio::sync::watch::Sender<u64>,
    mut source_revision_rx: tokio::sync::mpsc::Receiver<
        thermalwriter::service::tick::SourceRevisionApply,
    >,
    tick_rate_rx: tokio::sync::watch::Receiver<u32>,
    fps: u32,
) {
    use thermalwriter::sensor::SensorHub;
    use thermalwriter::service::tick::run_tick_loop;

    let (_needed_tx, needed_rx) =
        tokio::sync::watch::channel::<Option<std::collections::HashSet<String>>>(None);
    let (_recipe_tx, recipe_rx) =
        tokio::sync::watch::channel::<Option<thermalwriter::sensor::LayoutSensorRecipe>>(None);

    let mut hub = SensorHub::new();
    let outputs = Some(vec![OpenedDisplay {
        transport: Box::new(MockTransport {
            frames_sent,
            connected: true,
        }),
        info: bulk_info(),
    }]);
    run_tick_loop(
        outputs,
        Some(bulk_info()),
        test_connector(),
        vec![frame_source],
        false,
        source_build_tx,
        &mut source_result_rx,
        &mut hub,
        fps,
        85,
        vec![0],
        template_rx,
        bg_rx,
        &mut background_apply_rx,
        shutdown_rx,
        None,
        std::time::Duration::from_millis(500),
        None,
        connected_tx,
        display_tx,
        display_count_tx,
        generation_tx,
        &mut source_revision_rx,
        tick_rate_rx,
        needed_rx,
        recipe_rx,
    )
    .await
    .unwrap();
}

#[allow(clippy::too_many_arguments)]
fn spawn_tick(
    frames_sent: Arc<AtomicU32>,
    source_build_tx: tokio::sync::mpsc::Sender<SourceBuildRequest>,
    source_result_rx: tokio::sync::mpsc::Receiver<SourceBuildResult>,
    template_rx: tokio::sync::watch::Receiver<String>,
    bg_rx: tokio::sync::watch::Receiver<Option<Arc<BackgroundImage>>>,
    bg_apply_rx: tokio::sync::mpsc::Receiver<BackgroundApply>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    connected_tx: tokio::sync::watch::Sender<bool>,
    display_tx: tokio::sync::watch::Sender<RuntimeDisplayDimensions>,
    display_count_tx: tokio::sync::watch::Sender<u32>,
    generation_tx: tokio::sync::watch::Sender<u64>,
    tick_rate_rx: tokio::sync::watch::Receiver<u32>,
    fps: u32,
) -> tokio::task::JoinHandle<()> {
    let (_source_revision_tx, source_revision_rx) = tokio::sync::mpsc::channel(4);
    spawn_tick_with_source_revision(
        frames_sent,
        source_build_tx,
        source_result_rx,
        template_rx,
        bg_rx,
        bg_apply_rx,
        shutdown_rx,
        connected_tx,
        display_tx,
        display_count_tx,
        generation_tx,
        source_revision_rx,
        tick_rate_rx,
        fps,
    )
}

#[allow(clippy::too_many_arguments)]
fn spawn_tick_with_source_revision(
    frames_sent: Arc<AtomicU32>,
    source_build_tx: tokio::sync::mpsc::Sender<SourceBuildRequest>,
    source_result_rx: tokio::sync::mpsc::Receiver<SourceBuildResult>,
    template_rx: tokio::sync::watch::Receiver<String>,
    bg_rx: tokio::sync::watch::Receiver<Option<Arc<BackgroundImage>>>,
    bg_apply_rx: tokio::sync::mpsc::Receiver<BackgroundApply>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    connected_tx: tokio::sync::watch::Sender<bool>,
    display_tx: tokio::sync::watch::Sender<RuntimeDisplayDimensions>,
    display_count_tx: tokio::sync::watch::Sender<u32>,
    generation_tx: tokio::sync::watch::Sender<u64>,
    source_revision_rx: tokio::sync::mpsc::Receiver<
        thermalwriter::service::tick::SourceRevisionApply,
    >,
    tick_rate_rx: tokio::sync::watch::Receiver<u32>,
    fps: u32,
) -> tokio::task::JoinHandle<()> {
    let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_time()
                .build()
                .unwrap();
            rt.block_on(async {
                run_mock_tick(
                    frames_sent,
                    Box::new(MockFrameSource),
                    source_build_tx,
                    source_result_rx,
                    template_rx,
                    bg_rx,
                    bg_apply_rx,
                    shutdown_rx,
                    connected_tx,
                    display_tx,
                    display_count_tx,
                    generation_tx,
                    source_revision_rx,
                    tick_rate_rx,
                    fps,
                )
                .await;
            })
        }));
        let _ = finished_tx.send(result);
    });
    tokio::spawn(async move {
        match finished_rx
            .await
            .expect("tick thread exited without reporting")
        {
            Ok(()) => {}
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
}

#[test]
fn jpeg_encode_produces_valid_output() {
    use thermalwriter::service::tick::encode_jpeg;
    let frame = RawFrame {
        data: vec![128u8; 480 * 480 * 3],
        width: 480,
        height: 480,
    };
    let jpeg = encode_jpeg(&frame, 85, 0).unwrap();
    assert!(jpeg.len() > 100);
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
}

#[test]
fn jpeg_encode_rejects_malformed_rgb_before_rotation() {
    use thermalwriter::service::tick::encode_jpeg;
    let frame = RawFrame {
        data: vec![0; 2 * 2 * 3 - 1],
        width: 2,
        height: 2,
    };

    let error = encode_jpeg(&frame, 85, 90).unwrap_err();
    assert!(
        error.to_string().contains("raw RGB payload length 11"),
        "{error:#}"
    );
}

#[test]
fn jpeg_quality_affects_size() {
    use thermalwriter::service::tick::encode_jpeg;
    let frame = RawFrame {
        data: {
            let mut d = vec![0u8; 480 * 480 * 3];
            for (i, b) in d.iter_mut().enumerate() {
                *b = (i % 256) as u8;
            }
            d
        },
        width: 480,
        height: 480,
    };
    let jpeg_high = encode_jpeg(&frame, 95, 0).unwrap();
    let jpeg_low = encode_jpeg(&frame, 10, 0).unwrap();
    assert!(jpeg_high.len() > jpeg_low.len());
}

#[tokio::test(flavor = "multi_thread")]
async fn tick_loop_sends_frames_and_stops_on_shutdown() {
    let frames_sent = Arc::new(AtomicU32::new(0));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let _guard = ShutdownOnDrop(shutdown_tx.clone());
    let (_template_tx, template_rx) = tokio::sync::watch::channel(String::new());
    let (_bg_tx, bg_rx) = tokio::sync::watch::channel(None);
    let (_bg_apply_tx, bg_apply_rx) = tokio::sync::mpsc::channel(4);
    let (connected_tx, _) = tokio::sync::watch::channel(true);
    let (display_tx, _) = tokio::sync::watch::channel(RuntimeDisplayDimensions::new(480, 480));
    let (display_count_tx, _) = tokio::sync::watch::channel(1u32);
    let (generation_tx, _) = tokio::sync::watch::channel(0u64);
    let (_tick_tx, tick_rate_rx) = tokio::sync::watch::channel(30u32);
    let (source_build_tx, source_build_rx) = tokio::sync::mpsc::channel(4);
    let (source_result_tx, source_result_rx) = tokio::sync::mpsc::channel(4);
    let helper = tokio::spawn(source_build_helper(source_build_rx, source_result_tx));

    let handle = spawn_tick(
        Arc::clone(&frames_sent),
        source_build_tx,
        source_result_rx,
        template_rx,
        bg_rx,
        bg_apply_rx,
        shutdown_rx,
        connected_tx,
        display_tx,
        display_count_tx,
        generation_tx,
        tick_rate_rx,
        30,
    );

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let _ = shutdown_tx.send(true);
    handle.await.unwrap();
    helper.abort();
    assert!(frames_sent.load(Ordering::Relaxed) > 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn tick_loop_mirrors_to_two_different_display_profiles() {
    use thermalwriter::sensor::SensorHub;
    use thermalwriter::service::tick::run_tick_loop;

    let primary_frames = Arc::new(AtomicU32::new(0));
    let secondary_frames = Arc::new(AtomicU32::new(0));
    let outputs = vec![
        OpenedDisplay {
            transport: Box::new(MockTransport {
                frames_sent: Arc::clone(&primary_frames),
                connected: true,
            }),
            info: bulk_info(),
        },
        OpenedDisplay {
            transport: Box::new(MockTransport {
                frames_sent: Arc::clone(&secondary_frames),
                connected: true,
            }),
            info: hid2_1280_info(),
        },
    ];

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let (_template_tx, template_rx) = tokio::sync::watch::channel(String::new());
    let (_bg_tx, bg_rx) = tokio::sync::watch::channel(None);
    let (_bg_apply_tx, mut bg_apply_rx) = tokio::sync::mpsc::channel(4);
    let (connected_tx, _) = tokio::sync::watch::channel(false);
    let (display_tx, _) = tokio::sync::watch::channel(RuntimeDisplayDimensions::new(0, 0));
    let (display_count_tx, display_count_rx) = tokio::sync::watch::channel(0u32);
    let (generation_tx, _) = tokio::sync::watch::channel(0u64);
    let (_tick_tx, tick_rate_rx) = tokio::sync::watch::channel(30u32);
    let (source_build_tx, _source_build_rx) = tokio::sync::mpsc::channel(4);
    let (_source_result_tx, mut source_result_rx) = tokio::sync::mpsc::channel(4);
    let (_source_revision_tx, mut source_revision_rx) = tokio::sync::mpsc::channel(4);
    let (_needed_tx, needed_rx) =
        tokio::sync::watch::channel::<Option<std::collections::HashSet<String>>>(None);
    let (_recipe_tx, recipe_rx) =
        tokio::sync::watch::channel::<Option<thermalwriter::sensor::LayoutSensorRecipe>>(None);

    let task = tokio::spawn(async move {
        let mut hub = SensorHub::new();
        run_tick_loop(
            Some(outputs),
            Some(bulk_info()),
            test_connector(),
            vec![Box::new(MockFrameSource)],
            false,
            source_build_tx,
            &mut source_result_rx,
            &mut hub,
            30,
            85,
            vec![0],
            template_rx,
            bg_rx,
            &mut bg_apply_rx,
            shutdown_rx,
            None,
            std::time::Duration::from_millis(500),
            None,
            connected_tx,
            display_tx,
            display_count_tx,
            generation_tx,
            &mut source_revision_rx,
            tick_rate_rx,
            needed_rx,
            recipe_rx,
        )
        .await
        .unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(*display_count_rx.borrow(), 2);
    shutdown_tx.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("mirrored tick loop shutdown")
        .unwrap();

    assert!(primary_frames.load(Ordering::Relaxed) > 0);
    assert!(secondary_frames.load(Ordering::Relaxed) > 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn tick_loop_applies_template_updates() {
    let frames_sent = Arc::new(AtomicU32::new(0));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let _guard = ShutdownOnDrop(shutdown_tx.clone());
    let (template_tx, template_rx) = tokio::sync::watch::channel(String::new());
    let (_bg_tx, bg_rx) = tokio::sync::watch::channel(None);
    let (_bg_apply_tx, bg_apply_rx) = tokio::sync::mpsc::channel(4);
    let (connected_tx, _) = tokio::sync::watch::channel(true);
    let (display_tx, _) = tokio::sync::watch::channel(RuntimeDisplayDimensions::new(480, 480));
    let (display_count_tx, _) = tokio::sync::watch::channel(1u32);
    let (generation_tx, mut generation_rx) = tokio::sync::watch::channel(0u64);
    let (_tick_tx, tick_rate_rx) = tokio::sync::watch::channel(20u32);
    let (source_build_tx, source_build_rx) = tokio::sync::mpsc::channel(4);
    let (source_result_tx, source_result_rx) = tokio::sync::mpsc::channel(4);
    let helper = tokio::spawn(source_build_helper(
        source_build_rx,
        source_result_tx.clone(),
    ));

    let handle = spawn_tick(
        frames_sent,
        source_build_tx,
        source_result_rx,
        template_rx,
        bg_rx,
        bg_apply_rx,
        shutdown_rx,
        connected_tx,
        display_tx,
        display_count_tx,
        generation_tx,
        tick_rate_rx,
        20,
    );

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if *generation_rx.borrow() >= 1 {
                break;
            }
            generation_rx.changed().await.unwrap();
        }
    })
    .await
    .expect("generation commit timed out");
    let generation = *generation_rx.borrow();

    let (template_applied_tx, template_applied_rx) = tokio::sync::oneshot::channel();
    let (source_commit_tx, source_commit_rx) = tokio::sync::oneshot::channel();
    source_result_tx
        .send(SourceBuildResult {
            generation,
            sources: Ok(vec![Box::new(TemplateTrackingSource {
                applied_tx: Some(template_applied_tx),
            })]),
            source_revision: 0,
            commit: Some(source_commit_tx),
        })
        .await
        .expect("send template-tracking source");
    tokio::time::timeout(std::time::Duration::from_secs(2), source_commit_rx)
        .await
        .expect("template-tracking source commit timed out")
        .expect("template-tracking source commit channel closed")
        .expect("template-tracking source commit rejected");

    let expected = "updated";
    template_tx
        .send(expected.into())
        .expect("send template update");
    let applied = tokio::time::timeout(std::time::Duration::from_secs(2), template_applied_rx)
        .await
        .expect("template update apply timed out")
        .expect("template update apply channel closed");
    assert_eq!(applied, expected);

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("tick loop shutdown timed out")
        .expect("tick loop task failed");
    helper.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn tick_loop_accepts_generation_tagged_source_swap() {
    let frames_sent = Arc::new(AtomicU32::new(0));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let _guard = ShutdownOnDrop(shutdown_tx.clone());
    let (_template_tx, template_rx) = tokio::sync::watch::channel(String::new());
    let (_bg_tx, bg_rx) = tokio::sync::watch::channel(None);
    let (_bg_apply_tx, bg_apply_rx) = tokio::sync::mpsc::channel(4);
    let (connected_tx, _) = tokio::sync::watch::channel(true);
    let (display_tx, _) = tokio::sync::watch::channel(RuntimeDisplayDimensions::new(480, 480));
    let (display_count_tx, _) = tokio::sync::watch::channel(1u32);
    let (generation_tx, mut generation_rx) = tokio::sync::watch::channel(0u64);
    let (_tick_tx, tick_rate_rx) = tokio::sync::watch::channel(20u32);
    let (source_build_tx, source_build_rx) = tokio::sync::mpsc::channel(4);
    let (source_result_tx, source_result_rx) = tokio::sync::mpsc::channel(4);
    let helper = tokio::spawn(source_build_helper(
        source_build_rx,
        source_result_tx.clone(),
    ));

    let handle = spawn_tick(
        frames_sent,
        source_build_tx,
        source_result_rx,
        template_rx,
        bg_rx,
        bg_apply_rx,
        shutdown_rx,
        connected_tx,
        display_tx,
        display_count_tx,
        generation_tx,
        tick_rate_rx,
        20,
    );

    // Wait for startup generation commit (not a fixed sleep race).
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if *generation_rx.borrow() >= 1 {
                break;
            }
            generation_rx.changed().await.unwrap();
        }
    })
    .await
    .expect("generation commit timed out");

    let generation = *generation_rx.borrow();
    let (matching_commit_tx, matching_commit_rx) = tokio::sync::oneshot::channel();
    let _ = source_result_tx
        .send(SourceBuildResult {
            generation,
            sources: Ok(vec![Box::new(MockFrameSource)]),
            source_revision: 0,
            commit: Some(matching_commit_tx),
        })
        .await;
    let matching_commit =
        tokio::time::timeout(std::time::Duration::from_secs(2), matching_commit_rx)
            .await
            .expect("matching source commit acknowledgement timed out")
            .expect("matching source commit acknowledgement channel closed");
    assert_eq!(matching_commit, Ok(()));
    let (stale_commit_tx, stale_commit_rx) = tokio::sync::oneshot::channel();
    let _ = source_result_tx
        .send(SourceBuildResult {
            generation: generation.saturating_add(99),
            sources: Ok(vec![Box::new(MockFrameSource)]),
            source_revision: 0,
            commit: Some(stale_commit_tx),
        })
        .await;
    let stale_commit = tokio::time::timeout(std::time::Duration::from_secs(2), stale_commit_rx)
        .await
        .expect("stale commit acknowledgement timed out")
        .expect("stale commit acknowledgement channel closed");
    assert!(stale_commit.is_err());
    let _ = shutdown_tx.send(true);
    tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("tick loop shutdown timed out")
        .expect("tick loop task failed");
    helper.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn tick_loop_rejects_stale_same_generation_source_revision() {
    let frames_sent = Arc::new(AtomicU32::new(0));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let _guard = ShutdownOnDrop(shutdown_tx.clone());
    let (template_tx, template_rx) = tokio::sync::watch::channel(String::new());
    let (_bg_tx, bg_rx) = tokio::sync::watch::channel(None);
    let (_bg_apply_tx, bg_apply_rx) = tokio::sync::mpsc::channel(4);
    let (connected_tx, _) = tokio::sync::watch::channel(true);
    let (display_tx, _) = tokio::sync::watch::channel(RuntimeDisplayDimensions::new(480, 480));
    let (display_count_tx, _) = tokio::sync::watch::channel(1u32);
    let (generation_tx, _generation_rx) = tokio::sync::watch::channel(0u64);
    let (_tick_tx, tick_rate_rx) = tokio::sync::watch::channel(20u32);
    let (source_build_tx, source_build_rx) = tokio::sync::mpsc::channel(4);
    let (source_result_tx, source_result_rx) = tokio::sync::mpsc::channel(4);
    let (source_revision_tx, source_revision_rx) = tokio::sync::mpsc::channel(4);
    let helper = tokio::spawn(source_build_helper(
        source_build_rx,
        source_result_tx.clone(),
    ));
    let generation = 1;

    let (old_commit_tx, old_commit_rx) = tokio::sync::oneshot::channel();
    source_result_tx
        .send(SourceBuildResult {
            generation,
            sources: Ok(vec![Box::new(MockFrameSource)]),
            source_revision: 1,
            commit: Some(old_commit_tx),
        })
        .await
        .expect("queue stale source");

    let (revision_ack_tx, revision_ack_rx) = tokio::sync::oneshot::channel();
    source_revision_tx
        .send(SourceRevisionApply {
            revision: 2,
            reset_connection: false,
            ack: revision_ack_tx,
        })
        .await
        .expect("send source revision apply");

    let handle = spawn_tick_with_source_revision(
        frames_sent,
        source_build_tx,
        source_result_rx,
        template_rx,
        bg_rx,
        bg_apply_rx,
        shutdown_rx,
        connected_tx,
        display_tx,
        display_count_tx,
        generation_tx,
        source_revision_rx,
        tick_rate_rx,
        20,
    );

    let revision_ack = tokio::time::timeout(std::time::Duration::from_secs(2), revision_ack_rx)
        .await
        .expect("source revision acknowledgement timed out")
        .expect("source revision acknowledgement channel closed");
    assert_eq!(revision_ack, Ok(()));

    let old_commit = tokio::time::timeout(std::time::Duration::from_secs(2), old_commit_rx)
        .await
        .expect("stale source commit timed out")
        .expect("stale source commit channel closed");
    assert!(old_commit.is_err());

    let (new_template_tx, new_template_rx) = tokio::sync::oneshot::channel();
    let (new_commit_tx, new_commit_rx) = tokio::sync::oneshot::channel();
    source_result_tx
        .send(SourceBuildResult {
            generation,
            sources: Ok(vec![Box::new(TemplateTrackingSource {
                applied_tx: Some(new_template_tx),
            })]),
            source_revision: 2,
            commit: Some(new_commit_tx),
        })
        .await
        .expect("send newer source");
    let new_commit = tokio::time::timeout(std::time::Duration::from_secs(2), new_commit_rx)
        .await
        .expect("newer source commit timed out")
        .expect("newer source commit channel closed");
    assert_eq!(new_commit, Ok(()));

    template_tx
        .send("newer-template".to_owned())
        .expect("send template update");
    let applied_template = tokio::time::timeout(std::time::Duration::from_secs(2), new_template_rx)
        .await
        .expect("newer source template update timed out")
        .expect("newer source template update channel closed");
    assert_eq!(applied_template, "newer-template");

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("tick loop shutdown timed out")
        .expect("tick loop task failed");
    helper.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn tick_loop_acks_stale_source_revision_apply_as_failure() {
    let frames_sent = Arc::new(AtomicU32::new(0));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let _guard = ShutdownOnDrop(shutdown_tx.clone());
    let (_template_tx, template_rx) = tokio::sync::watch::channel(String::new());
    let (_bg_tx, bg_rx) = tokio::sync::watch::channel(None);
    let (_bg_apply_tx, bg_apply_rx) = tokio::sync::mpsc::channel(4);
    let (connected_tx, _) = tokio::sync::watch::channel(true);
    let (display_tx, _) = tokio::sync::watch::channel(RuntimeDisplayDimensions::new(480, 480));
    let (display_count_tx, _) = tokio::sync::watch::channel(1u32);
    let (generation_tx, _generation_rx) = tokio::sync::watch::channel(0u64);
    let (_tick_tx, tick_rate_rx) = tokio::sync::watch::channel(20u32);
    let (source_build_tx, source_build_rx) = tokio::sync::mpsc::channel(4);
    let (source_result_tx, source_result_rx) = tokio::sync::mpsc::channel(4);
    let (source_revision_tx, source_revision_rx) = tokio::sync::mpsc::channel(4);
    let helper = tokio::spawn(source_build_helper(
        source_build_rx,
        source_result_tx.clone(),
    ));

    // Establish current revision = 5 before the stale apply is processed.
    let (fresh_ack_tx, fresh_ack_rx) = tokio::sync::oneshot::channel();
    source_revision_tx
        .send(SourceRevisionApply {
            revision: 5,
            reset_connection: false,
            ack: fresh_ack_tx,
        })
        .await
        .expect("send fresh source revision apply");

    let (stale_ack_tx, stale_ack_rx) = tokio::sync::oneshot::channel();
    source_revision_tx
        .send(SourceRevisionApply {
            revision: 3,
            reset_connection: false,
            ack: stale_ack_tx,
        })
        .await
        .expect("send stale source revision apply");

    let handle = spawn_tick_with_source_revision(
        frames_sent,
        source_build_tx,
        source_result_rx,
        template_rx,
        bg_rx,
        bg_apply_rx,
        shutdown_rx,
        connected_tx,
        display_tx,
        display_count_tx,
        generation_tx,
        source_revision_rx,
        tick_rate_rx,
        20,
    );

    let fresh_ack = tokio::time::timeout(std::time::Duration::from_secs(2), fresh_ack_rx)
        .await
        .expect("fresh source revision acknowledgement timed out")
        .expect("fresh source revision acknowledgement channel closed");
    assert_eq!(fresh_ack, Ok(()));

    let stale_ack = tokio::time::timeout(std::time::Duration::from_secs(2), stale_ack_rx)
        .await
        .expect("stale source revision acknowledgement timed out")
        .expect("stale source revision acknowledgement channel closed");
    assert!(
        stale_ack.is_err(),
        "stale SourceRevisionApply must ack Err, got: {stale_ack:?}"
    );
    let err = stale_ack.unwrap_err();
    assert!(
        err.contains("stale source revision 3"),
        "unexpected stale ack error: {err}"
    );

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("tick loop shutdown timed out")
        .expect("tick loop task failed");
    helper.abort();
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn tick_loop_clears_published_frame_when_streaming_source_is_replaced() {
    use thermalwriter::service::frame_dump;

    let runtime = tempfile::tempdir().unwrap();
    let _runtime_guard = RuntimeDirGuard::set(runtime.path());
    let frames_sent = Arc::new(AtomicU32::new(0));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let _guard = ShutdownOnDrop(shutdown_tx.clone());
    let (_template_tx, template_rx) = tokio::sync::watch::channel(String::new());
    let (_bg_tx, bg_rx) = tokio::sync::watch::channel(None);
    let (_bg_apply_tx, bg_apply_rx) = tokio::sync::mpsc::channel(4);
    let (connected_tx, _) = tokio::sync::watch::channel(true);
    let (display_tx, _) = tokio::sync::watch::channel(RuntimeDisplayDimensions::new(480, 480));
    let (display_count_tx, _) = tokio::sync::watch::channel(1u32);
    let (generation_tx, mut generation_rx) = tokio::sync::watch::channel(0u64);
    let (_tick_tx, tick_rate_rx) = tokio::sync::watch::channel(20u32);
    let (source_build_tx, source_build_rx) = tokio::sync::mpsc::channel(4);
    let (source_result_tx, source_result_rx) = tokio::sync::mpsc::channel(4);
    let helper = tokio::spawn(source_build_helper(
        source_build_rx,
        source_result_tx.clone(),
    ));
    let handle = spawn_tick(
        frames_sent,
        source_build_tx,
        source_result_rx,
        template_rx,
        bg_rx,
        bg_apply_rx,
        shutdown_rx,
        connected_tx,
        display_tx,
        display_count_tx,
        generation_tx,
        tick_rate_rx,
        20,
    );

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if *generation_rx.borrow() >= 1 {
                break;
            }
            generation_rx.changed().await.unwrap();
        }
    })
    .await
    .expect("generation commit timed out");
    let generation = *generation_rx.borrow();

    let (streaming_commit_tx, streaming_commit_rx) = tokio::sync::oneshot::channel();
    source_result_tx
        .send(SourceBuildResult {
            generation,
            sources: Ok(vec![Box::new(StreamingMockSource)]),
            source_revision: 0,
            commit: Some(streaming_commit_tx),
        })
        .await
        .expect("send streaming source");
    tokio::time::timeout(std::time::Duration::from_secs(2), streaming_commit_rx)
        .await
        .expect("streaming source commit timed out")
        .expect("streaming source commit channel closed")
        .expect("streaming source commit rejected");

    let frame_path = frame_dump::frame_path(&runtime.path().join("thermalwriter"));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !frame_path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("streaming source did not publish last.jpg");

    let (replacement_commit_tx, replacement_commit_rx) = tokio::sync::oneshot::channel();
    source_result_tx
        .send(SourceBuildResult {
            generation,
            sources: Ok(vec![Box::new(MockFrameSource)]),
            source_revision: 0,
            commit: Some(replacement_commit_tx),
        })
        .await
        .expect("send non-streaming source");
    tokio::time::timeout(std::time::Duration::from_secs(2), replacement_commit_rx)
        .await
        .expect("non-streaming source commit timed out")
        .expect("non-streaming source commit channel closed")
        .expect("non-streaming source commit rejected");

    assert!(!frame_path.exists());

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("tick loop shutdown timed out")
        .expect("tick loop task failed");
    helper.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn tick_loop_accepts_background_updates() {
    let frames_sent = Arc::new(AtomicU32::new(0));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let _guard = ShutdownOnDrop(shutdown_tx.clone());
    let (_template_tx, template_rx) = tokio::sync::watch::channel(String::new());
    let (bg_tx, bg_rx) = tokio::sync::watch::channel(None);
    let (_bg_apply_tx, bg_apply_rx) = tokio::sync::mpsc::channel(4);
    let (connected_tx, _) = tokio::sync::watch::channel(true);
    let (display_tx, _) = tokio::sync::watch::channel(RuntimeDisplayDimensions::new(480, 480));
    let (display_count_tx, _) = tokio::sync::watch::channel(1u32);
    let (generation_tx, mut generation_rx) = tokio::sync::watch::channel(0u64);
    let (_tick_tx, tick_rate_rx) = tokio::sync::watch::channel(20u32);
    let (source_build_tx, source_build_rx) = tokio::sync::mpsc::channel(4);
    let (source_result_tx, source_result_rx) = tokio::sync::mpsc::channel(4);
    let helper = tokio::spawn(source_build_helper(
        source_build_rx,
        source_result_tx.clone(),
    ));

    let handle = spawn_tick(
        frames_sent,
        source_build_tx,
        source_result_rx,
        template_rx,
        bg_rx,
        bg_apply_rx,
        shutdown_rx,
        connected_tx,
        display_tx,
        display_count_tx,
        generation_tx,
        tick_rate_rx,
        20,
    );

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if *generation_rx.borrow() >= 1 {
                break;
            }
            generation_rx.changed().await.unwrap();
        }
    })
    .await
    .expect("generation commit timed out");
    let generation = *generation_rx.borrow();

    let (seed_background_tx, seed_background_rx) = tokio::sync::oneshot::channel();
    let (seed_commit_tx, seed_commit_rx) = tokio::sync::oneshot::channel();
    source_result_tx
        .send(SourceBuildResult {
            generation,
            sources: Ok(vec![Box::new(BackgroundTrackingSource {
                applied_tx: Some(seed_background_tx),
                release_rx: None,
            })]),
            source_revision: 0,
            commit: Some(seed_commit_tx),
        })
        .await
        .expect("send seed source");
    tokio::time::timeout(std::time::Duration::from_secs(2), seed_commit_rx)
        .await
        .expect("seed source commit timed out")
        .expect("seed source commit channel closed")
        .expect("seed source commit rejected");

    let mut img = image::RgbaImage::new(8, 8);
    for p in img.pixels_mut() {
        *p = image::Rgba([255, 0, 0, 255]);
    }
    let mut cursor = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .unwrap();
    let background = Arc::new(BackgroundImage::decode(&cursor.into_inner()).unwrap());
    let _ = bg_tx.send(Some(Arc::clone(&background)));
    let seed_background =
        tokio::time::timeout(std::time::Duration::from_secs(2), seed_background_rx)
            .await
            .expect("seed background apply timed out")
            .expect("seed background apply channel closed")
            .expect("seed background was cleared");
    assert!(Arc::ptr_eq(&seed_background, &background));

    let (replacement_background_tx, replacement_background_rx) = tokio::sync::oneshot::channel();
    let (replacement_release_tx, replacement_release_rx) = std::sync::mpsc::channel();
    let (replacement_commit_tx, mut replacement_commit_rx) = tokio::sync::oneshot::channel();
    source_result_tx
        .send(SourceBuildResult {
            generation,
            sources: Ok(vec![Box::new(BackgroundTrackingSource {
                applied_tx: Some(replacement_background_tx),
                release_rx: Some(replacement_release_rx),
            })]),
            source_revision: 0,
            commit: Some(replacement_commit_tx),
        })
        .await
        .expect("send replacement source");
    let replacement_background =
        tokio::time::timeout(std::time::Duration::from_secs(2), replacement_background_rx)
            .await
            .expect("replacement background apply timed out")
            .expect("replacement background apply channel closed")
            .expect("replacement background was cleared");
    assert!(Arc::ptr_eq(&replacement_background, &background));
    assert!(matches!(
        replacement_commit_rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    replacement_release_tx
        .send(())
        .expect("release replacement background apply");
    tokio::time::timeout(std::time::Duration::from_secs(2), replacement_commit_rx)
        .await
        .expect("replacement source commit timed out")
        .expect("replacement source commit channel closed")
        .expect("replacement source commit rejected");

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("tick loop shutdown timed out")
        .expect("tick loop task failed");
    helper.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn tick_loop_skips_render_when_content_fingerprint_unchanged() {
    use std::sync::atomic::AtomicU64;
    use thermalwriter::sensor::SensorHub;
    use thermalwriter::service::tick::run_tick_loop;

    let frames_sent = Arc::new(AtomicU32::new(0));
    let renders = Arc::new(AtomicU32::new(0));
    let fingerprint = Arc::new(AtomicU64::new(0xA11CE));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let _guard = ShutdownOnDrop(shutdown_tx.clone());
    let (_template_tx, template_rx) = tokio::sync::watch::channel(String::new());
    let (_bg_tx, bg_rx) = tokio::sync::watch::channel(None);
    let (_bg_apply_tx, mut bg_apply_rx) = tokio::sync::mpsc::channel(4);
    let (connected_tx, _) = tokio::sync::watch::channel(true);
    let (display_tx, _) = tokio::sync::watch::channel(RuntimeDisplayDimensions::new(480, 480));
    let (generation_tx, _) = tokio::sync::watch::channel(0u64);
    let (_tick_tx, tick_rate_rx) = tokio::sync::watch::channel(30u32);
    let (source_build_tx, source_build_rx) = tokio::sync::mpsc::channel(4);
    let (source_result_tx, mut source_result_rx) = tokio::sync::mpsc::channel(4);
    let (_source_revision_tx, mut source_revision_rx) = tokio::sync::mpsc::channel(4);
    let helper = tokio::spawn(source_build_helper(source_build_rx, source_result_tx));

    let source = CountingFrameSource {
        renders: Arc::clone(&renders),
        fingerprint: Arc::clone(&fingerprint),
        always_dirty: false,
    };
    let (_needed_tx, needed_rx) =
        tokio::sync::watch::channel::<Option<std::collections::HashSet<String>>>(None);
    let (_recipe_tx, recipe_rx) =
        tokio::sync::watch::channel::<Option<thermalwriter::sensor::LayoutSensorRecipe>>(None);

    let frames_for_loop = Arc::clone(&frames_sent);
    let handle = tokio::spawn(async move {
        let mut hub = SensorHub::new();
        let outputs = Some(vec![OpenedDisplay {
            transport: Box::new(MockTransport {
                frames_sent: frames_for_loop,
                connected: true,
            }),
            info: bulk_info(),
        }]);
        let (display_count_tx, _) = tokio::sync::watch::channel(1u32);
        run_tick_loop(
            outputs,
            Some(bulk_info()),
            test_connector(),
            vec![Box::new(source)],
            false,
            source_build_tx,
            &mut source_result_rx,
            &mut hub,
            30,
            85,
            vec![0],
            template_rx,
            bg_rx,
            &mut bg_apply_rx,
            shutdown_rx,
            None,
            std::time::Duration::from_secs(60), // never re-poll empty sensors
            None,
            connected_tx,
            display_tx,
            display_count_tx,
            generation_tx,
            &mut source_revision_rx,
            tick_rate_rx,
            needed_rx,
            recipe_rx,
        )
        .await
        .unwrap();
    });

    // Let ~200ms of 30fps ticks elapse (~6 ticks). Only the first should render.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let renders_before = renders.load(Ordering::Relaxed);
    let sent_before = frames_sent.load(Ordering::Relaxed);
    assert_eq!(
        renders_before, 1,
        "unchanged fingerprint must render exactly once, got {renders_before}"
    );
    assert_eq!(
        sent_before, 1,
        "unchanged fingerprint must send exactly once, got {sent_before}"
    );

    // Bust the fingerprint; the next tick must render+send again.
    fingerprint.store(0xBEEF, Ordering::Relaxed);
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    let renders_after = renders.load(Ordering::Relaxed);
    let sent_after = frames_sent.load(Ordering::Relaxed);
    assert!(
        renders_after >= 2,
        "fingerprint change must force a re-render, got {renders_after}"
    );
    assert_eq!(
        sent_after, renders_after,
        "every render must still send; sent={sent_after} renders={renders_after}"
    );

    let _ = shutdown_tx.send(true);
    handle.await.unwrap();
    helper.abort();
}
