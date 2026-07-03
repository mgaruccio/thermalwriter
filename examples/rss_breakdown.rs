//! Quick diagnostic: measure RSS at each phase to understand where the 13MB comes from.
use std::sync::{Arc, Mutex};
use thermalwriter::config::builtin_layouts;
use thermalwriter::render::background::decode_to_pixmap;
use thermalwriter::render::frontmatter::LayoutFrontmatter;
use thermalwriter::render::svg::SvgRenderer;
use thermalwriter::render::FrameSource;
use thermalwriter::sensor::history::SensorHistory;
use thermalwriter::sensor::mock::{fill_synthetic_history, mock_sensors};
use thermalwriter::theme::ThemePalette;

fn rss_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.trim().split_whitespace().next()
                .and_then(|s| s.parse().ok()).unwrap_or(0);
        }
    }
    0
}

fn main() {
    eprintln!("phase 0 (process start): {} KB", rss_kb());
    
    let sensors = mock_sensors();
    eprintln!("phase 1 (after mock_sensors): {} KB", rss_kb());
    
    let template = builtin_layouts::SVG_NEON_DASH_V2;
    eprintln!("phase 2 (template ref): {} KB", rss_kb());
    
    // This triggers shared_fontdb() which loads system fonts
    let mut renderer = SvgRenderer::new(template, 480, 480).unwrap();
    eprintln!("phase 3 (after SvgRenderer::new — includes fontdb): {} KB", rss_kb());
    
    renderer.set_theme(ThemePalette::default());
    
    let frontmatter = LayoutFrontmatter::parse(template);
    if !frontmatter.history_configs.is_empty() {
        let metrics: Vec<String> = frontmatter.history_configs.keys().cloned().collect();
        let mut history = SensorHistory::new();
        for (metric, cfg) in &frontmatter.history_configs {
            history.configure_metric(metric, cfg.duration);
        }
        fill_synthetic_history(&mut history, &metrics, &sensors);
        renderer.set_history(Arc::new(Mutex::new(history)));
    }
    eprintln!("phase 4 (after history setup): {} KB", rss_kb());
    
    let bg = decode_to_pixmap(builtin_layouts::BG_DARK_GRADIENT).unwrap();
    eprintln!("phase 5 (after bg decode): {} KB", rss_kb());
    
    renderer.set_background(Some(bg));
    eprintln!("phase 6 (after set_background): {} KB", rss_kb());
    
    let frame = renderer.render(&sensors).unwrap();
    eprintln!("phase 7 (after first render): {} KB", rss_kb());
    
    drop(frame);
    eprintln!("phase 8 (after drop frame): {} KB", rss_kb());
    
    // Render 10 more frames
    for i in 0..10 {
        let s = thermalwriter::sensor::mock::mock_sensors_varying(i);
        let frame = renderer.render(&s).unwrap();
        drop(frame);
    }
    eprintln!("phase 9 (after 10 more renders): {} KB", rss_kb());
}
