//! native —— 系统原生能力层（M6）。
//!
//! agent 工具里那些「不是文件、不是网页」的能力都走这里：剪贴板、通知、
//! 定位、天气，以及后续的日历/通讯录/照片。
//!
//! ## 为什么是独立一层（而不是塞进 loopback 的 `tool`）
//!
//! `loopback.rs` 的 `tool` 通道是 **workspace jail 内的文件操作**（D6：无
//! exec，路径越狱防护）。系统能力与文件系统是两套信任模型：前者读的是用户
//! 的通讯录/照片/位置，后者读的是一个沙箱子目录。混在一起会让 jail 语义
//! 变模糊，也会让审计时看不清哪些调用碰了真实用户数据。所以另开
//! `native` hostcall method，工具名与权限在 [`CAPABILITIES`] 里统一登记。
//!
//! ## 原生实现的选型纪律（来自 keepalive.rs 的血泪教训）
//!
//! `keepalive.rs` 记录了两次真机事故：从 Rust 走 `ndk_context` 裸 JNI，
//! 会因 panic 杀死宿主线程（审批决策丢失）或带崩 wry 事件循环。
//! 因此本层的硬性纪律是：**能用官方 Tauri 插件就用插件**——插件把 Android
//! JNI / iOS ObjC 的管线都封装在各自的原生侧，Rust 侧只调
//! `run_mobile_plugin`，不会把宿主线程暴露给原生代码的崩溃。
//!
//! 当前用插件的：剪贴板（`tauri-plugin-clipboard-manager`）、通知
//! （`tauri-plugin-notification`）、定位（`tauri-plugin-geolocation`）。
//! 天气无系统 API，走 Open-Meteo 公共接口（无需 key），纯 Rust HTTP。
//!
//! 插件覆盖不到的（日历 EventKit/CalendarContract、通讯录
//! Contacts/ContactsContract、照片 Photos/MediaStore）后续按**正规 Tauri
//! 插件**形态补，不写裸 JNI。

use serde_json::{json, Value};
use std::io::Read;
use std::sync::OnceLock;
use tauri::AppHandle;

/// Tauri 在 `setup()` 里注入。插件 API 都挂在 `Manager` 上（`ClipboardExt`
/// 等 trait 对 `AppHandle` 实现），所以这里存一份句柄即可。
static APP: OnceLock<AppHandle> = OnceLock::new();

pub fn set_app(handle: AppHandle) {
    let _ = APP.set(handle);
}

fn app() -> Result<&'static AppHandle, String> {
    APP.get().ok_or_else(|| "native: app handle not set".into())
}

// ── 能力注册表 ────────────────────────────────────────────────────────
//
// 单一真源：UI 设置页的能力开关、CONTRACTS §2.2 的文档、agent 工具白名单
// 都从这里读。新增能力必须先在这里登记（CONTRACTS §4「变更纪律」）。

pub struct Capability {
    /// 稳定 id（UI / 权限请求 / 审计日志共用）
    pub id: &'static str,
    pub title: &'static str,
    /// 一句话说明「agent 拿它做什么」，UI 直接展示给用户（权限同理心）
    pub detail: &'static str,
    /// agent 工具名
    pub tools: &'static [&'static str],
    /// 支持的平台：android / ios / desktop
    pub platforms: &'static [&'static str],
    /// 是否需要运行时权限（决定 UI 是否显示「授权」按钮）
    pub needs_permission: bool,
}

