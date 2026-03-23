use tera::{Context, Tera};

#[test]
fn graph_component_emits_svg_polyline() {
    let mut tera = Tera::default();
    thermalwriter::render::components::register_all(&mut tera);

    let mut context = Context::new();
    context.insert("cpu_util_history", &vec![10.0f64, 30.0, 50.0, 70.0, 90.0]);

    let template = r##"{{ graph(data=cpu_util_history, x=0, y=0, w=200, h=100, style="line", stroke="#ff0000") }}"##;
    tera.add_raw_template("test.svg", template).unwrap();
    let result = tera.render("test.svg", &context).unwrap();

    assert!(result.contains("<polyline"), "Should contain a polyline element");
    assert!(result.contains("stroke=\"#ff0000\""), "Should use specified stroke color");
    assert!(result.contains("<g"), "Should be wrapped in a <g> group");
}

#[test]
fn graph_component_area_style_emits_polygon() {
    let mut tera = Tera::default();
    thermalwriter::render::components::register_all(&mut tera);

    let mut context = Context::new();
    context.insert("cpu_util_history", &vec![10.0f64, 50.0, 90.0]);

    let template = r##"{{ graph(data=cpu_util_history, x=10, y=10, w=100, h=50, style="area", fill="#ff000033") }}"##;
    tera.add_raw_template("test.svg", template).unwrap();
    let result = tera.render("test.svg", &context).unwrap();

    assert!(result.contains("<polygon"), "Area style should use polygon");
    assert!(result.contains("fill=\"#ff000033\""), "Should use specified fill");
}

#[test]
fn graph_component_empty_data_returns_empty_group() {
    let mut tera = Tera::default();
    thermalwriter::render::components::register_all(&mut tera);

    let mut context = Context::new();
    context.insert("empty_history", &Vec::<f64>::new());

    let template = r#"{{ graph(data=empty_history, x=0, y=0, w=200, h=100) }}"#;
    tera.add_raw_template("test.svg", template).unwrap();
    let result = tera.render("test.svg", &context).unwrap();

    // Should return valid SVG (empty group), not error
    assert!(result.contains("<g"));
}

#[test]
fn graph_component_constant_values_no_division_by_zero() {
    let mut tera = Tera::default();
    thermalwriter::render::components::register_all(&mut tera);

    let mut context = Context::new();
    // All same values — would cause division by zero without range fallback
    context.insert("const_history", &vec![50.0f64, 50.0, 50.0]);

    let template = r#"{{ graph(data=const_history, x=0, y=0, w=200, h=100) }}"#;
    tera.add_raw_template("test.svg", template).unwrap();
    // Should not panic
    let result = tera.render("test.svg", &context).unwrap();
    assert!(result.contains("<g"));
}

#[test]
fn svg_renderer_uses_persistent_tera_with_components() {
    use thermalwriter::render::svg::SvgRenderer;
    use thermalwriter::render::FrameSource;
    use std::collections::HashMap;

    // Template with a graph call — uses a literal array so no undefined variable
    let template = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 480 480">
{{ graph(data=[10.0, 50.0, 90.0], x=0, y=400, w=480, h=80) }}
</svg>"#;

    let mut renderer = SvgRenderer::new(template, 480, 480).unwrap();
    let sensors: HashMap<String, String> = HashMap::new();
    let pixmap = renderer.render(&sensors);
    assert!(pixmap.is_ok(), "Renderer with component function should render: {:?}", pixmap.err());
}

// ─── btop-style visualization component tests ─────────────────────────────────

