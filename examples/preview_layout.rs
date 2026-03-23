//! Preview a layout as PNG without touching the USB device.
//! Usage:
//!   cargo run --example preview_layout [name_or_path]
//!
//! Examples:
//!   cargo run --example preview_layout system-stats              # built-in HTML
//!   cargo run --example preview_layout layouts/neon-dash.html    # HTML file path
//!   cargo run --example preview_layout layouts/svg/arc-gauge.svg # SVG file path
//!   cargo run --example preview_layout neon-dash                 # layouts/<name>.html

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use thermalwriter::render::{FrameSource, TemplateRenderer};
use thermalwriter::render::frontmatter::LayoutFrontmatter;
use thermalwriter::render::svg::SvgRenderer;
use thermalwriter::sensor::SensorHub;
use thermalwriter::sensor::history::SensorHistory;
use thermalwriter::sensor::hwmon::HwmonProvider;
use thermalwriter::sensor::sysinfo_provider::SysinfoProvider;
use thermalwriter::sensor::amdgpu::AmdGpuProvider;
use thermalwriter::sensor::nvidia::NvidiaProvider;
use thermalwriter::sensor::rapl::RaplProvider;
use thermalwriter::theme::ThemePalette;

/// Returns (content, display_name, is_svg).
fn load_template(name_or_path: &str) -> Result<(String, String, bool)> {
    // Try as file path first
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
            "Layout not found: '{}'\n\nUsage: cargo run --example preview_layout [name_or_path]\n  name_or_path: file path (.html or .svg), layouts/<name>.html, layouts/svg/<name>.svg, or built-in (system-stats, gpu-focus, minimal)",
            other
        ),
    }
}

/// Generate synthetic history data for preview (60 points, sinusoidal wave around a base value).
/// Uses a deterministic pattern so previews are reproducible.
fn fill_synthetic_history(history: &mut SensorHistory, metrics: &[String], sensor_data: &HashMap<String, String>) {
    let sample_count = 60usize;
    for metric in metrics {
        // Use current sensor value as base if available, otherwise pick a reasonable default
        let base: f64 = sensor_data.get(metric)
            .and_then(|v| v.parse().ok())
            .unwrap_or(50.0);

        for i in 0..sample_count {
            // Sinusoidal variation ±20% of base
            let phase = (i as f64 / sample_count as f64) * std::f64::consts::TAU;
            let amplitude = base * 0.2;
            let value = (base + amplitude * phase.sin()).max(0.0);

            let mut data = HashMap::new();
            data.insert(metric.clone(), format!("{:.1}", value));
            history.record(&data);
        }
    }
}

fn main() -> Result<()> {
    let name_or_path = std::env::args().nth(1).unwrap_or("system-stats".to_string());
    let (template, display_name, is_svg) = load_template(&name_or_path)?;

    let mut hub = SensorHub::new();
    hub.add_provider(Box::new(HwmonProvider::new()));
    hub.add_provider(Box::new(SysinfoProvider::new()));
    hub.add_provider(Box::new(AmdGpuProvider::new()));
    hub.add_provider(Box::new(NvidiaProvider::new()));
    hub.add_provider(Box::new(RaplProvider::new()));

    // RAPL needs two polls to compute power delta
    hub.poll();
    std::thread::sleep(std::time::Duration::from_millis(250));
    let sensors = hub.poll();
    let mut keys: Vec<_> = sensors.iter().collect();
    keys.sort_by_key(|(k, _)| (*k).clone());
    for (k, v) in &keys {
        println!("  {} = {}", k, v);
    }

    let mut renderer: Box<dyn FrameSource> = if is_svg {
        println!("Using SVG renderer");

        // Parse frontmatter for history/animation config
        let frontmatter = LayoutFrontmatter::parse(&template);

        // Create sensor history and pre-fill with synthetic data
        let sensor_history = if !frontmatter.history_configs.is_empty() {
            let metrics: Vec<String> = frontmatter.history_configs.keys().cloned().collect();
            println!("Frontmatter history metrics: {:?}", metrics);

            let mut history = SensorHistory::new();
            for (metric, cfg) in &frontmatter.history_configs {
                history.configure_metric(metric, cfg.duration);
            }
            fill_synthetic_history(&mut history, &metrics, &sensors);
            println!("Pre-filled {} metrics with {} synthetic samples each", metrics.len(), 60);
            Some(Arc::new(Mutex::new(history)))
        } else {
            None
        };

        let theme = ThemePalette::default();
        let mut renderer = SvgRenderer::new(&template, 480, 480)?;
        if let Some(hist) = sensor_history {
            renderer.set_history(hist);
        }
        renderer.set_theme(theme);
        Box::new(renderer)
    } else {
        Box::new(TemplateRenderer::new(&template, 480, 480)?)
    };

    let pixmap = renderer.render(&sensors)?;

    let path = format!("/tmp/thermalwriter_{}.png", display_name);
    pixmap.save_png(&path)?;
    println!("Saved: {}", path);
    Ok(())
}
