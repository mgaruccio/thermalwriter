//! Render multi-frame PNG sequences for gallery GIFs (no USB required).
//!
//! ```sh
//! cargo run --release --example render_gallery_frames -- \
//!   --background ~/.config/thermalwriter/backgrounds/anime.png \
//!   --output-dir target/gallery-frames \
//!   --frames 24
//! ```

use anyhow::{Context, Result, bail};
use clap::Parser;
use std::collections::HashMap;
use std::f64::consts::TAU;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thermalwriter::render::background::BackgroundImage;
use thermalwriter::render::frontmatter::LayoutFrontmatter;
use thermalwriter::render::svg::SvgRenderer;
use thermalwriter::render::{FrameSource, SensorData};
use thermalwriter::sensor::history::SensorHistory;
use thermalwriter::theme::ThemePalette;

#[derive(Debug, Parser)]
#[command(name = "render-gallery-frames")]
struct Options {
    /// Background image composited under every frame (daemon path).
    #[arg(long)]
    background: PathBuf,
    /// Directory for layout/frame_XXXX.png outputs.
    #[arg(long, default_value = "target/gallery-frames")]
    output_dir: PathBuf,
    /// Frames per layout.
    #[arg(long, default_value_t = 24)]
    frames: u32,
    /// Canvas size.
    #[arg(long, default_value_t = 480)]
    size: u32,
    /// Only these layout stems (default: stock SVG dashboards).
    #[arg(long)]
    layout: Vec<String>,
    /// Tokyo Night-ish palette overrides (hex), applied as theme + common layout vars.
    #[arg(long, default_value = "#7aa2f7")]
    primary: String,
    #[arg(long, default_value = "#bb9af7")]
    secondary: String,
    #[arg(long, default_value = "#9ece6a")]
    accent: String,
    #[arg(long, default_value = "#1a1b26")]
    theme_background: String,
    /// neon-dash-v2 panel accents (user defaults).
    #[arg(long, default_value = "#bcb2ff")]
    nd_primary: String,
    #[arg(long, default_value = "#fc8da0")]
    nd_secondary: String,
    #[arg(long, default_value = "#00cad1")]
    nd_accent: String,
}

const DEFAULT_LAYOUTS: &[&str] = &[
    "layouts/svg/neon-dash-v2.svg",
    "layouts/svg/neon-dash.svg",
    "layouts/svg/cyber-grid.svg",
    "layouts/svg/arc-gauge.svg",
];

