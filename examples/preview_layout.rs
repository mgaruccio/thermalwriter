//! Preview a layout as PNG without USB hardware.
//!
//! ```sh
//! cargo run --example preview_layout -- layouts/svg/neon-dash-v2.svg
//! cargo run --example preview_layout -- --matrix --output-dir target/multi-cooler-visual-qa
//! cargo run --example preview_layout -- --list
//! cargo run --example preview_layout -- --profile bulk-87ad-70db-pm4-sub5-fbl72 layouts/svg/neon-dash-v2.svg
//! cargo run --example preview_layout -- --size 1280x480 layouts/svg/neon-dash-v2.svg
//! ```

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use thermalwriter::render::frontmatter::LayoutFrontmatter;
use thermalwriter::render::svg::SvgRenderer;
use thermalwriter::render::{FrameSource, TemplateRenderer};
use thermalwriter::sensor::history::SensorHistory;
use thermalwriter::sensor::mock::{fill_synthetic_history, mock_sensors};
use thermalwriter::theme::ThemePalette;
use thermalwriter::transport::{
    device_info_from_fixture, known_fixture_profiles, supported_resolutions,
};

const SEEDED: &[&str] = &[
    "layouts/system-stats.html",
    "layouts/gpu-focus.html",
    "layouts/minimal.html",
    "layouts/svg/neon-dash.svg",
    "layouts/svg/arc-gauge.svg",
    "layouts/svg/cyber-grid.svg",
    "layouts/svg/neon-dash-v2.svg",
    "layouts/svg/now-playing.svg",
];

const CLASS_SIZES: &[(u32, u32)] = &[
    (240, 320),  // portrait
    (320, 320),  // square
    (854, 480),  // landscape
    (1280, 480), // wide
    (1920, 462), // ultrawide
];

fn load_template(name_or_path: &str) -> Result<(String, String, bool)> {
    let path = Path::new(name_or_path);
    if path.exists() && path.is_file() {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let display_name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("custom")
            .to_string();
        let is_svg = path.extension().is_some_and(|e| e == "svg");
        return Ok((content, display_name, is_svg));
    }
    let svg_path = format!("layouts/svg/{name_or_path}.svg");
    if Path::new(&svg_path).exists() {
        let content = std::fs::read_to_string(&svg_path)?;
        return Ok((content, name_or_path.to_string(), true));
    }
    let html_path = format!("layouts/{name_or_path}.html");
    if Path::new(&html_path).exists() {
        let content = std::fs::read_to_string(&html_path)?;
        return Ok((content, name_or_path.to_string(), false));
    }
    bail!("Layout not found: {name_or_path}");
}

fn render_one(
    template: &str,
    is_svg: bool,
    width: u32,
    height: u32,
) -> Result<thermalwriter::render::RawFrame> {
    let sensors = mock_sensors();
    let fm = LayoutFrontmatter::parse(template);
    let history = Arc::new(Mutex::new(SensorHistory::new()));
    {
        let mut h = history.lock().unwrap();
        let metrics: Vec<String> = fm.history_configs.keys().cloned().collect();
        for (metric, cfg) in &fm.history_configs {
            h.configure_metric(metric, cfg.duration);
        }
        fill_synthetic_history(&mut h, &metrics, &sensors);
    }
    let theme = ThemePalette::default();
    let mut source: Box<dyn FrameSource> = if is_svg {
        let mut r = SvgRenderer::new(template, width, height)?;
        r.set_history(history);
        r.set_theme(theme);
        Box::new(r)
    } else {
        Box::new(TemplateRenderer::new(template, width, height)?)
    };
    source.render(&sensors)
}

fn parse_size(s: &str) -> Result<(u32, u32)> {
    let (w, h) = s
        .split_once('x')
        .ok_or_else(|| anyhow::anyhow!("size must be WIDTHxHEIGHT"))?;
    Ok((w.parse()?, h.parse()?))
}

#[derive(Debug, PartialEq, Eq)]
struct PreviewOptions {
    output_dir: PathBuf,
    matrix: bool,
    size: Option<(u32, u32)>,
    profile: Option<String>,
    layout: String,
    list: bool,
}

fn required_value<'a>(args: &'a [String], index: &mut usize, option: &str) -> Result<&'a str> {
    *index += 1;
    let value = args
        .get(*index)
        .with_context(|| format!("{option} needs value"))?;
    if value.starts_with('-') {
        bail!("{option} needs value, found option {value}");
    }
    Ok(value)
}

fn parse_args(args: &[String]) -> Result<PreviewOptions> {
    let mut options = PreviewOptions {
        output_dir: PathBuf::from("target/preview"),
        matrix: false,
        size: None,
        profile: None,
        layout: String::from("layouts/svg/neon-dash-v2.svg"),
        list: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--matrix" => options.matrix = true,
            "--output-dir" => {
                options.output_dir = PathBuf::from(required_value(args, &mut i, "--output-dir")?);
            }
            "--size" => {
                options.size = Some(parse_size(required_value(args, &mut i, "--size")?)?);
            }
            "--profile" => {
                options.profile = Some(required_value(args, &mut i, "--profile")?.to_owned());
            }
            "--list" => options.list = true,
            other if !other.starts_with('-') => options.layout = other.to_owned(),
            other => bail!("unknown arg {other}"),
        }
        i += 1;
    }
    Ok(options)
}

