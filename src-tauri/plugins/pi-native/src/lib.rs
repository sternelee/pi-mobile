//! tauri-plugin-pi-native —— pi-mobile 的自建设备能力插件。
//!
//! ## 为什么存在（而不是全用官方插件）
//!
//! 官方插件覆盖率内、且实测可用的能力走官方插件（剪贴板 / 通知 / iOS 定位 /
//! 天气 HTTP），理由见 `src-tauri/src/native/mod.rs` 的选型纪律。本插件只装
//! 两类东西：
//!
//! 1. **官方插件覆盖不到的能力** —— 日历 / 通讯录 / 照片：没有官方插件，
//!    只能自己写 EventKit + Contacts + Photos 与 CalendarContract +
//!    ContactsContract + MediaStore。
//!
//! 2. **官方插件在其上不可用的能力** —— Android 定位：
//!    `tauri-plugin-geolocation` 的 Kotlin 实现走 Google **fused**
//!    provider，且 `getCurrentLocation(prio, null)` 没有 CancellationToken、
//!    没有超时。国内 ROM（实测 Honor MEY-AN00）用高德（AMap）代理网络定位、
//!    GPS provider 不可用，fused 拿不到 fix 时回调既不 success 也不 failure
//!    —— 永久挂起，最后被 JS 侧 30s hostcall 超时打断，报出无信息量的
//!    "The operation timed out"。所以 Android 侧改用 `LocationManager`
//!    （该 ROM 会把它桥到高德代理）+ 真超时 + last-known 回退。
//!    iOS 侧仍用官方插件（CoreLocation 工作正常），故 `native::location()`
//!    有一处平台分支 —— 这是刻意的，不是遗漏。
//!
//! ## 线程/崩溃纪律（承 keepalive.rs 两次真机事故）
//!
//! 原生代码只在各自平台的插件类里跑，Rust 侧仅 `run_mobile_plugin` ——
//! 不碰 `ndk_context` 裸 JNI，原生侧抛异常不会杀死宿主线程。

use serde_json::Value;
use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Runtime,
};

mod error;
pub use error::{Error, Result};

#[cfg(mobile)]
mod mobile;
#[cfg(mobile)]
pub use mobile::PiNative;

/// 桌面端没有这些系统能力（日历/通讯录/照片/定位是移动端概念）。
/// 保留 `init()` 是为了让同一份 `lib.rs` 在三端都能编译 ——
/// 桌面端调用会得到明确的 unsupported 错误而不是编译失败。
#[cfg(desktop)]
mod desktop;
#[cfg(desktop)]
pub use desktop::PiNative;

/// 官方插件的 `XxxExt` 惯例：`app.pi_native()` 取回插件句柄。
/// 宿主侧（`src-tauri/src/native/mod.rs`）只依赖这个 trait，不接触
/// 句柄的具体构造 —— 移动/桌面两套实现才能无声切换。
pub trait PiNativeExt<R: Runtime> {
    fn pi_native(&self) -> &PiNative<R>;
}

impl<R: Runtime, T: tauri::Manager<R>> PiNativeExt<R> for T {
    fn pi_native(&self) -> &PiNative<R> {
        self.state::<PiNative<R>>().inner()
    }
}

/// `location` 命令的参数。字段名即协议（camelCase）。
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocationArgs {
    /// 高精度（GPS）。false 时允许网络/基站定位，更快更省电。
    #[serde(default)]
    pub high_accuracy: bool,
    /// 等待 fix 的毫秒上限。**必须由调用方给** —— 原生侧会真的用它做超时，
    /// 这正是官方 Android 实现缺的那一环。
    pub timeout_ms: u64,
}

/// `calendar` 命令的参数。
///
/// 时间一律用 **epoch 毫秒**：模型不需要猜时区/日期格式，两端也不需要各自
/// 解析 ISO8601（历史上正是这类地方最容易出现「差一天/差几小时」的静默错误）。
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarArgs {
    /// "list" | "create"
    pub op: String,
    /// list: 时间窗口（闭开区间）。缺省 = 从现在起 7 天。
    #[serde(default)]
    pub from_ms: Option<i64>,
    #[serde(default)]
    pub to_ms: Option<i64>,
    /// list: 返回条数上限（防止把用户十年日历灌进上下文）
    #[serde(default)]
    pub limit: Option<u32>,
    /// create: 必填
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub start_ms: Option<i64>,
    #[serde(default)]
    pub end_ms: Option<i64>,
    #[serde(default)]
    pub all_day: Option<bool>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
}

