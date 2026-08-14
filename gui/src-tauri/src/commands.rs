use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::ipc::Response;
use thermalwriter::config::Config;
use thermalwriter::dbus_types::DisplayProxy;
use thermalwriter::layout_engine::diagnostic::TOML_PARSE_CODE;
use thermalwriter::layout_engine::{
    DiagnosticSeverity, DisplaySurfaceProfile, ImageFit, LayoutDiagnostic, LayoutDocument,
    LayoutDocumentError, LayoutEngineRenderer, ModuleDocument, PERSISTENCE_PATH_CODE,
    PreviewTopology, ResvgSceneBackend, SolvedLayout, SurfaceProfileId, resolve_surface_profile,
    solve, validate,
};
use thermalwriter::render::background::BackgroundImage;
use thermalwriter::render::frontmatter::{LayoutFrontmatter, VariableDecl as FrontmatterVar};
use thermalwriter::render::palette::{self, SchemeSuggestion};
use thermalwriter::render::svg::SvgRenderer;
use thermalwriter::render::{FrameSource, SensorData, TemplateRenderer};
use thermalwriter::sensor::history::SensorHistory;
use thermalwriter::validation::{
    LayoutVarError, PathContainmentError, validate_layout_vars, validate_path_within_dir,
};

use crate::error::AppError;

/// Application state shared across Tauri commands.
///
/// Wrapped in `tauri::State<'_, RendererState>` (which is internally `Arc`-backed),
/// so we never wrap this in another Arc/Mutex when registering with `manage()`.
/// The mutable renderer cache lives behind its own `std::sync::Mutex` (sync,
/// since rendering is CPU-bound and not held across `.await`).
pub struct RendererState {
    pub layout_dir: PathBuf,
    pub background_dir: PathBuf,
    pub config_path: PathBuf,
    cache: Mutex<RendererCache>,
    typed_cache: Mutex<TypedRendererCache>,
}

