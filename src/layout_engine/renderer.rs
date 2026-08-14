//! Shared layout-document renderer for GUI previews and the daemon.
//!
//! The renderer owns the complete requested path: document validation and
//! solving, typed module emission, bounded local-media resolution, scene
//! rasterization, and conversion to the existing straight-RGB [`RawFrame`].
//! No hardware-specific encoding or preview transport lives here.

use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use super::diagnostic::LayoutDiagnostic;
use super::document::{LayoutDocument, ModuleDocument};
use super::media_cache::MediaCache;
use super::modules::{
    BindingValue, MediaModule, MetricModule, MetricVariant, ModuleEmitter, ResolvedBindings,
    SparklineModule, SparklineVariant, TextModule, ThemeTokens,
};
use super::scene::{Scene, SceneNode, TextAlignment, TextRole};
use super::solver::{SolvedModule, solve};
use super::surface::DisplaySurfaceProfile;
use super::svg_backend::{ResolvedMedia, SceneBackend};
use crate::render::{FrameSource, RawFrame, SensorData};

/// A shared layout-engine frame source.
///
/// The document, solver, module emitters, backend, and media cache are kept in
/// one path so preview and daemon callers cannot accidentally render different
/// pixels for the same document and sensor input.
pub struct LayoutEngineRenderer<B: SceneBackend> {
    document: LayoutDocument,
    surface: DisplaySurfaceProfile,
    backend: B,
    media: MediaCache,
}

impl<B: SceneBackend> LayoutEngineRenderer<B> {
    /// Construct a renderer using the current working directory as the
    /// approved local-media root.
    pub fn new(document: LayoutDocument, surface: DisplaySurfaceProfile, backend: B) -> Self {
        Self::with_media_root(document, surface, backend, default_media_root())
    }

    /// Construct a renderer with an explicit approved local-media root.
    pub fn with_media_root(
        document: LayoutDocument,
        surface: DisplaySurfaceProfile,
        backend: B,
        media_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            document,
            surface,
            backend,
            media: MediaCache::new(media_root),
        }
    }

    /// Construct a renderer with a caller-provided cache.
    pub fn with_media_cache(
        document: LayoutDocument,
        surface: DisplaySurfaceProfile,
        backend: B,
        media: MediaCache,
    ) -> Self {
        Self {
            document,
            surface,
            backend,
            media,
        }
    }

    /// Alias for callers constructing a renderer from a parsed document and
    /// explicit media root.
    pub fn from_document(
        document: LayoutDocument,
        surface: DisplaySurfaceProfile,
        backend: B,
        media_root: impl Into<PathBuf>,
    ) -> Self {
        Self::with_media_root(document, surface, backend, media_root)
    }

    /// Return the active document.
    pub fn document(&self) -> &LayoutDocument {
        &self.document
    }

    /// Replace the active document and invalidate decoded media.
    pub fn set_document(&mut self, document: LayoutDocument) {
        self.document = document;
        self.media.clear();
    }

    /// Return the active native surface profile.
    pub fn surface(&self) -> DisplaySurfaceProfile {
        self.surface
    }

    /// Replace the active profile and invalidate decoded media.
    pub fn set_surface(&mut self, surface: DisplaySurfaceProfile) {
        self.surface = surface;
        self.media.clear();
    }

    /// Return the resolved-media cache for diagnostics and bounded cache
    /// inspection by preview callers.
    pub fn media_cache(&self) -> &MediaCache {
        &self.media
    }

    /// Return mutable access to the resolved-media cache.
    pub fn media_cache_mut(&mut self) -> &mut MediaCache {
        &mut self.media
    }

    /// Return the backend used by this renderer.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Return mutable access to the backend.
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    fn render_frame(&mut self, sensors: &SensorData) -> Result<RawFrame> {
        let (scene, media) = self.compile_scene(sensors)?;
        let pixmap = self
            .backend
            .render(&scene, &media)
            .map_err(|diagnostic| anyhow!(diagnostic_with_profile(diagnostic, self.surface)))?;
        if (pixmap.width(), pixmap.height()) != (self.surface.width, self.surface.height) {
            return Err(anyhow!(backend_dimensions_diagnostic(
                self.surface,
                pixmap.width(),
                pixmap.height(),
            )));
        }
        Ok(RawFrame::from_pixmap(&pixmap))
    }

    fn compile_scene(&mut self, sensors: &SensorData) -> Result<(Scene, ResolvedMedia)> {
        if self.surface.width == 0 || self.surface.height == 0 {
            return Err(anyhow!(surface_diagnostic(self.surface)));
        }

        self.media
            .prepare(document_fingerprint(&self.document), self.surface);
        let solved = solve(&self.document, &self.surface)
            .map_err(|diagnostics| anyhow!(diagnostics_error(&diagnostics, self.surface)))?;
        if solved.modules.len() != self.document.modules.len() {
            return Err(anyhow!(diagnostic_with_profile(
                LayoutDiagnostic::new(
                    "TWLAYOUT-E032",
                    super::diagnostic::DiagnosticSeverity::Error,
                    "Layout solve returned an incomplete module list",
                    format!(
                        "solver returned {} modules for {} document modules",
                        solved.modules.len(),
                        self.document.modules.len()
                    ),
                    "Use a document that passes layout validation before rendering",
                ),
                self.surface,
            )));
        }

        let bindings = resolved_bindings(sensors);
        let theme = ThemeTokens::default();
        let mut scene = Scene::new(self.surface.width, self.surface.height);
        let mut image_sources = BTreeSet::new();

        for (module, solved_module) in self.document.modules.iter().zip(&solved.modules) {
            let nodes = emit_module(
                module,
                solved_module,
                &bindings,
                &theme,
                self.media.media_root(),
                self.surface,
            )
            .map_err(|diagnostic| anyhow!(diagnostic_with_profile(diagnostic, self.surface)))?;
            for node in nodes {
                if let SceneNode::Image(image) = &node {
                    image_sources.insert(image.source.clone());
                }
                scene.push(node);
            }
        }

        let mut media = ResolvedMedia::new();
        for source in image_sources {
            let asset = self
                .media
                .resolve_path(Path::new(&source), self.surface.width, self.surface.height)
                .map_err(|diagnostic| anyhow!(diagnostic_with_profile(diagnostic, self.surface)))?;
            media.insert(source, asset);
        }

        Ok((scene, media))
    }
}

