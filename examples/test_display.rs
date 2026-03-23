//! Minimal display test — big visible text, correct orientation, continuous send.
//! Usage: cargo run --example test_display

use anyhow::Result;
use std::io::Cursor;
use std::thread;
use std::time::Duration;
use tiny_skia::*;
use fontdue::{Font, FontSettings};
use thermalwriter::transport::{Transport, bulk_usb::BulkUsb};

const WIDTH: u32 = 480;
const HEIGHT: u32 = 480;

fn draw_test_frame() -> Pixmap {
    let mut pixmap = Pixmap::new(WIDTH, HEIGHT).unwrap();

    // Dark background
    let mut bg = Paint::default();
    bg.set_color_rgba8(26, 26, 46, 255); // #1a1a2e
    pixmap.fill_rect(
        Rect::from_xywh(0.0, 0.0, WIDTH as f32, HEIGHT as f32).unwrap(),
        &bg, Transform::identity(), None,
    );

    // Red band at top
    let mut red = Paint::default();
    red.set_color_rgba8(255, 0, 0, 255);
    pixmap.fill_rect(
        Rect::from_xywh(0.0, 0.0, WIDTH as f32, 80.0).unwrap(),
        &red, Transform::identity(), None,
    );

    // Green band in middle
    let mut green = Paint::default();
    green.set_color_rgba8(0, 255, 0, 255);
    pixmap.fill_rect(
        Rect::from_xywh(0.0, 200.0, WIDTH as f32, 80.0).unwrap(),
        &green, Transform::identity(), None,
    );

    // Blue band at bottom
    let mut blue = Paint::default();
    blue.set_color_rgba8(0, 0, 255, 255);
    pixmap.fill_rect(
        Rect::from_xywh(0.0, 400.0, WIDTH as f32, 80.0).unwrap(),
        &blue, Transform::identity(), None,
    );

    // Draw "TOP" text near the red band using fontdue
    let font = Font::from_bytes(
        include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf") as &[u8],
        FontSettings::default(),
    ).unwrap();

    blit_text(&mut pixmap, &font, "RED=TOP", 20.0, 30.0, 48.0, [255, 255, 255]);
    blit_text(&mut pixmap, &font, "GREEN=MID", 20.0, 230.0, 48.0, [0, 0, 0]);
    blit_text(&mut pixmap, &font, "BLUE=BOT", 20.0, 430.0, 48.0, [255, 255, 255]);

    pixmap
}

fn blit_text(pixmap: &mut Pixmap, font: &Font, text: &str, x: f32, y: f32, size: f32, color: [u8; 3]) {
    let mut cx = x;
    for ch in text.chars() {
        let (metrics, bitmap) = font.rasterize(ch, size);
        let gx = cx + metrics.xmin as f32;
        let gy = y + (size - metrics.height as f32 - metrics.ymin as f32);
        for row in 0..metrics.height {
            for col in 0..metrics.width {
                let alpha = bitmap[row * metrics.width + col];
                if alpha == 0 { continue; }
                let px = (gx + col as f32) as i32;
                let py = (gy + row as f32) as i32;
                if px < 0 || py < 0 || px >= WIDTH as i32 || py >= HEIGHT as i32 { continue; }
                let idx = (py as u32 * WIDTH + px as u32) as usize * 4;
                let data = pixmap.data_mut();
                let a = alpha as u16;
                let inv = 255 - a;
                data[idx]     = ((color[0] as u16 * a + data[idx] as u16 * inv) / 255) as u8;
                data[idx + 1] = ((color[1] as u16 * a + data[idx + 1] as u16 * inv) / 255) as u8;
                data[idx + 2] = ((color[2] as u16 * a + data[idx + 2] as u16 * inv) / 255) as u8;
                data[idx + 3] = 255;
            }
        }
        cx += metrics.advance_width;
    }
}

fn rotate_180(pixmap: &Pixmap) -> Vec<u8> {
    let data = pixmap.data();
    let mut rotated = vec![0u8; data.len()];
    // 180° = reverse the entire pixel buffer
    let pixel_count = data.len() / 4;
    for i in 0..pixel_count {
        let src = i * 4;
        let dst = (pixel_count - 1 - i) * 4;
        rotated[dst..dst + 4].copy_from_slice(&data[src..src + 4]);
    }
    rotated
}

fn encode_jpeg(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    // Premultiplied -> straight (tiny-skia uses premultiplied)
    let mut straight = Vec::with_capacity(rgba.len());
    for chunk in rgba.chunks(4) {
        let a = chunk[3] as u16;
        if a == 0 {
            straight.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let r = ((chunk[0] as u16 * 255) / a).min(255) as u8;
            let g = ((chunk[1] as u16 * 255) / a).min(255) as u8;
            let b = ((chunk[2] as u16 * 255) / a).min(255) as u8;
            straight.extend_from_slice(&[r, g, b, chunk[3]]);
        }
    }
    let img: image::ImageBuffer<image::Rgba<u8>, _> =
        image::ImageBuffer::from_raw(width, height, straight)
            .ok_or_else(|| anyhow::anyhow!("bad image buffer"))?;
    let mut buf = Cursor::new(Vec::new());
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 90);
    image::DynamicImage::ImageRgba8(img).write_with_encoder(enc)?;
    Ok(buf.into_inner())
}

fn main() -> Result<()> {
    env_logger::init();

    let pixmap = draw_test_frame();

    // Save normal orientation preview
    pixmap.save_png("/tmp/thermalwriter_test_normal.png")?;
    println!("Saved normal preview: /tmp/thermalwriter_test_normal.png");

    // Flip for device
    let rotated = rotate_180(&pixmap);
    let jpeg = encode_jpeg(&rotated, WIDTH, HEIGHT)?;
    println!("JPEG: {} bytes", jpeg.len());

    // Also save rotated preview
    let mut rotated_pixmap = Pixmap::new(WIDTH, HEIGHT).unwrap();
    rotated_pixmap.data_mut().copy_from_slice(&rotated);
    rotated_pixmap.save_png("/tmp/thermalwriter_test_rotated.png")?;
    println!("Saved rotated preview: /tmp/thermalwriter_test_rotated.png");

    // Open device
    println!("Opening device...");
    let mut transport = BulkUsb::new()?;
    let info = transport.handshake()?;
    println!("Device: {}x{}, PM={}", info.width, info.height, info.pm);

    // Continuously send for 30 seconds
    println!("Sending frames continuously for 30 seconds — go look at the display!");
    let start = std::time::Instant::now();
    let mut count = 0u32;
    while start.elapsed() < Duration::from_secs(30) {
        transport.send_frame(&jpeg)?;
        count += 1;
        thread::sleep(Duration::from_millis(500));
    }
    println!("Sent {} frames in 30 seconds", count);

    transport.close();
    println!("Done.");
    Ok(())
}
