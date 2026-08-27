//! Typed, bounded scene emitters shared by preview and daemon paths.
//!
//! Module implementations intentionally resolve only typed bindings and theme
//! tokens.  They do not know which renderer will consume the resulting scene.

use std::borrow::Borrow;
use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use super::diagnostic::{DiagnosticSeverity, LayoutDiagnostic};
pub use super::scene::{Color, TextAlignment, TextRole};
use super::scene::{MIN_FOREGROUND_CHANNEL, MIN_OPACITY, MIN_TEXT_SIZE};
use super::solver::SolvedModule;

pub mod media;
pub mod metric;
pub mod sparkline;
pub mod text;

pub use media::{MediaFit, MediaModule};
pub use metric::{MetricModule, MetricVariant};
pub use sparkline::{
    HistoryBinding, SparklineModule, SparklineStyle, SparklineVariant, ValueRange,
};
pub use text::TextModule;

/// Stable diagnostic code for module data/style failures.
pub const MODULE_DIAGNOSTIC_CODE: &str = "TWLAYOUT-E015";

/// A value resolved from a runtime binding without exposing the sensor or
/// renderer implementation to a module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BindingValue {
    Number(f64),
    Text(String),
    Boolean(bool),
}

impl BindingValue {
    /// Return a finite numeric value, including numeric sensor strings.
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(value) if value.is_finite() => Some(*value),
            Self::Text(value) => value
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite()),
            Self::Number(_) | Self::Boolean(_) => None,
        }
    }

    /// Return a stable display representation for a binding value.
    pub fn display_value(&self) -> String {
        match self {
            Self::Number(value) if value.is_finite() => format_number(*value),
            Self::Number(_) => "--".to_owned(),
            Self::Text(value) => value.clone(),
            Self::Boolean(value) => value.to_string(),
        }
    }
}

impl From<f64> for BindingValue {
    fn from(value: f64) -> Self {
        Self::Number(value)
    }
}

impl From<f32> for BindingValue {
    fn from(value: f32) -> Self {
        Self::Number(f64::from(value))
    }
}

impl From<i64> for BindingValue {
    fn from(value: i64) -> Self {
        Self::Number(value as f64)
    }
}

impl From<i32> for BindingValue {
    fn from(value: i32) -> Self {
        Self::Number(f64::from(value))
    }
}

impl From<u64> for BindingValue {
    fn from(value: u64) -> Self {
        Self::Number(value as f64)
    }
}

impl From<u32> for BindingValue {
    fn from(value: u32) -> Self {
        Self::Number(f64::from(value))
    }
}

impl From<bool> for BindingValue {
    fn from(value: bool) -> Self {
        Self::Boolean(value)
    }
}

impl From<String> for BindingValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for BindingValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

/// Runtime values available to a module emitter, kept ordered for stable
/// serialization and deterministic tests.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResolvedBindings {
    pub values: BTreeMap<String, BindingValue>,
    /// Runtime sensor histories keyed by their typed layout binding.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub histories: BTreeMap<String, Vec<f64>>,
}

impl ResolvedBindings {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(
        &mut self,
        key: impl Into<String>,
        value: impl Into<BindingValue>,
    ) -> Option<BindingValue> {
        self.values.insert(key.into(), value.into())
    }

    pub fn with_value(mut self, key: impl Into<String>, value: impl Into<BindingValue>) -> Self {
        self.insert(key, value);
        self
    }

    pub fn get(&self, key: &str) -> Option<&BindingValue> {
        self.values.get(key)
    }
    /// Insert a deterministic runtime history for a sparkline binding.
    pub fn insert_history<I, V>(&mut self, key: impl Into<String>, values: I) -> Option<Vec<f64>>
    where
        I: IntoIterator<Item = V>,
        V: Borrow<f64>,
    {
        self.histories.insert(
            key.into(),
            values.into_iter().map(|value| *value.borrow()).collect(),
        )
    }
    /// Add a history while retaining the builder-style binding API.
    pub fn with_history<I, V>(mut self, key: impl Into<String>, values: I) -> Self
    where
        I: IntoIterator<Item = V>,
        V: Borrow<f64>,
    {
        self.insert_history(key, values);
        self
    }
    /// Resolve a history without exposing the sensor-history implementation.
    pub fn history(&self, key: &str) -> Option<&[f64]> {
        self.histories.get(key).map(Vec::as_slice)
    }
    /// Alias for callers that use getter naming for runtime bindings.
    pub fn get_history(&self, key: &str) -> Option<&[f64]> {
        self.history(key)
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty() && self.histories.is_empty()
    }
}

impl FromIterator<(String, String)> for ResolvedBindings {
    fn from_iter<T: IntoIterator<Item = (String, String)>>(iter: T) -> Self {
        let mut bindings = Self::new();
        for (key, value) in iter {
            bindings.insert(key, value);
        }
        bindings
    }
}

