use thermalrighter::render::parser::*;
use thermalrighter::render::layout::*;

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
