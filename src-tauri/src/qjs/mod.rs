//! qjs —— **QuickJS 版 agent 运行时**（B 路线），与 `pi_bun`（bun/skal，A 路线）并存。
//!
//! 目的：把底层 JS 引擎从 bun 换成 QuickJS，**UI 与命令契约一行不改**。
//! 分工：
//!   · 引擎与 agent 循环：`pi-bundle/dist/agent-qjs.js`（pi-agent-core + 纯 JS 插件）
//!   · 模型传输：本模块的 `deepseek`（Rust —— 换引擎后 provider 必须重写，见那里的注释）
//!   · 工具/会话/审批/设置：**全部复用 src-tauri 既有服务**（进程内直接调用，
//!     不再像 bun 路线那样过 loopback HTTP hostcall）
//!
//! 线程模型（rquickjs 的 Runtime/Context 不是 Send）：guest 独占一个 worker 线程，
//! Tauri 命令经 mpsc 投进去；worker 循环「处理命令 → tick（送事件 + 泵微任务）→
//! 把 agent 事件投给 UI」。**审批/提问的决策来自另一个线程**（UI 的 Tauri 命令），
//! 所以它们只往事件队列里塞事件、绝不直接碰 Context —— 与 M1 学到的教训一致
//! （在 VM 线程上等 I/O 会死锁）。

mod deepseek;
mod guest;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

const TICK_INTERVAL: Duration = Duration::from_millis(2);

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
        option_env!("PI_AGENT_RUNTIME_DEFAULT").unwrap_or("bun").to_string()
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

fn send<T>(
    build: impl FnOnce(Sender<Result<T, String>>) -> Job,
) -> Result<T, String>
where
    T: Send + 'static,
{
    let worker = WORKER.get().ok_or("qjs runtime not started")?;
    let (tx, rx) = mpsc::channel();
    worker.send(build(tx)).map_err(|_| "qjs worker stopped".to_string())?;
    rx.recv_timeout(Duration::from_secs(120))
        .map_err(|_| "qjs worker timeout".to_string())?
}

/// 由 `pi_bun::agent_init` 在选中 qjs 时调用。
pub fn agent_init(data_dir: &str) -> Result<(), String> {
    crate::pi_bun::set_log_dir(data_dir);
    crate::git::init_tls(data_dir);

    let workspace = format!("{data_dir}/workspace");
    let sessions_root = format!("{data_dir}/sessions");
    for dir in [&workspace, &sessions_root] {
        std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {dir}: {e}"))?;
    }

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
                grants
                    .lock()
                    .unwrap()
                    .insert(call_id.clone(), if decision == "allow" { "allow" } else { "deny" }.into());
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
            let cancelled = answer.get("cancelled").and_then(|v| v.as_bool()).unwrap_or(false);
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
    let (boot_tx, boot_rx) = mpsc::channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name("qjs-worker".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let result = worker_main(host, rx, &boot_data_dir);
            let failed = result.as_ref().err().cloned();
            let _ = boot_tx.send(match &failed {
                None => Ok(()),
                Some(error) => Err(error.clone()),
            });
            if let Some(error) = failed {
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
) -> Result<(), String> {
    let mut guest = guest::Guest::start(Arc::clone(&host), data_dir)?;
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
        assert!(kinds.iter().any(|k| k == "agent_start" || k == "message_start"), "没有开跑");
        assert!(kinds.iter().any(|k| k == "message_end"), "没有回合结束");
        assert!(kinds.iter().any(|k| k == "agent_end"), "没有 agent_end");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
