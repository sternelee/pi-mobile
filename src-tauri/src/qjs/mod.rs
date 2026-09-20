//! qjs —— **QuickJS 版 agent 运行时**（B 路线），与 `pi_bun`（bun/skal，A 路线）并存。
//!
//! 目的：把底层 JS 引擎从 bun 换成 QuickJS，**UI 与命令契约一行不改**。
//! 分工：
//!   · 引擎与 agent 循环：`pi-bundle/dist/agent-qjs.js`（pi-agent-core + 纯 JS 插件）
//!   · 模型传输：本模块的 `deepseek`（Rust —— 换引擎后 provider 必须重写，见那里的注释）
//!   · provider/模型目录：本模块的 `catalog`（pi-ai 的 39 家目录当**数据**搬进来，
//!     UI 的列表事件与传输层共用那一份，见那里的注释）
//!   · 工具/会话/审批/设置：**全部复用 src-tauri 既有服务**（进程内直接调用，
//!     不再像 bun 路线那样过 loopback HTTP hostcall）
//!
//! 线程模型（rquickjs 的 Runtime/Context 不是 Send）：guest 独占一个 worker 线程，
//! Tauri 命令经 mpsc 投进去；worker 循环「处理命令 → tick（送事件 + 泵微任务）→
//! 把 agent 事件投给 UI」。**审批/提问的决策来自另一个线程**（UI 的 Tauri 命令），
//! 所以它们只往事件队列里塞事件、绝不直接碰 Context —— 与 M1 学到的教训一致
//! （在 VM 线程上等 I/O 会死锁）。

mod catalog;
mod deepseek;
mod guest;
mod openai_responses;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

const TICK_INTERVAL: Duration = Duration::from_millis(2);

/// 传输层的凭证与端点（**所有 provider 家族共用**：completions / responses / …）。
/// 端点优先取目录里模型的 `baseUrl`（见 guest.rs 的 startModel）。
pub struct Config {
    pub api_key: String,
    pub base_url: String,
}

/// 模型流的增量事件 —— **与 pi-ai 的 `AssistantMessageEventStream` 无关**，
/// 只是「传输出来的东西」：一个思考增量或一个正文增量。两个家族同形。
#[derive(Clone, Debug)]
pub enum StreamEvent {
    Thinking(String),
    Text(String),
}

/// 开关：`PI_AGENT_RUNTIME` 环境变量 或 `{data_dir}/runtime.txt`（内容 `qjs`）。
/// 默认 `bun` —— 迁移期保持默认不变，等 B 路线在真机上过了同样几轮再翻转。
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Runtime {
    Bun,
    QuickJs,
}

/// 由 `pi_bun::agent_init` 在启动时调用一次，决议本次运行用哪个后端。
pub fn resolve_runtime(data_dir: &str) -> Runtime {
    let raw = std::env::var("PI_AGENT_RUNTIME")
        .ok()
        .or_else(|| std::fs::read_to_string(format!("{data_dir}/runtime.txt")).ok())
        .unwrap_or_default();
    // 三级优先级：显式环境变量 / runtime.txt → 编译期默认 → bun。
    // 编译期默认存在的意义：打一个「默认走 QuickJS」的包做真机验证
    // （`PI_AGENT_RUNTIME_DEFAULT=qjs bun tauri android build`），
    // 而不必把仓库默认值也翻过去 —— 迁移期两条路都还要能跑。
    let effective = if raw.trim().is_empty() {
        option_env!("PI_AGENT_RUNTIME_DEFAULT")
            .unwrap_or("bun")
            .to_string()
    } else {
        raw
    };
    let runtime = if effective.trim().eq_ignore_ascii_case("qjs") {
        Runtime::QuickJs
    } else {
        Runtime::Bun
    };
    RUNTIME.set(runtime).ok();
    runtime
}

pub fn is_quickjs() -> bool {
    matches!(RUNTIME.get(), Some(Runtime::QuickJs))
}

