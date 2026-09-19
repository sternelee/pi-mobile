//! QuickJS 宿主 —— 「薄 JS + 厚原生」路线的 Rust 半边。
//!
//! 职责（对照 pocket-pi 的 `crates/pocket-pi-embedded/src/lib.rs`）：
//!   · 起一个 QuickJS guest（rquickjs），注入 `globalThis.host` 四个函数；
//!   · 模型请求：开线程跑 HTTP+SSE，增量投进队列，guest 靠 `poll()` 取；
//!   · 工具调用：**同步**在 guest 线程上跑 `pi-host-tools`（本地 fs，微秒级）；
//!   · 循环由宿主驱动：`tick()` 送事件 + 泵微任务队列，guest 自己不等时钟。
//!
//! 线程模型：`Runtime`/`Context` 非 `Send`，整个 guest 只在调用线程上碰；
//! 模型线程与 guest 之间只共享一个 `Mutex<Vec<Value>>` 事件队列。

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rquickjs::{Context, Ctx, Function, Object, Runtime};
use serde_json::{json, Value};

use crate::approval::Approvals;
use crate::ask_user::AskUser;
use crate::deepseek::{self, DeepSeekConfig, StreamEvent};
use pi_host_tools::HostTools;

/// 宿主与 guest 之间共享的状态。
pub struct Sink {
    pub cfg: DeepSeekConfig,
    queue: Mutex<Vec<Value>>,
    counter: AtomicI64,
    /// 当前这一轮开始的时刻（主循环在 prompt 前设置）。
    turn_start: Mutex<Option<Instant>>,
    /// prompt → 第一个 token 的耗时（只记第一次）。
    first_delta: Mutex<Option<Duration>>,
    /// 每个模型请求的耗时。
    pub model_spans: Mutex<Vec<(i64, Duration)>>,
    /// 每次工具调用的耗时。
    pub tool_spans: Mutex<Vec<(String, Duration)>>,
    /// 被审批**拒绝**（或没握手）的调用 —— 拒绝路径也要可见。
    pub denied_calls: Mutex<Vec<String>>,
    /// 累计 token 用量（prompt / completion）。
    pub tokens: Mutex<(u64, u64)>,
    /// 是否有审批在等（决定 tick 循环要不要计「等待期间跑了几拍」）。
    approval_pending: AtomicBool,
    /// 等待审批期间 guest 又跑了几拍 —— 证明没阻塞 VM 线程。
    ticks_while_waiting: Mutex<u64>,
}

impl Sink {
    pub fn new(cfg: DeepSeekConfig) -> Self {
        Self {
            cfg,
            queue: Mutex::new(Vec::new()),
            counter: AtomicI64::new(0),
            turn_start: Mutex::new(None),
            first_delta: Mutex::new(None),
            model_spans: Mutex::new(Vec::new()),
            tool_spans: Mutex::new(Vec::new()),
            denied_calls: Mutex::new(Vec::new()),
            tokens: Mutex::new((0, 0)),
            approval_pending: AtomicBool::new(false),
            ticks_while_waiting: Mutex::new(0),
        }
    }

    /// 是否有审批在等（决定 tick 循环要不要计「等待期间跑了几拍」）。
    pub fn set_pending_approval(&self, pending: bool) {
        self.approval_pending.store(pending, Ordering::SeqCst);
    }

    pub fn pending_approval(&self) -> bool {
        self.approval_pending.load(Ordering::SeqCst)
    }

    /// 审批等待期间由 tick 循环累加 —— 它不为 0 就说明 VM 线程没被 stdin 卡住。
    pub fn bump_tick_while_waiting(&self) {
        *self.ticks_while_waiting.lock().unwrap() += 1;
    }

    pub fn ticks_while_waiting(&self) -> u64 {
        *self.ticks_while_waiting.lock().unwrap()
    }

    fn mark_turn_start(&self) {
        *self.turn_start.lock().unwrap() = Some(Instant::now());
        *self.first_delta.lock().unwrap() = None;
    }

    pub fn first_delta(&self) -> Option<Duration> {
        *self.first_delta.lock().unwrap()
    }

    fn note_delta(&self) {
        let mut slot = self.first_delta.lock().unwrap();
        if slot.is_none() {
            if let Some(start) = *self.turn_start.lock().unwrap() {
                *slot = Some(start.elapsed());
            }
        }
    }

    pub fn push(&self, event: Value) {
        self.queue.lock().unwrap().push(event);
    }

