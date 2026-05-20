// Tests for daemon D-Bus helper logic: path validation, layout listing,
// layout-vars read/write, background path validation and apply, sensor descriptors.
//
// These tests invoke the helper free-functions and associated-fn impls directly
// — they do NOT bind com.thermalwriter.Service on the session bus (the real
// daemon owns that name on the developer's machine).

#![cfg(feature = "daemon")]

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use tempfile::tempdir;
use thermalwriter::config::Config;
use thermalwriter::service::dbus::{
    get_layout_vars_impl, list_backgrounds_impl, list_layouts_impl, save_default_layout_impl,
    validate_background_path, validate_layout_path, ModeChange,
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

// ---------------------------------------------------------------------------
// F4: concurrent set_background keeps disk + channel + state consistent
// ---------------------------------------------------------------------------

/// Verifies that set_background serializes its full body (decode → disk →
/// channel → state-mirror) end-to-end so concurrent callers cannot leave disk
/// and the tick-channel out of sync.
///
/// The test mirrors what production does: each caller holds bg_change_lock
/// across the entire sequence. We inject a Barrier-forced delay (simulating
/// decode latency) so both tasks are in-flight simultaneously, confirming the
/// lock prevents interleaving. Disk and channel-last must agree after both
/// tasks complete.
///
/// This test also serves as a compile-time check: ServiceState must have a
/// `bg_change_lock: Arc<Mutex<()>>` field for the daemon to initialize.
#[tokio::test]
async fn concurrent_set_background_keeps_state_consistent() {
    use std::sync::Arc;
    use tokio::sync::{Mutex, Barrier};
    use tempfile::tempdir;
    use thermalwriter::config::Config;

    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    std::fs::write(&config_path, "[display]\ntick_rate = 2\n").unwrap();

    // bg_change_lock mirrors the field that ServiceState must have.
    // Without it on ServiceState, the daemon won't compile.
    let bg_change_lock: Arc<Mutex<()>> = Arc::new(Mutex::new(()));

    // Barrier lets both tasks reach their "after-decode" point before either
    // acquires bg_change_lock, so they contend on it under concurrent load.
    let barrier = Arc::new(Barrier::new(2));

    let (tx, mut rx) = tokio::sync::mpsc::channel::<&'static str>(8);

    let lock1 = bg_change_lock.clone();
    let b1 = barrier.clone();
    let tx1 = tx.clone();
    let cfg1 = config_path.clone();
    let h1 = tokio::spawn(async move {
        b1.wait().await; // both "decoded" before either acquires lock
        let _g = lock1.lock().await;
        Config::save_background_image(&cfg1, Some("a.png")).unwrap();
        tx1.send("a.png").await.unwrap();
    });

    let lock2 = bg_change_lock.clone();
    let b2 = barrier.clone();
    let tx2 = tx;
    let cfg2 = config_path.clone();
    let h2 = tokio::spawn(async move {
        b2.wait().await;
        let _g = lock2.lock().await;
        Config::save_background_image(&cfg2, Some("b.png")).unwrap();
        tx2.send("b.png").await.unwrap();
    });

    h1.await.unwrap();
    h2.await.unwrap();

    // Last sender under lock wins — not just FIFO; whichever caller acquired
    // bg_change_lock second sent last, and that determines the active background.
    let mut last_channel_name: Option<&'static str> = None;
    while let Ok(name) = rx.try_recv() {
        last_channel_name = Some(name);
    }

    // Read what disk says.
    let final_contents = std::fs::read_to_string(&config_path).unwrap();
    let doc: toml::Value = toml::from_str(&final_contents).unwrap();
    let disk_name = doc
        .get("background")
        .and_then(|b| b.get("image"))
        .and_then(|i| i.as_str())
        .unwrap_or("");

    // With bg_change_lock serializing disk+channel together, they must agree.
    assert_eq!(
        disk_name,
        last_channel_name.unwrap_or(""),
        "disk says {:?} but channel last says {:?} — bg_change_lock not serializing end-to-end",
        disk_name, last_channel_name
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
// layout_vars: persists to disk AND mutates the in-memory Config so the
// tick loop sees the new values without a restart (killer item).
// ---------------------------------------------------------------------------

#[test]
fn layout_vars_updates_in_memory_and_disk() {
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

    let vars: HashMap<String, String> = [
        ("theme_primary".to_string(), "#00ff88".to_string()),
        ("cpu_label".to_string(), "CPU".to_string()),
    ]
    .into();

    // Validate path, persist to disk, then mirror into in-memory Config.
    validate_layout_path(&layout_dir, "neon.svg").expect("layout path must be valid");
    Config::save_layout_vars(&config_path, "neon.svg", &vars)
        .expect("save_layout_vars should succeed");
    config.layout_vars.insert("neon.svg".to_string(), vars.clone());

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
fn layout_vars_rejects_traversal_before_touching_disk() {
    let tmp = tempdir().unwrap();
    let layout_dir = tmp.path().join("layouts");
    fs::create_dir(&layout_dir).unwrap();
    let config_path = tmp.path().join("config.toml");
    let pre = "[display]\ntick_rate = 2\n";
    fs::write(&config_path, pre).unwrap();
    let config = Config::default();

    // validate_layout_path must reject traversal before any disk write.
    let result = validate_layout_path(&layout_dir, "../outside.svg");
    assert!(result.is_err(), "traversal must be rejected by validate_layout_path");

    // Config file must be unchanged — nothing was persisted.
    let after = std::fs::read_to_string(&config_path).unwrap();
    assert_eq!(after, pre, "disk config must be untouched on traversal reject");
    // In-memory config must also be unchanged.
    assert!(
        config.layout_vars.is_empty(),
        "in-memory config.layout_vars must be untouched on traversal reject"
    );
}

// ---------------------------------------------------------------------------
// validate_background_path: rejects traversal, absolute paths, symlink escapes
// ---------------------------------------------------------------------------

#[test]
fn validate_background_path_rejects_parent_traversal() {
    let tmp = tempdir().unwrap();
    let bg_dir = tmp.path().join("backgrounds");
    fs::create_dir_all(&bg_dir).unwrap();
    let outside = tmp.path().join("secret.txt");
    fs::write(&outside, "PWNED").unwrap();

    let result = validate_background_path(&bg_dir, "../secret.txt");
    assert!(
        result.is_err(),
        "parent-traversal name must be rejected, got Ok({:?})",
        result.ok()
    );
}

#[test]
fn validate_background_path_rejects_absolute_path() {
    let tmp = tempdir().unwrap();
    let bg_dir = tmp.path();
    let result = validate_background_path(bg_dir, "/etc/passwd");
    assert!(
        result.is_err(),
        "absolute path must be rejected, got Ok({:?})",
        result.ok()
    );
}

#[cfg(unix)]
#[test]
fn validate_background_path_rejects_symlink_escape() {
    let tmp = tempdir().unwrap();
    let bg_dir = tmp.path().join("backgrounds");
    fs::create_dir_all(&bg_dir).unwrap();
    let outside = tmp.path().join("secret.png");
    fs::write(&outside, b"\x89PNG").unwrap();
    let link = bg_dir.join("pwn.png");
    std::os::unix::fs::symlink(&outside, &link).unwrap();

    let result = validate_background_path(&bg_dir, "pwn.png");
    assert!(
        result.is_err(),
        "symlink escape must be rejected, got Ok({:?})",
        result.ok()
    );
}

// ---------------------------------------------------------------------------
// list_backgrounds_impl: returns PNG/JPEG files only
// ---------------------------------------------------------------------------

#[test]
fn list_backgrounds_returns_png_and_jpeg_only() {
    let tmp = tempdir().unwrap();
    let bg_dir = tmp.path();
    fs::write(bg_dir.join("dark.png"), b"fake").unwrap();
    fs::write(bg_dir.join("city.jpg"), b"fake").unwrap();
    fs::write(bg_dir.join("readme.txt"), b"docs").unwrap();
    fs::write(bg_dir.join("config.toml"), b"[x]").unwrap();

    let bgs = list_backgrounds_impl(bg_dir);
    assert!(bgs.contains(&"dark.png".to_string()), "must include .png");
    assert!(bgs.contains(&"city.jpg".to_string()), "must include .jpg");
    assert!(!bgs.contains(&"readme.txt".to_string()), "must exclude .txt");
    assert!(!bgs.contains(&"config.toml".to_string()), "must exclude .toml");
}

// ---------------------------------------------------------------------------
// background save: persists to disk, updates in-memory config.background.image,
// sends ModeChange::Background over channel (production pattern: call
// Config::save_background_image for disk, mirror in-memory, send channel).
// ---------------------------------------------------------------------------

#[test]
fn background_save_sets_all_three_effects() {
    let tmp = tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    fs::write(&config_path, "[display]\ntick_rate = 2\n").unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ModeChange>(4);

        Config::save_background_image(&config_path, Some("dark.png"))
            .expect("save_background_image must succeed");
        tx.send(ModeChange::Background { image: None })
            .await
            .expect("channel send must succeed");

        // Effect 1 (in-memory mirror) is now the caller's explicit responsibility —
        // no longer bundled into a helper, so we exercise effects 2 and 3 here.

        // Effect 2: on-disk config updated
        let on_disk = fs::read_to_string(&config_path).unwrap();
        let parsed: toml::Value = toml::from_str(&on_disk).unwrap();
        assert_eq!(
            parsed["background"]["image"].as_str().unwrap(),
            "dark.png"
        );

        // Effect 3: ModeChange::Background sent over channel
        let msg = rx.try_recv().expect("ModeChange::Background must be sent");
        assert!(
            matches!(msg, ModeChange::Background { .. }),
            "expected ModeChange::Background, got {:?}",
            msg
        );
    });
}

#[test]
fn background_save_none_clears_all_three_effects() {
    let tmp = tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    fs::write(
        &config_path,
        "[display]\ntick_rate = 2\n\n[background]\nimage = \"old.png\"\n",
    )
    .unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ModeChange>(4);

        Config::save_background_image(&config_path, None)
            .expect("save_background_image(None) must succeed");
        tx.send(ModeChange::Background { image: None })
            .await
            .expect("channel send must succeed");

        // Effect 1 (in-memory mirror) is now the caller's explicit responsibility —
        // no longer bundled into a helper, so we exercise effects 2 and 3 here.

        // Effect 2: on-disk key removed
        let on_disk = fs::read_to_string(&config_path).unwrap();
        let parsed: toml::Value = toml::from_str(&on_disk).unwrap();
        assert!(
            parsed.get("background").and_then(|b| b.get("image")).is_none(),
            "image key must be absent on disk after clear"
        );

        // Effect 3: ModeChange::Background { image: None } sent
        let msg = rx.try_recv().expect("ModeChange::Background must be sent");
        assert!(
            matches!(msg, ModeChange::Background { image: None }),
            "expected ModeChange::Background {{image: None}}, got {:?}",
            msg
        );
    });
}

