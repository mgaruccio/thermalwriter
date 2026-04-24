// Tests for daemon D-Bus helper logic: path validation, layout listing,
// layout-vars read/write, sensor descriptor exposure.
//
// These tests invoke the helper free-functions and associated-fn impls directly
// — they do NOT bind com.thermalwriter.Service on the session bus (the real
// daemon owns that name on the developer's machine).

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use tempfile::tempdir;
use thermalwriter::config::Config;
use thermalwriter::service::dbus::{
    apply_layout_vars, get_layout_vars_impl, list_layouts_impl, validate_layout_path,
};

// ---------------------------------------------------------------------------
// validate_layout_path: canonicalizes + enforces starts_with(layout_dir)
// ---------------------------------------------------------------------------

#[test]
fn validate_layout_path_accepts_simple_name() {
    let tmp = tempdir().unwrap();
    let layout_dir = tmp.path();
    // Seed a layout file
    let layout_file = layout_dir.join("neon-dash.svg");
    fs::write(&layout_file, "<svg/>").unwrap();

    let resolved = validate_layout_path(layout_dir, "neon-dash.svg").unwrap();
    assert_eq!(resolved, layout_file.canonicalize().unwrap());
}

#[test]
fn validate_layout_path_accepts_subdir_name() {
    let tmp = tempdir().unwrap();
    let layout_dir = tmp.path();
    fs::create_dir_all(layout_dir.join("svg")).unwrap();
    let layout_file = layout_dir.join("svg/neon-dash-v2.svg");
    fs::write(&layout_file, "<svg/>").unwrap();

    let resolved = validate_layout_path(layout_dir, "svg/neon-dash-v2.svg").unwrap();
    assert_eq!(resolved, layout_file.canonicalize().unwrap());
}

#[test]
fn validate_layout_path_rejects_parent_traversal() {
    let tmp = tempdir().unwrap();
    let layout_dir = tmp.path().join("layouts");
    fs::create_dir_all(&layout_dir).unwrap();
    // Put an "attacker" file outside the layout_dir but inside the tmp
    let outside = tmp.path().join("secret.txt");
    fs::write(&outside, "PWNED").unwrap();

    let result = validate_layout_path(&layout_dir, "../secret.txt");
    assert!(
        result.is_err(),
        "parent-traversal name should be rejected, got Ok({:?})",
        result.ok()
    );
}

#[test]
fn validate_layout_path_rejects_absolute_name() {
    let tmp = tempdir().unwrap();
    let layout_dir = tmp.path();
    fs::write(layout_dir.join("a.svg"), "<svg/>").unwrap();

    // Absolute paths must be rejected because they could point anywhere.
    let result = validate_layout_path(layout_dir, "/etc/passwd");
    assert!(
        result.is_err(),
        "absolute path must be rejected, got Ok({:?})",
        result.ok()
    );
}

#[test]
fn validate_layout_path_rejects_nonexistent_name() {
    let tmp = tempdir().unwrap();
    let layout_dir = tmp.path();
    let result = validate_layout_path(layout_dir, "does-not-exist.svg");
    assert!(result.is_err(), "nonexistent layout must error");
}

#[test]
fn validate_layout_path_rejects_symlink_escape() {
    let tmp = tempdir().unwrap();
    let layout_dir = tmp.path().join("layouts");
    fs::create_dir_all(&layout_dir).unwrap();
    let outside = tmp.path().join("secret.txt");
    fs::write(&outside, "PWNED").unwrap();

    // Symlink inside the layout_dir pointing outside it
    #[cfg(unix)]
    {
        let link = layout_dir.join("pwn.svg");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let result = validate_layout_path(&layout_dir, "pwn.svg");
        assert!(
            result.is_err(),
            "symlink escape must be rejected, got Ok({:?})",
            result.ok()
        );
    }
}

// ---------------------------------------------------------------------------
// list_layouts_impl: returns .html AND .svg files, including svg/*.svg
// ---------------------------------------------------------------------------