/// 事件汇（与 loopback/approval/ask_user 同款：lib.rs 在 setup 里注册）。
static EVENT_SINK: OnceLock<Box<dyn Fn(&str) + Send + Sync>> = OnceLock::new();

pub fn set_event_sink(f: impl Fn(&str) + Send + Sync + 'static) {
    EVENT_SINK.set(Box::new(f)).ok();
}

fn emit(event: &Value) {
    if let Some(sink) = EVENT_SINK.get() {
        sink(&event.to_string());
    }
}

/// 命令（Tauri 线程 → worker 线程）。带回复的用 oneshot 语义的 mpsc。
enum Job {
    Prompt(String, Sender<Result<String, String>>),
    Status(Sender<Result<String, String>>),
    History(Sender<Result<String, String>>),
    Stop(Sender<Result<String, String>>),
    OpenSession(String, Sender<Result<String, String>>),
    NewSession(Sender<Result<String, String>>),
    McpReconnect(Sender<Result<String, String>>),
    CallGlobal(String, String, Sender<Result<String, String>>),
    /// 决策注入（审批 / 提问的回答）：只往队列塞，worker 下一拍送进 guest。
    PushEvent(Value),
}

/// 宿主与 guest 共享的状态。
pub struct Host {
    data_dir: PathBuf,
    workspace: PathBuf,
    sessions_root: PathBuf,
    /// guest 的事件队列（host.poll 取走）—— 也是决策注入的落点。
    queue: Arc<Mutex<Vec<Value>>>,
    /// callId → 授权（**执行权的唯一凭据**，与 spike 同一套语义：
    /// 没握过手就不执行，JS 绕过审批也没用）。
    grants: Arc<Mutex<HashMap<String, String>>>,
    /// approval.rs 的 requestId → callId（UI 决策回来时把授权记到正确的 callId 上）。
    pending_approvals: Arc<Mutex<HashMap<String, String>>>,
}

impl Host {
    fn tool_root(&self) -> PathBuf {
        self.workspace.clone()
    }
}

static HOST: OnceLock<Arc<Host>> = OnceLock::new();
static WORKER: OnceLock<Sender<Job>> = OnceLock::new();

fn send<T>(build: impl FnOnce(Sender<Result<T, String>>) -> Job) -> Result<T, String>
where
    T: Send + 'static,
{
    let worker = WORKER.get().ok_or("qjs runtime not started")?;
    let (tx, rx) = mpsc::channel();
    worker
        .send(build(tx))
        .map_err(|_| "qjs worker stopped".to_string())?;
    rx.recv_timeout(Duration::from_secs(120))
        .map_err(|_| "qjs worker timeout".to_string())?
}

