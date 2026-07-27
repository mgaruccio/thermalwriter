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
/// Multiple sizes help hosts that pick a nearest pixmap instead of scaling.
static ICONS: LazyLock<Vec<ksni::Icon>> = LazyLock::new(|| {
    [
        include_bytes!("../icons/icon-16.png").as_slice(),
        include_bytes!("../icons/icon-22.png").as_slice(),
        include_bytes!("../icons/icon-24.png").as_slice(),
        include_bytes!("../icons/icon-32.png").as_slice(),
        include_bytes!("../icons/icon-48.png").as_slice(),
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
        // Keep a stable title so desktop tray hosts (e.g. Noctalia pin rules)
        // can match on "Thermalwriter*". Detailed state lives in the tooltip.
        "Thermalwriter".into()
    }

    fn status(&self) -> ksni::Status {
        if !self.status.online {
            ksni::Status::NeedsAttention
        } else if self.status.connected {
            ksni::Status::Active
        } else {
            // Still Active so hosts that hide Passive items keep showing us.
            ksni::Status::Active
        }
    }

    // IMPORTANT (Quickshell/Noctalia): if IconName is non-empty, the host uses
    // QIcon::fromTheme(IconName) and NEVER falls back to IconPixmap when the
    // theme lookup fails — which produces the missing-icon placeholder. Leave
    // IconName empty and ship pixmaps only.
    fn icon_name(&self) -> String {
        String::new()
    }

    fn icon_theme_path(&self) -> String {
        String::new()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        ICONS.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            icon_name: String::new(),
            title: self.status.tooltip_title(),
            description: self.status.tooltip_body(),
            icon_pixmap: ICONS.clone(),
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
    // Always reap first so dead children don't look "running".
    reap_child_zombies();

    if let Some(pid) = find_running_gui_pid() {
        info!("Config GUI already running (pid {pid}) — focusing");
        focus_gui_window(pid);
        return;
    }

    match find_gui_command() {
        Some((program, args)) => {
            info!("launching GUI: {} {:?}", program.display(), args);
            match launch_gui_detached(&program, &args) {
                Ok(()) => {
                    info!("Config GUI launch dispatched");
                    // Give the window a moment to map, then pull it onto the
                    // active workspace. Without this, Tauri often restores on
                    // a stale workspace and left-click looks like a no-op.
                    std::thread::spawn(|| {
                        for _ in 0..20 {
                            std::thread::sleep(std::time::Duration::from_millis(100));
                            if let Some(pid) = find_running_gui_pid() {
                                focus_gui_window(pid);
                                return;
                            }
                        }
                        warn!("Config GUI did not appear within 2s after launch");
                    });
                }
                Err(err) => error!("failed to launch GUI {}: {err:#}", program.display()),
            }
        }
        None => {
            error!(
                "could not find thermalwriter-gui; install the Config GUI or set THERMALWRITER_GUI"
            );
        }
    }
}

/// Launch the GUI outside the tray's process tree so:
/// 1) it gets a normal desktop session lifecycle
/// 2) exits don't leave unreaped zombies that block re-launch
fn launch_gui_detached(program: &Path, args: &[String]) -> Result<()> {
    // Prefer hyprctl so the window is owned by the compositor session.
    if let Some(hypr_pid) = find_hyprland_pid() {
        let mut cmdline = shell_quote(&program.to_string_lossy());
        for a in args {
            cmdline.push(' ');
            cmdline.push_str(&shell_quote(a));
        }
        let mut cmd = std::process::Command::new("hyprctl");
        cmd.args(["dispatch", "exec", &cmdline])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        inject_graphical_env_from_pid(&mut cmd, hypr_pid);
        let status = cmd
            .status()
            .context("hyprctl dispatch exec")?;
        if status.success() {
            return Ok(());
        }
        warn!("hyprctl exec failed ({status}); falling back to systemd-run");
    }

    // systemd --user scope: fully detached from the tray service cgroup.
    let mut cmd = std::process::Command::new("systemd-run");
    cmd.arg("--user")
        .arg("--collect")
        .arg("--same-dir")
        .arg("--quiet")
        .arg(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    inject_graphical_env(&mut cmd);
    let status = cmd.status().context("systemd-run")?;
    if status.success() {
        return Ok(());
    }

    // Last resort: direct spawn + setsid (still reaped via reap_child_zombies).
    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    inject_graphical_env(&mut cmd);
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            // New session so we aren't tied to the tray's controlling terminal/cgroup signals.
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn().context("spawn gui")?;
    info!("Config GUI started directly (pid {})", child.id());
    // Intentionally leak/detach: don't keep Child handle; zombies reaped periodically.
    std::mem::forget(child);
    Ok(())
}

fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_./:@+".contains(&b))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn reap_child_zombies() {
    // Non-blocking wait for any exited children (GUI launches, etc.).
    loop {
        let mut status = 0;
        let rc = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if rc <= 0 {
            break;
        }
    }
}

/// PID of a live Config GUI process, if any.
///
/// Matches on `/proc/<pid>/exe` (not free-text cmdline) so agent shells / scripts
/// that merely *mention* `thermalwriter-gui` are ignored. Zombies are skipped.
fn find_running_gui_pid() -> Option<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return None;
    };
    for ent in entries.flatten() {
        let pid: u32 = match ent.file_name().to_str().and_then(|s| s.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        if pid == std::process::id() {
            continue;
        }
            // Skip zombies (state 'Z') — they still have an exe link briefly.
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            // comm is in parentheses and may contain ')'; state is the next field.
            if let Some(idx) = stat.rfind(')') {
                let state = stat[idx + 1..].trim().chars().next().unwrap_or(' ');
                if state == 'Z' || state == 'X' {
                    continue;
                }
            }
        }
        let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok();
        let Some(exe) = exe else {
            continue;
        };
        let name = exe
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let path_lc = exe.to_string_lossy().to_ascii_lowercase();
        let is_gui = name == "thermalwriter-gui"
            || (name.contains("thermalwriter")
                && name.ends_with(".appimage")
                && name.contains("config"))
            || path_lc.ends_with("/thermalwriter-gui");
        if is_gui {
            return Some(pid);
        }
    }
    None
}

