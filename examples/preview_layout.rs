//! Preview a layout as PNG without USB hardware.
//!
//! ```sh
//! cargo run --example preview_layout -- layouts/svg/neon-dash-v2.svg
//! cargo run --example preview_layout -- --matrix --output-dir target/multi-cooler-visual-qa
//! cargo run --example preview_layout -- --matrix layouts/neon-composer.layout.toml
//! cargo run --example preview_layout -- --format json --profile thermalright-curved-2400x1080 layouts/neon-composer.layout.toml
//! cargo run --example preview_layout -- --list
//! cargo run --example preview_layout -- --profile bulk-87ad-70db-pm4-sub5-fbl72 layouts/svg/neon-dash-v2.svg
//! cargo run --example preview_layout -- --size 1280x480 layouts/svg/neon-dash-v2.svg
//! ```
#![allow(clippy::print_stdout)]
#![allow(clippy::print_stderr)]

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Arc, Mutex};
use thermalwriter::layout_engine::svg_backend::ResvgSceneBackend;
use thermalwriter::layout_engine::{
    CURRENT_VERSION, DiagnosticSeverity, DisplaySurfaceProfile, LayoutDiagnostic, LayoutDocument,
    LayoutDocumentError, LayoutEngineRenderer, SurfaceProfileId, rectangular_surface_profile,
    resolve_surface_profile, validate,
};
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
];

const CLASS_SIZES: &[(u32, u32)] = &[
    (240, 320),  // portrait
    (320, 320),  // square
    (854, 480),  // landscape
    (1280, 480), // wide
    (1920, 462), // ultrawide
];

const DOCUMENT_MATRIX: &[(&str, u32, u32, SurfaceProfileId)] = &[
    ("square", 480, 480, SurfaceProfileId::Rectangular),
    ("portrait", 480, 1280, SurfaceProfileId::Rectangular),
    ("wide", 1280, 480, SurfaceProfileId::Rectangular),
    (
        "thermalright-curved-2400x1080",
        2400,
        1080,
        SurfaceProfileId::ThermalrightCurved2400x1080,
    ),
];

const PREVIEW_PROFILE_CODE: &str = "TWLAYOUT-E028";
const UNSUPPORTED_VERSION_CODE: &str = "TWLAYOUT-E002";
const PREVIEW_HISTORY: &str = "[55.0, 57.5, 60.0, 62.5, 65.0, 67.0, 69.5, 72.0]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, PartialEq, Eq)]
struct PreviewOptions {
    output_dir: PathBuf,
    matrix: bool,
    size: Option<(u32, u32)>,
    profile: Option<String>,
    layout: String,
    list: bool,
    format: OutputFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DocumentTarget {
    profile: String,
    surface: DisplaySurfaceProfile,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct PreviewRecord {
    profile: String,
    dimensions: String,
    path: String,
    diagnostics: Vec<LayoutDiagnostic>,
}

#[derive(Debug)]
enum CliError {
    Message(anyhow::Error),
    Diagnostics(Vec<LayoutDiagnostic>),
}

impl From<anyhow::Error> for CliError {
    fn from(error: anyhow::Error) -> Self {
        Self::Message(error)
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(error) => write!(formatter, "{error:#}"),
            Self::Diagnostics(diagnostics) => {
                let text = diagnostics
                    .iter()
                    .map(LayoutDiagnostic::to_human)
                    .collect::<Vec<_>>()
                    .join("\n\n");
                formatter.write_str(&text)
            }
        }
    }
}

impl std::error::Error for CliError {}

type CliResult<T> = std::result::Result<T, CliError>;

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
        .ok_or_else(|| anyhow!("size must be WIDTHxHEIGHT"))?;
    let width: u32 = w.parse().context("width must be a positive integer")?;
    let height: u32 = h.parse().context("height must be a positive integer")?;
    if width == 0 || height == 0 {
        bail!("size must use positive dimensions");
    }
    Ok((width, height))
}