impl<K, V, const N: usize> From<[(K, V); N]> for ResolvedBindings
where
    K: Into<String>,
    V: Into<BindingValue>,
{
    fn from(values: [(K, V); N]) -> Self {
        let mut bindings = Self::new();
        for (key, value) in values {
            bindings.insert(key, value);
        }
        bindings
    }
}

impl From<BTreeMap<String, String>> for ResolvedBindings {
    fn from(values: BTreeMap<String, String>) -> Self {
        values.into_iter().collect()
    }
}

impl From<HashMap<String, String>> for ResolvedBindings {
    fn from(values: HashMap<String, String>) -> Self {
        values.into_iter().collect()
    }
}

/// Curated capabilities exposed to the future GUI inspector and curved-surface
/// policy.  The fields are deliberately finite rather than an arbitrary style
/// or plugin map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleCapabilities {
    pub can_span_bridge: bool,
    pub supports_binding: bool,
    pub supports_threshold: bool,
    pub supports_variants: bool,
}

impl Default for ModuleCapabilities {
    fn default() -> Self {
        Self {
            can_span_bridge: false,
            supports_binding: true,
            supports_threshold: false,
            supports_variants: false,
        }
    }
}

/// Typed colors, opacity, and text sizes shared by all emitters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeTokens {
    /// Dark colors are intentionally allowed for the background and panel.
    pub background: Color,
    pub panel: Color,
    /// Foreground tokens must satisfy the washed-out LCD floor.
    pub label: Color,
    pub value: Color,
    pub unit: Color,
    pub accent: Color,
    pub warning: Color,
    pub critical: Color,
    pub unavailable: Color,
    pub opacity: f32,
    pub panel_opacity: f32,
    pub title_size: u32,
    pub body_size: u32,
    pub label_size: u32,
    pub caption_size: u32,
    pub value_size: u32,
    pub unit_size: u32,
    pub status_size: u32,
}

impl Default for ThemeTokens {
    fn default() -> Self {
        Self {
            background: Color::rgb(0x08, 0x0c, 0x14),
            panel: Color::rgb(0x17, 0x20, 0x2c),
            label: Color::rgb(0xbb, 0xc2, 0xcc),
            value: Color::rgb(0xf5, 0xf5, 0xef),
            unit: Color::rgb(0xb5, 0xbd, 0xc8),
            accent: Color::rgb(0xa0, 0xcc, 0xff),
            warning: Color::rgb(0xff, 0xd0, 0x99),
            critical: Color::rgb(0xff, 0xb0, 0xa8),
            unavailable: Color::rgb(0xaa, 0xaa, 0xaa),
            opacity: 0.92,
            panel_opacity: 0.84,
            title_size: 28,
            body_size: 20,
            label_size: 16,
            caption_size: 14,
            value_size: 44,
            unit_size: 18,
            status_size: 20,
        }
    }
}

impl ThemeTokens {
    /// Validate the hard readability floor before a module emits scene nodes.
    pub fn validate(&self) -> Result<(), LayoutDiagnostic> {
        for (name, color) in [
            ("label", self.label),
            ("value", self.value),
            ("unit", self.unit),
            ("accent", self.accent),
            ("warning", self.warning),
            ("critical", self.critical),
            ("unavailable", self.unavailable),
        ] {
            if !color.meets_lcd_floor() {
                return Err(style_diagnostic(
                    None,
                    format!("theme color `{name}` is below the LCD floor #999999"),
                    format!(
                        "Use a `{name}` color with every channel at least #{MIN_FOREGROUND_CHANNEL:02x}"
                    ),
                ));
            }
        }

        for (name, opacity) in [
            ("opacity", self.opacity),
            ("panel_opacity", self.panel_opacity),
        ] {
            if !opacity.is_finite() || !(MIN_OPACITY..=1.0).contains(&opacity) {
                return Err(style_diagnostic(
                    None,
                    format!("theme opacity `{name}` must be between {MIN_OPACITY:.1} and 1.0"),
                    format!("Set `{name}` to a finite value at least {MIN_OPACITY:.1}"),
                ));
            }
        }

        for (name, size) in [
            ("title_size", self.title_size),
            ("body_size", self.body_size),
            ("label_size", self.label_size),
            ("caption_size", self.caption_size),
            ("value_size", self.value_size),
            ("unit_size", self.unit_size),
            ("status_size", self.status_size),
        ] {
            if size < MIN_TEXT_SIZE {
                return Err(style_diagnostic(
                    None,
                    format!("theme text size `{name}` is below {MIN_TEXT_SIZE}px"),
                    format!("Set `{name}` to at least {MIN_TEXT_SIZE}px"),
                ));
            }
        }

        Ok(())
    }