    /// 取走一批事件，并把同一请求的连续增量合并成一条
    /// （等价于 pocket-pi 的 `coalesce_host_events`，也等价于 PLAN D5 的 16ms 合并）。
    pub fn drain(&self) -> Vec<Value> {
        let taken: Vec<Value> = std::mem::take(&mut *self.queue.lock().unwrap());
        let mut batch: Vec<Value> = Vec::new();
        let mut progress: Vec<(i64, usize)> = Vec::new(); // (id, index in batch)
        for event in taken {
            if event["type"] != "model_progress" {
                batch.push(event);
                continue;
            }
            let id = event["id"].as_i64().unwrap_or_default();
            match progress.iter().find(|(pid, _)| *pid == id) {
                Some((_, index)) => {
                    let slot = &mut batch[*index];
                    let thinking = format!(
                        "{}{}",
                        slot["thinkingDelta"].as_str().unwrap_or(""),
                        event["thinkingDelta"].as_str().unwrap_or("")
                    );
                    let text = format!(
                        "{}{}",
                        slot["textDelta"].as_str().unwrap_or(""),
                        event["textDelta"].as_str().unwrap_or("")
                    );
                    slot["thinkingDelta"] = json!(thinking);
                    slot["textDelta"] = json!(text);
                }
                None => {
                    progress.push((id, batch.len()));
                    batch.push(event);
                }
            }
        }
        batch
    }
}

/// 编译进二进制的 bundle 大小。
///
/// 注意：bundle 是 `include_str!` 进来的，**运行期不需要 dist/agent.js 存在** ——
/// 早先 main.rs 为了打印体积去 `fs::metadata("spikes/quickjs-agent/dist/agent.js")`，
/// 在 Android 上直接失败（真机没有仓库相对路径）。这个函数取代那次文件读取。
pub fn bundle_bytes() -> usize {
    include_str!("../dist/agent.js").len()
}

/// 引擎层自检：不挂 host、不发请求，只回答「QuickJS 起得来吗 / bundle 能 eval 吗 /
/// `__spike` 的导出齐不齐」。真机上把「引擎」与「网络」分开判断用（见 netcheck.rs）。
pub fn engine_selftest() -> Result<String, String> {
    let runtime = Runtime::new().map_err(|e| format!("quickjs runtime: {e}"))?;
    runtime.set_memory_limit(256 * 1024 * 1024);
    let context = Context::full(&runtime).map_err(|e| format!("quickjs context: {e}"))?;
    let prelude = include_str!("../js/prelude.js");
    let bundle = include_str!("../dist/agent.js");

    context
        .with(|ctx| -> Result<f64, String> {
            ctx.eval::<(), _>(prelude)
                .map_err(|e| describe(&ctx, e, "prelude"))?;
            let started = Instant::now();
            ctx.eval::<(), _>(bundle)
                .map_err(|e| describe(&ctx, e, "bundle"))?;
            let eval_ms = started.elapsed().as_secs_f64() * 1000.0;
            let probe: String = ctx
                .eval(
                    "['boot','prompt','tick','drain','restore','sessionInfo','status','toolNames','history']\
                 .map((k) => typeof __spike[k]).join(',')",
                )
                .map_err(|e| describe(&ctx, e, "probe"))?;
            if probe != "function,function,function,function,function,function,function,function,function" {
                return Err(format!("__spike 导出不全: {probe}"));
            }
            Ok(eval_ms)
        })
        .map(|eval_ms| {
            // ⚠️ memory_usage() 要在 context.with **之外**取：with 期间 runtime 的
            // RefCell 已被借出，里面再借会 panic（RefCell already borrowed，实测）。
            let usage = runtime.memory_usage();
            format!(
                "QuickJS ok；bundle {} KB eval {:.0} ms；堆 {:.2} MB；__spike 9 个导出齐全",
                bundle.len() / 1024,
                eval_ms,
                usage.memory_used_size as f64 / 1_048_576.0
            )
        })
}

/// 宿主依赖（guest 能碰到的一切外部资源）。收成结构体而不是七个参数 ——
/// 以后加一个通道不必再动所有调用点。
pub struct HostDeps {
    pub tools: HostTools,
    pub sink: Arc<Sink>,
    pub approvals: Arc<Approvals>,
    pub asks: Arc<AskUser>,
    pub sessions_root: std::path::PathBuf,
    pub goal_path: std::path::PathBuf,
    pub mcp_config_path: std::path::PathBuf,
    /// MCP 服务器的授权源（`scheme://host:port`）。**由宿主从自己的配置读出来**，
    /// 不是 payload 字段 —— 否则 JS 自己给自己授权，SSRF 防护等于没有。
    pub allowed_origins: Vec<String>,
    /// skills 注册表所在的数据目录（注入半要读它）。
    pub data_dir: std::path::PathBuf,
}

