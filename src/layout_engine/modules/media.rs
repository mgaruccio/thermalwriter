//! Bounded local-image scene emission for the layout engine.
//!
//! Media is the one initial module class that may occupy a curved display's
//! protected bridge. The solver decides whether the supplied bounds are the
//! complete canvas or a readable zone; this emitter intentionally emits one
//! image node for either result.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    ModuleCapabilities, ModuleEmitter, ResolvedBindings, ThemeTokens, emission_diagnostic,
    validate_bounds,
};
use crate::layout_engine::LayoutDiagnostic;
use crate::layout_engine::document::MediaDocument;
use crate::layout_engine::scene::{ImageFit, ImageNode, MIN_OPACITY, SceneNode};
use crate::layout_engine::solver::SolvedModule;
use crate::render::background::BackgroundImage;
use crate::validation::{PathContainmentError, validate_path_within_dir};

/// The bounded image fit vocabulary exposed by the media module.
///
/// This is an alias rather than a second enum so that media configuration and
/// the backend-neutral [`ImageNode`] cannot drift apart.
pub type MediaFit = ImageFit;

/// A local image module with bounded scene properties.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaModule {
    /// A local filename or path. When `media_root` is set this must be relative
    /// to that root.
    pub source: PathBuf,
    pub fit: MediaFit,
    /// The requested bridge behavior is metadata; the solved bounds remain the
    /// source of truth for whether this emission spans the canvas.
    pub span_bridge: bool,
    pub opacity: f32,
    #[serde(skip)]
    media_root: Option<PathBuf>,
}

impl MediaModule {
    /// Construct a local media module with safe defaults.
    pub fn new(source: impl Into<PathBuf>) -> Self {
        Self {
            source: source.into(),
            fit: MediaFit::Contain,
            span_bridge: false,
            opacity: 1.0,
            media_root: None,
        }
    }

    /// Construct a module whose source is resolved below an approved root.
    pub fn in_root(root: impl Into<PathBuf>, source: impl Into<PathBuf>) -> Self {
        Self::new(source).with_media_root(root)
    }

    /// Build a runtime emitter from a persisted media document.
    ///
    /// Older documents use `binding` as the catalog source. Newer documents may
    /// provide `source` explicitly; an empty source therefore falls back to the
    /// existing binding field without changing the canonical TOML shape.
    pub fn from_document(document: &MediaDocument, media_root: impl Into<PathBuf>) -> Self {
        let source = if document.source.as_os_str().is_empty() {
            PathBuf::from(document.binding.trim())
        } else {
            document.source.clone()
        };
        Self {
            source,
            fit: document.fit,
            span_bridge: document.span_bridge,
            opacity: document.opacity,
            media_root: Some(media_root.into()),
        }
    }

    pub fn with_media_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.media_root = Some(root.into());
        self
    }

    pub fn with_fit(mut self, fit: MediaFit) -> Self {
        self.fit = fit;
        self
    }

    pub fn with_span_bridge(mut self, span_bridge: bool) -> Self {
        self.span_bridge = span_bridge;
        self
    }

    pub fn with_opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }

    /// Return the configured media root, if this module has one.
    pub fn media_root(&self) -> Option<&Path> {
        self.media_root.as_deref()
    }

    fn resolve_source(&self, module_id: &str) -> Result<PathBuf, LayoutDiagnostic> {
        if self.source.as_os_str().is_empty() {
            return Err(media_diagnostic(
                module_id,
                "source",
                "media source is empty",
                "Choose a local PNG or JPEG file, or bind the module to a media filename",
            ));
        }

        let source_display = self.source.display().to_string();
        if let Some(root) = &self.media_root {
            let source = self.source.to_str().ok_or_else(|| {
                media_diagnostic(
                    module_id,
                    "source",
                    format!("media source `{source_display}` is not valid UTF-8"),
                    "Choose a local media filename with a valid UTF-8 path",
                )
            })?;
            return validate_path_within_dir(root, source, "Media")
                .map_err(|error| containment_diagnostic(module_id, source, error));
        }

        // Without an explicitly configured root, relative sources are resolved
        // below the process working directory. Absolute sources are checked
        // below their lexical parent so symlinks cannot escape silently.
        if self
            .source
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(media_diagnostic(
                module_id,
                "source",
                format!("media source `{source_display}` contains a parent-directory escape"),
                "Use a filename below the approved media directory instead of `..`",
            ));
        }

        if self.source.is_absolute() {
            let Some(name) = self.source.file_name().and_then(|value| value.to_str()) else {
                return Err(media_diagnostic(
                    module_id,
                    "source",
                    format!("media source `{source_display}` has no usable filename"),
                    "Choose a local media file with a normal filename",
                ));
            };
            let root = self.source.parent().unwrap_or_else(|| Path::new("/"));
            return validate_path_within_dir(root, name, "Media")
                .map_err(|error| containment_diagnostic(module_id, name, error));
        }

        let root = std::env::current_dir().map_err(|error| {
            media_diagnostic(
                module_id,
                "source",
                format!("could not resolve the approved media directory: {error}"),
                "Run the layout preview from a readable media directory or configure one explicitly",
            )
        })?;
        validate_path_within_dir(&root, &source_display, "Media")
            .map_err(|error| containment_diagnostic(module_id, &source_display, error))
    }

    fn validate_decode(&self, module_id: &str, source: &Path) -> Result<(), LayoutDiagnostic> {
        let image = BackgroundImage::from_file(source)
            .map_err(|error| decode_diagnostic(module_id, source, format!("{error:#}")))?;
        image
            .to_pixmap(480, 480)
            .map(|_| ())
            .map_err(|error| decode_diagnostic(module_id, source, format!("{error:#}")))
    }
}

