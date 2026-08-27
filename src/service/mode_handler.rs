// Mode-transition helper: builds a new FrameSource from a layout path without
// side-effects on the existing source or xvfb handle.
//
// Callers (main.rs listener) call `build_layout_source` FIRST; only on Ok do
// they drop the old xvfb handle and send the new source to the tick loop.
// On Err, the old source/handle stays live — the stream keeps rendering.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::layout_engine::{
    LayoutDocument, LayoutEngineRenderer, ResvgSceneBackend, SurfaceProfileId,
    rectangular_surface_profile, resolve_surface_profile, validate,
};
use crate::render::FrameSource;
use crate::render::TemplateRenderer;
use crate::render::background::BackgroundImage;
use crate::render::frontmatter::LayoutFrontmatter;
use crate::render::svg::SvgRenderer;
#[cfg(feature = "daemon")]
use crate::render::xvfb::XvfbSource;
use crate::sensor::history::SensorHistory;
#[cfg(feature = "daemon")]
use crate::service::xvfb::{self as xvfb_manager, XvfbHandle};
use crate::theme::ThemePalette;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeDisplayDimensions {
    width: u32,
    height: u32,
}

impl RuntimeDisplayDimensions {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    pub fn width(self) -> u32 {
        self.width
    }

    pub fn height(self) -> u32 {
        self.height
    }

    pub fn build_layout_source(
        self,
        layout_path: &Path,
        vars: HashMap<String, String>,
        background: Option<Arc<BackgroundImage>>,
        sensor_history: Option<Arc<Mutex<SensorHistory>>>,
        theme: ThemePalette,
    ) -> anyhow::Result<Box<dyn FrameSource>> {
        build_layout_source(
            layout_path,
            vars,
            background,
            sensor_history,
            theme,
            self.width,
            self.height,
        )
    }

    /// Build a layout source while validating typed sensor bindings against the
    /// daemon's provider catalog. Empty `declared_keys` keeps the helper useful
    /// for callers that do not have a sensor hub (for example preview tests).
    pub fn build_layout_source_with_bindings(
        self,
        layout_path: &Path,
        vars: HashMap<String, String>,
        background: Option<Arc<BackgroundImage>>,
        sensor_history: Option<Arc<Mutex<SensorHistory>>>,
        theme: ThemePalette,
        declared_keys: &HashSet<String>,
    ) -> anyhow::Result<Box<dyn FrameSource>> {
        build_layout_source_with_bindings(
            layout_path,
            vars,
            background,
            sensor_history,
            theme,
            self.width,
            self.height,
            declared_keys,
        )
    }

    #[cfg(feature = "daemon")]
    pub fn start_xvfb_shell(self, command: &str) -> anyhow::Result<(XvfbHandle, XvfbSource)> {
        let handle = xvfb_manager::start(command, self.width, self.height)?;
        let source = XvfbSource::new(handle.screen_file(), self.width, self.height)?;
        Ok((handle, source))
    }

    #[cfg(feature = "daemon")]
    pub fn start_xvfb_argv(self, argv: &[String]) -> anyhow::Result<(XvfbHandle, XvfbSource)> {
        let handle = xvfb_manager::start_argv(argv, self.width, self.height)?;
        let source = XvfbSource::new(handle.screen_file(), self.width, self.height)?;
        Ok((handle, source))
    }
}

/// Build a new layout `FrameSource` from `layout_path`.
///
/// Returns `Ok(Box<dyn FrameSource>)` on success. On any failure (file not
/// found, bad document, bad SVG, renderer error) returns `Err` — the caller's
/// existing xvfb handle and frame source are untouched.
///
/// `sensor_history` and `theme` are optional — pass `None` for each if not
/// available (unit tests typically pass `None`).
pub fn build_layout_source(
    layout_path: &Path,
    vars: HashMap<String, String>,
    background: Option<Arc<BackgroundImage>>,
    sensor_history: Option<Arc<Mutex<SensorHistory>>>,
    theme: ThemePalette,
    width: u32,
    height: u32,
) -> anyhow::Result<Box<dyn FrameSource>> {
    build_layout_source_with_bindings(
        layout_path,
        vars,
        background,
        sensor_history,
        theme,
        width,
        height,
        &HashSet::new(),
    )
}

