//! Lightweight StatusNotifierItem tray for thermalwriter.
//!
//! Talks to the running daemon over the session bus (`DisplayProxy`) and
//! launches the optional Tauri GUI as a separate process. Idle path is pure
//! D-Bus event-driven — no timers, no WebKit.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use ksni::TrayMethods;
use log::{debug, error, info, warn};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

/// Session-bus proxy for `com.thermalwriter.Display`.
///
/// Mirrors `thermalwriter::dbus_types::Display` — only the methods the tray
/// needs. Keep signatures aligned with `src/dbus_types.rs`.
#[zbus::proxy(
    interface = "com.thermalwriter.Display",
    default_service = "com.thermalwriter.Service",
    default_path = "/com/thermalwriter/display"
)]
trait Display {
    async fn get_status(&self) -> zbus::Result<HashMap<String, String>>;
    async fn set_layout(&self, name: &str) -> zbus::Result<String>;
    async fn list_layouts(&self) -> zbus::Result<Vec<String>>;
    async fn stop(&self) -> zbus::Result<()>;
    async fn reload(&self) -> zbus::Result<()>;
    async fn start_stream_preset(&self, preset: &str) -> zbus::Result<String>;
}

/// Embedded tray icons (ARGB32), generated once from PNG assets.
static ICONS: LazyLock<Vec<ksni::Icon>> = LazyLock::new(|| {
    [
        include_bytes!("../icons/icon-32.png").as_slice(),
        include_bytes!("../icons/icon-64.png").as_slice(),
    ]
    .into_iter()
    .filter_map(|bytes| match load_argb_icon(bytes) {
        Ok(icon) => Some(icon),
        Err(err) => {
            eprintln!("thermalwriter-tray: failed to decode embedded icon: {err}");
            None
        }
    })
    .collect()
});

fn load_argb_icon(bytes: &[u8]) -> Result<ksni::Icon> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().context("png header")?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).context("png frame")?;
    let mut rgba = buf[..info.buffer_size()].to_vec();

    // ksni wants ARGB32; PNG is RGBA8.
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.rotate_right(1);
    }

    Ok(ksni::Icon {
        width: info.width as i32,
        height: info.height as i32,
        data: rgba,
    })
}

#[derive(Debug, Clone, Default)]
struct DaemonStatus {
    online: bool,
    active_layout: String,
    mode: String,
    connected: bool,
    resolution: String,
    tick_rate: String,
    layouts: Vec<String>,
    error: Option<String>,
}

impl DaemonStatus {
    fn from_maps(
        status: HashMap<String, String>,
        layouts: Vec<String>,
    ) -> Self {
        Self {
            online: true,
            active_layout: status
                .get("active_layout")
                .cloned()
                .unwrap_or_default(),
            mode: status.get("mode").cloned().unwrap_or_default(),
            connected: status
                .get("connected")
                .map(|v| v == "true")
                .unwrap_or(false),
            resolution: status.get("resolution").cloned().unwrap_or_default(),
            tick_rate: status.get("tick_rate").cloned().unwrap_or_default(),
            layouts,
            error: None,
        }
    }

    fn offline(err: impl Into<String>) -> Self {
        Self {
            online: false,
            error: Some(err.into()),
            ..Default::default()
        }
    }

    fn tooltip_title(&self) -> String {
        if !self.online {
            return "Thermalwriter (offline)".into();
        }
        if self.connected {
            "Thermalwriter".into()
        } else {
            "Thermalwriter (no device)".into()
        }
    }

    fn tooltip_body(&self) -> String {
        if let Some(err) = &self.error {
            return format!("Daemon offline\n{err}");
        }
        let link = if self.connected {
            "connected"
        } else {
            "disconnected"
        };
        let layout = if self.active_layout.is_empty() {
            "—"
        } else {
            &self.active_layout
        };
        let mode = if self.mode.is_empty() {
            "—"
        } else {
            &self.mode
        };
        let fps = if self.tick_rate.is_empty() {
            "—"
        } else {
            &self.tick_rate
        };
        let res = if self.resolution.is_empty() {
            "—"
        } else {
            &self.resolution
        };
        format!("Layout: {layout}\nMode: {mode}\nDevice: {link}\n{res} @ {fps} FPS")
    }
}