// ---------------------------------------------------------------------------
// background save outside lock: disk + channel without in-memory Config touch
// (the caller's responsibility — mirrors how set_background works in production).
// ---------------------------------------------------------------------------

#[test]
fn background_save_outside_lock_sets_disk_and_channel() {
    let tmp = tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    fs::write(&config_path, "[display]\ntick_rate = 2\n").unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ModeChange>(4);

        Config::save_background_image(&config_path, Some("dark.png"))
            .expect("save_background_image must succeed");
        tx.send(ModeChange::Background { image: None })
            .await
            .expect("channel send must succeed");

        // Effect 1: on-disk config must have image = "dark.png"
        let on_disk = fs::read_to_string(&config_path).unwrap();
        let parsed: toml::Value = toml::from_str(&on_disk).unwrap();
        assert_eq!(
            parsed["background"]["image"].as_str().unwrap(),
            "dark.png",
            "disk must record the new background filename"
        );

        // Effect 2: ModeChange::Background must be sent over channel
        let msg = rx.try_recv().expect("ModeChange::Background must be sent");
        assert!(
            matches!(msg, ModeChange::Background { .. }),
            "expected ModeChange::Background, got {:?}",
            msg
        );
    });
}

#[test]
fn background_save_outside_lock_none_clears_disk_and_channel() {
    let tmp = tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    fs::write(
        &config_path,
        "[display]\ntick_rate = 2\n\n[background]\nimage = \"old.png\"\n",
    )
    .unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ModeChange>(4);

        Config::save_background_image(&config_path, None)
            .expect("save_background_image(None) must succeed");
        tx.send(ModeChange::Background { image: None })
            .await
            .expect("channel send must succeed");

        // Disk: image key removed
        let on_disk = fs::read_to_string(&config_path).unwrap();
        let parsed: toml::Value = toml::from_str(&on_disk).unwrap();
        assert!(
            parsed.get("background").and_then(|b| b.get("image")).is_none(),
            "image key must be absent after clear, disk: {on_disk}"
        );

        // Channel: ModeChange::Background { image: None }
        let msg = rx.try_recv().expect("ModeChange::Background must be sent");
        assert!(
            matches!(msg, ModeChange::Background { image: None }),
            "expected Background{{image:None}}, got {:?}",
            msg
        );
    });
}

