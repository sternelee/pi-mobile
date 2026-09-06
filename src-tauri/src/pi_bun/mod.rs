//! pi_bun —— libpi-bun PoC 装载模块（M1）
//!
//! 当前阶段：dlopen skal 官方预构建产物（`libskal-android-arm64.so`，skal ABI），
//! 在真机上验证「嵌入式 bun + JSC 运行时」执行 JS 的完整链路。
//! M2 桥（预构建 ABI 阶段）：Rust loopback HTTP（JS→Rust hostcall）
//! + skal_evaluate 事件注入（Rust→JS）。详见 docs/CONTRACTS.md §2。
//! 后续：换成我们自己的 pi_entry.zig + `pi_bun_*` C ABI（见
//! `src-tauri/pi_bun/include/pi_bun.h`），桥协议不变。
//!
//! 参考文档：docs/LIBPI-BUN-NOTES.md、docs/CONTRACTS.md §2、skal.h（skal 仓）。

pub mod loopback;

use std::ffi::{c_char, c_int, CString};
use std::sync::{Mutex, OnceLock};

use libloading::Library;

/// M1 冒烟负载：同步探测嵌入式运行时能力面。
const HELLO_JS: &str = include_str!("../../../pi-bundle/hello.js");
/// M2 桥：JS→Rust hostcall（fetch → loopback）
const BRIDGE_JS: &str = include_str!("../../../pi-bundle/bridge.js");
/// M2 桥冒烟：异步（skal_evaluate 等待 Promise 落定）
const SMOKE2_JS: &str = include_str!("../../../pi-bundle/smoke2.js");

// ── skal C ABI（只用 PoC 需要的 4 个符号）──────────────────────────

type SkalHandle = i64;

#[allow(non_snake_case)]
// iOS：init 被 cfg 门控（静态链路待 M5），ABI 类型别名暂未使用
#[cfg_attr(target_os = "ios", allow(dead_code))]
mod abi {
    use std::ffi::{c_char, c_int};

    pub type CreateRuntime =
        unsafe extern "C" fn(dir: *const c_char, dir_len: usize) -> i64;
    pub type Evaluate = unsafe extern "C" fn(
        handle: i64,
        source: *const c_char,
        source_len: usize,
        url: *const c_char,
        url_len: usize,
        out_result: *mut *mut c_char,
        out_result_len: *mut usize,
        out_is_error: *mut c_int,
    );
    pub type FreeString = unsafe extern "C" fn(s: *mut c_char);
    pub type WasReused = unsafe extern "C" fn() -> c_int;
}

/// 持有 dlopen 的库与符号表。库句柄必须存活于运行时整个生命周期。
struct PiBunRuntime {
    _lib: Library,
    handle: SkalHandle,
    evaluate: abi::Evaluate,
    free_string: abi::FreeString,
}

static RUNTIME: OnceLock<Mutex<Option<PiBunRuntime>>> = OnceLock::new();

/// 进程内单运行时（skal 语义：每进程一个 VM，重复 create 会复用）。
fn runtime_lock() -> &'static Mutex<Option<PiBunRuntime>> {
    RUNTIME.get_or_init(|| Mutex::new(None))
}

/// Android 上把消息打进 logcat（M1 出口条件要求 logcat 可见 bun 执行输出）。
#[cfg(target_os = "android")]
pub(crate) fn logcat(msg: &str) {
    use std::ffi::CString;
    extern "C" {
        fn __android_log_print(prio: i32, tag: *const c_char, text: *const c_char) -> i32;
    }
    const INFO: i32 = 4;
    const ERROR: i32 = 5;
    let tag = CString::new("pibun").unwrap();
    let text = CString::new(msg.replace('\0', " ")).unwrap();
    unsafe {
        let prio = if msg.starts_with("ERROR") { ERROR } else { INFO };
        __android_log_print(prio, tag.as_ptr(), text.as_ptr());
    }
}

