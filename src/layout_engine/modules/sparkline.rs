//! The bounded, backend-neutral Sparkline scene emitter.
//!
//! A sparkline consumes a history binding supplied by the runtime and turns
//! finite samples into a clipped [`PathNode`].  It intentionally does not know
//! about SensorHistory, SVG, or any other renderer implementation.

use serde::{Deserialize, Serialize};
use std::fmt;

use super::{
    ModuleCapabilities, ModuleEmitter, ResolvedBindings, ThemeTokens, emission_diagnostic,
    validate_bounds,
};
use crate::layout_engine::LayoutDiagnostic;
use crate::layout_engine::scene::{
    ClipNode, MIN_OPACITY, PathNode, Point, Rect, RectNode, SceneNode, TextAlignment, TextNode,
    TextRole,
};
use crate::layout_engine::solver::SolvedModule;

/// A typed key for one of the histories exposed by the Thermalwriter runtime.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HistoryBinding(String);

impl HistoryBinding {
    /// Construct a history binding without consulting a renderer or sensor.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Return the runtime key used to resolve this history.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this binding has no runtime key.
    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl fmt::Display for HistoryBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AsRef<str> for HistoryBinding {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl From<&str> for HistoryBinding {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for HistoryBinding {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

/// Curated sparkline presentations.  Variants select bounded style tokens;
/// callers cannot inject renderer-specific CSS or path syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SparklineVariant {
    /// The default accent line.
    Default,
    /// An explicit line-only presentation.
    Line,
    /// An accent line with a bounded filled area.
    Area,
    /// A brighter, slightly heavier filled presentation for the flagship style.
    Neon,
    /// A lower-emphasis line using the theme's unit color.
    Muted,
}

impl Default for SparklineVariant {
    fn default() -> Self {
        Self::Default
    }
}

impl From<&str> for SparklineVariant {
    fn from(value: &str) -> Self {
        match value {
            "line" => Self::Line,
            "area" | "filled" => Self::Area,
            "neon" => Self::Neon,
            "muted" | "soft" => Self::Muted,
            _ => Self::Default,
        }
    }
}

impl From<String> for SparklineVariant {
    fn from(value: String) -> Self {
        Self::from(value.as_str())
    }
}

impl SparklineVariant {
    /// Return the stable variant name used by layout documents and inspectors.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Line => "line",
            Self::Area => "area",
            Self::Neon => "neon",
            Self::Muted => "muted",
        }
    }
}

/// Optional numeric bounds for y-axis normalization.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ValueRange {
    pub min: f64,
    pub max: f64,
}

impl ValueRange {
    pub const fn new(min: f64, max: f64) -> Self {
        Self { min, max }
    }

    /// Return whether these bounds can be used for normalization.
    pub fn is_valid(self) -> bool {
        self.min.is_finite() && self.max.is_finite() && self.min <= self.max
    }
}

impl From<(f64, f64)> for ValueRange {
    fn from((min, max): (f64, f64)) -> Self {
        Self::new(min, max)
    }
}

/// Resolved style metadata exposed to a future GUI inspector.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SparklineStyle {
    pub stroke: super::Color,
    pub fill: Option<super::Color>,
    pub stroke_width: f32,
    pub opacity: f32,
    pub closed: bool,
}

/// A typed history visualization module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SparklineModule {
    pub binding: HistoryBinding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<ValueRange>,
    pub variant: SparklineVariant,
}

impl SparklineModule {
    pub fn new(binding: impl Into<HistoryBinding>) -> Self {
        Self {
            binding: binding.into(),
            range: None,
            variant: SparklineVariant::Default,
        }
    }

    pub fn with_range(mut self, range: impl Into<ValueRange>) -> Self {
        self.range = Some(range.into());
        self
    }

    pub fn with_bounds(mut self, min: f64, max: f64) -> Self {
        self.range = Some(ValueRange::new(min, max));
        self
    }

    pub fn with_variant(mut self, variant: impl Into<SparklineVariant>) -> Self {
        self.variant = variant.into();
        self
    }

    fn default_capabilities() -> ModuleCapabilities {
        ModuleCapabilities {
            can_span_bridge: false,
            supports_binding: true,
            supports_threshold: false,
            supports_variants: true,
        }
    }

    /// Return bounded style metadata for the future inspector.
    pub fn style_metadata(&self, theme: &ThemeTokens) -> SparklineStyle {
        let (stroke, fill, stroke_width, closed) = match self.variant {
            SparklineVariant::Default | SparklineVariant::Line => (theme.accent, None, 2.0, false),
            SparklineVariant::Area => (theme.accent, Some(theme.accent), 2.0, true),
            SparklineVariant::Neon => (theme.accent, Some(theme.accent), 3.0, true),
            SparklineVariant::Muted => (theme.unit, None, 2.0, false),
        };
        SparklineStyle {
            stroke,
            fill,
            stroke_width,
            opacity: theme.opacity.max(MIN_OPACITY),
            closed,
        }
    }

