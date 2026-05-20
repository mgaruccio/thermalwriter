use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tauri::ipc::Response;
use thermalwriter::config::Config;
use thermalwriter::dbus_types::DisplayProxy;
use thermalwriter::render::frontmatter::{LayoutFrontmatter, VariableDecl as FrontmatterVar};
use thermalwriter::render::svg::SvgRenderer;
use thermalwriter::render::{FrameSource, SensorData};

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

/// Cached `SvgRenderer` keyed by the layout name that produced it. Rebuilt on
/// layout change to ensure usvg options/fontdb are correct for the new template.
struct RendererCache {
    current_layout: Option<String>,
    renderer: Option<SvgRenderer<'static>>,
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
}

#[derive(Debug, Serialize)]
pub struct SensorDescriptor {
    pub key: String,
    pub name: String,
    pub unit: String,
}

#[tauri::command]
pub fn list_layouts(state: tauri::State<'_, RendererState>) -> Result<Vec<LayoutSummary>, AppError> {
    let mut layouts = Vec::new();
    for name in list_layout_names(&state.layout_dir) {
        let path = match validate_layout_path(&state.layout_dir, &name) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let configurable = std::fs::read_to_string(&path)
            .map(|content| !LayoutFrontmatter::parse(&content).variables.is_empty())
            .unwrap_or(false);
        let kind = if name.ends_with(".svg") { "svg" } else { "html" }.to_string();
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
    let content = std::fs::read_to_string(&path)
        .map_err(|e| AppError::LayoutIo(e.to_string()))?;
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
    state: tauri::State<'_, RendererState>,
) -> Result<Response, AppError> {
    let path = validate_layout_path(&state.layout_dir, &layout)?;
    let content = std::fs::read_to_string(&path)
        .map_err(|e| AppError::LayoutIo(e.to_string()))?;
    let frontmatter = LayoutFrontmatter::parse(&content);
    validate_vars(&frontmatter.variables, &vars)?;

    let config = Config::load(&state.config_path).map_err(|e| AppError::Config(e.to_string()))?;
    let theme = config.theme.manual.unwrap_or_default();

    let mut cache = state.cache.lock().map_err(|_| AppError::StatePoisoned)?;
    if cache.current_layout.as_deref() != Some(layout.as_str()) || cache.renderer.is_none() {
        let renderer = SvgRenderer::new(&content, 480, 480)
            .map_err(|e| AppError::Render(e.to_string()))?;
        cache.renderer = Some(renderer);
        cache.current_layout = Some(layout.clone());
    }
    let renderer = cache
        .renderer
        .as_mut()
        .ok_or_else(|| AppError::Render("renderer not initialized".into()))?;
    renderer.set_theme(theme);
    renderer.set_layout_vars(vars);

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
    let content = std::fs::read_to_string(&path)
        .map_err(|e| AppError::LayoutIo(e.to_string()))?;
    let frontmatter = LayoutFrontmatter::parse(&content);
    validate_vars(&frontmatter.variables, &vars)?;

    let mode = if layout.ends_with(".html") { "html" } else { "svg" };
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
    let content = std::fs::read_to_string(&path)
        .map_err(|e| AppError::LayoutIo(e.to_string()))?;
    let frontmatter = LayoutFrontmatter::parse(&content);
    validate_vars(&frontmatter.variables, &vars)?;

    let connection = zbus::Connection::session()
        .await
        .map_err(|e| AppError::DaemonUnavailable {
            reason: format!("session bus unavailable: {e}"),
        })?;
    let proxy = DisplayProxy::new(&connection)
        .await
        .map_err(|e| AppError::DaemonUnavailable {
            reason: format!("daemon proxy not reachable: {e}"),
        })?;
    // Route layout vars through daemon — it owns the in-memory state and
    // triggers ModeChange::Layout for the tick loop.
    proxy
        .set_layout_vars(&layout, vars)
        .await
        .map_err(|e| AppError::DaemonCall(format!("set_layout_vars failed: {e}")))?;
    // Persist the new default layout through daemon so both config.toml writes
    // go through the same atomic path (no concurrent GUI vs daemon clobber).
    proxy
        .set_default_layout(&layout)
        .await
        .map_err(|e| AppError::DaemonCall(format!("set_default_layout failed: {e}")))?;
    Ok(())
}

// ---- background commands ----

#[tauri::command]
pub fn list_backgrounds(
    state: tauri::State<'_, RendererState>,
) -> Result<Vec<String>, AppError> {
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
    let connection = zbus::Connection::session()
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

// ---- helpers ----

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
                if sub_path.is_file() && has_layout_ext(&sub_path) {
                    if let Ok(rel) = sub_path.strip_prefix(layout_dir) {
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

/// Resolve a background filename relative to the background directory, rejecting
/// any path that escapes via `..`, absolute paths, or symlinks pointing outside.
fn validate_background_path(bg_dir: &Path, name: &str) -> Result<PathBuf, AppError> {
    let candidate = Path::new(name);
    if candidate.is_absolute()
        || candidate
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(AppError::InvalidBackground(name.to_string()));
    }

    let base = bg_dir
        .canonicalize()
        .map_err(|e| AppError::InvalidBackground(format!("background dir not accessible: {e}")))?;
    let resolved = base
        .join(name)
        .canonicalize()
        .map_err(|_| AppError::BackgroundNotFound(name.to_string()))?;
    if !resolved.starts_with(&base) {
        return Err(AppError::InvalidBackground(format!(
            "{name} escapes background directory"
        )));
    }
    Ok(resolved)
}

/// Resolve a layout name relative to the layout directory, rejecting any path
/// that escapes the layout directory (via `..`, absolute paths, or symlinks
/// pointing outside). Uses `canonicalize` + `starts_with` per the security
/// requirement — never relies on textual `..` checks alone.
fn validate_layout_path(layout_dir: &Path, name: &str) -> Result<PathBuf, AppError> {
    let candidate = Path::new(name);
    if candidate.is_absolute()
        || candidate
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(AppError::InvalidLayout(name.to_string()));
    }

    let base = layout_dir
        .canonicalize()
        .map_err(|e| AppError::InvalidLayout(format!("layout dir not accessible: {e}")))?;
    let resolved = base
        .join(name)
        .canonicalize()
        .map_err(|_| AppError::LayoutNotFound(name.to_string()))?;
    if !resolved.starts_with(&base) {
        return Err(AppError::InvalidLayout(format!(
            "{name} escapes layout directory"
        )));
    }
    Ok(resolved)
}

fn validate_vars(
    declarations: &HashMap<String, FrontmatterVar>,
    vars: &HashMap<String, String>,
) -> Result<(), AppError> {
    for (name, value) in vars {
        let Some(decl) = declarations.get(name) else {
            return Err(AppError::InvalidVariable(format!(
                "unknown layout variable: {name}"
            )));
        };
        match decl.var_type.as_str() {
            "color" if !is_valid_color(value) => {
                return Err(AppError::InvalidVariable(format!(
                    "{name} must be a #rrggbb or #rrggbbaa color"
                )));
            }
            "text" if contains_template_syntax(value) => {
                return Err(AppError::InvalidVariable(format!(
                    "{name} may not contain template syntax"
                )));
            }
            "sensor" if value.trim().is_empty() => {
                return Err(AppError::InvalidVariable(format!(
                    "{name} must select a sensor"
                )));
            }
            "color" | "text" | "sensor" => {}
            other => {
                return Err(AppError::InvalidVariable(format!(
                    "unsupported variable type for {name}: {other}"
                )));
            }
        }
    }
    Ok(())
}

fn is_valid_color(value: &str) -> bool {
    let Some(hex) = value.strip_prefix('#') else {
        return false;
    };
    matches!(hex.len(), 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit())
}

fn contains_template_syntax(value: &str) -> bool {
    value.contains("{{") || value.contains("}}") || value.contains("{%") || value.contains("%}")
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

        let err = validate_layout_path(&layout_dir, "../outside.svg")
            .expect_err("must reject ..");
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
            let mut cache = state.cache.lock().unwrap();
            assert!(cache.renderer.is_none(), "cache empty initially");
            let r = SvgRenderer::new(&content, 480, 480).unwrap();
            cache.renderer = Some(r);
            cache.current_layout = Some(layout.clone());
        }

        // Simulate the swap-decision the same way render_preview does:
        let cache = state.cache.lock().unwrap();
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
            let mut cache = state.cache.lock().unwrap();
            let r = SvgRenderer::new(SIMPLE_SVG, 480, 480).unwrap();
            cache.renderer = Some(r);
            cache.current_layout = Some(layout1.clone());
        }

        let cache = state.cache.lock().unwrap();
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
        let saved = config
            .layout_vars
            .get(&layout)
            .cloned()
            .unwrap_or_default();
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
        let err = validate_background_path(&bg_dir, "../outside.png")
            .expect_err("must reject ..");
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
        let resolved = validate_background_path(&bg_dir, "dark-solid.png")
            .expect("legit file must resolve");
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
        assert_eq!(names, vec!["alpha.jpg", "middle.jpeg", "zebra.png"],
            "must be sorted, PNG/JPEG only, no .txt or .svg");
    }

    #[test]
    fn list_background_names_returns_empty_for_missing_dir() {
        let tmp = TempDir::new().unwrap();
        let bg_dir = tmp.path().join("backgrounds_nonexistent");
        let names = list_background_names(&bg_dir);
        assert!(names.is_empty(), "missing dir must yield empty list, not panic");
    }

    // ---- get_active_background semantics ----

    #[test]
    fn get_active_background_returns_none_when_no_config() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        // Config file doesn't exist yet — should load defaults cleanly
        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.background.image, None,
            "fresh config must have no active background");
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
        assert_eq!(config.background.image, None,
            "clearing image must persist as None");
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
}