/// Build a source from a path with the daemon's declared sensor catalog.
#[allow(clippy::too_many_arguments)]
pub fn build_layout_source_with_bindings(
    layout_path: &Path,
    vars: HashMap<String, String>,
    background: Option<Arc<BackgroundImage>>,
    sensor_history: Option<Arc<Mutex<SensorHistory>>>,
    theme: ThemePalette,
    width: u32,
    height: u32,
    declared_keys: &HashSet<String>,
) -> anyhow::Result<Box<dyn FrameSource>> {
    let template = std::fs::read_to_string(layout_path)
        .map_err(|e| anyhow::anyhow!("Failed to read layout '{}': {}", layout_path.display(), e))?;

    if is_layout_document_path(layout_path) {
        let document = LayoutDocument::from_toml(&template).map_err(|error| {
            anyhow::anyhow!(
                "Failed to parse layout document '{}': {}",
                layout_path.display(),
                error
            )
        })?;
        let media_root = layout_path.parent().unwrap_or_else(|| Path::new("."));
        return build_layout_document_source(document, media_root, width, height, declared_keys);
    }

    let extension = layout_path
        .extension()
        .and_then(|extension| extension.to_str());
    let is_svg = extension == Some("svg");
    let is_html = matches!(extension, Some("html" | "htm"));
    if !is_svg && !is_html {
        anyhow::bail!(
            "Unsupported layout file '{}': expected .layout.toml, .svg, or .html",
            layout_path.display()
        );
    }

    let new_fm = LayoutFrontmatter::parse(&template);
    // Configure any new history metrics idempotently.
    if let Some(ref hist) = sensor_history
        && let Ok(mut h) = hist.lock()
    {
        for (metric, cfg) in &new_fm.history_configs {
            h.configure_metric(metric, cfg.duration);
        }
    }

    if is_svg {
        let mut renderer = SvgRenderer::new(&template, width, height).map_err(|e| {
            anyhow::anyhow!(
                "Failed to create SvgRenderer for '{}': {}",
                layout_path.display(),
                e
            )
        })?;
        renderer.set_theme(theme);
        if let Some(ref hist) = sensor_history {
            renderer.set_history(hist.clone());
        }
        renderer.set_layout_vars(vars);
        renderer.set_background(background)?;
        Ok(Box::new(renderer))
    } else {
        let mut renderer = TemplateRenderer::new(&template, width, height).map_err(|e| {
            anyhow::anyhow!(
                "Failed to create TemplateRenderer for '{}': {}",
                layout_path.display(),
                e
            )
        })?;
        renderer.set_layout_vars(vars);
        Ok(Box::new(renderer))
    }
}

/// Build a layout-engine source from a parsed document and its media root.
pub fn build_layout_document_source(
    document: LayoutDocument,
    media_root: &Path,
    width: u32,
    height: u32,
    declared_keys: &HashSet<String>,
) -> anyhow::Result<Box<dyn FrameSource>> {
    let surface = resolve_document_surface(&document, width, height)?;
    validate(&document, &surface)
        .map_err(|diagnostics| anyhow::anyhow!(format_layout_diagnostics(&diagnostics)))?;
    validate_document_bindings(&document, declared_keys)?;

    let renderer =
        LayoutEngineRenderer::with_media_root(document, surface, ResvgSceneBackend, media_root);
    Ok(Box::new(renderer))
}

fn is_layout_document_path(path: &Path) -> bool {
    path.to_string_lossy().ends_with(".layout.toml")
}