impl<B: SceneBackend + Send> FrameSource for LayoutEngineRenderer<B> {
    fn render(&mut self, sensors: &SensorData) -> Result<RawFrame> {
        self.render_frame(sensors)
    }

    fn name(&self) -> &str {
        "layout-engine"
    }

    fn is_time_varying(&self) -> bool {
        false
    }
}

fn emit_module(
    module: &ModuleDocument,
    solved: &SolvedModule,
    bindings: &ResolvedBindings,
    theme: &ThemeTokens,
    media_root: &Path,
    surface: DisplaySurfaceProfile,
) -> Result<Vec<SceneNode>, LayoutDiagnostic> {
    let nodes = match module {
        ModuleDocument::Metric(document) => {
            MetricModule::new(document.id.clone(), document.binding.clone(), "")
                .with_variant(MetricVariant::from(document.variant.as_str()))
                .emit(solved, bindings, theme)
        }
        ModuleDocument::Sparkline(document) => SparklineModule::new(document.binding.clone())
            .with_variant(SparklineVariant::from(document.variant.as_str()))
            .emit(solved, bindings, theme),
        ModuleDocument::Text(document) => TextModule::bound(
            document.binding.clone(),
            document.id.clone(),
            text_role_from_variant(&document.variant),
            TextAlignment::Start,
        )
        .emit(solved, bindings, theme),
        ModuleDocument::Media(document) => {
            MediaModule::from_document(document, media_root.to_path_buf())
                .emit(solved, bindings, theme)
        }
    }?;

    if solved.id != module_id(module) {
        let mut diagnostic = LayoutDiagnostic::new(
            "TWLAYOUT-E032",
            super::diagnostic::DiagnosticSeverity::Error,
            "Layout module identity changed during solving",
            format!(
                "document module `{}` was assigned solved module `{}`",
                module_id(module),
                solved.id
            ),
            "Keep document module ids unique and render the validated solve result",
        );
        diagnostic.module_id = Some(module_id(module).to_owned());
        diagnostic.profile = Some(surface.id.as_str().to_owned());
        return Err(diagnostic);
    }

    Ok(nodes)
}

fn module_id(module: &ModuleDocument) -> &str {
    match module {
        ModuleDocument::Metric(document) => &document.id,
        ModuleDocument::Sparkline(document) => &document.id,
        ModuleDocument::Text(document) => &document.id,
        ModuleDocument::Media(document) => &document.id,
    }
}

fn text_role_from_variant(variant: &str) -> TextRole {
    match variant.trim() {
        "title" => TextRole::Title,
        "label" => TextRole::Label,
        "caption" => TextRole::Caption,
        "value" => TextRole::Value,
        "unit" => TextRole::Unit,
        "status" => TextRole::Status,
        _ => TextRole::Body,
    }
}