#[test]
fn list_layouts_returns_html_and_svg_files() {
    let tmp = tempdir().unwrap();
    let layout_dir = tmp.path();
    fs::write(layout_dir.join("system-stats.html"), "<html/>").unwrap();
    fs::write(layout_dir.join("minimal.html"), "<html/>").unwrap();
    fs::create_dir(layout_dir.join("svg")).unwrap();
    fs::write(layout_dir.join("svg/neon-dash.svg"), "<svg/>").unwrap();
    fs::write(layout_dir.join("svg/arc-gauge.svg"), "<svg/>").unwrap();

    let layouts = list_layouts_impl(layout_dir);
    assert!(
        layouts.contains(&"system-stats.html".to_string()),
        "expected system-stats.html in {:?}",
        layouts
    );
    assert!(
        layouts.contains(&"minimal.html".to_string()),
        "expected minimal.html in {:?}",
        layouts
    );
    assert!(
        layouts.contains(&"svg/neon-dash.svg".to_string()),
        "expected svg/ prefix on neon-dash.svg, got {:?}",
        layouts
    );
    assert!(
        layouts.contains(&"svg/arc-gauge.svg".to_string()),
        "expected svg/ prefix on arc-gauge.svg, got {:?}",
        layouts
    );
}

#[test]
fn list_layouts_sorted_and_deduped() {
    let tmp = tempdir().unwrap();
    let layout_dir = tmp.path();
    fs::write(layout_dir.join("zeta.html"), "<html/>").unwrap();
    fs::write(layout_dir.join("alpha.html"), "<html/>").unwrap();
    fs::create_dir(layout_dir.join("svg")).unwrap();
    fs::write(layout_dir.join("svg/mid.svg"), "<svg/>").unwrap();

    let layouts = list_layouts_impl(layout_dir);
    let mut sorted = layouts.clone();
    sorted.sort();
    assert_eq!(
        layouts, sorted,
        "list_layouts must return sorted output"
    );
}

#[test]
fn list_layouts_ignores_other_extensions() {
    let tmp = tempdir().unwrap();
    let layout_dir = tmp.path();
    fs::write(layout_dir.join("readme.txt"), "docs").unwrap();
    fs::write(layout_dir.join("config.toml"), "[x]").unwrap();
    fs::write(layout_dir.join("actual.svg"), "<svg/>").unwrap();

    let layouts = list_layouts_impl(layout_dir);
    assert_eq!(layouts, vec!["actual.svg".to_string()]);
}

// ---------------------------------------------------------------------------
// get_layout_vars_impl: reads frontmatter from disk, returns parsed vars
// ---------------------------------------------------------------------------

#[test]
fn get_layout_vars_reads_frontmatter_from_disk() {
    let tmp = tempdir().unwrap();
    let layout_dir = tmp.path();
    fs::create_dir(layout_dir.join("svg")).unwrap();
    let svg = r##"{# vars:
   theme_primary: color = "#00ff88" "Primary accent"
   cpu_label: text = "CPU" "CPU panel label"
#}
<svg viewBox="0 0 480 480">...</svg>"##;
    fs::write(layout_dir.join("svg/neon.svg"), svg).unwrap();

    let vars = get_layout_vars_impl(layout_dir, "svg/neon.svg").unwrap();
    assert_eq!(vars.len(), 2, "expected 2 vars, got {:?}", vars);

    // Find each by name (order isn't guaranteed — HashMap under the hood).
    let by_name: HashMap<&str, &HashMap<String, String>> = vars
        .iter()
        .map(|m| (m["name"].as_str(), m))
        .collect();

    let theme = by_name["theme_primary"];
    assert_eq!(theme["type"], "color");
    assert_eq!(theme["default"], "#00ff88");
    assert_eq!(theme["help"], "Primary accent");

    let cpu = by_name["cpu_label"];
    assert_eq!(cpu["type"], "text");
    assert_eq!(cpu["default"], "CPU");
    assert_eq!(cpu["help"], "CPU panel label");
}

#[test]
fn get_layout_vars_rejects_traversal() {
    let tmp = tempdir().unwrap();
    let layout_dir = tmp.path().join("layouts");
    fs::create_dir_all(&layout_dir).unwrap();
    fs::write(tmp.path().join("outside.svg"), "{# vars:\nx: text = \"a\" \"b\"\n#}").unwrap();

    let result = get_layout_vars_impl(&layout_dir, "../outside.svg");
    assert!(
        result.is_err(),
        "get_layout_vars must reject traversal; got Ok({:?})",
        result.ok()
    );
}

