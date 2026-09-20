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
/// M5 真机网络探测（排障：DNS / loopback / 外网 HTTPS）
const NETPROBE_JS: &str = include_str!("../../../pi-bundle/netprobe.js");
/// M6 系统原生能力自检（四个能力各打一次 hostcall）
const NATIVEPROBE_JS: &str = include_str!("../../../pi-bundle/nativeprobe.js");

// ── skal C ABI（只用 PoC 需要的 4 个符号）──────────────────────────

type SkalHandle = i64;

#[allow(non_snake_case)]
// iOS：init 被 cfg 门控（静态链路待 M5），ABI 类型别名暂未使用
mod abi {
    use std::ffi::{c_char, c_int};

    pub type CreateRuntime = unsafe extern "C" fn(dir: *const c_char, dir_len: usize) -> i64;
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
    /// D14：隔离脚本 runner。签名与 `patches/pi_entry.zig` 的
    /// `pibun_run_script` 一致（用现有 free_string 释放 out_result）。
    pub type RunScript = unsafe extern "C" fn(
        token: *const u8,
        token_len: usize,
        port: u16,
        code: *const u8,
        code_len: usize,
        wall_ms: u32,
        out_result: *mut *mut u8,
        out_result_len: *mut usize,
        out_is_error: *mut c_int,
    ) -> c_int;
    pub type WasReused = unsafe extern "C" fn() -> c_int;
}

/// 持有 dlopen 的库与符号表。库句柄必须存活于运行时整个生命周期。
struct PiBunRuntime {
    _lib: Library,
    handle: SkalHandle,
    evaluate: abi::Evaluate,
    free_string: abi::FreeString,
    /// 软绑定：老产物没有这个符号时为 None（脚本能力不可用，其余功能照常）。
    run_script: Option<abi::RunScript>,
}

static RUNTIME: OnceLock<Mutex<Option<PiBunRuntime>>> = OnceLock::new();

/// 进程内单运行时（skal 语义：每进程一个 VM，重复 create 会复用）。
fn runtime_lock() -> &'static Mutex<Option<PiBunRuntime>> {
    RUNTIME.get_or_init(|| Mutex::new(None))
}

/// 日志文件路径（两个平台共用）。由 `agent_init` 在拿到 data_dir 后写入。
///
/// 为什么不能靠平台默认通道：
/// * iOS：`println!` 进统一日志，但 `devicectl` 不转 stdout，
///   `idevicesyslog` 在 CoreDevice 隧道占用 uSMux 后也连不上设备。
/// * Android：Honor 等 ROM 会间歇性加密/丢弃任意 tag 的 logcat
///   （本仓库早期就记过：tag 含连字符被加密成 HKS/HKE 块，改 `pibun`
///   只能缓解）；而 `HOME` 在 Android 应用进程里不存在，早先回退到
///   `/tmp` 根本不可写 —— 于是 Android 侧完全盲调。
///
/// 所以统一写到 **data_dir**（两端都是 app 可写的真实目录）：
/// * iOS：`xcrun devicectl device copy from --domain-type appDataContainer \
///      --domain-identifier <bundle-id> --source "Library/Application Support/
///      com.sternelee.pi-mobile/pi-bun.log" --destination <本地>`
/// * Android（debug 包）：`adb shell run-as com.sternelee.pi_mobile cat files/pi-bun.log`
static LOG_PATH: OnceLock<String> = OnceLock::new();

/// 由 `agent_init` 调用：把日志落到 data_dir 下。
pub(crate) fn set_log_dir(data_dir: &str) {
    let _ = LOG_PATH.set(format!("{data_dir}/pi-bun.log"));
}

/// **宿主路径登记 —— 两条运行时路线都必须调。**
///
/// 建好标准子目录（workspace / sessions）并登记 `loopback` 那几个 OnceLock。
/// 为什么必须抽成一处：`lib.rs` 里那些 **UI 侧命令**（`workspace_tree` /
/// `workspace_read` / `workspace_revert` / `preview_*` / git）只认
/// `loopback::configure` 写下的根目录，而 agent 自己的文件工具读的是
/// `HostTools` 里的那份 —— 两套来源都在，漏登记其中一套时：
/// **UI 报「workspace not configured」、agent 却读写正常**，很难联想到是路径没登记。
/// bun 路线一直调着，qjs 路线曾经漏掉（真机上就是这么发现的），所以现在两条都从这里走。
pub(crate) fn configure_host_paths(data_dir: &str) -> Result<(), String> {
    let workspace = format!("{data_dir}/workspace");
    let sessions = format!("{data_dir}/sessions");
    for dir in [&workspace, &sessions] {
        std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {dir}: {e}"))?;
    }
    loopback::configure(&workspace, data_dir);
    Ok(())
}

