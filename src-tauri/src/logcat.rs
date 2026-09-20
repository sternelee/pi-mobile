//! logcat —— 跨平台日志汇（文件 + 平台通道）。
//!
//! 从 `pi_bun` 抽出来的（bun 运行时已删，见 docs/PROGRESS.md 第十五轮）：这份日志
//! 从来不属于 bun —— qjs 路线、approval、native、keepalive 都在用它，只是当年跟着
//! 「bun 的日志」一起长在了那个模块里。
//!
//! 为什么必须有文件日志（两端的平台通道都不可靠）：
//! * iOS：`println!` 进统一日志，但 `devicectl` 不转 stdout，
//!   `idevicesyslog` 在 CoreDevice 隧道占用 uSMux 后也连不上设备。
//! * Android：Honor 等 ROM 会间歇性加密/丢弃任意 tag 的 logcat
//!   （本仓库早期就记过：tag 含连字符被加密成 HKS/HKE 块，改 tag 名只能缓解）；
//!   而 `HOME` 在 Android 应用进程里不存在，早先回退到 `/tmp` 根本不可写 ——
//!   于是 Android 侧完全盲调。
//!
//! 所以统一写到 **data_dir**（两端都是 app 可写的真实目录）：
//! * iOS：`xcrun devicectl device copy from --domain-type appDataContainer \
//!      --domain-identifier <bundle-id> --source "Library/Application Support/
//!      com.sternelee.pi-mobile/pi-agent.log" --destination <本地>`
//! * Android（debug 包）：`adb shell run-as com.sternelee.pi_mobile cat files/pi-agent.log`

use std::sync::OnceLock;

/// 日志文件路径（两个平台共用）。由 `agent_init` 在拿到 data_dir 后写入。
static LOG_PATH: OnceLock<String> = OnceLock::new();

/// 由 `agent_init` 调用：把日志落到 data_dir 下。
pub(crate) fn set_log_dir(data_dir: &str) {
    let _ = LOG_PATH.set(format!("{data_dir}/pi-agent.log"));
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

/// Android 上把消息打进 logcat（同时落文件，免得 ROM 把它加密/丢了）。
#[cfg(target_os = "android")]
pub(crate) fn logcat(msg: &str) {
    use std::ffi::{c_char, CString};
    extern "C" {
        fn __android_log_print(prio: i32, tag: *const c_char, text: *const c_char) -> i32;
    }
    const INFO: i32 = 4;
    const ERROR: i32 = 5;
    // tag 里**不能有连字符**：Honor 等 ROM 会把带连字符的 tag 加密成 HKS/HKE 块丢给
    // 分析平台（本仓库早期踩过）。所以用无连字符的 `piagent`，别改成 "pi-agent"。
    let tag = CString::new("piagent").unwrap();
    let text = CString::new(msg.replace('\0', " ")).unwrap();
    unsafe {
        let prio = if msg.starts_with("ERROR") {
            ERROR
        } else {
            INFO
        };
        __android_log_print(prio, tag.as_ptr(), text.as_ptr());
    }
    log_to_file(&format!("[pi] {msg}"));
}

#[cfg(not(target_os = "android"))]
pub(crate) fn logcat(msg: &str) {
    println!("[pi] {msg}");
    log_to_file(&format!("[pi] {msg}"));
}