pub const CAPABILITIES: &[Capability] = &[
    Capability {
        id: "clipboard",
        title: "Clipboard",
        detail: "Read what you just copied, or write a result back to the clipboard",
        tools: &["clipboard"],
        platforms: &["android", "ios", "desktop"],
        needs_permission: false, // iOS 读剪贴板会弹系统提示，但无 API 可预请求
    },
    Capability {
        id: "notification",
        title: "Notifications",
        detail: "Send a system notification when a task finishes or needs your decision",
        tools: &["notify"],
        platforms: &["android", "ios", "desktop"],
        needs_permission: true,
    },
    Capability {
        id: "location",
        title: "Location",
        detail: "Get the current coordinates for weather, travel time and nearby info",
        tools: &["location"],
        platforms: &["android", "ios", "desktop"],
        needs_permission: true,
    },
    Capability {
        id: "calendar",
        title: "Calendar",
        detail: "Read your schedule, or add events to the system calendar (writes need your approval)",
        tools: &["calendar_list", "calendar_create"],
        platforms: &["android", "ios"],
        needs_permission: true,
    },
    Capability {
        id: "contacts",
        title: "Contacts",
        detail: "Look up a contact's phone and email by name (read-only — never modifies your contacts)",
        tools: &["contacts"],
        platforms: &["android", "ios"],
        needs_permission: true,
    },
    Capability {
        id: "photos",
        title: "Photos",
        detail: "Browse the library by time and copy a photo into the workspace (your library is never modified)",
        tools: &["photos_list", "photos_save"],
        platforms: &["android", "ios"],
        needs_permission: true,
    },
    Capability {
        id: "weather",
        title: "Weather",
        detail: "Current conditions and a multi-day forecast (data from Open-Meteo, no account needed)",
        tools: &["weather"],
        platforms: &["android", "ios", "desktop"],
        needs_permission: false,
    },
];

/// UI 用：能力清单 + 各自权限态。
pub fn status() -> Value {
    let caps: Vec<Value> = CAPABILITIES
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "title": c.title,
                "detail": c.detail,
                "tools": c.tools,
                "platforms": c.platforms,
                "needsPermission": c.needs_permission,
                "permission": permission_state(c.id),
                "supported": c.platforms.contains(&platform_tag()),
            })
        })
        .collect();
    json!({ "platform": platform_tag(), "capabilities": caps })
}

fn platform_tag() -> &'static str {
    if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "ios") {
        "ios"
    } else {
        "desktop"
    }
}

// ── 权限 ──────────────────────────────────────────────────────────────

fn permission_state(cap: &str) -> &'static str {
    let Ok(app) = app() else { return "unknown" };
    match cap {
        "notification" => {
            use tauri_plugin_notification::NotificationExt;
            match app.notification().permission_state() {
                Ok(tauri::plugin::PermissionState::Granted) => "granted",
                Ok(tauri::plugin::PermissionState::Denied) => "denied",
                Ok(_) => "prompt",
                Err(_) => "unknown",
            }
        }
        "location" => {
            use tauri_plugin_geolocation::GeolocationExt;
            match app.geolocation().check_permissions() {
                Ok(s) => {
                    use tauri::plugin::PermissionState::*;
                    match s.location {
                        Granted => "granted",
                        Denied => "denied",
                        _ => "prompt",
                    }
                }
                Err(_) => "unknown",
            }
        }
        // 日历：走 pi-native 的原生查询（EventKit / CalendarContract），
        // **同步且不弹窗**。
        //
        // 早期版本在这里硬编码 "unknown"（理由是「Rust 侧查不到」）——
        // 后果是设置页永远显示 Allow，用户授权后看不到状态更新，只能靠
        // 反复点击试探。状态查询本来就不需要弹窗能力，让它真的去问系统。
        //
        // 原生侧可能返回第五种值 "writeOnly"（iOS 只写权限）：那是「能写不能读」，
        // 报成 granted 会让用户以为能读、直到工具调用才失败。向上折叠成
        // "denied" 并让 UI 引导用户去系统设置补全访问 —— 对用户来说可操作
        // 的动作是一样的（去开权限）。
        "photos" => {
            use tauri_plugin_pi_native::PiNativeExt;
            match app.pi_native().permission_state(
                tauri_plugin_pi_native::PermissionKind::Photos,
            ) {
                Ok(st) => match st.state.as_str() {
                    "granted" => "granted",
                    // iOS 14+ 的「受限访问」：能读但不完整。折叠成 denied 让 UI
                    // 引导去补全（对用户来说动作一样：去开权限）。
                    "limited" => "denied",
                    "denied" => "denied",
                    _ => "prompt",
                },
                Err(_) => "unknown",
            }
        }
        "contacts" => {
            use tauri_plugin_pi_native::PiNativeExt;
            match app.pi_native().permission_state(
                tauri_plugin_pi_native::PermissionKind::Contacts,
            ) {
                Ok(st) => match st.state.as_str() {
                    "granted" => "granted",
                    "denied" => "denied",
                    // iOS 18 的「部分授权」：能读但不完整，折叠成 denied 让 UI
                    // 引导去补全（对用户来说动作一样：去开权限）。
                    "limited" | "writeOnly" => "denied",
                    _ => "prompt",
                },
                Err(_) => "unknown",
            }
        }
        "calendar" => {
            use tauri_plugin_pi_native::PiNativeExt;
            match app.pi_native().permission_state(
                tauri_plugin_pi_native::PermissionKind::Calendar,
            ) {
                Ok(st) => match st.state.as_str() {
                    "granted" => "granted",
                    "denied" => "denied",
                    "writeOnly" => "denied",
                    "prompt" => "prompt",
                    _ => "unknown",
                },
                Err(_) => "unknown",
            }
        }
        // 无 API 可查询的能力：视为无需授权
        _ => "granted",
    }
}

