// TOML config parsing for thermalwriter.
// Config file location: ~/.config/thermalwriter/config.toml
// Missing file → defaults. Invalid TOML → error with path.

use crate::theme::ThemePalette;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

fn parse_device_selector(s: &str) -> Result<(), String> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("auto") || s.eq_ignore_ascii_case("all") {
        return Ok(());
    }
    let (vid_s, pid_s) = s
        .split_once(':')
        .ok_or_else(|| format!("must be 'auto', 'all', or 'VID:PID', got {s:?}"))?;
    let parse = |part: &str| -> Result<u16, String> {
        let part = part
            .trim()
            .trim_start_matches("0x")
            .trim_start_matches("0X");
        u16::from_str_radix(part, 16).map_err(|_| format!("not a hex u16: {part:?}"))
    };
    parse(vid_s)?;
    parse(pid_s)?;
    Ok(())
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_tmp_suffix() -> u64 {
    TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Serializes all read-modify-write cycles on config.toml within a process.
/// Each save_* method acquires this before reading the file and holds it
/// through the rename, preventing lost-update races between concurrent callers.
static CONFIG_WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DisplayConfig {
    /// Frames per second to send to the display (1–30).
    pub tick_rate: u32,
    /// Layout filename to load on startup (relative to layouts dir).
    pub default_layout: String,
    /// JPEG encoding quality (1–100).
    pub jpeg_quality: u8,
    /// Rotate frames before sending to device (0, 90, 180, 270 degrees).
    /// Depends on how the cooler is physically mounted. Default 180 for
    /// Peerless Vision with LCD at bottom.
    pub rotation: u16,
    /// Display mode: "svg", "html", or "xvfb".
    pub mode: String,
    /// Device selector: `"auto"`, `"all"`, or `"VID:PID"` (hex). Default auto.
    pub device: String,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            tick_rate: 2,
            default_layout: "svg/neon-dash-v2.svg".to_string(),
            jpeg_quality: 85,
            rotation: 180,
            mode: "svg".to_string(),
            device: "auto".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SensorsConfig {
    /// How often to poll sensors in milliseconds.
    pub poll_interval_ms: u64,
    /// Override MangoHud log directory. Empty string = auto-detect.
    pub mangohud_log_dir: String,
}

impl Default for SensorsConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: 2000,
            mangohud_log_dir: String::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct XvfbConfig {
    /// Shell command to run inside the virtual display.
    pub command: String,
    /// Frame rate for xvfb capture mode (1-60 FPS).
    pub tick_rate: u32,
}

impl Default for XvfbConfig {
    fn default() -> Self {
        Self {
            command: String::new(),
            tick_rate: 15,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub source: String,
    pub manual: Option<ThemePalette>,
}

impl ThemeConfig {
    /// Resolve the active palette from `source`.
    ///
    /// - `""` / `"default"` → built-in defaults (ignores any manual table)
    /// - `"manual"` → configured manual palette, or defaults if unset
    /// - anything else → error
    pub fn resolve_palette(&self) -> Result<ThemePalette> {
        match self.source.as_str() {
            "" | "default" => Ok(ThemePalette::default()),
            "manual" => Ok(self.manual.clone().unwrap_or_default()),
            other => {
                anyhow::bail!("theme.source='{other}' must be one of default, manual (or empty)")
            }
        }
    }
}

/// Background image configuration. The image file lives under
/// `~/.config/thermalwriter/backgrounds/`. Empty/None = no background.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct BackgroundConfig {
    /// Filename (no path) of the active background. None = no background.
    pub image: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub display: DisplayConfig,
    pub sensors: SensorsConfig,
    pub theme: ThemeConfig,
    pub xvfb: XvfbConfig,
    pub background: BackgroundConfig,
    /// Per-layout variable overrides keyed by layout filename.
    /// The outer map is `{layout_name: {var_name: value}}`.
    pub layout_vars: HashMap<String, HashMap<String, String>>,
}

impl Config {
    /// Load config from the given path. Returns defaults if the file doesn't exist.
    /// Returns an error (with the file path in the message) if the file exists but is invalid TOML
    /// or contains out-of-range values.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;
        let cfg: Self = toml::from_str(&contents)
            .with_context(|| format!("Invalid TOML in config file: {}", path.display()))?;
        cfg.validate()
            .with_context(|| format!("Invalid values in config file: {}", path.display()))?;
        Ok(cfg)
    }

    /// Validate all config fields are within acceptable ranges.
    pub fn validate(&self) -> Result<()> {
        if self.display.tick_rate == 0 || self.display.tick_rate > 60 {
            anyhow::bail!(
                "display.tick_rate={} out of range [1, 60]",
                self.display.tick_rate
            );
        }
        if self.display.jpeg_quality < 10 || self.display.jpeg_quality > 100 {
            anyhow::bail!(
                "display.jpeg_quality={} out of range [10, 100]",
                self.display.jpeg_quality
            );
        }
        if !matches!(self.display.rotation, 0 | 90 | 180 | 270) {
            anyhow::bail!(
                "display.rotation={} must be one of 0, 90, 180, 270",
                self.display.rotation
            );
        }
        // Validate device selector shape without requiring USB.
        if let Err(e) = parse_device_selector(&self.display.device) {
            anyhow::bail!("display.device invalid: {e}");
        }
        if self.sensors.poll_interval_ms < 100 || self.sensors.poll_interval_ms > 60_000 {
            anyhow::bail!(
                "sensors.poll_interval_ms={} out of range [100, 60000]",
                self.sensors.poll_interval_ms
            );
        }
        if self.xvfb.tick_rate == 0 || self.xvfb.tick_rate > 60 {
            anyhow::bail!(
                "xvfb.tick_rate={} out of range [1, 60]",
                self.xvfb.tick_rate
            );
        }
        match self.display.mode.as_str() {
            "svg" => {
                if !self.display.default_layout.ends_with(".svg") {
                    anyhow::bail!(
                        "display.default_layout='{}' must end with .svg when display.mode is svg",
                        self.display.default_layout
                    );
                }
            }
            "html" => {
                if !(self.display.default_layout.ends_with(".html")
                    || self.display.default_layout.ends_with(".htm"))
                {
                    anyhow::bail!(
                        "display.default_layout='{}' must end with .html/.htm when display.mode is html",
                        self.display.default_layout
                    );
                }
            }
            "xvfb" => {}
            other => {
                anyhow::bail!("display.mode='{other}' must be one of svg, html, xvfb")
            }
        }
        // Validate theme.source the same way resolve_palette does.
        let _ = self.theme.resolve_palette()?;
        Ok(())
    }

    /// Returns the default config file path: ~/.config/thermalwriter/config.toml
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()))
            .join("thermalwriter")
            .join("config.toml")
    }

    /// Persist the variable overrides for `layout_name` to the on-disk config,
    /// preserving user comments and formatting via `toml_edit`. Writes a
    /// sibling temp file in the same directory as `path` and atomically renames
    /// it on success.
    ///
    /// Any existing `[layout_vars."<layout_name>"]` section is replaced
    /// wholesale; other sections are left untouched.
    pub fn save_layout_vars(
        path: &Path,
        layout_name: &str,
        vars: &HashMap<String, String>,
    ) -> Result<()> {
        use toml_edit::{DocumentMut, Item, Table, value};

        let _guard = CONFIG_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // Load existing document (or start empty).
        let existing = if path.exists() {
            std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read config: {}", path.display()))?
        } else {
            String::new()
        };
        let mut doc: DocumentMut = existing
            .parse()
            .with_context(|| format!("Invalid TOML in config: {}", path.display()))?;

        // Ensure top-level [layout_vars] exists as a table.
        if doc.get("layout_vars").is_none() {
            doc["layout_vars"] = Item::Table(Table::new());
        }
        let layout_vars = doc["layout_vars"]
            .as_table_mut()
            .context("layout_vars section is not a table")?;

        // Replace the target layout's section with fresh contents. We sort keys
        // for stable on-disk ordering; toml_edit keeps comments elsewhere intact.
        let mut new_section = Table::new();
        let mut sorted: Vec<(&String, &String)> = vars.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));
        for (k, v) in sorted {
            new_section.insert(k, value(v.clone()));
        }
        layout_vars.insert(layout_name, Item::Table(new_section));

        // Atomic write: temp file in the same directory (so rename is atomic on
        // the same filesystem), then rename over the target.
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("config path has no parent: {}", path.display()))?;
        let file_name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("config path has no file name: {}", path.display()))?;
        let tmp_name = format!(
            "{}.tmp.{}.{}",
            file_name.to_string_lossy(),
            std::process::id(),
            next_tmp_suffix(),
        );
        let tmp_path = parent.join(tmp_name);

        {
            let mut tmp = std::fs::File::create(&tmp_path)
                .with_context(|| format!("Failed to create temp file: {}", tmp_path.display()))?;
            tmp.write_all(doc.to_string().as_bytes())
                .with_context(|| format!("Failed to write temp file: {}", tmp_path.display()))?;
            tmp.sync_all()
                .with_context(|| format!("Failed to fsync temp file: {}", tmp_path.display()))?;
        }

        std::fs::rename(&tmp_path, path).with_context(|| {
            format!(
                "Failed to rename {} -> {}",
                tmp_path.display(),
                path.display()
            )
        })?;

        Ok(())
    }

    /// Persist the active display layout and mode while preserving user comments.
    pub fn save_display_layout(path: &Path, layout_name: &str, mode: &str) -> Result<()> {
        use toml_edit::{DocumentMut, Item, Table, value};

        let _guard = CONFIG_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let existing = if path.exists() {
            std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read config: {}", path.display()))?
        } else {
            String::new()
        };
        let mut doc: DocumentMut = existing
            .parse()
            .with_context(|| format!("Invalid TOML in config: {}", path.display()))?;

        if doc.get("display").is_none() {
            doc["display"] = Item::Table(Table::new());
        }
        let display = doc["display"]
            .as_table_mut()
            .context("display section is not a table")?;
        display.insert("default_layout", value(layout_name));
        display.insert("mode", value(mode));

        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("config path has no parent: {}", path.display()))?;
        let file_name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("config path has no file name: {}", path.display()))?;
        let tmp_name = format!(
            "{}.tmp.{}.{}",
            file_name.to_string_lossy(),
            std::process::id(),
            next_tmp_suffix(),
        );
        let tmp_path = parent.join(tmp_name);

        {
            let mut tmp = std::fs::File::create(&tmp_path)
                .with_context(|| format!("Failed to create temp file: {}", tmp_path.display()))?;
            tmp.write_all(doc.to_string().as_bytes())
                .with_context(|| format!("Failed to write temp file: {}", tmp_path.display()))?;
            tmp.sync_all()
                .with_context(|| format!("Failed to fsync temp file: {}", tmp_path.display()))?;
        }

        std::fs::rename(&tmp_path, path).with_context(|| {
            format!(
                "Failed to rename {} -> {}",
                tmp_path.display(),
                path.display()
            )
        })?;

        Ok(())
    }

    /// Persist the active background image filename (or None to clear) to the
    /// on-disk config, preserving user comments via `toml_edit`. Atomic write.
    pub fn save_background_image(path: &Path, image: Option<&str>) -> Result<()> {
        use toml_edit::{DocumentMut, Item, Table, value};

        let _guard = CONFIG_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let existing = if path.exists() {
            std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read config: {}", path.display()))?
        } else {
            String::new()
        };
        let mut doc: DocumentMut = existing
            .parse()
            .with_context(|| format!("Invalid TOML in config: {}", path.display()))?;

        if doc.get("background").is_none() {
            doc["background"] = Item::Table(Table::new());
        }
        let bg = doc["background"]
            .as_table_mut()
            .context("background section is not a table")?;

        match image {
            Some(name) => {
                bg.insert("image", value(name));
            }
            None => {
                bg.remove("image");
            }
        }

        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("config path has no parent: {}", path.display()))?;
        let file_name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("config path has no file name: {}", path.display()))?;
        let tmp_name = format!(
            "{}.tmp.{}.{}",
            file_name.to_string_lossy(),
            std::process::id(),
            next_tmp_suffix(),
        );
        let tmp_path = parent.join(tmp_name);

        {
            let mut tmp = std::fs::File::create(&tmp_path)
                .with_context(|| format!("Failed to create temp file: {}", tmp_path.display()))?;
            tmp.write_all(doc.to_string().as_bytes())
                .with_context(|| format!("Failed to write temp file: {}", tmp_path.display()))?;
            tmp.sync_all()
                .with_context(|| format!("Failed to fsync temp file: {}", tmp_path.display()))?;
        }

        std::fs::rename(&tmp_path, path).with_context(|| {
            format!(
                "Failed to rename {} -> {}",
                tmp_path.display(),
                path.display()
            )
        })?;

        Ok(())
    }
}