/// guest 启动配置（与宿主依赖分开：这些是要交给 JS 的字符串/数值）。
pub struct GuestOptions {
    pub model_label: String,
    pub system_prompt: String,
    pub thinking_level: String,
    pub workspace_label: String,
    pub compact_at: u64,
}

pub struct Guest {
    runtime: Runtime,
    context: Context,
    pub sink: Arc<Sink>,
    tools: HostTools,
}

impl Guest {
    /// 建 guest：注入 host 面 → 跑 prelude → 跑 bundle → boot 配置。
    pub fn start(deps: HostDeps, options: GuestOptions) -> Result<Self, String> {
        let sink = Arc::clone(&deps.sink);
        let runtime = Runtime::new().map_err(|e| format!("quickjs runtime: {e}"))?;
        // 显式给个上限：QuickJS 默认堆很小（512KB 级），bundle 解析会直接 OOM。
        runtime.set_memory_limit(256 * 1024 * 1024);
        let context = Context::full(&runtime).map_err(|e| format!("quickjs context: {e}"))?;

        let prelude = include_str!("../js/prelude.js");
        let bundle = include_str!("../dist/agent.js");

        context.with(|ctx| -> Result<(), String> {
            mount_host(&ctx, &deps)?;
            ctx.eval::<(), _>(prelude)
                .map_err(|e| describe(&ctx, e, "prelude"))?;
            ctx.eval::<(), _>(bundle)
                .map_err(|e| describe(&ctx, e, "bundle"))?;

            let goal = std::fs::read_to_string(&deps.goal_path)
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .and_then(|value| value["objective"].as_str().map(str::to_owned));
            let config = json!({
                "model": options.model_label,
                "thinkingLevel": options.thinking_level,
                "systemPrompt": options.system_prompt,
                "workspace": options.workspace_label,
                "goal": goal,
                "compactAt": options.compact_at,
                "tools": tool_definitions(),
            })
            .to_string();
            call::<()>(&ctx, "boot", (config,)).map_err(|e| format!("boot: {e}"))?;
            Ok(())
        })?;

        Ok(Self {
            runtime,
            context,
            sink,
            tools: deps.tools,
        })
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

    pub fn prompt(&self, text: &str) -> Result<(), String> {
        self.sink.mark_turn_start();
        self.context.with(|ctx| {
            call::<()>(&ctx, "prompt", (text.to_string(),)).map_err(|e| format!("prompt: {e}"))
        })
    }

    /// 一拍：送事件进 guest → 泵微任务队列 → 取回 agent 事件。
    pub fn tick(&self) -> Result<Vec<Value>, String> {
        // 有审批在等时记一拍：这个计数不为 0，就证明 stdin 没把 VM 线程堵住。
        if self.sink.pending_approval() {
            self.sink.bump_tick_while_waiting();
        }
        self.context.with(|ctx| -> Result<Vec<Value>, String> {
            let tick: Function = self.spike(&ctx, "tick")?;
            tick.call::<_, ()>(())
                .map_err(|e| describe(&ctx, e, "tick"))?;
            // 没有这一步，agent.prompt() 的 await 永远不会继续（QuickJS 的 job queue
            // 不会自己跑）—— 相当于 pocket-pi 的 guest.drain_jobs()。
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

    /// `--resume`：让 guest 从最新会话恢复（消息灌回 agent + todo 状态重建）。
    pub fn restore(&self) -> Result<(), String> {
        self.context
            .with(|ctx| call::<()>(&ctx, "restore", ()).map_err(|e| format!("restore: {e}")))
    }

    /// 会话信息（id / 消息数 / todo 数 / goal）—— 收尾时打印，便于下一轮 resume。
    pub fn session_info(&self) -> Result<Value, String> {
        self.context.with(|ctx| {
            let raw: String =
                call(&ctx, "sessionInfo", ()).map_err(|e| format!("sessionInfo: {e}"))?;
            serde_json::from_str(&raw).map_err(|e| format!("sessionInfo json: {e}"))
        })
    }

    /// 宿主控制面（对齐 App 的 __pi_status / __pi_tool_names）。
    pub fn status(&self) -> Result<Value, String> {
        self.context.with(|ctx| {
            let raw: String = call(&ctx, "status", ()).map_err(|e| format!("status: {e}"))?;
            serde_json::from_str(&raw).map_err(|e| format!("status json: {e}"))
        })
    }

    pub fn tool_names(&self) -> Result<Vec<String>, String> {
        self.context.with(|ctx| {
            let raw: String = call(&ctx, "toolNames", ()).map_err(|e| format!("toolNames: {e}"))?;
            serde_json::from_str(&raw).map_err(|e| format!("toolNames json: {e}"))
        })
    }

    /// guest 堆用量（QuickJS 自己记账，不含 Rust/宿主内存）。
    /// 返回 (JS 侧在用字节, malloc 总量字节)。
    pub fn memory_usage(&self) -> (usize, usize) {
        let usage = self.runtime.memory_usage();
        (usage.memory_used_size as usize, usage.malloc_size as usize)
    }

    /// 工具定义也要让 spike 的宿主能自己校验（与 JS 侧同源，避免两边漂移）。
    #[allow(dead_code)]
    pub fn tool_count(&self) -> usize {
        tool_definitions().len()
    }

    pub fn tools(&self) -> &HostTools {
        &self.tools
    }
}

/// 注入 `globalThis.host`：guest 能看到的**全部**宿主能力。
fn mount_host(ctx: &Ctx<'_>, deps: &HostDeps) -> Result<(), String> {
    let sink = Arc::clone(&deps.sink);
    let tools = deps.tools.clone();
    let approvals = Arc::clone(&deps.approvals);
    let asks = Arc::clone(&deps.asks);
    let sessions_root = deps.sessions_root.clone();
    let goal_path = deps.goal_path.clone();
    let mcp_config_path = deps.mcp_config_path.clone();
    let allowed_origins = deps.allowed_origins.clone();
    let data_dir_for_skills = deps.data_dir.clone();
    let host = Object::new(ctx.clone()).map_err(|e| format!("host object: {e}"))?;

    // log(line)
    host.set(
        "log",
        Function::new(ctx.clone(), move |line: String| {
            println!("[guest] {line}");
        })
        .map_err(|e| format!("host.log: {e}"))?,
    )
    .map_err(|e| format!("host.log: {e}"))?;

    // startModel(requestJson) -> id（异步：结果经 poll 回来）
    {
        let sink = sink.clone();
        let counter = &sink.counter;
        let _ = counter;
        host.set(
            "startModel",
            Function::new(ctx.clone(), move |request: String| -> i64 {
                let id = sink.counter.fetch_add(1, Ordering::Relaxed) + 1;
                let sink = sink.clone();
                let started = Instant::now();
                std::thread::spawn(move || {
                    let mut emit = |event: StreamEvent| {
                        sink.note_delta();
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
                        sink.push(payload);
                    };
                    let outcome = deepseek::complete(&sink.cfg, &request, &mut emit);
                    sink.model_spans
                        .lock()
                        .unwrap()
                        .push((id, started.elapsed()));
                    match outcome {
                        Ok(result) => {
                            if let Ok(parsed) = serde_json::from_str::<Value>(&result) {
                                let usage = &parsed["usage"];
                                let mut totals = sink.tokens.lock().unwrap();
                                totals.0 += usage["input"].as_u64().unwrap_or(0)
                                    + usage["cacheRead"].as_u64().unwrap_or(0);
                                totals.1 += usage["output"].as_u64().unwrap_or(0);
                            }
                            sink.push(json!({ "type": "model_done", "id": id, "result": result }));
                        }
                        Err(error) => {
                            sink.push(json!({ "type": "model_error", "id": id, "error": error }));
                        }
                    }
                });
                id
            })
            .map_err(|e| format!("host.startModel: {e}"))?,
        )
        .map_err(|e| format!("host.startModel: {e}"))?;
    }

    // callTool(name, argsJson) -> resultJson（同步）
    {
        let tools = tools.clone();
        let sink = sink.clone();
        let approvals = Arc::clone(&approvals);
        host.set(
            "callTool",
            Function::new(
                ctx.clone(),
                move |call_id: String, name: String, args: String| -> String {
                    // 执行权的**唯一**判定点（见 approval.rs 模块头 ①）：
                    // 没握过手就不执行，与档位无关。
                    if let Err(denied) = approvals.ensure_granted(&call_id, &name) {
                        // 被拒的调用也要记账，否则「拒绝路径」在指标里是隐形的。
                        sink.denied_calls.lock().unwrap().push(name.clone());
                        return json!({ "text": denied, "isError": true, "terminate": false })
                            .to_string();
                    }
                    let started = Instant::now();
                    let parsed: Value = serde_json::from_str(&args).unwrap_or_else(|_| json!({}));
                    let result = match tools.run_tool(&name, &parsed) {
                        Ok(text) => json!({ "text": text, "isError": false, "terminate": false }),
                        Err(error) => json!({ "text": error, "isError": true, "terminate": false }),
                    };
                    sink.tool_spans
                        .lock()
                        .unwrap()
                        .push((name, started.elapsed()));
                    result.to_string()
                },
            )
            .map_err(|e| format!("host.callTool: {e}"))?,
        )
        .map_err(|e| format!("host.callTool: {e}"))?;
    }

    // ensureApproval(callId, name, argsJson) -> approvalId（异步：决策经 poll 回来）
    //
    // JS 在**每次**工具调用前调它（包括只读工具）—— 策略不在 JS 侧，Rust 按档位
    // 决定是立刻放行还是问用户。返回值只用于对上号，执行权记在 callId 上。
    {
        let approvals = Arc::clone(&approvals);
        host.set(
            "ensureApproval",
            Function::new(
                ctx.clone(),
                move |call_id: String, name: String, args: String| -> i64 {
                    let parsed: Value = serde_json::from_str(&args).unwrap_or_else(|_| json!({}));
                    approvals.request(&call_id, &name, &parsed)
                },
            )
            .map_err(|e| format!("host.ensureApproval: {e}"))?,
        )
        .map_err(|e| format!("host.ensureApproval: {e}"))?;
    }

    // http(callId, paramsJson) -> 出网请求（SSRF 防护 + HTML→文本，实现在
    // pi-host-tools::http）。**要求该 callId 先完成审批握手** —— 与 host.callTool 同一条
    // 规矩：JS 侧实现的工具（fetch / MCP）自己动手发请求，所以执行权要卡在「拿到网络」
    // 这一步，否则改写 JS 就能绕过审批直接出网。
    {
        let approvals = Arc::clone(&approvals);
        host.set(
            "http",
            Function::new(
                ctx.clone(),
                move |call_id: String, params: String| -> String {
                    if let Err(denied) = approvals.ensure_granted(&call_id, "http") {
                        return json!({ "error": denied }).to_string();
                    }
                    let parsed: Value = serde_json::from_str(&params).unwrap_or_else(|_| json!({}));
                    pi_host_tools::http::run_authorized(&parsed, &allowed_origins).to_string()
                },
            )
            .map_err(|e| format!("host.http: {e}"))?,
        )
        .map_err(|e| format!("host.http: {e}"))?;
    }

    // skillsConfig() -> 启用中的技能（复用 App 的注入半，见 pi-host-tools::skills）
    {
        let data_dir = data_dir_for_skills.clone();
        host.set(
            "skillsConfig",
            Function::new(ctx.clone(), move || -> String {
                pi_host_tools::skills::enabled_for_injection(&data_dir.to_string_lossy())
                    .to_string()
            })
            .map_err(|e| format!("host.skillsConfig: {e}"))?,
        )
        .map_err(|e| format!("host.skillsConfig: {e}"))?;
    }

    // mcpConfig() -> 已配置的 MCP 服务器（宿主读文件，与 App 的 mcp_config 同分工）
    {
        let config_path = mcp_config_path.clone();
        host.set(
            "mcpConfig",
            Function::new(ctx.clone(), move || -> String {
                std::fs::read_to_string(&config_path)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                    .unwrap_or_else(|| json!({ "servers": [] }))
                    .to_string()
            })
            .map_err(|e| format!("host.mcpConfig: {e}"))?,
        )
        .map_err(|e| format!("host.mcpConfig: {e}"))?;
    }

    // fs(op, payloadJson) -> pi 的 Result 形状（会话持久化用，jail 到 sessions 根）
    {
        let sessions_root = sessions_root.clone();
        host.set(
            "fs",
            Function::new(ctx.clone(), move |op: String, payload: String| -> String {
                let mut request: Value =
                    serde_json::from_str(&payload).unwrap_or_else(|_| json!({}));
                if let Some(object) = request.as_object_mut() {
                    object.insert("op".into(), json!(op));
                }
                let response = pi_host_tools::fs_op(&sessions_root, &request);
                // 宿主侧的 fs 失败必须可见 —— 否则 JS 只看到一个 FileError，
                // 排查时不知道是哪一步、哪个路径（第一版会话没落盘就是这么瞎着的）。
                if response["ok"] != json!(true) {
                    eprintln!(
                        "[fs] {op} {} failed: {}",
                        request["path"].as_str().unwrap_or("?"),
                        response["error"]["message"].as_str().unwrap_or("?")
                    );
                }
                response.to_string()
            })
            .map_err(|e| format!("host.fs: {e}"))?,
        )
        .map_err(|e| format!("host.fs: {e}"))?;
    }

    // goalGet() -> 持久目标（goal.json 由宿主持有；JS 只负责拼进 systemPrompt）
    {
        let goal_path = goal_path.clone();
        host.set(
            "goalGet",
            Function::new(ctx.clone(), move || -> String {
                std::fs::read_to_string(&goal_path)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                    .and_then(|value| value["objective"].as_str().map(str::to_owned))
                    .unwrap_or_default()
            })
            .map_err(|e| format!("host.goalGet: {e}"))?,
        )
        .map_err(|e| format!("host.goalGet: {e}"))?;
    }

    // askUser(payloadJson) -> id（异步：答案经 poll 回来，与审批同一模式）
    {
        let asks = Arc::clone(&asks);
        host.set(
            "askUser",
            Function::new(ctx.clone(), move |payload: String| -> i64 {
                let parsed: Value = serde_json::from_str(&payload).unwrap_or_else(|_| json!({}));
                asks.register(&parsed)
            })
            .map_err(|e| format!("host.askUser: {e}"))?,
        )
        .map_err(|e| format!("host.askUser: {e}"))?;
    }

    // poll() -> json[]
    {
        let sink = sink.clone();
        host.set(
            "poll",
            Function::new(ctx.clone(), move || -> String {
                serde_json::to_string(&sink.drain()).unwrap_or_else(|_| "[]".into())
            })
            .map_err(|e| format!("host.poll: {e}"))?,
        )
        .map_err(|e| format!("host.poll: {e}"))?;
    }

    ctx.globals()
        .set("host", host)
        .map_err(|e| format!("set globalThis.host: {e}"))?;
    Ok(())
}

/// 交给 guest 的工具表（名字/描述/参数 schema）——实现在 Rust（`pi-host-tools`）。
/// 与 `pi-bundle/agent-main.js` 里的工具描述保持一致，差异写进 README。
pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "read",
            "label": "Read",
            "description": "Read a UTF-8 text file inside the workspace.",
            "parameters": {
                "type": "object",
                "properties": { "path": { "type": "string", "description": "Path relative to the workspace root." } },
                "required": ["path"], "additionalProperties": false
            }
        }),
        json!({
            "name": "write",
            "label": "Write",
            "description": "Create or overwrite a file inside the workspace.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"], "additionalProperties": false
            }
        }),
        json!({
            "name": "edit",
            "label": "Edit",
            "description": "Replace an exact text fragment in a file. Fails unless oldText occurs exactly once (pass replaceAll to replace every occurrence).",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "oldText": { "type": "string" },
                    "newText": { "type": "string" },
                    "replaceAll": { "type": "boolean" }
                },
                "required": ["path", "oldText", "newText"], "additionalProperties": false
            }
        }),
        json!({
            "name": "ls",
            "label": "List",
            "description": "List a directory inside the workspace.",
            "parameters": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "grep",
            "label": "Grep",
            "description": "Regex search over text files in the workspace.",
            "parameters": {
                "type": "object",
                "properties": { "pattern": { "type": "string" }, "path": { "type": "string" } },
                "required": ["pattern"], "additionalProperties": false
            }
        }),
        json!({
            "name": "mkdir",
            "label": "Make directory",
            "description": "Create a directory (and parents) inside the workspace.",
            "parameters": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"], "additionalProperties": false
            }
        }),
        json!({
            "name": "rm",
            "label": "Remove",
            "description": "Delete a file or directory inside the workspace. Directories need recursive: true, which cannot be undone.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "recursive": { "type": "boolean" }
                },
                "required": ["path"], "additionalProperties": false
            }
        }),
    ]
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

/// 把 rquickjs 错误变成「带 JS 异常内容」的字符串 —— 否则只能看到 `Exception`。
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