#[test]
fn btop_bars_emits_rect_grid() {
    let mut tera = Tera::default();
    thermalwriter::render::components::register_all(&mut tera);

    let mut context = Context::new();
    context.insert("cpu_c0_util_history", &vec![20.0f64, 60.0, 90.0]);
    context.insert("cpu_c1_util_history", &vec![10.0f64, 40.0, 70.0]);

    // Pass history arrays explicitly since Tera functions can't access context
    let template = r##"{{ btop_bars(histories=[cpu_c0_util_history, cpu_c1_util_history], x=0, y=0, w=120, h=40, color="#e94560") }}"##;
    tera.add_raw_template("test.svg", template).unwrap();
    let result = tera.render("test.svg", &context).unwrap();

    assert!(result.contains("<rect"), "Should contain rect elements");
    assert!(result.contains("<g"), "Should be wrapped in a group");
}

#[test]
fn btop_net_emits_mirrored_polygons() {
    let mut tera = Tera::default();
    thermalwriter::render::components::register_all(&mut tera);

    let mut context = Context::new();
    context.insert("net_rx_history", &vec![1000.0f64, 5000.0, 3000.0]);
    context.insert("net_tx_history", &vec![500.0f64, 2000.0, 1500.0]);

    let template = r##"{{ btop_net(rx_data=net_rx_history, tx_data=net_tx_history, x=0, y=0, w=200, h=100, rx_color="#53d8fb", tx_color="#e94560") }}"##;
    tera.add_raw_template("test.svg", template).unwrap();
    let result = tera.render("test.svg", &context).unwrap();

    let polygon_count = result.matches("<polygon").count();
    assert!(polygon_count >= 2, "Should have at least 2 polygons (rx + tx), got {}", polygon_count);
}

#[test]
fn btop_ram_emits_area_with_capacity_line() {
    let mut tera = Tera::default();
    thermalwriter::render::components::register_all(&mut tera);

    let mut context = Context::new();
    context.insert("ram_used_history", &vec![24.0f64, 25.0, 26.0, 24.5]);

    let template = r##"{{ btop_ram(data=ram_used_history, total=64.0, x=0, y=0, w=200, h=60, fill="#cc9eff") }}"##;
    tera.add_raw_template("test.svg", template).unwrap();
    let result = tera.render("test.svg", &context).unwrap();

    assert!(result.contains("<polygon"), "Should contain area polygon");
    assert!(result.contains("fill=\"#cc9eff\""), "Should use specified fill color");
}

// ─── Background component tests ──────────────────────────────────────────────

#[test]
fn background_pattern_grid_emits_svg_pattern() {
    let mut tera = Tera::default();
    thermalwriter::render::components::register_all(&mut tera);

    let context = Context::new();
    let template = r##"{{ background(pattern="grid", color="#ffffff10", spacing=20) }}"##;
    tera.add_raw_template("test.svg", template).unwrap();
    let result = tera.render("test.svg", &context).unwrap();

    assert!(result.contains("<defs>"), "Should contain defs for pattern");
    assert!(result.contains("<pattern"), "Should contain pattern element");
    assert!(result.contains("<rect"), "Should contain rect using the pattern");
}

#[test]
fn background_image_emits_base64_image_tag() {
    let mut tera = Tera::default();
    thermalwriter::render::components::register_all(&mut tera);

    let mut context = Context::new();
    context.insert("__bg_image", &"iVBORw0KGgo="); // tiny base64 stub

    let template = r#"{{ background(image_data=__bg_image, w=480, h=480) }}"#;
    tera.add_raw_template("test.svg", template).unwrap();
    let result = tera.render("test.svg", &context).unwrap();

    assert!(result.contains("<image"), "Should contain image element");
    assert!(result.contains("data:image/png;base64,"), "Should use data URI");
}

#[test]
fn background_with_opacity_sets_attribute() {
    let mut tera = Tera::default();
    thermalwriter::render::components::register_all(&mut tera);

    let mut context = Context::new();
    context.insert("__bg_image", &"iVBORw0KGgo=");

    let template = r#"{{ background(image_data=__bg_image, w=480, h=480, opacity=0.3) }}"#;
    tera.add_raw_template("test.svg", template).unwrap();
    let result = tera.render("test.svg", &context).unwrap();

    assert!(result.contains("opacity=\"0.3\""), "Should set opacity attribute");
}
