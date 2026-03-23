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