/// 由 `pi_bun::agent_init` 在选中 qjs 时调用。
pub fn agent_init(data_dir: &str) -> Result<(), String> {
    crate::pi_bun::set_log_dir(data_dir);
    crate::git::init_tls(data_dir);

    // ⚠️ 必须先登记宿主路径：`lib.rs` 里那些 **UI 侧命令**（workspace_tree /
    // workspace_read / workspace_revert / preview / git）读的是 `loopback` 里
    // OnceLock 的根目录，而 agent 自己的工具读的是下面 `Host.workspace`。两者都在，
    // 只登记其中一套时的症状很迷惑：**agent 读写正常、UI 报
    // 「workspace_tree failed: workspace not configured」**（真机上就是这么碰到的）。
    // 所以建目录与登记都走 `pi_bun::configure_host_paths`（bun 路线也调同一个）。
    crate::pi_bun::configure_host_paths(data_dir)?;
    let workspace = format!("{data_dir}/workspace");
    let sessions_root = format!("{data_dir}/sessions");

    // 复用既有服务：workspace 工具 + 审批 + 提问 + 会话 fs
    crate::approval::configure(data_dir);
    let queue: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let grants: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
    let pending: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));

    // UI 的决策回来时：把授权记到对应 callId，并把事件推进 guest 队列
    {
        let grants = Arc::clone(&grants);
        let queue = Arc::clone(&queue);
        let pending = Arc::clone(&pending);
        crate::approval::set_resolver(move |request_id, decision| {
            let call_id = pending.lock().unwrap().remove(request_id);
            if let Some(call_id) = call_id {
                grants.lock().unwrap().insert(
                    call_id.clone(),
                    if decision == "allow" { "allow" } else { "deny" }.into(),
                );
                queue.lock().unwrap().push(json!({
                    "type": "approval_decision", "id": request_id, "decision": decision,
                }));
            }
        });
    }
    {
        let queue = Arc::clone(&queue);
        crate::ask_user::set_resolver(move |request_id, answer_json| {
            let answer: Value = serde_json::from_str(answer_json).unwrap_or(Value::Null);
            let cancelled = answer
                .get("cancelled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            queue.lock().unwrap().push(json!({
                "type": "ask_user_decision", "id": request_id,
                "answer": answer.get("text").cloned().unwrap_or(answer.clone()),
                "cancelled": cancelled,
            }));
        });
    }

    let host = Arc::new(Host {
        data_dir: PathBuf::from(data_dir),
        workspace: PathBuf::from(&workspace),
        sessions_root: PathBuf::from(&sessions_root),
        queue: Arc::clone(&queue),
        grants,
        pending_approvals: pending,
    });
    HOST.set(Arc::clone(&host)).ok();

    let (tx, rx) = mpsc::channel::<Job>();
    WORKER.set(tx).ok();

    let boot_data_dir = data_dir.to_string();
    // boot 结果如实回报：先前是「等 BOOTED 标志 + 超时」，真实错误会被
    // 「agent_init 超时」盖住（真机上看到的就是这个，白查一轮）。
    //
    // ⚠️ 信号必须由 `worker_main` 在 **guest 起来的那一刻**发出，**不能**放在它返回
    // 之后：worker 主循环是常驻的（正常路径永不返回），放在后面就等于永远只报超时
    // —— 症状是「日志里已经 booted，却要白等 30s 才返回，UI 上一条 BOOT ERROR」，
    // 而 agent 实际上又能用（worker 线程活着）。这里踩过一次，所以 qjs 的启动路径
    // 必须有测试看着（见 `qjs_globals_offline`）。
    let (boot_tx, boot_rx) = mpsc::channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name("qjs-worker".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            if let Err(error) = worker_main(host, rx, &boot_data_dir, boot_tx) {
                // 走到这里 = **boot 之后** worker 才死（boot 失败已由 worker_main 报过）
                logcat(&format!("qjs worker exited: {error}"));
                emit(&json!({ "type": "boot_error", "error": error }));
            }
        })
        .map_err(|e| format!("spawn qjs worker: {e}"))?;

    match boot_rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(error),
        Err(_) => Err("qjs boot timeout（30s）".into()),
    }
}

fn worker_main(
    host: Arc<Host>,
    rx: mpsc::Receiver<Job>,
    data_dir: &str,
    boot: Sender<Result<(), String>>,
) -> Result<(), String> {
    // boot 的结果在这里就落地（成功/失败都要发）—— 下面的 loop 是常驻的
    let guest = match guest::Guest::start(Arc::clone(&host), data_dir) {
        Ok(guest) => {
            let _ = boot.send(Ok(()));
            guest
        }
        Err(error) => {
            let _ = boot.send(Err(error.clone()));
            return Err(error);
        }
    };
    logcat("qjs: guest booted");
    loop {
        while let Ok(job) = rx.try_recv() {
            match job {
                Job::PushEvent(value) => host.queue.lock().unwrap().push(value),
                Job::Prompt(text, reply) => {
                    let _ = reply.send(guest.prompt(&text).map(|_| "started".to_string()));
                }
                Job::Status(reply) => {
                    let _ = reply.send(guest.status());
                }
                Job::History(reply) => {
                    let _ = reply.send(guest.history());
                }
                Job::Stop(reply) => {
                    let _ = reply.send(guest.stop().map(|_| String::new()));
                }
                Job::OpenSession(id, reply) => {
                    let _ = reply.send(guest.open_session(&id).map(|_| String::new()));
                }
                Job::NewSession(reply) => {
                    let _ = reply.send(guest.new_session().map(|_| String::new()));
                }
                Job::McpReconnect(reply) => {
                    let _ = reply.send(guest.mcp_reconnect().map(|_| String::new()));
                }
                Job::CallGlobal(name, arg, reply) => {
                    let _ = reply.send(guest.call_global(&name, &arg));
                }
            }
        }
        for event in guest.tick()? {
            emit(&event);
        }
        std::thread::sleep(TICK_INTERVAL);
    }
}

