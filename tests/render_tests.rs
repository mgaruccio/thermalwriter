use thermalwriter::render::parser::*;
use thermalwriter::render::layout::*;
use thermalwriter::render::{FrameSource, TemplateRenderer};
use thermalwriter::render::svg::SvgRenderer;
use thermalwriter::sensor::history::SensorHistory;
use thermalwriter::theme::ThemePalette;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[test]
fn parse_style_extracts_flex_properties() {
    let style = parse_style("display: flex; flex-direction: column; gap: 8px;");
    assert_eq!(style.display.as_deref(), Some("flex"));
    assert_eq!(style.flex_direction.as_deref(), Some("column"));
    assert_eq!(style.gap, Some(8.0));
}

#[test]
fn parse_style_extracts_colors() {
    let style = parse_style("color: #ff0000; background: #1a1a2e;");
    let color = style.color.unwrap();
    assert_eq!((color.r, color.g, color.b), (255, 0, 0));
    let bg = style.background.unwrap();
    assert_eq!((bg.r, bg.g, bg.b), (0x1a, 0x1a, 0x2e));
}

#[test]
fn parse_style_extracts_font_size() {
    let style = parse_style("font-size: 24px; font-family: monospace;");
    assert_eq!(style.font_size, Some(24.0));
    assert_eq!(style.font_family.as_deref(), Some("monospace"));
}

