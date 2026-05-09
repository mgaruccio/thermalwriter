use thermalwriter::render::parser::*;
use thermalwriter::render::layout::*;
use thermalwriter::render::{FrameSource, TemplateRenderer};
use thermalwriter::render::svg::SvgRenderer;
use thermalwriter::sensor::history::SensorHistory;
use thermalwriter::theme::ThemePalette;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

fn make_solid_color_png(width: u32, height: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
    use image::{ImageBuffer, Rgb, ImageFormat};
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(width, height, Rgb([r, g, b]));
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Png).unwrap();
    buf.into_inner()
}

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
// The bug was that the new SvgRenderer was built with ThemePalette::default() and
// set_history() was never called. Tera is not strict-mode, so undefined variables
// render silently as empty. The real symptom: wrong theme colors on the LCD and
// blank sparkline charts after clicking Apply in the GUI.

/// Without history attached, passing `cpu_temp_history` to the graph() Tera function
/// causes a hard error: "Variable `cpu_temp_history` not found in context". This is
/// the actual "Tera template substitution failed" symptom the plan describes.
/// Plain `{{ cpu_temp_history }}` in text is lenient (empty string), but Tera Function
/// calls error on undefined arguments. All production layouts use graph(), so the
/// plan's symptom description is correct for real layout files.
#[test]
fn svg_renderer_without_history_errors_on_graph_component() {
    // graph() is a Tera Function — it errors when cpu_temp_history is undefined.
    let template = r##"<svg viewBox="0 0 480 480" xmlns="http://www.w3.org/2000/svg">
        <rect width="480" height="480" fill="#000000"/>
        {{ graph(data=cpu_temp_history, x=0, y=0, w=480, h=240, stroke="#ff0000", style="line") }}
    </svg>"##;

    let mut renderer = SvgRenderer::new(template, 480, 480).unwrap();
    // Deliberately do NOT call set_history() — simulates the buggy reload path

    let result = renderer.render(&HashMap::new());
    assert!(result.is_err(), "Expected Tera error when graph() receives undefined cpu_temp_history");
    let err_str = format!("{:#}", result.unwrap_err());
    assert!(
        err_str.contains("cpu_temp_history") || err_str.contains("not found"),
        "Expected error mentioning cpu_temp_history, got: {err_str}"
    );
}

/// With history attached, the graph() component receives data and renders a visible
/// stroke in the chart area. At least one pixel in the chart region should be non-black.
/// This is what the fixed reload path (with set_history) must produce.
#[test]
fn svg_renderer_with_history_renders_visible_chart() {
    let template = r##"<svg viewBox="0 0 480 480" xmlns="http://www.w3.org/2000/svg">
        <rect width="480" height="480" fill="#000000"/>
        {{ graph(data=cpu_temp_history, x=0, y=0, w=480, h=240, stroke="#ff0000", style="line") }}
    </svg>"##;

    let mut renderer = SvgRenderer::new(template, 480, 480).unwrap();

    // Attach history with actual data — simulates what the fixed reload path must do
    let mut history = SensorHistory::new();
    history.configure_metric("cpu_temp", std::time::Duration::from_secs(30));
    // Record several samples so the graph has data to draw
    let mut data = HashMap::new();
    for val in ["65", "68", "70", "72", "69", "67"] {
        data.insert("cpu_temp".to_string(), val.to_string());
        history.record(&data);
    }
    renderer.set_history(Arc::new(Mutex::new(history)));

    let frame = renderer.render(&HashMap::new()).unwrap();
    assert_eq!(frame.data.len(), 480 * 480 * 3);

    // With history, the red stroke line is rendered — at least one pixel in the
    // top-half chart area should have R > 0 (red stroke).
    let has_red_pixel = (0..240usize).flat_map(|row| (0..480usize).map(move |col| (row, col)))
        .any(|(row, col)| {
            let idx = (row * 480 + col) * 3;
            frame.data[idx] > 0 // R channel: red stroke
        });
    assert!(has_red_pixel, "Expected a visible red chart stroke when history data is present");
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

// ---------------------------------------------------------------------------
// Background decode module tests (plan Task 5)
// ---------------------------------------------------------------------------

#[test]
fn decode_png_to_pixmap_roundtrips_dimensions_and_pixel_values() {
    let bytes = make_solid_color_png(480, 480, 255, 0, 0); // solid red
    let pixmap = thermalwriter::render::background::decode_to_pixmap(&bytes)
        .expect("decode_to_pixmap must succeed on valid PNG");

    assert_eq!(pixmap.width(), 480, "decoded pixmap must be 480 wide");
    assert_eq!(pixmap.height(), 480, "decoded pixmap must be 480 tall");

    // Center pixel (240, 240): premultiplied RGBA. For a fully opaque red pixel,
    // premultiplied == straight: R=255, G=0, B=0, A=255.
    let idx = (240 * 480 + 240) * 4;
    let data = pixmap.data();
    assert_eq!(data[idx],     255, "center pixel R should be 255 (red)");
    assert_eq!(data[idx + 1],   0, "center pixel G should be 0");
    assert_eq!(data[idx + 2],   0, "center pixel B should be 0");
    assert_eq!(data[idx + 3], 255, "center pixel A should be 255 (fully opaque)");
}

#[test]
fn decode_resizes_non_480_input_to_480() {
    let bytes = make_solid_color_png(800, 600, 0, 255, 0); // solid green, non-LCD size
    let pixmap = thermalwriter::render::background::decode_to_pixmap(&bytes)
        .expect("decode_to_pixmap must resize and succeed");

    assert_eq!(pixmap.width(), 480, "output must be resized to 480 wide");
    assert_eq!(pixmap.height(), 480, "output must be resized to 480 tall");
}
