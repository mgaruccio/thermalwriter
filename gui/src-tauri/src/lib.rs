/// Minimal Tauri command — wired up so the scaffold is end-to-end testable
/// without requiring the full renderer pipeline. Replaced in Task 10.
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {} — thermalwriter GUI is ready.", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Work around webkit2gtk 2.52+ DMABUF renderer crash on Wayland (GDK error 71).
    // Must be set before WebKit initializes.
    // SAFETY: single-threaded at startup, no other code has read this env var yet.
    unsafe {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