/// Built-in layout HTML content, embedded at compile time.
pub mod builtin_layouts {
    pub const SYSTEM_STATS: &str = include_str!("../layouts/system-stats.html");
    pub const GPU_FOCUS: &str = include_str!("../layouts/gpu-focus.html");
    pub const MINIMAL: &str = include_str!("../layouts/minimal.html");

    // SVG layouts
    pub const SVG_NEON_DASH: &str = include_str!("../layouts/svg/neon-dash.svg");
    pub const SVG_ARC_GAUGE: &str = include_str!("../layouts/svg/arc-gauge.svg");
    pub const SVG_CYBER_GRID: &str = include_str!("../layouts/svg/cyber-grid.svg");
    pub const SVG_NEON_DASH_V2: &str = include_str!("../layouts/svg/neon-dash-v2.svg");

    // Xvfb wrapper configs (conky + cava starter presets for LCD streaming)
    pub const WRAPPER_CONKY: &str = include_str!("../layouts/wrappers/conky-480.conf");
    pub const WRAPPER_CAVA: &str = include_str!("../layouts/wrappers/cava-480.conf");

    // Seed background images (tiny PNGs, decoded + resized to 480×480 at runtime)
    pub const BG_DARK_SOLID: &[u8] = include_bytes!("../assets/backgrounds/dark-solid.png");
    pub const BG_DARK_GRADIENT: &[u8] = include_bytes!("../assets/backgrounds/dark-gradient.png");

