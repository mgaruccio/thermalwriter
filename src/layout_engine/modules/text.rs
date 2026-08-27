//! The bounded Text scene emitter.

use serde::{Deserialize, Serialize};

use super::{
    BindingValue, ModuleCapabilities, ModuleEmitter, ResolvedBindings, ThemeTokens, bounded_text,
    emission_diagnostic, inset_rect, validate_bounds,
};
use crate::layout_engine::LayoutDiagnostic;
#[cfg(test)]
use crate::layout_engine::scene::Rect;
use crate::layout_engine::scene::{
    MAX_TEXT_CONTENT, MIN_OPACITY, MIN_TEXT_SIZE, RectNode, SceneNode, TextAlignment, TextNode,
    TextRole,
};
use crate::layout_engine::solver::SolvedModule;

/// A text module is limited to semantic roles rather than arbitrary CSS.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextModule {
    pub content: String,
    pub binding: Option<String>,
    pub role: TextRole,
    pub alignment: TextAlignment,
}

impl TextModule {
    /// Construct static text.  Content is bounded before it enters the scene.
    pub fn new(content: impl Into<String>, role: TextRole, alignment: TextAlignment) -> Self {
        Self {
            content: bounded_text(&content.into()),
            binding: None,
            role,
            alignment,
        }
    }

    /// Construct text backed by a runtime value with a bounded fallback.
    pub fn bound(
        binding: impl Into<String>,
        fallback: impl Into<String>,
        role: TextRole,
        alignment: TextAlignment,
    ) -> Self {
        Self {
            content: bounded_text(&fallback.into()),
            binding: Some(binding.into()),
            role,
            alignment,
        }
    }

    /// Fallible constructor for callers that prefer diagnostics over the
    /// constructor's deterministic truncation behavior.
    pub fn try_new(
        content: impl Into<String>,
        role: TextRole,
        alignment: TextAlignment,
    ) -> Result<Self, LayoutDiagnostic> {
        let content = content.into();
        if content.chars().count() > MAX_TEXT_CONTENT {
            return Err(emission_diagnostic(
                "text",
                "text content exceeds the bounded scene limit",
                "Shorten the text to 160 characters or fewer",
            ));
        }
        Ok(Self::new(content, role, alignment))
    }

    pub fn with_binding(mut self, binding: impl Into<String>) -> Self {
        self.binding = Some(binding.into());
        self
    }

    fn capabilities_for_role(&self) -> ModuleCapabilities {
        ModuleCapabilities {
            can_span_bridge: false,
            supports_binding: true,
            supports_threshold: false,
            supports_variants: false,
        }
    }

    fn resolve_content(&self, data: &ResolvedBindings) -> (String, bool) {
        let Some(binding) = self.binding.as_deref() else {
            return (bounded_text(&self.content), false);
        };
        let Some(value) = data.get(binding) else {
            return ("--".to_owned(), true);
        };
        if matches!(value, BindingValue::Number(number) if !number.is_finite()) {
            return ("--".to_owned(), true);
        }
        let content = bounded_text(&value.display_value());
        if content.is_empty() || (content == "--" && matches!(value, BindingValue::Number(_))) {
            ("--".to_owned(), true)
        } else {
            (content, false)
        }
    }
}

impl ModuleEmitter for TextModule {
    fn capabilities(&self) -> ModuleCapabilities {
        self.capabilities_for_role()
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
        if self
            .binding
            .as_deref()
            .is_some_and(|binding| binding.trim().is_empty())
        {
            return Err(emission_diagnostic(
                &solved.id,
                "text binding cannot be empty",
                "Remove the binding for static text or use a supported runtime key",
            ));
        }
        if self.content.chars().count() > MAX_TEXT_CONTENT {
            return Err(emission_diagnostic(
                &solved.id,
                "text content exceeds the bounded scene limit",
                "Shorten the text to 160 characters or fewer",
            ));
        }

        let (content, unavailable) = self.resolve_content(data);
        let bounds = solved.bounds;
        let padding = 12.min(bounds.width / 8).min(bounds.height / 8);
        let text_bounds = inset_rect(bounds, padding);
        let color = if unavailable {
            theme.unavailable
        } else {
            theme.color_for_role(self.role)
        };
        let font_size = theme.size_for_role(self.role).max(MIN_TEXT_SIZE);

        Ok(vec![
            SceneNode::Rect(RectNode::new(bounds, theme.panel, theme.panel_opacity)),
            SceneNode::Text(TextNode::new(
                text_bounds,
                content,
                self.role,
                self.alignment,
                color,
                font_size,
                theme.opacity.max(MIN_OPACITY),
            )),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_engine::scene::{SceneNode, TextAlignment, TextRole};

    fn solved() -> SolvedModule {
        SolvedModule {
            id: "status".to_owned(),
            bounds: Rect::new(16, 16, 448, 172),
            zone: None,
        }
    }

    #[test]
    fn text_capabilities_are_local_and_do_not_advertise_thresholds() {
        let text = TextModule::new("online", TextRole::Status, TextAlignment::Center);
        assert_eq!(
            text.capabilities(),
            ModuleCapabilities {
                can_span_bridge: false,
                supports_binding: true,
                supports_threshold: false,
                supports_variants: false,
            }
        );
    }

    #[test]
    fn text_emits_panel_then_text_in_deterministic_order() {
        let text = TextModule::new("online", TextRole::Status, TextAlignment::Center);
        let nodes = text
            .emit(
                &solved(),
                &ResolvedBindings::default(),
                &ThemeTokens::default(),
            )
            .expect("text scene");
        assert_eq!(nodes.len(), 2);
        assert!(matches!(nodes[0], SceneNode::Rect(_)));
        let SceneNode::Text(node) = &nodes[1] else {
            unreachable!()
        };
        assert_eq!(node.content, "online");
        assert_eq!(node.role, TextRole::Status);
        assert_eq!(node.alignment, TextAlignment::Center);
        assert!(node.font_size >= MIN_TEXT_SIZE);
        assert!(node.opacity >= MIN_OPACITY);
    }

    #[test]
    fn missing_bound_text_is_a_stable_unavailable_state() {
        let text = TextModule::bound("host.name", "unknown", TextRole::Body, TextAlignment::Start);
        let nodes = text
            .emit(
                &solved(),
                &ResolvedBindings::default(),
                &ThemeTokens::default(),
            )
            .expect("missing binding is rendered");
        let SceneNode::Text(node) = &nodes[1] else {
            unreachable!()
        };
        assert_eq!(node.content, "--");
        assert_eq!(node.color, ThemeTokens::default().unavailable);
    }

    #[test]
    fn bound_text_uses_runtime_value_and_keeps_content_bounded() {
        let text = TextModule::bound("host.name", "unknown", TextRole::Body, TextAlignment::Start);
        let value = "a".repeat(MAX_TEXT_CONTENT + 20);
        let data = ResolvedBindings::new().with_value("host.name", value);
        let nodes = text
            .emit(&solved(), &data, &ThemeTokens::default())
            .expect("bound text scene");
        let SceneNode::Text(node) = &nodes[1] else {
            unreachable!()
        };
        assert_eq!(node.content.chars().count(), MAX_TEXT_CONTENT);
    }

    #[test]
    fn try_new_rejects_unbounded_content() {
        let error = TextModule::try_new(
            "x".repeat(MAX_TEXT_CONTENT + 1),
            TextRole::Body,
            TextAlignment::Start,
        )
        .expect_err("unbounded content must be rejected by fallible constructor");
        assert!(error.reason.contains("bounded scene limit"));
    }
}