/// Cached preview renderer state. The renderer is keyed by layout; the decoded
/// background pixmap is keyed by image name so slider/keystroke previews avoid
/// re-reading and re-decoding the same background file.
/// Cached preview renderer. SVG and HTML layouts use different backends.
enum CachedRenderer {
    Svg(Box<SvgRenderer<'static>>),
    Html(TemplateRenderer),
}

struct RendererCache {
    current_layout: Option<String>,
    renderer: Option<CachedRenderer>,
    current_background: Option<String>,
    background_pixmap: Option<Arc<BackgroundImage>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TypedPreviewCacheKey {
    document_fingerprint: String,
    profile: SurfaceProfileId,
    width: u32,
    height: u32,
    media_fingerprint: String,
}

struct TypedRendererCache {
    key: Option<TypedPreviewCacheKey>,
    renderer: Option<LayoutEngineRenderer<ResvgSceneBackend>>,
}

impl RendererState {
    pub fn new(layout_dir: PathBuf, background_dir: PathBuf, config_path: PathBuf) -> Self {
        Self {
            layout_dir,
            background_dir,
            config_path,
            cache: Mutex::new(RendererCache {
                current_layout: None,
                renderer: None,
                current_background: None,
                background_pixmap: None,
            }),
            typed_cache: Mutex::new(TypedRendererCache {
                key: None,
                renderer: None,
            }),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct LayoutSummary {
    pub name: String,
    pub kind: String,
    pub configurable: bool,
}

#[derive(Debug, Serialize)]
pub struct VariableDecl {
    pub name: String,
    #[serde(rename = "type")]
    pub var_type: String,
    pub default: String,
    pub help: String,
    pub value: String,
    // Slider bounds for "number" vars; null for other types.
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct SensorDescriptor {
    pub key: String,
    pub name: String,
    pub unit: String,
    /// Last measured poll cost attributed to this key (microseconds).
    pub cost_us: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LayoutDocumentResponse {
    pub document: LayoutDocument,
    pub document_fingerprint: String,
}

#[derive(Debug, Serialize)]
pub struct LayoutValidationResponse {
    pub width: u32,
    pub height: u32,
    pub valid: bool,
    pub diagnostics: Vec<LayoutDiagnostic>,
    pub topology: PreviewTopology,
    pub document_fingerprint: String,
}

#[derive(Debug, Serialize)]
pub struct LayoutPreviewResponse {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub diagnostics: Vec<LayoutDiagnostic>,
    pub topology: PreviewTopology,
    pub document_fingerprint: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LayoutSaveResponse {
    pub name: String,
    pub path: PathBuf,
    pub document_fingerprint: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum LayoutActivationState {
    Active,
    DaemonUnavailable { reason: String },
    ActivationFailed { reason: String },
    ActiveButDefaultNotPersisted { reason: String },
}

#[derive(Debug, Serialize)]
pub struct LayoutApplyResponse {
    pub saved: LayoutSaveResponse,
    pub activation: LayoutActivationState,
}

#[tauri::command]
pub fn list_layouts(
    state: tauri::State<'_, RendererState>,
) -> Result<Vec<LayoutSummary>, AppError> {
    let mut layouts = Vec::new();
    for name in list_layout_names(&state.layout_dir) {
        let path = match validate_layout_path(&state.layout_dir, &name) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let configurable = std::fs::read_to_string(&path)
            .map(|content| !LayoutFrontmatter::parse(&content).variables.is_empty())
            .unwrap_or(false);
        let kind = if name.ends_with(".svg") {
            "svg"
        } else if name.ends_with(".layout.toml") {
            "layout"
        } else {
            "html"
        }
        .to_string();
        layouts.push(LayoutSummary {
            name,
            kind,
            configurable,
        });
    }
    Ok(layouts)
}

#[tauri::command]
pub fn get_layout_vars(
    layout: String,
    state: tauri::State<'_, RendererState>,
) -> Result<Vec<VariableDecl>, AppError> {
    let path = validate_layout_path(&state.layout_dir, &layout)?;
    let content = std::fs::read_to_string(&path).map_err(|e| AppError::LayoutIo(e.to_string()))?;
    let frontmatter = LayoutFrontmatter::parse(&content);

    let config = Config::load(&state.config_path).map_err(|e| AppError::Config(e.to_string()))?;
    let overrides = config.layout_vars.get(&layout);

    let mut vars = frontmatter
        .variables
        .iter()
        .map(|(name, decl)| VariableDecl {
            name: name.clone(),
            var_type: decl.var_type.clone(),
            default: decl.default.clone(),
            help: decl.help.clone(),
            value: overrides
                .and_then(|m| m.get(name))
                .cloned()
                .unwrap_or_else(|| decl.default.clone()),
            min: decl.min,
            max: decl.max,
            step: decl.step,
        })
        .collect::<Vec<_>>();
    vars.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(vars)
}

#[tauri::command]
pub fn get_saved_vars(
    layout: String,
    state: tauri::State<'_, RendererState>,
) -> Result<HashMap<String, String>, AppError> {
    let config = Config::load(&state.config_path).map_err(|e| AppError::Config(e.to_string()))?;
    Ok(config.layout_vars.get(&layout).cloned().unwrap_or_default())
}
#[tauri::command]
pub fn load_layout_preset(preset: String) -> Result<LayoutDocumentResponse, AppError> {
    let requested = preset.trim();
    let normalized = requested.strip_suffix(".layout.toml").unwrap_or(requested);
    if normalized != "neon-composer" {
        return Err(AppError::LayoutNotFound(format!("preset {requested}")));
    }

    let document =
        parse_layout_document(None, thermalwriter::config::builtin_layouts::NEON_COMPOSER)?;
    layout_document_response(document)
}

#[tauri::command]
pub fn load_layout_document(
    name: String,
    state: tauri::State<'_, RendererState>,
) -> Result<LayoutDocumentResponse, AppError> {
    load_layout_document_impl(&state.layout_dir, &name)
}

fn load_layout_document_impl(
    layout_dir: &Path,
    name: &str,
) -> Result<LayoutDocumentResponse, AppError> {
    let path = validate_layout_path(layout_dir, name)?;
    if !path
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.to_ascii_lowercase().ends_with(".layout.toml"))
    {
        return Err(AppError::InvalidLayout(
            "typed layout documents must use the .layout.toml suffix".into(),
        ));
    }

    let content =
        std::fs::read_to_string(&path).map_err(|error| AppError::LayoutIo(error.to_string()))?;
    let document = parse_layout_document(Some(&path), &content)?;
    reject_layout_media_paths(layout_dir, &document)?;
    Ok(LayoutDocumentResponse {
        document,
        document_fingerprint: fingerprint_bytes(content.as_bytes()),
    })
}

#[tauri::command]
pub fn validate_layout_document(
    draft: LayoutDocument,
    profile: SurfaceProfileId,
    width: u32,
    height: u32,
    state: tauri::State<'_, RendererState>,
) -> Result<LayoutValidationResponse, AppError> {
    let mut response = validate_layout_document_impl(draft.clone(), profile, width, height)?;
    let media_diagnostics = layout_media_path_diagnostics(&state.layout_dir, &draft);
    response.valid &= media_diagnostics.is_empty();
    response.diagnostics.extend(media_diagnostics);
    Ok(response)
}

#[tauri::command]
pub fn copy_layout_design_context(
    draft: LayoutDocument,
    profile: SurfaceProfileId,
    width: u32,
    height: u32,
    state: tauri::State<'_, RendererState>,
) -> Result<String, AppError> {
    copy_layout_design_context_impl(&draft, profile, width, height, &state.layout_dir)
}

/// Build the paste-only design context from the same validation and solver path as
/// the preview command.  The formatter intentionally receives no runtime sensor
/// data or media bytes: a context describes authoring decisions, not a frame.
fn copy_layout_design_context_impl(
    draft: &LayoutDocument,
    profile: SurfaceProfileId,
    width: u32,
    height: u32,
    layout_dir: &Path,
) -> Result<String, AppError> {
    let surface = resolve_preview_surface(profile, width, height)?;
    let validation = validate_layout_document_impl(draft.clone(), profile, width, height)?;
    let mut diagnostics = validation.diagnostics;
    let solved = if diagnostics.is_empty() {
        match solve(draft, &surface) {
            Ok(solved) => Some(solved),
            Err(mut solve_diagnostics) => {
                diagnostics.append(&mut solve_diagnostics);
                None
            }
        }
    } else {
        None
    };
    diagnostics.extend(layout_media_path_diagnostics(layout_dir, draft));

    Ok(format_layout_design_context(
        draft,
        &surface,
        solved.as_ref(),
        &diagnostics,
    ))
}

fn format_layout_design_context(
    document: &LayoutDocument,
    surface: &DisplaySurfaceProfile,
    solved: Option<&SolvedLayout>,
    diagnostics: &[LayoutDiagnostic],
) -> String {
    let configured_profile = document.profiles.get(surface.id.as_str());
    let recipe = configured_profile
        .map(|profile| profile.recipe.trim())
        .filter(|recipe| !recipe.is_empty())
        .map(context_value)
        .or_else(|| solved.map(|layout| layout.recipe.as_str().to_owned()))
        .unwrap_or_else(|| "<not configured>".to_owned());
    let bridge_policy = configured_profile
        .and_then(|profile| profile.bridge.as_deref())
        .map(str::trim)
        .filter(|bridge| !bridge.is_empty())
        .map(context_value)
        .unwrap_or_else(|| "local-only".to_owned());

    let mut output = String::new();
    let document_name = context_value(&document.name);
    let profile_name = context_value(surface.id.as_str());
    let topology = context_topology(surface, &bridge_policy);

    let _ = writeln!(output, "# Thermalwriter layout design context");
    let _ = writeln!(
        output,
        "- Document: {document_name} (schema {})",
        document.version
    );
    if let Some(preset) = document.preset.as_deref() {
        let _ = writeln!(output, "- Preset: {}", context_value(preset));
    }
    let _ = writeln!(
        output,
        "- Target: {profile_name} ({}x{})",
        surface.width, surface.height
    );
    let _ = writeln!(output, "- Topology: {topology}");
    let _ = writeln!(output);

    let _ = writeln!(output, "## Profile");
    let _ = writeln!(output, "- Recipe: {recipe}");
    let _ = writeln!(output, "- Bridge policy: {bridge_policy}");
    let _ = writeln!(output, "- Readable regions:");
    for zone in surface.readable_zones {
        let _ = writeln!(
            output,
            "  - {}: {}",
            context_value(zone.name),
            context_rect(
                zone.bounds.x,
                zone.bounds.y,
                zone.bounds.width,
                zone.bounds.height,
            )
        );
    }
    if surface.readable_zones.is_empty() {
        let _ = writeln!(output, "  - none");
    }
    let _ = writeln!(output, "- Protected regions:");
    for region in surface.protected_regions {
        let _ = writeln!(
            output,
            "  - {}: {}",
            context_value(region.name),
            context_rect(
                region.bounds.x,
                region.bounds.y,
                region.bounds.width,
                region.bounds.height,
            )
        );
    }
    if surface.protected_regions.is_empty() {
        let _ = writeln!(output, "  - none");
    }
    let _ = writeln!(output);

    let _ = writeln!(output, "## Ordered modules");
    if document.modules.is_empty() {
        let _ = writeln!(output, "- None.");
    } else {
        for (index, module) in document.modules.iter().enumerate() {
            let zone = solved
                .and_then(|layout| layout.modules.get(index))
                .map(|module| {
                    module
                        .zone
                        .as_deref()
                        .map(context_zone_name)
                        .unwrap_or_else(|| {
                            if surface.preview == PreviewTopology::CurvedPanorama {
                                "spanning".to_owned()
                            } else {
                                "full-surface".to_owned()
                            }
                        })
                })
                .unwrap_or_else(|| "unresolved".to_owned());
            let _ = writeln!(
                output,
                "{}. {} — {} — {} — {} — {}",
                index + 1,
                context_value(context_module_id(module)),
                context_module_kind(module),
                context_module_binding(module),
                zone,
                context_value(context_module_variant(module)),
            );
        }
    }
    let _ = writeln!(output);

    let _ = writeln!(output, "## Bindings and styles");
    if document.modules.is_empty() {
        let _ = writeln!(output, "- None.");
    } else {
        for module in &document.modules {
            let _ = writeln!(
                output,
                "- {}: binding={}; style={}",
                context_value(context_module_id(module)),
                context_module_binding(module),
                context_module_style(module),
            );
        }
    }
    let _ = writeln!(output);

    let _ = writeln!(output, "## Solved geometry");
    if let Some(layout) = solved {
        let _ = writeln!(output, "- Recipe: {}", layout.recipe);
        let _ = writeln!(
            output,
            "- Canvas bounds: {}",
            context_rect(
                layout.bounds.x,
                layout.bounds.y,
                layout.bounds.width,
                layout.bounds.height,
            )
        );
        for (index, module) in layout.modules.iter().enumerate() {
            let zone = module
                .zone
                .as_deref()
                .map(context_zone_name)
                .unwrap_or_else(|| {
                    if surface.preview == PreviewTopology::CurvedPanorama {
                        "spanning".to_owned()
                    } else {
                        "full-surface".to_owned()
                    }
                });
            let fallback_id = document
                .modules
                .get(index)
                .map(context_module_id)
                .unwrap_or("<unknown>");
            let id = if module.id.is_empty() {
                context_value(fallback_id)
            } else {
                context_value(&module.id)
            };
            let _ = writeln!(
                output,
                "- {}: {}; zone={}",
                id,
                context_rect(
                    module.bounds.x,
                    module.bounds.y,
                    module.bounds.width,
                    module.bounds.height,
                ),
                zone
            );
        }
    } else {
        let _ = writeln!(output, "- Unavailable until layout validation passes.");
    }
    let _ = writeln!(output);

    let _ = writeln!(output, "## Problems");
    if diagnostics.is_empty() {
        let _ = writeln!(output, "- None (validation passed for this profile).");
    } else {
        for diagnostic in diagnostics {
            let _ = writeln!(
                output,
                "- {} [{}] {}",
                context_value(&diagnostic.code),
                diagnostic.severity,
                context_value(&diagnostic.message),
            );
            if let Some(profile) = diagnostic.profile.as_deref() {
                let _ = writeln!(output, "  - Profile: {}", context_value(profile));
            }
            if let Some(module_id) = diagnostic.module_id.as_deref() {
                let _ = writeln!(output, "  - Module: {}", context_value(module_id));
            }
            if let Some(property_path) = diagnostic.property_path.as_deref() {
                let _ = writeln!(output, "  - Property: {}", context_value(property_path));
            }
            if diagnostic.line.is_some() || diagnostic.column.is_some() {
                let location = match (diagnostic.line, diagnostic.column) {
                    (Some(line), Some(column)) => format!("line {line}, column {column}"),
                    (Some(line), None) => format!("line {line}"),
                    (None, Some(column)) => format!("column {column}"),
                    (None, None) => unreachable!(),
                };
                let _ = writeln!(output, "  - Location: {location}");
            }
            let _ = writeln!(output, "  - Reason: {}", context_value(&diagnostic.reason));
            let _ = writeln!(output, "  - Fix: {}", context_value(&diagnostic.fix));
        }
    }
    let _ = writeln!(output);

    let _ = writeln!(output, "## Workflow");
    let _ = writeln!(output, "Generate → validate → preview → inspect → iterate.");
    let _ = writeln!(
        output,
        "1. Generate or revise the typed document and its ordered modules."
    );
    let _ = writeln!(
        output,
        "2. Validate the document for the selected profile and resolve every problem above."
    );
    let _ = writeln!(
        output,
        "3. Preview the validated document at the target dimensions."
    );
    let _ = writeln!(
        output,
        "4. Inspect the rendered frame for readability, bridge protection, and module order."
    );
    let _ = writeln!(
        output,
        "5. Iterate on bindings, variants, or module order, then validate and preview again."
    );
    let _ = writeln!(output);
    let _ = writeln!(output, "## Next check");
    let _ = writeln!(
        output,
        "validate → preview_layout → inspect the rendered frame for profile `{profile_name}`"
    );

    output
}

fn context_value(value: &str) -> String {
    let value = value.trim();
    let mut sanitized = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control() || character == '`' {
            sanitized.push(' ');
        } else {
            sanitized.push(character);
        }
    }
    if sanitized.trim().is_empty() {
        "<none>".to_owned()
    } else {
        sanitized.trim().to_owned()
    }
}

fn context_rect(x: u32, y: u32, width: u32, height: u32) -> String {
    format!("x={x}, y={y}, width={width}, height={height}")
}

fn context_topology(surface: &DisplaySurfaceProfile, bridge_policy: &str) -> String {
    if surface.preview == PreviewTopology::CurvedPanorama {
        let zones = surface
            .readable_zones
            .iter()
            .map(|zone| context_zone_name(zone.name))
            .collect::<Vec<_>>()
            .join("/");
        format!("readable zones {zones}; protected bridge; {bridge_policy} spanning")
    } else {
        "single readable surface; no protected bridge".to_owned()
    }
}

fn context_zone_name(name: &str) -> String {
    context_value(name.strip_suffix("-readable").unwrap_or(name))
}

fn context_module_kind(module: &ModuleDocument) -> &'static str {
    match module {
        ModuleDocument::Metric(_) => "metric",
        ModuleDocument::Sparkline(_) => "sparkline",
        ModuleDocument::Text(_) => "text",
        ModuleDocument::Media(_) => "media",
    }
}

fn context_module_id(module: &ModuleDocument) -> &str {
    match module {
        ModuleDocument::Metric(module) => &module.id,
        ModuleDocument::Sparkline(module) => &module.id,
        ModuleDocument::Text(module) => &module.id,
        ModuleDocument::Media(module) => &module.id,
    }
}

fn context_module_variant(module: &ModuleDocument) -> &str {
    match module {
        ModuleDocument::Metric(module) => &module.variant,
        ModuleDocument::Sparkline(module) => &module.variant,
        ModuleDocument::Text(module) => &module.variant,
        ModuleDocument::Media(module) => &module.variant,
    }
}

fn context_module_binding(module: &ModuleDocument) -> String {
    match module {
        ModuleDocument::Metric(module) => context_value(&module.binding),
        ModuleDocument::Sparkline(module) => context_value(&module.binding),
        ModuleDocument::Text(module) => context_value(&module.binding),
        ModuleDocument::Media(module) => {
            if module.binding.trim().is_empty() {
                "media source (bytes omitted)".to_owned()
            } else {
                format!("{} (media bytes omitted)", context_value(&module.binding))
            }
        }
    }
}

fn context_module_style(module: &ModuleDocument) -> String {
    match module {
        ModuleDocument::Metric(module) => format!("variant={}", context_value(&module.variant)),
        ModuleDocument::Sparkline(module) => {
            format!("variant={}", context_value(&module.variant))
        }
        ModuleDocument::Text(module) => format!("variant={}", context_value(&module.variant)),
        ModuleDocument::Media(module) => format!(
            "variant={}; fit={}; opacity={:.2}; span_bridge={}",
            context_value(&module.variant),
            context_image_fit(module.fit),
            module.opacity,
            if module.span_bridge {
                "requested"
            } else {
                "off"
            },
        ),
    }
}

fn context_image_fit(fit: ImageFit) -> &'static str {
    match fit {
        ImageFit::Contain => "contain",
        ImageFit::Cover => "cover",
    }
}

fn validate_layout_document_impl(
    draft: LayoutDocument,
    profile: SurfaceProfileId,
    width: u32,
    height: u32,
) -> Result<LayoutValidationResponse, AppError> {
    let surface = resolve_preview_surface(profile, width, height)?;
    let document_fingerprint = document_fingerprint(&draft)?;
    let diagnostics = match validate(&draft, &surface) {
        Ok(_) => Vec::new(),
        Err(diagnostics) => diagnostics,
    };

    Ok(LayoutValidationResponse {
        width,
        height,
        valid: diagnostics.is_empty(),
        diagnostics,
        topology: surface.preview,
        document_fingerprint,
    })
}

#[tauri::command]
pub fn preview_layout_document(
    draft: LayoutDocument,
    profile: SurfaceProfileId,
    width: u32,
    height: u32,
    state: tauri::State<'_, RendererState>,
) -> Result<LayoutPreviewResponse, AppError> {
    preview_layout_document_impl(draft, profile, width, height, &state)
}

fn preview_layout_document_impl(
    draft: LayoutDocument,
    profile: SurfaceProfileId,
    width: u32,
    height: u32,
    state: &RendererState,
) -> Result<LayoutPreviewResponse, AppError> {
    let surface = resolve_preview_surface(profile, width, height)?;
    let document_fingerprint = document_fingerprint(&draft)?;
    let mut diagnostics = layout_media_path_diagnostics(&state.layout_dir, &draft);
    diagnostics.extend(match validate(&draft, &surface) {
        Ok(_) => Vec::new(),
        Err(diagnostics) => diagnostics,
    });
    if !diagnostics.is_empty() {
        return Ok(LayoutPreviewResponse {
            width,
            height,
            rgba: Vec::new(),
            diagnostics,
            topology: surface.preview,
            document_fingerprint,
        });
    }

    let key = TypedPreviewCacheKey {
        document_fingerprint: document_fingerprint.clone(),
        profile,
        width,
        height,
        media_fingerprint: media_fingerprint(&draft, &state.layout_dir),
    };
    let mut cache = state
        .typed_cache
        .lock()
        .map_err(|_| AppError::StatePoisoned)?;
    if cache.key.as_ref() != Some(&key) || cache.renderer.is_none() {
        cache.renderer = Some(LayoutEngineRenderer::with_media_root(
            draft,
            surface,
            ResvgSceneBackend,
            state.layout_dir.clone(),
        ));
        cache.key = Some(key);
    }

    let renderer = cache
        .renderer
        .as_mut()
        .ok_or_else(|| AppError::Render("typed renderer not initialized".into()))?;
    let frame = renderer
        .render(&mock_layout_sensors())
        .map_err(|error| AppError::Render(error.to_string()))?;

    Ok(LayoutPreviewResponse {
        width: frame.width,
        height: frame.height,
        rgba: rgb_to_rgba(&frame.data),
        diagnostics: Vec::new(),
        topology: surface.preview,
        document_fingerprint,
    })
}

#[tauri::command]
pub fn save_layout_document(
    name: String,
    expected_fingerprint: Option<String>,
    draft: LayoutDocument,
    state: tauri::State<'_, RendererState>,
) -> Result<LayoutSaveResponse, AppError> {
    save_layout_document_impl(
        &state.layout_dir,
        &name,
        expected_fingerprint.as_deref(),
        &draft,
    )
}

fn save_layout_document_impl(
    layout_dir: &Path,
    name: &str,
    expected_fingerprint: Option<&str>,
    draft: &LayoutDocument,
) -> Result<LayoutSaveResponse, AppError> {
    reject_layout_media_paths(layout_dir, draft)?;
    let saved = thermalwriter::layout_engine::save_layout_document(
        layout_dir,
        name,
        expected_fingerprint,
        draft,
    )
    .map_err(|diagnostic| AppError::LayoutDiagnostics(vec![diagnostic]))?;
    Ok(LayoutSaveResponse {
        name: saved.name,
        path: saved.path,
        document_fingerprint: saved.fingerprint,
    })
}

#[tauri::command]
pub async fn apply_layout_document(
    name: String,
    expected_fingerprint: Option<String>,
    draft: LayoutDocument,
    state: tauri::State<'_, RendererState>,
) -> Result<LayoutApplyResponse, AppError> {
    let saved = save_layout_document_impl(
        &state.layout_dir,
        &name,
        expected_fingerprint.as_deref(),
        &draft,
    )?;
    let activation = activate_layout_document(&state.layout_dir, &saved.name).await;
    Ok(LayoutApplyResponse { saved, activation })
}

async fn activate_layout_document(layout_dir: &Path, name: &str) -> LayoutActivationState {
    let layout_name = format!("{name}.layout.toml");
    if let Err(error) = validate_layout_path(layout_dir, &layout_name) {
        return LayoutActivationState::ActivationFailed {
            reason: error.to_string(),
        };
    }

    let connection = match zbus::Connection::session().await {
        Ok(connection) => connection,
        Err(error) => {
            return LayoutActivationState::DaemonUnavailable {
                reason: format!("session bus unavailable: {error}"),
            };
        }
    };
    let proxy = match DisplayProxy::new(&connection).await {
        Ok(proxy) => proxy,
        Err(error) => {
            return LayoutActivationState::DaemonUnavailable {
                reason: format!("daemon proxy not reachable: {error}"),
            };
        }
    };
    if let Err(error) = proxy.set_layout(&layout_name).await {
        return LayoutActivationState::ActivationFailed {
            reason: format!("set_layout failed: {error}"),
        };
    }
    if let Err(error) = proxy.set_default_layout(&layout_name).await {
        return LayoutActivationState::ActiveButDefaultNotPersisted {
            reason: format!("set_default_layout failed after activation: {error}"),
        };
    }

    LayoutActivationState::Active
}

#[tauri::command]
pub async fn list_sensors() -> Result<Vec<SensorDescriptor>, AppError> {
    // Always measure live on the GUI side so setup sees real poll cost even
    // when the daemon catalog is stale or offline. Two polls: first warms
    // providers / discovery window; second carries attributed costs.
    let measured = tokio::task::spawn_blocking(|| {
        let mut hub = thermalwriter::sensor::SensorHub::with_default_providers("");
        let _ = hub.poll();
        let _ = hub.poll();
        hub.available_sensors()
    })
    .await
    .map_err(|e| AppError::Render(format!("sensor measure task failed: {e}")))?;

    let mut out: Vec<SensorDescriptor> = measured
        .into_iter()
        .map(|d| SensorDescriptor {
            key: d.key,
            name: d.name,
            unit: d.unit,
            cost_us: d.cost_us,
        })
        .collect();

    // Prefer non-empty local measure; if somehow empty, fall back to daemon
    // catalog (without relying on it for costs).
    if out.is_empty()
        && let Ok(connection) = zbus::Connection::session().await
        && let Ok(proxy) = DisplayProxy::new(&connection).await
        && let Ok(sensors) = proxy.list_sensors().await
        && !sensors.is_empty()
    {
        out = sensors
            .into_iter()
            .map(|(key, name, unit, cost_us)| SensorDescriptor {
                key,
                name,
                unit,
                cost_us,
            })
            .collect();
    }

    if out.is_empty() {
        out = fallback_sensors();
    }

    // Sort by cost descending so expensive sensors surface first in the picker.
    out.sort_by(|a, b| b.cost_us.cmp(&a.cost_us).then_with(|| a.key.cmp(&b.key)));
    Ok(out)
}

#[tauri::command]
pub fn render_preview(
    layout: String,
    vars: HashMap<String, String>,
    background: Option<String>,
    state: tauri::State<'_, RendererState>,
) -> Result<Response, AppError> {
    let path = validate_layout_path(&state.layout_dir, &layout)?;
    let content = std::fs::read_to_string(&path).map_err(|e| AppError::LayoutIo(e.to_string()))?;
    let frontmatter = LayoutFrontmatter::parse(&content);
    validate_vars(&frontmatter.variables, &vars)?;

    let config = Config::load(&state.config_path).map_err(|e| AppError::Config(e.to_string()))?;
    let theme = config
        .theme
        .resolve_palette()
        .map_err(|e| AppError::Config(e.to_string()))?;

    let mut cache = state.cache.lock().map_err(|_| AppError::StatePoisoned)?;
    let background_image = cached_preview_background(&state, &mut cache, background.as_deref())?;
    let is_svg = layout.ends_with(".svg");
    if cache.current_layout.as_deref() != Some(layout.as_str()) || cache.renderer.is_none() {
        let renderer = if is_svg {
            let mut renderer = SvgRenderer::new(&content, 480, 480)
                .map_err(|e| AppError::Render(e.to_string()))?;
            // Layouts that declare history metrics reference history arrays via
            // graph(data=...); Tera errors on the undefined arg if they're absent.
            // Seed synthetic history (same approach as the preview_layout example)
            // so the live preview matches what the daemon renders instead of failing.
            if !frontmatter.history_configs.is_empty() {
                let mut history = SensorHistory::new();
                for (metric, cfg) in &frontmatter.history_configs {
                    history.configure_metric(metric, cfg.duration);
                }
                let metrics: Vec<String> = frontmatter.history_configs.keys().cloned().collect();
                fill_synthetic_history(&mut history, &metrics, &mock_sensors());
                renderer.set_history(Arc::new(std::sync::Mutex::new(history)));
            }
            CachedRenderer::Svg(Box::new(renderer))
        } else {
            let renderer = TemplateRenderer::new(&content, 480, 480)
                .map_err(|e| AppError::Render(e.to_string()))?;
            CachedRenderer::Html(renderer)
        };
        cache.renderer = Some(renderer);
        cache.current_layout = Some(layout.clone());
    }
    let renderer = cache
        .renderer
        .as_mut()
        .ok_or_else(|| AppError::Render("renderer not initialized".into()))?;
    let frame = match renderer {
        CachedRenderer::Svg(renderer) => {
            renderer.set_theme(theme);
            renderer.set_layout_vars(vars);
            renderer
                .set_background(background_image)
                .map_err(|error| AppError::Render(error.to_string()))?;
            renderer
                .render(&mock_sensors())
                .map_err(|e| AppError::Render(e.to_string()))?
        }
        CachedRenderer::Html(renderer) => {
            renderer.set_layout_vars(vars);
            renderer
                .render(&mock_sensors())
                .map_err(|e| AppError::Render(e.to_string()))?
        }
    };
    Ok(Response::new(rgb_to_rgba(&frame.data)))
}

/// Fallback config write used only when the daemon is NOT running.
/// When the daemon is up, use `apply_to_daemon` — it routes all writes
/// through D-Bus so the daemon remains the sole writer of config.toml.
#[tauri::command]
pub fn save_config(
    layout: String,
    vars: HashMap<String, String>,
    state: tauri::State<'_, RendererState>,
) -> Result<(), AppError> {
    // Validate that the named layout exists and the vars are well-typed before
    // persisting — prevents writing junk to config.toml.
    let path = validate_layout_path(&state.layout_dir, &layout)?;
    let content = std::fs::read_to_string(&path).map_err(|e| AppError::LayoutIo(e.to_string()))?;
    let frontmatter = LayoutFrontmatter::parse(&content);
    validate_vars(&frontmatter.variables, &vars)?;

    let mode = if layout.ends_with(".html") {
        "html"
    } else {
        "svg"
    };
    Config::save_layout_vars(&state.config_path, &layout, &vars)
        .map_err(|e| AppError::ConfigWrite(e.to_string()))?;
    Config::save_display_layout(&state.config_path, &layout, mode)
        .map_err(|e| AppError::ConfigWrite(e.to_string()))?;
    Ok(())
}

#[tauri::command]
pub async fn apply_to_daemon(
    layout: String,
    vars: HashMap<String, String>,
    state: tauri::State<'_, RendererState>,
) -> Result<(), AppError> {
    // Validate before touching D-Bus — same guarantees as save_config.
    let path = validate_layout_path(&state.layout_dir, &layout)?;
    let content = std::fs::read_to_string(&path).map_err(|e| AppError::LayoutIo(e.to_string()))?;
    let frontmatter = LayoutFrontmatter::parse(&content);
    validate_vars(&frontmatter.variables, &vars)?;

    let connection =
        zbus::Connection::session()
            .await
            .map_err(|e| AppError::DaemonUnavailable {
                reason: format!("session bus unavailable: {e}"),
            })?;
    let proxy = DisplayProxy::new(&connection)
        .await
        .map_err(|e| AppError::DaemonUnavailable {
            reason: format!("daemon proxy not reachable: {e}"),
        })?;
    // Route layout vars through daemon — it owns the in-memory state the live
    // layout switch reads from.
    proxy
        .set_layout_vars(&layout, vars)
        .await
        .map_err(|e| AppError::DaemonCall(format!("set_layout_vars failed: {e}")))?;
    // Then switch the active daemon renderer and wait for its ack so get_status()
    // and the sidebar active-daemon highlight reflect the applied layout.
    proxy
        .set_layout(&layout)
        .await
        .map_err(|e| AppError::DaemonCall(format!("set_layout failed: {e}")))?;
    // Persist the new default layout through daemon so both config.toml writes
    proxy
        .set_default_layout(&layout)
        .await
        .map_err(|e| AppError::DaemonCall(format!("set_default_layout failed: {e}")))?;
    Ok(())
}

// ---- background commands ----

#[tauri::command]
pub fn list_backgrounds(state: tauri::State<'_, RendererState>) -> Result<Vec<String>, AppError> {
    Ok(list_background_names(&state.background_dir))
}

/// Read raw background image bytes for thumbnail rendering in the GUI.
/// Validates the path through `validate_background_path` so traversal/symlink
/// escapes are rejected before any disk read.
#[tauri::command]
pub fn read_background(
    name: String,
    state: tauri::State<'_, RendererState>,
) -> Result<Response, AppError> {
    let path = validate_background_path(&state.background_dir, &name)?;
    let bytes = std::fs::read(&path).map_err(|e| AppError::BackgroundIo(e.to_string()))?;
    Ok(Response::new(bytes))
}

#[tauri::command]
pub async fn set_background(
    name: Option<String>,
    state: tauri::State<'_, RendererState>,
) -> Result<(), AppError> {
    let connection =
        zbus::Connection::session()
            .await
            .map_err(|e| AppError::DaemonUnavailable {
                reason: format!("session bus unavailable: {e}"),
            })?;
    let proxy = DisplayProxy::new(&connection)
        .await
        .map_err(|e| AppError::DaemonUnavailable {
            reason: format!("daemon proxy not reachable: {e}"),
        })?;

    match name {
        None => proxy
            .clear_background()
            .await
            .map_err(|e| AppError::DaemonCall(format!("clear_background failed: {e}")))?,
        Some(ref n) => {
            // Validate path before touching D-Bus — prevents traversal even if
            // the daemon also validates, since the GUI is a trust boundary.
            validate_background_path(&state.background_dir, n)?;
            proxy
                .set_background(n)
                .await
                .map_err(|e| AppError::DaemonCall(format!("set_background failed: {e}")))?;
        }
    }
    Ok(())
}

#[tauri::command]
pub fn save_background(
    name: Option<String>,
    state: tauri::State<'_, RendererState>,
) -> Result<(), AppError> {
    if let Some(ref n) = name {
        validate_background_path(&state.background_dir, n)?;
    }
    Config::save_background_image(&state.config_path, name.as_deref())
        .map_err(|e| AppError::BackgroundIo(e.to_string()))
}

#[tauri::command]
pub fn get_active_background(
    state: tauri::State<'_, RendererState>,
) -> Result<Option<String>, AppError> {
    let config = Config::load(&state.config_path).map_err(|e| AppError::Config(e.to_string()))?;
    Ok(config.background.image)
}

/// Import raw image bytes (from a file the user picked in the GUI) into the
/// backgrounds directory so it appears in the gallery. Validates the extension
/// and that the bytes actually decode as an image, then writes atomically under
/// a non-clobbering filename. Returns the stored filename so the caller can
/// select it. The daemon resizes to 480×480 at SetBackground time, so the
/// original resolution is preserved on disk.
#[tauri::command]
pub fn import_background(
    filename: String,
    data: Vec<u8>,
    state: tauri::State<'_, RendererState>,
) -> Result<String, AppError> {
    import_background_impl(&state.background_dir, &filename, &data)
}

/// Suggest values for a layout's `color` vars from a background image's
/// dominant colors (Material You-style extraction + fixed HCT recipe in
/// `render::palette`). Returns a map of var name → `#rrggbb` covering only
/// the layout's color-typed vars; the frontend merges it into its live
/// `values` so the user can preview and tweak before applying.
#[tauri::command]
pub fn suggest_colors(
    layout: String,
    background: String,
    state: tauri::State<'_, RendererState>,
) -> Result<HashMap<String, String>, AppError> {
    let bg_path = validate_background_path(&state.background_dir, &background)?;
    let layout_path = validate_layout_path(&state.layout_dir, &layout)?;
    let content =
        std::fs::read_to_string(&layout_path).map_err(|e| AppError::LayoutIo(e.to_string()))?;
    let frontmatter = LayoutFrontmatter::parse(&content);

    let image =
        BackgroundImage::from_file(&bg_path).map_err(|e| AppError::BackgroundIo(e.to_string()))?;
    // Aspect-preserving thumbnail: when the target aspect equals the source
    // aspect, cover-crop degenerates to a pure resize, so the whole image
    // (not a center crop) informs the extraction.
    let (w, h) = image.source_dimensions();
    let scale = (128.0 / f64::from(w.max(h))).min(1.0);
    let tw = ((f64::from(w) * scale).round() as u32).max(1);
    let th = ((f64::from(h) * scale).round() as u32).max(1);
    let pixmap = image
        .to_pixmap(tw, th)
        .map_err(|e| AppError::BackgroundIo(e.to_string()))?;

    let scheme = palette::suggest_scheme(&pixmap);
    Ok(assign_scheme_to_vars(&frontmatter, &scheme))
}

/// Map suggested role colors onto a layout's color vars by name convention.
///
/// Known role patterns (checked in order, case-insensitive substring):
/// background/panel → panel_bg, text → text, dim/muted/label → dim,
/// primary/cpu → primary, secondary/gpu → secondary,
/// accent/tertiary/fps → tertiary. Color vars matching no pattern cycle
/// through the three accents in name order so every color var gets a value.
fn assign_scheme_to_vars(
    frontmatter: &LayoutFrontmatter,
    scheme: &SchemeSuggestion,
) -> HashMap<String, String> {
    let mut color_vars: Vec<&String> = frontmatter
        .variables
        .iter()
        .filter(|(_, decl)| decl.var_type == "color")
        .map(|(name, _)| name)
        .collect();
    color_vars.sort();

    let role_for = |name: &str| -> Option<&str> {
        let n = name.to_ascii_lowercase();
        if n.contains("background") || n.contains("panel") {
            Some(&scheme.panel_bg)
        } else if n.contains("text") {
            Some(&scheme.text)
        } else if n.contains("dim") || n.contains("muted") || n.contains("label") {
            Some(&scheme.dim)
        } else if n.contains("primary") || n.contains("cpu") {
            Some(&scheme.primary)
        } else if n.contains("secondary") || n.contains("gpu") {
            Some(&scheme.secondary)
        } else if n.contains("accent") || n.contains("tertiary") || n.contains("fps") {
            Some(&scheme.tertiary)
        } else {
            None
        }
    };

    let accents = [&scheme.primary, &scheme.secondary, &scheme.tertiary];
    let mut next_accent = 0usize;
    let mut out = HashMap::new();
    for name in color_vars {
        let hex = role_for(name).unwrap_or_else(|| {
            let hex = accents[next_accent % accents.len()];
            next_accent += 1;
            hex
        });
        out.insert(name.clone(), hex.to_string());
    }
    out
}

// ---- daemon status ----

/// Snapshot of the daemon's runtime state, returned by `get_status`.
///
/// Field names match the keys the daemon inserts in its `GetStatus` D-Bus
/// response so nothing is silently dropped when the raw HashMap is parsed.
/// The frontend can treat `.mode == "xvfb"` as the streaming-active signal.
#[derive(Debug, serde::Serialize)]
pub struct DaemonStatus {
    /// Current display mode: "svg", "html", or "xvfb".
    pub mode: String,
    /// Current tick rate in FPS (may be the streaming rate while in xvfb mode).
    pub tick_rate: u32,
    /// Whether the USB display is connected.
    pub connected: bool,
    /// Active layout filename (e.g. "svg/neon-dash-v2.svg"). Empty in xvfb mode.
    pub active_layout: String,
    /// Display resolution as "WxH" string (e.g. "480x480").
    pub resolution: String,
}

/// Query the daemon for its current runtime status. Returns a typed struct so
/// the frontend doesn't need to parse string fields from the raw D-Bus map.
///
/// Returns `DaemonUnavailable` if the daemon is not running — callers should
/// treat that as "offline / no status available" rather than an error to show.
#[tauri::command]
pub async fn get_status() -> Result<DaemonStatus, AppError> {
    let connection =
        zbus::Connection::session()
            .await
            .map_err(|e| AppError::DaemonUnavailable {
                reason: format!("session bus unavailable: {e}"),
            })?;
    let proxy = DisplayProxy::new(&connection)
        .await
        .map_err(|e| AppError::DaemonUnavailable {
            reason: format!("daemon proxy not reachable: {e}"),
        })?;
    let raw = proxy
        .get_status()
        .await
        .map_err(|e| AppError::DaemonCall(format!("get_status failed: {e}")))?;

    Ok(DaemonStatus {
        mode: raw.get("mode").cloned().unwrap_or_default(),
        tick_rate: raw
            .get("tick_rate")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        connected: raw.get("connected").map(|v| v == "true").unwrap_or(false),
        active_layout: raw.get("active_layout").cloned().unwrap_or_default(),
        resolution: raw.get("resolution").cloned().unwrap_or_default(),
    })
}

// ---- stream commands ----

/// Start streaming: send a fully-formed argv to the daemon's generic
/// `set_mode_argv` method.  The GUI builds the argv from its preset registry
/// (resolved binary paths, custom config paths, terminal wrappers) — the daemon
/// just executes it inside a Xvfb virtual display.
///
/// Mirrors the `apply_to_daemon` pattern: connect to D-Bus, call the proxy.
#[tauri::command]
pub async fn apply_stream(argv: Vec<String>) -> Result<(), AppError> {
    if argv.is_empty() {
        return Err(AppError::DaemonCall(
            "apply_stream: argv must not be empty".to_string(),
        ));
    }
    let connection =
        zbus::Connection::session()
            .await
            .map_err(|e| AppError::DaemonUnavailable {
                reason: format!("session bus unavailable: {e}"),
            })?;
    let proxy = DisplayProxy::new(&connection)
        .await
        .map_err(|e| AppError::DaemonUnavailable {
            reason: format!("daemon proxy not reachable: {e}"),
        })?;
    proxy
        .set_mode_argv(argv)
        .await
        .map_err(|e| AppError::DaemonCall(format!("set_mode_argv failed: {e}")))?;
    Ok(())
}

/// Stop streaming: return to a layout by calling `set_mode("svg"|"html", layout)`.
/// The daemon restores tick_rate and kills the xvfb child automatically.
#[tauri::command]
pub async fn stop_stream(layout: String) -> Result<(), AppError> {
    let mode = if layout.ends_with(".html") {
        "html"
    } else {
        "svg"
    };
    let connection =
        zbus::Connection::session()
            .await
            .map_err(|e| AppError::DaemonUnavailable {
                reason: format!("session bus unavailable: {e}"),
            })?;
    let proxy = DisplayProxy::new(&connection)
        .await
        .map_err(|e| AppError::DaemonUnavailable {
            reason: format!("daemon proxy not reachable: {e}"),
        })?;
    proxy
        .set_mode(mode, &layout)
        .await
        .map_err(|e| AppError::DaemonCall(format!("set_mode failed: {e}")))?;
    Ok(())
}

/// Read the last JPEG frame written by the daemon's tick loop for the GUI preview.
///
/// Path: `$XDG_RUNTIME_DIR/thermalwriter/last.jpg`. There is no `/tmp` fallback:
/// streamed frames can expose private window contents. Returns raw JPEG bytes;
/// the frontend wraps them in a Blob URL for display.
///
/// Returns `AppError::NoFrame` — not a panic — if no frame has been written yet
/// (stream not started, or file was cleared when mode left xvfb).
#[tauri::command]
pub fn read_frame() -> Result<Response, AppError> {
    let dir = frame_dir()?;
    let bytes = read_frame_impl(&dir)?;
    Ok(Response::new(bytes))
}

/// Set the daemon tick rate (1–60 FPS). Validates range locally before calling
/// D-Bus so the GUI gets a fast, descriptive error without a round-trip.
///
/// Intended use: the Stream tab calls `apply_stream(argv)` then
/// `set_tick_rate(fps)` to start streaming at the desired frame rate. The
/// daemon's existing restore_from_streaming path resets the rate on stop.
#[tauri::command]
pub async fn set_tick_rate(rate: u32) -> Result<(), AppError> {
    validate_tick_rate(rate)?;
    let connection =
        zbus::Connection::session()
            .await
            .map_err(|e| AppError::DaemonUnavailable {
                reason: format!("session bus unavailable: {e}"),
            })?;
    let proxy = DisplayProxy::new(&connection)
        .await
        .map_err(|e| AppError::DaemonUnavailable {
            reason: format!("daemon proxy not reachable: {e}"),
        })?;
    proxy
        .set_tick_rate(rate)
        .await
        .map_err(|e| AppError::DaemonCall(format!("set_tick_rate failed: {e}")))?;
    Ok(())
}

/// Resolve binary names to absolute paths using the daemon's PATH.
/// Delegates to `DisplayProxy::resolve_binaries` so the GUI can detect which
/// preset binaries are installed before offering them.
#[tauri::command]
pub async fn resolve_binaries(names: Vec<String>) -> Result<HashMap<String, String>, AppError> {
    let connection =
        zbus::Connection::session()
            .await
            .map_err(|e| AppError::DaemonUnavailable {
                reason: format!("session bus unavailable: {e}"),
            })?;
    let proxy = DisplayProxy::new(&connection)
        .await
        .map_err(|e| AppError::DaemonUnavailable {
            reason: format!("daemon proxy not reachable: {e}"),
        })?;
    proxy
        .resolve_binaries(names)
        .await
        .map_err(|e| AppError::DaemonCall(format!("resolve_binaries failed: {e}")))
}

// ---- helpers ----

/// Validate that `rate` is within the daemon's accepted range [1, 60].
/// Extracted so tests can call it without a running D-Bus session.
fn validate_tick_rate(rate: u32) -> Result<(), AppError> {
    if rate == 0 || rate > 60 {
        return Err(AppError::DaemonCall(format!(
            "tick rate {rate} out of range — must be 1–60"
        )));
    }
    Ok(())
}

/// Resolve a registered native surface profile without inferring topology from dimensions.
fn resolve_preview_surface(
    profile: SurfaceProfileId,
    width: u32,
    height: u32,
) -> Result<thermalwriter::layout_engine::DisplaySurfaceProfile, AppError> {
    resolve_surface_profile(width, height, profile)
        .copied()
        .ok_or_else(|| {
            AppError::InvalidLayoutProfile(format!(
                "profile {profile} is not registered for {width}x{height}"
            ))
        })
}

fn parse_layout_document(path: Option<&Path>, input: &str) -> Result<LayoutDocument, AppError> {
    LayoutDocument::from_toml(input).map_err(|error| layout_document_error(path, input, error))
}

fn layout_document_response(document: LayoutDocument) -> Result<LayoutDocumentResponse, AppError> {
    let document_fingerprint = document_fingerprint(&document)?;
    Ok(LayoutDocumentResponse {
        document,
        document_fingerprint,
    })
}

fn layout_document_error(path: Option<&Path>, input: &str, error: LayoutDocumentError) -> AppError {
    let file = path.map(Path::to_path_buf);
    let diagnostic = match error {
        LayoutDocumentError::Parse(error) => LayoutDiagnostic::from_toml_error(&error, input, file),
        LayoutDocumentError::Serialize(error) => {
            let mut diagnostic = LayoutDiagnostic::new(
                TOML_PARSE_CODE,
                DiagnosticSeverity::Error,
                "Invalid layout document TOML",
                error.to_string(),
                "Correct the layout document, then validate it again before saving.",
            );
            diagnostic.file = file;
            diagnostic
        }
        LayoutDocumentError::UnsupportedVersion(version) => {
            let mut diagnostic = LayoutDiagnostic::new(
                TOML_PARSE_CODE,
                DiagnosticSeverity::Error,
                "Unsupported layout document version",
                format!("version {version} is not supported"),
                format!(
                    "Use layout document version {} before saving.",
                    thermalwriter::layout_engine::CURRENT_VERSION
                ),
            );
            diagnostic.file = file;
            diagnostic
        }
    };
    AppError::LayoutDiagnostics(vec![diagnostic])
}

fn document_fingerprint(document: &LayoutDocument) -> Result<String, AppError> {
    let content = document
        .to_toml()
        .map_err(|error| layout_document_error(None, "", error))?;
    Ok(fingerprint_bytes(content.as_bytes()))
}

fn fingerprint_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn media_fingerprint(document: &LayoutDocument, media_root: &Path) -> String {
    let mut hasher = Sha256::new();
    for module in &document.modules {
        let ModuleDocument::Media(media) = module else {
            continue;
        };
        let source = if media.source.as_os_str().is_empty() {
            PathBuf::from(media.binding.trim())
        } else {
            media.source.clone()
        };
        let source_display = source.to_string_lossy();
        hasher.update(source_display.as_bytes());
        hasher.update([0]);

        let bytes = source
            .to_str()
            .and_then(|name| validate_path_within_dir(media_root, name, "Media").ok())
            .and_then(|path| std::fs::read(path).ok());
        if let Some(bytes) = bytes {
            hasher.update(bytes);
        } else {
            hasher.update(b"<unreadable-media>");
        }
        hasher.update([0xff]);
    }
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn reject_layout_media_paths(layout_dir: &Path, document: &LayoutDocument) -> Result<(), AppError> {
    let diagnostics = layout_media_path_diagnostics(layout_dir, document);
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(AppError::LayoutDiagnostics(diagnostics))
    }
}

fn layout_media_path_diagnostics(
    layout_dir: &Path,
    document: &LayoutDocument,
) -> Vec<LayoutDiagnostic> {
    let mut diagnostics = Vec::new();
    for module in &document.modules {
        let ModuleDocument::Media(media) = module else {
            continue;
        };
        let source = if media.source.as_os_str().is_empty() {
            PathBuf::from(media.binding.trim())
        } else {
            media.source.clone()
        };
        let Some(source_name) = source.to_str() else {
            let mut diagnostic = LayoutDiagnostic::new(
                PERSISTENCE_PATH_CODE,
                DiagnosticSeverity::Error,
                "Media source is outside the layout root",
                "the media source path is not valid UTF-8",
                "Choose a relative media filename below the layout directory.",
            );
            diagnostic.module_id = Some(media.id.clone());
            diagnostic.property_path = Some("source".into());
            diagnostics.push(diagnostic);
            continue;
        };
        let Err(error) = validate_path_within_dir(layout_dir, source_name, "Media") else {
            continue;
        };
        if matches!(error, PathContainmentError::NotFound { .. }) {
            continue;
        }
        let mut diagnostic = LayoutDiagnostic::new(
            PERSISTENCE_PATH_CODE,
            DiagnosticSeverity::Error,
            "Media source is outside the layout root",
            error.to_string(),
            "Choose a relative media filename below the layout directory.",
        );
        diagnostic.module_id = Some(media.id.clone());
        diagnostic.property_path = Some("source".into());
        diagnostics.push(diagnostic);
    }
    diagnostics
}

/// Return the private directory where the daemon writes the last-frame JPEG.
///
/// Mirrors `thermalwriter::service::frame_dump::frame_dir()` — the GUI builds
/// with `default-features = false` (no `daemon` feature), so the daemon's
/// `service` module is not available here; the path logic is replicated inline.
/// Path contract: `$XDG_RUNTIME_DIR/thermalwriter`; no `/tmp` fallback because
/// streamed frames can expose private window contents.
fn frame_dir() -> Result<PathBuf, AppError> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").map_err(|e| {
        AppError::NoFrame(format!(
            "XDG_RUNTIME_DIR unavailable; refusing to read shared /tmp fallback: {e}"
        ))
    })?;
    Ok(PathBuf::from(runtime).join("thermalwriter"))
}

/// Core of `read_frame`, factored out so it can be tested without a Tauri runtime.
/// Reads `dir/last.jpg`; returns `AppError::NoFrame` if the file is absent.
fn read_frame_impl(dir: &Path) -> Result<Vec<u8>, AppError> {
    let path = dir.join("last.jpg");
    std::fs::read(&path).map_err(|e| AppError::NoFrame(format!("{}: {e}", path.display())))
}

fn list_layout_names(layout_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(top) = std::fs::read_dir(layout_dir) else {
        return out;
    };
    for entry in top.flatten() {
        let path = entry.path();
        if path.is_file() && has_layout_ext(&path) {
            if let Some(name) = path.file_name() {
                out.push(name.to_string_lossy().to_string());
            }
        } else if path.is_dir() {
            let Ok(sub) = std::fs::read_dir(&path) else {
                continue;
            };
            for sub_entry in sub.flatten() {
                let sub_path = sub_entry.path();
                if sub_path.is_file()
                    && has_layout_ext(&sub_path)
                    && let Ok(rel) = sub_path.strip_prefix(layout_dir)
                {
                    let s = rel
                        .components()
                        .filter_map(|c| match c {
                            std::path::Component::Normal(os) => {
                                Some(os.to_string_lossy().into_owned())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("/");
                    if !s.is_empty() {
                        out.push(s);
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn has_layout_ext(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".layout.toml"))
        || matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("html") | Some("svg")
        )
}

fn list_background_names(bg_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(bg_dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.is_file() && has_background_ext(&p) {
                p.file_name().map(|n| n.to_string_lossy().into_owned())
            } else {
                None
            }
        })
        .collect();
    names.sort();
    names
}

fn has_background_ext(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("png") | Some("jpg") | Some("jpeg")
    )
}

/// Max byte size accepted for an imported background. Matches the daemon's own
/// 8 MB file-size pre-check in `render::background`.
const MAX_IMPORT_BYTES: usize = 8 * 1024 * 1024;

/// Core of `import_background`, factored out so it can be unit-tested without a
/// Tauri runtime/`State`.
fn import_background_impl(bg_dir: &Path, filename: &str, data: &[u8]) -> Result<String, AppError> {
    if data.len() > MAX_IMPORT_BYTES {
        return Err(AppError::BackgroundIo(format!(
            "image too large: {} bytes (max {MAX_IMPORT_BYTES})",
            data.len()
        )));
    }

    // Keep only the final path component — strips any directory the browser may
    // have included and rejects names with no real filename (e.g. "..").
    let base = Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| AppError::InvalidBackground(filename.to_string()))?;
    if !has_background_ext(Path::new(base)) {
        return Err(AppError::InvalidBackground(format!(
            "{base}: unsupported extension (use .png, .jpg, or .jpeg)"
        )));
    }

    // Validate the bytes really are a decodable image. Reuses the daemon's
    // decoder, which also enforces dimension/allocation limits — so we never
    // write a corrupt or maliciously-oversized file into the gallery.
    thermalwriter::render::background::decode_to_pixmap(data)
        .map_err(|e| AppError::BackgroundIo(format!("{base}: not a valid image ({e})")))?;

    std::fs::create_dir_all(bg_dir).map_err(|e| AppError::BackgroundIo(e.to_string()))?;

    // Reserve the destination with create_new so concurrent imports cannot
    // clobber an existing file via rename-replace. Retry under dedupe if a
    // race loses the exclusive create.
    for _ in 0..10_000 {
        let stored = dedupe_background_name(bg_dir, base);
        let dest = bg_dir.join(&stored);
        match write_new_file(&dest, data) {
            Ok(()) => return Ok(stored),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(AppError::BackgroundIo(e.to_string())),
        }
    }
    Err(AppError::BackgroundIo(format!(
        "could not reserve a unique name for {base}"
    )))
}

/// Write `data` only if `path` does not already exist (`create_new`).
fn write_new_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}

/// Resolve a non-clobbering filename within `bg_dir`. If `name` is free it is
/// returned as-is; otherwise a `-1`, `-2`, … suffix is appended to the stem.
fn dedupe_background_name(bg_dir: &Path, name: &str) -> String {
    if !bg_dir.join(name).exists() {
        return name.to_string();
    }
    let path = Path::new(name);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("png");
    for i in 1..10_000 {
        let candidate = format!("{stem}-{i}.{ext}");
        if !bg_dir.join(&candidate).exists() {
            return candidate;
        }
    }
    format!("{stem}-import.{ext}")
}

fn map_background_path_error(err: PathContainmentError) -> AppError {
    match err {
        PathContainmentError::NotFound { name, .. } => AppError::BackgroundNotFound(name),
        PathContainmentError::BaseInaccessible { source, .. } => {
            AppError::InvalidBackground(format!("background dir not accessible: {source}"))
        }
        PathContainmentError::Escapes { name, .. } => {
            AppError::InvalidBackground(format!("{name} escapes background directory"))
        }
        PathContainmentError::Absolute { name, .. }
        | PathContainmentError::ParentDir { name, .. } => AppError::InvalidBackground(name),
    }
}

/// Resolve a background filename relative to the background directory, rejecting
/// any path that escapes via `..`, absolute paths, or symlinks pointing outside.
fn validate_background_path(bg_dir: &Path, name: &str) -> Result<PathBuf, AppError> {
    validate_path_within_dir(bg_dir, name, "Background").map_err(map_background_path_error)
}

fn map_layout_path_error(err: PathContainmentError) -> AppError {
    match err {
        PathContainmentError::NotFound { name, .. } => AppError::LayoutNotFound(name),
        PathContainmentError::BaseInaccessible { source, .. } => {
            AppError::InvalidLayout(format!("layout dir not accessible: {source}"))
        }
        PathContainmentError::Escapes { name, .. } => {
            AppError::InvalidLayout(format!("{name} escapes layout directory"))
        }
        PathContainmentError::Absolute { name, .. }
        | PathContainmentError::ParentDir { name, .. } => AppError::InvalidLayout(name),
    }
}

/// Resolve a layout name relative to the layout directory, rejecting any path
/// that escapes the layout directory (via `..`, absolute paths, or symlinks
/// pointing outside). Uses `canonicalize` + `starts_with` per the security
/// requirement — never relies on textual `..` checks alone.
fn validate_layout_path(layout_dir: &Path, name: &str) -> Result<PathBuf, AppError> {
    validate_path_within_dir(layout_dir, name, "Layout").map_err(map_layout_path_error)
}

fn validate_vars(
    declarations: &HashMap<String, FrontmatterVar>,
    vars: &HashMap<String, String>,
) -> Result<(), AppError> {
    validate_layout_vars(declarations, vars)
        .map_err(|LayoutVarError(msg)| AppError::InvalidVariable(msg))
}

fn cached_preview_background(
    state: &RendererState,
    cache: &mut RendererCache,
    background: Option<&str>,
) -> Result<Option<Arc<BackgroundImage>>, AppError> {
    let Some(name) = background else {
        cache.current_background = None;
        cache.background_pixmap = None;
        return Ok(None);
    };

    let path = validate_background_path(&state.background_dir, name)?;
    if cache.current_background.as_deref() != Some(name) || cache.background_pixmap.is_none() {
        let image = BackgroundImage::from_file(&path)
            .map(Arc::new)
            .map_err(|error| AppError::BackgroundIo(error.to_string()))?;
        cache.current_background = Some(name.to_string());
        cache.background_pixmap = Some(image);
    }

    Ok(cache.background_pixmap.clone())
}

/// Pre-fill `history` with a deterministic sinusoidal wave per metric so the
/// GUI live-preview can render history-graph layouts (e.g. neon-dash-v2) without
/// a running daemon. Mirrors `examples/preview_layout.rs::fill_synthetic_history`.
fn fill_synthetic_history(
    history: &mut SensorHistory,
    metrics: &[String],
    sensor_data: &SensorData,
) {
    const SAMPLE_COUNT: usize = 60;
    for metric in metrics {
        let base: f64 = sensor_data
            .get(metric)
            .and_then(|v| v.parse().ok())
            .unwrap_or(50.0);
        for i in 0..SAMPLE_COUNT {
            let phase = (i as f64 / SAMPLE_COUNT as f64) * std::f64::consts::TAU;
            let value = (base + base * 0.2 * phase.sin()).max(0.0);
            let mut data = HashMap::new();
            data.insert(metric.clone(), format!("{value:.1}"));
            history.record(&data);
        }
    }
}

fn mock_layout_sensors() -> SensorData {
    let mut sensors = mock_sensors();
    sensors.insert(
        "cpu_temp_history".to_string(),
        "[52, 55, 58, 61, 64, 62]".to_string(),
    );
    sensors
}

fn mock_sensors() -> SensorData {
    HashMap::from([
        ("cpu_temp".to_string(), "62".to_string()),
        ("cpu_util".to_string(), "48".to_string()),
        ("cpu_power".to_string(), "82".to_string()),
        ("gpu_temp".to_string(), "67".to_string()),
        ("gpu_util".to_string(), "73".to_string()),
        ("gpu_power".to_string(), "218".to_string()),
        ("ram_used".to_string(), "18".to_string()),
        ("ram_total".to_string(), "64".to_string()),
        ("vram_used".to_string(), "9".to_string()),
        ("vram_total".to_string(), "16".to_string()),
        ("fps".to_string(), "144".to_string()),
        ("frametime".to_string(), "6.9".to_string()),
        ("net_rx".to_string(), "125".to_string()),
        ("net_tx".to_string(), "42".to_string()),
    ])
}

fn fallback_sensors() -> Vec<SensorDescriptor> {
    vec![
        SensorDescriptor {
            key: "cpu_temp".into(),
            name: "CPU Temperature".into(),
            unit: "°C".into(),
            cost_us: 0,
        },
        SensorDescriptor {
            key: "cpu_util".into(),
            name: "CPU Utilization".into(),
            unit: "%".into(),
            cost_us: 0,
        },
        SensorDescriptor {
            key: "cpu_power".into(),
            name: "CPU Power".into(),
            unit: "W".into(),
            cost_us: 0,
        },
        SensorDescriptor {
            key: "gpu_temp".into(),
            name: "GPU Temperature".into(),
            unit: "°C".into(),
            cost_us: 0,
        },
        SensorDescriptor {
            key: "gpu_util".into(),
            name: "GPU Utilization".into(),
            unit: "%".into(),
            cost_us: 0,
        },
        SensorDescriptor {
            key: "gpu_power".into(),
            name: "GPU Power".into(),
            unit: "W".into(),
            cost_us: 0,
        },
        SensorDescriptor {
            key: "ram_used".into(),
            name: "RAM Used".into(),
            unit: "GB".into(),
            cost_us: 0,
        },
        SensorDescriptor {
            key: "vram_used".into(),
            name: "VRAM Used".into(),
            unit: "GB".into(),
            cost_us: 0,
        },
        SensorDescriptor {
            key: "fps".into(),
            name: "FPS".into(),
            unit: "fps".into(),
            cost_us: 0,
        },
    ]
}

/// Convert a straight-RGB buffer (3 bytes/pixel) into straight RGBA (4 bytes/pixel,
/// alpha=255). The input comes from `RawFrame::from_pixmap`, which already
/// un-premultiplies the alpha — so the caller can hand the result straight to
/// `putImageData` without color washout. NEVER feed `pixmap.data()` here, since
/// that buffer is premultiplied RGBA.
pub(crate) fn rgb_to_rgba(rgb: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((rgb.len() / 3) * 4);
    for chunk in rgb.chunks_exact(3) {
        rgba.extend_from_slice(chunk);
        rgba.push(255);
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;
    use thermalwriter::render::FrameSource;
    use thermalwriter::validation::{contains_template_syntax, is_valid_color};
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const SIMPLE_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="480" height="480" viewBox="0 0 480 480">
<rect width="480" height="480" fill="#101820"/>
<text x="240" y="240" fill="#ffffff" font-size="32" text-anchor="middle">{{ cpu_temp }}</text>
</svg>
"##;

    fn make_state(tmp: &TempDir) -> RendererState {
        let layout_dir = tmp.path().join("layouts");
        let svg_dir = layout_dir.join("svg");
        fs::create_dir_all(&svg_dir).unwrap();
        fs::write(svg_dir.join("simple.svg"), SIMPLE_SVG).unwrap();
        let background_dir = tmp.path().join("backgrounds");
        fs::create_dir_all(&background_dir).unwrap();
        let config_path = tmp.path().join("config.toml");
        RendererState::new(layout_dir, background_dir, config_path)
    }

    fn lock_cache(state: &RendererState) -> std::sync::MutexGuard<'_, RendererCache> {
        match state.cache.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    // ---- color scheme suggestion ----

    fn test_scheme() -> SchemeSuggestion {
        SchemeSuggestion {
            primary: "#111111".into(),
            secondary: "#222222".into(),
            tertiary: "#333333".into(),
            text: "#444444".into(),
            dim: "#555555".into(),
            panel_bg: "#666666".into(),
        }
    }

    #[test]
    fn assign_scheme_maps_theme_style_vars() {
        // neon-dash family naming
        let fm = LayoutFrontmatter::parse(
            "{# vars:\n\
             theme_primary: color = \"#e94560\" \"CPU accent\"\n\
             theme_secondary: color = \"#53d8fb\" \"GPU accent\"\n\
             theme_accent: color = \"#20f5d8\" \"Bottom accent\"\n\
             theme_background: color = \"#08080f\" \"Background\"\n\
             cpu_label: text = \"CPU\" \"label\"\n\
             #}",
        );
        let out = assign_scheme_to_vars(&fm, &test_scheme());
        assert_eq!(out.get("theme_primary").unwrap(), "#111111");
        assert_eq!(out.get("theme_secondary").unwrap(), "#222222");
        assert_eq!(out.get("theme_accent").unwrap(), "#333333");
        assert_eq!(out.get("theme_background").unwrap(), "#666666");
        // non-color vars must not be touched
        assert!(!out.contains_key("cpu_label"));
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn assign_scheme_maps_sensor_style_vars() {
        // arc-gauge / cyber-grid family naming
        let fm = LayoutFrontmatter::parse(
            "{# vars:\n\
             cpu_color: color = \"#f7768e\" \"CPU\"\n\
             gpu_color: color = \"#7aa2f7\" \"GPU\"\n\
             fps_color: color = \"#9ece6a\" \"FPS\"\n\
             text_color: color = \"#c0caf5\" \"Text\"\n\
             dim_color: color = \"#565f89\" \"Labels\"\n\
             #}",
        );
        let out = assign_scheme_to_vars(&fm, &test_scheme());
        assert_eq!(out.get("cpu_color").unwrap(), "#111111");
        assert_eq!(out.get("gpu_color").unwrap(), "#222222");
        assert_eq!(out.get("fps_color").unwrap(), "#333333");
        assert_eq!(out.get("text_color").unwrap(), "#444444");
        assert_eq!(out.get("dim_color").unwrap(), "#555555");
    }

    #[test]
    fn assign_scheme_cycles_accents_for_unknown_names() {
        let fm = LayoutFrontmatter::parse(
            "{# vars:\n\
             alpha: color = \"#000000\" \"a\"\n\
             beta: color = \"#000000\" \"b\"\n\
             gamma: color = \"#000000\" \"c\"\n\
             delta: color = \"#000000\" \"d\"\n\
             #}",
        );
        let out = assign_scheme_to_vars(&fm, &test_scheme());
        // name-sorted: alpha, beta, delta, gamma — accents cycle P, S, T, P
        assert_eq!(out.get("alpha").unwrap(), "#111111");
        assert_eq!(out.get("beta").unwrap(), "#222222");
        assert_eq!(out.get("delta").unwrap(), "#333333");
        assert_eq!(out.get("gamma").unwrap(), "#111111");
    }

    #[test]
    fn suggest_colors_layout_without_color_vars_returns_empty() {
        let fm = LayoutFrontmatter::parse("{# vars:\nlabel: text = \"x\" \"label\"\n#}");
        assert!(assign_scheme_to_vars(&fm, &test_scheme()).is_empty());
    }

    // ---- pixel format / byte count ----

    #[test]
    fn rgb_to_rgba_doubles_length_correctly() {
        // 480x480x3 = 691200 bytes RGB → 480x480x4 = 921600 bytes RGBA
        let rgb = vec![0u8; 480 * 480 * 3];
        let rgba = rgb_to_rgba(&rgb);
        assert_eq!(rgba.len(), 480 * 480 * 4);
        assert_eq!(rgba.len(), 921_600);
    }

    #[test]
    fn rgb_to_rgba_appends_full_alpha() {
        let rgb = vec![10, 20, 30, 40, 50, 60];
        let rgba = rgb_to_rgba(&rgb);
        assert_eq!(rgba, vec![10, 20, 30, 255, 40, 50, 60, 255]);
    }

    // A graph layout that references a history array, like neon-dash-v2. Tera
    // errors on graph(data=undef) unless history is injected.
    const HISTORY_SVG: &str = r##"{# history: cpu_temp=60s #}
<svg xmlns="http://www.w3.org/2000/svg" width="480" height="480" viewBox="0 0 480 480">
<rect width="480" height="480" fill="#101820"/>
{{ graph(data=cpu_temp_history, x=16, y=100, w=448, h=88, style="area", fill="#53d8fb22", stroke="#53d8fb", stroke_width=1) }}
</svg>
"##;

    #[test]
    fn render_without_history_fails_for_graph_layout() {
        // Regression guard documenting the pre-existing failure that history
        // seeding fixes: a graph layout rendered with no history errors in Tera.
        let mut renderer = SvgRenderer::new(HISTORY_SVG, 480, 480).unwrap();
        assert!(
            renderer.render(&mock_sensors()).is_err(),
            "graph(data=undef) must error without seeded history"
        );
    }

    #[test]
    fn render_with_seeded_history_succeeds_for_graph_layout() {
        let fm = LayoutFrontmatter::parse(HISTORY_SVG);
        assert!(
            !fm.history_configs.is_empty(),
            "frontmatter declares history"
        );
        let mut history = SensorHistory::new();
        for (metric, cfg) in &fm.history_configs {
            history.configure_metric(metric, cfg.duration);
        }
        let metrics: Vec<String> = fm.history_configs.keys().cloned().collect();
        fill_synthetic_history(&mut history, &metrics, &mock_sensors());

        let mut renderer = SvgRenderer::new(HISTORY_SVG, 480, 480).unwrap();
        renderer.set_history(Arc::new(std::sync::Mutex::new(history)));
        let frame = renderer
            .render(&mock_sensors())
            .expect("graph layout must render once history is seeded");
        assert_eq!(frame.data.len(), 480 * 480 * 3);
    }

    #[test]
    fn html_layout_preview_builds_template_renderer_not_svg() {
        let html = r#"<div style="width: 480px; height: 480px; background: #112233;"></div>"#;
        // Dispatch by extension: .html -> TemplateRenderer. Construction + render
        // must succeed for HTML (the old path always built SvgRenderer).
        let mut renderer = TemplateRenderer::new(html, 480, 480).expect("html preview renderer");
        let frame = renderer
            .render(&SensorData::new())
            .expect("html preview must render");
        assert_eq!(frame.width, 480);
        assert_eq!(frame.height, 480);
        assert_eq!(frame.data.len(), 480 * 480 * 3);
        // Center pixel should be the declared fill (straight RGB).
        let offset = ((240 * 480 + 240) * 3) as usize;
        assert_eq!(&frame.data[offset..offset + 3], &[0x11, 0x22, 0x33]);
    }

    #[test]
    fn render_preview_helper_produces_921600_bytes() {
        // Drive the same pipeline render_preview uses — but call into it via
        // the helpers we can construct in tests (no Tauri runtime available).
        let mut renderer = SvgRenderer::new(SIMPLE_SVG, 480, 480).expect("renderer");
        let frame = renderer.render(&mock_sensors()).expect("render");
        let bytes = rgb_to_rgba(&frame.data);
        assert_eq!(
            bytes.len(),
            480 * 480 * 4,
            "render_preview output must be exactly 480*480*4 = 921600 bytes"
        );
    }

    // ---- path traversal ----

    #[test]
    fn validate_layout_path_rejects_parent_dir_traversal() {
        let tmp = TempDir::new().unwrap();
        let layout_dir = tmp.path().join("layouts");
        fs::create_dir_all(&layout_dir).unwrap();
        fs::write(layout_dir.join("ok.svg"), SIMPLE_SVG).unwrap();

        let outside = tmp.path().join("outside.svg");
        fs::write(&outside, SIMPLE_SVG).unwrap();

        let err = validate_layout_path(&layout_dir, "../outside.svg").expect_err("must reject ..");
        assert!(matches!(err, AppError::InvalidLayout(_)), "got {err:?}");
    }

    #[test]
    fn validate_layout_path_rejects_absolute_paths() {
        let tmp = TempDir::new().unwrap();
        let layout_dir = tmp.path().join("layouts");
        fs::create_dir_all(&layout_dir).unwrap();
        let err = validate_layout_path(&layout_dir, "/etc/passwd")
            .expect_err("must reject absolute path");
        assert!(matches!(err, AppError::InvalidLayout(_)), "got {err:?}");
    }

    #[test]
    fn validate_layout_path_rejects_symlink_escape() {
        // canonicalize() resolves symlinks, so a symlink pointing outside the
        // layout dir is caught by the starts_with() check.
        let tmp = TempDir::new().unwrap();
        let layout_dir = tmp.path().join("layouts");
        fs::create_dir_all(&layout_dir).unwrap();
        let outside = tmp.path().join("outside.svg");
        fs::write(&outside, SIMPLE_SVG).unwrap();
        // Build a symlink layouts/escape.svg → ../outside.svg
        let link = layout_dir.join("escape.svg");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let err = validate_layout_path(&layout_dir, "escape.svg")
            .expect_err("symlink escape must be rejected");
        assert!(matches!(err, AppError::InvalidLayout(_)), "got {err:?}");
    }

    #[test]
    fn validate_layout_path_accepts_legit_subdir() {
        let tmp = TempDir::new().unwrap();
        let state = make_state(&tmp);
        let resolved = validate_layout_path(&state.layout_dir, "svg/simple.svg")
            .expect("legit nested layout must resolve");
        assert!(resolved.ends_with("svg/simple.svg"));
    }

    // ---- renderer cache ----

    #[test]
    fn renderer_cache_reuses_renderer_for_same_layout() {
        let tmp = TempDir::new().unwrap();
        let state = make_state(&tmp);

        // First render builds the renderer; second render with same layout must reuse.
        // We inspect cache state directly (no Tauri runtime needed).
        let layout = "svg/simple.svg".to_string();
        let path = validate_layout_path(&state.layout_dir, &layout).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();

        {
            let mut cache = lock_cache(&state);
            assert!(cache.renderer.is_none(), "cache empty initially");
            let r = SvgRenderer::new(&content, 480, 480).unwrap();
            cache.renderer = Some(CachedRenderer::Svg(Box::new(r)));
            cache.current_layout = Some(layout.clone());
        }

        // Simulate the swap-decision the same way render_preview does:
        let cache = lock_cache(&state);
        let needs_rebuild =
            cache.current_layout.as_deref() != Some(layout.as_str()) || cache.renderer.is_none();
        assert!(!needs_rebuild, "should NOT rebuild for unchanged layout");
    }

    #[test]
    fn renderer_cache_rebuilds_on_layout_change() {
        let tmp = TempDir::new().unwrap();
        let state = make_state(&tmp);
        let svg_dir = state.layout_dir.join("svg");
        fs::write(svg_dir.join("other.svg"), SIMPLE_SVG).unwrap();

        let layout1 = "svg/simple.svg".to_string();
        let layout2 = "svg/other.svg".to_string();

        {
            let mut cache = lock_cache(&state);
            let r = SvgRenderer::new(SIMPLE_SVG, 480, 480).unwrap();
            cache.renderer = Some(CachedRenderer::Svg(Box::new(r)));
            cache.current_layout = Some(layout1.clone());
        }

        let cache = lock_cache(&state);
        let needs_rebuild =
            cache.current_layout.as_deref() != Some(layout2.as_str()) || cache.renderer.is_none();
        assert!(needs_rebuild, "must rebuild when layout changes");
    }

    // ---- get_saved_vars semantics ----

    #[test]
    fn get_saved_vars_returns_empty_for_unknown_layout() {
        // Mirror the semantics of get_saved_vars without a tauri::State wrapper.
        let tmp = TempDir::new().unwrap();
        let state = make_state(&tmp);
        let config = Config::load(&state.config_path).unwrap();
        let saved = config
            .layout_vars
            .get("svg/never-saved.svg")
            .cloned()
            .unwrap_or_default();
        assert!(saved.is_empty());
    }

    #[test]
    fn get_saved_vars_returns_persisted_overrides() {
        let tmp = TempDir::new().unwrap();
        let state = make_state(&tmp);
        let layout = "svg/simple.svg".to_string();
        let mut overrides = HashMap::new();
        overrides.insert("accent".to_string(), "#ff00aa".to_string());
        Config::save_layout_vars(&state.config_path, &layout, &overrides).unwrap();

        let config = Config::load(&state.config_path).unwrap();
        let saved = config.layout_vars.get(&layout).cloned().unwrap_or_default();
        assert_eq!(saved.get("accent").map(String::as_str), Some("#ff00aa"));
    }

    // ---- background path validation ----

    fn make_bg_dir(tmp: &TempDir) -> PathBuf {
        let bg_dir = tmp.path().join("backgrounds");
        fs::create_dir_all(&bg_dir).unwrap();
        // Seed two valid background files
        fs::write(bg_dir.join("dark-solid.png"), b"PNG").unwrap();
        fs::write(bg_dir.join("hex-grid.png"), b"PNG").unwrap();
        bg_dir
    }

    #[test]
    fn validate_background_path_rejects_parent_dir_traversal() {
        let tmp = TempDir::new().unwrap();
        let bg_dir = make_bg_dir(&tmp);
        let outside = tmp.path().join("outside.png");
        fs::write(&outside, b"PNG").unwrap();
        let err = validate_background_path(&bg_dir, "../outside.png").expect_err("must reject ..");
        assert!(matches!(err, AppError::InvalidBackground(_)), "got {err:?}");
    }

    #[test]
    fn validate_background_path_rejects_absolute_paths() {
        let tmp = TempDir::new().unwrap();
        let bg_dir = make_bg_dir(&tmp);
        let err = validate_background_path(&bg_dir, "/etc/passwd")
            .expect_err("must reject absolute path");
        assert!(matches!(err, AppError::InvalidBackground(_)), "got {err:?}");
    }

    #[test]
    fn validate_background_path_rejects_symlink_escape() {
        let tmp = TempDir::new().unwrap();
        let bg_dir = make_bg_dir(&tmp);
        let outside = tmp.path().join("outside.png");
        fs::write(&outside, b"PNG").unwrap();
        let link = bg_dir.join("escape.png");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let err = validate_background_path(&bg_dir, "escape.png")
            .expect_err("symlink escape must be rejected");
        assert!(matches!(err, AppError::InvalidBackground(_)), "got {err:?}");
    }

    #[test]
    fn validate_background_path_accepts_legit_file() {
        let tmp = TempDir::new().unwrap();
        let bg_dir = make_bg_dir(&tmp);
        let resolved =
            validate_background_path(&bg_dir, "dark-solid.png").expect("legit file must resolve");
        assert!(resolved.ends_with("dark-solid.png"));
    }

    // ---- list_backgrounds semantics ----

    #[test]
    fn list_background_names_returns_sorted_png_jpeg_only() {
        let tmp = TempDir::new().unwrap();
        let bg_dir = tmp.path().join("backgrounds");
        fs::create_dir_all(&bg_dir).unwrap();
        fs::write(bg_dir.join("zebra.png"), b"PNG").unwrap();
        fs::write(bg_dir.join("alpha.jpg"), b"JPG").unwrap();
        fs::write(bg_dir.join("middle.jpeg"), b"JPEG").unwrap();
        fs::write(bg_dir.join("ignored.txt"), b"txt").unwrap();
        fs::write(bg_dir.join("ignored.svg"), b"svg").unwrap();

        let names = list_background_names(&bg_dir);
        assert_eq!(
            names,
            vec!["alpha.jpg", "middle.jpeg", "zebra.png"],
            "must be sorted, PNG/JPEG only, no .txt or .svg"
        );
    }

    #[test]
    fn list_background_names_returns_empty_for_missing_dir() {
        let tmp = TempDir::new().unwrap();
        let bg_dir = tmp.path().join("backgrounds_nonexistent");
        let names = list_background_names(&bg_dir);
        assert!(
            names.is_empty(),
            "missing dir must yield empty list, not panic"
        );
    }

    // ---- get_active_background semantics ----

    #[test]
    fn get_active_background_returns_none_when_no_config() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        // Config file doesn't exist yet — should load defaults cleanly
        let config = Config::load(&config_path).unwrap();
        assert_eq!(
            config.background.image, None,
            "fresh config must have no active background"
        );
    }

    #[test]
    fn get_active_background_returns_saved_image_name() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        Config::save_background_image(&config_path, Some("dark-solid.png")).unwrap();
        let config = Config::load(&config_path).unwrap();
        assert_eq!(
            config.background.image.as_deref(),
            Some("dark-solid.png"),
            "must round-trip the saved image name"
        );
    }

    #[test]
    fn get_active_background_returns_none_after_clear() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        Config::save_background_image(&config_path, Some("dark-solid.png")).unwrap();
        Config::save_background_image(&config_path, None).unwrap();
        let config = Config::load(&config_path).unwrap();
        assert_eq!(
            config.background.image, None,
            "clearing image must persist as None"
        );
    }

    // ---- variable validation ----

    #[test]
    fn validate_vars_rejects_unknown_variable() {
        let decls: HashMap<String, FrontmatterVar> = HashMap::new();
        let mut vars = HashMap::new();
        vars.insert("nope".to_string(), "value".to_string());
        let err = validate_vars(&decls, &vars).unwrap_err();
        assert!(matches!(err, AppError::InvalidVariable(_)));
    }

    #[test]
    fn is_valid_color_accepts_rrggbb_and_rrggbbaa() {
        assert!(is_valid_color("#aabbcc"));
        assert!(is_valid_color("#aabbccdd"));
        assert!(!is_valid_color("aabbcc"));
        assert!(!is_valid_color("#xyz123"));
        assert!(!is_valid_color("#abc"));
    }

    #[test]
    fn contains_template_syntax_detects_tera_markers() {
        assert!(contains_template_syntax("hello {{ x }}"));
        assert!(contains_template_syntax("{% if x %}"));
        assert!(!contains_template_syntax("plain text"));
    }

    fn number_decl(min: Option<f64>, max: Option<f64>) -> HashMap<String, FrontmatterVar> {
        HashMap::from([(
            "panel_opacity".to_string(),
            FrontmatterVar {
                var_type: "number".to_string(),
                default: "0.5".to_string(),
                help: String::new(),
                min,
                max,
                step: Some(0.05),
            },
        )])
    }

    #[test]
    fn validate_vars_number_accepts_in_range() {
        let decls = number_decl(Some(0.0), Some(1.0));
        let vars = HashMap::from([("panel_opacity".to_string(), "0.7".to_string())]);
        assert!(validate_vars(&decls, &vars).is_ok());
    }

    #[test]
    fn validate_vars_number_rejects_out_of_range() {
        let decls = number_decl(Some(0.0), Some(1.0));
        let too_high = HashMap::from([("panel_opacity".to_string(), "1.5".to_string())]);
        assert!(matches!(
            validate_vars(&decls, &too_high).unwrap_err(),
            AppError::InvalidVariable(_)
        ));
        let too_low = HashMap::from([("panel_opacity".to_string(), "-0.2".to_string())]);
        assert!(matches!(
            validate_vars(&decls, &too_low).unwrap_err(),
            AppError::InvalidVariable(_)
        ));
    }

    #[test]
    fn validate_vars_number_rejects_non_numeric() {
        let decls = number_decl(None, None);
        let vars = HashMap::from([("panel_opacity".to_string(), "opaque".to_string())]);
        assert!(matches!(
            validate_vars(&decls, &vars).unwrap_err(),
            AppError::InvalidVariable(_)
        ));
    }

    #[test]
    fn validate_vars_number_rejects_non_finite() {
        let decls = number_decl(Some(0.0), Some(100.0));
        for bad in ["NaN", "inf", "-inf"] {
            let vars = HashMap::from([("panel_opacity".to_string(), bad.to_string())]);
            let err = validate_vars(&decls, &vars).unwrap_err();
            assert!(
                matches!(err, AppError::InvalidVariable(ref m) if m.contains("finite")),
                "expected finite rejection for {bad}, got {err:?}"
            );
        }
    }

    // ---- background import ----

    fn tiny_png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([10, 20, 30]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    }

    #[test]
    fn import_background_writes_decodable_png() {
        let tmp = TempDir::new().unwrap();
        let bg_dir = tmp.path().join("backgrounds");
        fs::create_dir_all(&bg_dir).unwrap();
        let stored = import_background_impl(&bg_dir, "my photo.png", &tiny_png(8, 8))
            .expect("valid PNG must import");
        assert_eq!(stored, "my photo.png");
        assert!(bg_dir.join("my photo.png").exists());
        // No stray temp file left behind.
        assert!(!bg_dir.join(".my photo.png.tmp").exists());
    }

    #[test]
    fn import_background_strips_directory_components() {
        let tmp = TempDir::new().unwrap();
        let bg_dir = tmp.path().join("backgrounds");
        fs::create_dir_all(&bg_dir).unwrap();
        let stored = import_background_impl(&bg_dir, "/etc/evil/../shot.png", &tiny_png(4, 4))
            .expect("only the file name is used");
        assert_eq!(stored, "shot.png");
        assert!(bg_dir.join("shot.png").exists());
    }

    #[test]
    fn import_background_dedupes_on_collision() {
        let tmp = TempDir::new().unwrap();
        let bg_dir = tmp.path().join("backgrounds");
        fs::create_dir_all(&bg_dir).unwrap();
        let png = tiny_png(4, 4);
        let first = import_background_impl(&bg_dir, "bg.png", &png).unwrap();
        let second = import_background_impl(&bg_dir, "bg.png", &png).unwrap();
        assert_eq!(first, "bg.png");
        assert_eq!(second, "bg-1.png");
        assert!(bg_dir.join("bg.png").exists());
        assert!(bg_dir.join("bg-1.png").exists());
    }

    #[test]
    fn import_background_create_new_does_not_clobber_existing() {
        let tmp = TempDir::new().unwrap();
        let bg_dir = tmp.path().join("backgrounds");
        fs::create_dir_all(&bg_dir).unwrap();
        let existing = tiny_png(4, 4);
        fs::write(bg_dir.join("bg.png"), &existing).unwrap();

        // Even if dedupe raced and returned "bg.png", create_new must refuse to
        // replace the existing file and retry under a new name.
        let stored = import_background_impl(&bg_dir, "bg.png", &tiny_png(8, 8)).unwrap();
        assert_ne!(stored, "bg.png");
        let kept = fs::read(bg_dir.join("bg.png")).unwrap();
        assert_eq!(kept, existing, "original bg.png must not be overwritten");
        assert!(bg_dir.join(&stored).exists());
    }

    #[test]
    fn import_background_rejects_bad_extension() {
        let tmp = TempDir::new().unwrap();
        let bg_dir = tmp.path().join("backgrounds");
        fs::create_dir_all(&bg_dir).unwrap();
        let err = import_background_impl(&bg_dir, "notes.txt", &tiny_png(4, 4)).unwrap_err();
        assert!(matches!(err, AppError::InvalidBackground(_)), "got {err:?}");
    }

    #[test]
    fn import_background_rejects_non_image_bytes() {
        let tmp = TempDir::new().unwrap();
        let bg_dir = tmp.path().join("backgrounds");
        fs::create_dir_all(&bg_dir).unwrap();
        let err = import_background_impl(&bg_dir, "fake.png", b"not really a png").unwrap_err();
        assert!(matches!(err, AppError::BackgroundIo(_)), "got {err:?}");
        // Nothing should have been written.
        assert!(!bg_dir.join("fake.png").exists());
    }

    #[test]
    fn import_background_rejects_oversized() {
        let tmp = TempDir::new().unwrap();
        let bg_dir = tmp.path().join("backgrounds");
        fs::create_dir_all(&bg_dir).unwrap();
        let big = vec![0u8; MAX_IMPORT_BYTES + 1];
        let err = import_background_impl(&bg_dir, "huge.png", &big).unwrap_err();
        assert!(matches!(err, AppError::BackgroundIo(_)), "got {err:?}");
    }

    // ---- read_frame ----

    /// read_frame_impl with an existing last.jpg returns its raw bytes.
    #[test]
    fn read_frame_returns_bytes_when_file_exists() {
        let tmp = TempDir::new().unwrap();
        let frame_dir = tmp.path().join("thermalwriter");
        fs::create_dir_all(&frame_dir).unwrap();
        let expected = b"\xff\xd8\xff\xe0FAKEJPEG";
        fs::write(frame_dir.join("last.jpg"), expected).unwrap();

        let bytes = read_frame_impl(&frame_dir).expect("must return bytes for existing file");
        assert_eq!(bytes, expected, "returned bytes must match written JPEG");
    }

    /// read_frame_impl with no last.jpg returns NoFrame error, not a panic.
    #[test]
    fn read_frame_returns_no_frame_error_when_missing() {
        let tmp = TempDir::new().unwrap();
        let frame_dir = tmp.path().join("thermalwriter"); // dir doesn't even exist

        let err = read_frame_impl(&frame_dir).unwrap_err();
        assert!(
            matches!(err, AppError::NoFrame(_)),
            "missing last.jpg must yield NoFrame, got: {err:?}"
        );
    }

    #[test]
    fn frame_dir_rejects_missing_xdg_runtime_dir() {
        let _guard = match ENV_LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        unsafe {
            std::env::remove_var("XDG_RUNTIME_DIR");
        }
        let err = frame_dir().unwrap_err();
        match original {
            Some(v) => unsafe { std::env::set_var("XDG_RUNTIME_DIR", v) },
            None => unsafe { std::env::remove_var("XDG_RUNTIME_DIR") },
        }
        assert!(
            matches!(err, AppError::NoFrame(_)),
            "missing XDG_RUNTIME_DIR must yield NoFrame, got: {err:?}"
        );
    }

    // ---- set_tick_rate validation ----

    /// set_tick_rate_validated rejects 0 and values > 60 with DaemonCall error.
    #[test]
    fn set_tick_rate_rejects_out_of_range() {
        assert!(
            matches!(validate_tick_rate(0), Err(AppError::DaemonCall(_))),
            "rate=0 must be rejected"
        );
        assert!(
            matches!(validate_tick_rate(61), Err(AppError::DaemonCall(_))),
            "rate=61 must be rejected"
        );
        assert!(
            matches!(validate_tick_rate(100), Err(AppError::DaemonCall(_))),
            "rate=100 must be rejected"
        );
    }

    /// set_tick_rate_validated accepts boundary values 1 and 60.
    #[test]
    fn set_tick_rate_accepts_boundary_values() {
        assert!(validate_tick_rate(1).is_ok(), "rate=1 must be accepted");
        assert!(validate_tick_rate(15).is_ok(), "rate=15 must be accepted");
        assert!(validate_tick_rate(60).is_ok(), "rate=60 must be accepted");
    }

    fn make_solid_color_png(width: u32, height: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
        use image::{ImageBuffer, ImageFormat, Rgb};
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(width, height, Rgb([r, g, b]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    fn background_rgb(background: &BackgroundImage) -> [u8; 3] {
        let pixmap = background.to_pixmap(480, 480).unwrap();
        [pixmap.data()[0], pixmap.data()[1], pixmap.data()[2]]
    }

    #[test]
    fn test_cached_renderer_background_clearing() {
        // Transparent layout: background pixels show through when a background
        // is present, then return to the fallback color when cleared.
        const TRANSPARENT_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="480" height="480" viewBox="0 0 480 480">
<text x="240" y="240" fill="#ffffff" font-size="32" text-anchor="middle">transparent</text>
</svg>
"##;

        let mut renderer = SvgRenderer::new(TRANSPARENT_SVG, 480, 480).unwrap();

        // 1. Set background to red
        let bg_bytes = make_solid_color_png(480, 480, 255, 0, 0); // solid red
        let bg = Arc::new(BackgroundImage::decode(&bg_bytes).unwrap());
        renderer.set_background(Some(bg)).unwrap();

        // Render and verify background shows through (red)
        let frame_with_bg = renderer.render(&mock_sensors()).unwrap();
        assert_eq!(
            frame_with_bg.data[0], 255,
            "R should be 255 (red background)"
        );
        assert_eq!(frame_with_bg.data[1], 0, "G should be 0");
        assert_eq!(frame_with_bg.data[2], 0, "B should be 0");

        // 2. Clear background (set to None), mimicking render_preview background = None.
        renderer.set_background(None).unwrap();

        // Render and verify background is cleared back to fallback #08080f (R:8, G:8, B:15)
        let frame_cleared = renderer.render(&mock_sensors()).unwrap();
        assert_eq!(frame_cleared.data[0], 8, "R should be 8 (fallback)");
        assert_eq!(frame_cleared.data[1], 8, "G should be 8 (fallback)");
        assert_eq!(frame_cleared.data[2], 15, "B should be 15 (fallback)");
    }

    #[test]
    fn test_preview_background_cache_semantics() {
        let tmp = TempDir::new().unwrap();
        let state = make_state(&tmp);

        // 1. Create a solid red PNG background file
        let bg1_path = state.background_dir.join("bg1.png");
        let red_png = make_solid_color_png(480, 480, 255, 0, 0);
        fs::write(&bg1_path, &red_png).unwrap();

        // Retrieve and verify it decodes to red
        let mut cache = lock_cache(&state);
        let pixmap = cached_preview_background(&state, &mut cache, Some("bg1.png"))
            .unwrap()
            .expect("should return a pixmap");
        assert_eq!(background_rgb(&pixmap), [255, 0, 0]);

        // Check that it's cached in the state
        assert_eq!(cache.current_background.as_deref(), Some("bg1.png"));
        assert!(cache.background_pixmap.is_some());

        // 2. Change the file on disk to blue
        let blue_png = make_solid_color_png(480, 480, 0, 0, 255);
        fs::write(&bg1_path, &blue_png).unwrap();

        // Retrieve again and verify it STILL decodes to red (cached)
        let pixmap_cached = cached_preview_background(&state, &mut cache, Some("bg1.png"))
            .unwrap()
            .expect("should return a pixmap");
        assert_eq!(background_rgb(&pixmap_cached), [255, 0, 0]);
        assert!(Arc::ptr_eq(&pixmap_cached, &pixmap));

        // 3. Clearing the background invalidates the cached background state
        let cleared = cached_preview_background(&state, &mut cache, None).unwrap();
        assert!(cleared.is_none());
        assert!(cache.current_background.is_none());
        assert!(cache.background_pixmap.is_none());

        // 4. Querying bg1.png after clearing reads the current version from disk (which is blue now)
        let pixmap_after_clear = cached_preview_background(&state, &mut cache, Some("bg1.png"))
            .unwrap()
            .expect("should return a pixmap");
        assert_eq!(background_rgb(&pixmap_after_clear), [0, 0, 255]);
        assert_eq!(cache.current_background.as_deref(), Some("bg1.png"));

        // 5. Changing selected background name decodes the new file
        let bg2_path = state.background_dir.join("bg2.png");
        let green_png = make_solid_color_png(480, 480, 0, 255, 0);
        fs::write(&bg2_path, &green_png).unwrap();

        let pixmap_new = cached_preview_background(&state, &mut cache, Some("bg2.png"))
            .unwrap()
            .expect("should return a pixmap");
        assert_eq!(background_rgb(&pixmap_new), [0, 255, 0]);
        assert_eq!(cache.current_background.as_deref(), Some("bg2.png"));
    }

    fn flagship_document() -> LayoutDocument {
        LayoutDocument::from_toml(thermalwriter::config::builtin_layouts::NEON_COMPOSER)
            .expect("flagship layout document")
    }

    #[test]
    fn typed_document_response_serializes_document_and_fingerprint() {
        let response = load_layout_preset("neon-composer".into()).expect("preset response");
        let json = serde_json::to_value(&response).expect("typed response JSON");

        assert_eq!(json["document"]["version"], 1);
        assert_eq!(json["document"]["modules"][0]["kind"], "metric");
        assert_eq!(json["document_fingerprint"].as_str().unwrap().len(), 64);
    }

    #[test]
    fn layout_document_load_rejects_paths_outside_layout_root() {
        let tmp = TempDir::new().unwrap();
        let layout_dir = tmp.path().join("layouts");
        fs::create_dir_all(&layout_dir).unwrap();
        fs::write(
            tmp.path().join("outside.layout.toml"),
            thermalwriter::config::builtin_layouts::NEON_COMPOSER,
        )
        .unwrap();

        let error = load_layout_document_impl(&layout_dir, "../outside.layout.toml")
            .expect_err("layout traversal must be rejected");
        assert!(matches!(error, AppError::InvalidLayout(_)), "got {error:?}");
    }

    #[test]
    fn save_maps_fingerprint_conflict_to_structured_diagnostic() {
        let tmp = TempDir::new().unwrap();
        let document = flagship_document();
        let saved = save_layout_document_impl(tmp.path(), "draft", None, &document)
            .expect("initial typed save");
        let error =
            save_layout_document_impl(tmp.path(), "draft", Some("stale-fingerprint"), &document)
                .expect_err("stale save must be rejected");

        let AppError::LayoutDiagnostics(diagnostics) = error else {
            panic!("expected structured diagnostics");
        };
        assert_eq!(
            diagnostics[0].code,
            thermalwriter::layout_engine::PERSISTENCE_CONFLICT_CODE
        );
        assert_eq!(saved.name, "draft");
    }

    #[test]
    fn design_context_locks_curved_profile_geometry_and_redacts_media() {
        let tmp = TempDir::new().unwrap();
        let state = make_state(&tmp);
        let document = flagship_document();
        let context = copy_layout_design_context_impl(
            &document,
            SurfaceProfileId::ThermalrightCurved2400x1080,
            2400,
            1080,
            &state.layout_dir,
        )
        .expect("curved design context");

        assert_eq!(
            context,
            copy_layout_design_context_impl(
                &document,
                SurfaceProfileId::ThermalrightCurved2400x1080,
                2400,
                1080,
                &state.layout_dir,
            )
            .expect("repeat curved design context")
        );
        assert!(context.contains("- Target: thermalright-curved-2400x1080 (2400x1080)"));
        assert!(context.contains(
            "- Topology: readable zones left/right; protected bridge; media-only spanning"
        ));
        assert!(context.contains("1. cpu-temp — metric — cpu.temperature — left — hero"));
        assert!(context.contains("- cpu-temp: x=36, y=36, width=888, height=387; zone=left"));
        assert!(context.contains("- history: x=1476, y=36, width=888, height=387; zone=right"));
        assert!(context.contains("- None (validation passed for this profile)."));
        assert!(context.contains("Generate → validate → preview → inspect → iterate."));

        let mut media_document = document;
        let mut media = thermalwriter::layout_engine::MediaDocument::default();
        media.id = "wallpaper".into();
        media.source = PathBuf::from("private-secret.png");
        media.span_bridge = true;
        media_document.modules.push(ModuleDocument::Media(media));
        let media_context = copy_layout_design_context_impl(
            &media_document,
            SurfaceProfileId::ThermalrightCurved2400x1080,
            2400,
            1080,
            &state.layout_dir,
        )
        .expect("media design context");
        assert!(media_context.contains("media source (bytes omitted)"));
        assert!(!media_context.contains("private-secret.png"));
    }

    #[test]
    fn validation_and_preview_report_native_profile_dimensions() {
        let document = flagship_document();
        let validation = validate_layout_document_impl(
            document.clone(),
            SurfaceProfileId::Rectangular,
            480,
            1280,
        )
        .expect("portrait validation");
        assert!(validation.valid);
        assert_eq!((validation.width, validation.height), (480, 1280));
        assert_eq!(validation.topology, PreviewTopology::Rectangular);

        let tmp = TempDir::new().unwrap();
        let state = make_state(&tmp);
        let preview =
            preview_layout_document_impl(document, SurfaceProfileId::Rectangular, 480, 480, &state)
                .expect("square preview");
        assert_eq!((preview.width, preview.height), (480, 480));
        assert_eq!(preview.rgba.len(), 480 * 480 * 4);
        assert_eq!(preview.topology, PreviewTopology::Rectangular);

        let cache = state.typed_cache.lock().unwrap();
        let key = cache.key.as_ref().expect("typed preview cache key");
        assert_eq!(key.profile, SurfaceProfileId::Rectangular);
        assert_eq!((key.width, key.height), (480, 480));
        assert_eq!(key.document_fingerprint, preview.document_fingerprint);
    }

    #[test]
    fn save_rejects_media_paths_outside_layout_root() {
        let tmp = TempDir::new().unwrap();
        let mut document = flagship_document();
        document.modules.push(ModuleDocument::Media(
            thermalwriter::layout_engine::MediaDocument {
                id: "unsafe-media".into(),
                binding: "../outside.png".into(),
                variant: "default".into(),
                ..Default::default()
            },
        ));

        let error = save_layout_document_impl(tmp.path(), "unsafe", None, &document)
            .expect_err("media traversal must be rejected");
        let AppError::LayoutDiagnostics(diagnostics) = error else {
            panic!("expected structured media path diagnostics");
        };
        assert_eq!(diagnostics[0].code, PERSISTENCE_PATH_CODE);
        assert_eq!(diagnostics[0].module_id.as_deref(), Some("unsafe-media"));
    }
}
