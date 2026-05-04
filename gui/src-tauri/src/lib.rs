use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::ipc::Response;
use thermalwriter::config::{Config, builtin_layouts};
use thermalwriter::dbus_types::DisplayProxy;
use thermalwriter::render::frontmatter::{LayoutFrontmatter, VariableDecl as FrontmatterVar};
use thermalwriter::render::svg::SvgRenderer;
use thermalwriter::render::{FrameSource, SensorData};
use thermalwriter::theme::ThemePalette;

#[derive(Clone)]
struct AppState {
    layout_dir: PathBuf,
    config_path: PathBuf,
}

#[derive(Debug, Serialize)]
struct LayoutSummary {
    name: String,
    kind: String,
    configurable: bool,
}

#[derive(Debug, Serialize)]
struct VariableDecl {
    name: String,
    #[serde(rename = "type")]
    var_type: String,
    default: String,
    help: String,
    value: String,
}

#[derive(Debug, Serialize)]
struct SensorDescriptor {
    key: String,
    name: String,
    unit: String,
}

#[derive(Debug, Serialize)]
struct ApplyResult {
    saved: bool,
    applied: bool,
    message: String,
}

#[tauri::command]
fn list_layouts(state: tauri::State<'_, AppState>) -> Result<Vec<LayoutSummary>, String> {
    let mut layouts = Vec::new();
    for name in list_layout_names(&state.layout_dir) {
        let path = validate_layout_path(&state.layout_dir, &name)?;
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
fn get_layout_vars(
    layout: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<VariableDecl>, String> {
    let path = validate_layout_path(&state.layout_dir, &layout)?;
    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("Failed to read layout: {e}"))?;
    let frontmatter = LayoutFrontmatter::parse(&content);
    let config = Config::load(&state.config_path).map_err(|e| e.to_string())?;
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
async fn list_sensors() -> Result<Vec<SensorDescriptor>, String> {
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
fn render_preview(
    layout: String,
    vars: HashMap<String, String>,
    state: tauri::State<'_, AppState>,
) -> Result<Response, String> {
    let path = validate_layout_path(&state.layout_dir, &layout)?;
    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("Failed to read layout: {e}"))?;
    let frontmatter = LayoutFrontmatter::parse(&content);
    validate_vars(&frontmatter.variables, &vars)?;

    let config = Config::load(&state.config_path).map_err(|e| e.to_string())?;
    let theme = config.theme.manual.unwrap_or_else(ThemePalette::default);
    let mut renderer = SvgRenderer::new(&content, 480, 480).map_err(|e| e.to_string())?;
    renderer.set_theme(theme);
    renderer.set_layout_vars(vars);

    let frame = renderer
        .render(&mock_sensors())
        .map_err(|e| format!("Preview render failed: {e}"))?;
    Ok(Response::new(rgb_to_rgba(&frame.data)))
}

#[tauri::command]
async fn apply_layout(
    layout: String,
    vars: HashMap<String, String>,
    state: tauri::State<'_, AppState>,
) -> Result<ApplyResult, String> {
    let path = validate_layout_path(&state.layout_dir, &layout)?;
    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("Failed to read layout: {e}"))?;
    let frontmatter = LayoutFrontmatter::parse(&content);
    validate_vars(&frontmatter.variables, &vars)?;

    let mode = if layout.ends_with(".html") {
        "html"
    } else {
        "svg"
    };
    Config::save_layout_vars(&state.config_path, &layout, &vars).map_err(|e| e.to_string())?;
    Config::save_display_layout(&state.config_path, &layout, mode).map_err(|e| e.to_string())?;

    match zbus::Connection::session().await {
        Ok(connection) => match DisplayProxy::new(&connection).await {
            Ok(proxy) => {
                if let Err(e) = proxy.set_layout_vars(&layout, vars).await {
                    return Ok(ApplyResult {
                        saved: true,
                        applied: false,
                        message: format!("Saved, but daemon apply failed: {e}"),
                    });
                }
                match proxy.set_layout(&layout).await {
                    Ok(_) => Ok(ApplyResult {
                        saved: true,
                        applied: true,
                        message: format!("Saved and applied {layout}"),
                    }),
                    Err(e) => Ok(ApplyResult {
                        saved: true,
                        applied: false,
                        message: format!("Saved, but daemon layout switch failed: {e}"),
                    }),
                }
            }
            Err(_) => Ok(ApplyResult {
                saved: true,
                applied: false,
                message: "Saved. Daemon is not running, so changes were not applied live."
                    .to_string(),
            }),
        },
        Err(_) => Ok(ApplyResult {
            saved: true,
            applied: false,
            message: "Saved. Daemon is not running, so changes were not applied live.".to_string(),
        }),
    }
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

fn validate_layout_path(layout_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let candidate = Path::new(name);
    if candidate.is_absolute()
        || candidate
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!("Invalid layout name: {name}"));
    }

    let base = layout_dir
        .canonicalize()
        .map_err(|e| format!("Layout directory is not accessible: {e}"))?;
    let resolved = base
        .join(name)
        .canonicalize()
        .map_err(|_| format!("Layout not found: {name}"))?;
    if !resolved.starts_with(&base) {
        return Err(format!("Layout escapes layout directory: {name}"));
    }
    Ok(resolved)
}

fn validate_vars(
    declarations: &HashMap<String, FrontmatterVar>,
    vars: &HashMap<String, String>,
) -> Result<(), String> {
    for (name, value) in vars {
        let Some(decl) = declarations.get(name) else {
            return Err(format!("Unknown layout variable: {name}"));
        };
        match decl.var_type.as_str() {
            "color" if !is_valid_color(value) => {
                return Err(format!("{name} must be a #rrggbb or #rrggbbaa color"));
            }
            "text" if contains_template_syntax(value) => {
                return Err(format!("{name} may not contain template syntax"));
            }
            "sensor" if value.trim().is_empty() => {
                return Err(format!("{name} must select a sensor"));
            }
            "color" | "text" | "sensor" => {}
            other => return Err(format!("Unsupported variable type for {name}: {other}")),
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

fn rgb_to_rgba(rgb: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((rgb.len() / 3) * 4);
    for chunk in rgb.chunks_exact(3) {
        rgba.extend_from_slice(chunk);
        rgba.push(255);
    }
    rgba
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = env_logger::try_init();
    unsafe {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    let config_path = Config::default_path();
    let layout_dir = config_path
        .parent()
        .map(|p| p.join("layouts"))
        .unwrap_or_else(|| PathBuf::from("layouts"));
    std::fs::create_dir_all(&layout_dir).expect("failed to create layout directory");
    builtin_layouts::seed_layout_dir(&layout_dir).expect("failed to seed built-in layouts");

    tauri::Builder::default()
        .manage(AppState {
            layout_dir,
            config_path,
        })
        .invoke_handler(tauri::generate_handler![
            list_layouts,
            get_layout_vars,
            list_sensors,
            render_preview,
            apply_layout,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