fn parse_output_format(value: &str) -> Result<OutputFormat> {
    match value {
        "human" => Ok(OutputFormat::Human),
        "json" => Ok(OutputFormat::Json),
        other => bail!("format must be human or json, found {other}"),
    }
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
        format: OutputFormat::Human,
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
            "--format" => {
                options.format = parse_output_format(required_value(args, &mut i, "--format")?)?;
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

fn is_layout_document(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension == "toml")
}

fn layout_display_name(path: &Path) -> String {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("layout");
    file_name
        .strip_suffix(".layout.toml")
        .or_else(|| file_name.strip_suffix(".toml"))
        .or_else(|| path.file_stem().and_then(|name| name.to_str()))
        .unwrap_or("layout")
        .to_owned()
}

fn parse_layout_document(input: &str, path: &Path) -> CliResult<LayoutDocument> {
    match LayoutDocument::from_toml(input) {
        Ok(document) => Ok(document),
        Err(LayoutDocumentError::Parse(error)) => Err(CliError::Diagnostics(vec![
            LayoutDiagnostic::from_toml_error(&error, input, Some(path.to_path_buf())),
        ])),
        Err(LayoutDocumentError::UnsupportedVersion(version)) => {
            let mut diagnostic = LayoutDiagnostic::new(
                UNSUPPORTED_VERSION_CODE,
                DiagnosticSeverity::Error,
                "Unsupported layout document version",
                format!("layout document version {version} is not supported"),
                format!("Set `version = {CURRENT_VERSION}` before previewing this document."),
            );
            diagnostic.file = Some(path.to_path_buf());
            Err(CliError::Diagnostics(vec![diagnostic]))
        }
        Err(error) => Err(CliError::Message(anyhow!(error))),
    }
}

fn load_layout_document(path: &Path) -> CliResult<LayoutDocument> {
    let input = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read layout document {}", path.display()))?;
    parse_layout_document(&input, path)
}

fn unsupported_profile_diagnostic(
    profile: &str,
    dimensions: Option<(u32, u32)>,
    reason: impl Into<String>,
    fix: impl Into<String>,
) -> LayoutDiagnostic {
    let dimension_text = dimensions
        .map(|(width, height)| format!(" for {width}x{height}"))
        .unwrap_or_default();
    let mut diagnostic = LayoutDiagnostic::new(
        PREVIEW_PROFILE_CODE,
        DiagnosticSeverity::Error,
        "Unsupported preview profile",
        format!("profile `{profile}`{dimension_text}: {}", reason.into()),
        fix,
    );
    diagnostic.profile = Some(profile.to_owned());
    diagnostic
}

fn rectangular_target(
    profile: impl Into<String>,
    width: u32,
    height: u32,
) -> CliResult<DocumentTarget> {
    let profile = profile.into();
    let Some(surface) = rectangular_surface_profile(width, height).copied() else {
        return Err(CliError::Diagnostics(vec![unsupported_profile_diagnostic(
            &profile,
            Some((width, height)),
            "the dimensions are not in the bounded rectangular profile registry",
            "Choose 480x480, 480x1280, 1280x480, or an explicitly supported device profile.",
        )]));
    };
    Ok(DocumentTarget { profile, surface })
}

fn curved_target(profile: impl Into<String>) -> CliResult<DocumentTarget> {
    let profile = profile.into();
    let Some(surface) =
        resolve_surface_profile(2400, 1080, SurfaceProfileId::ThermalrightCurved2400x1080).copied()
    else {
        return Err(CliError::Diagnostics(vec![unsupported_profile_diagnostic(
            &profile,
            Some((2400, 1080)),
            "the curved profile is not registered",
            "Use a registered Thermalright curved 2400x1080 profile.",
        )]));
    };
    Ok(DocumentTarget { profile, surface })
}

fn target_for_profile(profile: &str) -> CliResult<DocumentTarget> {
    match profile {
        "square" => rectangular_target("square", 480, 480),
        "portrait" => rectangular_target("portrait", 480, 1280),
        "wide" => rectangular_target("wide", 1280, 480),
        "rectangular" => rectangular_target("rectangular", 480, 480),
        "thermalright-curved-2400x1080" => curved_target(profile),
        fixture => match device_info_from_fixture(fixture) {
            Ok(info) => rectangular_target(fixture, info.width(), info.height()),
            Err(_) => Err(CliError::Diagnostics(vec![unsupported_profile_diagnostic(
                fixture,
                None,
                "the name is not a known layout or fixture profile",
                "Use square, portrait, wide, thermalright-curved-2400x1080, or a known fixture profile.",
            )])),
        },
    }
}

fn target_for_size((width, height): (u32, u32)) -> CliResult<DocumentTarget> {
    let profile = if width == height {
        "square"
    } else if height > width {
        "portrait"
    } else {
        "wide"
    };
    rectangular_target(profile, width, height)
}

fn document_targets(options: &PreviewOptions) -> CliResult<Vec<DocumentTarget>> {
    if options.matrix {
        return DOCUMENT_MATRIX
            .iter()
            .map(|(profile, width, height, topology)| {
                let surface = resolve_surface_profile(*width, *height, *topology)
                    .copied()
                    .ok_or_else(|| {
                        CliError::Diagnostics(vec![unsupported_profile_diagnostic(
                            *profile,
                            Some((*width, *height)),
                            "the matrix target is not registered",
                            "Use the bounded preview profile matrix.",
                        )])
                    })?;
                Ok(DocumentTarget {
                    profile: (*profile).to_owned(),
                    surface,
                })
            })
            .collect();
    }
    if let Some(size) = options.size {
        return Ok(vec![target_for_size(size)?]);
    }
    let target = options
        .profile
        .as_deref()
        .map_or_else(|| target_for_profile("square"), target_for_profile)?;
    Ok(vec![target])
}

fn annotate_diagnostic(
    mut diagnostic: LayoutDiagnostic,
    path: &Path,
    target: &DocumentTarget,
) -> LayoutDiagnostic {
    if diagnostic.file.is_none() {
        diagnostic.file = Some(path.to_path_buf());
    }
    if diagnostic.profile.is_none() {
        diagnostic.profile = Some(target.profile.clone());
    }
    diagnostic
}

fn preview_sensors() -> thermalwriter::render::SensorData {
    let mut sensors = mock_sensors();
    sensors.insert("cpu.temperature".to_owned(), "67".to_owned());
    sensors.insert(
        "cpu.temperature.history".to_owned(),
        PREVIEW_HISTORY.to_owned(),
    );
    sensors
}

fn document_output_path(
    output_dir: &Path,
    name: &str,
    target: &DocumentTarget,
    matrix: bool,
) -> PathBuf {
    let suffix = if matrix {
        format!(
            "-{}-{}x{}",
            target.profile, target.surface.width, target.surface.height
        )
    } else {
        format!("-{}x{}", target.surface.width, target.surface.height)
    };
    output_dir.join(format!("{name}{suffix}.png"))
}

fn render_document_previews(options: &PreviewOptions) -> CliResult<Vec<PreviewRecord>> {
    let input_path = Path::new(&options.layout);
    let document = load_layout_document(input_path)?;
    let targets = document_targets(options)?;

    // Validate every requested target before creating any output or rendering a frame.
    let mut diagnostics = Vec::new();
    for target in &targets {
        if let Err(target_diagnostics) = validate(&document, &target.surface) {
            diagnostics.extend(
                target_diagnostics
                    .into_iter()
                    .map(|diagnostic| annotate_diagnostic(diagnostic, input_path, target)),
            );
        }
    }
    if !diagnostics.is_empty() {
        return Err(CliError::Diagnostics(diagnostics));
    }

    std::fs::create_dir_all(&options.output_dir)
        .with_context(|| format!("Failed to create {}", options.output_dir.display()))?;
    let media_root = input_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let sensors = preview_sensors();
    let name = layout_display_name(input_path);
    let mut records = Vec::with_capacity(targets.len());

    for target in targets {
        let output = document_output_path(&options.output_dir, &name, &target, options.matrix);
        let mut renderer = LayoutEngineRenderer::with_media_root(
            document.clone(),
            target.surface,
            ResvgSceneBackend,
            media_root,
        );
        let frame = renderer.render(&sensors).map_err(|error| {
            CliError::Message(anyhow!(
                "failed to render layout document profile `{}`: {error:#}",
                target.profile
            ))
        })?;
        let expected_dimensions = target.surface.dimensions();
        if (frame.width, frame.height) != expected_dimensions {
            let mut diagnostic = LayoutDiagnostic::new(
                "TWLAYOUT-E032",
                DiagnosticSeverity::Error,
                "Preview renderer returned unexpected dimensions",
                format!(
                    "renderer returned {}x{} for native profile {}x{}",
                    frame.width, frame.height, expected_dimensions.0, expected_dimensions.1
                ),
                "Use the shared layout-engine renderer without scaling the native surface.",
            );
            diagnostic.file = Some(input_path.to_path_buf());
            diagnostic.profile = Some(target.profile.clone());
            return Err(CliError::Diagnostics(vec![diagnostic]));
        }
        let output_text = output
            .to_str()
            .ok_or_else(|| anyhow!("PNG path is not valid UTF-8"))?;
        frame
            .save_png(output_text)
            .with_context(|| format!("Failed to write preview PNG {}", output.display()))?;
        let encoded_dimensions = image::image_dimensions(&output)
            .with_context(|| format!("Failed to inspect preview PNG {}", output.display()))?;
        if encoded_dimensions != expected_dimensions {
            let mut diagnostic = LayoutDiagnostic::new(
                "TWLAYOUT-E032",
                DiagnosticSeverity::Error,
                "Preview PNG has unexpected dimensions",
                format!(
                    "PNG encoded as {}x{} for native profile {}x{}",
                    encoded_dimensions.0,
                    encoded_dimensions.1,
                    expected_dimensions.0,
                    expected_dimensions.1
                ),
                "Write the preview through the shared RawFrame PNG path without resizing.",
            );
            diagnostic.file = Some(output.clone());
            diagnostic.profile = Some(target.profile.clone());
            return Err(CliError::Diagnostics(vec![diagnostic]));
        }
        records.push(PreviewRecord {
            profile: target.profile,
            dimensions: format!("{}x{}", encoded_dimensions.0, encoded_dimensions.1),
            path: output.display().to_string(),
            diagnostics: Vec::new(),
        });
    }
    Ok(records)
}

fn print_preview_records(records: &[PreviewRecord], format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Human => {
            for record in records {
                println!("profile: {}", record.profile);
                println!("dimensions: {}", record.dimensions);
                println!("path: {}", record.path);
                for diagnostic in &record.diagnostics {
                    println!("{}", diagnostic.to_human());
                }
            }
            if records.len() > 1 {
                println!("matrix complete: {} images", records.len());
            }
        }
        OutputFormat::Json => {
            if records.len() == 1 {
                println!("{}", serde_json::to_string_pretty(&records[0])?);
            } else {
                println!("{}", serde_json::to_string_pretty(records)?);
            }
        }
    }
    Ok(())
}

