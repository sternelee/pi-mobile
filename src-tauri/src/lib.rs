// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod pi_bun;

use tauri::Manager;

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

/// M1 PoC：在嵌入式 bun 运行时内执行 pi-bundle/hello.js。
/// skal_evaluate 是同步阻塞调用 —— 必须 off main thread。
#[tauri::command]
async fn pi_bun_smoke(app: tauri::AppHandle) -> Result<String, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?
        .to_string_lossy()
        .into_owned();
    std::fs::create_dir_all(&data_dir).map_err(|e| format!("create data dir: {e}"))?;
    tauri::async_runtime::spawn_blocking(move || pi_bun::smoke(&data_dir))
        .await
        .map_err(|e| format!("join: {e}"))?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .invoke_handler(tauri::generate_handler![greet, pi_bun_smoke])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