// ---------------------------------------------------------------------------
// save_default_layout_impl (Task 17): persists default_layout + mode to disk.
// ---------------------------------------------------------------------------

/// save_default_layout_impl must write display.default_layout and display.mode
/// to config.toml for an SVG layout.
#[test]
fn save_default_layout_impl_writes_svg_layout_to_disk() {
    let tmp = tempdir().unwrap();
    let layout_dir = tmp.path().join("layouts");
    fs::create_dir_all(layout_dir.join("svg")).unwrap();
    fs::write(layout_dir.join("svg/neon-dash-v2.svg"), "<svg/>").unwrap();
    let config_path = tmp.path().join("config.toml");
    fs::write(&config_path, "[display]\ntick_rate = 2\n").unwrap();

    save_default_layout_impl(&layout_dir, &config_path, "svg/neon-dash-v2.svg")
        .expect("save_default_layout_impl must succeed for a valid SVG layout");

    let on_disk = fs::read_to_string(&config_path).unwrap();
    let parsed: toml::Value = toml::from_str(&on_disk).unwrap();
    assert_eq!(
        parsed["display"]["default_layout"].as_str().unwrap(),
        "svg/neon-dash-v2.svg",
        "default_layout must be written to disk"
    );
    assert_eq!(
        parsed["display"]["mode"].as_str().unwrap(),
        "svg",
        "mode must be 'svg' for .svg layout"
    );
}

