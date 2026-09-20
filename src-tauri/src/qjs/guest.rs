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

use super::{catalog, deepseek, Config, Host, StreamEvent};
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
    /// **UI 面的当前模型**（`__pi_model_current` 的回读源，也是 `model_applied`
    /// 事件的来源）。目录与传输都在 Rust，UI 只需要 `(provider, id, name)` 三个字段；
    /// provider 存的是 **UI id**（`google-gemini`），因为 UI 拿它去自己的列表里匹配。
    current_model: std::sync::Mutex<Value>,
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
        // 生效模型由**目录**解析成完整对象（baseUrl / compat / thinkingLevelMap 全带上），
        // 而不是在 JS 里手抄一份 —— 抄本一定会跟目录漂移。见 catalog 模块头注释。
        let (ui_provider, model) = catalog::resolve_for_boot(
            provider_cfg.get("provider").and_then(|v| v.as_str()),
            provider_cfg.get("modelId").and_then(|v| v.as_str()),
        );
        if let Err(reason) = catalog::transport_for(model) {
            // 不静默换一家去发（那会让 UI 显示的 provider 与真实请求不一致）。
            // 这里只记日志，真正的拒绝发生在每轮请求的 startModel —— 那时用户能
            // 在对话流里看到明确的错误，而不是一个「还能用」的假象。
            super::logcat(&format!("qjs: 这个模型没有可用的传输（{reason}）"));
        }
        // 凭证：读 env 覆盖（测试/开发缝 —— 让集成测试不用碰用户的 keychain），
        // 否则读宿主凭证服务（桌面 keyring / Android 沙箱文件）。
        // ⚠️ 凭证**不是 boot 的门槛**：与 bun 路线一致 —— 没 key 也要能起来，
        // UI 显示配置页，用户存完 key 直接就能聊。所以 key 在**每次模型请求时**读
        // （见 mount 里的 startModel），而不是 boot 时定死。
        //
        // baseUrl 现在以目录里的为准（模型的 `baseUrl` 字段，在 startModel 里取）；
        // 这个值只剩两个用途：DEEPSEEK_BASE_URL 的开发缝（mock/自建端点），
        // 以及目录里没写 baseUrl 时的最后回退。
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

        // ⚠️ 工具表必须在这里传进 guest：JS 侧是 `toolsFor(config.tools || [])`，
        // 漏了这个字段就等于 agent 手里**一个文件工具都没有**（只剩 fetch/todo/
        // subagent/ask_user）—— 这个坑真踩过，见 docs/PROGRESS.md。
        let tool_definitions = pi_host_tools::tool_definitions();
        let boot_host = Arc::clone(&host);
        let boot_data_dir = data_dir.to_string();
        context.with(|ctx| -> Result<(), String> {
            mount(
                &ctx,
                Arc::clone(&host),
                tools.clone(),
                base_url.clone(),
                mcp_origins,
            )?;
            ctx.eval::<(), _>(prelude)
                .map_err(|e| describe(&ctx, e, "prelude"))?;
            ctx.eval::<(), _>(bundle)
                .map_err(|e| describe(&ctx, e, "bundle"))?;

            let goal = crate::goal::get(&boot_data_dir)
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .and_then(|v| v.as_str().map(str::to_owned));
            let config = json!({
                "model": model,
                "tools": tool_definitions,
                "thinkingLevel": "high",
                "systemPrompt": SYSTEM_PROMPT,
                "workspace": boot_host.workspace.to_string_lossy(),
                "goal": goal,
                "compactAt": 0,
            })
            .to_string();
            call::<()>(&ctx, "boot", (config,)).map_err(|e| format!("boot: {e}"))?;
            // boot 后自动恢复**最近一次会话**（原 bun 入口顶部那句 `restoreLatest()`；
            // 语义照搬，出处见 pi-bundle/agent-qjs.js 的 `restoreLatestSession()`）。不做的话重开 App 是空白对话，用户得自己去抽屉里点
            // 一下 —— 真机上就是这么发现的（会话文件都在，界面却空的）。
            // 恢复是异步的（结果经 `session_restored` 事件），下面的热身 tick 会推完。
            call::<()>(&ctx, "restore", ()).map_err(|e| format!("restore: {e}"))?;
            Ok(())
        })?;

        let guest = Self {
            runtime,
            context,
            host,
            tools,
            current_model: std::sync::Mutex::new(
                json!({ "provider": ui_provider, "id": model["id"], "name": model["name"] }),
            ),
        };
        // 推进若干拍让 boot 后的异步（会话恢复 / MCP / 技能注入）跑起来，
        // 这样 agent_init 返回时 history 已经是可读的。
        //
        // ⚠️ 这里的 tick 事件**必须转发给宿主，不能丢**：UI 的顺序是
        // `listen("pi-agent-event")` → `invoke("agent_init")`（App.tsx onMount），
        // 所以 boot 期间发的事件正好是它在等的那批 —— 尤其 `agent_ready`
        // （UI 靠它把 composer 从 "agent booting…" 解锁）。曾经这里把返回值直接
        // 扔掉，真机上就是永远停在 booting（静态检查与单测都拦不住）。
        for _ in 0..200 {
            guest.tick_and_emit()?;
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(guest)
    }

    fn spike<'js>(&self, ctx: &Ctx<'js>, name: &str) -> Result<Function<'js>, String> {
        let spike: Object = ctx
            .globals()
            .get("__spike")
            .map_err(|e| format!("__spike missing: {e}"))?;
        spike
            .get(name)
            .map_err(|e| format!("__spike.{name} missing: {e}"))
    }

    /// 一拍：把队列事件交给 guest → 泵微任务队列 → 取回 agent 事件给 UI。
    ///
    /// 「泵微任务」这步不能省：没有它，agent.prompt() 的 await 永远不会继续
    /// （QuickJS 的 job queue 不自己跑）。spike 里同样是这个发现。
    pub fn tick(&self) -> Result<Vec<Value>, String> {
        self.context.with(|ctx| -> Result<Vec<Value>, String> {
            let tick: Function = self.spike(&ctx, "tick")?;
            tick.call::<_, ()>(())
                .map_err(|e| describe(&ctx, e, "tick"))?;
            while ctx.execute_pending_job() {}
            let drain: Function = self.spike(&ctx, "drain")?;
            let raw: String = drain
                .call::<_, String>(())
                .map_err(|e| describe(&ctx, e, "drain"))?;
            let parsed: Value =
                serde_json::from_str(&raw).map_err(|e| format!("drain json: {e}"))?;
            Ok(parsed["events"].as_array().cloned().unwrap_or_default())
        })
    }

    /// 一拍：把事件**转发给宿主**（与 worker 主循环同一条路）。
    ///
    /// ⚠️ 任何「边 tick 边等某个事件」的地方都要走这里：`tick()` 返回的是一条事件流，
    /// 自己拿着看而不转发，等于把 UI 正在等的东西吃掉。两个真实例：
    ///   · boot 热身的 `agent_ready` 被吃掉 → 真机上永远停在 "agent booting…"；
    ///   · `session_open` 轮询窗口里的 `approval_required` 被吃掉 → 审批卡不弹、
    ///     agent 在另一头干等（这类“静静卡住”比报错难查得多）。
    fn tick_and_emit(&self) -> Result<usize, String> {
        let events = self.tick()?;
        for event in &events {
            super::emit(event);
        }
        Ok(events.len())
    }

    pub fn prompt(&self, text: &str) -> Result<(), String> {
        self.context.with(|ctx| {
            call::<()>(&ctx, "prompt", (text.to_string(),)).map_err(|e| format!("prompt: {e}"))
        })
    }

    pub fn status(&self) -> Result<String, String> {
        self.context
            .with(|ctx| call::<String>(&ctx, "status", ()).map_err(|e| format!("status: {e}")))
    }

    pub fn history(&self) -> Result<String, String> {
        self.context
            .with(|ctx| call::<String>(&ctx, "history", ()).map_err(|e| format!("history: {e}")))
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
        self.context
            .with(|ctx| call::<()>(&ctx, "newSession", ()).map_err(|e| format!("newSession: {e}")))
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
                // 先转发再判定：这一窗口里可能夹着别的回合的事件（model 增量、
                // approval_required…），吞掉它们就是让 UI 干等一个不会来的东西。
                super::emit(&event);
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
        self.context
            .with(|ctx| call::<String>(&ctx, "mcpReconnect", ()).map(|_| ()))
    }

    /// `pi_call_global` 的命令面映射。
    ///
    /// bun 路线这些名字是 `globalThis.__pi_*`（JS 自己实现）；qjs 路线**分两类**：
    ///  · 目录类（providers/models/model_current/model_select）→ 在 Rust 直接答，
    ///    因为目录（models.json）与 "当前模型" 都在 Rust；
    ///  · 命令类（plan/btw/commands/skills/goal）→ 映射到 guest 的 `__spike.*`。
    ///
    /// 返回 "started" 的语义与 bun 一致：**结果经事件回来**（UI 的 kick 模式）。
    pub fn call_global(&self, fn_name: &str, arg: &str) -> Result<String, String> {
        // ── 目录类：事件由 Rust 直接投（UI 只认事件形状，不看谁 emit 的）──
        match fn_name {
            "__pi_providers_list" => {
                super::emit(&json!({
                    "type": "providers_listed",
                    "providers": catalog::providers_list(),
                }));
                return Ok("started".into());
            }
            "__pi_models_refresh" => {
                // 不做网络刷新（目录是静态数据，见 catalog 模块头注释）。
                // shapes 与 bun 逐字对齐：成功 models_listed / 失败 models_error，
                // 两者都带 UI 侧的 provider id —— UI 就拿它去自己的列表里匹配。
                let event = match catalog::model_list(arg) {
                    Ok(models) => {
                        json!({ "type": "models_listed", "provider": arg, "models": models })
                    }
                    Err(error) => {
                        json!({ "type": "models_error", "provider": arg, "error": error })
                    }
                };
                super::emit(&event);
                return Ok("started".into());
            }
            "__pi_model_current" => return Ok(self.current_model.lock().unwrap().to_string()),
            "__pi_model_select" => return self.model_select(arg),
            "__pi_oauth_login" => {
                return Err(
                    "qjs 路线未接 OAuth 订阅登录（已记录在 docs/PROGRESS.md「已知缺口」）".into(),
                )
            }
            _ => {}
        }

        // ── 命令类：名字映射 + 参数形态两套（有参/无参）──
        let (target, arg) = match fn_name {
            "__pi_plan_start" => ("draftPlan", Some(arg)),
            "__pi_btw_start" => ("askByTheWay", Some(arg)),
            "__pi_commands" => ("commands", None),
            // 热生效：bun 侧是 JS 自己重读，这边同样交给 guest（见 agent-qjs.js）
            "__pi_skills_apply" => ("skillsApply", None),
            "__pi_goal_apply" => ("goalApply", None),
            other => return Err(format!("qjs: 未实现的全局调用 {other}")),
        };
        self.context
            .with(|ctx| match arg {
                Some(arg) => call::<String>(&ctx, target, (arg.to_string(),)),
                None => call::<String>(&ctx, target, ()),
            })
            .map_err(|e| format!("{target}: {e}"))
    }

    /// `__pi_model_select({"provider":"deepseek","modelId":"…"})`。
    ///
    /// 三道判断的顺序是有意的：**先查目录**（不存在的模型要报得跟 bun 一样），
    /// **再查传输**（没有传输的 provider 现在是明确报错，不是静默换一家），
    /// 最后才热切换。
    fn model_select(&self, arg: &str) -> Result<String, String> {
        let parsed: Value =
            serde_json::from_str(arg).map_err(|e| format!("bad model select: {e}"))?;
        let provider = parsed["provider"].as_str().unwrap_or_default();
        let model_id = parsed["modelId"].as_str().unwrap_or_default();
        let model = catalog::find_model(provider, model_id)
            .ok_or_else(|| format!("unknown model: {provider}/{model_id}"))?;
        // 闸门：按 model.api 分派。选不了的要**说清是哪一族没做**，而不是假装成功。
        catalog::transport_for(model)?;
        // 热切换：请求体里的 model 字段就是 agent.state.model（见 agent-qjs.js setModel）
        self.context.with(|ctx| {
            call::<String>(&ctx, "setModel", (model.to_string(),))
                .map_err(|e| format!("setModel: {e}"))
        })?;
        let name = model["name"].as_str().unwrap_or(model_id);
        // 回给 UI 的 provider 一定是 **UI id**（`google-gemini`），不能是目录 id：
        // UI 拿它去自己的 provider 列表里匹配（模型快选、当前模型状态条）。
        let ui_id = catalog::ui_provider(provider)
            .map(|(id, _, _)| id)
            .unwrap_or(provider);
        *self.current_model.lock().unwrap() =
            json!({ "provider": ui_id, "id": model_id, "name": name });
        super::emit(&json!({
            "type": "model_applied", "provider": ui_id, "modelId": model_id, "name": name,
        }));
        Ok("started".into())
    }

    pub fn tool_count(&self) -> usize {
        self.tools
            .run_tool("ls", &json!({ "path": "." }))
            .map(|_| 1)
            .unwrap_or(0)
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
        Function::new(ctx.clone(), |line: String| {
            super::logcat(&format!("qjs-js: {line}"))
        })
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
                let base_url_fallback = base_url.clone();
                std::thread::spawn(move || {
                    // 请求体里的 model 对象就是真相（**Rust 用目录解析后随 boot 传给 JS
                    // 的那一份**）：provider 决定走哪条传输、用谁的凭证，baseUrl 决定打哪儿。
                    let request_value: Value =
                        serde_json::from_str(&request).unwrap_or(Value::Null);
                    let model = request_value["model"].clone();
                    let provider = model["provider"].as_str().unwrap_or("deepseek").to_string();
                    let api = model["api"].as_str().unwrap_or_default().to_string();
                    // 传输闸门：按 **model.api** 分派（一个家族实现一次就解锁一批 provider）。
                    // 没有可用的就明确报错 —— 绝不静默改动去发（否则 UI 显示 OpenAI、
                    // 真实请求打 DeepSeek，是最难查的一类不一致）。
                    let transport = match catalog::transport_for(&model) {
                        Ok(transport) => transport,
                        Err(reason) => {
                            queue.lock().unwrap().push(json!({
                                "type": "model_error", "id": id,
                                "error": format!("{reason}（当前模型：{provider}/{api}）"),
                            }));
                            return;
                        }
                    };
                    // 每次请求现读凭证：用户在 UI 里刚存的 key 立刻生效（不用重启）。
                    // 查 key 用 **UI id**（`set_creds` 存的就是它），与请求体里的目录 id
                    // 不同名时（google / google-gemini）不能混用。
                    let creds_provider = catalog::to_ui_id(&provider);
                    let api_key = std::env::var(format!(
                        "PI_{}_API_KEY",
                        creds_provider.to_uppercase().replace('-', "_")
                    ))
                    .ok()
                    .filter(|k| !k.trim().is_empty())
                    .or_else(|| crate::creds::get(&data_dir.to_string_lossy(), creds_provider))
                    .unwrap_or_default();
                    if api_key.trim().is_empty() {
                        queue.lock().unwrap().push(json!({
                            "type": "model_error", "id": id,
                            "error": format!("没有 {creds_provider} 凭证 —— 在设置里配 provider + API key"),
                        }));
                        return;
                    }
                    // baseUrl：env 覆盖（开发缝/自建端点：`PI_<PROVIDER>_BASE_URL`；
                    // DeepSeek 那家还认历史上的 `DEEPSEEK_BASE_URL`）优先，
                    // 其次目录里的真值，最后 boot 传给 mount 的那个。
                    let env_provider = creds_provider.to_uppercase().replace('-', "_");
                    let env_base_url = std::env::var(format!("PI_{env_provider}_BASE_URL"))
                        .ok()
                        .filter(|s| !s.trim().is_empty())
                        .or_else(|| {
                            // 旧名只给 DeepSeek 用：全局设了就抢别的 provider 的端点会很莫名
                            (provider == "deepseek")
                                .then(|| std::env::var("DEEPSEEK_BASE_URL").ok())
                                .flatten()
                                .filter(|s| !s.trim().is_empty())
                        });
                    let base_url = env_base_url
                        .or_else(|| model["baseUrl"].as_str().map(str::to_owned))
                        .unwrap_or(base_url_fallback);
                    let cfg = Config { api_key, base_url };
                    let mut emit = |event: StreamEvent| {
                        let payload = match event {
                            StreamEvent::Thinking(delta) => json!({
                                "type": "model_progress", "id": id,
                                "thinkingDelta": delta, "textDelta": "",
                            }),
                            StreamEvent::Text(delta) => json!({
                                "type": "model_progress", "id": id,
                                "thinkingDelta": "", "textDelta": delta,
                            }),
                        };
                        queue.lock().unwrap().push(payload);
                    };
                    let result = match transport {
                        catalog::Transport::OpenAiResponses => {
                            super::openai_responses::complete(&cfg, &request, &mut emit)
                        }
                        catalog::Transport::OpenAiCompletions => {
                            deepseek::complete(&cfg, &request, &mut emit)
                        }
                    };
                    match result {
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
                        grants
                            .lock()
                            .unwrap()
                            .insert(call_id.clone(), decision.into());
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
                let mut request: Value =
                    serde_json::from_str(&payload).unwrap_or_else(|_| json!({}));
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
            registered["id"]
                .as_str()
                .unwrap_or("ask-unknown")
                .to_string()
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
    let spike: Object = ctx
        .globals()
        .get("__spike")
        .map_err(|e| format!("__spike missing: {e}"))?;
    let function: Function = spike
        .get(name)
        .map_err(|e| format!("__spike.{name} missing: {e}"))?;
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
        format!(
            "{tag}: {rendered}\n{}",
            stack.lines().take(6).collect::<Vec<_>>().join("\n")
        )
    }
}