/// Convert the flat sensor boundary into typed module bindings.
///
/// Ordinary sensor values remain text values, matching the existing
/// `SensorData` contract.  When a caller supplies a JSON or comma-separated
/// history under a conventional `.history`/`_history` key, the same input is
/// also exposed through `ResolvedBindings::histories` for sparkline modules.
pub fn resolved_bindings(sensors: &SensorData) -> ResolvedBindings {
    let mut bindings: ResolvedBindings = sensors.clone().into();
    for (key, value) in sensors {
        if let Some(alias) = layout_binding_alias(key) {
            bindings.insert(alias, value.clone());
        }

        let Some(values) = parse_history(value) else {
            continue;
        };
        if !is_history_key(key) {
            continue;
        }
        bindings.insert_history(key.clone(), values.iter());
        if let Some(base) = key.strip_suffix("_history") {
            bindings.insert_history(format!("{base}.history"), values.iter());
            bindings.insert_history(base.to_owned(), values.iter());
            if let Some(alias) = layout_binding_alias(base) {
                bindings.insert_history(format!("{alias}.history"), values.iter());
                bindings.insert_history(alias, values.iter());
            }
        }
    }
    bindings
}

fn layout_binding_alias(key: &str) -> Option<&'static str> {
    super::bindings::layout_binding_alias(key)
}

fn is_history_key(key: &str) -> bool {
    key.ends_with(".history") || key.ends_with("_history")
}

fn parse_history(value: &str) -> Option<Vec<f64>> {
    let value = value.trim();
    let values = if value.starts_with('[') {
        serde_json::from_str::<Vec<f64>>(value).ok()?
    } else {
        let values = value
            .split([',', ' ', '\t', '\r', '\n'])
            .filter(|part| !part.is_empty())
            .map(str::parse::<f64>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .ok()?;
        if values.is_empty() {
            return None;
        }
        values
    };
    Some(
        values
            .into_iter()
            .filter(|value| value.is_finite())
            .collect(),
    )
}

fn default_media_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn document_fingerprint(document: &LayoutDocument) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{document:?}").hash(&mut hasher);
    hasher.finish()
}