/// Messages from tray menu callbacks → async main loop.
enum Action {
    OpenGui,
    SetLayout(String),
    StreamPreset(&'static str),
    ReturnToLayout,
    Reload,
    StopDaemon,
    Refresh,
    Quit,
}

struct ThermalTray {
    status: DaemonStatus,
    tx: UnboundedSender<Action>,
}

impl ThermalTray {
    fn request(&self, action: Action) {
        if let Err(err) = self.tx.send(action) {
            error!("tray action dropped: {err}");
        }
    }
}

impl ksni::Tray for ThermalTray {
    const MENU_ON_ACTIVATE: bool = false;

    fn id(&self) -> String {
        "thermalwriter-tray".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }

    fn title(&self) -> String {
        self.status.tooltip_title()
    }

    fn status(&self) -> ksni::Status {
        if !self.status.online {
            ksni::Status::NeedsAttention
        } else if self.status.connected {
            ksni::Status::Active
        } else {
            ksni::Status::Passive
        }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        ICONS.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.status.tooltip_title(),
            description: self.status.tooltip_body(),
            icon_pixmap: ICONS.clone(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.request(Action::OpenGui);
    }

    fn menu_about_to_show(&mut self) {
        // Non-blocking: refresh caches for the *next* open / tooltip update.
        self.request(Action::Refresh);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;

        let mut items: Vec<MenuItem<Self>> = Vec::new();

        // Status header (disabled label)
        items.push(
            StandardItem {
                label: status_header(&self.status),
                enabled: false,
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);

        items.push(
            StandardItem {
                label: "Open Config…".into(),
                icon_name: "preferences-system".into(),
                activate: Box::new(|this: &mut Self| this.request(Action::OpenGui)),
                ..Default::default()
            }
            .into(),
        );

        // Layouts submenu
        let layout_items: Vec<MenuItem<Self>> = if self.status.layouts.is_empty() {
            vec![
                StandardItem {
                    label: if self.status.online {
                        "(no layouts found)".into()
                    } else {
                        "(daemon offline)".into()
                    },
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            ]
        } else {
            self.status
                .layouts
                .iter()
                .map(|name| {
                    let label = name.clone();
                    let checked = name == &self.status.active_layout
                        && self.status.mode != "xvfb";
                    let layout_name = name.clone();
                    StandardItem {
                        label,
                        icon_name: if checked {
                            "object-select-symbolic".into()
                        } else {
                            String::new()
                        },
                        activate: Box::new(move |this: &mut Self| {
                            this.request(Action::SetLayout(layout_name.clone()));
                        }),
                        ..Default::default()
                    }
                    .into()
                })
                .collect()
        };
        items.push(
            SubMenu {
                label: "Layouts".into(),
                submenu: layout_items,
                enabled: self.status.online,
                ..Default::default()
            }
            .into(),
        );

        // Stream presets
        let streaming = self.status.mode == "xvfb";
        let mut stream_items: Vec<MenuItem<Self>> = ["conky", "cava", "btop"]
            .into_iter()
            .map(|preset| {
                StandardItem {
                    label: format!("Start {preset}"),
                    enabled: self.status.online,
                    activate: Box::new(move |this: &mut Self| {
                        this.request(Action::StreamPreset(preset));
                    }),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        if streaming {
            stream_items.push(MenuItem::Separator);
            stream_items.push(
                StandardItem {
                    label: "Return to layout".into(),
                    enabled: self.status.online && !self.status.active_layout.is_empty(),
                    activate: Box::new(|this: &mut Self| this.request(Action::ReturnToLayout)),
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(
            SubMenu {
                label: if streaming {
                    "Stream (active)".into()
                } else {
                    "Stream".into()
                },
                submenu: stream_items,
                enabled: self.status.online,
                ..Default::default()
            }
            .into(),
        );

        items.push(MenuItem::Separator);

        items.push(
            StandardItem {
                label: "Reload config".into(),
                icon_name: "view-refresh".into(),
                enabled: self.status.online,
                activate: Box::new(|this: &mut Self| this.request(Action::Reload)),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Refresh status".into(),
                enabled: true,
                activate: Box::new(|this: &mut Self| this.request(Action::Refresh)),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Stop daemon".into(),
                icon_name: "process-stop".into(),
                enabled: self.status.online,
                activate: Box::new(|this: &mut Self| this.request(Action::StopDaemon)),
                ..Default::default()
            }
            .into(),
        );

        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit tray".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|this: &mut Self| this.request(Action::Quit)),
                ..Default::default()
            }
            .into(),
        );

        items
    }
}

fn status_header(status: &DaemonStatus) -> String {
    if !status.online {
        return "Daemon · Offline".into();
    }
    let link = if status.connected { "On" } else { "No LCD" };
    let layout = if status.active_layout.is_empty() {
        "—".into()
    } else {
        status.active_layout.clone()
    };
    if status.mode == "xvfb" {
        format!("Stream · {link}")
    } else {
        format!("{layout} · {link}")
    }
}

/// Session-bus client. Reconnects on each call so a daemon restart is fine.
struct DaemonClient;

impl DaemonClient {
    async fn proxy(
        connection: &zbus::Connection,
    ) -> Result<DisplayProxy<'_>> {
        DisplayProxy::new(connection)
            .await
            .context("thermalwriter Display proxy")
    }

    async fn connection() -> Result<zbus::Connection> {
        zbus::Connection::session()
            .await
            .context("session bus")
    }

    async fn fetch_status() -> DaemonStatus {
        match Self::connection().await {
            Ok(conn) => match Self::proxy(&conn).await {
                Ok(proxy) => match proxy.get_status().await {
                    Ok(status) => {
                        let layouts = proxy.list_layouts().await.unwrap_or_default();
                        DaemonStatus::from_maps(status, layouts)
                    }
                    Err(err) => DaemonStatus::offline(err.to_string()),
                },
                Err(err) => DaemonStatus::offline(format!("{err:#}")),
            },
            Err(err) => DaemonStatus::offline(format!("{err:#}")),
        }
    }

    async fn set_layout(name: &str) -> Result<String> {
        let conn = Self::connection().await?;
        let proxy = Self::proxy(&conn).await?;
        proxy
            .set_layout(name)
            .await
            .context("set_layout")
    }

    async fn start_stream_preset(preset: &str) -> Result<String> {
        let conn = Self::connection().await?;
        let proxy = Self::proxy(&conn).await?;
        proxy
            .start_stream_preset(preset)
            .await
            .context("start_stream_preset")
    }

    async fn reload() -> Result<()> {
        let conn = Self::connection().await?;
        let proxy = Self::proxy(&conn).await?;
        proxy.reload().await.context("reload")
    }

    async fn stop() -> Result<()> {
        let conn = Self::connection().await?;
        let proxy = Self::proxy(&conn).await?;
        proxy.stop().await.context("stop")
    }
}

async fn apply_status(handle: &ksni::Handle<ThermalTray>, status: DaemonStatus) {
    let _ = handle
        .update(|tray| {
            tray.status = status;
        })
        .await;
}

fn open_gui() {
    match find_gui_command() {
        Some((program, args)) => {
            info!("launching GUI: {} {:?}", program.display(), args);
            match std::process::Command::new(&program)
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(_) => {}
                Err(err) => error!("failed to launch GUI {}: {err}", program.display()),
            }
        }
        None => {
            error!(
                "could not find thermalwriter-gui; install the Config GUI or set THERMALWRITER_GUI"
            );
        }
    }
}

/// Resolve the GUI launcher.
///
/// Order: `THERMALWRITER_GUI` (absolute path or `cmd arg…`), then `thermalwriter-gui`
/// on `PATH`, then common install locations.
fn find_gui_command() -> Option<(PathBuf, Vec<String>)> {
    if let Ok(raw) = std::env::var("THERMALWRITER_GUI") {
        let raw = raw.trim();
        if !raw.is_empty() {
            let mut parts = shell_split(raw);
            if let Some(program) = parts.first().map(PathBuf::from) {
                let args = parts.drain(1..).collect();
                return Some((program, args));
            }
        }
    }

    if let Some(path) = which("thermalwriter-gui") {
        return Some((path, Vec::new()));
    }

    let home = dirs_home();
    let candidates = [
        home.as_ref()
            .map(|h| h.join(".cargo/bin/thermalwriter-gui")),
        home.as_ref()
            .map(|h| h.join(".local/bin/thermalwriter-gui")),
        Some(PathBuf::from("/usr/bin/thermalwriter-gui")),
        Some(PathBuf::from("/usr/local/bin/thermalwriter-gui")),
    ];
    for path in candidates.into_iter().flatten() {
        if path.is_file() && is_executable(&path) {
            return Some((path, Vec::new()));
        }
    }
    None
}

fn shell_split(s: &str) -> Vec<String> {
    // Minimal whitespace split — enough for `THERMALWRITER_GUI="/path/AppImage"`.
    s.split_whitespace().map(|p| p.to_string()).collect()
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() && is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

async fn handle_action(action: Action, handle: &ksni::Handle<ThermalTray>) -> bool {
    match action {
        Action::Quit => {
            info!("quit requested");
            return false;
        }
        Action::OpenGui => {
            open_gui();
        }
        Action::Refresh => {
            apply_status(handle, DaemonClient::fetch_status().await).await;
        }
        Action::SetLayout(name) => {
            match DaemonClient::set_layout(&name).await {
                Ok(msg) => info!("set_layout: {msg}"),
                Err(err) => error!("set_layout failed: {err:#}"),
            }
            apply_status(handle, DaemonClient::fetch_status().await).await;
        }
        Action::StreamPreset(preset) => {
            match DaemonClient::start_stream_preset(preset).await {
                Ok(msg) => info!("stream {preset}: {msg}"),
                Err(err) => error!("stream {preset} failed: {err:#}"),
            }
            apply_status(handle, DaemonClient::fetch_status().await).await;
        }
        Action::ReturnToLayout => {
            let current = DaemonClient::fetch_status().await;
            if current.active_layout.is_empty() {
                error!("no active_layout to restore");
            } else {
                match DaemonClient::set_layout(&current.active_layout).await {
                    Ok(msg) => info!("return to layout: {msg}"),
                    Err(err) => error!("return to layout failed: {err:#}"),
                }
            }
            apply_status(handle, DaemonClient::fetch_status().await).await;
        }
        Action::Reload => {
            match DaemonClient::reload().await {
                Ok(()) => info!("daemon reloaded"),
                Err(err) => error!("reload failed: {err:#}"),
            }
            apply_status(handle, DaemonClient::fetch_status().await).await;
        }
        Action::StopDaemon => {
            match DaemonClient::stop().await {
                Ok(()) => info!("daemon stop requested"),
                Err(err) => error!("stop failed: {err:#}"),
            }
            apply_status(handle, DaemonClient::fetch_status().await).await;
        }
    }
    true
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let (tx, mut rx): (UnboundedSender<Action>, UnboundedReceiver<Action>) =
        mpsc::unbounded_channel();

    let initial = DaemonClient::fetch_status().await;
    if initial.online {
        info!(
            "daemon online (layout={}, mode={}, connected={})",
            initial.active_layout, initial.mode, initial.connected
        );
    } else {
        warn!(
            "daemon offline at startup: {}",
            initial.error.as_deref().unwrap_or("unknown")
        );
    }

    let tray = ThermalTray {
        status: initial,
        tx,
    };

    let handle = tray
        .spawn()
        .await
        .context("failed to register StatusNotifierItem (is a tray host running?)")?;

    info!("tray registered");

    while let Some(action) = rx.recv().await {
        debug!("action: {action:?}");
        if !handle_action(action, &handle).await {
            break;
        }
    }

    // Dropping the handle unregisters the tray.
    drop(handle);
    Ok(())
}

// Manual Debug for Action so StreamPreset &'static str is fine.
impl std::fmt::Debug for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Action::OpenGui => write!(f, "OpenGui"),
            Action::SetLayout(n) => write!(f, "SetLayout({n})"),
            Action::StreamPreset(p) => write!(f, "StreamPreset({p})"),
            Action::ReturnToLayout => write!(f, "ReturnToLayout"),
            Action::Reload => write!(f, "Reload"),
            Action::StopDaemon => write!(f, "StopDaemon"),
            Action::Refresh => write!(f, "Refresh"),
            Action::Quit => write!(f, "Quit"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_embedded_icons() {
        assert!(!ICONS.is_empty(), "at least one icon should decode");
        for icon in ICONS.iter() {
            assert!(icon.width > 0 && icon.height > 0);
            assert_eq!(icon.data.len(), (icon.width * icon.height * 4) as usize);
        }
    }

    #[test]
    fn offline_tooltip() {
        let s = DaemonStatus::offline("connection refused");
        assert!(s.tooltip_title().contains("offline"));
        assert!(s.tooltip_body().contains("connection refused"));
    }

    #[test]
    fn online_tooltip() {
        let s = DaemonStatus {
            online: true,
            active_layout: "neon-dash.svg".into(),
            mode: "svg".into(),
            connected: true,
            resolution: "480x480".into(),
            tick_rate: "2".into(),
            layouts: vec![],
            error: None,
        };
        assert_eq!(s.tooltip_title(), "Thermalwriter");
        let body = s.tooltip_body();
        assert!(body.contains("neon-dash.svg"));
        assert!(body.contains("480x480"));
    }
}