#[cfg(not(target_os = "android"))]
pub(crate) fn logcat(msg: &str) {
    println!("[pi-bun] {msg}");
}

/// 初始化（懒加载）：dlopen + create_runtime，失败原因显式返回。
/// 初始化（懒加载）：dlopen + create_runtime，失败原因显式返回。
#[cfg(target_os = "ios")]
fn init(_data_dir: &str) -> Result<(), String> {
    // iOS：libpi-bun 需静态链接（.a + 从源码构建 WebKit JSC，见
    // LIBPI-BUN-NOTES §2 —— skal build-jsc-ios.sh + link-skal-ios.sh 工艺），
    // dlopen 路径不可用。App 其余能力（workspace 工具/会话/MCP 配置/审批）
    // 在 iOS 全量编译可用，agent 运行时待 M5 静态链路落地。
    Err("pi runtime is not yet available on iOS — libpi-bun static link pending".into())
}

#[cfg(not(target_os = "ios"))]
fn init(data_dir: &str) -> Result<(), String> {
    let mut guard = runtime_lock().lock().unwrap();
    if guard.is_some() {
        return Ok(());
    }

    // jniLibs 产物名：libskal.so（见 scripts/fetch-libpi-bun.sh 的安装步骤）
    let lib = unsafe { Library::new("libskal.so") }
        .map_err(|e| format!("dlopen libskal.so failed: {e}"))?;

    // libloading::Symbol 解引用为裸函数指针后即可长期保存（库句柄由 PiBunRuntime 持有）
    let create: abi::CreateRuntime = unsafe {
        *lib.get(b"skal_create_runtime")
            .map_err(|e| format!("symbol skal_create_runtime missing: {e}"))?
    };
    let evaluate: abi::Evaluate = unsafe {
        *lib.get(b"skal_evaluate")
            .map_err(|e| format!("symbol skal_evaluate missing: {e}"))?
    };
    let free_string: abi::FreeString = unsafe {
        *lib.get(b"skal_free_string")
            .map_err(|e| format!("symbol skal_free_string missing: {e}"))?
    };
    let was_reused: abi::WasReused = unsafe {
        *lib.get(b"skal_runtime_was_reused")
            .map_err(|e| format!("symbol skal_runtime_was_reused missing: {e}"))?
    };

    // dir 非 NUL 结尾、显式传长度（skal.h 契约）
    let handle = unsafe { create(data_dir.as_ptr().cast(), data_dir.len()) };
    if handle == 0 {
        return Err("skal_create_runtime returned 0 (VM failed to start)".into());
    }
    let reused = unsafe { was_reused() };
    logcat(&format!(
        "runtime up: handle={handle} reused={reused} bun-pins=1.3.14 data_dir={data_dir}"
    ));

    *guard = Some(PiBunRuntime {
        _lib: lib,
        handle,
        evaluate,
        free_string,
    });
    Ok(())
}

/// 同步求值一段 JS（在调用线程阻塞直至 JS worker 返回——必须 off main thread 调用）。
/// 返回 (result, is_error)。
fn evaluate_blocking(js: &str, url: &str) -> Result<(String, bool), String> {
    let guard = runtime_lock().lock().unwrap();
    let rt = guard.as_ref().ok_or("runtime not initialized")?;

    let js_c = CString::new(js).map_err(|e| format!("js contains NUL: {e}"))?;
    let url_c = CString::new(url).map_err(|e| format!("url contains NUL: {e}"))?;

    let mut out_result: *mut c_char = std::ptr::null_mut();
    let mut out_len: usize = 0;
    let mut out_is_error: c_int = 0;

    unsafe {
        (rt.evaluate)(
            rt.handle,
            js_c.as_ptr(),
            js.len(),
            url_c.as_ptr(),
            url.len(),
            &mut out_result,
            &mut out_len,
            &mut out_is_error,
        );
    }

    if out_result.is_null() {
        return Err("skal_evaluate returned null result".into());
    }
    // 结果非 NUL 结尾、显式长度（skal.h 契约）
    let bytes = unsafe { std::slice::from_raw_parts(out_result.cast::<u8>(), out_len) };
    let text = String::from_utf8_lossy(bytes).into_owned();
    unsafe { (rt.free_string)(out_result) };
    Ok((text, out_is_error != 0))
}

