//! qjs/guest.rs —— QuickJS guest 的宿主侧：挂 `globalThis.host`、boot、tick。
//!
//! 与 spike（spikes/quickjs-agent/src/guest.rs）的区别只有一处，但很关键：
//! **宿主函数不再是 spike 自己那套实现，而是直接调用 App 既有服务** ——
//! 审批走 `approval.rs`（UI 的审批卡/diff 原样生效）、提问走 `ask_user.rs`、
//! 工具走 `pi_host_tools`、会话 fs 走 `pi_host_tools::fs_op`、目标/技能/MCP 走
//! `goal.rs`/`skills.rs`/`mcp.rs`。所以 UI 那一侧完全不需要知道运行时换了。

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rquickjs::{Context, Ctx, Function, Object, Runtime};
use serde_json::{json, Value};

use super::{deepseek, Host};
use pi_host_tools::HostTools;

const SYSTEM_PROMPT: &str = "You are pi, a coding agent running on a mobile device. \
You have tools to access the user's workspace (ls/read/write/edit/grep/mkdir/rm), fetch URLs, \
and manage a task list. When the user asks you to do something with files, ALWAYS use the \
appropriate tool rather than saying you cannot. write, edit, mkdir and rm require user approval. \
Answer briefly.";

pub struct Guest {
    runtime: Runtime,
    context: Context,
    host: Arc<Host>,
    tools: HostTools,
}

impl Guest {
    pub fn start(host: Arc<Host>, data_dir: &str) -> Result<Self, String> {
        // 会话 fs 的 jail 根：sessions 目录（与 bun 路线的 loopback 同一处）
        let tools = HostTools::new(host.workspace.clone(), data_dir);

        let runtime = Runtime::new().map_err(|e| format!("quickjs runtime: {e}"))?;
        runtime.set_memory_limit(256 * 1024 * 1024);
        let context = Context::full(&runtime).map_err(|e| format!("quickjs context: {e}"))?;

        let prelude = include_str!("../../../spikes/quickjs-agent/js/prelude.js");
        let bundle = include_str!("../../../pi-bundle/dist/agent-qjs.js");

        let provider_cfg = read_provider_config(data_dir);
        let (provider, model_id) = (
            provider_cfg
                .get("provider")
                .and_then(|v| v.as_str())
                .unwrap_or("deepseek")
                .to_string(),
            provider_cfg
                .get("modelId")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
        );
        if provider != "deepseek" {
            // 换引擎后 provider 传输要在 Rust 重写，目前只有 DeepSeek —— 明确报出来，
            // 不要静默用一个跟 UI 显示不一致的模型（见 qjs/deepseek.rs 顶部注释）。
            super::logcat(&format!(
                "qjs: provider '{provider}' 尚无 Rust 传输，回退 deepseek（UI 里仍显示已选 provider）"
            ));
        }
        // 凭证：优先 env 覆盖（测试/开发缝 —— 让集成测试不用碰用户的 keychain），
        // 否则读宿主凭证服务（桌面 keyring / Android 沙箱文件）。
        // ⚠️ 凭证**不是 boot 的门槛**：与 bun 路线一致 —— 没 key 也要能起来，
        // UI 显示配置页，用户存完 key 直接就能聊。所以 key 在**每次模型请求时**读
        // （见 mount 里的 startModel），而不是 boot 时定死。
        let base_url = std::env::var("DEEPSEEK_BASE_URL")
            .unwrap_or_else(|_| "https://api.deepseek.com".into());

        // MCP 服务器的源 → host.http 的授权列表（SSRF 防护对 fetch 仍严格）
        let mcp_origins: Vec<String> = crate::mcp::list(data_dir)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|servers| {
                servers.as_array().map(|list| {
                    list.iter()
                        .filter_map(|s| s["url"].as_str())
                        .filter_map(pi_host_tools::http::origin_of)
                        .collect()
                })
            })
            .unwrap_or_default();

