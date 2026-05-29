use std::time::Duration;
use thermalwriter::render::frontmatter::{LayoutFrontmatter, VariableDecl};

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
fn parse_number_var_with_bounds() {
    let svg = r#"{# vars:
panel_opacity: number(0,1,0.05) = "0.5" "Panel background opacity"
plain_num: number = "12" "Unbounded number"
#}
<svg viewBox="0 0 480 480">...</svg>"#;

    let fm = LayoutFrontmatter::parse(svg);
    let op = &fm.variables["panel_opacity"];
    assert_eq!(op.var_type, "number");
    assert_eq!(op.default, "0.5");
    assert_eq!(op.min, Some(0.0));
    assert_eq!(op.max, Some(1.0));
    assert_eq!(op.step, Some(0.05));

    let plain = &fm.variables["plain_num"];
    assert_eq!(plain.var_type, "number");
    assert_eq!(plain.min, None);
    assert_eq!(plain.max, None);
}

#[test]
fn parse_number_var_rejects_non_numeric_default_and_bad_bounds() {
    let svg = r##"{# vars:
not_a_number: number = "abc" "Bad default"
bad_bounds: number(0,oops) = "0.5" "Bad bound"
color_with_bounds: color(0,1) = "#ffffff" "Bounds not allowed on color"
good: number(0,1) = "0.5" "Valid"
#}
<svg/>"##;
    let fm = LayoutFrontmatter::parse(svg);
    assert!(!fm.variables.contains_key("not_a_number"));
    assert!(!fm.variables.contains_key("bad_bounds"));
    assert!(!fm.variables.contains_key("color_with_bounds"));
    assert!(fm.variables.contains_key("good"));
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

#[test]
fn invalid_var_name_uppercase_is_rejected() {
    // Variable names must match [a-z_][a-z0-9_]* — uppercase rejected
    let svg = r##"{# vars:
BadName: color = "#00ff88" "A color"
good_name: color = "#00ff88" "A color"
#}
<svg/>"##;
    let fm = LayoutFrontmatter::parse(svg);
    assert!(
        !fm.variables.contains_key("BadName"),
        "uppercase name must be rejected"
    );
    assert!(
        fm.variables.contains_key("good_name"),
        "valid name must be accepted"
    );
}

#[test]
fn invalid_color_default_is_rejected() {
    // Color values must be #rrggbb or #rrggbbaa hex
    let svg = r##"{# vars:
bad_color: color = "red" "Not a hex color"
also_bad: color = "#gghhii" "Bad hex digits"
good_color: color = "#ff8800" "Valid hex color"
#}
<svg/>"##;
    let fm = LayoutFrontmatter::parse(svg);
    assert!(
        !fm.variables.contains_key("bad_color"),
        "named color must be rejected"
    );
    assert!(
        !fm.variables.contains_key("also_bad"),
        "invalid hex digits must be rejected"
    );
    assert!(
        fm.variables.contains_key("good_color"),
        "valid hex color must be accepted"
    );
}

#[test]
fn text_with_tera_delimiters_is_rejected() {
    // Text defaults must not contain {{ }} {% %} to prevent template injection
    let svg = r#"{# vars:
unsafe_text: text = "{{ cpu_temp }}" "Tera expression"
safe_text: text = "CPU Temperature" "Plain text label"
#}
<svg/>"#;
    let fm = LayoutFrontmatter::parse(svg);
    assert!(
        !fm.variables.contains_key("unsafe_text"),
        "Tera delimiters must be rejected"
    );
    assert!(
        fm.variables.contains_key("safe_text"),
        "plain text must be accepted"
    );
}
