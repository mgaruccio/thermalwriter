use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::ipc::Response;
use thermalwriter::config::Config;
use thermalwriter::dbus_types::DisplayProxy;
use thermalwriter::render::background::BackgroundImage;
use thermalwriter::render::frontmatter::{LayoutFrontmatter, VariableDecl as FrontmatterVar};
use thermalwriter::render::palette::{self, SchemeSuggestion};
use thermalwriter::render::svg::SvgRenderer;
use thermalwriter::render::{FrameSource, SensorData};
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
}

/// Cached preview renderer state. The renderer is keyed by layout; the decoded
/// background pixmap is keyed by image name so slider/keystroke previews avoid
/// re-reading and re-decoding the same background file.
struct RendererCache {
    current_layout: Option<String>,
    renderer: Option<SvgRenderer<'static>>,
    current_background: Option<String>,
    background_pixmap: Option<Arc<BackgroundImage>>,
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
pub async fn list_sensors() -> Result<Vec<SensorDescriptor>, AppError> {
    match zbus::Connection::session().await {
        Ok(connection) => match DisplayProxy::new(&connection).await {
            Ok(proxy) => match proxy.list_sensors().await {
                Ok(sensors) if !sensors.is_empty() => Ok(sensors
                    .into_iter()
                    .map(|(key, name, unit)| SensorDescriptor { key, name, unit })
                    .collect()),
                _ => Ok(fallback_sensors()),
            },
            Err(_) => Ok(fallback_sensors()),
        },
        Err(_) => Ok(fallback_sensors()),
    }
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
    let theme = config.theme.manual.unwrap_or_default();

    let mut cache = state.cache.lock().map_err(|_| AppError::StatePoisoned)?;
    let background_image = cached_preview_background(&state, &mut cache, background.as_deref())?;
    if cache.current_layout.as_deref() != Some(layout.as_str()) || cache.renderer.is_none() {
        let mut renderer =
            SvgRenderer::new(&content, 480, 480).map_err(|e| AppError::Render(e.to_string()))?;
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
        cache.renderer = Some(renderer);
        cache.current_layout = Some(layout.clone());
    }
    let renderer = cache
        .renderer
        .as_mut()
        .ok_or_else(|| AppError::Render("renderer not initialized".into()))?;
    renderer.set_theme(theme);
    renderer.set_layout_vars(vars);
    renderer
        .set_background(background_image)
        .map_err(|error| AppError::Render(error.to_string()))?;

    let frame = renderer
        .render(&mock_sensors())
        .map_err(|e| AppError::Render(e.to_string()))?;
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
    matches!(
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

    let stored = dedupe_background_name(bg_dir, base);
    let dest = bg_dir.join(&stored);
    // Atomic write: temp sibling (hidden, .tmp suffix so list_backgrounds skips
    // it) then rename into place.
    let tmp = bg_dir.join(format!(".{stored}.tmp"));
    std::fs::write(&tmp, data).map_err(|e| AppError::BackgroundIo(e.to_string()))?;
    std::fs::rename(&tmp, &dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        AppError::BackgroundIo(e.to_string())
    })?;
    Ok(stored)
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
        },
        SensorDescriptor {
            key: "cpu_util".into(),
            name: "CPU Utilization".into(),
            unit: "%".into(),
        },
        SensorDescriptor {
            key: "cpu_power".into(),
            name: "CPU Power".into(),
            unit: "W".into(),
        },
        SensorDescriptor {
            key: "gpu_temp".into(),
            name: "GPU Temperature".into(),
            unit: "°C".into(),
        },
        SensorDescriptor {
            key: "gpu_util".into(),
            name: "GPU Utilization".into(),
            unit: "%".into(),
        },
        SensorDescriptor {
            key: "gpu_power".into(),
            name: "GPU Power".into(),
            unit: "W".into(),
        },
        SensorDescriptor {
            key: "ram_used".into(),
            name: "RAM Used".into(),
            unit: "GB".into(),
        },
        SensorDescriptor {
            key: "vram_used".into(),
            name: "VRAM Used".into(),
            unit: "GB".into(),
        },
        SensorDescriptor {
            key: "fps".into(),
            name: "FPS".into(),
            unit: "fps".into(),
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
            cache.renderer = Some(r);
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
            cache.renderer = Some(r);
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
}