/// 向日志文件追一行（两端共用；写失败不影响调用方）。
fn log_to_file(line: &str) {
    use std::io::Write;
    let Some(path) = LOG_PATH.get() else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
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
        let prio = if msg.starts_with("ERROR") {
            ERROR
        } else {
            INFO
        };
        __android_log_print(prio, tag.as_ptr(), text.as_ptr());
    }
    log_to_file(&format!("[pi-bun] {msg}"));
}

#[cfg(not(target_os = "android"))]
pub(crate) fn logcat(msg: &str) {
    println!("[pi-bun] {msg}");
    log_to_file(&format!("[pi-bun] {msg}"));
}

/// 初始化（懒加载）：dlopen + create_runtime，失败原因显式返回。
fn init(data_dir: &str) -> Result<(), String> {
    let mut guard = runtime_lock().lock().unwrap();
    if guard.is_some() {
        return Ok(());
    }

    // 平台库名：Android = libskal.so（jniLibs），iOS = libskal.dylib（Embed Frameworks）
    #[cfg(target_os = "android")]
    let lib_name = "libskal.so";
    #[cfg(target_os = "ios")]
    let lib_name = "@rpath/libskal.dylib";
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let lib_name = "libskal.so";

    let lib =
        unsafe { Library::new(lib_name) }.map_err(|e| format!("dlopen {lib_name} failed: {e}"))?;

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
    // D14 脚本执行。**软绑定**：老产物的 .o 里没有这个符号（iOS 的
    // ios-release 对象就是旧入口编的，实测 0 次），缺了不该让整个 agent 起不来
    // —— 只把脚本能力置为不可用，其余功能照常。
    let run_script: Option<abi::RunScript> =
        unsafe { lib.get(b"pibun_run_script").ok().map(|s| *s) };
    if run_script.is_none() {
        logcat("WARN pibun_run_script missing — script execution disabled (stale build?)");
    }

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
        run_script,
    });
    Ok(())
}

