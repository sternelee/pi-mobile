// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod approval;
mod ask_user;
mod creds;
mod goal;
mod http_tool;
mod keepalive;
mod mcp;
mod native;
mod oauth;
mod pi_bun;
mod preview;
mod script;
mod sessions;
mod skills;

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
    // 标准子目录无条件创建：iOS 上 agent 运行时被门控（agent_init 提前返回），
    // 目录若只在 agent_init 里建，session_list / 文件树等命令会报 ENOENT。
    for sub in ["sessions", "workspace"] {
        std::fs::create_dir_all(format!("{dir}/{sub}"))
            .map_err(|e| format!("create {sub} dir: {e}"))?;
    }
    Ok(dir)
}

/// M2：加载 agent bundle（幂等，App 启动时调用）。
#[tauri::command]
async fn agent_init(app: tauri::AppHandle) -> Result<(), String> {
    let data_dir = app_data_dir(&app)?;
    let r = tauri::async_runtime::spawn_blocking(move || pi_bun::agent_init(&data_dir))
        .await
        .map_err(|e| format!("join: {e}"))?;
    // 失败要进设备日志（真机上只有设备日志可看，前端那条 status 文字
    // 拿不出来）—— 错误串里带 JS 侧 boot_error，是定位根因的关键。
    if let Err(e) = &r {
        pi_bun::logcat(&format!("agent_init failed: {e}"));
    }
    r
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

/// M3：中止当前 agent 运行。
#[tauri::command]
async fn agent_stop() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(pi_bun::agent_stop)
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

/// D4：探测某 provider 是否已配置 API key（UI 决定是否显示 key 输入）。
#[tauri::command]
fn has_creds(app: tauri::AppHandle, provider: String) -> Result<bool, String> {
    let dir = app_data_dir(&app)?;
    Ok(creds::get(&dir, &provider).is_some() || creds::get_json(&dir, &provider).is_some())
}

/// provider.json —— 用户选择的默认模型（provider + modelId）。
/// bundle 侧用 pi-ai 目录把 (provider, modelId) 解析成完整模型对象。
fn default_model_path(dir: &str) -> std::path::PathBuf {
    std::path::Path::new(dir).join("provider.json")
}

/// 读取默认模型选择；未配置返回 "null"。
#[tauri::command]
fn get_default_model(app: tauri::AppHandle) -> Result<String, String> {
    let dir = app_data_dir(&app)?;
    match std::fs::read_to_string(default_model_path(&dir)) {
        Ok(raw) => {
            let v: serde_json::Value =
                serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
            Ok(v.to_string())
        }
        Err(_) => Ok("null".into()),
    }
}

/// 保存默认模型选择（agent_init 注入 __PI_CONFIG.providerConfig，boot 时生效；
/// 运行中经 __pi_model_select 热切换）。provider 为空串即清除选择。
#[tauri::command]
fn set_default_model(
    app: tauri::AppHandle,
    provider: String,
    model_id: String,
) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    let path = default_model_path(&dir);
    if provider.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    let v = serde_json::json!({ "provider": provider, "modelId": model_id });
    std::fs::write(&path, v.to_string()).map_err(|e| format!("write provider.json: {e}"))
}

/// M3：回填审批决策（allow / deny / always），经 resolver 反向 skal_evaluate
/// 注入运行时（kick+resolve 模式）—— 必须 off main thread。
#[tauri::command]
async fn approval_respond(request_id: String, decision: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || approval::respond(&request_id, &decision))
        .await
        .map_err(|e| format!("join: {e}"))?
}

/// 审批策略读写（write 基线 ask/auto）——设置页的审批开关。
#[tauri::command]
fn approval_policy_get() -> String {
    approval::policy_get()
}

#[tauri::command]
fn approval_policy_set(policy: String) -> Result<(), String> {
    approval::policy_set(&policy)
}

