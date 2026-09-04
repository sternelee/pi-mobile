//! pi_bun —— libpi-bun PoC 装载模块（M1）
//!
//! 当前阶段：dlopen skal 官方预构建产物（`libskal-android-arm64.so`，skal ABI），
//! 在真机上验证「嵌入式 bun + JSC 运行时」执行 JS 的完整链路。
//! 后续（M1 后半）：换成我们自己的 pi_entry.zig + `pi_bun_*` C ABI（见
//! `src-tauri/pi_bun/include/pi_bun.h`），接口形态不变。
//!
//! 参考文档：docs/LIBPI-BUN-NOTES.md、docs/CONTRACTS.md §2、skal.h（skal 仓）。

use std::ffi::{c_char, c_int, CString};
use std::sync::{Mutex, OnceLock};

use libloading::Library;

/// PoC 负载：与桌面 bun 同版本、无依赖的探测脚本。
const HELLO_JS: &str = include_str!("../../../pi-bundle/hello.js");

// ── skal C ABI（只用 PoC 需要的 4 个符号）──────────────────────────

type SkalHandle = i64;

#[allow(non_snake_case)]
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
fn logcat(msg: &str) {
    use std::ffi::CString;
    extern "C" {
        fn __android_log_print(prio: i32, tag: *const c_char, text: *const c_char) -> i32;
    }
    const INFO: i32 = 4;
    const ERROR: i32 = 5;
    let tag = CString::new("pi-bun").unwrap();
    let text = CString::new(msg.replace('\0', " ")).unwrap();
    unsafe {
        let prio = if msg.starts_with("ERROR") { ERROR } else { INFO };
        __android_log_print(prio, tag.as_ptr(), text.as_ptr());
    }
}

#[cfg(not(target_os = "android"))]
fn logcat(msg: &str) {
    println!("[pi-bun] {msg}");
}

/// 初始化（懒加载）：dlopen + create_runtime，失败原因显式返回。
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

/// PoC 冒烟：初始化（如需）→ 执行 hello.js → 回传结果给 UI/logcat。
/// 注意：`skal_evaluate` 同步阻塞，调用方须在 blocking 线程（本函数由
/// async command 经 spawn_blocking 调用）。
pub fn smoke(data_dir: &str) -> Result<String, String> {
    init(data_dir)?;
    let t0 = std::time::Instant::now();
    let (result, is_error) = evaluate_blocking(HELLO_JS, "pi-bundle/hello.js")?;
    let dt = t0.elapsed().as_millis();
    logcat(&format!("evaluate ok in {dt}ms -> {result}"));
    if is_error {
        return Err(format!("JS threw: {result}"));
    }
    Ok(result)
}
