//! Render a built-in layout with real sensor data and push to the display.
//! Usage: cargo run --example render_layout [layout_name]
//! Default: system-stats

use anyhow::Result;
use std::thread;
use std::time::Duration;
use thermalrighter::render::{FrameSource, TemplateRenderer};
use thermalrighter::sensor::SensorHub;
use thermalrighter::sensor::hwmon::HwmonProvider;
use thermalrighter::sensor::sysinfo_provider::SysinfoProvider;
use thermalrighter::sensor::amdgpu::AmdGpuProvider;
use thermalrighter::sensor::nvidia::NvidiaProvider;
use thermalrighter::service::tick::encode_jpeg;
use thermalrighter::transport::{Transport, bulk_usb::BulkUsb};

fn main() -> Result<()> {
    env_logger::init();

    let layout_name = std::env::args().nth(1).unwrap_or("system-stats".to_string());

    let template = match layout_name.as_str() {
        "system-stats" => include_str!("../layouts/system-stats.html"),
        "gpu-focus" => include_str!("../layouts/gpu-focus.html"),
        "minimal" => include_str!("../layouts/minimal.html"),
        other => {
            eprintln!("Unknown layout: {}. Use: system-stats, gpu-focus, minimal", other);
            std::process::exit(1);
        }
    };

    // Set up sensors
    let mut hub = SensorHub::new();
    hub.add_provider(Box::new(HwmonProvider::new()));
    hub.add_provider(Box::new(SysinfoProvider::new()));
    hub.add_provider(Box::new(AmdGpuProvider::new()));
    hub.add_provider(Box::new(NvidiaProvider::new()));

    // Poll sensors
    let sensors = hub.poll();
    println!("Sensor readings ({} keys):", sensors.len());
    let mut keys: Vec<_> = sensors.iter().collect();
    keys.sort_by_key(|(k, _)| (*k).clone());
    for (k, v) in &keys {
        println!("  {} = {}", k, v);
    }

    // Render
    let mut renderer = TemplateRenderer::new(template, 480, 480)?;
    let pixmap = renderer.render(&sensors)?;

    // Save un-rotated PNG preview
    let png_path = format!("/tmp/thermalrighter_{}.png", layout_name);
    pixmap.save_png(&png_path)?;
    println!("\nSaved preview (before rotation): {}", png_path);

    // Encode to JPEG with 180° rotation for the device
    let jpeg_data = encode_jpeg(&pixmap, 85, 180)?;
    println!("JPEG encoded: {} bytes (rotated 180°)", jpeg_data.len());

    // Open device and send continuously
    println!("\nOpening device...");
    let mut transport = BulkUsb::new()?;
    let info = transport.handshake()?;
    println!("Device: {}x{}, PM={}", info.width, info.height, info.pm);

    println!("Sending '{}' continuously for 60 seconds — go look at the display!", layout_name);
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(60) {
        transport.send_frame(&jpeg_data)?;
        thread::sleep(Duration::from_millis(500));
    }

    transport.close();
    println!("Done.");
    Ok(())
}
