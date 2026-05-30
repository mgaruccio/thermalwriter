// D-Bus client types shared between the CLI and the GUI.
//
// The `DisplayProxy` trait is extracted here (unconditionally compiled) so the
// GUI crate can build against it without depending on the daemon feature set.
// Keeping it out of the `cli` module lets `--no-default-features` builds
// (library-only) still expose the proxy for downstream consumers.

use std::collections::HashMap;

/// zbus proxy for the `com.thermalwriter.Display` D-Bus interface.
///
/// This mirrors the interface the daemon's `dbus::DisplayIface` implementation
/// exposes in `service/dbus.rs`. Any method changes there must also be
/// reflected here.
#[zbus::proxy(
    interface = "com.thermalwriter.Display",
    default_service = "com.thermalwriter.Service",
    default_path = "/com/thermalwriter/display"
)]
pub trait Display {
    async fn get_status(&self) -> zbus::Result<HashMap<String, String>>;
    async fn set_layout(&self, name: &str) -> zbus::Result<String>;
    async fn set_mode(&self, mode: &str, command: &str) -> zbus::Result<String>;
    async fn list_layouts(&self) -> zbus::Result<Vec<String>>;
    /// Returns (key, name, unit) tuples for each available sensor.
    async fn list_sensors(&self) -> zbus::Result<Vec<(String, String, String)>>;
    async fn get_layout_vars(&self, name: &str) -> zbus::Result<Vec<HashMap<String, String>>>;
    async fn set_layout_vars(&self, name: &str, vars: HashMap<String, String>) -> zbus::Result<()>;
    async fn stop(&self) -> zbus::Result<()>;
    async fn reload(&self) -> zbus::Result<()>;
    async fn set_background(&self, name: &str) -> zbus::Result<()>;
    async fn clear_background(&self) -> zbus::Result<()>;
    async fn list_backgrounds(&self) -> zbus::Result<Vec<String>>;
    async fn set_default_layout(&self, name: &str) -> zbus::Result<()>;
    /// Launch a streaming session with a fully-specified argv (no shell).
    /// The GUI uses this to pass custom preset configs, terminal wrappers, etc.
    /// SDL_VIDEODRIVER=x11 is injected unconditionally by the daemon.
    /// Session-only: never persisted.
    async fn set_mode_argv(&self, argv: Vec<String>) -> zbus::Result<String>;
    /// Launch a named streaming preset (conky | cava | btop) via structured argv.
    /// The preset binary is launched without a shell — no word-splitting occurs
    /// on config paths with spaces. SDL_VIDEODRIVER=x11 is set unconditionally.
    async fn start_stream_preset(&self, preset: &str) -> zbus::Result<String>;
    /// Resolve binary names to absolute paths using the daemon's PATH.
    /// Missing binaries map to an empty string. Returns absolute paths so
    /// the GUI can bake them into preset argv without exec-time re-resolution.
    async fn resolve_binaries(&self, names: Vec<String>) -> zbus::Result<HashMap<String, String>>;
    /// Read the current tick rate (FPS).
    /// Note: zbus property accessors in proxy traits must be non-async.
    #[zbus(property)]
    fn tick_rate(&self) -> zbus::Result<u32>;
    /// Set the daemon tick rate (1–60 FPS). The daemon also validates the range.
    #[zbus(property)]
    fn set_tick_rate(&self, rate: u32) -> zbus::Result<()>;
}