/// UI 用：请求某项能力的权限（触发系统弹窗）。
pub fn request(cap: &str) -> Result<Value, String> {
    let app = app()?;
    match cap {
        "notification" => {
            use tauri_plugin_notification::NotificationExt;
            app.notification()
                .request_permission()
                .map_err(|e| format!("notification permission: {e}"))?;
        }
        "location" => {
            use tauri_plugin_geolocation::{GeolocationExt, PermissionType};
            // 只申请精确定位；插件会在 iOS 上同时覆盖「使用期间」授权。
            app.geolocation()
                .request_permissions(Some(vec![PermissionType::Location]))
                .map_err(|e| format!("location permission: {e}"))?;
        }
        "calendar" => {
            // 日历权限只能由原生侧触发系统弹窗，走 pi-native 的
            // requestPermission 命令；两端实现里都用系统 API 请求。
            use tauri_plugin_pi_native::PiNativeExt;
            app.pi_native()
                .request_permission(tauri_plugin_pi_native::PermissionKind::Calendar)
                .map_err(|e| format!("calendar permission: {e}"))?;
        }
        "contacts" => {
            use tauri_plugin_pi_native::PiNativeExt;
            app.pi_native()
                .request_permission(tauri_plugin_pi_native::PermissionKind::Contacts)
                .map_err(|e| format!("contacts permission: {e}"))?;
        }
        "photos" => {
            use tauri_plugin_pi_native::PiNativeExt;
            app.pi_native()
                .request_permission(tauri_plugin_pi_native::PermissionKind::Photos)
                .map_err(|e| format!("photos permission: {e}"))?;
        }
        other => return Err(format!("capability '{other}' has no requestable permission")),
    }
    Ok(json!({ "capability": cap, "permission": permission_state(cap) }))
}

// ── agent 工具分发 ────────────────────────────────────────────────────
//
// 返回 `Ok(text)` = 给模型的文本结果；`Err(msg)` = 工具错误。
// loopback 的 `native` method 把这里的结果包成 `{text}` / `{error}`。

pub fn tool(name: &str, args: &Value) -> Result<String, String> {
    match name {
        "clipboard" => clipboard(args),
        "notify" => notify(args),
        "location" => location(args),
        "calendar_list" => calendar("list", args),
        "calendar_create" => calendar("create", args),
        "contacts" => contacts(args),
        "photos_list" => photos("list", args),
        "photos_save" => photos("save", args),
        "weather" => weather(args),
        other => Err(format!("unknown native tool: {other}")),
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{key}? (non-empty string required)"))
}

fn arg_f64(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(|v| v.as_f64())
}

fn arg_u64(args: &Value, key: &str, default: u64) -> u64 {
    args.get(key).and_then(|v| v.as_u64()).unwrap_or(default)
}

// ── 剪贴板 ────────────────────────────────────────────────────────────

fn clipboard(args: &Value) -> Result<String, String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    let app = app()?;
    match args.get("op").and_then(|v| v.as_str()).unwrap_or("read") {
        "read" => {
            let text = app
                .clipboard()
                .read_text()
                .map_err(|e| format!("clipboard read: {e}"))?;
            if text.is_empty() {
                return Ok("(clipboard is empty)".into());
            }
            Ok(text)
        }
        "write" => {
            let text = arg_str(args, "text")?;
            // mobile 有 write_text_with_label：iOS 上带 label 可避免每次写入
            // 都弹「已粘贴自…」提示（UIPasteboard 的隐私门槛）。
            #[cfg(mobile)]
            app.clipboard()
                .write_text_with_label(text, "pi-mobile")
                .map_err(|e| format!("clipboard write: {e}"))?;
            #[cfg(not(mobile))]
            app.clipboard()
                .write_text(text)
                .map_err(|e| format!("clipboard write: {e}"))?;
            Ok(format!("copied {} chars to clipboard", text.chars().count()))
        }
        "clear" => {
            app.clipboard()
                .clear()
                .map_err(|e| format!("clipboard clear: {e}"))?;
            Ok("clipboard cleared".into())
        }
        other => Err(format!("clipboard op must be read|write|clear, got '{other}'")),
    }
}

