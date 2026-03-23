//! Preview a layout as PNG without touching the USB device.
//! Usage:
//!   cargo run --example preview_layout [name_or_path]
//!
//! Examples:
//!   cargo run --example preview_layout system-stats         # built-in
//!   cargo run --example preview_layout layouts/neon-dash.html  # file path
//!   cargo run --example preview_layout neon-dash            # layouts/<name>.html

use anyhow::{Context, Result};
use std::path::Path;
use thermalwriter::render::{FrameSource, TemplateRenderer};
use thermalwriter::sensor::SensorHub;
use thermalwriter::sensor::hwmon::HwmonProvider;
use thermalwriter::sensor::sysinfo_provider::SysinfoProvider;
use thermalwriter::sensor::amdgpu::AmdGpuProvider;
use thermalwriter::sensor::nvidia::NvidiaProvider;

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

    // Fall back to built-in layouts
    match name_or_path {
        "system-stats" => Ok((include_str!("../layouts/system-stats.html").to_string(), "system-stats".to_string())),
        "gpu-focus" => Ok((include_str!("../layouts/gpu-focus.html").to_string(), "gpu-focus".to_string())),
        "minimal" => Ok((include_str!("../layouts/minimal.html").to_string(), "minimal".to_string())),
        other => anyhow::bail!(
            "Layout not found: '{}'\n\nUsage: cargo run --example preview_layout [name_or_path]\n  name_or_path: file path, layouts/<name>.html, or built-in (system-stats, gpu-focus, minimal)",
            other
        ),
    }
}

fn main() -> Result<()> {
    let name_or_path = std::env::args().nth(1).unwrap_or("system-stats".to_string());
    let (template, display_name) = load_template(&name_or_path)?;

    let mut hub = SensorHub::new();
    hub.add_provider(Box::new(HwmonProvider::new()));
    hub.add_provider(Box::new(SysinfoProvider::new()));
    hub.add_provider(Box::new(AmdGpuProvider::new()));
    hub.add_provider(Box::new(NvidiaProvider::new()));

    let sensors = hub.poll();
    let mut keys: Vec<_> = sensors.iter().collect();
    keys.sort_by_key(|(k, _)| (*k).clone());
    for (k, v) in &keys {
        println!("  {} = {}", k, v);
    }

    let mut renderer = TemplateRenderer::new(&template, 480, 480)?;
    let pixmap = renderer.render(&sensors)?;

    let path = format!("/tmp/thermalwriter_{}.png", display_name);
    pixmap.save_png(&path)?;
    println!("Saved: {}", path);
    Ok(())
}