/// M4：MCP 服务器配置增删查（存 mcp.json，重启/重连后生效）。
#[tauri::command]
fn mcp_list(app: tauri::AppHandle) -> Result<String, String> {
    let dir = app_data_dir(&app)?;
    mcp::list(&dir)
}

/// D12 Skills：列表 / URL 安装（网络，off main thread）/ 启停 / 删除。
#[tauri::command]
fn skills_list(app: tauri::AppHandle) -> Result<String, String> {
    let dir = app_data_dir(&app)?;
    skills::list(&dir)
}

#[tauri::command]
async fn skills_install(app: tauri::AppHandle, url: String) -> Result<String, String> {
    let dir = app_data_dir(&app)?;
    tauri::async_runtime::spawn_blocking(move || skills::install(&dir, &url))
        .await
        .map_err(|e| format!("join: {e}"))?
        .map(|entry| entry.to_string())
}

#[tauri::command]
fn skills_toggle(app: tauri::AppHandle, id: String, enabled: bool) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    skills::toggle(&dir, &id, enabled)
}

#[tauri::command]
fn skills_remove(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    skills::remove(&dir, &id)
}

/// D12：热生效——改完 registry 后重新注入（bundle `__pi_skills_apply` kick）。
#[tauri::command]
async fn skills_reconnect() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| pi_bun::call_string_global("__pi_skills_apply", ""))
        .await
        .map_err(|e| format!("join: {e}"))??;
    Ok(())
}

#[tauri::command]
fn mcp_add(
    app: tauri::AppHandle,
    name: String,
    url: String,
    timeout_ms: Option<u64>,
    headers: Option<serde_json::Value>,
) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    mcp::add(&dir, &name, &url, timeout_ms, headers)
}

#[tauri::command]
fn mcp_remove(app: tauri::AppHandle, name: String) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    mcp::remove(&dir, &name)
}

/// M4：热重连 MCP 服务器（改配置后立即可用，无需重启）。
#[tauri::command]
async fn mcp_reconnect() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(pi_bun::mcp_reconnect)
        .await
        .map_err(|e| format!("join: {e}"))?
}

/// 扩展（pi-goal 移动原生化）：持久目标设置/清除/查询。
/// set/clear 后 UI 调 mcp_reconnect 同款机制经 `__pi_goal_apply` 热生效。
#[tauri::command]
fn goal_get(app: tauri::AppHandle) -> Result<String, String> {
    let dir = app_data_dir(&app)?;
    goal::get(&dir)
}

#[tauri::command]
async fn pi_call_global(fn_name: String, arg: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || pi_bun::call_string_global(&fn_name, &arg))
        .await
        .map_err(|e| format!("join: {e}"))?
}

// ── M6：系统原生能力（UI 侧）──────────────────────────────────────
//
// agent 工具侧走 hostcall `native`（loopback.rs，每条连接独立线程，安全）；
// UI 侧走这两个命令。
//
// ⚠️ 两个命令**必须** async + spawn_blocking，不能写成同步命令：
// Tauri 的同步命令在宿主线程上执行，而插件的 `run_mobile_plugin` 是阻塞的
// —— 它等到原生侧回调才返回。iOS 上 `requestWhenInUseAuthorization` 恰好
// 又必须回主队列才能弹窗（插件 Swift 里就是 `DispatchQueue.main.async`）：
// 同步命令占着主线程等回复、弹窗等主线程 → 死锁，实测「点 Allow 整屏卡死」。
// CLLocationManager 在 .notDetermined 时还会把 invoke 挂起直到用户作答，
// 阻塞窗口不是一个瞬间。
// 这与 `pi_bun_smoke` 的纪律一致：任何同步阻塞调用都不得占主线程。

/// 能力清单 + 各自权限态（设置页展示用）。
#[tauri::command]
async fn native_capabilities() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(native::status)
        .await
        .map_err(|e| format!("join: {e}"))
}

