//! Micro-benches for the SVG render pipeline: full per-layout renders, each
//! extracted sub-stage in isolation, background decoding, and the final
//! premultiplied-to-straight pixel conversion.
//!
//! Compare workflow: `cargo bench -- --save-baseline before`, make a change,
//! then `cargo bench -- --baseline before` to diff against the saved run.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use thermalwriter::config::{Config, builtin_layouts};
use thermalwriter::layout_engine::{
    LayoutDocument, LayoutEngineRenderer, ResvgSceneBackend, SurfaceProfileId,
    resolve_surface_profile,
};
use thermalwriter::render::background::decode_to_pixmap;
use thermalwriter::render::frontmatter::LayoutFrontmatter;
use thermalwriter::render::svg::{SvgRenderer, composite, parse_svg, rasterize};
use thermalwriter::render::{FrameSource, RawFrame, SensorData};
use thermalwriter::sensor::history::SensorHistory;
use thermalwriter::sensor::mock::{fill_synthetic_history, mock_sensors};
use thermalwriter::theme::ThemePalette;

const BUILTIN_SVG_LAYOUTS: &[(&str, &str)] = &[
    ("neon-dash-v2", builtin_layouts::SVG_NEON_DASH_V2),
    ("neon-dash", builtin_layouts::SVG_NEON_DASH),
    ("arc-gauge", builtin_layouts::SVG_ARC_GAUGE),
    ("cyber-grid", builtin_layouts::SVG_CYBER_GRID),
];

/// Build an `SvgRenderer` for a built-in layout, pre-filled with synthetic
/// history where the layout's frontmatter declares history metrics — mirrors
/// `examples/preview_layout.rs` so bench numbers reflect the same renderer
/// shape the daemon actually runs.
fn build_renderer(template: &str, sensors: &SensorData) -> SvgRenderer<'static> {
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

    renderer
}

fn bench_full_render_per_layout(c: &mut Criterion) {
    let sensors = mock_sensors();
    let mut group = c.benchmark_group("render_full");
    for (name, template) in BUILTIN_SVG_LAYOUTS {
        let mut renderer = build_renderer(template, &sensors);
        group.bench_function(*name, |b| {
            b.iter(|| renderer.render(black_box(&sensors)).unwrap());
        });
    }
    group.finish();
}

/// Sub-stage benches on the default layout (neon-dash-v2) — representative of
/// the shipped default, avoids an N-layout x M-stage matrix.
fn bench_render_sub_stages(c: &mut Criterion) {
    let sensors = mock_sensors();
    let renderer = build_renderer(builtin_layouts::SVG_NEON_DASH_V2, &sensors);

    let mut group = c.benchmark_group("render_sub_stages");

    group.bench_function("build_context", |b| {
        b.iter(|| renderer.build_context(black_box(&sensors)));
    });

    let context = renderer.build_context(&sensors);
    group.bench_function("render_template", |b| {
        b.iter(|| renderer.render_template(black_box(&context)).unwrap());
    });

    let svg_string = renderer.render_template(&context).unwrap();
    group.bench_function("parse_svg", |b| {
        b.iter(|| parse_svg(black_box(&svg_string), renderer.options()).unwrap());
    });

    let tree = parse_svg(&svg_string, renderer.options()).unwrap();
    group.bench_function("rasterize", |b| {
        b.iter(|| rasterize(black_box(&tree), 480, 480).unwrap());
    });

    let layout_pixmap = rasterize(&tree, 480, 480).unwrap();
    let fallback_color = tiny_skia::Color::from_rgba8(8, 8, 15, 255);
    group.bench_function("composite", |b| {
        b.iter(|| composite(black_box(&layout_pixmap), None, 480, 480, fallback_color).unwrap());
    });

    group.finish();
}

/// `builtin_layouts::BG_DARK_GRADIENT` is a 4x4 placeholder — too small to
/// give a meaningful decode baseline. Generate solid-color PNGs at realistic
/// sizes instead: a 480x480 (no resize) case and a 1920x1080 (real downscale)
/// case, matching the decode path background images actually take.
fn make_solid_color_png(width: u32, height: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
    use image::{ImageBuffer, ImageFormat, Rgb};
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(width, height, Rgb([r, g, b]));
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Png).unwrap();
    buf.into_inner()
}

fn bench_decode_background(c: &mut Criterion) {
    let png_480 = make_solid_color_png(480, 480, 32, 64, 128);
    let png_1080p = make_solid_color_png(1920, 1080, 32, 64, 128);

    let mut group = c.benchmark_group("background_decode");
    group.bench_function("decode_480", |b| {
        b.iter(|| decode_to_pixmap(black_box(&png_480)).unwrap());
    });
    group.bench_function("decode_1080p_downscale", |b| {
        b.iter(|| decode_to_pixmap(black_box(&png_1080p)).unwrap());
    });
    group.finish();
}

fn bench_raw_frame_from_pixmap(c: &mut Criterion) {
    let sensors = mock_sensors();
    let renderer = build_renderer(builtin_layouts::SVG_NEON_DASH_V2, &sensors);
    let context = renderer.build_context(&sensors);
    let svg_string = renderer.render_template(&context).unwrap();
    let tree = parse_svg(&svg_string, renderer.options()).unwrap();
    let pixmap = rasterize(&tree, 480, 480).unwrap();

    c.bench_function("raw_frame_from_pixmap", |b| {
        b.iter(|| RawFrame::from_pixmap(black_box(&pixmap)));
    });
}

#[derive(Debug, Clone, Copy)]
struct TargetProfile {
    id: &'static str,
    width: u32,
    height: u32,
    surface_id: SurfaceProfileId,
}

