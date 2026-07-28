fn main() {
    let capabilities_pattern = if std::env::var("CARGO_FEATURE_DEVTOOLS").is_ok() {
        "./capability-templates/devtools.json"
    } else {
        "./capability-templates/default.json"
    };

    let attributes = tauri_build::Attributes::new().capabilities_path_pattern(capabilities_pattern);

    tauri_build::try_build(attributes).expect("failed to run build script");
}