/// M2 agent bundle（bun build 单文件产物，kick 模式加载）。
const AGENT_JS: &str = include_str!("../../../pi-bundle/dist/agent.js");

/// 初始化 agent：配置注入 + bundle 加载（同步 kick，立即返回）。
pub fn agent_init(data_dir: &str) -> Result<(), String> {
    let port = loopback::start()?;
    init(data_dir)?;

    let workspace = format!("{data_dir}/workspace");
    std::fs::create_dir_all(&workspace).map_err(|e| format!("workspace: {e}"))?;
    std::fs::create_dir_all(format!("{data_dir}/sessions"))
        .map_err(|e| format!("sessions: {e}"))?;
    loopback::configure(&workspace, data_dir);
    crate::approval::configure(data_dir);
    crate::ask_user::set_resolver(|id, answer| {
        let id_j = serde_json::to_string(id).unwrap_or_else(|_| "\"\"".into());
        let ans_j = serde_json::to_string(answer).unwrap_or_else(|_| "\"\"".into());
        let _ = evaluate_blocking(
            &format!("globalThis.__pi_ask_resolve({id_j}, {ans_j})"),
            "pi:ask-resolve",
        );
    });

    // 异步引导（dynamic import 等）需要 VM tick 数拍——轮询 __pi_ready。
    // null result 视为瞬时失败可重试（实测出现过）。
    let eval_retry = |js: &str, url: &str| -> Result<(String, bool), String> {
        let mut last = Err("no attempt".into());
        for _ in 0..3 {
            last = evaluate_blocking(js, url);
            match &last {
                Err(e) if e.contains("null result") => {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }
                _ => return last,
            }
        }
        last
    };

    let mut cfg = serde_json::json!({ "port": port, "dataDir": data_dir });
    // 上次保存的默认模型选择：bundle 用 pi-ai 目录解析 (provider, modelId)
    // 为完整模型对象（无选择或目录缺模型时 bundle 落回兜底模型）。
    if let Ok(raw) = std::fs::read_to_string(
        std::path::Path::new(data_dir).join("provider.json"),
    ) {
        if let Ok(sel) = serde_json::from_str::<serde_json::Value>(&raw) {
            if sel.get("provider").and_then(|v| v.as_str()).is_some()
                && sel.get("modelId").and_then(|v| v.as_str()).is_some()
            {
                cfg["providerConfig"] = sel;
            }
        }
    }
    let cfg_json = cfg;
    let (r, err) = eval_retry(
        &format!("globalThis.__PI_CONFIG = {};", cfg_json),
        "pi:agent-config",
    )?;
    if err {
        return Err(format!("agent config eval threw: {r}"));
    }

    let (r, err) = eval_retry(AGENT_JS, "pi-bundle/dist/agent.js")?;
    if err {
        return Err(format!("agent bundle eval threw: {r}"));
    }

    // 轮询 __pi_ready（CJS bundle 完成值是 wrapper 函数，不能用作就绪信号）
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match evaluate_blocking("String(globalThis.__pi_ready === true)", "pi:agent-ready") {
            Ok((r, false)) if r.trim() == "true" => break,
            attempt => {
                if std::time::Instant::now() > deadline {
                    let boot = evaluate_blocking("String(globalThis.__pi_boot_error ?? '')", "pi:boot-err")
                        .map(|(s, _)| s)
                        .unwrap_or_default();
                    return Err(format!(
                        "agent not ready after 20s: last={attempt:?} boot_error={boot}"
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        }
    }
    logcat("agent bundle kicked");

    // 等会话恢复（boot kick 的 restoreLatest）完成，保证 agent_history 可读；
    // 超时不致命（历史晚一点也还在 bundle 内存里）。
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match evaluate_blocking("String(globalThis.__pi_restored === true)", "pi:restored") {
            Ok((r, false)) if r.trim() == "true" => break,
            _ if std::time::Instant::now() > deadline => {
                logcat("WARN session restore flag not set within 5s");
                break;
            }
            _ => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
    Ok(())
}

/// 向 agent 提交一条 prompt（kick；结果经 agent_event 异步流回）。
pub fn agent_prompt(text: &str) -> Result<String, String> {
    let arg = serde_json::to_string(text).map_err(|e| format!("serialize: {e}"))?;
    let (r, err) = evaluate_blocking(
        &format!("globalThis.__pi_prompt({arg})"),
        "pi:prompt",
    )?;
    if err {
        return Err(format!("prompt eval threw: {r}"));
    }
    Ok(r)
}

/// 轮询 agent 状态（busy/lastError/queued）。
pub fn agent_status() -> Result<String, String> {
    let (r, err) = evaluate_blocking("globalThis.__pi_status()", "pi:status")?;
    if err {
        return Err(format!("status eval threw: {r}"));
    }
    Ok(r)
}

/// 重启恢复：取 bundle 内已恢复的历史消息（boot 时从最新 JSONL 会话回放）。
pub fn agent_history() -> Result<String, String> {
    let (r, err) = evaluate_blocking("globalThis.__pi_history()", "pi:history")?;
    if err {
        return Err(format!("history eval threw: {r}"));
    }
    Ok(r)
}

/// 切换到指定会话（bundle 内 repo.open + 回放进 agent 状态与 UI 历史）。
/// kick+轮询模式：__pi_open_session 同步返回 "started"，结果落
/// __pi_session_open_result。不得直接 eval 挂 I/O 的 Promise——waitForPromise
/// 会阻塞 VM 线程，而 fs hostcall 恰需该线程 tick → 桥死锁（smoke2 同款教训）。
pub fn session_open(id: &str) -> Result<(), String> {
    let arg = serde_json::to_string(id).map_err(|e| format!("serialize: {e}"))?;
    let (r, err) = evaluate_blocking(
        &format!("globalThis.__pi_open_session({arg})"),
        "pi:session-open",
    )?;
    if err {
        return Err(format!("session open threw: {r}"));
    }
    if r.trim() != "started" {
        return Err(format!("unexpected session open result: {r}"));
    }
    // 轮询结果（每次 eval 泵一次 VM 事件循环，驱动 fs hostcall 完成）。
    // 大会话回放可能较慢，放宽到 30s。
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let (r, err) = evaluate_blocking(
            "JSON.stringify(globalThis.__pi_session_open_result)",
            "pi:session-open-poll",
        )?;
        if err {
            return Err(format!("session open poll threw: {r}"));
        }
        match r.trim() {
            // 仍在跑
            "null" => {}
            // 成功：结果字符串 "ok" 的 JSON 编码
            "\"ok\"" => return Ok(()),
            // 其余非 null：doOpenSession 的错误 JSON
            other => return Err(format!("session open failed: {other}")),
        }
        if std::time::Instant::now() > deadline {
            return Err("session open timed out after 30s".into());
        }
    }
}

/// 新建空白会话（下一个 prompt 落新 JSONL）。
pub fn session_new() -> Result<(), String> {
    let (r, err) = evaluate_blocking("globalThis.__pi_new_session()", "pi:session-new")?;
    if err {
        return Err(format!("session new threw: {r}"));
    }
    Ok(())
}

/// 中止当前 agent 运行（UI 停止按钮）。
pub fn agent_stop() -> Result<(), String> {
    let (r, err) = evaluate_blocking("globalThis.__pi_stop()", "pi:stop")?;
    if err {
        return Err(format!("stop eval threw: {r}"));
    }
    Ok(())
}

/// M4：热重连 MCP 服务器（改配置后无需重启 App）。
pub fn mcp_reconnect() -> Result<(), String> {
    let (r, err) = evaluate_blocking("globalThis.__pi_mcp_reconnect()", "pi:mcp-reconnect")?;
    if err {
        return Err(format!("mcp reconnect eval threw: {r}"));
    }
    Ok(())
}

/// 命令类插件后端：调用 bundle 里返回字符串的 async/sync 全局函数
/// （__pi_plan / __pi_btw / __pi_goal_apply）。skal waitForPromise 会等待
/// Promise 落定（smoke2 已验证），plan/btw 的嵌套 Agent 运行期间事件仍经
/// loopback 流动。
pub fn call_string_global(fn_name: &str, arg: &str) -> Result<String, String> {
    let f = serde_json::to_string(fn_name).map_err(|e| format!("serialize: {e}"))?;
    let a = serde_json::to_string(arg).map_err(|e| format!("serialize: {e}"))?;
    let (r, err) = evaluate_blocking(&format!("globalThis[{f}]({a})"), "pi:call-global")?;
    if err {
        return Err(format!("{fn_name} threw: {r}"));
    }
    Ok(r)
}

/// PoC 冒烟 v2：初始化 → 注入配置 → 安装桥 → loopback hostcall 往返。
/// 注意：`skal_evaluate` 同步阻塞（会等待 Promise 落定），调用方须在
/// blocking 线程（本函数由 async command 经 spawn_blocking 调用）。
pub fn smoke(data_dir: &str) -> Result<String, String> {
    let port = loopback::start()?;
    init(data_dir)?;

    // 1. 注入配置（桥与负载都从这里读端口/数据目录）
    let cfg_json = serde_json::json!({ "port": port, "dataDir": data_dir });
    let (r, err) = evaluate_blocking(
        &format!("globalThis.__pi_config = {};", cfg_json),
        "pi:config",
    )?;
    if err {
        return Err(format!("config eval threw: {r}"));
    }

    // 2. 安装桥（__pi_hostcall / __pi_on / __pi_dispatch）
    let (r, err) = evaluate_blocking(BRIDGE_JS, "pi-bundle/bridge.js")?;
    if err {
        return Err(format!("bridge eval threw: {r}"));
    }
    let ready = evaluate_blocking("String(globalThis.__pi_bridge_ready)", "pi:check")?;
    if ready.0 != "true" {
        return Err(format!("bridge not ready: {}", ready.0));
    }

    // 3. M1 同步冒烟（能力面探测）
    let (hello, hello_err) = evaluate_blocking(HELLO_JS, "pi-bundle/hello.js")?;

    // 4. M2 桥冒烟：立即返回 "started"，异步结果写 __smoke2，宿主轮询。
    //    （不能从 eval 返回 Promise —— waitForPromise 会阻塞 VM 线程，
    //     而 fetch 的 I/O 完成需要该线程 tick，实测死锁。）
    let (r, err) = evaluate_blocking(SMOKE2_JS, "pi-bundle/smoke2.js")?;
    if err || r.trim() != "started" {
        return Err(format!("smoke2 kick failed: err={err} r={r}"));
    }
    let mut smoke2 = String::from("{\"state\":\"timeout\"}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(250));
        let (s, e) = evaluate_blocking("JSON.stringify(globalThis.__smoke2)", "pi:poll")?;
        if e {
            return Err(format!("poll threw: {s}"));
        }
        if s.contains("\"done\"") || s.contains("\"error\"") {
            smoke2 = s;
            break;
        }
        if std::time::Instant::now() > deadline {
            smoke2 = format!("{{\"state\":\"timeout\",\"last\":{s}}}");
            break;
        }
    }
    logcat(&format!("smoke2 -> {smoke2}"));

    if hello_err {
        return Err(format!("hello_err: {hello}"));
    }
    Ok(format!("{{\"hello\":{hello},\"smoke2\":{smoke2}}}"))
}
