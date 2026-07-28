fn main() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let templates = manifest_dir.join("capability-templates");
    let out_cap = manifest_dir.join("capabilities").join("default.json");

    let template = if std::env::var("CARGO_FEATURE_DEVTOOLS").is_ok() {
        templates.join("devtools.json")
    } else {
        templates.join("default.json")
    };

    let contents = std::fs::read_to_string(&template).unwrap_or_else(|e| {
        panic!(
            "failed to read capability template {}: {e}",
            template.display()
        )
    });
    std::fs::write(&out_cap, contents)
        .unwrap_or_else(|e| panic!("failed to write capability file {}: {e}", out_cap.display()));

    tauri_build::build()
}