// ── 通知 ──────────────────────────────────────────────────────────────

fn notify(args: &Value) -> Result<String, String> {
    use tauri_plugin_notification::NotificationExt;
    let app = app()?;
    let title = arg_str(args, "title")?;
    let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");

    let permission = app
        .notification()
        .permission_state()
        .map_err(|e| format!("notification permission: {e}"))?;
    if permission != tauri::plugin::PermissionState::Granted {
        return Err(
            "notification permission not granted — ask the user to enable 通知 in 设置".into(),
        );
    }

    let mut builder = app.notification().builder().title(title).body(body);
    // 定时通知（`at` = epoch 毫秒）属于「提醒事项」批次的能力（iOS 用
    // EventKit、Android 用 AlarmManager，语义比 UNNotificationTrigger 更贴
    // 近用户预期），这里不实现——避免为了它引入 `time` 依赖。
    let _ = &mut builder;
    builder
        .show()
        .map_err(|e| format!("notification show: {e}"))?;
    Ok(format!("notification sent: {title}"))
}

// ── 定位 ──────────────────────────────────────────────────────────────
//
// **平台分流是刻意的**，不是遗漏：
//   * iOS → 官方 `tauri-plugin-geolocation`（CoreLocation）。真机实测可用
//     （精度 ~11m）。
//   * Android → 自建 `tauri-plugin-pi-native`（`LocationManager`）。官方插件
//     的 Kotlin 实现走 Google **fused** provider 且
//     `getCurrentLocation(prio, null)` 没有 CancellationToken、没有超时；
//     国内 ROM（实测 Honor MEY-AN00）用高德代理网络定位、GPS provider
//     不可用，拿不到 fix 时回调既不 success 也不 failure → 永久挂起，最后被
//     JS 侧 30s hostcall 超时打断，报出无信息量的 "The operation timed out"。
//
// 两侧返回的 JSON 字段名保持一致（latitude/longitude/accuracyMeters/…），
// 所以 `native::tool("location")` 与 weather 的隐式定位无需分支。
// Android 侧额外给出 `fromLastKnown`/`staleMs` —— 国内 ROM 上首次 fix 常
// 拿不到，只能回退到缓存位置；这两个字段让模型/UI 能判断可信度，而不是把
// 缓存当成实时位置用。

/// 等 fix 的超时（毫秒）。比 JS 侧 hostcall 的 30s 短，这样超时由**我们**
/// 报出（带原因与建议），而不是被 JS 侧截断成一个没有上下文的
/// "The operation timed out"。
const LOCATION_TIMEOUT_MS: u64 = 12_000;

/// 权限未授时统一的、可执行的提示文案（两平台共用，避免措辞漂移）。
fn location_permission_hint() -> String {
    "location permission not granted — ask the user to enable Location in Settings → Agent".into()
}

