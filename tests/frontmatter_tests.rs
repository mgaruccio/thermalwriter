use thermalwriter::render::frontmatter::{LayoutFrontmatter, VariableDecl};
use std::time::Duration;

#[test]
fn parse_history_frontmatter() {
    let svg = r#"{# history: cpu_temp=60s, cpu_util=120s, net_rx=300s@0.2hz #}
<svg viewBox="0 0 480 480">...</svg>"#;

    let fm = LayoutFrontmatter::parse(svg);
    assert_eq!(fm.history_configs.len(), 3);

    let cpu_temp = &fm.history_configs["cpu_temp"];
    assert_eq!(cpu_temp.duration, Duration::from_secs(60));
    assert!(cpu_temp.sample_hz.is_none()); // uses default

    let net_rx = &fm.history_configs["net_rx"];
    assert_eq!(net_rx.duration, Duration::from_secs(300));
    assert!((net_rx.sample_hz.unwrap() - 0.2).abs() < 0.01);
}

#[test]
fn parse_animation_frontmatter() {
    let svg = r#"{# animation: fps=15, decode=stream #}
<svg viewBox="0 0 480 480">...</svg>"#;

    let fm = LayoutFrontmatter::parse(svg);
    assert_eq!(fm.animation_fps, Some(15));
    assert_eq!(fm.animation_decode.as_deref(), Some("stream"));
}

#[test]
fn missing_frontmatter_returns_defaults() {
    let svg = r#"<svg viewBox="0 0 480 480">...</svg>"#;
    let fm = LayoutFrontmatter::parse(svg);
    assert!(fm.history_configs.is_empty());
    assert!(fm.animation_fps.is_none());
}

#[test]
fn parse_vars_frontmatter() {
    // Use r##"..."## because the content contains "#color" style strings with "#
    let svg = r##"{# vars:
accent_color: color = "#00ff88" "Primary accent color"
label_text: text = "CPU" "Label shown above gauge"
temp_sensor: sensor = "cpu_temp" "Temperature sensor to display"
#}
<svg viewBox="0 0 480 480">...</svg>"##;

    let fm = LayoutFrontmatter::parse(svg);
    assert_eq!(fm.variables.len(), 3);

    let accent = &fm.variables["accent_color"];
    assert_eq!(accent.var_type, "color");
    assert_eq!(accent.default, "#00ff88");
    assert_eq!(accent.help, "Primary accent color");

    let label = &fm.variables["label_text"];
    assert_eq!(label.var_type, "text");
    assert_eq!(label.default, "CPU");
    assert_eq!(label.help, "Label shown above gauge");

    let sensor = &fm.variables["temp_sensor"];
    assert_eq!(sensor.var_type, "sensor");
    assert_eq!(sensor.default, "cpu_temp");
    assert_eq!(sensor.help, "Temperature sensor to display");
}

#[test]
fn parse_vars_coexists_with_history() {
    let svg = r##"{# history: cpu_temp=60s, cpu_util=120s #}
{# vars:
accent_color: color = "#7aa2f7" "Theme accent color"
#}
<svg viewBox="0 0 480 480">...</svg>"##;

    let fm = LayoutFrontmatter::parse(svg);
    assert_eq!(fm.history_configs.len(), 2);
    assert_eq!(fm.variables.len(), 1);
    assert_eq!(fm.variables["accent_color"].default, "#7aa2f7");
}

#[test]
fn existing_single_line_directives_still_work() {
    // regression: ensure old single-line {# ... #} still parse after refactor
    let svg = r#"{# history: cpu_temp=60s #}
{# animation: fps=15, decode=stream #}
<svg viewBox="0 0 480 480">...</svg>"#;

    let fm = LayoutFrontmatter::parse(svg);
    assert_eq!(fm.history_configs.len(), 1);
    assert_eq!(fm.animation_fps, Some(15));
    assert_eq!(fm.animation_decode.as_deref(), Some("stream"));
    assert!(fm.variables.is_empty());
}

// Ensure VariableDecl is accessible from tests (compile check)
#[allow(dead_code)]
fn _type_check(_: VariableDecl) {}
