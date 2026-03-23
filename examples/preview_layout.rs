//! Preview a layout as PNG without touching the USB device.
//! Usage: cargo run --example preview_layout [layout_name]

use anyhow::Result;
use thermalwriter::render::{FrameSource, TemplateRenderer};
use thermalwriter::sensor::SensorHub;
use thermalwriter::sensor::hwmon::HwmonProvider;
use thermalwriter::sensor::sysinfo_provider::SysinfoProvider;
use thermalwriter::sensor::amdgpu::AmdGpuProvider;
use thermalwriter::sensor::nvidia::NvidiaProvider;

fn main() -> Result<()> {
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

    let mut renderer = TemplateRenderer::new(template, 480, 480)?;
    let pixmap = renderer.render(&sensors)?;

    let path = format!("/tmp/thermalwriter_{}.png", layout_name);
    pixmap.save_png(&path)?;
    println!("Saved: {}", path);
    Ok(())
}
