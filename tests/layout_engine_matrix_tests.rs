use std::collections::{HashMap, HashSet};
use std::path::Path;

use thermalwriter::layout_engine::{
    solve, validate, LayoutDocument, LayoutEngineRenderer, ResvgSceneBackend, SurfaceProfileId,
    rectangular_surface_profile, resolve_surface_profile,
    BRIDGE_VIOLATION_CODE, RECIPE_OVERFLOW_CODE,
};
use thermalwriter::render::{FrameSource, RawFrame, SensorData};
use thermalwriter::service::mode_handler::build_layout_document_source;

const FLAGSHIP: &str = include_str!("../layouts/neon-composer.layout.toml");

#[derive(Clone, Copy)]
struct Fixture {
    name: &'static str,
    surface: thermalwriter::layout_engine::DisplaySurfaceProfile,
}

fn fixtures() -> [Fixture; 4] {
    [
        Fixture { name: "square", surface: *rectangular_surface_profile(480, 480).unwrap() },
        Fixture { name: "portrait", surface: *rectangular_surface_profile(480, 1280).unwrap() },
        Fixture { name: "wide", surface: *rectangular_surface_profile(1280, 480).unwrap() },
        Fixture {
            name: "curved",
            surface: *resolve_surface_profile(2400, 1080, SurfaceProfileId::ThermalrightCurved2400x1080).unwrap(),
        },
    ]
}

fn fixed_data() -> SensorData {
    HashMap::from([
        ("cpu.temperature".into(), "62.5".into()),
        ("cpu.temperature.history".into(), "[50,55,60,62.5]".into()),
    ])
}

fn render_preview(doc: LayoutDocument, surface: thermalwriter::layout_engine::DisplaySurfaceProfile, data: &SensorData) -> anyhow::Result<RawFrame> {
    let mut renderer = LayoutEngineRenderer::with_media_root(doc, surface, ResvgSceneBackend, Path::new("."));
    renderer.render(data)
}

fn render_daemon(doc: LayoutDocument, surface: thermalwriter::layout_engine::DisplaySurfaceProfile, data: &SensorData) -> anyhow::Result<RawFrame> {
    let mut source = build_layout_document_source(
        doc,
        Path::new("."),
        surface.width,
        surface.height,
        &HashSet::new(),
    )?;
    source.render(data)
}

#[test]
fn flagship_preview_and_daemon_match_for_every_target_profile() {
    let data = fixed_data();
    let doc = LayoutDocument::from_toml(FLAGSHIP).unwrap();

    for fixture in fixtures() {
        let preview = render_preview(doc.clone(), fixture.surface, &data)
            .unwrap_or_else(|error| panic!("preview failed for {}: {error:#}", fixture.name));
        let daemon = render_daemon(doc.clone(), fixture.surface, &data)
            .unwrap_or_else(|error| panic!("daemon failed for {}: {error:#}", fixture.name));
        assert_eq!(preview.data, daemon.data, "pixel mismatch for {}", fixture.name);
        assert_eq!((preview.width, preview.height), fixture.surface.dimensions());
    }
}

fn document(modules: &str, profiles: &str) -> LayoutDocument {
    LayoutDocument::from_toml(&format!(
        "version = 1\nname = \"matrix-variant\"\n\n{modules}\n{profiles}"
    ))
    .unwrap()
}

const METRIC: &str = r#"[[modules]]
id = "metric"
kind = "metric"
binding = "cpu.temperature"
variant = "hero"
"#;

#[test]
fn fixture_variants_render_or_report_stable_diagnostics() {
    let data = fixed_data();
    let square = fixtures()[0].surface;
    let profiles = "[profiles.square]\nrecipe = \"column\"\n";

    let sparse = document(METRIC, profiles);
    let sparse_frame = render_preview(sparse.clone(), square, &data).unwrap();
    assert_eq!((sparse_frame.width, sparse_frame.height), (480, 480));
    let sparse_daemon = render_daemon(sparse, square, &data).unwrap();
    assert_eq!(sparse_frame.data, sparse_daemon.data);

    let dense = document(
        &format!("{METRIC}{}", METRIC.replace("id = \"metric\"", "id = \"metric-b\"")),
        profiles,
    );
    assert!(validate(&dense, &square).is_ok());
    assert!(render_preview(dense, square, &data).is_ok(), "dense fixture should fit its profile");

    let long_label = document(
        r#"[[modules]]
id = "label"
kind = "text"
binding = "host.name"
variant = "body"
"#,
        profiles,
    );
    let long_data = HashMap::from([("host.name".into(), "A very long deterministic label for layout regression".into())]);
    assert!(render_preview(long_label, square, &long_data).is_ok());

    let missing_sensor = document(METRIC, profiles);
    assert!(render_preview(missing_sensor, square, &HashMap::new()).is_ok(), "missing bindings use stable unavailable state");
}

#[test]
fn curved_metrics_stay_in_readable_zones_and_bridge_policy_is_bounded() {
    let curved = fixtures()[3].surface;
    let doc = LayoutDocument::from_toml(FLAGSHIP).unwrap();
    let solved = solve(&doc, &curved).unwrap();
    assert!(solved.modules.iter().all(|module| module.zone.is_some()));
    for module in &solved.modules {
        let zone = curved.readable_zones.iter().find(|zone| Some(zone.name) == module.zone.as_deref()).unwrap();
        assert!(zone.bounds.contains(module.bounds.x, module.bounds.y));
        assert!(module.bounds.x + module.bounds.width <= zone.bounds.right());
        assert!(module.bounds.y + module.bounds.height <= zone.bounds.bottom());
    }

    let unsafe_bridge = document(
        r#"[[modules]]
id = "metric"
kind = "metric"
binding = "cpu.temperature"
variant = "hero"
"#,
        "[profiles.thermalright-curved-2400x1080]\nrecipe = \"zoned-panorama\"\nbridge = \"unsafe\"\n",
    );
    let diagnostics = validate(&unsafe_bridge, &curved).unwrap_err();
    assert!(diagnostics.iter().any(|diagnostic| diagnostic.code == BRIDGE_VIOLATION_CODE));
}

#[test]
fn overflow_fixture_is_rejected_without_baseline_auto_acceptance() {
    let profiles = "[profiles.square]\nrecipe = \"column\"\n";
    let modules = (0..8).map(|index| format!("[[modules]]\nid = \"metric-{index}\"\nkind = \"metric\"\nbinding = \"cpu.temperature\"\nvariant = \"hero\"\n")).collect::<String>();
    let overflow = document(&modules, profiles);
    let diagnostics = validate(&overflow, &fixtures()[0].surface).unwrap_err();
    assert!(diagnostics.iter().any(|diagnostic| diagnostic.code == RECIPE_OVERFLOW_CODE));
}