fn diagnostics_error(diagnostics: &[LayoutDiagnostic], surface: DisplaySurfaceProfile) -> String {
    diagnostics
        .iter()
        .cloned()
        .map(|diagnostic| diagnostic_with_profile(diagnostic, surface).to_human())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn diagnostic_with_profile(
    mut diagnostic: LayoutDiagnostic,
    surface: DisplaySurfaceProfile,
) -> LayoutDiagnostic {
    if diagnostic.profile.is_none() {
        diagnostic.profile = Some(surface.id.as_str().to_owned());
    }
    diagnostic
}

fn surface_diagnostic(surface: DisplaySurfaceProfile) -> LayoutDiagnostic {
    LayoutDiagnostic::new(
        "TWLAYOUT-E032",
        super::diagnostic::DiagnosticSeverity::Error,
        "Invalid native surface dimensions",
        format!(
            "layout renderer cannot allocate a {}x{} scene",
            surface.width, surface.height
        ),
        "Choose a bounded display profile with positive native dimensions",
    )
}

fn backend_dimensions_diagnostic(
    surface: DisplaySurfaceProfile,
    width: u32,
    height: u32,
) -> LayoutDiagnostic {
    LayoutDiagnostic::new(
        "TWLAYOUT-E032",
        super::diagnostic::DiagnosticSeverity::Error,
        "Scene backend returned unexpected dimensions",
        format!(
            "backend returned {width}x{height} for native profile {}x{}",
            surface.width, surface.height
        ),
        "Use a scene backend that preserves the profile's native dimensions",
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use tempfile::tempdir;
    use tiny_skia::Pixmap;

    use super::*;
    use crate::layout_engine::document::{CURRENT_VERSION, MediaDocument, ProfileRecipeDocument};
    use crate::layout_engine::surface::{
        SurfaceProfileId, rectangular_surface_profile, resolve_surface_profile,
    };
    use crate::layout_engine::svg_backend::ResvgSceneBackend;

    fn media_document(path: &str, profile: &str, recipe: &str) -> LayoutDocument {
        LayoutDocument {
            version: CURRENT_VERSION,
            name: "renderer-fixture".to_owned(),
            preset: None,
            modules: vec![ModuleDocument::Media(MediaDocument {
                id: "wallpaper".to_owned(),
                binding: path.to_owned(),
                variant: "default".to_owned(),
                source: PathBuf::new(),
                fit: super::super::scene::ImageFit::Cover,
                span_bridge: true,
                opacity: 1.0,
            })],
            profiles: BTreeMap::from([(
                profile.to_owned(),
                ProfileRecipeDocument {
                    recipe: recipe.to_owned(),
                    bridge: Some("media-only".to_owned()),
                },
            )]),
        }
    }

    fn tiny_png(color: [u8; 4]) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(2, 2, image::Rgba(color));
        let mut output = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut output, image::ImageFormat::Png)
            .expect("fixture PNG");
        output.into_inner()
    }

    #[test]
    fn repeated_render_is_byte_identical_at_native_dimensions() {
        let directory = tempdir().expect("fixture directory");
        let filename = "wallpaper.png";
        fs::write(
            directory.path().join(filename),
            tiny_png([0x22, 0x44, 0x66, 0xff]),
        )
        .expect("fixture image");
        let surface =
            *resolve_surface_profile(2400, 1080, SurfaceProfileId::ThermalrightCurved2400x1080)
                .expect("curved fixture surface");
        let document = media_document(filename, "thermalright-curved-2400x1080", "zoned-panorama");
        let mut renderer = LayoutEngineRenderer::with_media_root(
            document,
            surface,
            ResvgSceneBackend,
            directory.path(),
        );
        let sensors = SensorData::new();

        let first = renderer.render(&sensors).expect("first render");
        let second = renderer.render(&sensors).expect("second render");
        assert_eq!((first.width, first.height), (2400, 1080));
        let center = ((540 * 2400 + 1200) * 3) as usize;
        assert_eq!(&first.data[center..center + 3], &[0x22, 0x44, 0x66]);
        assert_eq!(first.data, second.data);
        assert_eq!(renderer.media_cache().len(), 1);
        assert_eq!(first.data.len(), 2400 * 1080 * 3);
    }

    #[test]
    fn media_and_profile_changes_invalidate_the_cache() {
        let directory = tempdir().expect("fixture directory");
        let filename = "wallpaper.png";
        let path = directory.path().join(filename);
        fs::write(&path, tiny_png([0x22, 0x44, 0x66, 0xff])).expect("fixture image");
        let square = *rectangular_surface_profile(480, 480).expect("square fixture surface");
        let wide = *rectangular_surface_profile(1280, 480).expect("wide fixture surface");
        let mut renderer = LayoutEngineRenderer::with_media_root(
            media_document(filename, "square", "column"),
            square,
            ResvgSceneBackend,
            directory.path(),
        );
        let before = renderer.render(&SensorData::new()).expect("square render");
        assert_eq!(renderer.media_cache().len(), 1);

        fs::write(&path, tiny_png([0xaa, 0xbb, 0xcc, 0xff])).expect("changed image");
        let changed = renderer.render(&SensorData::new()).expect("changed render");
        assert_ne!(before.data, changed.data);
        assert_eq!(renderer.media_cache().len(), 1);

        renderer.set_surface(wide);
        renderer.set_document(media_document(filename, "wide", "two-column"));
        let resized = renderer.render(&SensorData::new()).expect("wide render");
        assert_eq!((resized.width, resized.height), (1280, 480));
        assert_eq!(renderer.media_cache().len(), 1);
    }

    #[test]
    fn sensor_history_is_exposed_to_sparkline_bindings() {
        let sensors = SensorData::from([("cpu_history".to_owned(), "[1.0, 2.5, 3.0]".to_owned())]);
        let bindings = resolved_bindings(&sensors);
        assert_eq!(
            bindings.history("cpu.history"),
            Some([1.0, 2.5, 3.0].as_slice())
        );
        assert!(matches!(
            bindings.get("cpu_history"),
            Some(BindingValue::Text(value)) if value == "[1.0, 2.5, 3.0]"
        ));
    }

    #[test]
    fn malformed_media_returns_a_diagnostic_instead_of_panicking() {
        let directory = tempdir().expect("fixture directory");
        let filename = "not-an-image.bin";
        fs::write(directory.path().join(filename), b"not an image").expect("fixture bytes");
        let surface = *rectangular_surface_profile(480, 480).expect("square fixture surface");
        let document = media_document(filename, "square", "column");
        let mut renderer = LayoutEngineRenderer::with_media_root(
            document,
            surface,
            ResvgSceneBackend,
            directory.path(),
        );
        let error = renderer
            .render(&SensorData::new())
            .expect_err("malformed media must fail safely");
        assert!(
            error.to_string().contains("TWLAYOUT-E031")
                || error.to_string().contains("TWLAYOUT-E015")
        );
    }

    #[allow(dead_code)]
    fn _pixmap_fixture(width: u32, height: u32) -> Pixmap {
        Pixmap::new(width, height).expect("test pixmap dimensions")
    }
}