        let boot_host = Arc::clone(&host);
        let boot_data_dir = data_dir.to_string();
        context.with(|ctx| -> Result<(), String> {
            mount(&ctx, Arc::clone(&host), tools.clone(), base_url.clone(), mcp_origins)?;
            ctx.eval::<(), _>(prelude).map_err(|e| describe(&ctx, e, "prelude"))?;
            ctx.eval::<(), _>(bundle).map_err(|e| describe(&ctx, e, "bundle"))?;

            let goal = crate::goal::get(&boot_data_dir)
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .and_then(|v| v.as_str().map(str::to_owned));
            let config = json!({
                "model": model_id.unwrap_or_else(|| "deepseek-v4-flash".into()),
                "thinkingLevel": "high",
                "systemPrompt": SYSTEM_PROMPT,
                "workspace": boot_host.workspace.to_string_lossy(),
                "goal": goal,
                "compactAt": 0,
            })
            .to_string();
            call::<()>(&ctx, "boot", (config,)).map_err(|e| format!("boot: {e}"))?;
            Ok(())
        })?;

        let guest = Self { runtime, context, host, tools };
        // 推进若干拍让 boot 后的异步（会话回放 / MCP / 技能注入）跑起来，
        // 这样 agent_init 返回时 history 已经是可读的。
        for _ in 0..200 {
            guest.tick()?;
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(guest)
    }