fn focus_gui_window(pid: u32) {
    // Move to the active workspace first, then focus + raise.
    // `movetoworkspace current` follows the focused monitor's active WS.
    let ops = [
        vec![
            "dispatch".into(),
            "movetoworkspace".into(),
            format!("current,pid:{pid}"),
        ],
        vec!["dispatch".into(), "focuswindow".into(), format!("pid:{pid}")],
        vec![
            "dispatch".into(),
            "alterzorder".into(),
            "top".into(),
            format!("pid:{pid}"),
        ],
    ];
    for args in ops {
        let mut cmd = std::process::Command::new("hyprctl");
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        inject_graphical_env(&mut cmd);
        let _ = cmd.status();
    }
}

/// Graphical session bits needed to talk to the running compositor.
#[derive(Debug, Clone)]
struct GraphicalEnv {
    his: Option<String>,
    display: Option<String>,
    wayland: Option<String>,
    runtime_dir: Option<String>,
    desktop: Option<String>,
    session_type: Option<String>,
}

fn inject_graphical_env(cmd: &mut std::process::Command) {
    if let Some(env) = resolve_graphical_env() {
        apply_graphical_env(cmd, &env);
    }
}

fn inject_graphical_env_from_pid(cmd: &mut std::process::Command, hypr_pid: u32) {
    if let Some(env) = resolve_graphical_env_for_pid(hypr_pid) {
        apply_graphical_env(cmd, &env);
    }
}

