use thermalwriter::config::Config;
use thermalwriter::render::parser::parse_html;
use std::collections::HashMap;
use std::io::Write;
use tempfile::{NamedTempFile, tempdir};

// ---------------------------------------------------------------------------
// BackgroundConfig — [background] section parsing and backwards compat
// ---------------------------------------------------------------------------

#[test]
fn config_parses_background_image_field() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, r#"
[display]
tick_rate = 2

[background]
image = "skyline.png"
"#).unwrap();

    let cfg = Config::load(f.path()).unwrap();
    assert_eq!(
        cfg.background.image,
        Some("skyline.png".to_string()),
        "background.image should be Some(\"skyline.png\")"
    );
}

#[test]
fn config_without_background_section_defaults_to_none() {
    // Existing config files have no [background] section — they must load cleanly.
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, r#"
[display]
tick_rate = 2

[sensors]
poll_interval_ms = 1000
"#).unwrap();

    let cfg = Config::load(f.path()).unwrap();
    assert_eq!(
        cfg.background.image,
        None,
        "background.image should be None when [background] section is absent"
    );
}

#[test]
fn config_without_theme_section_still_loads() {
    // After deleting theme.background_image, configs without [theme] must still parse.
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, r#"
[display]
tick_rate = 2
"#).unwrap();

    // Must not error — the theme section is entirely optional
    let cfg = Config::load(f.path()).unwrap();
    assert_eq!(cfg.display.tick_rate, 2);
    assert!(cfg.theme.manual.is_none());
    assert_eq!(cfg.background.image, None);
}

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

// ---------------------------------------------------------------------------
// Config::save_layout_vars — toml_edit-backed persistence.
// Preserve comments, update only the target section, write atomically.
// ---------------------------------------------------------------------------

#[test]
fn save_layout_vars_creates_section_in_new_file() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    // Start from a minimal config so we know the write went through.
    std::fs::write(&path, "[display]\ntick_rate = 2\n").unwrap();

    let mut vars = HashMap::new();
    vars.insert("theme_primary".to_string(), "#00ff88".to_string());
    vars.insert("cpu_label".to_string(), "CPU".to_string());
    Config::save_layout_vars(&path, "neon-dash.svg", &vars).unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    let reloaded: toml::Value = toml::from_str(&contents).unwrap();
    let layout_vars = reloaded
        .get("layout_vars")
        .and_then(|v| v.get("neon-dash.svg"))
        .expect("layout_vars.neon-dash.svg should exist after save");
    assert_eq!(layout_vars["theme_primary"].as_str().unwrap(), "#00ff88");
    assert_eq!(layout_vars["cpu_label"].as_str().unwrap(), "CPU");
}

#[test]
fn save_display_layout_updates_layout_and_mode() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(
        &path,
        r#"# keep this comment
[display]
tick_rate = 2
default_layout = "old.html"
mode = "html"
"#,
    ).unwrap();

    Config::save_display_layout(&path, "svg/neon-dash-v2.svg", "svg").unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(contents.contains("# keep this comment"));
    let reloaded: toml::Value = toml::from_str(&contents).unwrap();
    assert_eq!(reloaded["display"]["default_layout"].as_str().unwrap(), "svg/neon-dash-v2.svg");
    assert_eq!(reloaded["display"]["mode"].as_str().unwrap(), "svg");
    assert_eq!(reloaded["display"]["tick_rate"].as_integer().unwrap(), 2);
}

#[test]
fn save_layout_vars_preserves_user_comments() {
    // Killer test: verbatim user comments must survive a save round-trip.
    // This only works with toml_edit::DocumentMut — toml::to_string drops them.
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    let original = "\
# ~/.config/thermalwriter/config.toml — hand-edited!
# Please do not throw away my comments.

[display]
# explanation for tick_rate
tick_rate = 2
default_layout = \"svg/neon-dash-v2.svg\"

[sensors]
poll_interval_ms = 1000
";
    std::fs::write(&path, original).unwrap();

    let mut vars = HashMap::new();
    vars.insert("theme_primary".to_string(), "#00ff88".to_string());
    Config::save_layout_vars(&path, "neon-dash-v2.svg", &vars).unwrap();

    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.contains("# ~/.config/thermalwriter/config.toml — hand-edited!"),
        "top-of-file comment must survive save; got:\n{}",
        after
    );
    assert!(
        after.contains("# Please do not throw away my comments."),
        "second header comment must survive save; got:\n{}",
        after
    );
    assert!(
        after.contains("# explanation for tick_rate"),
        "inline section comment must survive save; got:\n{}",
        after
    );
    // And the new data is present
    assert!(
        after.contains("theme_primary"),
        "new var must be written; got:\n{}",
        after
    );
    assert!(
        after.contains("#00ff88"),
        "new var value must be written; got:\n{}",
        after
    );
}

