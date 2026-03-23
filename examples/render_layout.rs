//! Render a layout with real sensor data and push to the display.
//! Usage:
//!   cargo run --example render_layout [name_or_path] [seconds] [--mock]
//!
//! Examples:
//!   cargo run --example render_layout neon-dash            # live sensors, 30s
//!   cargo run --example render_layout fps-hero 15 --mock   # mock gaming data, 15s
//!   cargo run --example render_layout layouts/my.html      # file path, live, 30s

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::thread;
use std::time::Duration;
use thermalwriter::render::{FrameSource, SensorData, TemplateRenderer};
use thermalwriter::render::svg::SvgRenderer;
use thermalwriter::sensor::SensorHub;
use thermalwriter::sensor::hwmon::HwmonProvider;
use thermalwriter::sensor::sysinfo_provider::SysinfoProvider;
use thermalwriter::sensor::amdgpu::AmdGpuProvider;
use thermalwriter::sensor::nvidia::NvidiaProvider;
use thermalwriter::sensor::rapl::RaplProvider;
use thermalwriter::service::tick::encode_jpeg;
use thermalwriter::transport::{Transport, bulk_usb::BulkUsb};

/// Returns (content, display_name, is_svg).
fn load_template(name_or_path: &str) -> Result<(String, String, bool)> {
    // Try as file path first (absolute or relative)
    let path = Path::new(name_or_path);
    if path.exists() && path.is_file() {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let display_name = path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("custom")
            .to_string();
        let is_svg = path.extension().is_some_and(|e| e == "svg");
        return Ok((content, display_name, is_svg));
    }

    // Try as layouts/svg/<name>.svg
    let svg_path = format!("layouts/svg/{}.svg", name_or_path);
    let path = Path::new(&svg_path);
    if path.exists() && path.is_file() {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        return Ok((content, name_or_path.to_string(), true));
    }

    // Try as layouts/<name>.html
    let layout_path = format!("layouts/{}.html", name_or_path);
    let path = Path::new(&layout_path);
    if path.exists() && path.is_file() {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        return Ok((content, name_or_path.to_string(), false));
    }

    // Fall back to built-in layouts
    match name_or_path {
        "system-stats" => Ok((include_str!("../layouts/system-stats.html").to_string(), "system-stats".to_string(), false)),
        "gpu-focus" => Ok((include_str!("../layouts/gpu-focus.html").to_string(), "gpu-focus".to_string(), false)),
        "minimal" => Ok((include_str!("../layouts/minimal.html").to_string(), "minimal".to_string(), false)),
        other => anyhow::bail!(
            "Layout not found: '{}'\n\nUsage: cargo run --example render_layout [name_or_path] [seconds] [--mock]",
            other
        ),
    }
}

/// Mock sensor data simulating a gaming session under load.
fn mock_sensors() -> SensorData {
    let mut m = HashMap::new();
    m.insert("cpu_temp".into(), "67".into());
    m.insert("cpu_util".into(), "42".into());
    m.insert("gpu_temp".into(), "71".into());
    m.insert("gpu_util".into(), "87".into());
    m.insert("gpu_power".into(), "285".into());
    m.insert("ram_used".into(), "24.2".into());
    m.insert("ram_total".into(), "60.4".into());
    m.insert("vram_used".into(), "9.8".into());
    m.insert("vram_total".into(), "15.9".into());
    m.insert("fps".into(), "144".into());
    m.insert("frametime".into(), "6.9".into());
    m
}

fn main() -> Result<()> {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    let use_mock = args.iter().any(|a| a == "--mock");
    let positional: Vec<&str> = args[1..].iter()
        .filter(|a| !a.starts_with("--"))
        .map(|s| s.as_str())
        .collect();

    let name_or_path = positional.first().copied().unwrap_or("system-stats");
    let duration_secs: u64 = positional.get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);

    let (template, display_name, is_svg) = load_template(name_or_path)?;

    // Set up sensors (even in mock mode, for merging real data with mock overrides)
    let mut hub = SensorHub::new();
    hub.add_provider(Box::new(HwmonProvider::new()));
    hub.add_provider(Box::new(SysinfoProvider::new()));
    hub.add_provider(Box::new(AmdGpuProvider::new()));
    hub.add_provider(Box::new(NvidiaProvider::new()));
    hub.add_provider(Box::new(RaplProvider::new()));

    // Prime RAPL (needs two readings for delta)
    hub.poll();
    thread::sleep(Duration::from_millis(250));

    let initial_sensors = if use_mock {
        let mock = mock_sensors();
        println!("Using MOCK sensor data (gaming load):");
        let mut keys: Vec<_> = mock.iter().collect();
        keys.sort_by_key(|(k, _)| (*k).clone());
        for (k, v) in &keys {
            println!("  {} = {}", k, v);
        }
        mock
    } else {
        let sensors = hub.poll();
        println!("Sensor readings ({} keys):", sensors.len());
        let mut keys: Vec<_> = sensors.iter().collect();
        keys.sort_by_key(|(k, _)| (*k).clone());
        for (k, v) in &keys {
            println!("  {} = {}", k, v);
        }
        sensors
    };

    // Render initial frame
    let mut renderer: Box<dyn FrameSource> = if is_svg {
        println!("Using SVG renderer");
        Box::new(SvgRenderer::new(&template, 480, 480)?)
    } else {
        Box::new(TemplateRenderer::new(&template, 480, 480)?)
    };
    let pixmap = renderer.render(&initial_sensors)?;

    let png_path = format!("/tmp/thermalwriter_{}.png", display_name);
    pixmap.save_png(&png_path)?;
    println!("\nSaved preview: {}", png_path);

    let jpeg_data = encode_jpeg(&pixmap, 85, 180)?;
    println!("JPEG encoded: {} bytes (rotated 180°)", jpeg_data.len());

    // Open device and send continuously
    println!("\nOpening device...");
    let mut transport = BulkUsb::new()?;
    let info = transport.handshake()?;
    println!("Device: {}x{}, PM={}", info.width, info.height, info.pm);

    let mode = if use_mock { "mock" } else { "live" };
    println!("Sending '{}' ({}) for {}s — go look at the display!", display_name, mode, duration_secs);
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(duration_secs) {
        let sensors = if use_mock {
            mock_sensors()
        } else {
            hub.poll()
        };
        let pixmap = renderer.render(&sensors)?;
        let jpeg_data = encode_jpeg(&pixmap, 85, 180)?;
        transport.send_frame(&jpeg_data)?;
        thread::sleep(Duration::from_millis(500));
    }

    transport.close();
    println!("Done.");
    Ok(())
}