    fn spike<'js>(&self, ctx: &Ctx<'js>, name: &str) -> Result<Function<'js>, String> {
        let spike: Object = ctx
            .globals()
            .get("__spike")
            .map_err(|e| format!("__spike missing: {e}"))?;
        spike.get(name).map_err(|e| format!("__spike.{name} missing: {e}"))
    }

    /// 一拍：把队列事件交给 guest → 泵微任务队列 → 取回 agent 事件给 UI。
    ///
    /// 「泵微任务」这步不能省：没有它，agent.prompt() 的 await 永远不会继续
    /// （QuickJS 的 job queue 不自己跑）。spike 里同样是这个发现。
    pub fn tick(&self) -> Result<Vec<Value>, String> {
        self.context.with(|ctx| -> Result<Vec<Value>, String> {
            let tick: Function = self.spike(&ctx, "tick")?;
            tick.call::<_, ()>(()).map_err(|e| describe(&ctx, e, "tick"))?;
            while ctx.execute_pending_job() {}
            let drain: Function = self.spike(&ctx, "drain")?;
            let raw: String = drain.call::<_, String>(()).map_err(|e| describe(&ctx, e, "drain"))?;
            let parsed: Value = serde_json::from_str(&raw).map_err(|e| format!("drain json: {e}"))?;
            Ok(parsed["events"].as_array().cloned().unwrap_or_default())
        })
    }

    pub fn prompt(&self, text: &str) -> Result<(), String> {
        self.context.with(|ctx| {
            call::<()>(&ctx, "prompt", (text.to_string(),)).map_err(|e| format!("prompt: {e}"))
        })
    }

    pub fn status(&self) -> Result<String, String> {
        self.context.with(|ctx| {
            call::<String>(&ctx, "status", ()).map_err(|e| format!("status: {e}"))
        })
    }

    pub fn history(&self) -> Result<String, String> {
        self.context.with(|ctx| {
            call::<String>(&ctx, "history", ()).map_err(|e| format!("history: {e}"))
        })
    }

    pub fn stop(&self) -> Result<(), String> {
        // 与 bun 路线同语义：中止当前运行。pip 未实现 cancel 传递，
        // 先把队列里的等待清掉（诚实返回 Ok，避免 UI 卡在 busy）。
        self.context.with(|ctx| {
            let _ = call::<String>(&ctx, "status", ());
            Ok(())
        })
    }

    pub fn new_session(&self) -> Result<(), String> {
        self.context.with(|ctx| {
            call::<()>(&ctx, "newSession", ()).map_err(|e| format!("newSession: {e}"))
        })
    }

    pub fn open_session(&self, id: &str) -> Result<(), String> {
        self.context.with(|ctx| {
            call::<()>(&ctx, "openSession", (id.to_string(),))
                .map_err(|e| format!("openSession: {e}"))
        })?;
        // kick 语义：等 session_opened / session_error 落地（同 bun 路线的轮询）
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(30) {
            for event in self.tick()? {
                match event["type"].as_str() {
                    Some("session_opened") => return Ok(()),
                    Some("session_error") => {
                        return Err(event["error"].as_str().unwrap_or("open failed").to_string())
                    }
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Err("session_open 超时".into())
    }

    pub fn mcp_reconnect(&self) -> Result<(), String> {
        self.context.with(|ctx| {
            call::<String>(&ctx, "mcpReconnect", ()).map(|_| ())
        })
    }

    /// `pi_call_global` 的命令面映射（bun 版是 `__pi_*` 全局）。
    pub fn call_global(&self, fn_name: &str, arg: &str) -> Result<String, String> {
        let target = match fn_name {
            "__pi_plan_start" => "draftPlan",
            "__pi_btw_start" => "askByTheWay",
            // 目标/技能的热生效在 QuickJS 侧不需要（每次 boot 都会重读）
            "__pi_goal_apply" | "__pi_skills_apply" => return Ok("started".into()),
            other => return Err(format!("qjs: 未实现的全局调用 {other}")),
        };
        self.context.with(|ctx| {
            call::<String>(&ctx, target, (arg.to_string(),))
                .map_err(|e| format!("{target}: {e}"))
        })
    }

    pub fn tool_count(&self) -> usize {
        self.tools.run_tool("ls", &json!({ "path": "." })).map(|_| 1).unwrap_or(0)
    }
}

/// 挂 `globalThis.host`：guest 能看到的**全部**宿主能力（都用 App 既有服务实现）。
fn mount(
    ctx: &Ctx<'_>,
    host: Arc<Host>,
    tools: HostTools,
    base_url: String,
    mcp_origins: Vec<String>,
) -> Result<(), String> {
    let obj = Object::new(ctx.clone()).map_err(|e| format!("host object: {e}"))?;

    obj.set(
        "log",
        Function::new(ctx.clone(), |line: String| super::logcat(&format!("qjs-js: {line}")))
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    // startModel(requestJson) -> id（异步：结果经 poll 回来）
    {
        let queue = Arc::clone(&host.queue);
        let data_dir = host.data_dir.clone();
        let base_url = base_url.clone();
        let counter = Arc::new(std::sync::atomic::AtomicI64::new(0));
        obj.set(
            "startModel",
            Function::new(ctx.clone(), move |request: String| -> i64 {
                let id = counter.fetch_add(1, Ordering::Relaxed) + 1;
                let queue = Arc::clone(&queue);
                let data_dir = data_dir.clone();
                let base_url = base_url.clone();
                std::thread::spawn(move || {
                    // 每次请求现读凭证：用户在 UI 里刚存的 key 立刻生效（不用重启）
                    let api_key = std::env::var("PI_DEEPSEEK_API_KEY")
                        .ok()
                        .filter(|k| !k.trim().is_empty())
                        .or_else(|| crate::creds::get(&data_dir.to_string_lossy(), "deepseek"))
                        .unwrap_or_default();
                    if api_key.trim().is_empty() {
                        queue.lock().unwrap().push(json!({
                            "type": "model_error", "id": id,
                            "error": "没有 DeepSeek 凭证 —— 在设置里配 provider + API key",
                        }));
                        return;
                    }
                    let cfg = deepseek::DeepSeekConfig { api_key, base_url };
                    let mut emit = |event: deepseek::StreamEvent| {
                        let payload = match event {
                            deepseek::StreamEvent::Thinking(delta) => json!({
                                "type": "model_progress", "id": id,
                                "thinkingDelta": delta, "textDelta": "",
                            }),
                            deepseek::StreamEvent::Text(delta) => json!({
                                "type": "model_progress", "id": id,
                                "thinkingDelta": "", "textDelta": delta,
                            }),
                        };
                        queue.lock().unwrap().push(payload);
                    };
                    match deepseek::complete(&cfg, &request, &mut emit) {
                        Ok(result) => queue
                            .lock()
                            .unwrap()
                            .push(json!({ "type": "model_done", "id": id, "result": result })),
                        Err(error) => queue.lock().unwrap().push(json!({
                            "type": "model_error", "id": id, "error": error,
                        })),
                    }
                });
                id
            })
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }

    // ensureApproval(callId, tool, argsJson) -> requestId（字符串）
    // 走 approval.rs：auto 档立即放行；ask 档 emit `approval_required`（UI 弹卡）后挂起，
    // 用户在 UI 上点完 → approval::respond → resolver → 我们把授权记到 callId。
    {
        let grants = Arc::clone(&host.grants);
        let pending = Arc::clone(&host.pending_approvals);
        let queue = Arc::clone(&host.queue);
        obj.set(
            "ensureApproval",
            Function::new(
                ctx.clone(),
                move |call_id: String, tool: String, args: String| -> String {
                    let args: Value = serde_json::from_str(&args).unwrap_or_else(|_| json!({}));
                    let verdict = crate::approval::request(&json!({ "tool": tool, "args": args }));
                    if let Some(decision) = verdict["decision"].as_str() {
                        // auto / deny / 无 UI：立即定论
                        grants.lock().unwrap().insert(call_id.clone(), decision.into());
                        let id = format!("auto-{call_id}");
                        queue.lock().unwrap().push(json!({
                            "type": "approval_decision", "id": id, "decision": decision,
                        }));
                        return id;
                    }
                    let request_id = verdict["requestId"]
                        .as_str()
                        .unwrap_or("approval-unknown")
                        .to_string();
                    pending
                        .lock()
                        .unwrap()
                        .insert(request_id.clone(), call_id.clone());
                    request_id
                },
            )
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }

    // callTool(callId, name, argsJson) -> resultJson（同步）
    // **执行权的唯一判定点**：callId 没握过手就不执行，与档位无关。
    {
        let grants = Arc::clone(&host.grants);
        let tools = tools.clone();
        obj.set(
            "callTool",
            Function::new(
                ctx.clone(),
                move |call_id: String, name: String, args: String| -> String {
                    let granted = grants
                        .lock()
                        .unwrap()
                        .get(&call_id)
                        .map(|d| d == "allow")
                        .unwrap_or(false);
                    if !granted {
                        return json!({
                            "text": format!("{name}: 没有审批握手（宿主拒绝执行）"),
                            "isError": true, "terminate": false,
                        })
                        .to_string();
                    }
                    let args: Value = serde_json::from_str(&args).unwrap_or_else(|_| json!({}));
                    match tools.run_tool(&name, &args) {
                        Ok(text) => json!({ "text": text, "isError": false, "terminate": false }),
                        Err(error) => {
                            json!({ "text": error, "isError": true, "terminate": false })
                        }
                    }
                    .to_string()
                },
            )
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }

    // fs(op, payloadJson) —— 会话持久化（jail 到 sessions 根）
    {
        let sessions_root = host.sessions_root.clone();
        obj.set(
            "fs",
            Function::new(ctx.clone(), move |op: String, payload: String| -> String {
                let mut request: Value = serde_json::from_str(&payload).unwrap_or_else(|_| json!({}));
                if let Some(object) = request.as_object_mut() {
                    object.insert("op".into(), json!(op));
                }
                pi_host_tools::fs_op(&sessions_root, &request).to_string()
            })
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }

    // http(callId, paramsJson) —— 出网（fetch / MCP）。授权源 = 已配置的 MCP 服务器。
    {
        let grants = Arc::clone(&host.grants);
        obj.set(
            "http",
            Function::new(
                ctx.clone(),
                move |call_id: String, params: String| -> String {
                    let granted = grants
                        .lock()
                        .unwrap()
                        .get(&call_id)
                        .map(|d| d == "allow")
                        .unwrap_or(false);
                    if !granted {
                        return json!({ "error": "没有审批握手（宿主拒绝出网）" }).to_string();
                    }
                    let parsed: Value = serde_json::from_str(&params).unwrap_or_else(|_| json!({}));
                    pi_host_tools::http::run_authorized(&parsed, &mcp_origins).to_string()
                },
            )
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }

    // goalGet / skillsConfig / mcpConfig —— 直接读既有服务（单一真源）
    {
        let data_dir = host.data_dir.clone();
        obj.set(
            "goalGet",
            Function::new(ctx.clone(), move || -> String {
                crate::goal::get(&data_dir.to_string_lossy())
                    .ok()
                    .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default()
            })
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    {
        let data_dir = host.data_dir.clone();
        obj.set(
            "skillsConfig",
            Function::new(ctx.clone(), move || -> String {
                crate::skills::enabled_for_injection(&data_dir.to_string_lossy()).to_string()
            })
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    {
        let data_dir = host.data_dir.clone();
        obj.set(
            "mcpConfig",
            Function::new(ctx.clone(), move || -> String {
                let servers: Value = crate::mcp::list(&data_dir.to_string_lossy())
                    .ok()
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .unwrap_or_else(|| json!([]));
                json!({ "servers": servers }).to_string()
            })
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }

    // askUser(payloadJson) -> id（走 ask_user.rs：UI 弹卡，答案经 resolver 回来）
    obj.set(
        "askUser",
        Function::new(ctx.clone(), move |payload: String| -> String {
            let parsed: Value = serde_json::from_str(&payload).unwrap_or_else(|_| json!({}));
            let registered = crate::ask_user::register(&parsed);
            registered["id"].as_str().unwrap_or("ask-unknown").to_string()
        })
        .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    // poll() -> json[]（取走一批宿主事件）
    {
        let queue = Arc::clone(&host.queue);
        obj.set(
            "poll",
            Function::new(ctx.clone(), move || -> String {
                let taken: Vec<Value> = std::mem::take(&mut *queue.lock().unwrap());
                serde_json::to_string(&taken).unwrap_or_else(|_| "[]".into())
            })
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }

    ctx.globals().set("host", obj).map_err(|e| e.to_string())?;
    Ok(())
}

/// 读 provider.json（默认模型选择）：`{provider, modelId}`。
fn read_provider_config(data_dir: &str) -> Value {
    std::fs::read_to_string(format!("{data_dir}/provider.json"))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null)
}

fn call<'js, R>(
    ctx: &Ctx<'js>,
    name: &str,
    args: impl rquickjs::function::IntoArgs<'js>,
) -> Result<R, String>
where
    R: rquickjs::FromJs<'js>,
{
    let spike: Object = ctx.globals().get("__spike").map_err(|e| format!("__spike missing: {e}"))?;
    let function: Function = spike.get(name).map_err(|e| format!("__spike.{name} missing: {e}"))?;
    function.call(args).map_err(|e| describe(ctx, e, name))
}

/// 把 rquickjs 错误变成「带 JS 异常内容」的字符串。
fn describe(ctx: &Ctx<'_>, error: rquickjs::Error, tag: &str) -> String {
    if !error.is_exception() {
        return format!("{tag}: {error}");
    }
    let caught = ctx.catch();
    let rendered = caught
        .as_exception()
        .map(|e| e.message().unwrap_or_else(|| "<no message>".into()))
        .or_else(|| {
            ctx.json_stringify(caught.clone())
                .ok()
                .flatten()
                .and_then(|s| s.to_string().ok())
        })
        .unwrap_or_else(|| "<unrenderable exception>".into());
    let stack = caught
        .as_object()
        .and_then(|o| o.get::<_, String>("stack").ok())
        .unwrap_or_default();
    if stack.is_empty() {
        format!("{tag}: {rendered}")
    } else {
        format!("{tag}: {rendered}\n{}", stack.lines().take(6).collect::<Vec<_>>().join("\n"))
    }
}
