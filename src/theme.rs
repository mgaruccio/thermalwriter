use serde::{Deserialize, Serialize};
use anyhow::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemePalette {
    pub primary: String,
    pub secondary: String,
    pub accent: String,
    pub background: String,
    pub surface: String,
    pub text: String,
    pub text_dim: String,
    pub success: String,
    pub warning: String,
    pub critical: String,
}

impl Default for ThemePalette {
    fn default() -> Self {
        Self {
            primary: "#e94560".to_string(),
            secondary: "#53d8fb".to_string(),
            accent: "#20f5d8".to_string(),
            background: "#08080f".to_string(),
            surface: "#12121e".to_string(),
            text: "#e0e0e0".to_string(),
            text_dim: "#888888".to_string(),
            success: "#00ff88".to_string(),
            warning: "#ffaa00".to_string(),
            critical: "#ff3333".to_string(),
        }
    }
}

impl ThemePalette {
    /// Inject all theme colors into a Tera context as theme_primary, theme_secondary, etc.
    pub fn inject_into_context(&self, context: &mut tera::Context) {
        context.insert("theme_primary", &self.primary);
        context.insert("theme_secondary", &self.secondary);
        context.insert("theme_accent", &self.accent);
        context.insert("theme_background", &self.background);
        context.insert("theme_surface", &self.surface);
        context.insert("theme_text", &self.text);
        context.insert("theme_text_dim", &self.text_dim);
        context.insert("theme_success", &self.success);
        context.insert("theme_warning", &self.warning);
        context.insert("theme_critical", &self.critical);
    }
}

pub trait ThemeSource: Send {
    fn name(&self) -> &str;
    fn load(&self) -> Result<ThemePalette>;
}

pub struct DefaultThemeSource;

impl ThemeSource for DefaultThemeSource {
    fn name(&self) -> &str {
        "default"
    }
    fn load(&self) -> Result<ThemePalette> {
        Ok(ThemePalette::default())
    }
}

pub struct ManualThemeSource {
    palette: ThemePalette,
}

impl ManualThemeSource {
    pub fn new(palette: ThemePalette) -> Self {
        Self { palette }
    }
}

impl ThemeSource for ManualThemeSource {
    fn name(&self) -> &str {
        "manual"
    }
    fn load(&self) -> Result<ThemePalette> {
        Ok(self.palette.clone())
    }
}
