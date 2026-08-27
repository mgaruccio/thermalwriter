//! Backend-neutral scene primitives emitted by typed layout modules.
//!
//! The scene is intentionally small and owns only display geometry and typed
//! presentation data.  Renderer backends may translate these values into SVG,
//! pixels, or another target without making the layout document depend on a
//! renderer implementation.

use serde::{Deserialize, Serialize};

pub use super::solver::Rect;

/// Minimum opacity for content that is intended to remain legible on a washed-
/// out LCD panel.
pub const MIN_OPACITY: f32 = 0.7;

/// Minimum font size for readable LCD labels and values.
pub const MIN_TEXT_SIZE: u32 = 14;

/// Minimum per-channel foreground floor from the layout design guidance.
pub const MIN_FOREGROUND_CHANNEL: u8 = 0x99;

/// Maximum number of Unicode scalar values in a text node emitted by a module.
pub const MAX_TEXT_CONTENT: usize = 160;

/// A display color independent of any renderer color type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Color {
    pub const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }

    /// Return whether every channel meets the LCD foreground floor.
    pub const fn meets_lcd_floor(self) -> bool {
        self.red >= MIN_FOREGROUND_CHANNEL
            && self.green >= MIN_FOREGROUND_CHANNEL
            && self.blue >= MIN_FOREGROUND_CHANNEL
    }

    /// Return a stable `#rrggbb` representation for diagnostics and tests.
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.red, self.green, self.blue)
    }

    /// Parse a six-digit `#rrggbb` color without accepting CSS extensions.
    pub fn from_hex(value: &str) -> Option<Self> {
        let value = value.strip_prefix('#').unwrap_or(value);
        if value.len() != 6 || !value.is_ascii() {
            return None;
        }
        let red = u8::from_str_radix(&value[0..2], 16).ok()?;
        let green = u8::from_str_radix(&value[2..4], 16).ok()?;
        let blue = u8::from_str_radix(&value[4..6], 16).ok()?;
        Some(Self::rgb(red, green, blue))
    }
}

/// Semantic role used by a text node.  Backends choose fonts and glyph
/// handling, while the scene keeps the role bounded and renderer-neutral.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum TextRole {
    Title,
    #[default]
    Body,
    Label,
    Caption,
    Value,
    Unit,
    Status,
}

/// Logical text alignment.  `Start` and `End` avoid baking a writing direction
/// into the scene; the current LCD layouts use the horizontal direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum TextAlignment {
    #[default]
    Start,
    Center,
    End,
}

/// A filled axis-aligned rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RectNode {
    pub bounds: Rect,
    pub fill: Color,
    pub opacity: f32,
}

impl RectNode {
    pub const fn new(bounds: Rect, fill: Color, opacity: f32) -> Self {
        Self {
            bounds,
            fill,
            opacity,
        }
    }
}

/// A renderer-neutral point used by path nodes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// A bounded polyline or polygon path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathNode {
    pub bounds: Rect,
    pub points: Vec<Point>,
    pub stroke: Color,
    pub fill: Option<Color>,
    pub stroke_width: f32,
    pub opacity: f32,
    pub closed: bool,
}

impl PathNode {
    pub fn new(
        bounds: Rect,
        points: Vec<Point>,
        stroke: Color,
        stroke_width: f32,
        opacity: f32,
    ) -> Self {
        Self {
            bounds,
            points,
            stroke,
            fill: None,
            stroke_width,
            opacity,
            closed: false,
        }
    }
}

/// A text run with a bounded box and semantic presentation metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextNode {
    pub bounds: Rect,
    pub content: String,
    pub role: TextRole,
    pub alignment: TextAlignment,
    pub color: Color,
    pub font_size: u32,
    pub opacity: f32,
}

impl TextNode {
    pub fn new(
        bounds: Rect,
        content: impl Into<String>,
        role: TextRole,
        alignment: TextAlignment,
        color: Color,
        font_size: u32,
        opacity: f32,
    ) -> Self {
        Self {
            bounds,
            content: content.into(),
            role,
            alignment,
            color,
            font_size,
            opacity,
        }
    }
}

/// A bounded image reference.  The reference is an opaque logical asset name;
/// decoding and file policy belong to a later media backend/module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageNode {
    pub bounds: Rect,
    pub source: String,
    pub fit: ImageFit,
    pub opacity: f32,
}

impl ImageNode {
    pub fn new(bounds: Rect, source: impl Into<String>, fit: ImageFit, opacity: f32) -> Self {
        Self {
            bounds,
            source: source.into(),
            fit,
            opacity,
        }
    }
}

/// How an image fills its bounded scene rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum ImageFit {
    #[default]
    Contain,
    Cover,
}

/// A clipping instruction for following backend operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClipNode {
    pub bounds: Rect,
}

impl ClipNode {
    pub const fn new(bounds: Rect) -> Self {
        Self { bounds }
    }
}

/// The complete native-resolution scene passed from the layout engine to a
/// renderer backend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    pub width: u32,
    pub height: u32,
    pub nodes: Vec<SceneNode>,
}

impl Scene {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            nodes: Vec::new(),
        }
    }

    pub fn with_nodes(width: u32, height: u32, nodes: Vec<SceneNode>) -> Self {
        Self {
            width,
            height,
            nodes,
        }
    }

    pub fn push(&mut self, node: SceneNode) {
        self.nodes.push(node);
    }
}

/// Typed scene primitives.  No renderer-specific object is stored here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "node", rename_all = "kebab-case")]
pub enum SceneNode {
    Rect(RectNode),
    Path(PathNode),
    Text(TextNode),
    Image(ImageNode),
    Clip(ClipNode),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_preserves_native_dimensions_and_node_order() {
        let mut scene = Scene::new(480, 480);
        let bounds = Rect::new(16, 16, 448, 172);
        scene.push(SceneNode::Rect(RectNode::new(
            bounds,
            Color::rgb(0x18, 0x20, 0x2c),
            0.8,
        )));
        scene.push(SceneNode::Text(TextNode::new(
            bounds,
            "CPU",
            TextRole::Label,
            TextAlignment::Start,
            Color::rgb(0xbb, 0xbb, 0xbb),
            MIN_TEXT_SIZE,
            MIN_OPACITY,
        )));

        assert_eq!((scene.width, scene.height), (480, 480));
        assert!(matches!(scene.nodes[0], SceneNode::Rect(_)));
        assert!(matches!(scene.nodes[1], SceneNode::Text(_)));
    }

    #[test]
    fn color_floor_is_exact_and_hex_round_trips() {
        assert!(!Color::rgb(0x98, 0xff, 0xff).meets_lcd_floor());
        assert!(Color::rgb(0x99, 0x99, 0x99).meets_lcd_floor());
        assert_eq!(Color::from_hex("#9aB0ff").unwrap().to_hex(), "#9ab0ff");
    }
}