    /// Alias suitable for inspector callers that call the value a style.
    pub fn style(&self, theme: &ThemeTokens) -> SparklineStyle {
        self.style_metadata(theme)
    }

    fn normalized_points(
        &self,
        bounds: Rect,
        values: &[f64],
    ) -> Result<Vec<Point>, LayoutDiagnostic> {
        let (min, max) = match self.range {
            Some(range) if !range.is_valid() => {
                return Err(emission_diagnostic(
                    "sparkline",
                    "sparkline range must contain finite ordered bounds",
                    "Remove the range for automatic bounds or set finite min/max values",
                ));
            }
            Some(range) => (range.min, range.max),
            None => {
                let min = values.iter().copied().fold(f64::INFINITY, f64::min);
                let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                (min, max)
            }
        };

        let left = bounds.x as f32;
        let right = bounds.right() as f32;
        let top = bounds.y as f32;
        let bottom = bounds.bottom() as f32;
        let width = right - left;
        let height = bottom - top;

        Ok(values
            .iter()
            .copied()
            .enumerate()
            .map(|(index, value)| {
                let x = if values.len() <= 1 {
                    left
                } else {
                    left + width * (index as f32 / (values.len() - 1) as f32)
                };
                let normalized = normalize_value(value, min, max);
                let y = bottom - height * normalized;
                Point::new(x.clamp(left, right), y.clamp(top, bottom))
            })
            .collect())
    }
}

impl ModuleEmitter for SparklineModule {
    fn capabilities(&self) -> ModuleCapabilities {
        Self::default_capabilities()
    }

    fn emit(
        &self,
        solved: &SolvedModule,
        data: &ResolvedBindings,
        theme: &ThemeTokens,
    ) -> Result<Vec<SceneNode>, LayoutDiagnostic> {
        validate_bounds(&solved.id, solved.bounds)?;
        theme.validate().map_err(|mut diagnostic| {
            diagnostic.module_id = Some(solved.id.clone());
            diagnostic
        })?;
        if self.binding.is_empty() {
            return Err(emission_diagnostic(
                &solved.id,
                "sparkline binding cannot be empty",
                "Bind the sparkline to a supported sensor history key",
            ));
        }

        let values = data
            .history(self.binding.as_str())
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(|value| value.is_finite())
            .collect::<Vec<_>>();
        let points = self
            .normalized_points(solved.bounds, &values)
            .map_err(|mut diagnostic| {
                diagnostic.module_id = Some(solved.id.clone());
                diagnostic
            })?;
        let style = self.style_metadata(theme);
        let mut path_points = points.clone();
        let closed = style.closed && !points.is_empty();
        if closed {
            let bottom = solved.bounds.bottom() as f32;
            let left = solved.bounds.x as f32;
            let right = solved.bounds.right() as f32;
            path_points.extend([Point::new(right, bottom), Point::new(left, bottom)]);
        }

        let mut path = PathNode::new(
            solved.bounds,
            path_points,
            style.stroke,
            style.stroke_width,
            style.opacity,
        );
        path.fill = style.fill;
        path.closed = closed;

        let mut nodes = vec![
            SceneNode::Rect(RectNode::new(
                solved.bounds,
                theme.panel,
                theme.panel_opacity,
            )),
            // Keep the clipping operation adjacent to the path so every
            // renderer can enforce the same module boundary.
            SceneNode::Clip(ClipNode::new(solved.bounds)),
            SceneNode::Path(path),
        ];

        if values.is_empty() {
            nodes.push(SceneNode::Text(TextNode::new(
                solved.bounds,
                "--",
                TextRole::Status,
                TextAlignment::Center,
                theme.unavailable,
                theme.status_size,
                theme.opacity.max(MIN_OPACITY),
            )));
        }

        Ok(nodes)
    }
}