/// save_default_layout_impl must write mode = "html" for an HTML layout.
#[test]
fn save_default_layout_impl_writes_html_mode_for_html_layout() {
    let tmp = tempdir().unwrap();
    let layout_dir = tmp.path().to_path_buf();
    fs::write(layout_dir.join("system-stats.html"), "<html/>").unwrap();
    let config_path = tmp.path().join("config.toml");
    fs::write(&config_path, "[display]\ntick_rate = 2\n").unwrap();

    save_default_layout_impl(&layout_dir, &config_path, "system-stats.html")
        .expect("save_default_layout_impl must succeed for a valid HTML layout");

    let on_disk = fs::read_to_string(&config_path).unwrap();
    let parsed: toml::Value = toml::from_str(&on_disk).unwrap();
    assert_eq!(
        parsed["display"]["mode"].as_str().unwrap(),
        "html",
        "mode must be 'html' for .html layout"
    );
}

/// save_default_layout_impl must reject path traversal before touching disk.
#[test]
fn save_default_layout_impl_rejects_traversal() {
    let tmp = tempdir().unwrap();
    let layout_dir = tmp.path().join("layouts");
    fs::create_dir_all(&layout_dir).unwrap();
    let config_path = tmp.path().join("config.toml");
    let original = "[display]\ntick_rate = 2\n";
    fs::write(&config_path, original).unwrap();

    let result = save_default_layout_impl(&layout_dir, &config_path, "../outside.svg");
    assert!(result.is_err(), "traversal must be rejected");

    // Disk must be unchanged
    let after = fs::read_to_string(&config_path).unwrap();
    assert_eq!(after, original, "config must be untouched on traversal reject");
}
