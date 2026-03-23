//! Preview a layout rendered via Blitz (full CSS support).
//! Usage:
//!   cargo run --features blitz --example preview_blitz [name_or_path]
//!
//! Examples:
//!   cargo run --features blitz --example preview_blitz neon-dash
//!   cargo run --features blitz --example preview_blitz layouts/dual-gauge.html
//!   cargo run --features blitz --example preview_blitz layouts/blitz-glass.html

use anyhow::{Context, Result};
use std::path::Path;
use thermalwriter::render::blitz::BlitzRenderer;
use thermalwriter::render::FrameSource;
use thermalwriter::sensor::SensorHub;
use thermalwriter::sensor::hwmon::HwmonProvider;
use thermalwriter::sensor::sysinfo_provider::SysinfoProvider;
use thermalwriter::sensor::amdgpu::AmdGpuProvider;
use thermalwriter::sensor::nvidia::NvidiaProvider;
use thermalwriter::sensor::rapl::RaplProvider;

fn load_template(name_or_path: &str) -> Result<(String, String)> {
    // Try as file path first
    let path = Path::new(name_or_path);
    if path.exists() && path.is_file() {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let display_name = path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("custom")
            .to_string();
        return Ok((content, display_name));
    }

    // Try as layouts/<name>.html
    let layout_path = format!("layouts/{}.html", name_or_path);
    let path = Path::new(&layout_path);
    if path.exists() && path.is_file() {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        return Ok((content, name_or_path.to_string()));
    }

    anyhow::bail!(
        "Layout not found: '{}'\n\nUsage: cargo run --features blitz --example preview_blitz [name_or_path]",
        name_or_path
    )
}

fn main() -> Result<()> {
    let name_or_path = std::env::args().nth(1).unwrap_or("neon-dash".to_string());
    let use_mock = std::env::args().any(|a| a == "--mock");

    println!("Rendering '{}' via Blitz...", name_or_path);
    let (template, display_name) = load_template(&name_or_path)?;

    let sensors = if use_mock {
        // Mock gaming-load data for testing
        let mut m = std::collections::HashMap::new();
        m.insert("cpu_temp".into(), "72".into());
        m.insert("cpu_util".into(), "45".into());
        m.insert("gpu_temp".into(), "68".into());
        m.insert("gpu_util".into(), "97".into());
        m.insert("gpu_power".into(), "285".into());
        m.insert("ram_used".into(), "18.2".into());
        m.insert("ram_total".into(), "64.0".into());
        m.insert("vram_used".into(), "10.4".into());
        m.insert("vram_total".into(), "16.0".into());
        m.insert("fps".into(), "144".into());
        m.insert("gpu_clock".into(), "2520".into());
        m.insert("gpu_mem_clock".into(), "1200".into());
        m
    } else {
        let mut hub = SensorHub::new();
        hub.add_provider(Box::new(HwmonProvider::new()));
        hub.add_provider(Box::new(SysinfoProvider::new()));
        hub.add_provider(Box::new(AmdGpuProvider::new()));
        hub.add_provider(Box::new(NvidiaProvider::new()));
        hub.add_provider(Box::new(RaplProvider::new()));
        hub.poll();
        std::thread::sleep(std::time::Duration::from_millis(250));
        hub.poll()
    };

    let mut keys: Vec<_> = sensors.iter().collect();
    keys.sort_by_key(|(k, _)| (*k).clone());
    for (k, v) in &keys {
        println!("  {} = {}", k, v);
    }

    let mut renderer = BlitzRenderer::new(&template, 480, 480)?;
    let pixmap = renderer.render(&sensors)?;

    let path = format!("/tmp/thermalwriter_blitz_{}.png", display_name);
    pixmap.save_png(&path)?;
    println!("Saved: {}", path);
    Ok(())
}
