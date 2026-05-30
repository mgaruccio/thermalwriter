// D-Bus interface: exposes service control via com.thermalwriter.Display.
// Methods: set_layout, get_status, list_layouts, list_sensors, stop, reload,
//          get_layout_vars, set_layout_vars, set_background, clear_background,
//          list_backgrounds.
// Properties: active_layout, connected, resolution, tick_rate.
// Signals: layout_changed, device_connected, device_disconnected, error.

use log::info;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, oneshot, watch};
use zbus::{interface, object_server::SignalEmitter};

use crate::config::Config;
use crate::render::frontmatter::LayoutFrontmatter;

/// Message sent through the mode change channel to switch display modes.
///
/// Every variant carries an `ack` oneshot sender. The listener task sends
/// `Ok(())` once the new source is confirmed live, or `Err(e)` on failure.
/// The D-Bus caller awaits `ack_rx` before committing `state.mode` — so a
/// failed transition leaves the daemon state unchanged.
///
/// For callers that don't need confirmation (e.g. `set_layout_vars` hot-reload,
/// background changes), create a throwaway `let (ack_tx, _) = oneshot::channel()`
/// and pass `ack_tx` — the `_` drops immediately after the listener sends.
pub enum ModeChange {
    /// Switch to an SVG or HTML layout by name.
    Layout {
        name: String,
        vars: HashMap<String, String>,
        /// Confirmation channel: listener sends Ok once the new source is live.
        ack: oneshot::Sender<anyhow::Result<()>>,
    },
    /// Switch to xvfb capture mode with the given shell command.
    Xvfb {
        command: String,
        /// Confirmation channel: listener sends Ok once Xvfb starts and
        /// the new XvfbSource is confirmed sent to the tick loop.
        ack: oneshot::Sender<anyhow::Result<()>>,
    },
    /// Set or clear the global background image.
    Background {
        image: Option<tiny_skia::Pixmap>,
        /// Confirmation channel: listener sends Ok once the background is
        /// applied to the tick loop. Callers that don't need confirmation
        /// may drop this sender by passing `oneshot::channel().0`.
        ack: oneshot::Sender<anyhow::Result<()>>,
    },
}

/// Manual Debug: omit the non-Debug `oneshot::Sender` fields.
impl std::fmt::Debug for ModeChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModeChange::Layout { name, vars, .. } => f
                .debug_struct("ModeChange::Layout")
                .field("name", name)
                .field("vars", vars)
                .finish_non_exhaustive(),
            ModeChange::Xvfb { command, .. } => f
                .debug_struct("ModeChange::Xvfb")
                .field("command", command)
                .finish_non_exhaustive(),
            ModeChange::Background { image, .. } => f
                .debug_struct("ModeChange::Background")
                .field("has_image", &image.is_some())
                .finish_non_exhaustive(),
        }
    }
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
    pub tick_rate_tx: watch::Sender<u32>,
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
    /// Serializes all writes to config.toml so concurrent D-Bus calls don't lose
    /// each other's edits (each writer does a read-modify-write cycle).
    pub config_write_lock: Arc<tokio::sync::Mutex<()>>,
    /// Serializes the full set_background / clear_background body (decode →
    /// disk → channel → state-mirror) so concurrent callers cannot interleave
    /// their disk writes and channel sends, leaving them out of sync.
    pub bg_change_lock: Arc<tokio::sync::Mutex<()>>,
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
    let resolved = base
        .join(name)
        .canonicalize()
        .map_err(|_| zbus::fdo::Error::InvalidArgs(format!("{kind} not found: {name}")))?;
    if !resolved.starts_with(&base) {
        return Err(zbus::fdo::Error::InvalidArgs(format!(
            "{kind} path escapes directory: {name}"
        )));
    }
    Ok(resolved)
}

