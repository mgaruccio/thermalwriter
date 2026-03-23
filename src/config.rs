// TOML config parsing for thermalwriter.
// Config file location: ~/.config/thermalwriter/config.toml
// Missing file → defaults. Invalid TOML → error with path.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use crate::theme::ThemePalette;

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
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            tick_rate: 2,
            default_layout: "svg/neon-dash-v2.svg".to_string(),
            jpeg_quality: 85,
            rotation: 180,
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

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub source: String,
    pub background_image: Option<String>,
    pub manual: Option<ThemePalette>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub display: DisplayConfig,
    pub sensors: SensorsConfig,
    pub theme: ThemeConfig,
}

impl Config {
    /// Load config from the given path. Returns defaults if the file doesn't exist.
    /// Returns an error (with the file path in the message) if the file exists but is invalid TOML.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;
        toml::from_str(&contents)
            .with_context(|| format!("Invalid TOML in config file: {}", path.display()))
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