impl ModuleEmitter for MediaModule {
    fn capabilities(&self) -> ModuleCapabilities {
        ModuleCapabilities {
            can_span_bridge: true,
            supports_binding: true,
            supports_threshold: false,
            supports_variants: true,
        }
    }

    fn emit(
        &self,
        solved: &SolvedModule,
        _data: &ResolvedBindings,
        _theme: &ThemeTokens,
    ) -> Result<Vec<SceneNode>, LayoutDiagnostic> {
        validate_bounds(&solved.id, solved.bounds)?;

        if !self.opacity.is_finite() || !(MIN_OPACITY..=1.0).contains(&self.opacity) {
            return Err(media_diagnostic(
                &solved.id,
                "opacity",
                format!("media opacity must be finite and between {MIN_OPACITY:.1} and 1.0"),
                format!("Set opacity to a value between {MIN_OPACITY:.1} and 1.0"),
            ));
        }

        let source = self.resolve_source(&solved.id)?;
        self.validate_decode(&solved.id, &source)?;

        // A panoramic image is represented by one logical node. The solver has
        // already assigned either full-canvas or readable-zone bounds.
        Ok(vec![SceneNode::Image(ImageNode::new(
            solved.bounds,
            source.to_string_lossy(),
            self.fit,
            self.opacity,
        ))])
    }
}

fn media_diagnostic(
    module_id: &str,
    property: &str,
    reason: impl Into<String>,
    fix: impl Into<String>,
) -> LayoutDiagnostic {
    let mut diagnostic = emission_diagnostic(module_id, reason, fix);
    diagnostic.property_path = Some(property.to_owned());
    diagnostic
}

fn decode_diagnostic(
    module_id: &str,
    source: &Path,
    detail: impl Into<String>,
) -> LayoutDiagnostic {
    let detail = detail.into();
    let lower = detail.to_ascii_lowercase();
    let (reason, fix) = if lower.contains("too large") {
        (
            format!(
                "media source `{}` exceeds the 8 MB file limit: {detail}",
                source.display()
            ),
            "Choose a local image no larger than 8 MB",
        )
    } else if (lower.contains("limit exceeded")
        || lower.contains("exceeds")
        || lower.contains("rasterization requires")
        || lower.contains("allocation"))
        && !lower.contains("format could not")
    {
        (
            format!(
                "media source `{}` exceeds the bounded decode dimensions or allocation: {detail}",
                source.display()
            ),
            "Resize the image to at most 8192 pixels per side and keep the decoded allocation bounded",
        )
    } else {
        (
            format!(
                "media source `{}` is not a valid decodable image: {detail}",
                source.display()
            ),
            "Choose a valid local PNG or JPEG image",
        )
    };
    media_diagnostic(module_id, "source", reason, fix)
}

