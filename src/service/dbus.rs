// D-Bus interface: exposes service control via com.thermalwriter.Display.
// Methods: set_layout, get_status, list_layouts, list_sensors, stop, reload,
//          get_layout_vars, set_layout_vars, set_background, clear_background,
//          list_backgrounds.
// Properties: active_layout, connected, resolution, tick_rate.
// Signals: layout_changed, device_connected, device_disconnected, error.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, watch};
use zbus::{interface, object_server::SignalEmitter};
use log::info;

use crate::config::Config;
use crate::render::frontmatter::LayoutFrontmatter;

/// Message sent through the mode change channel to switch display modes.
#[derive(Debug, Clone)]
pub enum ModeChange {
    /// Switch to an SVG or HTML layout by name.
    Layout {
        name: String,
        vars: HashMap<String, String>,
    },
    /// Switch to xvfb capture mode with the given shell command.
    Xvfb { command: String },
    /// Set or clear the global background image.
    Background { image: Option<tiny_skia::Pixmap> },
}

/// Shared state between the D-Bus interface and the tick loop.
pub struct ServiceState {
    pub active_layout: String,
    pub mode: String,
    pub connected: bool,
    pub resolution: (u32, u32),
    pub tick_rate: u32,
    pub jpeg_quality: u8,
    pub shutdown_tx: watch::Sender<bool>,
    pub layout_dir: std::path::PathBuf,
    /// Path to the on-disk config.toml (used by set_layout_vars for persistence).
    pub config_path: std::path::PathBuf,
    /// Snapshot of sensor descriptors (key, name, unit). Populated in main.rs
    /// after the first sensor_hub.poll() so list_sensors() returns real data.
    pub sensor_descriptors: Vec<(String, String, String)>,
    /// In-memory mirror of the running daemon's Config. set_layout_vars mutates
    /// this alongside the on-disk file so the tick loop sees fresh values
    /// without a restart.
    pub config: Config,
    /// Notify the daemon to switch display mode or layout.
    pub mode_change_tx: tokio::sync::mpsc::Sender<ModeChange>,
    /// Directory containing background image files.
    pub background_dir: PathBuf,
    /// Currently active decoded background pixmap (premultiplied RGBA 480x480).
    pub current_background: Option<tiny_skia::Pixmap>,
}

pub struct DisplayInterface {
    state: Arc<Mutex<ServiceState>>,
}

impl DisplayInterface {
    pub fn new(state: Arc<Mutex<ServiceState>>) -> Self {
        Self { state }
    }
}

// ---------------------------------------------------------------------------
// Free-function helpers: path validation + layout listing + vars read.
// Factored out so tests can call them without binding a D-Bus service name.
// ---------------------------------------------------------------------------

/// Shared traversal guard: resolve `name` against `base_dir` and return the
/// canonical path only if it stays within the directory. Rejects absolute paths,
/// `..` components, symlink escapes, and non-existent names.
/// `kind` labels error messages ("Layout", "Background").
fn validate_path_within_dir(
    base_dir: &Path,
    name: &str,
    kind: &str,
) -> Result<PathBuf, zbus::fdo::Error> {
    let candidate = Path::new(name);
    if candidate.is_absolute() {
        return Err(zbus::fdo::Error::InvalidArgs(format!(
            "{kind} name must be relative: {name}"
        )));
    }
    if candidate
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(zbus::fdo::Error::InvalidArgs(format!(
            "{kind} name may not contain '..': {name}"
        )));
    }
    let base = base_dir.canonicalize().map_err(|e| {
        zbus::fdo::Error::Failed(format!(
            "{kind} directory not accessible ({}): {e}",
            base_dir.display()
        ))
    })?;
    let resolved = base.join(name).canonicalize().map_err(|_| {
        zbus::fdo::Error::InvalidArgs(format!("{kind} not found: {name}"))
    })?;
    if !resolved.starts_with(&base) {
        return Err(zbus::fdo::Error::InvalidArgs(format!(
            "{kind} path escapes directory: {name}"
        )));
    }
    Ok(resolved)
}

/// Resolve `name` against `layout_dir`, rejecting traversal and symlink escapes.
pub fn validate_layout_path(
    layout_dir: &Path,
    name: &str,
) -> Result<PathBuf, zbus::fdo::Error> {
    validate_path_within_dir(layout_dir, name, "Layout")
}