    /// Copy built-in background images to the backgrounds directory if they don't
    /// already exist. Mirrors `seed_layout_dir` — only writes if `!dest.exists()`.
    pub fn seed_background_dir(bg_dir: &std::path::Path) -> anyhow::Result<()> {
        use anyhow::Context as _;
        let backgrounds: &[(&str, &[u8])] = &[
            ("dark-solid.png", BG_DARK_SOLID),
            ("dark-gradient.png", BG_DARK_GRADIENT),
        ];
        std::fs::create_dir_all(bg_dir)
            .with_context(|| format!("Failed to create backgrounds dir: {}", bg_dir.display()))?;
        for (name, content) in backgrounds {
            let dest = bg_dir.join(name);
            if !dest.exists() {
                std::fs::write(&dest, content).with_context(|| {
                    format!("Failed to write built-in background: {}", dest.display())
                })?;
            }
        }
        Ok(())
    }

    /// Copy built-in Xvfb wrapper configs (conky + cava) to `wrapper_dir` if they
    /// don't already exist.  Mirrors `seed_layout_dir` — only writes if
    /// `!dest.exists()` so user edits are never clobbered.
    ///
    /// `wrapper_dir` is typically `~/.config/thermalwriter/wrappers/`.
    pub fn seed_wrapper_dir(wrapper_dir: &std::path::Path) -> anyhow::Result<()> {
        use anyhow::Context as _;
        let wrappers: &[(&str, &str)] = &[
            ("conky-480.conf", WRAPPER_CONKY),
            ("cava-480.conf", WRAPPER_CAVA),
        ];
        std::fs::create_dir_all(wrapper_dir)
            .with_context(|| format!("Failed to create wrappers dir: {}", wrapper_dir.display()))?;
        for (name, content) in wrappers {
            let dest = wrapper_dir.join(name);
            if !dest.exists() {
                std::fs::write(&dest, content).with_context(|| {
                    format!("Failed to write built-in wrapper: {}", dest.display())
                })?;
            }
        }
        Ok(())
    }

