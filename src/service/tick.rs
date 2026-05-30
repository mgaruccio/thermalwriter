// Tick loop: polls sensors, renders a frame, encodes to JPEG, sends via transport.

use anyhow::Result;
use image::{ImageBuffer, Rgb};
use log::{debug, info, warn};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::render::{FrameSource, RawFrame};
use crate::sensor::SensorHub;
use crate::sensor::history::SensorHistory;
use crate::service::frame_dump;
use crate::transport::Transport;

/// Rotate raw RGB pixel data by the given degrees (0, 90, 180, 270).
/// Returns (new_data, new_width, new_height).
pub fn rotate_pixels(data: &[u8], width: u32, height: u32, degrees: u16) -> (Vec<u8>, u32, u32) {
    let w = width as usize;
    let h = height as usize;
    let pixel_count = w * h;

    match degrees {
        0 => (data.to_vec(), width, height),
        180 => {
            let mut out = vec![0u8; data.len()];
            for i in 0..pixel_count {
                let src = i * 3;
                let dst = (pixel_count - 1 - i) * 3;
                out[dst..dst + 3].copy_from_slice(&data[src..src + 3]);
            }
            (out, width, height)
        }
        90 => {
            let mut out = vec![0u8; data.len()];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * 3;
                    let dst = (x * h + (h - 1 - y)) * 3;
                    out[dst..dst + 3].copy_from_slice(&data[src..src + 3]);
                }
            }
            (out, height, width)
        }
        270 => {
            let mut out = vec![0u8; data.len()];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * 3;
                    let dst = ((w - 1 - x) * h + y) * 3;
                    out[dst..dst + 3].copy_from_slice(&data[src..src + 3]);
                }
            }
            (out, height, width)
        }
        _ => {
            log::warn!("Unsupported rotation {}, using 0", degrees);
            (data.to_vec(), width, height)
        }
    }
}

/// Encode a RawFrame to JPEG bytes, with optional rotation.
pub fn encode_jpeg(frame: &RawFrame, quality: u8, rotation: u16) -> Result<Vec<u8>> {
    let (rotated, out_w, out_h) = rotate_pixels(&frame.data, frame.width, frame.height, rotation);
    let img: ImageBuffer<Rgb<u8>, _> = ImageBuffer::from_raw(out_w, out_h, rotated)
        .ok_or_else(|| anyhow::anyhow!("Failed to create image buffer"))?;
    let mut buf = std::io::Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    image::DynamicImage::ImageRgb8(img).write_with_encoder(encoder)?;
    Ok(buf.into_inner())
}

