#![cfg(feature = "daemon")]

use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use thermalwriter::render::background::BackgroundImage;
use thermalwriter::render::{FrameSource, RawFrame, SensorData};
use thermalwriter::service::mode_handler::RuntimeDisplayDimensions;
use thermalwriter::service::tick::{BackgroundApply, SourceBuildRequest, SourceBuildResult};
use thermalwriter::transport::discovery::TransportConnector;
use thermalwriter::transport::{
    DeviceInfo, EncodedFrame, Transport, WireProtocol, build_device_info,
};

fn bulk_info() -> DeviceInfo {
    build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, Some(72)).unwrap()
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
        let source: Result<Box<dyn FrameSource>, String> = Ok(Box::new(SizedMockSource {
            width: req.width,
            height: req.height,
        }));
        if result_tx
            .send(SourceBuildResult {
                generation: req.generation,
                source,
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
    generation_tx: tokio::sync::watch::Sender<u64>,
    tick_rate_rx: tokio::sync::watch::Receiver<u32>,
    fps: u32,
) {
    use thermalwriter::sensor::SensorHub;
    use thermalwriter::service::tick::run_tick_loop;

    let mut hub = SensorHub::new();
    let transport: Option<Box<dyn Transport>> = Some(Box::new(MockTransport {
        frames_sent,
        connected: true,
    }));
    run_tick_loop(
        transport,
        Some(bulk_info()),
        test_connector(),
        frame_source,
        source_build_tx,
        &mut source_result_rx,
        &mut hub,
        fps,
        85,
        0,
        template_rx,
        bg_rx,
        &mut background_apply_rx,
        shutdown_rx,
        None,
        std::time::Duration::from_millis(500),
        connected_tx,
        display_tx,
        generation_tx,
        tick_rate_rx,
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
    generation_tx: tokio::sync::watch::Sender<u64>,
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
                    generation_tx,
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
async fn tick_loop_applies_template_updates() {
    let frames_sent = Arc::new(AtomicU32::new(0));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let _guard = ShutdownOnDrop(shutdown_tx.clone());
    let (template_tx, template_rx) = tokio::sync::watch::channel(String::new());
    let (_bg_tx, bg_rx) = tokio::sync::watch::channel(None);
    let (_bg_apply_tx, bg_apply_rx) = tokio::sync::mpsc::channel(4);
    let (connected_tx, _) = tokio::sync::watch::channel(true);
    let (display_tx, _) = tokio::sync::watch::channel(RuntimeDisplayDimensions::new(480, 480));
    let (generation_tx, _) = tokio::sync::watch::channel(0u64);
    let (_tick_tx, tick_rate_rx) = tokio::sync::watch::channel(20u32);
    let (source_build_tx, source_build_rx) = tokio::sync::mpsc::channel(4);
    let (source_result_tx, source_result_rx) = tokio::sync::mpsc::channel(4);
    let helper = tokio::spawn(source_build_helper(source_build_rx, source_result_tx));

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
        generation_tx,
        tick_rate_rx,
        20,
    );

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let _ = template_tx.send("updated".into());
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let _ = shutdown_tx.send(true);
    handle.await.unwrap();
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
    let _ = source_result_tx
        .send(SourceBuildResult {
            generation,
            source: Ok(Box::new(MockFrameSource)),
            commit: None,
        })
        .await;
    let (stale_commit_tx, stale_commit_rx) = tokio::sync::oneshot::channel();
    let _ = source_result_tx
        .send(SourceBuildResult {
            generation: generation.saturating_add(99),
            source: Ok(Box::new(MockFrameSource)),
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
async fn tick_loop_accepts_background_updates() {
    let frames_sent = Arc::new(AtomicU32::new(0));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let _guard = ShutdownOnDrop(shutdown_tx.clone());
    let (_template_tx, template_rx) = tokio::sync::watch::channel(String::new());
    let (bg_tx, bg_rx) = tokio::sync::watch::channel(None);
    let (_bg_apply_tx, bg_apply_rx) = tokio::sync::mpsc::channel(4);
    let (connected_tx, _) = tokio::sync::watch::channel(true);
    let (display_tx, _) = tokio::sync::watch::channel(RuntimeDisplayDimensions::new(480, 480));
    let (generation_tx, _) = tokio::sync::watch::channel(0u64);
    let (_tick_tx, tick_rate_rx) = tokio::sync::watch::channel(20u32);
    let (source_build_tx, source_build_rx) = tokio::sync::mpsc::channel(4);
    let (source_result_tx, source_result_rx) = tokio::sync::mpsc::channel(4);
    let helper = tokio::spawn(source_build_helper(source_build_rx, source_result_tx));

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
        generation_tx,
        tick_rate_rx,
        20,
    );

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let mut img = image::RgbaImage::new(8, 8);
    for p in img.pixels_mut() {
        *p = image::Rgba([255, 0, 0, 255]);
    }
    let mut cursor = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .unwrap();
    let bg = BackgroundImage::decode(&cursor.into_inner()).unwrap();
    let _ = bg_tx.send(Some(Arc::new(bg)));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let _ = shutdown_tx.send(true);
    handle.await.unwrap();
    helper.abort();
}