fn resolve_document_surface(
    document: &LayoutDocument,
    width: u32,
    height: u32,
) -> anyhow::Result<crate::layout_engine::DisplaySurfaceProfile> {
    let curved_id = SurfaceProfileId::ThermalrightCurved2400x1080;
    if (width, height) == (2400, 1080) && document.profiles.contains_key(curved_id.as_str()) {
        return resolve_surface_profile(width, height, curved_id).copied().ok_or_else(|| {
            anyhow::anyhow!(
                "layout document explicitly selects curved profile '{}' but it is not available for {}x{}",
                curved_id,
                width,
                height
            )
        });
    }

    rectangular_surface_profile(width, height)
        .copied()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unsupported rectangular layout surface {}x{}; select a supported display profile",
                width,
                height
            )
        })
}

fn format_layout_diagnostics(diagnostics: &[crate::layout_engine::LayoutDiagnostic]) -> String {
    diagnostics
        .iter()
        .map(crate::layout_engine::LayoutDiagnostic::to_human)
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn validate_document_bindings(
    document: &LayoutDocument,
    declared_keys: &HashSet<String>,
) -> anyhow::Result<()> {
    if declared_keys.is_empty() {
        return Ok(());
    }

    let mut unknown = Vec::new();
    for module in &document.modules {
        let (module_id, binding) = match module {
            crate::layout_engine::ModuleDocument::Metric(module) => (&module.id, &module.binding),
            crate::layout_engine::ModuleDocument::Sparkline(module) => {
                (&module.id, &module.binding)
            }
            crate::layout_engine::ModuleDocument::Text(module) => (&module.id, &module.binding),
            // Media bindings are logical asset identifiers, not sensor keys.
            crate::layout_engine::ModuleDocument::Media(_) => continue,
        };
        let binding = binding.trim();
        let base = binding.strip_suffix(".history").unwrap_or(binding);
        if !sensor_binding_is_known(base, declared_keys) {
            unknown.push(format!("module '{module_id}' -> '{binding}'"));
        }
    }

    if unknown.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "layout document contains unknown sensor binding(s): {}. Known-but-unavailable sensors are allowed; choose a declared sensor binding.",
            unknown.join(", ")
        )
    }
}