fn print_error(error: &CliError, format: OutputFormat) {
    match format {
        OutputFormat::Human => eprintln!("{error}"),
        OutputFormat::Json => {
            let value = match error {
                CliError::Diagnostics(diagnostics) => serde_json::json!({
                    "diagnostics": diagnostics,
                }),
                CliError::Message(message) => serde_json::json!({
                    "error": message.to_string(),
                    "diagnostics": [],
                }),
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&value).expect("JSON output")
            );
        }
    }
}

fn run_legacy(options: &PreviewOptions) -> Result<()> {
    if options.list {
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

    std::fs::create_dir_all(&options.output_dir)?;

    if options.matrix {
        let mut written = Vec::new();
        // 7 seeded × 5 class sizes = 35
        for layout in SEEDED {
            let (template, name, is_svg) = load_template(layout)?;
            for &(width, height) in CLASS_SIZES {
                let frame = render_one(&template, is_svg, width, height)?;
                let path = options
                    .output_dir
                    .join(format!("{name}-{width}x{height}.png"));
                frame.save_png(path.to_str().unwrap())?;
                written.push((path, width, height));
                println!(
                    "wrote {} ({width}x{height})",
                    written.last().unwrap().0.display()
                );
            }
        }
        // default neon-dash-v2 + calibration at all supported sizes
        let all = supported_resolutions();
        for label in [
            "layouts/svg/neon-dash-v2.svg",
            "examples/fixtures/calibration.svg",
        ] {
            let (template, name, is_svg) = load_template(label)?;
            for &(width, height) in &all {
                let frame = render_one(&template, is_svg, width, height)?;
                let path = options
                    .output_dir
                    .join(format!("{name}-full-{width}x{height}.png"));
                frame.save_png(path.to_str().unwrap())?;
                written.push((path, width, height));
            }
        }
        let sheet = options.output_dir.join("contact-sheet.png");
        write_contact_sheet(&written, &sheet)?;
        println!("contact sheet: {}", sheet.display());
        println!("matrix complete: {} images", written.len());
        return Ok(());
    }

    let (width, height) = if let Some(size) = options.size {
        size
    } else if let Some(id) = &options.profile {
        let info = device_info_from_fixture(id)?;
        (info.width(), info.height())
    } else {
        (480, 480)
    };

    let (template, display_name, is_svg) = load_template(&options.layout)?;
    let frame = render_one(&template, is_svg, width, height)?;
    let output = options
        .output_dir
        .join(format!("{display_name}-{width}x{height}.png"));
    frame.save_png(output.to_str().unwrap())?;
    println!("Preview saved: {} ({}x{})", output.display(), width, height);
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = match parse_args(&args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error:#}");
            process::exit(2);
        }
    };

    let result = if is_layout_document(&options.layout) {
        render_document_previews(&options).and_then(|records| {
            print_preview_records(&records, options.format).map_err(CliError::from)
        })
    } else if options.format == OutputFormat::Json {
        Err(CliError::Message(anyhow!(
            "--format json requires a .layout.toml document"
        )))
    } else {
        run_legacy(&options).map_err(CliError::from)
    };

    if let Err(error) = result {
        print_error(&error, options.format);
        process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thermalwriter::layout_engine::diagnostic::TOML_PARSE_CODE;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn value_options_reject_missing_and_option_like_values() {
        for values in [
            &["--output-dir"][..],
            &["--size"][..],
            &["--profile"][..],
            &["--format"][..],
            &["--output-dir", "--matrix"][..],
            &["--size", "--list"][..],
            &["--profile", "--matrix"][..],
            &["--format", "--matrix"][..],
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
            "--format",
            "json",
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
                format: OutputFormat::Json,
            }
        );
    }

    #[test]
    fn document_matrix_has_exact_native_surface_profiles() {
        let options = PreviewOptions {
            output_dir: PathBuf::from("target/preview"),
            matrix: true,
            size: None,
            profile: None,
            layout: "layouts/neon-composer.layout.toml".to_owned(),
            list: false,
            format: OutputFormat::Human,
        };
        let targets = document_targets(&options).expect("matrix targets");
        assert_eq!(
            targets
                .iter()
                .map(|target| (
                    target.profile.as_str(),
                    target.surface.dimensions(),
                    target.surface.id,
                ))
                .collect::<Vec<_>>(),
            vec![
                ("square", (480, 480), SurfaceProfileId::Rectangular),
                ("portrait", (480, 1280), SurfaceProfileId::Rectangular),
                ("wide", (1280, 480), SurfaceProfileId::Rectangular),
                (
                    "thermalright-curved-2400x1080",
                    (2400, 1080),
                    SurfaceProfileId::ThermalrightCurved2400x1080,
                ),
            ]
        );
    }

    #[test]
    fn invalid_toml_reports_stable_file_diagnostic() {
        let error = parse_layout_document("version = [", Path::new("broken.layout.toml"))
            .expect_err("invalid TOML");
        match error {
            CliError::Diagnostics(diagnostics) => {
                assert_eq!(diagnostics.len(), 1);
                assert_eq!(diagnostics[0].code, TOML_PARSE_CODE);
                assert_eq!(
                    diagnostics[0].file,
                    Some(PathBuf::from("broken.layout.toml"))
                );
                assert_eq!(diagnostics[0].severity, DiagnosticSeverity::Error);
            }
            other => panic!("expected diagnostics, got {other:?}"),
        }
    }

    #[test]
    fn semantic_recipe_errors_keep_stable_codes_and_context() {
        let input = r#"
version = 1
name = "broken"
modules = []

[profiles.square]
recipe = "not-a-recipe"
"#;
        let document =
            parse_layout_document(input, Path::new("broken.layout.toml")).expect("document syntax");
        let target = target_for_profile("square").expect("square target");
        let diagnostics = validate(&document, &target.surface).expect_err("semantic error");
        let diagnostic = annotate_diagnostic(
            diagnostics[0].clone(),
            Path::new("broken.layout.toml"),
            &target,
        );
        assert_eq!(diagnostic.code, "TWLAYOUT-E025");
        assert_eq!(diagnostic.profile.as_deref(), Some("square"));
        assert_eq!(diagnostic.property_path.as_deref(), Some("recipe"));
        assert_eq!(diagnostic.file, Some(PathBuf::from("broken.layout.toml")));
    }

    #[test]
    fn output_format_accepts_human_and_json_only() {
        assert_eq!(parse_output_format("human").unwrap(), OutputFormat::Human);
        assert_eq!(parse_output_format("json").unwrap(), OutputFormat::Json);
        assert!(parse_output_format("yaml").is_err());
    }

    #[test]
    fn preview_record_json_contains_public_fields_and_diagnostics() {
        let record = PreviewRecord {
            profile: "square".to_owned(),
            dimensions: "480x480".to_owned(),
            path: "target/preview/neon-composer-480x480.png".to_owned(),
            diagnostics: vec![LayoutDiagnostic::new(
                "TWLAYOUT-E025",
                DiagnosticSeverity::Error,
                "Unknown layout recipe",
                "recipe is not supported",
                "Use a supported recipe.",
            )],
        };
        let value = serde_json::to_value(record).expect("preview JSON");
        assert_eq!(value["profile"], "square");
        assert_eq!(value["dimensions"], "480x480");
        assert_eq!(value["path"], "target/preview/neon-composer-480x480.png");
        assert_eq!(value["diagnostics"][0]["code"], "TWLAYOUT-E025");
    }
}
