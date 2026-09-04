// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod pi_bun;

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

/// M1 PoC：在嵌入式 bun 运行时内执行 pi-bundle/hello.js。
/// skal_evaluate 是同步阻塞调用 —— 必须 off main thread。
#[tauri::command]
async fn pi_bun_smoke(app: tauri::AppHandle) -> Result<String, String> {
    let data_dir = app_data_dir(&app)?;
    tauri::async_runtime::spawn_blocking(move || pi_bun::smoke(&data_dir))
        .await
        .map_err(|e| format!("join: {e}"))?
}

fn app_data_dir(app: &tauri::AppHandle) -> Result<String, String> {
    use tauri::Manager;
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?
        .to_string_lossy()
        .into_owned();
    std::fs::create_dir_all(&dir).map_err(|e| format!("create data dir: {e}"))?;
    Ok(dir)
}

/// M2：加载 agent bundle（幂等，App 启动时调用）。
#[tauri::command]
async fn agent_init(app: tauri::AppHandle) -> Result<(), String> {
    let data_dir = app_data_dir(&app)?;
    tauri::async_runtime::spawn_blocking(move || pi_bun::agent_init(&data_dir))
        .await
        .map_err(|e| format!("join: {e}"))?
}

/// M2：提交 prompt（kick；回复经 `pi-agent-event` 事件流回 WebView）。
#[tauri::command]
async fn agent_prompt(text: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || pi_bun::agent_prompt(&text))
        .await
        .map_err(|e| format!("join: {e}"))?
}

/// M2：轮询 agent 状态。
#[tauri::command]
async fn agent_status() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(pi_bun::agent_status)
        .await
        .map_err(|e| format!("join: {e}"))?
}

/// M2：保存 provider 凭证（M2 文件态；M3 迁 keystore，见 D4）。
#[tauri::command]
fn set_creds(app: tauri::AppHandle, provider: String, api_key: String) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    let path = std::path::Path::new(&dir).join("creds.json");
    let mut v: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::json!({}));
    v[provider.as_str()] = serde_json::json!(api_key);
    std::fs::write(&path, v.to_string()).map_err(|e| format!("write creds: {e}"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .setup(|app| {
            // agent_event → WebView 事件桥（loopback 线程 → main emit）
            let handle = app.handle().clone();
            pi_bun::loopback::set_event_sink(move |json| {
                use tauri::Emitter;
                let _ = handle.emit("pi-agent-event", json);
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            pi_bun_smoke,
            agent_init,
            agent_prompt,
            agent_status,
            set_creds
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
