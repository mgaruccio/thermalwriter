use std::collections::HashMap;
use std::io::Write;
use tempfile::{NamedTempFile, tempdir};
use thermalwriter::config::Config;
use thermalwriter::render::{FrameSource, SensorData, TemplateRenderer};

// ---------------------------------------------------------------------------
// BackgroundConfig — [background] section parsing and backwards compat
// ---------------------------------------------------------------------------

#[test]
fn config_parses_background_image_field() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(
        f,
        r#"
[display]
tick_rate = 2

[background]
image = "skyline.png"
"#
    )
    .unwrap();

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
    writeln!(
        f,
        r#"
[display]
tick_rate = 2

[sensors]
poll_interval_ms = 1000
"#
    )
    .unwrap();

    let cfg = Config::load(f.path()).unwrap();
    assert_eq!(
        cfg.background.image, None,
        "background.image should be None when [background] section is absent"
    );
}

#[test]
fn config_without_theme_section_still_loads() {
    // After deleting theme.background_image, configs without [theme] must still parse.
    let mut f = NamedTempFile::new().unwrap();
    writeln!(
        f,
        r#"
[display]
tick_rate = 2
"#
    )
    .unwrap();

    // Must not error — the theme section is entirely optional
    let cfg = Config::load(f.path()).unwrap();
    assert_eq!(cfg.display.tick_rate, 2);
    assert!(cfg.theme.manual.is_none());
    assert_eq!(cfg.background.image, None);
}

#[test]
fn config_loads_from_valid_toml() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(
        f,
        r#"
[display]
tick_rate = 5
default_layout = "gpu-focus.html"
jpeg_quality = 90
mode = "html"

[sensors]
poll_interval_ms = 500
mangohud_log_dir = "/tmp/mango"
"#
    )
    .unwrap();

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
    assert_eq!(cfg.sensors.poll_interval_ms, 2000);
}

#[test]
fn config_uses_defaults_for_missing_fields() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(
        f,
        r#"
[display]
tick_rate = 10
"#
    )
    .unwrap();

    let cfg = Config::load(f.path()).unwrap();
    assert_eq!(cfg.display.tick_rate, 10);
    // Unspecified fields should be defaults
    assert_eq!(cfg.display.default_layout, "svg/neon-dash-v2.svg");
    assert_eq!(cfg.display.jpeg_quality, 85);
    assert_eq!(cfg.sensors.poll_interval_ms, 2000);
}

#[test]
fn config_returns_error_on_invalid_toml() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, "this is not [ valid toml = !!!").unwrap();
    let result = Config::load(f.path());
    assert!(result.is_err(), "Invalid TOML should return an error");
}

fn assert_builtin_html_renders(template: &str, name: &str) {
    let mut renderer =
        TemplateRenderer::new(template, 854, 480).expect("renderer construction should succeed");
    renderer
        .render(&SensorData::new())
        .unwrap_or_else(|error| panic!("{name} should render without error: {error:#}"));
}

#[test]
fn builtin_system_stats_layout_parses() {
    assert_builtin_html_renders(
        include_str!("../layouts/system-stats.html"),
        "system-stats.html",
    );
}

#[test]
fn builtin_gpu_focus_layout_parses() {
    assert_builtin_html_renders(include_str!("../layouts/gpu-focus.html"), "gpu-focus.html");
}

