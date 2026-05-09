use std::path::PathBuf;

use thermalwriter::config::{Config, builtin_layouts};

mod commands;
mod error;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = env_logger::try_init();

    // Workaround for WebKit2GTK DMABUF renderer bug under Wayland on this
    // hardware/distro: forces the SHM/EGL fallback. Must run before tauri
    // boots WebKit. See commit 00da89e.
    unsafe {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    let config_path = Config::default_path();
    let layout_dir = config_path
        .parent()
        .map(|p| p.join("layouts"))
        .unwrap_or_else(|| PathBuf::from("layouts"));
    std::fs::create_dir_all(&layout_dir).expect("failed to create layout directory");
    builtin_layouts::seed_layout_dir(&layout_dir).expect("failed to seed built-in layouts");

    tauri::Builder::default()
        .manage(commands::RendererState::new(layout_dir, config_path))
        .invoke_handler(tauri::generate_handler![
            commands::list_layouts,
            commands::get_layout_vars,
            commands::get_saved_vars,
            commands::list_sensors,
            commands::render_preview,
            commands::save_config,
            commands::apply_to_daemon,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
