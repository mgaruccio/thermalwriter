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
    let config_dir = config_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let layout_dir = config_dir.join("layouts");
    let background_dir = config_dir.join("backgrounds");
    std::fs::create_dir_all(&layout_dir).expect("failed to create layout directory");
    std::fs::create_dir_all(&background_dir).expect("failed to create background directory");
    builtin_layouts::seed_layout_dir(&layout_dir).expect("failed to seed built-in layouts");
    builtin_layouts::seed_background_dir(&background_dir)
        .expect("failed to seed built-in backgrounds");

    let mut builder = tauri::Builder::default();
    #[cfg(debug_assertions)]
    {
        builder = builder.plugin(tauri_plugin_mcp_bridge::init());
    }

    builder
        .manage(commands::RendererState::new(
            layout_dir,
            background_dir,
            config_path,
        ))
        .invoke_handler(tauri::generate_handler![
            commands::list_layouts,
            commands::get_layout_vars,
            commands::get_saved_vars,
            commands::list_sensors,
            commands::render_preview,
            commands::save_config,
            commands::apply_to_daemon,
            commands::list_backgrounds,
            commands::read_background,
            commands::set_background,
            commands::save_background,
            commands::get_active_background,
            commands::import_background,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