/// 启动预览服务（D15）并返回端口。幂等——UI 每次打开预览都能安全调。
///
/// 懒启动：不给 app 启动路径加任何东西（预览不常用，而 bind 失败也不该
/// 影响 agent 启动）。
#[tauri::command]
async fn preview_start() -> Result<u16, String> {
    tauri::async_runtime::spawn_blocking(preview::start)
        .await
        .map_err(|e| format!("join: {e}"))?
}

/// workspace 里的 html 入口候选（有界扫描），给预览面板的选择列表。
#[tauri::command]
async fn preview_targets() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(preview::targets)
        .await
        .map_err(|e| format!("join: {e}"))
}

/// 在**系统浏览器**里打开预览（D15 的逃生口）。
///
/// 存在的理由只有一个：A3 —— 预览页里的同步死循环会**冻住整个 app**（iframe 与
/// app 共用 WebView 主线程，真机实测确认）。系统浏览器是**独立进程**，页面再重
/// 也带不倒 pi-mobile，所以这是真机上唯一可靠的隔离手段。
///
/// 局限要说清：它**不能在已经卡死之后救场**（卡死时连这个按钮都点不动），它是
/// **事前选择**——早知道页面重就先用浏览器开。事后只能靠系统手势划掉 app。
#[tauri::command]
async fn preview_open_external(
    app: tauri::AppHandle,
    port: u16,
    path: String,
) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let url = format!(
        "http://127.0.0.1:{port}/{}",
        path.trim_start_matches('/')
    );
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| format!("open_url: {e}"))
}

/// 脚本能力目录（D14）。
///
/// UI 用它把审批卡上的能力 id 翻成人话——**说明文字只在 `script.rs` 里写一份**。
/// 在 TS 里另拄一张表就是「分头写」，按 D14 的说法必然漂移：那两份一旦不一，
/// 用户看到的就是与实际授权集不同的东西，而审批卡的全部意义就在「所见即所授」。
#[tauri::command]
async fn script_capabilities() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(crate::script::catalog)
        .await
        .map_err(|e| format!("join: {e}"))
}

/// 请求某项能力的系统权限（弹系统窗，用户作答前一直挂着）。
#[tauri::command]
async fn native_request_permission(capability: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || native::request(&capability))
        .await
        .map_err(|e| format!("join: {e}"))?
}

#[tauri::command]
fn goal_set(app: tauri::AppHandle, objective: String) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    goal::set(&dir, &objective)
}

#[tauri::command]
fn goal_clear(app: tauri::AppHandle) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    goal::clear(&dir)
}

/// 扩展：回填 ask_user 答案（JSON：{response: ...} 或 {response: null, cancelled: true}）。
/// 经 resolver 反向 skal_evaluate 注入运行时 —— 必须 off main thread。
#[tauri::command]
async fn ask_user_respond(request_id: String, answer: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || ask_user::respond(&request_id, &answer))
        .await
        .map_err(|e| format!("join: {e}"))?
}

/// M3：回滚 workspace 文件到上一次覆盖写入前（消费对应备份）。
#[tauri::command]
fn workspace_revert(path: String) -> Result<u64, String> {
    pi_bun::loopback::revert_workspace_file(&path)
}

/// M3：查询某 workspace 文件的最新备份时间戳（null = 无备份）。
#[tauri::command]
fn workspace_backup_info(path: String) -> Result<String, String> {
    match pi_bun::loopback::latest_backup_millis(&path) {
        Some(m) => Ok(format!("{{\"millis\":{m}}}")),
        None => Ok("null".into()),
    }
}

/// M3：workspace 文件树（扁平列表，深度 ≤6 / 条目 ≤500）。
#[tauri::command]
fn workspace_tree() -> Result<String, String> {
    pi_bun::loopback::workspace_tree()
}

/// M3：workspace 文件只读预览（上限 256KB）。
#[tauri::command]
fn workspace_read(path: String) -> Result<String, String> {
    pi_bun::loopback::workspace_read(&path)
}

/// M3：会话索引（modifiedAt 倒序，供会话列表 UI）。
#[tauri::command]
fn session_list(app: tauri::AppHandle) -> Result<String, String> {
    let dir = app_data_dir(&app)?;
    sessions::list(&format!("{dir}/sessions"))
}

