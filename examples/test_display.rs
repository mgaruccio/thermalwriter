//! Minimal display test — big visible text, correct orientation, continuous send.
//! Usage: cargo run --example test_display

use anyhow::{Context, Result};
use fontdue::{Font, FontSettings};
use std::thread;
use std::time::Duration;
use thermalwriter::render::RawFrame;
use thermalwriter::transport::{discovery::TransportConnector, encode::encode_frame};
use tiny_skia::*;

const REFERENCE_SIZE: f32 = 480.0;
const ROTATION: u16 = 180;

fn draw_test_frame(width: u32, height: u32) -> Result<Pixmap> {
    let mut pixmap = Pixmap::new(width, height)
        .with_context(|| format!("failed to allocate test frame {width}x{height}"))?;
    let width_f = width as f32;
    let height_f = height as f32;
    let band_height = height_f / 6.0;
    let scale = width.min(height) as f32 / REFERENCE_SIZE;

    // Dark background
    let mut bg = Paint::default();
    bg.set_color_rgba8(26, 26, 46, 255); // #1a1a2e
    pixmap.fill_rect(
        Rect::from_xywh(0.0, 0.0, width_f, height_f).unwrap(),
        &bg,
        Transform::identity(),
        None,
    );

    // Red band at top
    let mut red = Paint::default();
    red.set_color_rgba8(255, 0, 0, 255);
    pixmap.fill_rect(
        Rect::from_xywh(0.0, 0.0, width_f, band_height).unwrap(),
        &red,
        Transform::identity(),
        None,
    );

    // Green band in middle
    let mut green = Paint::default();
    green.set_color_rgba8(0, 255, 0, 255);
    pixmap.fill_rect(
        Rect::from_xywh(0.0, height_f * 5.0 / 12.0, width_f, band_height).unwrap(),
        &green,
        Transform::identity(),
        None,
    );

    // Blue band at bottom
    let mut blue = Paint::default();
    blue.set_color_rgba8(0, 0, 255, 255);
    pixmap.fill_rect(
        Rect::from_xywh(0.0, height_f * 5.0 / 6.0, width_f, band_height).unwrap(),
        &blue,
        Transform::identity(),
        None,
    );

    // Draw "TOP" text near the red band using fontdue
    let font = Font::from_bytes(
        include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf") as &[u8],
        FontSettings::default(),
    )
    .unwrap();

    blit_text(
        &mut pixmap,
        &font,
        "RED=TOP",
        20.0 * scale,
        height_f * 30.0 / REFERENCE_SIZE,
        48.0 * scale,
        [255, 255, 255],
    );
    blit_text(
        &mut pixmap,
        &font,
        "GREEN=MID",
        20.0 * scale,
        height_f * 230.0 / REFERENCE_SIZE,
        48.0 * scale,
        [0, 0, 0],
    );
    blit_text(
        &mut pixmap,
        &font,
        "BLUE=BOT",
        20.0 * scale,
        height_f * 430.0 / REFERENCE_SIZE,
        48.0 * scale,
        [255, 255, 255],
    );

    Ok(pixmap)
}

fn blit_text(
    pixmap: &mut Pixmap,
    font: &Font,
    text: &str,
    x: f32,
    y: f32,
    size: f32,
    color: [u8; 3],
) {
    let mut cx = x;
    let width = pixmap.width();
    let height = pixmap.height();
    let data = pixmap.data_mut();
    for ch in text.chars() {
        let (metrics, bitmap) = font.rasterize(ch, size);
        let gx = cx + metrics.xmin as f32;
        let gy = y + (size - metrics.height as f32 - metrics.ymin as f32);
        for row in 0..metrics.height {
            for col in 0..metrics.width {
                let alpha = bitmap[row * metrics.width + col];
                if alpha == 0 {
                    continue;
                }
                let px = (gx + col as f32) as i32;
                let py = (gy + row as f32) as i32;
                if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
                    continue;
                }
                let idx = (py as u32 * width + px as u32) as usize * 4;
                let a = alpha as u16;
                let inv = 255 - a;
                data[idx] = ((color[0] as u16 * a + data[idx] as u16 * inv) / 255) as u8;
                data[idx + 1] = ((color[1] as u16 * a + data[idx + 1] as u16 * inv) / 255) as u8;
                data[idx + 2] = ((color[2] as u16 * a + data[idx + 2] as u16 * inv) / 255) as u8;
                data[idx + 3] = 255;
            }
        }
        cx += metrics.advance_width;
    }
}