    /// Builtin layout identity used when a configured `.svg` file is missing.
    pub const FALLBACK_SVG_NAME: &str = "svg/neon-dash-v2.svg";
    /// Builtin layout identity used when a configured HTML file is missing.
    pub const FALLBACK_HTML_NAME: &str = "system-stats.html";

    /// Resolve startup layout identity and content together.
    ///
    /// When `on_disk` is `Some`, the configured name is preserved. When the
    /// configured file is missing, both the fallback content and its canonical
    /// name are chosen from the configured layout kind so an SVG mode/name
    /// never receives HTML (and vice versa).
    pub fn resolve_layout_identity(
        configured_name: &str,
        on_disk: Option<String>,
    ) -> (String, String) {
        match on_disk {
            Some(content) => (configured_name.to_string(), content),
            None if configured_name.ends_with(".svg") => {
                (FALLBACK_SVG_NAME.to_string(), SVG_NEON_DASH_V2.to_string())
            }
            None => (FALLBACK_HTML_NAME.to_string(), SYSTEM_STATS.to_string()),
        }
    }

    /// Copy built-in layouts to the layouts directory if they don't already exist.
    /// This lets users edit the layouts without losing the originals on first run.
    pub fn seed_layout_dir(layout_dir: &std::path::Path) -> anyhow::Result<()> {
        use anyhow::Context as _;
        let layouts = [
            ("system-stats.html", SYSTEM_STATS),
            ("gpu-focus.html", GPU_FOCUS),
            ("minimal.html", MINIMAL),
            ("svg/neon-dash.svg", SVG_NEON_DASH),
            ("svg/arc-gauge.svg", SVG_ARC_GAUGE),
            ("svg/cyber-grid.svg", SVG_CYBER_GRID),
            ("svg/neon-dash-v2.svg", SVG_NEON_DASH_V2),
        ];
        for (name, content) in &layouts {
            let dest = layout_dir.join(name);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("Failed to create layout dir: {}", parent.display())
                })?;
            }
            if !dest.exists() {
                std::fs::write(&dest, content).with_context(|| {
                    format!("Failed to write built-in layout: {}", dest.display())
                })?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::builtin_layouts;
    use super::{Config, ThemeConfig};
    use crate::render::svg::SvgRenderer;
    use crate::theme::ThemePalette;

    #[test]
    fn missing_svg_layout_falls_back_to_svg_builtin_not_html() {
        let (name, content) = builtin_layouts::resolve_layout_identity("missing/custom.svg", None);
        assert_eq!(name, builtin_layouts::FALLBACK_SVG_NAME);
        assert_eq!(content, builtin_layouts::SVG_NEON_DASH_V2);
        assert!(
            content.contains("<svg"),
            "missing .svg must fall back to SVG content, not HTML"
        );
        // Construction succeeds; feeding SYSTEM_STATS HTML here would fail later
        // at tick time after a misleadingly successful path.
        SvgRenderer::new(&content, 480, 480).expect("fallback SVG must construct SvgRenderer");
    }

    #[test]
    fn missing_html_layout_falls_back_to_system_stats() {
        let (name, content) = builtin_layouts::resolve_layout_identity("missing/custom.html", None);
        assert_eq!(name, builtin_layouts::FALLBACK_HTML_NAME);
        assert_eq!(content, builtin_layouts::SYSTEM_STATS);
    }

    #[test]
    fn present_layout_keeps_configured_identity() {
        let (name, content) = builtin_layouts::resolve_layout_identity(
            "svg/custom.svg",
            Some("<svg></svg>".to_string()),
        );
        assert_eq!(name, "svg/custom.svg");
        assert_eq!(content, "<svg></svg>");
    }

    #[test]
    fn validate_rejects_unknown_display_mode() {
        let mut cfg = super::Config::default();
        cfg.display.mode = "svgg".into();
        let err = cfg.validate().expect_err("unknown mode must fail");
        assert!(
            err.to_string().contains("must be one of svg, html, xvfb"),
            "{err}"
        );
    }

    #[test]
    fn validate_rejects_mode_layout_extension_mismatch() {
        let mut cfg = super::Config::default();
        cfg.display.mode = "svg".into();
        cfg.display.default_layout = "system-stats.html".into();
        let err = cfg
            .validate()
            .expect_err("svg mode + html layout must fail");
        assert!(err.to_string().contains("must end with .svg"), "{err}");

        cfg.display.mode = "html".into();
        cfg.display.default_layout = "svg/neon-dash-v2.svg".into();
        let err = cfg
            .validate()
            .expect_err("html mode + svg layout must fail");
        assert!(err.to_string().contains("must end with .html"), "{err}");
    }

    #[test]
    fn validate_accepts_matching_mode_and_layout() {
        let mut cfg = super::Config::default();
        cfg.display.mode = "svg".into();
        cfg.display.default_layout = "svg/neon-dash-v2.svg".into();
        cfg.validate().expect("matching svg mode/layout");

        cfg.display.mode = "html".into();
        cfg.display.default_layout = "system-stats.html".into();
        cfg.validate().expect("matching html mode/layout");

        cfg.display.mode = "xvfb".into();
        cfg.display.default_layout = "svg/neon-dash-v2.svg".into();
        cfg.validate().expect("xvfb mode ignores layout extension");
    }

    #[test]
    fn seed_wrapper_dir_creates_both_configs() {
        let tmp = tempfile::tempdir().unwrap();
        let wrapper_dir = tmp.path().join("wrappers");
        builtin_layouts::seed_wrapper_dir(&wrapper_dir).unwrap();

        assert!(wrapper_dir.join("conky-480.conf").exists());
        assert!(wrapper_dir.join("cava-480.conf").exists());
    }

    #[test]
    fn seed_wrapper_dir_does_not_clobber_existing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let wrapper_dir = tmp.path().join("wrappers");
        std::fs::create_dir_all(&wrapper_dir).unwrap();

        // Write a user-customised version
        let user_content = b"# user edit";
        std::fs::write(wrapper_dir.join("conky-480.conf"), user_content).unwrap();

        // Seed should leave the user file untouched
        builtin_layouts::seed_wrapper_dir(&wrapper_dir).unwrap();

        let content = std::fs::read(wrapper_dir.join("conky-480.conf")).unwrap();
        assert_eq!(
            content, user_content,
            "seed_wrapper_dir clobbered user edit"
        );
    }

    #[test]
    fn wrapper_conky_content_has_required_keys() {
        let conf = builtin_layouts::WRAPPER_CONKY;
        // Foreground operation
        assert!(
            conf.contains("background        = false"),
            "conky must be foreground"
        );
        // Window setup
        assert!(conf.contains("own_window        = true"));
        assert!(conf.contains("own_window_type   = 'desktop'"));
        assert!(conf.contains("double_buffer     = true"));
        // 480x480
        assert!(conf.contains("minimum_width     = 480"));
        assert!(conf.contains("minimum_height    = 480"));
        assert!(conf.contains("maximum_width     = 480"));
        // Alignment / gap
        assert!(conf.contains("alignment         = 'top_left'"));
        assert!(conf.contains("gap_x             = 0"));
        assert!(conf.contains("gap_y             = 0"));
        // Opaque own_window_colour
        assert!(conf.contains("own_window_colour = '#"));
        // Font >= 14px
        assert!(
            conf.contains("size=14") || conf.contains("size=16") || conf.contains("size=18"),
            "conky font must be >= 14px"
        );
    }

    #[test]
    fn wrapper_cava_content_has_required_keys() {
        let conf = builtin_layouts::WRAPPER_CAVA;
        // SDL backend
        assert!(conf.contains("method = sdl"), "cava must use sdl output");
        // 480x480
        assert!(conf.contains("width = 480"));
        assert!(conf.contains("height = 480"));
        // bars must NOT be 24 (crashes at 480px); accept 0 (auto) or <= 22
        if conf.contains("bars = ") {
            let bars_line = conf
                .lines()
                .find(|l| l.trim().starts_with("bars ="))
                .unwrap();
            let val: u32 = bars_line
                .split('=')
                .nth(1)
                .unwrap()
                .split('#')
                .next()
                .unwrap()
                .trim()
                .parse()
                .unwrap_or(0);
            assert!(
                val == 0 || val <= 22,
                "cava bars={} would abort at 480px (max safe=22, or 0=auto)",
                val
            );
        }
        // mono channels: prevents stereo splitting spectrum to edges
        assert!(
            conf.contains("channels = mono"),
            "cava must use channels=mono to fill all bars across 480px width"
        );
        // bar_width set so bars fill the frame (not sparse edge-only rendering)
        assert!(
            conf.contains("bar_width"),
            "cava must set bar_width to control fill at 480px"
        );
        // SDL_VIDEODRIVER requirement must be documented
        assert!(
            conf.contains("SDL_VIDEODRIVER"),
            "cava config must document SDL_VIDEODRIVER=x11 requirement"
        );
        // Pulse audio input
        assert!(conf.contains("method = pulse"));
    }

    #[test]
    fn resolve_palette_default_ignores_manual() {
        let theme = ThemeConfig {
            source: "default".to_string(),
            manual: Some(ThemePalette {
                primary: "#aabbcc".to_string(),
                ..ThemePalette::default()
            }),
        };
        let palette = theme.resolve_palette().unwrap();
        assert_eq!(palette.primary, ThemePalette::default().primary);
        assert_ne!(palette.primary, "#aabbcc");
    }

    #[test]
    fn resolve_palette_empty_source_ignores_manual() {
        let theme = ThemeConfig {
            source: String::new(),
            manual: Some(ThemePalette {
                primary: "#aabbcc".to_string(),
                ..ThemePalette::default()
            }),
        };
        let palette = theme.resolve_palette().unwrap();
        assert_eq!(palette, ThemePalette::default());
    }

    #[test]
    fn resolve_palette_manual_uses_table() {
        let manual = ThemePalette {
            primary: "#aabbcc".to_string(),
            secondary: "#112233".to_string(),
            ..ThemePalette::default()
        };
        let theme = ThemeConfig {
            source: "manual".to_string(),
            manual: Some(manual.clone()),
        };
        let palette = theme.resolve_palette().unwrap();
        assert_eq!(palette.primary, "#aabbcc");
        assert_eq!(palette.secondary, "#112233");
    }

    #[test]
    fn resolve_palette_manual_missing_table_uses_defaults() {
        let theme = ThemeConfig {
            source: "manual".to_string(),
            manual: None,
        };
        assert_eq!(theme.resolve_palette().unwrap(), ThemePalette::default());
    }

    #[test]
    fn resolve_palette_unknown_source_fails() {
        let theme = ThemeConfig {
            source: "garbage".to_string(),
            manual: None,
        };
        assert!(theme.resolve_palette().is_err());
    }

    #[test]
    fn validate_rejects_unknown_theme_source() {
        let mut cfg = Config::default();
        cfg.theme.source = "garbage".to_string();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("theme.source"), "unexpected error: {err}");
    }

    #[test]
    fn validate_accepts_all_device_selector() {
        let mut cfg = Config::default();
        cfg.display.device = "all".into();
        cfg.validate().expect("all selector must validate");
    }

    #[test]
    fn validate_accepts_default_and_manual_theme_source() {
        let mut cfg = Config::default();
        cfg.theme.source = "default".to_string();
        cfg.validate().unwrap();
        cfg.theme.source = "manual".to_string();
        cfg.validate().unwrap();
        cfg.theme.source = String::new();
        cfg.validate().unwrap();
    }
}