/// M3：切换到指定会话。
#[tauri::command]
async fn session_open(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || pi_bun::session_open(&id))
        .await
        .map_err(|e| format!("join: {e}"))?
}

/// M3：新建空白会话。
#[tauri::command]
async fn session_new() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(pi_bun::session_new)
        .await
        .map_err(|e| format!("join: {e}"))?
}

/// 会话删除：删 JSONL（当前会话被删时 UI 先 session_new 再删，避免
/// 运行中的 repo append 在无 header 的孤儿文件上重建）。
#[tauri::command]
fn session_delete(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    sessions::delete(&format!("{dir}/sessions"), &id)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_deep_link::init())
        // M6 系统能力：插件把 Android JNI / iOS ObjC 管线封装在各自原生侧，
        // Rust 只调 run_mobile_plugin —— 不会重蹈 keepalive.rs 裸 JNI 覆辙。
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_geolocation::init())
        // 自建设备能力：官方插件覆盖不到的（1b 的日历/通讯录/照片），以及官方
        // 插件在其上不可用的（Android 定位走 Google fused provider 且无超时，
        // 国内 ROM 上会永久挂起）。详见 plugins/pi-native/src/lib.rs 头注。
        .plugin(tauri_plugin_pi_native::init())
        .setup(|app| {
            // agent_event / approval_required / ask_user → WebView 事件桥。
            // oauth_open_url 同时唤起系统浏览器（provider 授权页）。
            let handle = app.handle().clone();
            let emit = move |json: &str| {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(json) {
                    if v["type"] == "oauth_open_url" {
                        if let Some(url) = v["url"].as_str() {
                            let app2 = handle.clone();
                            let url = url.to_string();
                            tauri::async_runtime::spawn(async move {
                                use tauri_plugin_opener::OpenerExt;
                                let _ = app2.opener().open_url(url, None::<&str>);
                            });
                        }
                    }
                }
                use tauri::Emitter;
                let _ = handle.emit("pi-agent-event", json);
            };
            pi_bun::loopback::set_event_sink(emit.clone());
            approval::set_event_sink(emit.clone());
            ask_user::set_event_sink(emit);

            // M6：native 能力层需要 AppHandle 调插件 API
            // （ClipboardExt/NotificationExt/GeolocationExt 都实现于 Manager）。
            // 注意：`handle` 已被上面的 emit 闭包 move 走，这里重新取一份。
            native::set_app(app.handle().clone());

            // pimobile:// deep link → bundle 的 __pi_oauth_callback（oauth.rs
            // 注释：辅助回调通道；本工程 manifest 已注册 pimobile scheme）。
            use tauri_plugin_deep_link::DeepLinkExt;
            let dl = app.handle().clone();
            let _ = dl;
            app.deep_link().on_open_url(move |event| {
                for u in event.urls() {
                    let s = u.to_string();
                    let _ = tauri::async_runtime::spawn_blocking(move || {
                        let _ = pi_bun::call_string_global("__pi_oauth_callback", &s);
                    });
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            pi_bun_smoke,
            agent_init,
            agent_prompt,
            agent_status,
            agent_stop,
            agent_history,
            native_capabilities,
            native_request_permission,
            preview_start,
            preview_targets,
            preview_open_external,
            script_capabilities,
            set_creds,
            has_creds,
            get_default_model,
            set_default_model,
            approval_respond,
            approval_policy_get,
            approval_policy_set,
            ask_user_respond,
            workspace_revert,
            workspace_backup_info,
            workspace_tree,
            workspace_read,
            session_list,
            session_open,
            session_new,
            session_delete,
            mcp_list,
            mcp_add,
            mcp_remove,
            mcp_reconnect,
            goal_get,
            goal_set,
            goal_clear,
            pi_call_global,
            skills_list,
            skills_install,
            skills_toggle,
            skills_remove,
            skills_reconnect
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