impl TargetProfile {
    const fn pixels(self) -> u64 {
        self.width as u64 * self.height as u64
    }
}

const TARGET_PROFILES: &[TargetProfile] = &[
    TargetProfile {
        id: "square",
        width: 480,
        height: 480,
        surface_id: SurfaceProfileId::Rectangular,
    },
    TargetProfile {
        id: "portrait",
        width: 480,
        height: 1280,
        surface_id: SurfaceProfileId::Rectangular,
    },
    TargetProfile {
        id: "wide",
        width: 1280,
        height: 480,
        surface_id: SurfaceProfileId::Rectangular,
    },
    TargetProfile {
        id: "thermalright-curved-2400x1080",
        width: 2400,
        height: 1080,
        surface_id: SurfaceProfileId::ThermalrightCurved2400x1080,
    },
];

#[derive(Debug)]
struct ProfileMeasurement {
    profile: TargetProfile,
    median_ms: f64,
}

fn median(values: &mut [f64]) -> f64 {
    assert!(
        !values.is_empty(),
        "Criterion did not produce timing samples"
    );
    values.sort_by(|left, right| left.total_cmp(right));
    values[values.len() / 2]
}

fn build_layout_engine_renderer(profile: TargetProfile) -> LayoutEngineRenderer<ResvgSceneBackend> {
    let document = LayoutDocument::from_toml(builtin_layouts::NEON_COMPOSER)
        .expect("flagship layout document should parse");
    let surface = resolve_surface_profile(profile.width, profile.height, profile.surface_id)
        .expect("target profile should be registered");
    LayoutEngineRenderer::new(document, *surface, ResvgSceneBackend)
}

/// Measure the flagship document through the shared LayoutEngineRenderer path at
/// every target surface. Criterion's custom timing loop supplies both the
/// statistical benchmark and the samples used by the normalized scaling gate.
fn bench_layout_document_scaling(c: &mut Criterion) {
    let sensors = mock_sensors();
    let mut group = c.benchmark_group("layout_document_scaling");
    group.sample_size(10);
    let mut measurements = Vec::with_capacity(TARGET_PROFILES.len());

    for &profile in TARGET_PROFILES {
        let mut renderer = build_layout_engine_renderer(profile);
        renderer
            .render(&sensors)
            .expect("flagship layout document should render during warmup");
        let samples = Arc::new(Mutex::new(Vec::new()));
        let recorded_samples = Arc::clone(&samples);

        group.throughput(Throughput::Elements(profile.pixels()));
        group.bench_with_input(
            BenchmarkId::new("layout_document", profile.id),
            &profile,
            |b, _profile| {
                b.iter_custom(|iters| {
                    let started = Instant::now();
                    for _ in 0..iters {
                        black_box(
                            renderer
                                .render(black_box(&sensors))
                                .expect("flagship layout document should render"),
                        );
                    }
                    let elapsed = started.elapsed();
                    let per_iter_ms = elapsed.as_secs_f64() * 1_000.0 / iters as f64;
                    recorded_samples.lock().unwrap().push(per_iter_ms);
                    elapsed
                });
            },
        );
        drop(recorded_samples);

        let mut samples = Arc::try_unwrap(samples)
            .expect("Criterion timing samples should have one owner")
            .into_inner()
            .unwrap();
        measurements.push(ProfileMeasurement {
            profile,
            median_ms: median(&mut samples),
        });
    }
    group.finish();

    let tick_rate = Config::default().display.tick_rate;
    assert!(tick_rate > 0, "default display tick rate must be positive");
    let tick_deadline_ms = 1_000.0 / f64::from(tick_rate);
    let square = measurements
        .first()
        .expect("target profile matrix must include the square baseline");
    let square_pixels = square.profile.pixels() as f64;

    eprintln!(
        "layout_engine_scaling backend=resvg tick_rate={}fps tick_deadline_ms={:.3}",
        tick_rate, tick_deadline_ms
    );
    for measurement in &measurements {
        let pixels = measurement.profile.pixels() as f64;
        let ms_per_megapixel = measurement.median_ms * 1_000_000.0 / pixels;
        let latency_ratio = measurement.median_ms / square.median_ms;
        let pixel_ratio = pixels / square_pixels;
        let allowed_ratio = 1.30 * pixel_ratio;
        let scaling_pass = latency_ratio <= allowed_ratio;
        let tick_pass = measurement.median_ms <= tick_deadline_ms;

        eprintln!(
            "layout_engine_scaling profile={} dimensions={}x{} latency_ms={:.3} ms_per_megapixel={:.3} scaling_ratio={:.3}/{:.3} scaling={} tick={}",
            measurement.profile.id,
            measurement.profile.width,
            measurement.profile.height,
            measurement.median_ms,
            ms_per_megapixel,
            latency_ratio,
            allowed_ratio,
            if scaling_pass { "PASS" } else { "FAIL" },
            if tick_pass { "PASS" } else { "FAIL" },
        );

        assert!(
            scaling_pass,
            "resvg pixel-scaling gate failed for {}: latency ratio {:.3} > allowed {:.3}",
            measurement.profile.id, latency_ratio, allowed_ratio
        );
        assert!(
            tick_pass,
            "resvg tick deadline failed for {}: {:.3} ms > {:.3} ms",
            measurement.profile.id, measurement.median_ms, tick_deadline_ms
        );
    }
}

criterion_group!(
    benches,
    bench_full_render_per_layout,
    bench_render_sub_stages,
    bench_decode_background,
    bench_raw_frame_from_pixmap,
    bench_layout_document_scaling
);
criterion_main!(benches);