#[test]
fn parse_html_single_div_with_text() {
    let el = parse_html(r#"<div style="color: #fff;">Hello</div>"#).unwrap();
    assert_eq!(el.tag, "div");
    assert_eq!(el.text.as_deref(), Some("Hello"));
    assert_eq!(el.style.color.as_ref().unwrap().r, 255);
}

#[test]
fn parse_html_nested_elements() {
    let html = r#"<div style="display: flex;">
        <span>CPU 65C</span>
        <span>GPU 72C</span>
    </div>"#;
    let el = parse_html(html).unwrap();
    assert_eq!(el.tag, "div");
    assert_eq!(el.children.len(), 2);
    assert_eq!(el.children[0].text.as_deref(), Some("CPU 65C"));
    assert_eq!(el.children[1].text.as_deref(), Some("GPU 72C"));
}

#[test]
fn layout_single_element_fills_container() {
    let el = parse_html(r#"<div style="width: 480px; height: 480px;">Hello</div>"#).unwrap();
    let nodes = compute_layout(&el, 480.0, 480.0).unwrap();
    assert_eq!(nodes.len(), 1);
    assert!((nodes[0].x - 0.0).abs() < 1.0);
    assert!((nodes[0].y - 0.0).abs() < 1.0);
    assert!((nodes[0].width - 480.0).abs() < 1.0);
    assert!((nodes[0].height - 480.0).abs() < 1.0);
}

#[test]
fn layout_flex_column_stacks_children() {
    let html = r#"<div style="display: flex; flex-direction: column; width: 480px; height: 480px;">
        <div style="height: 100px;">Top</div>
        <div style="height: 100px;">Bottom</div>
    </div>"#;
    let el = parse_html(html).unwrap();
    let nodes = compute_layout(&el, 480.0, 480.0).unwrap();
    // Find children by text
    let top = nodes.iter().find(|n| n.text.as_deref() == Some("Top")).unwrap();
    let bottom = nodes.iter().find(|n| n.text.as_deref() == Some("Bottom")).unwrap();
    assert!(bottom.y > top.y, "Bottom should be below Top");
}

#[test]
fn template_renderer_produces_480x480_frame() {
    let layout_html = r#"<div style="display: flex; flex-direction: column; padding: 12px; background: #1a1a2e; color: #ffffff; font-size: 24px;">
        <span>CPU {{ cpu_temp }}C</span>
    </div>"#;

    let mut renderer = TemplateRenderer::new(layout_html, 480, 480).unwrap();
    let mut sensors = HashMap::new();
    sensors.insert("cpu_temp".to_string(), "65".to_string());

    let frame = renderer.render(&sensors).unwrap();
    assert_eq!(frame.width, 480);
    assert_eq!(frame.height, 480);
    assert_eq!(frame.data.len(), 480 * 480 * 3);
    // Verify the background color is exactly #1a1a2e (RGB: 0x1a, 0x1a, 0x2e)
    let pixel = &frame.data[0..3]; // first pixel RGB
    assert_eq!(pixel[0], 0x1a, "R channel should be 0x1a");
    assert_eq!(pixel[1], 0x1a, "G channel should be 0x1a");
    assert_eq!(pixel[2], 0x2e, "B channel should be 0x2e");
}

// Regression tests for the ModeChange::Layout reload path bug:
// The bug was that new SvgRenderer was built with ThemePalette::default() and
// set_history() was never called, causing Tera substitution failures.

/// A layout using `{{ cpu_temp_history }}` FAILS to render without history attached,
/// because the variable is never injected into the Tera context.
#[test]
fn svg_renderer_without_history_fails_on_history_template() {
    let template = r##"<svg viewBox="0 0 480 480" xmlns="http://www.w3.org/2000/svg">
        <text x="10" y="20" fill="#fff">{{ cpu_temp_history | join(sep=",") }}</text>
    </svg>"##;

    let mut renderer = SvgRenderer::new(template, 480, 480).unwrap();
    // Deliberately do NOT call set_history() — simulates the buggy reload path

    let result = renderer.render(&HashMap::new());
    assert!(result.is_err(), "Expected Tera error for undefined cpu_temp_history variable, got Ok");
}

/// A layout using `{{ cpu_temp_history }}` renders successfully when history is attached.
/// This is what the fixed reload path must do.
#[test]
fn svg_renderer_with_history_renders_history_template() {
    let template = r##"<svg viewBox="0 0 480 480" xmlns="http://www.w3.org/2000/svg">
        <text x="10" y="20" fill="#fff">{{ cpu_temp_history | join(sep=",") }}</text>
    </svg>"##;

    let mut renderer = SvgRenderer::new(template, 480, 480).unwrap();

    // Attach history — simulates what the fixed reload path must do
    let mut history = SensorHistory::new();
    history.configure_metric("cpu_temp", std::time::Duration::from_secs(30));
    let history = Arc::new(Mutex::new(history));
    renderer.set_history(history);

    let frame = renderer.render(&HashMap::new()).unwrap();
    assert_eq!(frame.width, 480);
    assert_eq!(frame.height, 480);
    assert_eq!(frame.data.len(), 480 * 480 * 3);
}

/// Rendering with a configured (non-default) theme palette injects the configured
/// color into `{{ theme_primary }}`, not the default `#e94560`.
/// The fixed reload path must pass the configured theme, not ThemePalette::default().
#[test]
fn svg_renderer_uses_configured_theme_not_default() {
    // Full-canvas rect filled with {{ theme_primary }} — easy to assert on pixel color
    let template = r#"<svg viewBox="0 0 480 480" xmlns="http://www.w3.org/2000/svg">
        <rect width="480" height="480" fill="{{ theme_primary }}"/>
    </svg>"#;

    let custom_theme = ThemePalette {
        primary: "#7aa2f7".to_string(), // Tokyo Night blue — NOT the default #e94560
        secondary: "#bb9af7".to_string(),
        accent: "#7dcfff".to_string(),
        background: "#1a1b26".to_string(),
        surface: "#16161e".to_string(),
        text: "#c0caf5".to_string(),
        text_dim: "#565f89".to_string(),
        success: "#9ece6a".to_string(),
        warning: "#e0af68".to_string(),
        critical: "#f7768e".to_string(),
    };

    let mut renderer = SvgRenderer::new(template, 480, 480).unwrap();
    renderer.set_theme(custom_theme);

    let frame = renderer.render(&HashMap::new()).unwrap();

    // Top-left pixel should be #7aa2f7 (R=0x7a, G=0xa2, B=0xf7)
    // NOT the default primary #e94560 (R=0xe9, G=0x45, B=0x60)
    assert_eq!(frame.data[0], 0x7a, "R: expected Tokyo Night blue #7aa2f7, not default #e94560");
    assert_eq!(frame.data[1], 0xa2, "G: expected Tokyo Night blue #7aa2f7, not default #e94560");
    assert_eq!(frame.data[2], 0xf7, "B: expected Tokyo Night blue #7aa2f7, not default #e94560");

    // Verify it's NOT the default color
    assert_ne!(frame.data[0], 0xe9, "Should not be the default theme primary R=0xe9");
}
