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
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use thermalwriter::render::{FrameSource, SensorData, TemplateRenderer};
use thermalwriter::render::frontmatter::LayoutFrontmatter;
use thermalwriter::render::svg::SvgRenderer;
use thermalwriter::sensor::SensorHub;
use thermalwriter::sensor::history::SensorHistory;
use thermalwriter::sensor::hwmon::HwmonProvider;
use thermalwriter::sensor::sysinfo_provider::SysinfoProvider;
use thermalwriter::sensor::amdgpu::AmdGpuProvider;
use thermalwriter::sensor::nvidia::NvidiaProvider;
use thermalwriter::sensor::rapl::RaplProvider;
use thermalwriter::service::tick::encode_jpeg;
use thermalwriter::theme::ThemePalette;
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

/// Generate mock sensor data with slight variation for history (to make graphs visible).
fn mock_sensors_varying(iteration: u64) -> SensorData {
    let mut m = mock_sensors();
    let phase = (iteration as f64 * 0.3).sin();
    let cpu_util: f64 = 42.0 + phase * 15.0;
    let cpu_temp: f64 = 67.0 + phase * 5.0;
    m.insert("cpu_util".into(), format!("{:.1}", cpu_util));
    m.insert("cpu_temp".into(), format!("{:.0}", cpu_temp));
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

    // Parse frontmatter for history config
    let frontmatter = LayoutFrontmatter::parse(&template);

    // Create sensor history if any metrics are configured in frontmatter
    let sensor_history: Option<Arc<Mutex<SensorHistory>>> = if is_svg && !frontmatter.history_configs.is_empty() {
        let mut history = SensorHistory::new();
        for (metric, cfg) in &frontmatter.history_configs {
            history.configure_metric(metric, cfg.duration);
        }
        let metrics: Vec<String> = frontmatter.history_configs.keys().cloned().collect();
        println!("History tracking: {:?}", metrics);
        Some(Arc::new(Mutex::new(history)))
    } else {
        None
    };

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
        let mut renderer = SvgRenderer::new(&template, 480, 480)?;
        if let Some(ref hist) = sensor_history {
            renderer.set_history(hist.clone());
        }
        renderer.set_theme(ThemePalette::default());
        Box::new(renderer)
    } else {
        Box::new(TemplateRenderer::new(&template, 480, 480)?)
    };

    // Record initial sensors into history
    if let Some(ref hist) = sensor_history {
        if let Ok(mut h) = hist.lock() {
            h.record(&initial_sensors);
        }
    }

    let frame = renderer.render(&initial_sensors)?;

    let png_path = format!("/tmp/thermalwriter_{}.png", display_name);
    frame.save_png(&png_path)?;
    println!("\nSaved preview: {}", png_path);

    let jpeg_data = encode_jpeg(&frame, 85, 180)?;
    println!("JPEG encoded: {} bytes (rotated 180°)", jpeg_data.len());

    // Open device and send continuously
    println!("\nOpening device...");
    let mut transport = BulkUsb::new()?;
    let info = transport.handshake()?;
    println!("Device: {}x{}, PM={}", info.width, info.height, info.pm);

    let mode = if use_mock { "mock" } else { "live" };
    println!("Sending '{}' ({}) for {}s — go look at the display!", display_name, mode, duration_secs);
    let start = std::time::Instant::now();
    let mut iteration = 0u64;
    while start.elapsed() < Duration::from_secs(duration_secs) {
        let sensors = if use_mock {
            mock_sensors_varying(iteration)
        } else {
            hub.poll()
        };

        // Record into history on each poll cycle
        if let Some(ref hist) = sensor_history {
            if let Ok(mut h) = hist.lock() {
                h.record(&sensors);
            }
        }

        let frame = renderer.render(&sensors)?;
        let jpeg_data = encode_jpeg(&frame, 85, 180)?;
        transport.send_frame(&jpeg_data)?;
        thread::sleep(Duration::from_millis(500));
        iteration += 1;
    }

    // Print history stats
    if let Some(ref hist) = sensor_history {
        if let Ok(h) = hist.lock() {
            for metric in h.configured_metrics() {
                let count = h.query(&metric, 10000).len();
                println!("Recorded {} samples for {}", count, metric);
            }
        }
    }

    transport.close();
    println!("Done.");
    Ok(())
}