/// List layout files (`.html` and `.svg`) under `layout_dir`, recursing one
/// level into subdirectories so `svg/neon-dash.svg` is returned with the
/// `svg/` prefix. Output is sorted for stable client rendering.
pub fn list_layouts_impl(layout_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();

    let Ok(top) = std::fs::read_dir(layout_dir) else {
        return out;
    };
    for entry in top.flatten() {
        let path = entry.path();
        if path.is_file() {
            if has_layout_ext(&path) {
                if let Some(name) = path.file_name() {
                    out.push(name.to_string_lossy().to_string());
                }
            }
        } else if path.is_dir() {
            let Ok(sub) = std::fs::read_dir(&path) else {
                continue;
            };
            for sub_entry in sub.flatten() {
                let sub_path = sub_entry.path();
                if sub_path.is_file() && has_layout_ext(&sub_path) {
                    if let Ok(rel) = sub_path.strip_prefix(layout_dir) {
                        // Normalize to forward slashes; the LCD repo is unix-only.
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

fn has_layout_ext(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()),
        Some("html") | Some("svg")
    )
}

fn has_image_ext(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()),
        Some("png") | Some("jpg") | Some("jpeg")
    )
}

/// Resolve `name` against `bg_dir`, rejecting traversal and symlink escapes.
pub fn validate_background_path(
    bg_dir: &Path,
    name: &str,
) -> Result<PathBuf, zbus::fdo::Error> {
    validate_path_within_dir(bg_dir, name, "Background")
}

/// List background image files (PNG/JPEG) under `bg_dir`. Flat listing only.
pub fn list_backgrounds_impl(bg_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(bg_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name() else { continue; };
        if path.is_file() && has_image_ext(&path) {
            out.push(name.to_string_lossy().to_string());
        }
    }
    out.sort();
    out
}

/// Apply a background change — the non-D-Bus core of `set_background`/`clear_background`:
///   1. Persist `image` to `config_path` via `Config::save_background_image`.
///   2. Mirror into in-memory `config.background.image`.
///   3. Send `ModeChange::Background { image: pixmap }` over `tx`.
///
/// `name` is the filename string (for persistence); `pixmap` is the decoded
/// Pixmap (for the tick loop). Passing `None` for both clears the background.
pub async fn apply_background(
    config_path: &Path,
    config: &mut Config,
    name: Option<&str>,
    pixmap: Option<tiny_skia::Pixmap>,
    tx: &tokio::sync::mpsc::Sender<ModeChange>,
) -> Result<(), zbus::fdo::Error> {
    Config::save_background_image(config_path, name).map_err(|e| {
        zbus::fdo::Error::Failed(format!("Failed to persist background image: {}", e))
    })?;
    config.background.image = name.map(|s| s.to_string());
    tx.send(ModeChange::Background { image: pixmap })
        .await
        .map_err(|e| zbus::fdo::Error::Failed(format!("Failed to notify tick loop: {}", e)))?;
    Ok(())
}

/// Read the layout file under `layout_dir` (validated against traversal) and
/// return its declared variables as a list of dicts with keys `name`, `type`,
/// `default`, `help`. Empty list if the layout declares no vars.
pub fn get_layout_vars_impl(
    layout_dir: &Path,
    name: &str,
) -> Result<Vec<HashMap<String, String>>, zbus::fdo::Error> {
    let path = validate_layout_path(layout_dir, name)?;
    let content = std::fs::read_to_string(&path).map_err(|e| {
        zbus::fdo::Error::Failed(format!(
            "Failed to read layout {}: {}",
            path.display(),
            e
        ))
    })?;
    let fm = LayoutFrontmatter::parse(&content);

    let mut out = Vec::with_capacity(fm.variables.len());
    for (var_name, decl) in &fm.variables {
        let mut m = HashMap::new();
        m.insert("name".to_string(), var_name.clone());
        m.insert("type".to_string(), decl.var_type.clone());
        m.insert("default".to_string(), decl.default.clone());
        m.insert("help".to_string(), decl.help.clone());
        out.push(m);
    }
    Ok(out)
}

/// Apply variable overrides for `name`:
///   1. Validate the layout path against traversal.
///   2. Persist vars to `config_path` via `Config::save_layout_vars`.
///   3. Mirror the values into the in-memory `Config::layout_vars` so the
///      running tick loop can see them without a restart.
///
/// This is the non-D-Bus core of `DisplayInterface::set_layout_vars` so tests
/// can exercise the disk+in-memory contract without binding a session-bus
/// service name. Callers are responsible for signalling the tick loop to
/// reload the template after this returns Ok.
pub fn apply_layout_vars(
    layout_dir: &Path,
    config_path: &Path,
    config: &mut Config,
    name: &str,
    vars: HashMap<String, String>,
) -> Result<(), zbus::fdo::Error> {
    validate_layout_path(layout_dir, name)?;
    Config::save_layout_vars(config_path, name, &vars).map_err(|e| {
        zbus::fdo::Error::Failed(format!("Failed to persist layout vars: {}", e))
    })?;
    config.layout_vars.insert(name.to_string(), vars);
    Ok(())
}

#[interface(name = "com.thermalwriter.Display")]
impl DisplayInterface {
    /// Switch the active layout. Returns an error if the layout file doesn't exist
    /// or resolves outside the layout directory.
    async fn set_layout(
        &self,
        name: String,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<String> {
        // Hold the lock through both the channel send and state update — no TOCTOU window.
        // tokio::sync::Mutex is safe to hold across .await.
        let mut state = self.state.lock().await;
        // Path-traversal + existence check.
        validate_layout_path(&state.layout_dir, &name)?;
        let vars = state.config.layout_vars.get(&name).cloned().unwrap_or_default();
        state.mode_change_tx.send(ModeChange::Layout { name: name.clone(), vars }).await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        state.active_layout = name.clone();
        state.mode = if name.ends_with(".html") { "html" } else { "svg" }.to_string();

        Self::layout_changed(&emitter, &name).await?;
        Ok(format!("Layout set to: {}", name))
    }

    /// Switch display mode. mode="xvfb" starts capture with the given command.
    /// mode="svg" or mode="html" with command as layout name switches back to layout mode.
    async fn set_mode(&self, mode: String, command: String) -> zbus::fdo::Result<String> {
        let mut state = self.state.lock().await;
        let change = match mode.as_str() {
            "xvfb" => {
                if command.is_empty() {
                    return Err(zbus::fdo::Error::InvalidArgs(
                        "xvfb mode requires a command".to_string()
                    ));
                }
                ModeChange::Xvfb { command: command.clone() }
            }
            "svg" | "html" => {
                // Path-traversal + existence check on the layout name.
                validate_layout_path(&state.layout_dir, &command)?;
                let vars = state.config.layout_vars.get(&command).cloned().unwrap_or_default();
                ModeChange::Layout { name: command.clone(), vars }
            }
            _ => return Err(zbus::fdo::Error::InvalidArgs(
                format!("Unknown mode: {} (expected svg, html, or xvfb)", mode)
            )),
        };

        state.mode_change_tx.send(change).await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        state.mode = mode.clone();

        Ok(format!("Mode set to: {} ({})", mode, command))
    }

    /// Return a snapshot of service status as key→value pairs.
    async fn get_status(&self) -> HashMap<String, String> {
        let state = self.state.lock().await;
        let mut status = HashMap::new();
        status.insert("active_layout".to_string(), state.active_layout.clone());
        status.insert("mode".to_string(), state.mode.clone());
        status.insert("connected".to_string(), state.connected.to_string());
        status.insert("resolution".to_string(),
            format!("{}x{}", state.resolution.0, state.resolution.1));
        status.insert("tick_rate".to_string(), state.tick_rate.to_string());
        status
    }

    /// Return sorted list of available layout filenames (`.html` and `.svg`),
    /// including `svg/*.svg` under the `svg/` subdirectory.
    async fn list_layouts(&self) -> Vec<String> {
        let state = self.state.lock().await;
        list_layouts_impl(&state.layout_dir)
    }

    /// Return available sensor descriptors as `(key, name, unit)` tuples.
    /// Populated from the sensor hub at startup — D-Bus does not support
    /// custom structs, so we expose a tuple shape.
    async fn list_sensors(&self) -> Vec<(String, String, String)> {
        self.state.lock().await.sensor_descriptors.clone()
    }

    /// Return the declared variables for `name` as a list of dicts with keys
    /// `name`, `type`, `default`, `help`.
    async fn get_layout_vars(
        &self,
        name: String,
    ) -> zbus::fdo::Result<Vec<HashMap<String, String>>> {
        let state = self.state.lock().await;
        get_layout_vars_impl(&state.layout_dir, &name)
    }

    /// Apply variable overrides to `name`: (a) persist to config.toml via
    /// `Config::save_layout_vars`, (b) update the daemon's in-memory Config so
    /// the tick loop sees fresh values without a restart, (c) signal the tick
    /// loop to reload the layout.
    async fn set_layout_vars(
        &self,
        name: String,
        vars: HashMap<String, String>,
    ) -> zbus::fdo::Result<()> {
        let mut state = self.state.lock().await;

        // Steps (a) + (b): validate + persist + update in-memory config.
        let layout_dir = state.layout_dir.clone();
        let config_path = state.config_path.clone();
        apply_layout_vars(&layout_dir, &config_path, &mut state.config, &name, vars)?;

        // Step (c): tell the tick loop to reload with fresh context.
        let vars = state.config.layout_vars.get(&name).cloned().unwrap_or_default();
        state
            .mode_change_tx
            .send(ModeChange::Layout { name: name.clone(), vars })
            .await
            .map_err(|e| zbus::fdo::Error::Failed(format!("Failed to notify tick loop: {}", e)))?;

        Ok(())
    }

    /// Set the global background image by filename (must exist in background_dir).
    async fn set_background(&self, name: String) -> zbus::fdo::Result<()> {
        let mut state = self.state.lock().await;
        let bg_path = validate_background_path(&state.background_dir, &name)?;
        let pixmap = crate::render::background::decode_from_file(&bg_path).map_err(|e| {
            zbus::fdo::Error::Failed(format!("Failed to decode background '{}': {}", name, e))
        })?;
        let config_path = state.config_path.clone();
        let tx = state.mode_change_tx.clone();
        apply_background(&config_path, &mut state.config, Some(&name), Some(pixmap.clone()), &tx)
            .await?;
        state.current_background = Some(pixmap);
        Ok(())
    }

    /// Clear the global background image.
    async fn clear_background(&self) -> zbus::fdo::Result<()> {
        let mut state = self.state.lock().await;
        let config_path = state.config_path.clone();
        let tx = state.mode_change_tx.clone();
        apply_background(&config_path, &mut state.config, None, None, &tx).await?;
        state.current_background = None;
        Ok(())
    }

    /// List available background image filenames in the background directory.
    async fn list_backgrounds(&self) -> Vec<String> {
        let state = self.state.lock().await;
        list_backgrounds_impl(&state.background_dir)
    }

    /// Signal the daemon to shut down cleanly.
    async fn stop(&self) {
        let state = self.state.lock().await;
        let _ = state.shutdown_tx.send(true);
        info!("Shutdown requested via D-Bus");
    }

    /// Trigger a config reload (reconnect transport, re-read layout).
    async fn reload(&self) {
        info!("Reload requested via D-Bus");
        // Full reload handled by tick loop watching layout_change_tx
    }

    // --- Properties ---

    #[zbus(property)]
    /// Name of the currently active layout file.
    async fn active_layout(&self) -> String {
        self.state.lock().await.active_layout.clone()
    }

    #[zbus(property)]
    /// Whether the USB device is currently connected.
    async fn connected(&self) -> bool {
        self.state.lock().await.connected
    }

    #[zbus(property)]
    /// Display resolution as (width, height).
    async fn resolution(&self) -> (u32, u32) {
        self.state.lock().await.resolution
    }

    #[zbus(property)]
    /// Current tick rate in frames per second.
    async fn tick_rate(&self) -> u32 {
        self.state.lock().await.tick_rate
    }

    #[zbus(property)]
    /// Set the tick rate (1–30 FPS). Returns error outside that range.
    async fn set_tick_rate(&mut self, rate: u32) -> zbus::fdo::Result<()> {
        if rate == 0 || rate > 30 {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Tick rate must be 1-30".to_string()
            ));
        }
        self.state.lock().await.tick_rate = rate;
        Ok(())
    }

    // --- Signals ---

    /// Emitted when the active layout changes.
    #[zbus(signal)]
    async fn layout_changed(emitter: &SignalEmitter<'_>, name: &str) -> zbus::Result<()>;

    /// Emitted when the USB device connects (after handshake).
    #[zbus(signal)]
    async fn device_connected(
        emitter: &SignalEmitter<'_>,
        info: HashMap<String, String>,
    ) -> zbus::Result<()>;

    /// Emitted when the USB device disconnects.
    #[zbus(signal)]
    async fn device_disconnected(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    /// Emitted on non-fatal errors (render failure, sensor failure, etc.).
    #[zbus(signal)]
    async fn error(emitter: &SignalEmitter<'_>, message: &str) -> zbus::Result<()>;
}

/// Register and start the D-Bus service on the session bus.
///
/// Returns the active connection (must be kept alive for the service to remain registered).
pub async fn serve(state: Arc<Mutex<ServiceState>>) -> anyhow::Result<zbus::Connection> {
    let iface = DisplayInterface::new(state);
    let connection = zbus::connection::Builder::session()?
        .name("com.thermalwriter.Service")?
        .serve_at("/com/thermalwriter/display", iface)?
        .build()
        .await?;

    info!("D-Bus service registered: com.thermalwriter.Service at /com/thermalwriter/display");
    Ok(connection)
}