#[cfg(target_os = "android")]
fn location(args: &Value) -> Result<String, String> {
    use tauri_plugin_pi_native::PiNativeExt;
    let app = app()?;
    let high = args
        .get("highAccuracy")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // 先判权限：未授时给可执行指引，而不是让原生侧报更底层的错误。
    // 复用官方插件的权限查询（它与本插件声明的是同一组 Android 权限）。
    use tauri_plugin_geolocation::GeolocationExt;
    let perm = app
        .geolocation()
        .check_permissions()
        .map_err(|e| format!("location permission check: {e}"))?;
    if perm.location != tauri::plugin::PermissionState::Granted {
        return Err(location_permission_hint());
    }

    let result: Value = app
        .pi_native()
        .location(tauri_plugin_pi_native::LocationArgs {
            high_accuracy: high,
            timeout_ms: LOCATION_TIMEOUT_MS,
        })
        .map_err(|e| format!("get_current_position: {e}"))?;
    Ok(serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".into()))
}

#[cfg(not(target_os = "android"))]
fn location(args: &Value) -> Result<String, String> {
    use tauri_plugin_geolocation::GeolocationExt;
    let app = app()?;
    let high = args
        .get("highAccuracy")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let perm = app
        .geolocation()
        .check_permissions()
        .map_err(|e| format!("location permission check: {e}"))?;
    if perm.location != tauri::plugin::PermissionState::Granted {
        return Err(location_permission_hint());
    }

    let pos = app
        .geolocation()
        .get_current_position(Some(tauri_plugin_geolocation::PositionOptions {
            enable_high_accuracy: high,
            timeout: LOCATION_TIMEOUT_MS as u32,
            maximum_age: 0,
        }))
        .map_err(|e| format!("get_current_position: {e}"))?;

    // 字段集与 Android 分支对齐（provider/staleMs/fromLastKnown 在 iOS 上
    // 无意义，给 null/0/false）—— 让上层与模型看到的 schema 一致。
    Ok(serde_json::to_string_pretty(&json!({
        "latitude": pos.coords.latitude,
        "longitude": pos.coords.longitude,
        "accuracyMeters": pos.coords.accuracy,
        "altitude": pos.coords.altitude,
        "heading": pos.coords.heading,
        "speed": pos.coords.speed,
        "timestampMs": pos.timestamp,
        "provider": Value::Null,
        "staleMs": 0,
        "fromLastKnown": false,
    }))
    .unwrap_or_else(|_| "{}".into()))
}

// ── 日历 ──────────────────────────────────────────────────────────────
//
// 走自建 `pi-native`（两端都没有官方插件）。时间一律 epoch 毫秒：
// 模型不需要猜时区/日期格式，两端也不需要各自解析 ISO8601 ——
// 这类地方最容易出现「差一天/差几小时」的静默错误。
//
// 读（list）自动放行；写（create）在 JS 侧标了 mutating，由审批把关。
// 权限未授时返回可执行指引，而不是空列表 —— 空列表会让模型得出
// 「用户最近没有安排」这个错误结论。
fn calendar(op: &str, args: &Value) -> Result<String, String> {
    use tauri_plugin_pi_native::PiNativeExt;
    let app = app()?;

    let a = tauri_plugin_pi_native::CalendarArgs {
        op: op.to_string(),
        from_ms: args.get("fromMs").and_then(|v| v.as_i64()),
        to_ms: args.get("toMs").and_then(|v| v.as_i64()),
        limit: args.get("limit").and_then(|v| v.as_u64()).map(|v| v as u32),
        title: args.get("title").and_then(|v| v.as_str()).map(String::from),
        start_ms: args.get("startMs").and_then(|v| v.as_i64()),
        end_ms: args.get("endMs").and_then(|v| v.as_i64()),
        all_day: args.get("allDay").and_then(|v| v.as_bool()),
        notes: args.get("notes").and_then(|v| v.as_str()).map(String::from),
        location: args.get("location").and_then(|v| v.as_str()).map(String::from),
    };

    let result: Value = app
        .pi_native()
        .calendar(a)
        .map_err(|e| format!("calendar {op} failed: {e}"))?;
    Ok(serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".into()))
}

// ── 通讯录 ────────────────────────────────────────────────────────────
//
// 只读（search/get）。写通讯录不支持 —— agent 误改/误删联系人是不可逆的
// 社交损失，风险与收益严重不对称，且无产品需求。
//
// limit 默认 25（低于日历的 50）：通讯录是最容易撑爆上下文的数据源，
// 一个号码可能关联十几条字段（手机/工作/家庭/邮箱/地址…）。
fn contacts(args: &Value) -> Result<String, String> {
    use tauri_plugin_pi_native::PiNativeExt;
    let app = app()?;

    let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("search");
    if op != "search" && op != "get" {
        return Err(format!("op must be search|get, got '{op}'"));
    }

    let a = tauri_plugin_pi_native::ContactsArgs {
        op: op.to_string(),
        query: args.get("query").and_then(|v| v.as_str()).map(String::from),
        id: args.get("id").and_then(|v| v.as_str()).map(String::from),
        limit: args.get("limit").and_then(|v| v.as_u64()).map(|v| v as u32),
    };

    let result: Value = app
        .pi_native()
        .contacts(a)
        .map_err(|e| format!("contacts {op} failed: {e}"))?;
    Ok(serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".into()))
}

// ── 照片 ──────────────────────────────────────────────────────────────
//
// 只读：list 取元数据、save 把原图写进 workspace。**不写回用户相册**
// （那需要额外权限，且风险与收益不对称）。
//
// save 的信任边界刻意放在这里：原生侧只接收一个**绝对路径**并写字节，
// 不做任何路径判断；路径由 Rust 用 jail_path 校验后再传入（与
// read/write 工具同一套越狱防护）。这样两个平台上都不存在第二份路径逻辑。
fn photos(op: &str, args: &Value) -> Result<String, String> {
    use tauri_plugin_pi_native::PiNativeExt;
    let app = app()?;

    // save 的 default 文件名：用时间戳避免覆盖已有文件（模型可能连存多张）。
    // 仍受 jail 约束 —— 只是相对 workspace 的默认位置。
    let dest_rel = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            format!("photos/photo-{ts}.jpg")
        });
    // save：先过 workspace jail（与 read/write 工具同一套越狱防护），
    // 再把**绝对路径**交给原生侧 —— 原生侧不做任何路径判断，信任边界只有
    // 这一处。
    let dest_abs = if op == "save" {
        let abs = crate::pi_bun::loopback::jail_path(&dest_rel)?;
        // 原生侧写文件不会自动建父目录，这里显式建（仍在校验过的路径内）
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create dir for photo: {e}"))?;
        }
        Some(abs)
    } else {
        None
    };

    let op_id = args
        .get("id")
        .and_then(|v| v.as_str())
        .map(String::from);

    // **必须给 id**：save 没有 id 无从取图，list 无 id 是正常的。
    if op == "save" && op_id.is_none() {
        return Err("id is required for photos_save".into());
    }

    let a = tauri_plugin_pi_native::PhotosArgs {
        op: op.to_string(),
        from_ms: args.get("fromMs").and_then(|v| v.as_i64()),
        to_ms: args.get("toMs").and_then(|v| v.as_i64()),
        limit: args.get("limit").and_then(|v| v.as_u64()).map(|v| v as u32),
        id: op_id,
        dest_path: dest_abs
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
    };

    let result: Value = app
        .pi_native()
        .photos(a)
        .map_err(|e| format!("photos {op} failed: {e}"))?;

    // save：把原生侧回传的绝对路径换回 workspace 相对路径（不把宿主绝对
    // 路径喂给模型 —— 与 read/write 的展示口径一致）。
    if op == "save" {
        let mut v = result;
        if let Some(obj) = v.as_object_mut() {
            obj.insert("path".into(), Value::String(dest_rel.clone()));
        }
        return Ok(serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".into()));
    }
    Ok(serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".into()))
}

// ── 天气（Open-Meteo，无需 key）────────────────────────────────────────

const WEATHER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
const WEATHER_MAX_BODY: usize = 128 * 1024;

fn weather(args: &Value) -> Result<String, String> {
    // 允许显式给坐标；不给就用当前定位（于是隐式依赖定位能力）。
    let (lat, lon) = match (arg_f64(args, "latitude"), arg_f64(args, "longitude")) {
        (Some(a), Some(b)) => (a, b),
        (None, None) => {
            let loc = location(&json!({}))?;
            let v: Value = serde_json::from_str(&loc).map_err(|e| format!("parse location: {e}"))?;
            (
                v["latitude"].as_f64().ok_or("location has no latitude")?,
                v["longitude"].as_f64().ok_or("location has no longitude")?,
            )
        }
        _ => return Err("latitude and longitude must be provided together".into()),
    };
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(format!("coordinates out of range: {lat},{lon}"));
    }
    let days = arg_u64(args, "days", 3).clamp(1, 16);

    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat:.4}&longitude={lon:.4}\
         &current=temperature_2m,relative_humidity_2m,apparent_temperature,is_day,\
precipitation,weather_code,wind_speed_10m,wind_direction_10m\
         &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_sum,\
precipitation_probability_max,sunrise,sunset\
         &timezone=auto&forecast_days={days}"
    );

    let client = reqwest::blocking::Client::builder()
        .timeout(WEATHER_TIMEOUT)
        .user_agent("pi-mobile-agent/0.1")
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let resp = client
        .get(&url)
        .send()
        .map_err(|e| format!("weather request: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("weather API HTTP {}", resp.status()));
    }
    let mut body = String::new();
    resp.take(WEATHER_MAX_BODY as u64)
        .read_to_string(&mut body)
        .map_err(|e| format!("weather body: {e}"))?;

    // 折成给模型看的紧凑文本（原始 JSON 字段名长且含单位后缀，浪费 token）
    let v: Value = serde_json::from_str(&body).map_err(|e| format!("weather json: {e}"))?;
    Ok(format_weather(&v, lat, lon))
}