fn main() -> Result<()> {
    env_logger::init();

    println!("Opening device...");
    let connector = TransportConnector::from_config_device("auto")?;
    let (mut transport, info) = connector.connect()?;
    println!("Device: {}x{}, PM={}", info.width(), info.height(), info.pm);

    let (width, height) = info.oriented_dimensions(ROTATION)?;
    let pixmap = draw_test_frame(width, height)?;
    let frame = RawFrame::from_pixmap(&pixmap);

    frame.save_png("/tmp/thermalwriter_test_normal.png")?;
    println!("Saved normal preview: /tmp/thermalwriter_test_normal.png");

    let encoded = encode_frame(&frame, &info, ROTATION, 90)?;
    println!(
        "Encoded frame: {}x{} {}, {} bytes",
        encoded.width,
        encoded.height,
        encoded.encoding,
        encoded.data.len()
    );

    // Continuously send for 30 seconds
    println!("Sending frames continuously for 30 seconds — go look at the display!");
    let start = std::time::Instant::now();
    let mut count = 0u32;
    while start.elapsed() < Duration::from_secs(30) {
        transport.send_frame(&encoded)?;
        count += 1;
        thread::sleep(Duration::from_millis(500));
    }
    println!("Sent {} frames in 30 seconds", count);

    transport.close();
    println!("Done.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use thermalwriter::transport::{
        DeviceInfo, EncodedFrame, FrameEncoding, device_info_from_fixture,
    };

    fn encode_fixture(profile_id: &str) -> (DeviceInfo, EncodedFrame) {
        let info = device_info_from_fixture(profile_id).expect("fixture must resolve");
        let (width, height) = info
            .oriented_dimensions(ROTATION)
            .expect("fixture dimensions must orient");
        let pixmap = draw_test_frame(width, height).expect("test pattern must render");
        let frame = RawFrame::from_pixmap(&pixmap);
        assert_eq!((frame.width, frame.height), (width, height));
        let encoded = encode_frame(&frame, &info, ROTATION, 90).expect("test pattern must encode");
        (info, encoded)
    }

    #[test]
    fn non_480_jpeg_profile_encodes_at_negotiated_geometry() {
        let (info, encoded) = encode_fixture("bulk-87ad-70db-pm5-sub0-fbl50");

        assert_ne!((info.width(), info.height()), (480, 480));
        assert_eq!(encoded.encoding, FrameEncoding::Jpeg);
        assert_eq!(
            (encoded.width, encoded.height),
            info.wire_dimensions().unwrap()
        );
        let decoded = image::load_from_memory(&encoded.data).expect("JPEG must decode");
        assert_eq!(
            (decoded.width(), decoded.height()),
            (encoded.width, encoded.height)
        );
    }

    #[test]
    fn rgb565_profile_encodes_with_negotiated_format_and_size() {
        let (info, encoded) = encode_fixture("bulk-87ad-70db-pm32-sub0-fbl100");

        assert_eq!(encoded.encoding, FrameEncoding::Rgb565Be);
        assert_eq!(
            (encoded.width, encoded.height),
            info.wire_dimensions().unwrap()
        );
        assert_eq!(
            encoded.data.len(),
            encoded.width as usize * encoded.height as usize * 2
        );
    }
}