fn containment_diagnostic(
    module_id: &str,
    source: &str,
    error: PathContainmentError,
) -> LayoutDiagnostic {
    let (reason, fix) = match error {
        PathContainmentError::NotFound { .. } => (
            format!("media source `{source}` was not found below the approved media directory"),
            "Choose an existing local PNG or JPEG filename below the approved media directory",
        ),
        PathContainmentError::BaseInaccessible { source: error, .. } => (
            format!("approved media directory is not accessible: {error}"),
            "Create the configured media directory and grant the daemon read access",
        ),
        PathContainmentError::Escapes { .. }
        | PathContainmentError::Absolute { .. }
        | PathContainmentError::ParentDir { .. } => (
            format!("media source `{source}` escapes the approved media directory"),
            "Use a relative media filename and keep the resolved file below the approved media directory",
        ),
    };
    media_diagnostic(module_id, "source", reason, fix)
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};

    use tempfile::tempdir;

    use super::*;
    use crate::layout_engine::scene::{ImageFit, SceneNode};
    use crate::layout_engine::solver::Rect;

    fn tiny_png(width: u32, height: u32) -> Vec<u8> {
        let image =
            image::RgbaImage::from_pixel(width, height, image::Rgba([0x22, 0x44, 0x66, 0xff]));
        let mut output = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut output, image::ImageFormat::Png)
            .expect("tiny PNG encoding");
        output.into_inner()
    }

    fn solved(bounds: Rect) -> SolvedModule {
        SolvedModule {
            id: "wallpaper".to_owned(),
            bounds,
            zone: None,
        }
    }

    fn fixture(name: &str, bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let directory = tempdir().expect("fixture directory");
        let path = directory.path().join(name);
        fs::write(&path, bytes).expect("fixture image");
        (directory, path)
    }

    #[test]
    fn media_capabilities_advertise_bridge_spanning() {
        assert_eq!(
            MediaModule::new("wallpaper.png").capabilities(),
            ModuleCapabilities {
                can_span_bridge: true,
                supports_binding: true,
                supports_threshold: false,
                supports_variants: true,
            }
        );
    }

    #[test]
    fn full_span_emits_one_image_for_the_complete_canvas() {
        let (directory, _) = fixture("panorama.png", &tiny_png(8, 2));
        let module = MediaModule::in_root(directory.path(), "panorama.png")
            .with_fit(ImageFit::Cover)
            .with_span_bridge(true)
            .with_opacity(0.8);
        let bounds = Rect::new(0, 0, 2400, 1080);
        let nodes = module
            .emit(
                &solved(bounds),
                &ResolvedBindings::default(),
                &ThemeTokens::default(),
            )
            .expect("panoramic media scene");

        assert_eq!(nodes.len(), 1, "panorama must not be duplicated per zone");
        let SceneNode::Image(image) = &nodes[0] else {
            panic!("media emits one image node");
        };
        assert_eq!(image.bounds, bounds);
        assert_eq!(image.fit, ImageFit::Cover);
        assert_eq!(image.opacity, 0.8);
    }

    #[test]
    fn local_media_uses_the_solved_readable_zone() {
        let (directory, _) = fixture("local.png", &tiny_png(2, 8));
        let module = MediaModule::in_root(directory.path(), "local.png");
        let bounds = Rect::new(16, 120, 928, 172);
        let nodes = module
            .emit(
                &solved(bounds),
                &ResolvedBindings::default(),
                &ThemeTokens::default(),
            )
            .expect("local media scene");
        let SceneNode::Image(image) = &nodes[0] else {
            panic!("media emits one image node");
        };
        assert_eq!(image.bounds, bounds);
    }

    #[test]
    fn missing_and_escaping_media_are_actionable() {
        let (directory, _) = fixture("outside.png", &tiny_png(2, 2));
        let missing = MediaModule::in_root(directory.path(), "missing.png")
            .emit(
                &solved(Rect::new(0, 0, 480, 480)),
                &ResolvedBindings::default(),
                &ThemeTokens::default(),
            )
            .expect_err("missing media must fail");
        assert!(missing.reason.contains("not found"));
        assert_eq!(missing.property_path.as_deref(), Some("source"));

        let escaping = MediaModule::in_root(directory.path(), "../outside.png")
            .emit(
                &solved(Rect::new(0, 0, 480, 480)),
                &ResolvedBindings::default(),
                &ThemeTokens::default(),
            )
            .expect_err("escaping media must fail");
        assert!(escaping.reason.contains("escapes"));
        assert!(escaping.fix.contains("approved media"));
    }

    #[test]
    fn oversized_and_invalid_media_are_rejected_before_scene_emission() {
        let directory = tempdir().expect("fixture directory");
        let oversized = directory.path().join("oversized.png");
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&oversized)
            .expect("sparse fixture");
        file.set_len(8 * 1024 * 1024 + 1)
            .expect("sparse fixture size");
        let oversized_error = MediaModule::in_root(directory.path(), "oversized.png")
            .emit(
                &solved(Rect::new(0, 0, 480, 480)),
                &ResolvedBindings::default(),
                &ThemeTokens::default(),
            )
            .expect_err("oversized media must fail");
        assert!(oversized_error.reason.contains("8 MB"));
        assert!(oversized_error.fix.contains("8 MB"));

        let invalid = directory.path().join("invalid.png");
        fs::write(&invalid, b"not an image").expect("invalid fixture");
        let invalid_error = MediaModule::in_root(directory.path(), "invalid.png")
            .emit(
                &solved(Rect::new(0, 0, 480, 480)),
                &ResolvedBindings::default(),
                &ThemeTokens::default(),
            )
            .expect_err("invalid media must fail");
        assert!(invalid_error.reason.contains("not a valid decodable image"));
        assert!(invalid_error.fix.contains("valid local"));
    }

    #[test]
    fn opacity_is_bounded_before_decode() {
        let module = MediaModule::new("unused.png").with_opacity(0.6);
        let error = module
            .emit(
                &solved(Rect::new(0, 0, 480, 480)),
                &ResolvedBindings::default(),
                &ThemeTokens::default(),
            )
            .expect_err("low opacity must fail");
        assert_eq!(error.property_path.as_deref(), Some("opacity"));
        assert!(error.reason.contains("between"));
    }
}