fn format_weather(v: &Value, lat: f64, lon: f64) -> String {
    let cur = &v["current"];
    let units = &v["current_units"];
    let mut out = String::new();
    out.push_str(&format!("Weather at {lat:.4},{lon:.4}\n"));
    if let Some(tz) = v["timezone"].as_str() {
        out.push_str(&format!("(timezone: {tz})\n"));
    }
    out.push_str("\nNow:\n");
    let code = cur["weather_code"].as_i64().unwrap_or(-1);
    out.push_str(&format!(
        "  {} ({}) — {}{}\n",
        weather_code_text(code),
        code,
        fmt_num(&cur["temperature_2m"]),
        units["temperature_2m"].as_str().unwrap_or("°C"),
    ));
    out.push_str(&format!(
        "  feels like {}{}, humidity {}{}, wind {}{} from {}{}\n",
        fmt_num(&cur["apparent_temperature"]),
        units["apparent_temperature"].as_str().unwrap_or("°C"),
        fmt_num(&cur["relative_humidity_2m"]),
        units["relative_humidity_2m"].as_str().unwrap_or("%"),
        fmt_num(&cur["wind_speed_10m"]),
        units["wind_speed_10m"].as_str().unwrap_or("km/h"),
        fmt_num(&cur["wind_direction_10m"]),
        units["wind_direction_10m"].as_str().unwrap_or("°"),
    ));

    let daily = &v["daily"];
    if let Some(times) = daily["time"].as_array() {
        out.push_str("\nForecast:\n");
        for (i, t) in times.iter().enumerate() {
            // 首行是「今天」——current 段已经给了实时值，这里只补高低温
            let get = |k: &str| daily[k].as_array().and_then(|a| a.get(i)).cloned();
            out.push_str(&format!(
                "  {} — {}, {}{} ~ {}{}, precip {}mm ({}% chance)\n",
                t.as_str().unwrap_or("?"),
                weather_code_text(get("weather_code").and_then(|x| x.as_i64()).unwrap_or(-1)),
                fmt_num(&get("temperature_2m_min").unwrap_or(Value::Null)),
                daily["temperature_2m_min_units"].as_str().unwrap_or("°C"),
                fmt_num(&get("temperature_2m_max").unwrap_or(Value::Null)),
                daily["temperature_2m_max_units"].as_str().unwrap_or("°C"),
                fmt_num(&get("precipitation_sum").unwrap_or(Value::Null)),
                get("precipitation_probability_max")
                    .and_then(|x| x.as_i64())
                    .map(|x| x.to_string())
                    .unwrap_or_else(|| "?".into()),
            ));
        }
    }
    out
}

fn fmt_num(v: &Value) -> String {
    match v.as_f64() {
        Some(f) if f.fract() == 0.0 => format!("{f:.0}"),
        Some(f) => format!("{f:.1}"),
        None => "?".into(),
    }
}

/// WMO weather code → 人话（Open-Meteo 用的是 WMO 4677 子集）。
fn weather_code_text(code: i64) -> &'static str {
    match code {
        0 => "clear sky",
        1 => "mainly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 => "fog",
        48 => "depositing rime fog",
        51 => "light drizzle",
        53 => "moderate drizzle",
        55 => "dense drizzle",
        56 => "light freezing drizzle",
        57 => "dense freezing drizzle",
        61 => "slight rain",
        63 => "moderate rain",
        65 => "heavy rain",
        66 => "light freezing rain",
        67 => "heavy freezing rain",
        71 => "slight snow",
        73 => "moderate snow",
        75 => "heavy snow",
        77 => "snow grains",
        80 => "slight rain showers",
        81 => "moderate rain showers",
        82 => "violent rain showers",
        85 => "slight snow showers",
        86 => "heavy snow showers",
        95 => "thunderstorm",
        96 => "thunderstorm with slight hail",
        99 => "thunderstorm with heavy hail",
        _ => "unknown",
    }
}
