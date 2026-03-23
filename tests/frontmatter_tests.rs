use thermalwriter::render::frontmatter::LayoutFrontmatter;
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
