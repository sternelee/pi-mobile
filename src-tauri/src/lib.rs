// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod approval;
mod creds;
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

/// M2 收尾：重启恢复 —— 取 boot 时从最新 JSONL 会话回放的历史消息。
#[tauri::command]
async fn agent_history() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(pi_bun::agent_history)
        .await
        .map_err(|e| format!("join: {e}"))?
}

/// D4：保存 provider 凭证（桌面 keyring；Android 沙箱文件态，见 creds.rs）。
#[tauri::command]
fn set_creds(app: tauri::AppHandle, provider: String, api_key: String) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    creds::set(&dir, &provider, &api_key)
}

/// M3：回填审批决策（allow / deny / always），唤醒阻塞中的 approval_request。
#[tauri::command]
fn approval_respond(request_id: String, decision: String) -> Result<(), String> {
    approval::respond(&request_id, &decision)
}

/// M3：回滚 workspace 文件到上一次覆盖写入前（消费对应备份）。
#[tauri::command]
fn workspace_revert(path: String) -> Result<u64, String> {
    pi_bun::loopback::revert_workspace_file(&path)
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
            // agent_event / approval_required → WebView 事件桥（loopback 线程 → main emit）
            let handle = app.handle().clone();
            let emit = move |json: &str| {
                use tauri::Emitter;
                let _ = handle.emit("pi-agent-event", json);
            };
            pi_bun::loopback::set_event_sink(emit.clone());
            approval::set_event_sink(emit);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            pi_bun_smoke,
            agent_init,
            agent_prompt,
            agent_status,
            agent_history,
            set_creds,
            approval_respond,
            workspace_revert
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