/// Run the tick loop. Blocks until `shutdown` is signaled.
///
/// `frame_source`: owned initial frame source (swappable via `source_rx`).
/// `source_rx`: channel for hot-swapping the frame source at runtime.
/// `template_rx`: watch channel carrying updated HTML template strings.
/// `sensor_history`: optional shared history buffer — updated each time sensors are polled.
#[allow(clippy::too_many_arguments)]
pub async fn run_tick_loop(
    transport: &mut dyn Transport,
    mut frame_source: Box<dyn FrameSource>,
    source_rx: &mut tokio::sync::mpsc::Receiver<Box<dyn FrameSource>>,
    sensor_hub: &mut SensorHub,
    tick_rate_fps: u32,
    jpeg_quality: u8,
    rotation: u16,
    mut template_rx: tokio::sync::watch::Receiver<String>,
    mut background_rx: tokio::sync::watch::Receiver<Option<tiny_skia::Pixmap>>,
    shutdown: tokio::sync::watch::Receiver<bool>,
    sensor_history: Option<Arc<Mutex<SensorHistory>>>,
    sensor_poll_interval: Duration,
    connected_tx: tokio::sync::watch::Sender<bool>,
    mut tick_rate_rx: tokio::sync::watch::Receiver<u32>,
) -> Result<()> {
    info!(
        "Tick loop started: {} FPS, JPEG quality={}, rotation={}°",
        tick_rate_fps, jpeg_quality, rotation
    );

    let mut last_poll = Instant::now() - sensor_poll_interval; // poll on first tick
    let mut cached_sensors: HashMap<String, String> = HashMap::new();
    let mut cached_background: Option<tiny_skia::Pixmap> = background_rx.borrow().clone();

    loop {
        let tick_start = Instant::now();

        // Recompute tick duration from latest watch value each iteration so
        // D-Bus set_tick_rate takes effect without a restart.
        let current_fps = (*tick_rate_rx.borrow_and_update()).max(1);
        let tick_duration = Duration::from_secs_f64(1.0 / current_fps as f64);

        // Check shutdown
        if *shutdown.borrow() {
            info!("Tick loop shutdown requested");
            break;
        }

        // Apply background update before source swap so the cache is current
        // when a new source arrives in the same tick.
        if background_rx.has_changed().unwrap_or(false) {
            cached_background = background_rx.borrow_and_update().clone();
            frame_source.set_background(cached_background.clone());
        }

        // Drain all pending source swaps — keep only the latest. Re-apply cached
        // background and clear sensor cache so the new layout doesn't render with
        // stale sensor data from the previous layout.
        let mut latest_source: Option<Box<dyn FrameSource>> = None;
        while let Ok(new_source) = source_rx.try_recv() {
            latest_source = Some(new_source);
        }
        if let Some(new_source) = latest_source {
            info!(
                "Frame source swapped to: {} (queue drained)",
                new_source.name()
            );
            let leaving_streaming = frame_source.is_streaming();
            frame_source = new_source;
            frame_source.set_background(cached_background.clone());
            cached_sensors.clear();

            // If we just left xvfb mode, remove the stale last.jpg so the GUI
            // doesn't display a frozen frame from a previous streaming session.
            if leaving_streaming && !frame_source.is_streaming() {
                frame_dump::clear_frame(&frame_dump::frame_dir());
            }
        }

        // Apply template update if one arrived since last tick
        if template_rx.has_changed().unwrap_or(false) {
            let new_template = template_rx.borrow_and_update().clone();
            if !new_template.is_empty() {
                info!("Applying template update ({} bytes)", new_template.len());
                frame_source.set_template(&new_template);
            }
        }

        // Poll sensors if interval has elapsed (decoupled from render rate)
        let sensors = if tick_start.duration_since(last_poll) >= sensor_poll_interval {
            let data = sensor_hub.poll();
            // Record into history buffer if configured
            if let Some(ref hist) = sensor_history
                && let Ok(mut h) = hist.lock()
            {
                h.record(&data);
            }
            cached_sensors = data;
            last_poll = tick_start;
            &cached_sensors
        } else {
            &cached_sensors
        };

        // Render frame
        match frame_source.render(sensors) {
            Ok(frame) => {
                // Encode to JPEG
                match encode_jpeg(&frame, jpeg_quality, rotation) {
                    Ok(jpeg) => {
                        debug!("Frame rendered: {} bytes JPEG", jpeg.len());

                        // Dump last xvfb frame to tmpfs so the GUI Stream tab can
                        // display a live preview.  block_in_place keeps the async
                        // runtime responsive during the file write (same pattern as
                        // the USB send below).
                        if frame_source.is_streaming() {
                            let dir = frame_dump::frame_dir();
                            if let Err(e) = tokio::task::block_in_place(|| {
                                frame_dump::write_frame_atomic(&dir, &jpeg)
                            }) {
                                warn!("frame_dump write failed: {}", e);
                            }
                        }

                        // block_in_place yields the runtime thread pool during the USB
                        // syscall so D-Bus and other async tasks remain responsive even
                        // when a write stalls for the full WRITE_TIMEOUT (5s).
                        if let Err(e) = tokio::task::block_in_place(|| transport.send_frame(&jpeg))
                        {
                            warn!("Failed to send frame: {}", e);
                            if !transport.is_connected() {
                                let _ = connected_tx.send(false);
                                let reconnect_result =
                                    tokio::task::block_in_place(|| transport.try_reconnect());
                                match reconnect_result {
                                    Ok(()) => {
                                        info!("USB device reconnected");
                                        let _ = connected_tx.send(true);
                                    }
                                    Err(e) => {
                                        warn!("USB reconnect failed: {} — will retry next tick", e);
                                        tokio::time::sleep(Duration::from_secs(2)).await;
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => warn!("JPEG encode failed: {}", e),
                }
            }
            Err(e) => warn!("Render failed: {:#}", e),
        }

        // Sleep until next tick
        let elapsed = tick_start.elapsed();
        if elapsed < tick_duration {
            tokio::time::sleep(tick_duration - elapsed).await;
        }

        // Check shutdown again after sleep.
        // unwrap_or(true): if sender is dropped the daemon should exit.
        if shutdown.has_changed().unwrap_or(true) && *shutdown.borrow() {
            break;
        }
    }

    Ok(())
}
