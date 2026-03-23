// Tick loop: polls sensors, renders a frame, encodes to JPEG, sends via transport.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use anyhow::Result;
use image::{ImageBuffer, Rgb};
use log::{debug, info, warn};
use tiny_skia::Pixmap;

use crate::render::FrameSource;
use crate::sensor::history::SensorHistory;
use crate::sensor::SensorHub;
use crate::transport::Transport;

/// Rotate raw RGBA pixel data by the given degrees (0, 90, 180, 270).
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
                let src = i * 4;
                let dst = (pixel_count - 1 - i) * 4;
                out[dst..dst + 4].copy_from_slice(&data[src..src + 4]);
            }
            (out, width, height)
        }
        90 => {
            let mut out = vec![0u8; data.len()];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * 4;
                    let dst = (x * h + (h - 1 - y)) * 4;
                    out[dst..dst + 4].copy_from_slice(&data[src..src + 4]);
                }
            }
            (out, height, width)
        }
        270 => {
            let mut out = vec![0u8; data.len()];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * 4;
                    let dst = ((w - 1 - x) * h + y) * 4;
                    out[dst..dst + 4].copy_from_slice(&data[src..src + 4]);
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

/// Encode a tiny-skia Pixmap to JPEG bytes, with optional rotation.
///
/// tiny-skia uses premultiplied RGBA; we de-multiply before JPEG encoding
/// since JPEG doesn't support alpha and the image crate expects straight RGB(A).
pub fn encode_jpeg(pixmap: &Pixmap, quality: u8, rotation: u16) -> Result<Vec<u8>> {
    let data = pixmap.data(); // premultiplied RGBA

    // Rotate if needed
    let (rotated, out_w, out_h) = rotate_pixels(data, pixmap.width(), pixmap.height(), rotation);

    // Convert premultiplied RGBA → straight RGB (JPEG has no alpha channel)
    let pixel_count = (out_w * out_h) as usize;
    let mut rgb = Vec::with_capacity(pixel_count * 3);
    for chunk in rotated.chunks(4) {
        let a = chunk[3] as u16;
        if a == 0 {
            rgb.extend_from_slice(&[0, 0, 0]);
        } else {
            let r = ((chunk[0] as u16 * 255) / a).min(255) as u8;
            let g = ((chunk[1] as u16 * 255) / a).min(255) as u8;
            let b = ((chunk[2] as u16 * 255) / a).min(255) as u8;
            rgb.extend_from_slice(&[r, g, b]);
        }
    }

    let img: ImageBuffer<Rgb<u8>, _> = ImageBuffer::from_raw(out_w, out_h, rgb)
        .ok_or_else(|| anyhow::anyhow!("Failed to create image buffer"))?;

    let mut buf = std::io::Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    image::DynamicImage::ImageRgb8(img).write_with_encoder(encoder)?;

    Ok(buf.into_inner())
}

/// Run the tick loop. Blocks until `shutdown` is signaled.
///
/// `template_rx`: watch channel carrying updated HTML template strings.
/// When a new value arrives, `frame_source.set_template()` is called before the next render.
///
/// `sensor_history`: optional shared history buffer — updated each time sensors are polled.
pub async fn run_tick_loop(
    transport: &mut dyn Transport,
    frame_source: &mut dyn FrameSource,
    sensor_hub: &mut SensorHub,
    tick_rate_fps: u32,
    jpeg_quality: u8,
    rotation: u16,
    mut template_rx: tokio::sync::watch::Receiver<String>,
    shutdown: tokio::sync::watch::Receiver<bool>,
    sensor_history: Option<Arc<Mutex<SensorHistory>>>,
    sensor_poll_interval: Duration,
) -> Result<()> {
    let tick_duration = Duration::from_secs_f64(1.0 / tick_rate_fps as f64);
    info!("Tick loop started: {} FPS ({:?} per tick), JPEG quality={}, rotation={}°", tick_rate_fps, tick_duration, jpeg_quality, rotation);

    let mut last_poll = Instant::now() - sensor_poll_interval; // poll on first tick
    let mut cached_sensors: HashMap<String, String> = HashMap::new();

    loop {
        let tick_start = Instant::now();

        // Check shutdown
        if *shutdown.borrow() {
            info!("Tick loop shutdown requested");
            break;
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
            if let Some(ref hist) = sensor_history {
                if let Ok(mut h) = hist.lock() {
                    h.record(&data);
                }
            }
            cached_sensors = data;
            last_poll = tick_start;
            &cached_sensors
        } else {
            &cached_sensors
        };

        // Render frame
        match frame_source.render(sensors) {
            Ok(pixmap) => {
                // Encode to JPEG
                match encode_jpeg(&pixmap, jpeg_quality, rotation) {
                    Ok(jpeg) => {
                        debug!("Frame rendered: {} bytes JPEG", jpeg.len());
                        if let Err(e) = transport.send_frame(&jpeg) {
                            warn!("Failed to send frame: {}", e);
                        }
                    }
                    Err(e) => warn!("JPEG encode failed: {}", e),
                }
            }
            Err(e) => warn!("Render failed: {}", e),
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
