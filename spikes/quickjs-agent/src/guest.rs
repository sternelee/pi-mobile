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

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rquickjs::{Context, Ctx, Function, Object, Runtime};
use serde_json::{json, Value};

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
    /// 累计 token 用量（prompt / completion）。
    pub tokens: Mutex<(u64, u64)>,
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
            tokens: Mutex::new((0, 0)),
        }
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

    fn push(&self, event: Value) {
        self.queue.lock().unwrap().push(event);
    }

    /// 取走一批事件，并把同一请求的连续增量合并成一条
    /// （等价于 pocket-pi 的 `coalesce_host_events`，也等价于 PLAN D5 的 16ms 合并）。
    fn drain(&self) -> Vec<Value> {
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

pub struct Guest {
    runtime: Runtime,
    context: Context,
    pub sink: Arc<Sink>,
    tools: HostTools,
}

impl Guest {
    /// 建 guest：注入 host 面 → 跑 prelude → 跑 bundle → boot 配置。
    pub fn start(
        tools: HostTools,
        sink: Arc<Sink>,
        model_label: &str,
        system_prompt: &str,
        thinking_level: &str,
    ) -> Result<Self, String> {
        let runtime = Runtime::new().map_err(|e| format!("quickjs runtime: {e}"))?;
        // 显式给个上限：QuickJS 默认堆很小（512KB 级），bundle 解析会直接 OOM。
        runtime.set_memory_limit(256 * 1024 * 1024);
        let context = Context::full(&runtime).map_err(|e| format!("quickjs context: {e}"))?;

        let prelude = include_str!("../js/prelude.js");
        let bundle = include_str!("../dist/agent.js");

        context.with(|ctx| -> Result<(), String> {
            mount_host(&ctx, sink.clone(), tools.clone())?;
            ctx.eval::<(), _>(prelude).map_err(|e| describe(&ctx, e, "prelude"))?;
            ctx.eval::<(), _>(bundle).map_err(|e| describe(&ctx, e, "bundle"))?;

            let config = json!({
                "model": model_label,
                "thinkingLevel": thinking_level,
                "systemPrompt": system_prompt,
                "tools": tool_definitions(),
            })
            .to_string();
            call::<()>(&ctx, "boot", (config,)).map_err(|e| format!("boot: {e}"))?;
            Ok(())
        })?;

        Ok(Self { runtime, context, sink, tools })
    }

    fn spike<'js>(&self, ctx: &Ctx<'js>, name: &str) -> Result<Function<'js>, String> {
        let spike: Object = ctx
            .globals()
            .get("__spike")
            .map_err(|e| format!("__spike missing: {e}"))?;
        spike.get(name).map_err(|e| format!("__spike.{name} missing: {e}"))
    }

    pub fn prompt(&self, text: &str) -> Result<(), String> {
        self.sink.mark_turn_start();
        self.context.with(|ctx| {
            call::<()>(&ctx, "prompt", (text.to_string(),)).map_err(|e| format!("prompt: {e}"))
        })
    }

    /// 一拍：送事件进 guest → 泵微任务队列 → 取回 agent 事件。
    pub fn tick(&self) -> Result<Vec<Value>, String> {
        self.context.with(|ctx| -> Result<Vec<Value>, String> {
            let tick: Function = self.spike(&ctx, "tick")?;
            tick.call::<_, ()>(()).map_err(|e| describe(&ctx, e, "tick"))?;
            // 没有这一步，agent.prompt() 的 await 永远不会继续（QuickJS 的 job queue
            // 不会自己跑）—— 相当于 pocket-pi 的 guest.drain_jobs()。
            while ctx.execute_pending_job() {}
            let drain: Function = self.spike(&ctx, "drain")?;
            let raw: String = drain.call::<_, String>(()).map_err(|e| describe(&ctx, e, "drain"))?;
            let parsed: Value = serde_json::from_str(&raw).map_err(|e| format!("drain json: {e}"))?;
            Ok(parsed["events"].as_array().cloned().unwrap_or_default())
        })
    }

    /// guest 堆用量（QuickJS 自己记账，不含 Rust/宿主内存）。
    /// 返回 (JS 侧在用字节, malloc 总量字节)。
    pub fn memory_usage(&self) -> (usize, usize) {
        let usage = self.runtime.memory_usage();
        (
            usage.memory_used_size as usize,
            usage.malloc_size as usize,
        )
    }

    /// 工具定义也要让 spike 的宿主能自己校验（与 JS 侧同源，避免两边漂移）。
    pub fn tool_count(&self) -> usize {
        tool_definitions().len()
    }

    pub fn tools(&self) -> &HostTools {
        &self.tools
    }
}

/// 注入 `globalThis.host`：guest 能看到的**全部**宿主能力。
fn mount_host(ctx: &Ctx<'_>, sink: Arc<Sink>, tools: HostTools) -> Result<(), String> {
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
                    sink.model_spans.lock().unwrap().push((id, started.elapsed()));
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
        host.set(
            "callTool",
            Function::new(ctx.clone(), move |name: String, args: String| -> String {
                let started = Instant::now();
                let parsed: Value = serde_json::from_str(&args).unwrap_or_else(|_| json!({}));
                let result = match tools.run_tool(&name, &parsed) {
                    Ok(text) => json!({ "text": text, "isError": false, "terminate": false }),
                    Err(error) => json!({ "text": error, "isError": true, "terminate": false }),
                };
                sink.tool_spans.lock().unwrap().push((name, started.elapsed()));
                result.to_string()
            })
            .map_err(|e| format!("host.callTool: {e}"))?,
        )
        .map_err(|e| format!("host.callTool: {e}"))?;
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
    ]
}

fn call<'js, R>(ctx: &Ctx<'js>, name: &str, args: impl rquickjs::function::IntoArgs<'js>) -> Result<R, String>
where
    R: rquickjs::FromJs<'js>,
{
    let spike: Object = ctx.globals().get("__spike").map_err(|e| format!("__spike missing: {e}"))?;
    let function: Function = spike.get(name).map_err(|e| format!("__spike.{name} missing: {e}"))?;
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
        format!("{tag}: {rendered}\n{}", stack.lines().take(6).collect::<Vec<_>>().join("\n"))
    }
}
