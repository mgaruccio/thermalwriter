// TOML config parsing for thermalwriter.
// Config file location: ~/.config/thermalwriter/config.toml
// Missing file → defaults. Invalid TOML → error with path.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use anyhow::{Context, Result};
use crate::theme::ThemePalette;

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
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            tick_rate: 2,
            default_layout: "svg/neon-dash-v2.svg".to_string(),
            jpeg_quality: 85,
            rotation: 180,
            mode: "svg".to_string(),
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
            poll_interval_ms: 1000,
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
        Ok(())
    }

    /// Returns the default config file path: ~/.config/thermalwriter/config.toml
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from(
                std::env::var("HOME").unwrap_or_default()
            ))
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
            Some(name) => { bg.insert("image", value(name)); }
            None => { bg.remove("image"); }
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
                std::fs::write(&dest, content)
                    .with_context(|| format!("Failed to write built-in background: {}", dest.display()))?;
            }
        }
        Ok(())
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
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create layout dir: {}", parent.display()))?;
            }
            if !dest.exists() {
                std::fs::write(&dest, content)
                    .with_context(|| format!("Failed to write built-in layout: {}", dest.display()))?;
            }
        }
        Ok(())
    }
}