// ── 与 pi_bun 同名的命令面（pi_bun 按运行时开关分派到这里）────────────────
pub fn agent_prompt(text: &str) -> Result<String, String> {
    send(|reply| Job::Prompt(text.to_string(), reply))
}

pub fn agent_status() -> Result<String, String> {
    send(Job::Status)
}

pub fn agent_history() -> Result<String, String> {
    send(Job::History)
}

pub fn agent_stop() -> Result<(), String> {
    send(Job::Stop).map(|_| ())
}

pub fn session_open(id: &str) -> Result<(), String> {
    send(|reply| Job::OpenSession(id.to_string(), reply)).map(|_| ())
}

pub fn session_new() -> Result<(), String> {
    send(Job::NewSession).map(|_| ())
}

pub fn mcp_reconnect() -> Result<(), String> {
    send(Job::McpReconnect).map(|_| ())
}

/// `pi_call_global`：命令类插件后端（plan/btw/goal_apply/skills_apply…）。
/// QuickJS 侧的命令面是 `__spike.*`，这里做一次名字映射。
pub fn call_string_global(fn_name: &str, arg: &str) -> Result<String, String> {
    send(|reply| Job::CallGlobal(fn_name.to_string(), arg.to_string(), reply))
}

pub(crate) fn logcat(msg: &str) {
    crate::pi_bun::logcat(msg);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// QuickJS 后端的一次真实往返（**要网络 + key**，所以默认忽略）：
    ///   PI_DEEPSEEK_API_KEY=sk-… cargo test -p pi-mobile qjs_live_turn -- --ignored --nocapture
    ///
    /// 它验的是「App 的 qjs 后端」整条链：boot → prompt → 模型流 → 事件投递 →
    /// 会话落盘。工具与审批在真机/UI 上验（这里只跑只读对话，避免误改文件）。
    #[test]
    #[ignore = "needs network + PI_DEEPSEEK_API_KEY"]
    fn qjs_live_turn() {
        let dir = std::env::temp_dir().join(format!("pi-qjs-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data_dir = dir.to_string_lossy().to_string();

        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        set_event_sink(move |raw| {
            let value: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
            let kind = value["type"].as_str().unwrap_or("").to_string();
            sink.lock().unwrap().push(kind);
        });

        agent_init(&data_dir).expect("boot");
        agent_prompt("Reply with exactly: OK").expect("prompt");

        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(120) {
            std::thread::sleep(Duration::from_millis(20));
            if seen.lock().unwrap().iter().any(|k| k == "agent_end") {
                break;
            }
        }
        let kinds = seen.lock().unwrap().clone();
        println!("events: {kinds:?}");
        assert!(
            kinds
                .iter()
                .any(|k| k == "agent_start" || k == "message_start"),
            "没有开跑"
        );
        assert!(kinds.iter().any(|k| k == "message_end"), "没有回合结束");
        assert!(kinds.iter().any(|k| k == "agent_end"), "没有 agent_end");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 本地 mock：收一个 POST（把 body 存下来），回一段 Responses 的 SSE。
    /// 位置在这里而不是某个测试函数里 —— 两个 mock 测试都要用。
    fn spawn_responses_mock(bodies: Arc<Mutex<Vec<Value>>>) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = listener.local_addr().expect("mock addr");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let mut head_end = None;
                while head_end.is_none() {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                    head_end = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4);
                }
                let Some(head_end) = head_end else { continue };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let length: usize = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().ok())?
                    })
                    .unwrap_or(0);
                while buf.len() < head_end + length {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let body = &buf[head_end..(head_end + length).min(buf.len())];
                if let Ok(value) = serde_json::from_slice::<Value>(body) {
                    bodies.lock().unwrap().push(value);
                }
                let sse = responses_mock_sse();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{sse}",
                    sse.len()
                );
                let _ = stream.write_all(reply.as_bytes());
                let _ = stream.flush();
            }
        });
        format!("http://{addr}/v1")
    }

    /// 一段完整的 Responses SSE：推理 item → message item → 终态。
    fn responses_mock_sse() -> String {
        let events = vec![
            json!({ "type": "response.created", "response": { "id": "resp_mock" } }),
            json!({ "type": "response.output_item.added", "output_index": 0,
                    "item": { "type": "reasoning", "id": "rs_mock", "summary": [] } }),
            json!({ "type": "response.reasoning_summary_text.delta", "output_index": 0,
                    "delta": "thinking" }),
            json!({ "type": "response.output_item.done", "output_index": 0,
                    "item": { "type": "reasoning", "id": "rs_mock", "encrypted_content": "enc",
                              "summary": [{ "type": "summary_text", "text": "thinking" }] } }),
            json!({ "type": "response.output_item.added", "output_index": 1,
                    "item": { "type": "message", "id": "msg_mock", "role": "assistant",
                              "content": [] } }),
            json!({ "type": "response.output_text.delta", "output_index": 1, "delta": "mock-ok" }),
            json!({ "type": "response.output_item.done", "output_index": 1,
                    "item": { "type": "message", "id": "msg_mock", "role": "assistant",
                              "status": "completed",
                              "content": [{ "type": "output_text", "text": "mock-ok",
                                            "annotations": [] }] } }),
            json!({ "type": "response.completed", "response": {
                "id": "resp_mock", "status": "completed",
                "output": [{ "type": "reasoning", "id": "rs_mock", "encrypted_content": "enc",
                             "summary": [{ "type": "summary_text", "text": "thinking" }] }],
                "usage": { "input_tokens": 100, "output_tokens": 7, "total_tokens": 107,
                           "input_tokens_details": { "cached_tokens": 0 },
                           "output_tokens_details": { "reasoning_tokens": 3 } } } }),
        ];
        let mut sse = String::new();
        for event in events {
            sse.push_str(&format!(
                "event: {}\ndata: {}\n\n",
                event["type"].as_str().unwrap_or_default(),
                event
            ));
        }
        sse
    }

    /// Responses 家族的**端到端**验证（不需要真 key）：起本地 mock SSE，把 baseUrl
    /// 指过去，走完 App 的整条链（boot → 模型解析 → prompt → 事件到岸）。
    ///
    /// 它钉住三件事，每一条都对应一个真踩过的坑：
    ///  1. 「按 model.api 分派」真的生效 —— 换到 openai/gpt-5（responses 族）后请求体
    ///     必须是 Responses 形状（input/store/reasoning），不是 completions 的
    ///     messages/max_tokens；
    ///  2. **文件工具真的传进了 guest** —— boot config 曾漏掉 `tools`，
    ///     agent 手里一个文件工具都没有（这里会报空数组）；
    ///  3. SSE 解析出的增量与终态真的以 UI 认的事件形状到岸。
    #[test]
    #[ignore = "boots the qjs runtime (once per process)"]
    fn qjs_responses_mock_turn() {
        let dir = std::env::temp_dir().join(format!("pi-qjs-resp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data_dir = dir.to_string_lossy().to_string();

        let bodies: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let base_url = spawn_responses_mock(Arc::clone(&bodies));
        let events: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let sink = Arc::clone(&events);
            set_event_sink(move |raw| {
                if let Ok(value) = serde_json::from_str::<Value>(raw) {
                    sink.lock().unwrap().push(value);
                }
            });
        }
        // provider.json 指向 UI 第一行（openai），模型是 responses 族
        std::fs::write(
            format!("{data_dir}/provider.json"),
            json!({ "provider": "openai", "modelId": "gpt-5" }).to_string(),
        )
        .unwrap();
        std::env::set_var("PI_OPENAI_API_KEY", "sk-mock");
        std::env::set_var("PI_OPENAI_BASE_URL", &base_url);

        agent_init(&data_dir).expect("boot");

        // 传输层不再静默回退 DeepSeek：生效模型就是目录里那一个
        let current: Value =
            serde_json::from_str(&call_string_global("__pi_model_current", "").unwrap()).unwrap();
        assert_eq!(current["provider"], "openai");
        assert_eq!(current["id"], "gpt-5");

        agent_prompt("Say hi").expect("prompt");
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(60) {
            std::thread::sleep(Duration::from_millis(20));
            if events
                .lock()
                .unwrap()
                .iter()
                .any(|e| e["type"] == "agent_end")
            {
                break;
            }
        }

        // ① 请求体是 Responses 形状
        let bodies = bodies.lock().unwrap().clone();
        assert_eq!(bodies.len(), 1, "一轮只该发一次模型请求：{bodies:?}");
        let body = &bodies[0];
        assert_eq!(body["model"], "gpt-5");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert!(
            body.get("messages").is_none(),
            "发成了 Chat Completions：{body}"
        );
        assert_eq!(
            body["reasoning"]["effort"], "high",
            "thinkingLevel 要映射成 effort"
        );
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(
            body["input"][0]["role"], "developer",
            "推理模型用 developer 角色"
        );
        assert!(body["max_output_tokens"].as_u64().is_some_and(|n| n >= 16));
        assert!(body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["role"] == json!("user")));

        // ② 文件工具真的在（漏传 tools 时这里是空数组 —— 真踩过）
        let names: Vec<&str> = body["tools"]
            .as_array()
            .expect("tools 缺失：boot config 没传工具表")
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        for tool in ["read", "write", "edit", "ls", "grep", "mkdir", "rm"] {
            assert!(names.contains(&tool), "缺 {tool}: {names:?}");
        }
        assert_eq!(
            body["tools"][0]["type"], "function",
            "responses 的工具是扁平形状"
        );

        // ③ 模型输出以 UI 认的事件形状到岸
        let seen = events.lock().unwrap().clone();
        let joined = |kind: &str| -> String {
            seen.iter()
                .filter(|e| e["type"] == kind)
                .map(|e| e.to_string())
                .collect()
        };
        assert!(
            joined("message_update").contains("mock-ok"),
            "增量没到岸：{seen:?}"
        );
        assert!(joined("message_end").contains("mock-ok"), "终态消息没到岸");
        assert!(seen.iter().any(|e| e["type"] == "agent_end"));

        // ④ 磁盘 → 内存的回放路径（boot 的自动恢复走的就是它）：把刚写下的会话重新
        //    打开一次，历史里应当还有这一轮的消息，且 `session_opened` 要经事件到岸
        //    （open_session 的轮询曾经把沿途事件吞掉 —— 与 booting 那个坑同一类）。
        let history: Value = serde_json::from_str(&agent_history().unwrap()).unwrap();
        let sid = history["sessionId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(!sid.is_empty(), "这一轮应当落出一个会话：{history}");
        events.lock().unwrap().clear();
        session_open(&sid).expect("open session");
        let opened = events.lock().unwrap().clone();
        assert!(
            opened.iter().any(|e| e["type"] == "session_opened"),
            "session_opened 没到岸：{opened:?}"
        );
        let reopened = agent_history().unwrap();
        assert!(
            reopened.contains("Say hi"),
            "恢复的历史少了用户消息：{reopened}"
        );
        assert!(
            reopened.contains("mock-ok"),
            "恢复的历史少了助手回复：{reopened}"
        );

        std::env::remove_var("PI_OPENAI_API_KEY");
        std::env::remove_var("PI_OPENAI_BASE_URL");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 目录/模型/命令面这些全局的**离线**验证（不联网、不要 key）。
    ///
    /// UI 在 `pi_call_global` 上的每一次调用都在这里走一遍，事件形状逐项核对。
    /// 为什么要专门钉住：真机上 `__pi_models_refresh` 曾是
    /// 「qjs: 未实现的全局调用」——就这一条把「存完 key 拉模型列表」整条路断掉，
    /// 而它只在跑真机时才暴露（没有编译期或单元测试能拦住）。
    ///
    /// ⚠️ 与 `qjs_live_turn` **不能在同一个进程里一起跑**：`agent_init` 的
    /// HOST/WORKER 是 OnceLock（一个进程只 boot 一次）。两个都标了 ignore，
    /// 需要用过滤名单独跑：
    ///   cargo test -p pi-mobile qjs_globals_offline -- --ignored --nocapture
    #[test]
    #[ignore = "boots the qjs runtime (once per process)"]
    fn qjs_globals_offline() {
        let dir = std::env::temp_dir().join(format!("pi-qjs-globals-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data_dir = dir.to_string_lossy().to_string();

        let events: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        set_event_sink(move |raw| {
            if let Ok(value) = serde_json::from_str::<Value>(raw) {
                sink.lock().unwrap().push(value);
            }
        });

        agent_init(&data_dir).expect("boot");

        // 取走某类事件（取过就不再重复计数，方便逐段断言）
        let take = |kind: &str| -> Vec<Value> {
            let mut guard = events.lock().unwrap();
            let hit: Vec<Value> = guard
                .iter()
                .filter(|e| e["type"].as_str() == Some(kind))
                .cloned()
                .collect();
            guard.retain(|e| e["type"].as_str() != Some(kind));
            hit
        };

        // ⓞ **boot 期间的事件必须到宿主**。UI 的顺序是
        //    `listen("pi-agent-event")` → `invoke("agent_init")`（App.tsx onMount），
        //    所以这几条正是它在等的：
        //      · `agent_ready` → 解锁 composer（真机上曾永远停在 "agent booting…"，
        //        因为热身 tick 把事件扔掉了）；
        //      · `session_restored` → 证明 boot 真去恢复了上次会话（对齐 bun 的
        //        restoreLatest()），否则重开 App 是空白对话。
        let booted = take("agent_ready");
        assert_eq!(
            booted.len(),
            1,
            "boot 没把 agent_ready 发出来 → UI 会一直卡在 booting：{booted:?}"
        );
        let restored = take("session_restored");
        assert_eq!(restored.len(), 1, "boot 没触发会话恢复：{restored:?}");
        assert_eq!(restored[0]["found"], 0, "干净的临时目录里不该有会话");

        // ⓞ② UI 侧命令的根目录必须已登记 —— 它们读的是 `loopback` 里 OnceLock 的那份，
        //    与 agent 工具用的 `Host.workspace` 是两套。qjs 路线曾经只喂了后者，症状是
        //    UI 抽屉报 `workspace_tree failed: workspace not configured`（agent 却正常）。
        assert!(
            crate::pi_bun::loopback::workspace_tree().is_ok(),
            "UI 侧 workspace_tree 读不到根目录：loopback 没登记路径"
        );
        assert!(
            crate::pi_bun::loopback::workspace_dir().is_some(),
            "preview / git 靠 workspace_dir()，也得登记"
        );

        // ① providers_listed：8 家、顺序与 UI 的静态清单一致、展示名用 UI 的名字
        assert_eq!(
            call_string_global("__pi_providers_list", "").unwrap(),
            "started"
        );
        let listed = take("providers_listed");
        assert_eq!(listed.len(), 1, "{listed:?}");
        let providers = listed[0]["providers"].as_array().unwrap();
        assert_eq!(providers.len(), 8);
        assert_eq!(providers[0]["id"], "openai");
        assert!(providers
            .iter()
            .all(|p| p["name"].as_str().is_some_and(|n| !n.is_empty())));
        let deepseek = providers.iter().find(|p| p["id"] == "deepseek").unwrap();
        assert_eq!(deepseek["models"].as_array().unwrap().len(), 3);
        // 别名那家必须真拿到模型（bun 路线里这一行一直是空的）
        let google = providers
            .iter()
            .find(|p| p["id"] == "google-gemini")
            .unwrap();
        assert!(!google["models"].as_array().unwrap().is_empty(), "{google}");

        // ② models_listed / models_error —— 真机上报错的就是这一条
        assert_eq!(
            call_string_global("__pi_models_refresh", "google-gemini").unwrap(),
            "started"
        );
        let listed = take("models_listed");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["provider"], "google-gemini", "事件里必须是 UI id");
        assert!(!listed[0]["models"].as_array().unwrap().is_empty());

        assert_eq!(
            call_string_global("__pi_models_refresh", "nope").unwrap(),
            "started"
        );
        let errors = take("models_error");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["provider"], "nope");
        assert!(errors[0]["error"]
            .as_str()
            .unwrap()
            .contains("unknown provider"));

        // ③ model_current：临时目录里没有 provider.json → 回退 deepseek
        let current = || -> Value {
            serde_json::from_str(&call_string_global("__pi_model_current", "").unwrap()).unwrap()
        };
        assert_eq!(current()["provider"], "deepseek");
        assert_eq!(current()["id"], "deepseek-v4-flash");

        // ④ model_select：有传输的热切换成功；没传输的**明确报错**（不静默换一家）
        let arg = json!({ "provider": "deepseek", "modelId": "deepseek-v4-pro" }).to_string();
        assert_eq!(
            call_string_global("__pi_model_select", &arg).unwrap(),
            "started"
        );
        let applied = take("model_applied");
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0]["provider"], "deepseek");
        assert_eq!(applied[0]["modelId"], "deepseek-v4-pro");
        assert_eq!(
            current()["id"],
            "deepseek-v4-pro",
            "热切换后 current 要跟着变"
        );

        // responses 家族这轮之后真能选了（openai 是 UI 第一行）
        let arg = json!({ "provider": "openai", "modelId": "gpt-4" }).to_string();
        assert_eq!(
            call_string_global("__pi_model_select", &arg).unwrap(),
            "started"
        );
        let applied = take("model_applied");
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0]["provider"], "openai");
        assert_eq!(applied[0]["modelId"], "gpt-4");
        assert_eq!(current()["id"], "gpt-4");

        // 没实现的家族：要说清是**哪一族**没做（用户才知道该怎么办）
        let arg = json!({ "provider": "anthropic", "modelId": "claude-fable-5" }).to_string();
        let err = call_string_global("__pi_model_select", &arg).unwrap_err();
        assert!(err.contains("anthropic-messages"), "要说清是哪一族: {err}");
        // 同一族但没做那家的 compat 档：也要能区分（openrouter 是 openai-completions）
        let arg = json!({ "provider": "openrouter", "modelId": "aion-labs/aion-2.0" }).to_string();
        let err = call_string_global("__pi_model_select", &arg).unwrap_err();
        assert!(err.contains("DeepSeek 的 compat"), "{err}");

        let arg = json!({ "provider": "deepseek", "modelId": "nope" }).to_string();
        let err = call_string_global("__pi_model_select", &arg).unwrap_err();
        assert!(
            err.contains("unknown model"),
            "目录里没有的模型：文案与 bun 对齐: {err}"
        );

        // ⑤ 命令面的 kick 语义 + 明确的未接项
        assert_eq!(call_string_global("__pi_commands", "").unwrap(), "[]");
        assert_eq!(
            call_string_global("__pi_skills_apply", "").unwrap(),
            "started"
        );
        assert_eq!(
            call_string_global("__pi_goal_apply", "").unwrap(),
            "started"
        );
        assert!(
            call_string_global("__pi_oauth_login", "anthropic").is_err(),
            "OAuth 未接要明说"
        );

        // ⑥ 真没映射的名字仍要报「未实现」——别再静默吞掉
        let err = call_string_global("__pi_no_such_thing", "").unwrap_err();
        assert!(err.contains("未实现的全局调用"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