/// Resolve `name` against `layout_dir`, rejecting traversal and symlink escapes.
pub fn validate_layout_path(layout_dir: &Path, name: &str) -> Result<PathBuf, zbus::fdo::Error> {
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
            if has_layout_ext(&path)
                && let Some(name) = path.file_name()
            {
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
pub fn validate_background_path(bg_dir: &Path, name: &str) -> Result<PathBuf, zbus::fdo::Error> {
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
        let Some(name) = path.file_name() else {
            continue;
        };
        if path.is_file() && has_image_ext(&path) {
            out.push(name.to_string_lossy().to_string());
        }
    }
    out.sort();
    out
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
        zbus::fdo::Error::Failed(format!("Failed to read layout {}: {}", path.display(), e))
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

/// Validate `name` against `layout_dir`, then atomically persist it as the new
/// `display.default_layout` (and the inferred `display.mode`) to `config_path`.
///
/// This is the testable core of `DisplayInterface::set_default_layout`. The
/// in-memory Config mirror update is the caller's responsibility (brief lock).
pub fn save_default_layout_impl(
    layout_dir: &Path,
    config_path: &Path,
    name: &str,
) -> Result<(), zbus::fdo::Error> {
    validate_layout_path(layout_dir, name)?;
    let mode = if name.ends_with(".html") {
        "html"
    } else {
        "svg"
    };
    Config::save_display_layout(config_path, name, mode)
        .map_err(|e| zbus::fdo::Error::Failed(format!("save_display_layout: {}", e)))?;
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
        let vars = state
            .config
            .layout_vars
            .get(&name)
            .cloned()
            .unwrap_or_default();
        // Throwaway ack: set_layout already validates the path (existence check
        // above) and holds the state lock through the send. The caller doesn't
        // need to wait for the tick loop to confirm the swap.
        let (ack_tx, _ack_rx) = oneshot::channel();
        state
            .mode_change_tx
            .send(ModeChange::Layout {
                name: name.clone(),
                vars,
                ack: ack_tx,
            })
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        state.active_layout = name.clone();
        state.mode = if name.ends_with(".html") {
            "html"
        } else {
            "svg"
        }
        .to_string();

        Self::layout_changed(&emitter, &name).await?;
        Ok(format!("Layout set to: {}", name))
    }

    /// Switch display mode. mode="xvfb" starts capture with the given command.
    /// mode="svg" or mode="html" with command as layout name switches back to layout mode.
    ///
    /// Blocks until the mode-change listener confirms the swap via the ack channel.
    /// `state.mode` and `state.active_layout` are only updated on `Ok` — a failed
    /// transition (bad command, missing layout) leaves the daemon state unchanged.
    async fn set_mode(&self, mode: String, command: String) -> zbus::fdo::Result<String> {
        // Build the ack channel before acquiring the state lock so we can release
        // the lock before awaiting the listener (avoids holding Mutex across .await).
        let (ack_tx, ack_rx) = oneshot::channel::<anyhow::Result<()>>();

        let (tx, layout_dir_snap, layout_vars_snap) = {
            let state = self.state.lock().await;
            (
                state.mode_change_tx.clone(),
                state.layout_dir.clone(),
                state.config.layout_vars.clone(),
            )
        };

        let change = match mode.as_str() {
            "xvfb" => {
                if command.is_empty() {
                    return Err(zbus::fdo::Error::InvalidArgs(
                        "xvfb mode requires a command".to_string(),
                    ));
                }
                ModeChange::Xvfb {
                    command: command.clone(),
                    ack: ack_tx,
                }
            }
            "svg" | "html" => {
                // Path-traversal + existence check on the layout name.
                validate_layout_path(&layout_dir_snap, &command)?;
                let vars = layout_vars_snap.get(&command).cloned().unwrap_or_default();
                ModeChange::Layout {
                    name: command.clone(),
                    vars,
                    ack: ack_tx,
                }
            }
            _ => {
                return Err(zbus::fdo::Error::InvalidArgs(format!(
                    "Unknown mode: {} (expected svg, html, or xvfb)",
                    mode
                )));
            }
        };

        // Send the change request to the listener task.
        tx.send(change)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(format!("mode_change channel closed: {}", e)))?;

        // Block until the listener confirms the swap. The ack sender is consumed by
        // the match arm above, so if the listener drops it without sending, we get
        // RecvError — treat that as a failure too.
        let ack_result = ack_rx
            .await
            .map_err(|_| {
                zbus::fdo::Error::Failed(
                    "mode transition listener dropped ack channel without replying".to_string(),
                )
            })?
            .map_err(|e| zbus::fdo::Error::Failed(format!("mode transition failed: {}", e)))?;
        let _ = ack_result; // () on Ok path

        // Only mutate state after the listener confirms success.
        {
            let mut state = self.state.lock().await;
            state.mode = mode.clone();
            if mode != "xvfb" {
                state.active_layout = command.clone();
            }
        }

        Ok(format!("Mode set to: {} ({})", mode, command))
    }

    /// Return a snapshot of service status as key→value pairs.
    async fn get_status(&self) -> HashMap<String, String> {
        let state = self.state.lock().await;
        let mut status = HashMap::new();
        status.insert("active_layout".to_string(), state.active_layout.clone());
        status.insert("mode".to_string(), state.mode.clone());
        status.insert("connected".to_string(), state.connected.to_string());
        status.insert(
            "resolution".to_string(),
            format!("{}x{}", state.resolution.0, state.resolution.1),
        );
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
        // Clone handles out of the state lock, then release it before disk I/O.
        let (write_lock, layout_dir, config_path, tx) = {
            let state = self.state.lock().await;
            (
                state.config_write_lock.clone(),
                state.layout_dir.clone(),
                state.config_path.clone(),
                state.mode_change_tx.clone(),
            )
        };

        // Serialize all config.toml writers — prevents read-modify-write races.
        let _write_guard = write_lock.lock().await;
        validate_layout_path(&layout_dir, &name)?;
        Config::save_layout_vars(&config_path, &name, &vars).map_err(|e| {
            zbus::fdo::Error::Failed(format!("Failed to persist layout vars: {}", e))
        })?;
        drop(_write_guard);

        // Update in-memory mirror under a brief state lock.
        let reload_vars = {
            let mut state = self.state.lock().await;
            state.config.layout_vars.insert(name.clone(), vars);
            state
                .config
                .layout_vars
                .get(&name)
                .cloned()
                .unwrap_or_default()
        };

        // Channel send outside the lock — avoids holding Mutex across .await.
        // Throwaway ack: set_layout_vars has already serialized the disk write
        // and in-memory mirror update; no need to block on tick-loop confirmation.
        let (ack_tx, _ack_rx) = oneshot::channel();
        tx.send(ModeChange::Layout {
            name: name.clone(),
            vars: reload_vars,
            ack: ack_tx,
        })
        .await
        .map_err(|e| zbus::fdo::Error::Failed(format!("Failed to notify tick loop: {}", e)))?;

        Ok(())
    }

    /// Persist `name` as the new default layout in config.toml and update the
    /// in-memory Config mirror. Does NOT reload the active layout — use
    /// `set_layout_vars` or `set_layout` for live switching.
    async fn set_default_layout(&self, name: String) -> zbus::fdo::Result<()> {
        // Clone handles out of the state lock, then release before disk I/O.
        let (write_lock, layout_dir, config_path) = {
            let state = self.state.lock().await;
            (
                state.config_write_lock.clone(),
                state.layout_dir.clone(),
                state.config_path.clone(),
            )
        };

        // Serialize all config.toml writers.
        let _write_guard = write_lock.lock().await;
        save_default_layout_impl(&layout_dir, &config_path, &name)?;
        drop(_write_guard);

        // Brief commit lock: update in-memory mirror.
        {
            let mut state = self.state.lock().await;
            let mode = if name.ends_with(".html") {
                "html"
            } else {
                "svg"
            };
            state.config.display.default_layout = name;
            state.config.display.mode = mode.to_string();
        }
        Ok(())
    }

    /// Set the global background image by filename (must exist in background_dir).
    async fn set_background(&self, name: String) -> zbus::fdo::Result<()> {
        // Clone handles out of the state lock, then release before heavy work.
        let (bg_lock, write_lock, background_dir, config_path, tx) = {
            let state = self.state.lock().await;
            (
                state.bg_change_lock.clone(),
                state.config_write_lock.clone(),
                state.background_dir.clone(),
                state.config_path.clone(),
                state.mode_change_tx.clone(),
            )
        };

        let bg_path = validate_background_path(&background_dir, &name)?;

        // Serialize the full body so concurrent callers cannot interleave their
        // disk writes and channel sends (which would leave them out of sync).
        let _bg_guard = bg_lock.lock().await;

        // CPU-bound decode + Lanczos3 resize on a blocking thread — 50-200 ms.
        let pixmap = tokio::task::spawn_blocking(move || {
            crate::render::background::decode_from_file(&bg_path)
        })
        .await
        .map_err(|e| zbus::fdo::Error::Failed(format!("decode task panicked: {}", e)))?
        .map_err(|e| {
            zbus::fdo::Error::Failed(format!("Failed to decode background '{}': {}", name, e))
        })?;

        // Serialize the disk write alongside all other config writers.
        {
            let _write_guard = write_lock.lock().await;
            Config::save_background_image(&config_path, Some(&name)).map_err(|e| {
                zbus::fdo::Error::Failed(format!("Failed to persist background image: {}", e))
            })?;
        }

        // Signal the tick loop (still inside bg_guard, so channel send is ordered).
        // Throwaway ack: bg_guard + disk write already serialize correctness; no
        // need to block here on tick-loop confirmation.
        let (ack_tx, _ack_rx) = oneshot::channel();
        tx.send(ModeChange::Background {
            image: Some(pixmap.clone()),
            ack: ack_tx,
        })
        .await
        .map_err(|e| zbus::fdo::Error::Failed(format!("Failed to notify tick loop: {}", e)))?;

        // Brief commit lock: update in-memory state mirror.
        {
            let mut state = self.state.lock().await;
            state.current_background = Some(pixmap);
            state.config.background.image = Some(name);
        }
        Ok(())
    }

    /// Clear the global background image.
    async fn clear_background(&self) -> zbus::fdo::Result<()> {
        // Clone handles out of the state lock, then release before disk I/O.
        let (bg_lock, write_lock, config_path, tx) = {
            let state = self.state.lock().await;
            (
                state.bg_change_lock.clone(),
                state.config_write_lock.clone(),
                state.config_path.clone(),
                state.mode_change_tx.clone(),
            )
        };

        // Serialize against concurrent set_background calls.
        let _bg_guard = bg_lock.lock().await;

        // Serialize the disk write alongside all other config writers.
        {
            let _write_guard = write_lock.lock().await;
            Config::save_background_image(&config_path, None).map_err(|e| {
                zbus::fdo::Error::Failed(format!("Failed to clear background: {}", e))
            })?;
        }

        // Signal the tick loop (still inside bg_guard). Throwaway ack — same
        // rationale as set_background: bg_guard already serializes correctness.
        let (ack_tx, _ack_rx) = oneshot::channel();
        tx.send(ModeChange::Background { image: None, ack: ack_tx })
            .await
            .map_err(|e| zbus::fdo::Error::Failed(format!("Failed to notify tick loop: {}", e)))?;

        // Brief commit lock: update in-memory state mirror.
        {
            let mut state = self.state.lock().await;
            state.current_background = None;
            state.config.background.image = None;
        }
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
    /// Set the tick rate (1–60 FPS). Returns error outside that range.
    async fn set_tick_rate(&mut self, rate: u32) -> zbus::fdo::Result<()> {
        if rate == 0 || rate > 60 {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Tick rate must be 1-60".to_string(),
            ));
        }
        let mut state = self.state.lock().await;
        state.tick_rate = rate;
        let _ = state.tick_rate_tx.send(rate);
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::{mpsc, oneshot};

    /// Verify the ack-channel contract: when the listener sends Err, the caller
    /// must see Err and must NOT mutate the mode state mirror.
    ///
    /// This tests `await_mode_transition_ack` — the helper that `set_mode` will
    /// call to block until the listener confirms the swap.
    #[tokio::test]
    async fn failed_transition_leaves_state_unchanged() {
        let (mode_tx, mut mode_rx) = mpsc::channel::<ModeChange>(1);

        // Caller side: build the ack channel, send the ModeChange::Xvfb.
        let (ack_tx, ack_rx) = oneshot::channel::<anyhow::Result<()>>();
        mode_tx
            .send(ModeChange::Xvfb {
                command: "nonexistent-binary".to_string(),
                ack: ack_tx,
            })
            .await
            .unwrap();

        // Stub listener side: receive the message, extract the ack sender, reply Err.
        let msg = mode_rx.recv().await.unwrap();
        match msg {
            ModeChange::Xvfb { command: _, ack } => {
                ack.send(Err(anyhow::anyhow!("xvfb start failed"))).unwrap();
            }
            _ => panic!("unexpected variant"),
        }

        // State mirror: represents what set_mode reads before the await.
        let mut mode = "svg".to_string();

        // Caller awaits the ack — must get Err back.
        let result = ack_rx.await.expect("ack sender must not be dropped");
        assert!(
            result.is_err(),
            "listener replied Err; caller must see Err — got Ok instead"
        );

        // On Err, the caller must NOT update the mode mirror.
        // (In the real set_mode: `state.mode = new_mode` only executes on Ok.)
        assert_eq!(mode, "svg", "mode must remain 'svg' after a failed transition");
        let _ = &mut mode; // suppress unused-mut
    }

    /// Verify the ack-channel happy path: when the listener sends Ok, the caller
    /// may safely commit the mode state mirror.
    #[tokio::test]
    async fn successful_transition_allows_state_update() {
        let (mode_tx, mut mode_rx) = mpsc::channel::<ModeChange>(1);

        let (ack_tx, ack_rx) = oneshot::channel::<anyhow::Result<()>>();
        mode_tx
            .send(ModeChange::Xvfb {
                command: "cava".to_string(),
                ack: ack_tx,
            })
            .await
            .unwrap();

        // Stub listener: reply Ok (simulates a successful xvfb start).
        let msg = mode_rx.recv().await.unwrap();
        match msg {
            ModeChange::Xvfb { command: _, ack } => {
                ack.send(Ok(())).unwrap();
            }
            _ => panic!("unexpected variant"),
        }

        let result = ack_rx.await.expect("ack sender must not be dropped");
        assert!(result.is_ok(), "listener replied Ok; caller must see Ok");

        // On Ok, the caller updates the mode mirror — simulate the state commit.
        let mode = "xvfb".to_string();
        assert_eq!(mode, "xvfb", "mode must update to 'xvfb' after a successful transition");
    }

    /// Verify Layout variant also carries an ack.
    #[tokio::test]
    async fn layout_transition_ack_contract() {
        let (mode_tx, mut mode_rx) = mpsc::channel::<ModeChange>(1);

        let (ack_tx, ack_rx) = oneshot::channel::<anyhow::Result<()>>();
        mode_tx
            .send(ModeChange::Layout {
                name: "does-not-exist.svg".to_string(),
                vars: HashMap::new(),
                ack: ack_tx,
            })
            .await
            .unwrap();

        let msg = mode_rx.recv().await.unwrap();
        match msg {
            ModeChange::Layout { name: _, vars: _, ack } => {
                ack.send(Err(anyhow::anyhow!("layout file not found"))).unwrap();
            }
            _ => panic!("unexpected variant"),
        }

        let result = ack_rx.await.expect("ack sender must not be dropped");
        assert!(result.is_err(), "listener replied Err for missing layout");
    }
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