#[test]
fn save_layout_vars_updates_existing_section() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    let original = "\
[display]
tick_rate = 2

[layout_vars.\"neon-dash-v2.svg\"]
theme_primary = \"#111111\"
cpu_label = \"OLD\"
";
    std::fs::write(&path, original).unwrap();

    let mut vars = HashMap::new();
    vars.insert("theme_primary".to_string(), "#00ff88".to_string());
    vars.insert("cpu_label".to_string(), "CPU".to_string());
    Config::save_layout_vars(&path, "neon-dash-v2.svg", &vars).unwrap();

    let after = std::fs::read_to_string(&path).unwrap();
    let reloaded: toml::Value = toml::from_str(&after).unwrap();
    let lv = &reloaded["layout_vars"]["neon-dash-v2.svg"];
    assert_eq!(lv["theme_primary"].as_str().unwrap(), "#00ff88");
    assert_eq!(lv["cpu_label"].as_str().unwrap(), "CPU");
}

#[test]
fn save_layout_vars_does_not_touch_other_sections() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    let original = "\
[display]
tick_rate = 2
default_layout = \"svg/x.svg\"

[sensors]
poll_interval_ms = 1234

[layout_vars.\"other.svg\"]
theme_primary = \"#aa0000\"
";
    std::fs::write(&path, original).unwrap();

    let mut vars = HashMap::new();
    vars.insert("theme_primary".to_string(), "#00ff88".to_string());
    Config::save_layout_vars(&path, "neon-dash.svg", &vars).unwrap();

    let after = std::fs::read_to_string(&path).unwrap();
    let reloaded: toml::Value = toml::from_str(&after).unwrap();
    assert_eq!(reloaded["display"]["tick_rate"].as_integer().unwrap(), 2);
    assert_eq!(
        reloaded["display"]["default_layout"].as_str().unwrap(),
        "svg/x.svg"
    );
    assert_eq!(
        reloaded["sensors"]["poll_interval_ms"].as_integer().unwrap(),
        1234
    );
    // Existing layout_vars.other.svg untouched
    assert_eq!(
        reloaded["layout_vars"]["other.svg"]["theme_primary"]
            .as_str()
            .unwrap(),
        "#aa0000"
    );
    // New layout_vars.neon-dash.svg added
    assert_eq!(
        reloaded["layout_vars"]["neon-dash.svg"]["theme_primary"]
            .as_str()
            .unwrap(),
        "#00ff88"
    );
}

#[test]
fn save_layout_vars_writes_atomically_via_same_dir_tempfile() {
    // We assert the outcome of an atomic rename: the file exists, is valid,
    // and (as a proxy for "rename not copy") no stray .tmp files remain.
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, "[display]\ntick_rate = 2\n").unwrap();

    let mut vars = HashMap::new();
    vars.insert("theme_primary".to_string(), "#00ff88".to_string());
    Config::save_layout_vars(&path, "layout.svg", &vars).unwrap();

    // Final file is valid TOML
    let contents = std::fs::read_to_string(&path).unwrap();
    let _ = toml::from_str::<toml::Value>(&contents).expect("result must parse as TOML");

    // No leftover temp files in the same directory.
    let entries: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    let stragglers: Vec<_> = entries
        .iter()
        .filter(|n| n.contains("config.toml.") && *n != "config.toml")
        .collect();
    assert!(
        stragglers.is_empty(),
        "expected atomic rename to leave no stragglers; found {:?}",
        stragglers
    );
}

#[test]
fn config_load_parses_layout_vars_field() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(
        f,
        r##"
[display]
tick_rate = 2

[layout_vars."neon-dash.svg"]
theme_primary = "#00ff88"
cpu_label = "CPU"
"##
    )
    .unwrap();

    let cfg = Config::load(f.path()).unwrap();
    let lv = cfg
        .layout_vars
        .get("neon-dash.svg")
        .expect("layout_vars.neon-dash.svg should be parsed");
    assert_eq!(lv.get("theme_primary").unwrap(), "#00ff88");
    assert_eq!(lv.get("cpu_label").unwrap(), "CPU");
}