#[test]
fn builtin_minimal_layout_parses() {
    assert_builtin_html_renders(include_str!("../layouts/minimal.html"), "minimal.html");
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
    )
    .unwrap();

    Config::save_display_layout(&path, "svg/neon-dash-v2.svg", "svg").unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(contents.contains("# keep this comment"));
    let reloaded: toml::Value = toml::from_str(&contents).unwrap();
    assert_eq!(
        reloaded["display"]["default_layout"].as_str().unwrap(),
        "svg/neon-dash-v2.svg"
    );
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
        reloaded["sensors"]["poll_interval_ms"]
            .as_integer()
            .unwrap(),
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
fn concurrent_config_writes_do_not_corrupt_file() {
    use std::sync::Arc;
    use std::thread;

    let dir = tempdir().unwrap();
    let path = Arc::new(dir.path().join("config.toml"));
    std::fs::write(&*path, "[display]\ntick_rate = 2\n").unwrap();

    let mut handles = Vec::new();
    for i in 0..16u32 {
        let path = Arc::clone(&path);
        handles.push(thread::spawn(move || {
            let mut vars = HashMap::new();
            vars.insert(format!("var_{}", i), format!("value_{}", i));
            match i % 3 {
                0 => Config::save_layout_vars(&path, &format!("layout_{}.svg", i), &vars).unwrap(),
                1 => {
                    Config::save_display_layout(&path, &format!("layout_{}.svg", i), "svg").unwrap()
                }
                _ => Config::save_background_image(&path, Some(&format!("bg_{}.png", i))).unwrap(),
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    // Final file must parse as valid TOML.
    let contents = std::fs::read_to_string(&*path).unwrap();
    let doc: toml::Value =
        toml::from_str(&contents).expect("config.toml must be valid TOML after concurrent writes");

    // No stray temp files.
    let stragglers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
        .collect();
    assert!(
        stragglers.is_empty(),
        "expected no stray temp files, found {:?}",
        stragglers.iter().map(|e| e.file_name()).collect::<Vec<_>>()
    );

    // All 6 save_layout_vars writers (i%3==0: i=0,3,6,9,12,15) must survive
    // in the final [layout_vars] table. This proves the read-modify-write mutex
    // prevents lost updates — the counter alone (which avoids file collisions)
    // would not guarantee this.
    let layout_vars = doc
        .get("layout_vars")
        .expect("[layout_vars] table must be present after concurrent writes");
    for i in [0u32, 3, 6, 9, 12, 15] {
        let key = format!("layout_{}.svg", i);
        assert!(
            layout_vars.get(&key).is_some(),
            "layout_vars missing key '{}' — lost-update race not fixed",
            key
        );
    }

    // At least one save_display_layout writer (i%3==1) must have survived.
    let default_layout = doc
        .get("display")
        .and_then(|d| d.get("default_layout"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        !default_layout.is_empty(),
        "display.default_layout must be non-empty — save_display_layout writers lost"
    );

    // At least one save_background_image writer (i%3==2) must have survived.
    let bg_image = doc
        .get("background")
        .and_then(|b| b.get("image"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        !bg_image.is_empty(),
        "background.image must be non-empty — save_background_image writers lost"
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

// ---------------------------------------------------------------------------
// Config::save_background_image — toml_edit-backed persistence.
// ---------------------------------------------------------------------------

#[test]
fn save_background_image_writes_image_field() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, "[display]\ntick_rate = 2\n").unwrap();

    Config::save_background_image(&path, Some("skyline.png")).unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    let reloaded: toml::Value = toml::from_str(&contents).unwrap();
    assert_eq!(
        reloaded["background"]["image"].as_str().unwrap(),
        "skyline.png"
    );
}

#[test]
fn save_background_image_none_removes_image_key() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(
        &path,
        "[display]\ntick_rate = 2\n\n[background]\nimage = \"old.png\"\n",
    )
    .unwrap();

    Config::save_background_image(&path, None).unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    let reloaded: toml::Value = toml::from_str(&contents).unwrap();
    assert!(
        reloaded
            .get("background")
            .and_then(|b| b.get("image"))
            .is_none(),
        "image key should be absent after save_background_image(None)"
    );
}

#[test]
fn save_background_image_preserves_user_comments() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    let original = "\
# my hand-edited config
[display]
tick_rate = 2
";
    std::fs::write(&path, original).unwrap();

    Config::save_background_image(&path, Some("bg.png")).unwrap();

    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.contains("# my hand-edited config"),
        "user comment must survive save_background_image; got:\n{}",
        after
    );
    assert!(after.contains("bg.png"), "image value must be written");
}

// ---------------------------------------------------------------------------
// Config validation
// ---------------------------------------------------------------------------

#[test]
fn config_load_rejects_zero_tick_rate() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, "[display]\ntick_rate = 0").unwrap();
    let result = Config::load(f.path());
    assert!(
        result.is_err(),
        "tick_rate=0 must be rejected by validate()"
    );
    // Use {:#} to include the full error chain including the root cause
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("tick_rate"),
        "error message should mention the offending field; got: {}",
        msg
    );
}

#[test]
fn config_load_rejects_invalid_rotation() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, "[display]\nrotation = 45").unwrap();
    let result = Config::load(f.path());
    assert!(
        result.is_err(),
        "rotation=45 must be rejected by validate()"
    );
    // Use {:#} to include the full error chain including the root cause
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("rotation"),
        "error message should mention the offending field; got: {}",
        msg
    );
}

#[test]
fn config_load_rejects_out_of_range_jpeg_quality() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, "[display]\njpeg_quality = 5").unwrap();
    let result = Config::load(f.path());
    assert!(result.is_err(), "jpeg_quality=5 must be rejected (min 10)");
}

#[test]
fn config_load_accepts_valid_values() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(
        f,
        "[display]\ntick_rate = 30\nrotation = 90\njpeg_quality = 85"
    )
    .unwrap();
    let cfg = Config::load(f.path());
    assert!(
        cfg.is_ok(),
        "valid config must load without error: {:?}",
        cfg.err()
    );
}
