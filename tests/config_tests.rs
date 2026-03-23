use thermalwriter::config::Config;
use thermalwriter::render::parser::parse_html;
use std::io::Write;
use tempfile::NamedTempFile;

#[test]
fn config_loads_from_valid_toml() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, r#"
[display]
tick_rate = 5
default_layout = "gpu-focus.html"
jpeg_quality = 90

[sensors]
poll_interval_ms = 500
mangohud_log_dir = "/tmp/mango"
"#).unwrap();

    let cfg = Config::load(f.path()).unwrap();
    assert_eq!(cfg.display.tick_rate, 5);
    assert_eq!(cfg.display.default_layout, "gpu-focus.html");
    assert_eq!(cfg.display.jpeg_quality, 90);
    assert_eq!(cfg.sensors.poll_interval_ms, 500);
    assert_eq!(cfg.sensors.mangohud_log_dir, "/tmp/mango");
}

#[test]
fn config_uses_defaults_when_file_missing() {
    let cfg = Config::load(std::path::Path::new("/nonexistent/path/config.toml")).unwrap();
    assert_eq!(cfg.display.tick_rate, 2);
    assert_eq!(cfg.display.default_layout, "svg/neon-dash-v2.svg");
    assert_eq!(cfg.display.jpeg_quality, 85);
    assert_eq!(cfg.sensors.poll_interval_ms, 1000);
}

#[test]
fn config_uses_defaults_for_missing_fields() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, r#"
[display]
tick_rate = 10
"#).unwrap();

    let cfg = Config::load(f.path()).unwrap();
    assert_eq!(cfg.display.tick_rate, 10);
    // Unspecified fields should be defaults
    assert_eq!(cfg.display.default_layout, "svg/neon-dash-v2.svg");
    assert_eq!(cfg.display.jpeg_quality, 85);
    assert_eq!(cfg.sensors.poll_interval_ms, 1000);
}

#[test]
fn config_returns_error_on_invalid_toml() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, "this is not [ valid toml = !!!").unwrap();
    let result = Config::load(f.path());
    assert!(result.is_err(), "Invalid TOML should return an error");
}

#[test]
fn builtin_system_stats_layout_parses() {
    let html = include_str!("../layouts/system-stats.html");
    parse_html(html).expect("system-stats.html should parse without error");
}

#[test]
fn builtin_gpu_focus_layout_parses() {
    let html = include_str!("../layouts/gpu-focus.html");
    parse_html(html).expect("gpu-focus.html should parse without error");
}

#[test]
fn builtin_minimal_layout_parses() {
    let html = include_str!("../layouts/minimal.html");
    parse_html(html).expect("minimal.html should parse without error");
}