/// 可主动请求授权的能力（触发系统弹窗）。
///
/// 目前只有日历需要：定位/通知/剪贴板的授权走各自官方插件。
/// 之所以不把日历权限查询也做成「可查询」——EventKit 的授权状态与
/// Android 的运行时权限语义差异较大，做成统一的 status 反而会掩盖差异；
/// 让 UI 显示 unknown 并提供「Allow」，比给一个可能不准的状态更诚实。
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionKind {
    Calendar,
}

/// `requestPermission` 命令的参数。
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionArgs {
    pub kind: PermissionKind,
}

/// 权限当前状态。由原生侧**同步查询**（绝不弹窗）—— 弹窗只在
/// `requestPermission` 里发生。
///
/// 为什么不让 Rust 侧统一「猜」：iOS 有 `.writeOnly` 这一档（只写），
/// Android 没有对应概念；把它硬塞进 granted/denied 会让 UI 显示错误状态
/// （谎报 granted 会让用户以为能读，实际读取会失败）。所以状态枚举保留
/// 两端差异，由上层决定怎么展示。
// 不 derive Copy：`String` 字段不满足 Copy（`PermissionKind` 是 Copy 但结构体
// 整体不是）。Clone 足够 —— 调用点只读一次。
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionStatus {
    pub kind: PermissionKind,
    /// "granted" | "denied" | "prompt" | "writeOnly"
    pub state: String,
}

/// `permissionState` 命令的参数。
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionStateArgs {
    pub kind: PermissionKind,
}

/// 初始化插件。注册的命令名必须与 `build.rs` 的 COMMANDS 一致。
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("pi-native")
        .invoke_handler(tauri::generate_handler![
            location,
            calendar,
            permission_state,
            request_permission,
        ])
        .setup(|app, api| {
            #[cfg(mobile)]
            let pi_native = mobile::init(app, api)?;
            #[cfg(desktop)]
            let pi_native = desktop::init(app, api)?;
            app.manage(pi_native);
            Ok(())
        })
        .build()
}

/// 取当前位置（Android 走自建 `LocationManager` 实现；iOS 不用本命令）。
///
/// 返回 JSON：
/// ```json
/// { "latitude": 22.5, "longitude": 114.0, "accuracyMeters": 30.0,
///   "altitude": null, "heading": null, "speed": null,
///   "timestampMs": 1789000000000, "provider": "network",
///   "staleMs": 3600000, "fromLastKnown": true }
/// ```
/// `fromLastKnown` / `staleMs` 是关键的可信度信号：国内 ROM 上首次 fix 常
/// 拿不到，只能回退到上次位置 —— 调用方（模型 / UI）据此判断是否该提示
/// 「这是缓存位置」。
///
/// **必须 async**：同步命令在宿主线程执行，而原生侧要等 fix / 等系统弹窗，
/// 占着主线程会死锁（iOS 授权流程上已实测过整屏卡死，见 docs/PROGRESS.md）。
#[tauri::command]
async fn location<R: Runtime>(
    app: tauri::AppHandle<R>,
    args: LocationArgs,
) -> Result<Value> {
    use tauri::Manager;
    let pi_native = app.state::<PiNative<R>>();
    pi_native.location(args)
}

/// 查询某项系统权限的当前状态。**同步、不弹窗** —— 供设置页展示，
/// 不会打扰用户。
#[tauri::command]
async fn permission_state<R: Runtime>(
    app: tauri::AppHandle<R>,
    args: PermissionStateArgs,
) -> Result<PermissionStatus> {
    use tauri::Manager;
    let pi_native = app.state::<PiNative<R>>();
    pi_native.permission_state(args.kind)
}

/// 主动请求某项系统权限（触发系统弹窗，用户在系统弹窗作答后才返回）。
///
/// 与其它命令同理必须 async：弹窗要主线程，同步命令占着主线程会死锁。
#[tauri::command]
async fn request_permission<R: Runtime>(
    app: tauri::AppHandle<R>,
    args: PermissionArgs,
) -> Result<Value> {
    use tauri::Manager;
    let pi_native = app.state::<PiNative<R>>();
    pi_native.request_permission(args.kind)
}

/// 读/写系统日历。
///
/// `op: "list"` 返回 `{ events: [...] }`；`op: "create"` 返回 `{ id, ... }`。
/// 时间字段全部为 epoch 毫秒，事件对象另带 `calendarName` 便于模型/用户辨认
/// 事件属于哪个日历（"工作"/"家庭" 等）。
///
/// 与 `location` 同理**必须 async**：权限弹窗与 EventKit/CalendarContract 的
/// 查询都可能等主线程做事，同步命令占着主线程会死锁。
#[tauri::command]
async fn calendar<R: Runtime>(
    app: tauri::AppHandle<R>,
    args: CalendarArgs,
) -> Result<Value> {
    use tauri::Manager;
    let pi_native = app.state::<PiNative<R>>();
    pi_native.calendar(args)
}