fn apply_graphical_env(cmd: &mut std::process::Command, env: &GraphicalEnv) {
    if let Some(v) = &env.his {
        cmd.env("HYPRLAND_INSTANCE_SIGNATURE", v);
    }
    if let Some(v) = &env.display {
        cmd.env("DISPLAY", v);
    }
    if let Some(v) = &env.wayland {
        cmd.env("WAYLAND_DISPLAY", v);
    }
    if let Some(v) = &env.runtime_dir {
        cmd.env("XDG_RUNTIME_DIR", v);
    }
    if let Some(v) = &env.desktop {
        cmd.env("XDG_CURRENT_DESKTOP", v);
    }
    if let Some(v) = &env.session_type {
        cmd.env("XDG_SESSION_TYPE", v);
    }
    // Prefer wayland for Qt/Tauri children when under Hyprland.
    if env.wayland.is_some() {
        cmd.env("QT_QPA_PLATFORM", "wayland;xcb");
    }
}

fn resolve_graphical_env() -> Option<GraphicalEnv> {
    let hypr_pid = find_hyprland_pid()?;
    resolve_graphical_env_for_pid(hypr_pid)
}

fn resolve_graphical_env_for_pid(hypr_pid: u32) -> Option<GraphicalEnv> {
    let environ = std::fs::read(format!("/proc/{hypr_pid}/environ")).ok()?;
    let mut env = GraphicalEnv {
        his: None,
        display: None,
        wayland: None,
        runtime_dir: None,
        desktop: None,
        session_type: None,
    };
    for entry in environ.split(|b| *b == 0) {
        if entry.is_empty() {
            continue;
        }
        let Ok(s) = std::str::from_utf8(entry) else {
            continue;
        };
        let Some((k, v)) = s.split_once('=') else {
            continue;
        };
        match k {
            "HYPRLAND_INSTANCE_SIGNATURE" => env.his = Some(v.to_string()),
            "DISPLAY" => env.display = Some(v.to_string()),
            "WAYLAND_DISPLAY" => env.wayland = Some(v.to_string()),
            "XDG_RUNTIME_DIR" => env.runtime_dir = Some(v.to_string()),
            "XDG_CURRENT_DESKTOP" => env.desktop = Some(v.to_string()),
            "XDG_SESSION_TYPE" => env.session_type = Some(v.to_string()),
            _ => {}
        }
    }

    // `/proc/Hyprland/environ` can hold a STALE signature after compositor
    // restarts. Prefer the live instance dir that owns `.socket.sock` and
    // whose lock/pid matches this Hyprland process.
    if let Some(live) = find_live_hyprland_signature(hypr_pid, env.runtime_dir.as_deref()) {
        env.his = Some(live);
    } else if let Some(his) = env.his.clone() {
        // Drop unusable signature (no command socket).
        let runtime = env
            .runtime_dir
            .clone()
            .unwrap_or_else(|| format!("/run/user/{}", nix_uid()));
        let sock = PathBuf::from(&runtime)
            .join("hypr")
            .join(&his)
            .join(".socket.sock");
        if !sock.exists() {
            env.his = None;
        }
    }

    Some(env)
}

fn nix_uid() -> u32 {
    unsafe { libc::getuid() }
}

/// Locate the Hyprland instance signature that actually accepts `hyprctl`.
fn find_live_hyprland_signature(hypr_pid: u32, runtime_dir: Option<&str>) -> Option<String> {
    let runtime = runtime_dir
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", nix_uid())));
    let hypr_root = runtime.join("hypr");
    let entries = std::fs::read_dir(&hypr_root).ok()?;

    let mut fallback: Option<String> = None;
    for ent in entries.flatten() {
        let path = ent.path();
        if !path.is_dir() {
            continue;
        }
        let sock = path.join(".socket.sock");
        if !sock.exists() {
            continue;
        }
        let name = path.file_name()?.to_string_lossy().into_owned();

        // Prefer the instance whose lock file pid matches the running compositor.
        let lock = path.join("hyprland.lock");
        if let Ok(contents) = std::fs::read_to_string(&lock) {
            // lock format is typically "pid\nsig" or just contains the pid.
            if contents
                .lines()
                .next()
                .and_then(|l| l.trim().parse::<u32>().ok())
                == Some(hypr_pid)
            {
                return Some(name);
            }
        }

        // Keep newest usable socket as fallback (by directory mtime).
        fallback = Some(name);
    }
    fallback
}