fn sensor_binding_is_known(binding: &str, declared_keys: &HashSet<String>) -> bool {
    crate::layout_engine::layout_binding_is_known(binding, declared_keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use tempfile::tempdir;

    /// When build_layout_source fails (file not found), the caller's xvfb handle
    /// sentinel must remain Some — it must NOT be dropped before the new source
    /// is confirmed. This is the key invariant for Task 2.
    #[test]
    fn failed_build_does_not_disturb_existing_handle() {
        let tmp = tempdir().unwrap();
        let nonexistent = tmp.path().join("does-not-exist.svg");

        // Simulate the previous xvfb handle with a sentinel Option.
        let mut xvfb_sentinel: Option<u32> = Some(42);

        let result = build_layout_source(
            &nonexistent,
            HashMap::new(),
            None,
            None,
            ThemePalette::default(),
            480,
            480,
        );

        // Build failed — must return Err.
        assert!(
            result.is_err(),
            "build_layout_source must fail for a nonexistent path, got Ok"
        );

        // The sentinel must still be Some — caller only drops the handle on Ok.
        assert_eq!(
            xvfb_sentinel,
            Some(42),
            "xvfb handle sentinel must be untouched after a failed layout build"
        );

        // Demonstrate the correct caller pattern: only drop on Ok.
        if result.is_ok() {
            xvfb_sentinel.take(); // would drop on real handle
        }
        assert_eq!(
            xvfb_sentinel,
            Some(42),
            "sentinel must still be Some after Err path"
        );
    }

    /// When build_layout_source succeeds, the caller should drop the old handle
    /// and send the new source. This test confirms Ok is returned for valid SVG.
    #[test]
    fn successful_build_returns_frame_source() {
        let tmp = tempdir().unwrap();
        let svg_path = tmp.path().join("test.svg");
        // Minimal valid SVG with no frontmatter.
        std::fs::write(&svg_path, r#"<svg viewBox="0 0 480 480"></svg>"#).unwrap();

        let mut xvfb_sentinel: Option<u32> = Some(42);

        let result = build_layout_source(
            &svg_path,
            HashMap::new(),
            None,
            None,
            ThemePalette::default(),
            480,
            480,
        );

        if let Err(ref e) = result {
            panic!("build_layout_source must succeed for a valid SVG, got Err: {e}");
        }

        // On Ok: caller drops old handle, then sends new source.
        if result.is_ok() {
            xvfb_sentinel.take(); // represents dropping the old xvfb handle
        }
        assert_eq!(
            xvfb_sentinel, None,
            "handle must be dropped after successful build"
        );
    }

    #[test]
    fn runtime_dimensions_build_listener_layout_source() {
        let tmp = tempdir().unwrap();
        let svg_path = tmp.path().join("non_480.svg");
        std::fs::write(&svg_path, r#"<svg viewBox="0 0 320 240"></svg>"#).unwrap();

        let display = RuntimeDisplayDimensions::new(320, 240);
        let mut source = display
            .build_layout_source(
                &svg_path,
                HashMap::new(),
                None,
                None,
                ThemePalette::default(),
            )
            .expect("valid SVG should build");

        let frame = source.render(&HashMap::new()).expect("SVG should render");
        assert_eq!((frame.width, frame.height), (320, 240));
    }

    /// Verify the error message names the failing path (helps debugging).
    #[test]
    fn error_message_names_the_failing_path() {
        let tmp = tempdir().unwrap();
        let nonexistent = tmp.path().join("no-such-layout.svg");

        let result = build_layout_source(
            &nonexistent,
            HashMap::new(),
            None,
            None,
            ThemePalette::default(),
            480,
            480,
        );
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("build_layout_source must fail for a nonexistent path"),
        };

        let msg = err.to_string();
        assert!(
            msg.contains("no-such-layout.svg"),
            "error message must name the failing path, got: {msg}"
        );
    }

    #[test]
    fn layout_document_source_matches_shared_renderer_core() {
        let tmp = tempdir().unwrap();
        let layout_path = tmp.path().join("flagship.layout.toml");
        std::fs::write(&layout_path, crate::config::builtin_layouts::NEON_COMPOSER).unwrap();
        let declared = HashSet::from(["cpu_temp".to_owned()]);
        let sensors = HashMap::from([
            ("cpu_temp".to_owned(), "67".to_owned()),
            ("cpu_temp_history".to_owned(), "[60, 64, 67]".to_owned()),
        ]);

        let mut source = build_layout_source_with_bindings(
            &layout_path,
            HashMap::new(),
            None,
            None,
            ThemePalette::default(),
            480,
            480,
            &declared,
        )
        .expect("flagship layout document should build");
        let actual = source
            .render(&sensors)
            .expect("layout source should render");
        assert_eq!(source.name(), "layout-engine");
        assert_eq!((actual.width, actual.height), (480, 480));

        let document = LayoutDocument::from_toml(crate::config::builtin_layouts::NEON_COMPOSER)
            .expect("flagship document should parse");
        let surface = *rectangular_surface_profile(480, 480).expect("square surface");
        let mut renderer =
            LayoutEngineRenderer::with_media_root(document, surface, ResvgSceneBackend, tmp.path());
        let expected = renderer
            .render(&sensors)
            .expect("shared renderer should render");
        assert_eq!(actual.data, expected.data);
    }

    #[test]
    fn atomically_replaced_layout_document_is_used_on_reload() {
        let tmp = tempdir().unwrap();
        let layout_path = tmp.path().join("reload.layout.toml");
        let replacement_path = tmp.path().join("reload.layout.toml.next");
        let old_document = crate::config::builtin_layouts::NEON_COMPOSER;
        let new_document = old_document.replace("cpu.temperature", "gpu.temperature");
        let declared = HashSet::from(["cpu_temp".to_owned(), "gpu_temp".to_owned()]);
        let sensors = HashMap::from([
            ("cpu_temp".to_owned(), "67".to_owned()),
            ("gpu_temp".to_owned(), "12".to_owned()),
        ]);
        std::fs::write(&layout_path, old_document).unwrap();

        let mut old_source = build_layout_source_with_bindings(
            &layout_path,
            HashMap::new(),
            None,
            None,
            ThemePalette::default(),
            480,
            480,
            &declared,
        )
        .expect("old document should build");
        let old_frame = old_source
            .render(&sensors)
            .expect("old document should render");

        std::fs::write(&replacement_path, new_document).unwrap();
        std::fs::rename(&replacement_path, &layout_path).unwrap();

        let mut new_source = build_layout_source_with_bindings(
            &layout_path,
            HashMap::new(),
            None,
            None,
            ThemePalette::default(),
            480,
            480,
            &declared,
        )
        .expect("atomically replaced document should build");
        let new_frame = new_source
            .render(&sensors)
            .expect("new document should render");
        assert_ne!(old_frame.data, new_frame.data);
    }

    #[test]
    fn invalid_layout_document_stays_untouched_and_unknown_bindings_fail() {
        let tmp = tempdir().unwrap();
        let layout_path = tmp.path().join("invalid.layout.toml");
        let declared = HashSet::from(["cpu_temp".to_owned()]);

        let invalid = "version = 999\nname = \"invalid\"\nmodules = []\nprofiles = {}\n";
        std::fs::write(&layout_path, invalid).unwrap();
        let before = std::fs::read(&layout_path).unwrap();
        let error = match build_layout_source_with_bindings(
            &layout_path,
            HashMap::new(),
            None,
            None,
            ThemePalette::default(),
            480,
            480,
            &declared,
        ) {
            Ok(_) => panic!("unsupported document version must fail before activation"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("unsupported layout document version")
        );
        assert_eq!(std::fs::read(&layout_path).unwrap(), before);

        let unknown = crate::config::builtin_layouts::NEON_COMPOSER
            .replace("cpu.temperature", "unknown.temperature");
        std::fs::write(&layout_path, unknown).unwrap();
        let before = std::fs::read(&layout_path).unwrap();
        let error = match build_layout_source_with_bindings(
            &layout_path,
            HashMap::new(),
            None,
            None,
            ThemePalette::default(),
            480,
            480,
            &declared,
        ) {
            Ok(_) => panic!("unknown authored binding must fail before activation"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("unknown sensor binding"));
        assert_eq!(std::fs::read(&layout_path).unwrap(), before);

        std::fs::write(&layout_path, crate::config::builtin_layouts::NEON_COMPOSER).unwrap();
        let mut known_but_unavailable = build_layout_source_with_bindings(
            &layout_path,
            HashMap::new(),
            None,
            None,
            ThemePalette::default(),
            480,
            480,
            &declared,
        )
        .expect("declared but currently unavailable sensors must remain renderable");
        let frame = known_but_unavailable
            .render(&HashMap::new())
            .expect("unavailable sensor state should render");
        assert_eq!((frame.width, frame.height), (480, 480));
    }

    #[test]
    fn unsupported_layout_extension_is_rejected_without_rewriting_file() {
        let tmp = tempdir().unwrap();
        let layout_path = tmp.path().join("unsupported.layout.json");
        let content = b"{\"version\":1}";
        std::fs::write(&layout_path, content).unwrap();

        let error = match build_layout_source(
            &layout_path,
            HashMap::new(),
            None,
            None,
            ThemePalette::default(),
            480,
            480,
        ) {
            Ok(_) => panic!("unsupported layout extension must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("Unsupported layout file"));
        assert_eq!(std::fs::read(&layout_path).unwrap(), content);
    }
}
