//! CPU-usage benchmark for the render pipeline — no counting allocator,
//! so timing is uninstrumented. Companion to `memory_bench.rs`.
//!
//! Reports fps (frames per second) for the default neon-dash-v2 layout with
//! mock sensors, background, and JPEG encoding — the same workload as
//! `memory_bench` but without allocator instrumentation skewing the numbers.
//!
//! Usage:
//!   cargo run --release --example cpu_bench [frames] [warmup]

use std::sync::{Arc, Mutex};
use thermalwriter::config::builtin_layouts;
use thermalwriter::render::background::decode_to_pixmap;
use thermalwriter::render::frontmatter::LayoutFrontmatter;
use thermalwriter::render::svg::SvgRenderer;
use thermalwriter::render::FrameSource;
use thermalwriter::sensor::history::SensorHistory;
use thermalwriter::sensor::mock::{fill_synthetic_history, mock_sensors, mock_sensors_varying};
use thermalwriter::service::tick::encode_jpeg;
use thermalwriter::theme::ThemePalette;

fn build_renderer(
    template: &str,
    sensors: &thermalwriter::render::SensorData,
) -> SvgRenderer<'static> {
    let frontmatter = LayoutFrontmatter::parse(template);
    let mut renderer = SvgRenderer::new(template, 480, 480).expect("valid built-in layout");
    renderer.set_theme(ThemePalette::default());

    if !frontmatter.history_configs.is_empty() {
        let metrics: Vec<String> = frontmatter.history_configs.keys().cloned().collect();
        let mut history = SensorHistory::new();
        for (metric, cfg) in &frontmatter.history_configs {
            history.configure_metric(metric, cfg.duration);
        }
        fill_synthetic_history(&mut history, &metrics, sensors);
        renderer.set_history(Arc::new(Mutex::new(history)));
    }

    let bg = decode_to_pixmap(builtin_layouts::BG_DARK_GRADIENT)
        .expect("built-in background must decode");
    renderer.set_background(Some(bg));

    renderer
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let measure_frames: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(200);
    let warmup_frames: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(50);

    let sensors = mock_sensors();
    let mut renderer = build_renderer(builtin_layouts::SVG_NEON_DASH_V2, &sensors);

    // Warmup
    for i in 0..warmup_frames {
        let s = mock_sensors_varying(i);
        let frame = renderer.render(&s).expect("render must succeed");
        let _jpeg = encode_jpeg(&frame, 85, 180).expect("jpeg encode must succeed");
    }

    // Measurement
    let start = std::time::Instant::now();
    for i in 0..measure_frames {
        let s = mock_sensors_varying(warmup_frames + i);
        let frame = renderer.render(&s).expect("render must succeed");
        let _jpeg = encode_jpeg(&frame, 85, 180).expect("jpeg encode must succeed");
    }
    let elapsed = start.elapsed();

    let fps = measure_frames as f64 / elapsed.as_secs_f64();
    let ms_per_frame = elapsed.as_secs_f64() * 1000.0 / measure_frames as f64;

    // Primary: uninstrumented throughput
    println!("METRIC fps={:.2}", fps);
    // Secondary: ms per frame (matches profile.sh's cpu_per_frame_ms concept)
    println!("METRIC ms_per_frame={:.3}", ms_per_frame);

    eprintln!("--- cpu_bench summary ---");
    eprintln!("warmup:    {} frames", warmup_frames);
    eprintln!("measure:   {} frames", measure_frames);
    eprintln!("elapsed:   {:.3}s", elapsed.as_secs_f64());
    eprintln!("fps:       {:.2}", fps);
    eprintln!("ms/frame:  {:.3}", ms_per_frame);
}
