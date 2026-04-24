/// Minimal Tauri command — wired up so the scaffold is end-to-end testable
/// without requiring the full renderer pipeline. Replaced in Task 10.
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {} — thermalwriter GUI is ready.", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