    pub fn color_for_role(&self, role: TextRole) -> Color {
        match role {
            TextRole::Title | TextRole::Value => self.value,
            TextRole::Body => self.value,
            TextRole::Label => self.label,
            TextRole::Caption => self.unit,
            TextRole::Unit => self.unit,
            TextRole::Status => self.accent,
        }
    }

    pub fn size_for_role(&self, role: TextRole) -> u32 {
        match role {
            TextRole::Title => self.title_size,
            TextRole::Body => self.body_size,
            TextRole::Label => self.label_size,
            TextRole::Caption => self.caption_size,
            TextRole::Value => self.value_size,
            TextRole::Unit => self.unit_size,
            TextRole::Status => self.status_size,
        }
    }

    pub fn metric_value_color(&self, value: Option<f64>, threshold: Option<f64>) -> Color {
        match (value, threshold) {
            (Some(value), Some(threshold)) if value >= threshold => self.critical,
            _ => self.value,
        }
    }
}

/// A typed scene emitter for one solved module.
pub trait ModuleEmitter {
    fn capabilities(&self) -> ModuleCapabilities;

    fn emit(
        &self,
        solved: &SolvedModule,
        data: &ResolvedBindings,
        theme: &ThemeTokens,
    ) -> Result<Vec<super::scene::SceneNode>, LayoutDiagnostic>;
}

pub(crate) fn style_diagnostic(
    module_id: Option<&str>,
    reason: impl Into<String>,
    fix: impl Into<String>,
) -> LayoutDiagnostic {
    let mut diagnostic = LayoutDiagnostic::new(
        MODULE_DIAGNOSTIC_CODE,
        DiagnosticSeverity::Error,
        "Invalid layout module style",
        reason,
        fix,
    );
    diagnostic.module_id = module_id.map(str::to_owned);
    diagnostic
}

pub(crate) fn emission_diagnostic(
    module_id: &str,
    reason: impl Into<String>,
    fix: impl Into<String>,
) -> LayoutDiagnostic {
    style_diagnostic(Some(module_id), reason, fix)
}

pub(crate) fn format_number(value: f64) -> String {
    if !value.is_finite() {
        return "--".to_owned();
    }
    let mut formatted = format!("{value:.2}");
    while formatted.contains('.') && formatted.ends_with('0') {
        formatted.pop();
    }
    if formatted.ends_with('.') {
        formatted.pop();
    }
    formatted
}

pub(crate) fn bounded_text(value: &str) -> String {
    value.chars().take(super::scene::MAX_TEXT_CONTENT).collect()
}

pub(crate) fn validate_bounds(
    module_id: &str,
    bounds: super::scene::Rect,
) -> Result<(), LayoutDiagnostic> {
    if bounds.width == 0 || bounds.height == 0 {
        return Err(emission_diagnostic(
            module_id,
            "solved module bounds must have positive width and height",
            "Choose a recipe and display profile with enough space for this module",
        ));
    }
    Ok(())
}

pub(crate) fn inset_rect(bounds: super::scene::Rect, inset: u32) -> super::scene::Rect {
    let horizontal = inset.saturating_mul(2).min(bounds.width);
    let vertical = inset.saturating_mul(2).min(bounds.height);
    super::scene::Rect::new(
        bounds.x.saturating_add(inset.min(bounds.width)),
        bounds.y.saturating_add(inset.min(bounds.height)),
        bounds.width.saturating_sub(horizontal),
        bounds.height.saturating_sub(vertical),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_meets_lcd_readability_contract() {
        let theme = ThemeTokens::default();
        theme.validate().expect("default theme is readable");
        assert!(theme.label.meets_lcd_floor());
        assert!(theme.opacity >= MIN_OPACITY);
        assert!(theme.label_size >= MIN_TEXT_SIZE);
    }

    #[test]
    fn invalid_theme_reports_style_diagnostic() {
        let theme = ThemeTokens {
            label: Color::rgb(0x98, 0x99, 0x99),
            ..Default::default()
        };
        let error = theme.validate().expect_err("below-floor label must fail");
        assert_eq!(error.code, MODULE_DIAGNOSTIC_CODE);
        assert!(error.reason.contains("label"));
    }

    #[test]
    fn resolved_bindings_keep_values_typed_and_deterministic() {
        let bindings = ResolvedBindings::new()
            .with_value("cpu.temperature", 42.5)
            .with_value("host.name", "atlas");
        assert_eq!(
            bindings.get("cpu.temperature").unwrap().as_number(),
            Some(42.5)
        );
        assert_eq!(bindings.get("host.name").unwrap().display_value(), "atlas");
        assert_eq!(
            bindings
                .values
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["cpu.temperature", "host.name",]
        );
    }

    #[test]
    fn helper_inset_never_exits_bounds() {
        let outer = crate::layout_engine::scene::Rect::new(4, 8, 32, 20);
        let inner = inset_rect(outer, 12);
        assert!(outer.contains(inner));
    }
}