fn main() -> Result<()> {
    let opt = Options::parse();
    if opt.frames == 0 {
        bail!("--frames must be >= 1");
    }
    let bg = Arc::new(
        BackgroundImage::from_file(&opt.background)
            .with_context(|| format!("load background {}", opt.background.display()))?,
    );

    let layouts: Vec<PathBuf> = if opt.layout.is_empty() {
        DEFAULT_LAYOUTS.iter().map(PathBuf::from).collect()
    } else {
        opt.layout
            .iter()
            .map(|name| {
                let p = PathBuf::from(name);
                if p.exists() {
                    return p;
                }
                let svg = PathBuf::from(format!("layouts/svg/{name}.svg"));
                if svg.exists() {
                    return svg;
                }
                PathBuf::from(format!("layouts/{name}"))
            })
            .collect()
    };

    let theme = ThemePalette {
        primary: opt.primary.clone(),
        secondary: opt.secondary.clone(),
        accent: opt.accent.clone(),
        background: opt.theme_background.clone(),
        surface: "#24283b".into(),
        text: "#c0caf5".into(),
        text_dim: "#9aa5ce".into(),
        success: "#9ece6a".into(),
        warning: "#e0af68".into(),
        critical: "#f7768e".into(),
    };

    for layout_path in &layouts {
        let stem = layout_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("layout");
        let out_dir = opt.output_dir.join(stem);
        std::fs::create_dir_all(&out_dir)?;
        println!("==> {stem} ({} frames) → {}", opt.frames, out_dir.display());

        let template = std::fs::read_to_string(layout_path)
            .with_context(|| format!("read {}", layout_path.display()))?;
        let fm = LayoutFrontmatter::parse(&template);
        let history = Arc::new(Mutex::new(SensorHistory::new()));
        {
            let mut h = history.lock().unwrap();
            for (metric, cfg) in &fm.history_configs {
                h.configure_metric(metric, cfg.duration);
            }
        }

        let mut renderer = SvgRenderer::new(&template, opt.size, opt.size)?;
        renderer.set_history(Arc::clone(&history));
        renderer.set_theme(theme.clone());
        renderer.set_background(Some(Arc::clone(&bg)))?;

        // neon-dash-v2 user accents
        let mut vars = HashMap::new();
        if stem.contains("neon-dash") {
            vars.insert("theme_primary".into(), opt.nd_primary.clone());
            vars.insert("theme_secondary".into(), opt.nd_secondary.clone());
            vars.insert("theme_accent".into(), opt.nd_accent.clone());
            vars.insert("theme_background".into(), "#33313f".into());
            vars.insert("panel_opacity".into(), "0.5".into());
        }
        if !vars.is_empty() {
            renderer.set_layout_vars(vars);
        }

        for i in 0..opt.frames {
            let t = i as f64 / opt.frames.max(1) as f64;
            let sensors = synthetic_sensors(t);
            {
                let mut h = history.lock().unwrap();
                // advance history so graphs move
                let ts = Duration::from_millis(i as u64 * 250);
                record_history(&mut h, &sensors, ts);
            }
            let frame = renderer.render(&sensors)?;
            let path = out_dir.join(format!("frame_{i:04}.png"));
            save_rgb_png(&frame.data, frame.width, frame.height, &path)?;
        }
        println!("    wrote {} PNGs", opt.frames);
    }
    Ok(())
}

fn synthetic_sensors(t: f64) -> SensorData {
    // Smooth loops so GIFs cycle cleanly. Keys match stock SVG layout tokens.
    let w = TAU * t;
    let mut s = SensorData::new();
    let cpu_util = 28.0 + 45.0 * (0.5 + 0.5 * w.sin());
    let gpu_util = 35.0 + 40.0 * (0.5 + 0.5 * (w * 1.3 + 0.4).sin());
    let ram_used = 18.0 + 12.0 * (0.5 + 0.5 * (w * 0.7 + 1.1).cos());
    let vram_used = 4.0 + 6.0 * (0.5 + 0.5 * (w * 0.9 + 2.0).sin());
    let cpu_temp = 45.0 + 25.0 * (0.5 + 0.5 * (w * 0.8).sin());
    let gpu_temp = 50.0 + 28.0 * (0.5 + 0.5 * (w * 1.1 + 0.6).cos());
    let fps = 90.0 + 50.0 * (0.5 + 0.5 * (w * 2.0).sin());

    for (k, v) in [
        ("cpu_util", cpu_util),
        ("cpu_usage", cpu_util),
        ("cpu_load", cpu_util),
        ("cpu_temp", cpu_temp),
        ("cpu_power", 35.0 + cpu_util * 0.4),
        ("gpu_util", gpu_util),
        ("gpu_usage", gpu_util),
        ("gpu_load", gpu_util),
        ("gpu_temp", gpu_temp),
        ("gpu_power", 40.0 + gpu_util * 0.5),
        ("ram_used", ram_used),
        ("ram_total", 64.0),
        ("vram_used", vram_used),
        ("fps", fps),
    ] {
        s.insert(k.to_string(), format!("{v:.1}"));
    }
    s
}

fn record_history(h: &mut SensorHistory, sensors: &SensorData, _ts: Duration) {
    // SensorHistory::record stamps with Instant::now().
    // Sleep briefly so samples aren't all same Instant (graphs need time deltas).
    std::thread::sleep(Duration::from_millis(15));
    h.record(sensors);
}

fn save_rgb_png(rgb: &[u8], width: u32, height: u32, path: &Path) -> Result<()> {
    let img = image::RgbImage::from_raw(width, height, rgb.to_vec())
        .ok_or_else(|| anyhow::anyhow!("bad RGB dimensions for {}", path.display()))?;
    img.save(path)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}