/// D14：在隔离 VM 里跑一段 agent 自写的 JS。
///
/// **阻塞**到脚本结束（至多 `wall_ms` + 启动开销），不要在 Tauri 主线程调；
/// 当前只从 loopback 的 per-connection 线程进（`thread::spawn` 出来的）。
///
/// 返回 `(结果 JSON, is_error)`。超时/配额这类失败由 runner 以**结构化 JSON**
/// 回在结果里（`{"ok":false,"kind":"wall-clock-timeout",...}`），不是 Err ——
/// 这样模型能看见失败是什么并自己改，而不是拿到一句“工具报错了”。
pub fn run_script(token: &str, code: &str, wall_ms: u32) -> Result<(String, bool), String> {
    let port = loopback::port().ok_or("loopback not started")?;
    let guard = runtime_lock().lock().unwrap();
    let rt = guard.as_ref().ok_or("runtime not initialized")?;
    let call = rt
        .run_script
        .ok_or("script execution unavailable (pibun_run_script missing in this build)")?;

    let mut out_result: *mut u8 = std::ptr::null_mut();
    let mut out_len: usize = 0;
    let mut out_is_error: c_int = 0;

    let rc = unsafe {
        call(
            token.as_ptr(),
            token.len(),
            port,
            code.as_ptr(),
            code.len(),
            wall_ms,
            &mut out_result,
            &mut out_len,
            &mut out_is_error,
        )
    };
    if rc != 0 {
        return Err(format!("pibun_run_script failed to start (rc={rc})"));
    }
    if out_result.is_null() {
        return Err("pibun_run_script returned null result".into());
    }
    let bytes = unsafe { std::slice::from_raw_parts(out_result, out_len) };
    let text = String::from_utf8_lossy(bytes).into_owned();
    unsafe { (rt.free_string)(out_result.cast::<c_char>()) };
    Ok((text, out_is_error != 0))
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

/// 开发期自检：kick 一个探测脚本，轮询 `globalThis.<slot>` 直到 done，
/// 把逐步结果写进设备日志。
///
/// 背景：真机上很多坑只能靠日志定位（devicectl 不转 stdout、
/// idevicesyslog 在 CoreDevice 隧道占用 uSMux 后连不上设备）—— 所以
/// logcat 同时写 `<HOME>/Documents/pi-bun.log`。
///
/// 两个约束：
/// 1. 探测脚本**不能返回 Promise**（skal_evaluate 的 waitForPromise 会阻塞
///    VM worker 线程，而被探测的 fetch/插件回调恰好靠该线程 tick）。脚本
///    立即返回 "started"，结果增量写全局槽位。
/// 2. 不得阻塞启动 —— 调用方应在独立线程里跑（见 agent_init）。
fn run_probe(label: &str, script: &str, url: &str, slot: &str, budget: std::time::Duration) {
    let (r, err) = match evaluate_blocking(script, url) {
        Ok(v) => v,
        Err(e) => {
            logcat(&format!("{label} kick failed: {e}"));
            return;
        }
    };
    if err || r.trim() != "started" {
        logcat(&format!("{label} kick odd: err={err} r={r}"));
    }

    let deadline = std::time::Instant::now() + budget;
    let poll = format!("JSON.stringify(globalThis.{slot})");
    let mut last = String::from("{\"state\":\"?\"}");
    loop {
        match evaluate_blocking(&poll, &format!("pi:{label}")) {
            Ok((s, false)) => {
                last = s.clone();
                if s.contains("\"done\"") {
                    break;
                }
            }
            Ok((s, true)) => {
                logcat(&format!("{label} poll threw: {s}"));
                break;
            }
            Err(e) => {
                logcat(&format!("{label} poll failed: {e}"));
                break;
            }
        }
        if std::time::Instant::now() > deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    logcat(&format!("{label} -> {last}"));
}

/// 初始化 agent：配置注入 + bundle 加载（同步 kick，立即返回）。
pub fn agent_init(data_dir: &str) -> Result<(), String> {
    // 运行时开关（`PI_AGENT_RUNTIME` 或 `{data_dir}/runtime.txt`）：默认 bun，
    // 选 qjs 时整条链走 QuickJS（src-tauri/src/qjs/），UI 与命令契约不变。
    if crate::qjs::resolve_runtime(data_dir) == crate::qjs::Runtime::QuickJs {
        return crate::qjs::agent_init(data_dir);
    }
    // 先立日志通道：真机排障只能靠文件（各平台 stdout/logcat 都不可靠，
    // 缘由见 set_log_dir 注释）。之后的每条 logcat 都会落盘。
    set_log_dir(data_dir);
    // D16：Android 的 CA 信任库不在 OpenSSL 的默认路径上，必须在任何 git 网络操作
    // 之前指过去 —— 否则 clone/pull 一律报 `SSL certificate is invalid`。
    crate::git::init_tls(data_dir);
    let port = loopback::start()?;
    init(data_dir)?;
    // 开发期自检：debug 构建才跑，且不得阻塞启动（最坏要等各步超时
    // 合计 ~30s + 真实网络/定位往返）。丢到后台线程，日志照样进
    // <HOME>/Documents/pi-bun.log。release 构建下整段被编译掉。
    #[cfg(debug_assertions)]
    {
        std::thread::spawn(|| {
            run_probe(
                "netprobe",
                NETPROBE_JS,
                "pi-bundle/netprobe.js",
                "__netprobe",
                std::time::Duration::from_secs(30),
            );
            run_probe(
                "nativeprobe",
                NATIVEPROBE_JS,
                "pi-bundle/nativeprobe.js",
                "__nativeprobe",
                std::time::Duration::from_secs(40),
            );
        });
    }

    // 标准子目录 + 宿主路径登记（**两条路线共用一处**，见函数注释）
    configure_host_paths(data_dir)?;
    crate::approval::configure(data_dir);
    crate::ask_user::set_resolver(|id, answer| {
        let id_j = serde_json::to_string(id).unwrap_or_else(|_| "\"\"".into());
        let ans_j = serde_json::to_string(answer).unwrap_or_else(|_| "\"\"".into());
        let _ = evaluate_blocking(
            &format!("globalThis.__pi_ask_resolve({id_j}, {ans_j})"),
            "pi:ask-resolve",
        );
    });
    crate::approval::set_resolver(|id, decision| {
        let id_j = serde_json::to_string(id).unwrap_or_else(|_| "\"\"".into());
        let dec_j = serde_json::to_string(decision).unwrap_or_else(|_| "\"deny\"".into());
        let _ = evaluate_blocking(
            &format!("globalThis.__pi_approval_resolve({id_j}, {dec_j})"),
            "pi:approval-resolve",
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
    // D14：agent 主体的身份凭证。`/hostcall` 端点自身必须认证 —— 脚本 VM 是
    // 完整 bun VM、**自带原生 fetch**，不带 token 直接 POST 就会被 dispatch
    // 当成 agent 主体，整套授权被绕过。
    //
    // 安全性来源：脚本 VM 拿不到 `__PI_CONFIG`（隔离测试已证其
    // `typeof __PI_CONFIG === "undefined"`）。**绝不要**把这个值挂到别的
    // 全局上——那等于把 agent 身份交给脚本。
    // init 幂等（OnceLock）：loopback::start 已调过一次，这里取的是同一个值。
    cfg["hostToken"] = serde_json::Value::String(crate::script::init_host_token());
    // 上次保存的默认模型选择：bundle 用 pi-ai 目录解析 (provider, modelId)
    // 为完整模型对象（无选择或目录缺模型时 bundle 落回兜底模型）。
    if let Ok(raw) = std::fs::read_to_string(std::path::Path::new(data_dir).join("provider.json")) {
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

    // D14 诊断（临时）：hostToken 在 bundle 侧是 undefined，但 `port` 明明可用
    // （否则 hostcall 到不了 dispatch）。两者矛盾，所以直接把**实际对象**打出来。
    // 三种结果分辨：
    //   (a) keys 里没有 hostToken        → Rust 侧没带上
    //   (b) keys 里有但 typeof undefined → 对象值异常
    //   (c) 都正常                      → 问题在传输/比较，不在 __PI_CONFIG
    match evaluate_blocking(
        "JSON.stringify({k:Object.keys(globalThis.__PI_CONFIG||{}),t:typeof (globalThis.__PI_CONFIG||{}).hostToken,l:String(((globalThis.__PI_CONFIG||{}).hostToken||'')).length})",
        "pi:cfg-diag",
    ) {
        Ok((s, _)) => logcat(&format!("cfg-diag: {s}")),
        Err(e) => logcat(&format!("cfg-diag failed: {e}")),
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
                    let boot = evaluate_blocking(
                        "String(globalThis.__pi_boot_error ?? '')",
                        "pi:boot-err",
                    )
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
    if crate::qjs::is_quickjs() {
        return crate::qjs::agent_prompt(text);
    }
    let arg = serde_json::to_string(text).map_err(|e| format!("serialize: {e}"))?;
    let (r, err) = evaluate_blocking(&format!("globalThis.__pi_prompt({arg})"), "pi:prompt")?;
    if err {
        return Err(format!("prompt eval threw: {r}"));
    }
    Ok(r)
}

/// 轮询 agent 状态（busy/lastError/queued）。
pub fn agent_status() -> Result<String, String> {
    if crate::qjs::is_quickjs() {
        return crate::qjs::agent_status();
    }
    let (r, err) = evaluate_blocking("globalThis.__pi_status()", "pi:status")?;
    if err {
        return Err(format!("status eval threw: {r}"));
    }
    Ok(r)
}

/// 重启恢复：取 bundle 内已恢复的历史消息（boot 时从最新 JSONL 会话回放）。
pub fn agent_history() -> Result<String, String> {
    if crate::qjs::is_quickjs() {
        return crate::qjs::agent_history();
    }
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
    if crate::qjs::is_quickjs() {
        return crate::qjs::session_open(id);
    }
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
    if crate::qjs::is_quickjs() {
        return crate::qjs::session_new();
    }
    let (r, err) = evaluate_blocking("globalThis.__pi_new_session()", "pi:session-new")?;
    if err {
        return Err(format!("session new threw: {r}"));
    }
    Ok(())
}

/// 中止当前 agent 运行（UI 停止按钮）。
pub fn agent_stop() -> Result<(), String> {
    if crate::qjs::is_quickjs() {
        return crate::qjs::agent_stop();
    }
    let (r, err) = evaluate_blocking("globalThis.__pi_stop()", "pi:stop")?;
    if err {
        return Err(format!("stop eval threw: {r}"));
    }
    Ok(())
}

/// M4：热重连 MCP 服务器（改配置后无需重启 App）。
pub fn mcp_reconnect() -> Result<(), String> {
    if crate::qjs::is_quickjs() {
        return crate::qjs::mcp_reconnect();
    }
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
    if crate::qjs::is_quickjs() {
        return crate::qjs::call_string_global(fn_name, arg);
    }
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