fn normalize_value(value: f64, min: f64, max: f64) -> f32 {
    if min == max {
        return 0.5;
    }

    let span = max - min;
    let normalized = if span.is_finite() && span > 0.0 {
        (value.clamp(min, max) - min) / span
    } else {
        // Avoid overflow for finite values such as [-f64::MAX, f64::MAX].
        let scale = min.abs().max(max.abs());
        if scale == 0.0 || !scale.is_finite() {
            0.5
        } else {
            let scaled_min = min / scale;
            let scaled_max = max / scale;
            let scaled_value = value.clamp(min, max) / scale;
            let scaled_span = scaled_max - scaled_min;
            if scaled_span > 0.0 && scaled_span.is_finite() {
                (scaled_value - scaled_min) / scaled_span
            } else {
                0.5
            }
        }
    };

    normalized.clamp(0.0, 1.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_engine::scene::SceneNode;

    fn solved() -> SolvedModule {
        SolvedModule {
            id: "history".to_owned(),
            bounds: Rect::new(16, 20, 100, 50),
            zone: None,
        }
    }

    fn path(nodes: &[SceneNode]) -> &PathNode {
        nodes
            .iter()
            .find_map(|node| match node {
                SceneNode::Path(path) => Some(path),
                _ => None,
            })
            .expect("sparkline path")
    }

    #[test]
    fn sparkline_capabilities_and_style_are_inspector_ready() {
        let sparkline =
            SparklineModule::new("cpu.temperature.history").with_variant(SparklineVariant::Neon);
        assert_eq!(
            sparkline.capabilities(),
            ModuleCapabilities {
                can_span_bridge: false,
                supports_binding: true,
                supports_threshold: false,
                supports_variants: true,
            }
        );
        let style = sparkline.style_metadata(&ThemeTokens::default());
        assert_eq!(style.fill, Some(ThemeTokens::default().accent));
        assert!(style.closed);
        assert!(style.stroke_width.is_finite());
    }

    #[test]
    fn fixed_history_has_deterministic_clipped_points() {
        let sparkline = SparklineModule::new("cpu.temperature.history")
            .with_variant(SparklineVariant::Line)
            .with_bounds(0.0, 100.0);
        let data =
            ResolvedBindings::new().with_history("cpu.temperature.history", [0.0, 50.0, 100.0]);
        let nodes = sparkline
            .emit(&solved(), &data, &ThemeTokens::default())
            .expect("sparkline scene");
        assert_eq!(
            path(&nodes).points,
            vec![
                Point::new(16.0, 70.0),
                Point::new(66.0, 45.0),
                Point::new(116.0, 20.0),
            ]
        );
        assert!(path(&nodes).points.iter().all(|point| {
            point.x >= solved().bounds.x as f32
                && point.x <= solved().bounds.right() as f32
                && point.y >= solved().bounds.y as f32
                && point.y <= solved().bounds.bottom() as f32
        }));
    }

    #[test]
    fn empty_singleton_constant_and_non_finite_histories_are_safe() {
        let sparkline = SparklineModule::new("history");
        let cases = [
            Vec::new(),
            vec![42.0],
            vec![42.0, 42.0],
            vec![f64::NAN, f64::INFINITY, 7.0],
            vec![f64::NAN, f64::INFINITY],
        ];
        for values in cases {
            let data = ResolvedBindings::new().with_history("history", values.clone());
            let nodes = sparkline
                .emit(&solved(), &data, &ThemeTokens::default())
                .expect("history edge case should emit");
            let graph = path(&nodes);
            assert!(graph.points.iter().all(|point| {
                point.x.is_finite()
                    && point.y.is_finite()
                    && point.x >= solved().bounds.x as f32
                    && point.x <= solved().bounds.right() as f32
                    && point.y >= solved().bounds.y as f32
                    && point.y <= solved().bounds.bottom() as f32
            }));
            if values.iter().all(|value| !value.is_finite()) || values.is_empty() {
                assert!(matches!(nodes.last(), Some(SceneNode::Text(_))));
                assert!(graph.points.is_empty());
            }
        }
    }

    #[test]
    fn area_variant_closes_inside_module_bounds() {
        let sparkline = SparklineModule::new("history").with_variant(SparklineVariant::Area);
        let data = ResolvedBindings::new().with_history("history", [1.0, 2.0, 3.0]);
        let nodes = sparkline
            .emit(&solved(), &data, &ThemeTokens::default())
            .expect("area scene");
        let graph = path(&nodes);
        assert!(graph.closed);
        assert_eq!(graph.points.len(), 5);
        assert!(graph.points.iter().all(|point| {
            point.x >= solved().bounds.x as f32
                && point.x <= solved().bounds.right() as f32
                && point.y >= solved().bounds.y as f32
                && point.y <= solved().bounds.bottom() as f32
        }));
    }

    #[test]
    fn invalid_range_and_zero_bounds_report_diagnostics() {
        let invalid = SparklineModule::new("history").with_bounds(5.0, -5.0);
        let error = invalid
            .emit(
                &solved(),
                &ResolvedBindings::new().with_history("history", [1.0]),
                &ThemeTokens::default(),
            )
            .expect_err("inverted range must fail");
        assert!(error.reason.contains("range"));

        let mut zero = solved();
        zero.bounds.width = 0;
        let error = SparklineModule::new("history")
            .emit(&zero, &ResolvedBindings::default(), &ThemeTokens::default())
            .expect_err("zero bounds must fail");
        assert!(error.reason.contains("positive width"));
    }
}
