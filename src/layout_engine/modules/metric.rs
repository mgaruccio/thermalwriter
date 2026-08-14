//! The bounded Metric scene emitter.

use serde::{Deserialize, Serialize};

use super::{
    BindingValue, ModuleCapabilities, ModuleEmitter, ResolvedBindings, ThemeTokens, bounded_text,
    emission_diagnostic, format_number, inset_rect, validate_bounds,
};
use crate::layout_engine::LayoutDiagnostic;
use crate::layout_engine::scene::{
    MIN_OPACITY, MIN_TEXT_SIZE, Rect, RectNode, SceneNode, TextAlignment, TextNode, TextRole,
};
use crate::layout_engine::solver::SolvedModule;

/// Curated Metric presentation variants.  They change only bounded token
/// choices; an owner cannot inject renderer-specific style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricVariant {
    Default,
    Hero,
    Compact,
    Status,
}

impl Default for MetricVariant {
    fn default() -> Self {
        Self::Default
    }
}

impl From<&str> for MetricVariant {
    fn from(value: &str) -> Self {
        match value {
            "hero" => Self::Hero,
            "compact" => Self::Compact,
            "status" => Self::Status,
            _ => Self::Default,
        }
    }
}

impl From<String> for MetricVariant {
    fn from(value: String) -> Self {
        Self::from(value.as_str())
    }
}

impl MetricVariant {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Hero => "hero",
            Self::Compact => "compact",
            Self::Status => "status",
        }
    }

    fn value_size(self, theme: &ThemeTokens) -> u32 {
        match self {
            Self::Default => theme.value_size,
            Self::Hero => theme.value_size.saturating_add(8),
            Self::Compact => theme.value_size.saturating_sub(10).max(MIN_TEXT_SIZE),
            Self::Status => theme.status_size,
        }
    }
}

/// A numeric/status metric configured with a binding and a small presentation
/// vocabulary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricModule {
    pub label: String,
    pub binding: String,
    pub unit: String,
    pub threshold: Option<f64>,
    pub variant: MetricVariant,
}

impl MetricModule {
    pub fn new(
        label: impl Into<String>,
        binding: impl Into<String>,
        unit: impl Into<String>,
    ) -> Self {
        Self {
            label: bounded_text(&label.into()),
            binding: binding.into(),
            unit: bounded_text(&unit.into()),
            threshold: None,
            variant: MetricVariant::Default,
        }
    }

    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.threshold = Some(threshold);
        self
    }

    pub fn with_variant(mut self, variant: impl Into<MetricVariant>) -> Self {
        self.variant = variant.into();
        self
    }

    fn default_capabilities() -> ModuleCapabilities {
        ModuleCapabilities {
            can_span_bridge: false,
            supports_binding: true,
            supports_threshold: true,
            supports_variants: true,
        }
    }

    fn resolve_value<'a>(&self, data: &'a ResolvedBindings) -> (String, Option<f64>, bool) {
        let Some(binding) = data.get(&self.binding) else {
            return ("--".to_owned(), None, true);
        };

        match binding {
            BindingValue::Number(value) if value.is_finite() => {
                (format_number(*value), Some(*value), false)
            }
            BindingValue::Text(value) if !value.trim().is_empty() => {
                let trimmed = value.trim();
                match trimmed.parse::<f64>() {
                    Ok(numeric) if numeric.is_finite() => {
                        (format_number(numeric), Some(numeric), false)
                    }
                    Ok(_) => ("--".to_owned(), None, true),
                    Err(_) => (bounded_text(value), None, false),
                }
            }
            BindingValue::Boolean(value) => (value.to_string(), None, false),
            BindingValue::Number(_) | BindingValue::Text(_) => ("--".to_owned(), None, true),
        }
    }

    fn scene_nodes(
        &self,
        solved: &SolvedModule,
        theme: &ThemeTokens,
        value: String,
        numeric_value: Option<f64>,
        unavailable: bool,
    ) -> Vec<SceneNode> {
        let bounds = solved.bounds;
        let padding = 12.min(bounds.width / 8).min(bounds.height / 8);
        let inner = inset_rect(bounds, padding);
        let label_size = theme.label_size.max(MIN_TEXT_SIZE);
        let value_size = self.variant.value_size(theme).max(MIN_TEXT_SIZE);
        let unit_size = theme.unit_size.max(MIN_TEXT_SIZE);
        let gap = 6.min(inner.height / 12);

        let label_bounds = Rect::new(inner.x, inner.y, inner.width, label_size.min(inner.height));
        let unit_y = inner.bottom().saturating_sub(unit_size.min(inner.height));
        let value_y = label_bounds
            .bottom()
            .saturating_add(gap)
            .min(unit_y.saturating_sub(gap));
        let value_bottom = unit_y.saturating_sub(gap);
        let value_bounds = Rect::new(
            inner.x,
            value_y,
            inner.width,
            value_bottom.saturating_sub(value_y),
        );
        let unit_bounds = Rect::new(inner.x, unit_y, inner.width, unit_size.min(inner.height));

        let value_color = if unavailable {
            theme.unavailable
        } else {
            theme.metric_value_color(numeric_value, self.threshold)
        };

        vec![
            SceneNode::Rect(RectNode::new(bounds, theme.panel, theme.panel_opacity)),
            SceneNode::Text(TextNode::new(
                label_bounds,
                bounded_text(&self.label),
                TextRole::Label,
                TextAlignment::Start,
                theme.label,
                label_size,
                theme.opacity.max(MIN_OPACITY),
            )),
            SceneNode::Text(TextNode::new(
                value_bounds,
                value,
                TextRole::Value,
                TextAlignment::Start,
                value_color,
                value_size,
                theme.opacity.max(MIN_OPACITY),
            )),
            SceneNode::Text(TextNode::new(
                unit_bounds,
                bounded_text(&self.unit),
                TextRole::Unit,
                TextAlignment::Start,
                theme.unit,
                unit_size,
                theme.opacity.max(MIN_OPACITY),
            )),
        ]
    }
}

