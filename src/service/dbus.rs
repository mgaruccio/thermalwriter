// D-Bus interface: exposes service control via com.thermalwriter.Display.
// Methods: set_layout, get_status, list_layouts, list_sensors, stop, reload.
// Properties: active_layout, connected, resolution, tick_rate.
// Signals: layout_changed, device_connected, device_disconnected, error.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, watch};
use zbus::{interface, object_server::SignalEmitter};
use log::info;

/// Message sent through the mode change channel to switch display modes.
#[derive(Debug, Clone)]
pub enum ModeChange {
    /// Switch to an SVG or HTML layout by name.
    Layout(String),
    /// Switch to xvfb capture mode with the given shell command.
    Xvfb { command: String },
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
    /// Notify the daemon to switch display mode or layout.
    pub mode_change_tx: tokio::sync::mpsc::Sender<ModeChange>,
}

pub struct DisplayInterface {
    state: Arc<Mutex<ServiceState>>,
}

impl DisplayInterface {
    pub fn new(state: Arc<Mutex<ServiceState>>) -> Self {
        Self { state }
    }
}

#[interface(name = "com.thermalwriter.Display")]
impl DisplayInterface {
    /// Switch the active layout. Returns an error if the layout file doesn't exist.
    async fn set_layout(
        &self,
        name: String,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<String> {
        // Hold the lock through both the channel send and state update — no TOCTOU window.
        // tokio::sync::Mutex is safe to hold across .await.
        let mut state = self.state.lock().await;
        let layout_path = state.layout_dir.join(&name);
        if !layout_path.exists() {
            return Err(zbus::fdo::Error::InvalidArgs(
                format!("Layout not found: {}", name)
            ));
        }
        state.mode_change_tx.send(ModeChange::Layout(name.clone())).await
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
                let layout_path = state.layout_dir.join(&command);
                if !layout_path.exists() {
                    return Err(zbus::fdo::Error::InvalidArgs(
                        format!("Layout not found: {}", command)
                    ));
                }
                ModeChange::Layout(command.clone())
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

    /// Return sorted list of available layout filenames from the layout directory.
    async fn list_layouts(&self) -> Vec<String> {
        let state = self.state.lock().await;
        let mut layouts = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&state.layout_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext == "html") {
                    if let Some(name) = path.file_name() {
                        layouts.push(name.to_string_lossy().to_string());
                    }
                }
            }
        }
        layouts.sort();
        layouts
    }

    /// Return available sensor keys. Placeholder — wired up in tick loop integration.
    async fn list_sensors(&self) -> Vec<String> {
        Vec::new()
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
