// Tick loop: polls sensors, renders a frame, encodes to JPEG, sends via transport.

use std::time::{Duration, Instant};
use anyhow::Result;
use image::{ImageBuffer, Rgba};
use log::{debug, info, warn};
use tiny_skia::Pixmap;

use crate::render::FrameSource;
use crate::sensor::SensorHub;
use crate::transport::Transport;

/// Encode a tiny-skia Pixmap to JPEG bytes.
///
/// tiny-skia uses premultiplied RGBA; we de-multiply before JPEG encoding
/// since JPEG doesn't support alpha and the image crate expects straight RGB(A).
pub fn encode_jpeg(pixmap: &Pixmap, quality: u8) -> Result<Vec<u8>> {
    let width = pixmap.width();
    let height = pixmap.height();
    let data = pixmap.data(); // premultiplied RGBA

    // Convert premultiplied RGBA → straight RGBA
    let mut rgba = Vec::with_capacity(data.len());
    for chunk in data.chunks(4) {
        let a = chunk[3] as u16;
        if a == 0 {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let r = ((chunk[0] as u16 * 255) / a).min(255) as u8;
            let g = ((chunk[1] as u16 * 255) / a).min(255) as u8;
            let b = ((chunk[2] as u16 * 255) / a).min(255) as u8;
            rgba.extend_from_slice(&[r, g, b, chunk[3]]);
        }
    }

    let img: ImageBuffer<Rgba<u8>, _> = ImageBuffer::from_raw(width, height, rgba)
        .ok_or_else(|| anyhow::anyhow!("Failed to create image buffer"))?;

    let mut buf = std::io::Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    image::DynamicImage::ImageRgba8(img).write_with_encoder(encoder)?;

    Ok(buf.into_inner())
}

/// Run the tick loop. Blocks until `shutdown` is signaled.
pub async fn run_tick_loop(
    transport: &mut dyn Transport,
    frame_source: &mut dyn FrameSource,
    sensor_hub: &mut SensorHub,
    tick_rate_fps: u32,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let tick_duration = Duration::from_secs_f64(1.0 / tick_rate_fps as f64);
    info!("Tick loop started: {} FPS ({:?} per tick)", tick_rate_fps, tick_duration);

    loop {
        let tick_start = Instant::now();

        // Check shutdown
        if *shutdown.borrow() {
            info!("Tick loop shutdown requested");
            break;
        }

        // Poll sensors
        let sensors = sensor_hub.poll();

        // Render frame
        match frame_source.render(&sensors) {
            Ok(pixmap) => {
                // Encode to JPEG
                match encode_jpeg(&pixmap, 85) {
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

        // Check shutdown again after sleep
        if shutdown.has_changed().unwrap_or(false) && *shutdown.borrow() {
            break;
        }
    }

    Ok(())
}