fn write_contact_sheet(paths: &[(PathBuf, u32, u32)], out: &Path) -> Result<()> {
    // Simple horizontal strips per row of unique heights is complex; tile in a grid.
    if paths.is_empty() {
        return Ok(());
    }
    let cols = 5usize;
    let thumb_w = 240u32;
    let rows = paths.len().div_ceil(cols);
    let mut max_h = 120u32;
    for (_, w, h) in paths {
        let scale = thumb_w as f32 / *w as f32;
        max_h = max_h.max((*h as f32 * scale) as u32);
    }
    let sheet_w = thumb_w * cols as u32;
    let sheet_h = max_h * rows as u32;
    let mut sheet = image::RgbImage::new(sheet_w, sheet_h);
    for (i, (path, w, h)) in paths.iter().enumerate() {
        let img = image::open(path)?.to_rgb8();
        let scale = thumb_w as f32 / *w as f32;
        let th = (*h as f32 * scale).max(1.0) as u32;
        let resized =
            image::imageops::resize(&img, thumb_w, th, image::imageops::FilterType::Triangle);
        let col = (i % cols) as u32;
        let row = (i / cols) as u32;
        let x0 = col * thumb_w;
        let y0 = row * max_h;
        image::imageops::replace(&mut sheet, &resized, x0 as i64, y0 as i64);
    }
    sheet.save(out)?;
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let PreviewOptions {
        output_dir,
        matrix,
        size,
        profile,
        layout,
        list,
    } = parse_args(&args)?;
    if list {
        println!("Fixture profiles:");
        for f in known_fixture_profiles() {
            let info = device_info_from_fixture(f.id)?;
            println!(
                "  {} -> {}x{} {}",
                f.id,
                info.width(),
                info.height(),
                info.encoding()
            );
        }
        println!("Supported resolutions: {:?}", supported_resolutions());
        return Ok(());
    }

    std::fs::create_dir_all(&output_dir)?;

    if matrix {
        let mut written = Vec::new();
        // 7 seeded × 5 class sizes = 35
        for layout in SEEDED {
            let (template, name, is_svg) = load_template(layout)?;
            for &(w, h) in CLASS_SIZES {
                let frame = render_one(&template, is_svg, w, h)?;
                let path = output_dir.join(format!("{name}-{w}x{h}.png"));
                frame.save_png(path.to_str().unwrap())?;
                written.push((path, w, h));
                println!("wrote {} ({w}x{h})", written.last().unwrap().0.display());
            }
        }
        // default neon-dash-v2 + calibration at all supported sizes
        let all = supported_resolutions();
        for label in [
            "layouts/svg/neon-dash-v2.svg",
            "examples/fixtures/calibration.svg",
        ] {
            let (template, name, is_svg) = load_template(label)?;
            for &(w, h) in &all {
                let frame = render_one(&template, is_svg, w, h)?;
                let path = output_dir.join(format!("{name}-full-{w}x{h}.png"));
                frame.save_png(path.to_str().unwrap())?;
                written.push((path, w, h));
            }
        }
        let sheet = output_dir.join("contact-sheet.png");
        write_contact_sheet(&written, &sheet)?;
        println!("contact sheet: {}", sheet.display());
        println!("matrix complete: {} images", written.len());
        return Ok(());
    }

    let (w, h) = if let Some(s) = size {
        s
    } else if let Some(id) = profile {
        let info = device_info_from_fixture(&id)?;
        (info.width(), info.height())
    } else {
        (480, 480)
    };

    let (template, display_name, is_svg) = load_template(&layout)?;
    let frame = render_one(&template, is_svg, w, h)?;
    let out = output_dir.join(format!("{display_name}-{w}x{h}.png"));
    frame.save_png(out.to_str().unwrap())?;
    println!("Preview saved: {} ({}x{})", out.display(), w, h);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn value_options_reject_missing_and_option_like_values() {
        for values in [
            &["--output-dir"][..],
            &["--size"][..],
            &["--profile"][..],
            &["--output-dir", "--matrix"][..],
            &["--size", "--list"][..],
            &["--profile", "--matrix"][..],
        ] {
            let error = parse_args(&args(values)).unwrap_err();
            assert!(
                error.to_string().contains("needs value"),
                "{values:?}: {error:#}"
            );
        }
    }

    #[test]
    fn valid_value_options_preserve_following_flags_and_layout() {
        let parsed = parse_args(&args(&[
            "--output-dir",
            "target/custom-preview",
            "--size",
            "1280x480",
            "--profile",
            "bulk-87ad-70db-pm4-sub5-fbl72",
            "--matrix",
            "layouts/svg/neon-dash-v2.svg",
        ]))
        .unwrap();

        assert_eq!(
            parsed,
            PreviewOptions {
                output_dir: PathBuf::from("target/custom-preview"),
                matrix: true,
                size: Some((1280, 480)),
                profile: Some("bulk-87ad-70db-pm4-sub5-fbl72".to_owned()),
                layout: "layouts/svg/neon-dash-v2.svg".to_owned(),
                list: false,
            }
        );
    }
}