fn find_hyprland_pid() -> Option<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return None;
    };
    for ent in entries.flatten() {
        let pid: u32 = match ent.file_name().to_str().and_then(|s| s.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok();
        let Some(exe) = exe else { continue };
        if exe.file_name().and_then(|n| n.to_str()) == Some("Hyprland") {
            return Some(pid);
        }
    }
    None
}

/// Resolve the GUI launcher.
///
/// Order: `THERMALWRITER_GUI`, `thermalwriter-gui` on `PATH`, common install
/// dirs, then AppImages under `~/Applications`, `~/Downloads`, and the
/// thermalwriter checkout release bundle paths.
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
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(h) = home.as_ref() {
        candidates.push(h.join(".cargo/bin/thermalwriter-gui"));
        candidates.push(h.join(".local/bin/thermalwriter-gui"));
    }
    candidates.push(PathBuf::from("/usr/bin/thermalwriter-gui"));
    candidates.push(PathBuf::from("/usr/local/bin/thermalwriter-gui"));

    for path in &candidates {
        if path.is_file() && is_executable(path) {
            return Some((path.clone(), Vec::new()));
        }
    }

    // AppImage / checkout bundles
    let search_dirs: Vec<PathBuf> = home
        .iter()
        .flat_map(|h| {
            [
                h.join("Applications"),
                h.join("Downloads"),
                h.join(".cache/thermalwriter-qa/artifacts"),
                h.join("code/thermalrighter/target/release"),
                h.join("code/thermalrighter/target/release/bundle/appimage"),
                h.join("code/thermalrighter/gui/src-tauri/target/release"),
                h.join(
                    "code/thermalrighter/gui/src-tauri/target/release/bundle/appimage",
                ),
            ]
        })
        .collect();

    for dir in search_dirs {
        if let Some(found) = find_gui_in_dir(&dir) {
            return Some((found, Vec::new()));
        }
    }

    None
}

fn find_gui_in_dir(dir: &Path) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    // Prefer a real binary, then AppImage.
    let exact = dir.join("thermalwriter-gui");
    if exact.is_file() && is_executable(&exact) {
        return Some(exact);
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut appimages = Vec::new();
    for ent in entries.flatten() {
        let path = ent.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !is_executable(&path) {
            continue;
        }
        if name.contains("thermalwriter") && name.ends_with(".appimage") {
            appimages.push(path);
        }
    }
    appimages.sort();
    appimages.pop() // newest-ish by name sort; any match is fine
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

    // Retry while the desktop tray host (StatusNotifierWatcher) is starting.
    let handle = {
        let mut status = initial;
        let mut last_err = None;
        let mut handle = None;
        for attempt in 1..=30u32 {
            let tray = ThermalTray {
                status: status.clone(),
                tx: tx.clone(),
            };
            match tray.spawn().await {
                Ok(h) => {
                    handle = Some(h);
                    break;
                }
                Err(err) => {
                    last_err = Some(err);
                    warn!(
                        "tray host not ready (attempt {attempt}/30): {}",
                        last_err.as_ref().unwrap()
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    status = DaemonClient::fetch_status().await;
                }
            }
        }
        handle.ok_or_else(|| {
            anyhow::anyhow!(
                "failed to register StatusNotifierItem after retries: {}",
                last_err
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "unknown".into())
            )
        })?
    };

    info!("tray registered");

    // Periodically reap any direct children (defensive; normal path detaches GUI).
    let mut reap = tokio::time::interval(std::time::Duration::from_secs(5));
    reap.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = reap.tick() => {
                reap_child_zombies();
            }
            action = rx.recv() => {
                let Some(action) = action else { break; };
                debug!("action: {action:?}");
                if !handle_action(action, &handle).await {
                    break;
                }
            }
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