impl ModuleEmitter for MetricModule {
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
        if self.binding.trim().is_empty() {
            return Err(emission_diagnostic(
                &solved.id,
                "metric binding cannot be empty",
                "Bind the metric to a supported runtime sensor key",
            ));
        }
        if self
            .threshold
            .is_some_and(|threshold| !threshold.is_finite())
        {
            return Err(emission_diagnostic(
                &solved.id,
                "metric threshold must be finite",
                "Remove the threshold or choose a finite numeric threshold",
            ));
        }
        if self.label.chars().count() > super::super::scene::MAX_TEXT_CONTENT
            || self.unit.chars().count() > super::super::scene::MAX_TEXT_CONTENT
        {
            return Err(emission_diagnostic(
                &solved.id,
                "metric label and unit must remain bounded",
                "Shorten the label and unit before rendering",
            ));
        }

        let (value, numeric_value, unavailable) = self.resolve_value(data);
        Ok(self.scene_nodes(solved, theme, value, numeric_value, unavailable))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_engine::scene::{Color, SceneNode};

    fn solved() -> SolvedModule {
        SolvedModule {
            id: "cpu-temp".to_owned(),
            bounds: Rect::new(16, 16, 448, 172),
            zone: None,
        }
    }

    #[test]
    fn metric_capabilities_are_local_and_threshold_aware() {
        let metric = MetricModule::new("CPU", "cpu.temperature", "°C");
        assert_eq!(
            metric.capabilities(),
            ModuleCapabilities {
                can_span_bridge: false,
                supports_binding: true,
                supports_threshold: true,
                supports_variants: true,
            }
        );
    }

    #[test]
    fn metric_emits_stable_rect_label_value_unit_order() {
        let metric = MetricModule::new("CPU", "cpu.temperature", "°C")
            .with_threshold(80.0)
            .with_variant(MetricVariant::Hero);
        let data = ResolvedBindings::new().with_value("cpu.temperature", 72.5);
        let nodes = metric
            .emit(&solved(), &data, &ThemeTokens::default())
            .expect("metric scene");

        assert_eq!(nodes.len(), 4);
        assert!(matches!(nodes[0], SceneNode::Rect(_)));
        assert!(matches!(nodes[1], SceneNode::Text(_)));
        assert!(matches!(nodes[2], SceneNode::Text(_)));
        assert!(matches!(nodes[3], SceneNode::Text(_)));
        let SceneNode::Text(value) = &nodes[2] else {
            unreachable!()
        };
        assert_eq!(value.content, "72.5");
        assert!(value.font_size >= MIN_TEXT_SIZE);
        assert!(value.opacity >= MIN_OPACITY);
    }

    #[test]
    fn missing_metric_binding_is_a_stable_unavailable_state() {
        let metric = MetricModule::new("CPU", "cpu.temperature", "°C");
        let nodes = metric
            .emit(
                &solved(),
                &ResolvedBindings::default(),
                &ThemeTokens::default(),
            )
            .expect("missing binding is rendered");
        let SceneNode::Text(value) = &nodes[2] else {
            unreachable!()
        };
        assert_eq!(value.content, "--");
        assert_eq!(value.color, ThemeTokens::default().unavailable);
    }

    #[test]
    fn metric_threshold_uses_critical_color_without_changing_node_order() {
        let metric = MetricModule::new("CPU", "cpu.temperature", "°C").with_threshold(80.0);
        let data = ResolvedBindings::new().with_value("cpu.temperature", 81.0);
        let nodes = metric
            .emit(&solved(), &data, &ThemeTokens::default())
            .expect("metric scene");
        let SceneNode::Text(value) = &nodes[2] else {
            unreachable!()
        };
        assert_eq!(value.color, ThemeTokens::default().critical);
    }

    #[test]
    fn metric_rejects_zero_bounds() {
        let mut module = solved();
        module.bounds.width = 0;
        let metric = MetricModule::new("CPU", "cpu.temperature", "°C");
        let error = metric
            .emit(
                &module,
                &ResolvedBindings::default(),
                &ThemeTokens::default(),
            )
            .expect_err("zero bounds are not renderable");
        assert!(error.reason.contains("positive width"));
    }

    #[allow(dead_code)]
    fn _color_floor_is_used(_: Color) {}
}
