//! Manual test: sends a solid red 480x480 JPEG frame to the cooler LCD.
//! Run with: cargo run --example send_test_frame
//! Requires the device to be plugged in and accessible.

use anyhow::Result;
use image::{ImageBuffer, Rgb};
use std::io::Cursor;
use std::thread;
use std::time::Duration;
use thermalwriter::transport::{Transport, bulk_usb::BulkUsb};

fn main() -> Result<()> {
    env_logger::init();

    println!("Opening device...");
    let mut transport = BulkUsb::new()?;

    println!("Performing handshake...");
    let info = transport.handshake()?;
    println!("Device: {}x{}, PM={}, JPEG={}", info.width, info.height, info.pm, info.use_jpeg);

    // Create a solid red image
    let img = ImageBuffer::from_fn(info.width, info.height, |_x, _y| {
        Rgb([255u8, 0u8, 0u8])
    });

    // Encode to JPEG
    let mut jpeg_buf = Cursor::new(Vec::new());
    img.write_to(&mut jpeg_buf, image::ImageFormat::Jpeg)?;
    let jpeg_data = jpeg_buf.into_inner();
    println!("JPEG encoded: {} bytes", jpeg_data.len());

    // Send several frames — the device may need more than one to latch
    for i in 1..=5 {
        println!("Sending frame {}/5...", i);
        transport.send_frame(&jpeg_data)?;
        thread::sleep(Duration::from_millis(200));
    }

    println!("Display should show solid red. Holding for 3 seconds...");
    thread::sleep(Duration::from_secs(3));

    transport.close();
    println!("Done.");
    Ok(())
}