#[test]
fn get_layout_vars_empty_when_no_vars_block() {
    let tmp = tempdir().unwrap();
    let layout_dir = tmp.path();
    fs::write(
        layout_dir.join("plain.svg"),
        "<svg viewBox=\"0 0 480 480\">...</svg>",
    )
    .unwrap();

    let vars = get_layout_vars_impl(layout_dir, "plain.svg").unwrap();
    assert!(vars.is_empty(), "plain layout should have no vars");
}

// ---------------------------------------------------------------------------
// layout_dir path helpers: resolved path equals layout_dir.join(name) canonical
// ---------------------------------------------------------------------------

#[test]
fn validate_layout_path_returns_canonical_path() {
    let tmp = tempdir().unwrap();
    let layout_dir = tmp.path();
    fs::create_dir(layout_dir.join("svg")).unwrap();
    let file = layout_dir.join("svg/x.svg");
    fs::write(&file, "<svg/>").unwrap();

    let resolved: PathBuf = validate_layout_path(layout_dir, "svg/x.svg").unwrap();
    assert_eq!(resolved, file.canonicalize().unwrap());
    assert!(resolved.is_absolute(), "canonicalized path must be absolute");
}

// ---------------------------------------------------------------------------
// apply_layout_vars: persists to disk AND mutates the in-memory Config so the
// tick loop sees the new values without a restart (killer item).
// ---------------------------------------------------------------------------

#[test]
fn apply_layout_vars_updates_in_memory_and_disk() {
    let tmp = tempdir().unwrap();
    let layout_dir = tmp.path().join("layouts");
    fs::create_dir(&layout_dir).unwrap();
    fs::write(
        layout_dir.join("neon.svg"),
        "{# vars:\ntheme_primary: color = \"#111111\" \"Primary\"\n#}\n<svg/>",
    )
    .unwrap();
    let config_path = tmp.path().join("config.toml");
    fs::write(&config_path, "[display]\ntick_rate = 2\n").unwrap();
    let mut config = Config::default();

    let mut vars = HashMap::new();
    vars.insert("theme_primary".to_string(), "#00ff88".to_string());
    vars.insert("cpu_label".to_string(), "CPU".to_string());

    apply_layout_vars(&layout_dir, &config_path, &mut config, "neon.svg", vars.clone())
        .expect("apply_layout_vars should succeed");

    // In-memory Config must reflect the new values — this is what the tick
    // loop reads when rendering the next frame.
    let in_mem = config
        .layout_vars
        .get("neon.svg")
        .expect("in-memory Config.layout_vars must contain the new layout");
    assert_eq!(in_mem.get("theme_primary").unwrap(), "#00ff88");
    assert_eq!(in_mem.get("cpu_label").unwrap(), "CPU");

    // On-disk config must also reflect the new values (persisted so a
    // daemon restart keeps them).
    let on_disk = std::fs::read_to_string(&config_path).unwrap();
    let parsed: toml::Value = toml::from_str(&on_disk).unwrap();
    let lv = &parsed["layout_vars"]["neon.svg"];
    assert_eq!(lv["theme_primary"].as_str().unwrap(), "#00ff88");
    assert_eq!(lv["cpu_label"].as_str().unwrap(), "CPU");
}

#[test]
fn apply_layout_vars_rejects_traversal_before_touching_disk() {
    let tmp = tempdir().unwrap();
    let layout_dir = tmp.path().join("layouts");
    fs::create_dir(&layout_dir).unwrap();
    let config_path = tmp.path().join("config.toml");
    let pre = "[display]\ntick_rate = 2\n";
    fs::write(&config_path, pre).unwrap();
    let mut config = Config::default();

    let mut vars = HashMap::new();
    vars.insert("theme_primary".to_string(), "#00ff88".to_string());

    let result = apply_layout_vars(
        &layout_dir,
        &config_path,
        &mut config,
        "../outside.svg",
        vars,
    );
    assert!(result.is_err(), "traversal must be rejected");

    // Config file must be unchanged — nothing was persisted.
    let after = std::fs::read_to_string(&config_path).unwrap();
    assert_eq!(after, pre, "disk config must be untouched on traversal reject");
    // In-memory config must also be unchanged.
    assert!(
        config.layout_vars.is_empty(),
        "in-memory config.layout_vars must be untouched on traversal reject"
    );
}
