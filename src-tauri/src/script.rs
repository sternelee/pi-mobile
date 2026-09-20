//! script —— D14 脚本执行的能力授予与边界强制（安全核心）。
//!
//! ## 唯一不变式
//!
//! **脚本永远不能做超出「用户在审批卡上看到的那份能力清单」的事。**
//!
//! 三条支撑缺一不可（D14）：
//! 1. **隔离** —— 脚本跑独立 VM（zig 侧；spike 已证 `__pi_hostcall` 与
//!    `__PI_CONFIG` 在那里是 `undefined`，即 agent 的通道够不着）。
//! 2. **不可伪造的主体** —— 本模块的 per-run token。
//! 3. **边界强制** —— 本模块，在 `loopback::dispatch` 里。
//!
//! ## 为什么判定必须在这里、不能在 JS 侧
//!
//! JS 侧的检查只算 UX：脚本可以自己包一层 fetch 再发请求。唯一可信的判定点
//! 是**真正持有权限的那一侧**（Rust）。这与 CONTRACTS §4 的「能力↔权限单一
//! 真源」同一条纪律。
//!
//! ## ⚠️ 一个必须靠 host token 才能堵的洞
//!
//! 脚本 VM 是**完整 bun VM**，因此它**自带原生 `fetch`**（spike 的第二个 VM
//! 里 `__pi_hostcall` 是 undefined，但 `fetch` 在）。于是脚本可以不带任何
//! token 直接 POST 到 loopback —— 而 dispatch 会把「无 token」当作 **agent
//! 主体**，等于整套授权被绕过。
//!
//! 所以 **`/hostcall` 端点自身必须认证**：agent 的请求带 host token，脚本的
//! 请求带 script token，**两者都没有 → 拒绝**（fail-closed，绝不回退）。
//! `REQUIRE_HOST_TOKEN` 目前是 `false`，因为 bundle 侧还没开始带 token（现有
//! agent/probe 会立刻全挂）。**Phase 3 必须把它翻成 `true`，并与 bundle 的
//! wrapper 改动同一次落地**——在那之前脚本 runner 也不存在，所以此洞当前不
//! 可被利用。

//! ⚠️ 本模块里**工具实现的那半目前没有消费者**（bun 的 hostcall 已随
//! `backup/bun` 归档并从 main 删除，qjs 路线还没接这一类工具的 JS 壳）。
//! 保留实现与测试是刻意的：它正是 docs/PROGRESS.md「qjs 还没接的」那张清单要用的东西
//! （接上壳时把本文件的 `allow(dead_code)` 去掉即可，顺便就能看出还差哪几个）。
//! 所以这里显式 allow 掉「暂时无人调用」，而不是删代码或留一堆 warn。
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 脚本请求里携带 per-run token 的字段名（zig runner 注入的 wrapper 会带上）。
pub const TOKEN_FIELD: &str = "__scriptToken";

/// agent 请求里携带 host token 的字段名（bundle wrapper 会带上）。
pub const HOST_TOKEN_FIELD: &str = "__hostToken";

/// 是否强制 host token。见模块头「一个必须靠 host token 才能堵的洞」。
///
/// 已翻为 `true`。前置条件已满足：真机上诊断日志**完全静默**（ABSENT 0 /
/// MISMATCH 0），证明 token 真的送到了。
///
/// 曾经的失败原因值得留档：初版从**内层 `payload`** 找 token，而两个 token
/// 都是它的**同级**字段（见 `loopback::dispatch` 的 `body` 参数），于是永远读
/// 不到 → 翻 true 后所有 hostcall 被拒（一次启动 23 次 deny）。**客户端一直是
/// 对的，错在读的那一层。** 回退成 false 等于重新打开模块头那个洞。
pub const REQUIRE_HOST_TOKEN: bool = true;

const DEFAULT_CALLS: u32 = 200;
const DEFAULT_WALL_MS: u64 = 5_000;
const DEFAULT_BYTES: usize = 256 * 1024;

/// **可授予能力清单 —— 唯一真源。**
///
/// UI 展示、审批卡、边界判定全部从这里取。分头写必然漂移（CONTRACTS §4）。
/// 第二列是给用户看的说明，直接进审批卡——用户看的就是这两个字段。
pub const GRANTABLE: &[(&str, &str)] = &[
    ("fs:read", "读取工作区内的文件"),
    ("fs:write", "写入/修改工作区内的文件"),
    ("net", "访问网络"),
    ("native:contacts", "读取通讯录"),
    ("native:photos", "读取相册"),
    ("native:photos:write", "把照片写入相册"),
    ("native:calendar:read", "读取日历日程"),
    ("native:calendar:write", "写入日历事件"),
    ("native:location", "获取当前位置"),
    ("native:clipboard", "读取剪贴板"),
    ("native:clipboard:write", "写入剪贴板"),
    ("native:notify", "发送系统通知"),
    ("native:weather", "查询天气"),
];

/// **永不可授予** —— 脚本拿到即可冒充 agent 或窃取凭证。
///
/// 列清这份与列清 `GRANTABLE` 同等重要：只写后者等于默认其余可给。
/// 名字用的是 hostcall 的 method，便于审计时与 dispatch 对照。
pub const NEVER_GRANTABLE: &[(&str, &str)] = &[
    ("agent_event", "伪造 agent 生命周期事件"),
    ("approval_request", "自己弹审批（可以自问自答绕过人）"),
    ("ask_user_register", "劫持询问 UI"),
    ("creds_get", "读取凭证"),
    ("creds_set", "写入凭证"),
    ("creds_json_get", "读取凭证文件"),
    ("creds_json_set", "写入凭证文件"),
    ("oauth_pkce", "发起 OAuth"),
    ("oauth_listen", "监听 OAuth 回调（可窃取授权码）"),
    ("mcp_config", "读写 MCP 配置"),
    ("goal_get", "读取 goal 状态"),
    ("skills_config", "读写 skills 配置"),
    ("native_capabilities", "枚举设备能力与权限态"),
];

/// 一次脚本运行的授权记录。
struct Run {
    /// 用户实际批准的清单（审批卡上展示的就是它）。
    granted: HashSet<String>,
    /// 脚本声明要用的（`needs` 原样）。用于报错时对照。
    requested: Vec<String>,
    calls: u32,
    bytes: usize,
    deadline: Instant,
    max_calls: u32,
    max_bytes: usize,
}

/// grant 的结果：交给 zig runner 去起隔离 VM。
pub struct Granted {
    pub run_id: String,
    pub token: String,
    pub capabilities: Vec<String>,
    pub wall_ms: u64,
}

static RUNS: OnceLock<Mutex<HashMap<String, Run>>> = OnceLock::new();
static HOST_TOKEN: OnceLock<String> = OnceLock::new();
static SEQ: AtomicU64 = AtomicU64::new(0);

fn runs() -> &'static Mutex<HashMap<String, Run>> {
    RUNS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 生成不可伪造的 token。
///
/// 从 `/dev/urandom` 取 32 字节——iOS 与 Android 都有该设备，且不需要为此
/// 引入 `rand`/`getrandom` 依赖。兜底路径（时间+计数+地址混合）**不是密码学
/// 安全**，只在 urandom 打不开时走；真机上不会走到。
fn random_token(prefix: &str) -> String {
    use std::io::Read;
    let mut buf = [0u8; 32];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        if f.read_exact(&mut buf).is_ok() {
            let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
            return format!("{prefix}_{hex}");
        }
    }
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let c = SEQ.fetch_add(1, Ordering::Relaxed);
    format!(
        "{prefix}_{t:x}{c:x}{:x}{:x}",
        std::process::id(),
        &SEQ as *const _ as usize
    )
}

/// 进程级 host token（agent 主体身份）。`loopback::start` 调一次。
pub fn init_host_token() -> String {
    HOST_TOKEN.get_or_init(|| random_token("host")).clone()
}

/// 校验 host token 是否是本进程签发的那一个。
pub fn host_token_valid(token: &str) -> bool {
    HOST_TOKEN.get().is_some_and(|t| {
        // 定长比较，不做短路——token 是定长 hex，避免按前缀逐字节泄时序信息。
        t.len() == token.len()
            && t.bytes()
                .zip(token.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    })
}

/// 能力目录（UI 设置页 / 审计用）。含永不可授予，便于「为什么脚本做不了这个」。
pub fn catalog() -> serde_json::Value {
    serde_json::json!({
        "grantable": GRANTABLE
            .iter()
            .map(|(id, desc)| serde_json::json!({ "id": id, "desc": desc }))
            .collect::<Vec<_>>(),
        "neverGrantable": NEVER_GRANTABLE
            .iter()
            .map(|(id, desc)| serde_json::json!({ "id": id, "desc": desc }))
            .collect::<Vec<_>>(),
    })
}

/// 校验模型声明的 `needs`，返回规范化后的清单。
///
/// 拒绝的两类：**永不可授予**（否则会让用户去批准一件我们绝不会执行的事——
/// 既浪费用户注意力，又给了「批了却不生效」的错误预期）与**未知**（拼错的能力
/// 名必须报错，不能静默忽略——静默忽略会变成「批了但脚本仍失败」的谜题）。
pub fn validate_needs(needs: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for raw in needs {
        let cap = raw.trim();
        if cap.is_empty() {
            continue;
        }
        if NEVER_GRANTABLE.iter().any(|(id, _)| *id == cap) {
            return Err(format!(
                "capability '{cap}' can never be granted to a script (it could impersonate the agent). \
                 Do the action with the corresponding tool instead of inside the script."
            ));
        }
        if !GRANTABLE.iter().any(|(id, _)| *id == cap) {
            let valid: Vec<&str> = GRANTABLE.iter().map(|(id, _)| *id).collect();
            return Err(format!(
                "unknown capability '{cap}'. Valid capabilities: {}",
                valid.join(", ")
            ));
        }
        if !out.iter().any(|c| c == cap) {
            out.push(cap.to_string());
        }
    }
    Ok(out)
}

/// 建立一次运行的授权。`needs` 必须已经过 `validate_needs`。
///
/// `wall_ms` 是**墙钟**上界。为什么不能只靠 zig 侧的 JSC 看门狗：spike 发现
/// `Watchdog::shouldTerminate` 是 **CPU 计费**的，脚本阻塞在 I/O（`await fetch`
/// 永不返回）时不消耗 CPU → 看门狗不响。所以这里必须自己记墙钟账
/// （同时它也比「看门狗根本没在 bun 里被压测过」更靠得住）。
pub fn grant(needs: &[String], wall_ms: Option<u64>) -> Granted {
    let run_id = random_token("run");
    let token = random_token("script");
    let caps: Vec<String> = needs.to_vec();
    let wall = wall_ms.unwrap_or(DEFAULT_WALL_MS);
    runs().lock().unwrap().insert(
        token.clone(),
        Run {
            granted: caps.iter().cloned().collect(),
            requested: caps.clone(),
            calls: 0,
            bytes: 0,
            deadline: Instant::now() + Duration::from_millis(wall),
            max_calls: DEFAULT_CALLS,
            max_bytes: DEFAULT_BYTES,
        },
    );
    Granted {
        run_id,
        token,
        capabilities: caps,
        wall_ms: wall,
    }
}

/// 撤销（脚本跑完 / 超时 / 用户取消）。**必须调用**，否则 token 会一直有效。
pub fn revoke(token: &str) {
    runs().lock().unwrap().remove(token);
}

/// 撤销全部（agent 会话结束、或用户中途收回）。
pub fn revoke_all() {
    runs().lock().unwrap().clear();
}

/// 当前活跃运行数（诊断/探针用）。
pub fn active_runs() -> usize {
    runs().lock().unwrap().len()
}

/// 拒绝应答的统一形状。
///
/// **必须给出可执行指引**，不能只回一句 denied：模型看不出下一步就会反复重试
/// （CONTRACTS §2.2「权限未授返回可执行指引而非空列表」同一条纪律）。
fn deny(reason: &str, hint: &str) -> serde_json::Value {
    serde_json::json!({
        "error": reason,
        "hint": hint,
        "denied": true,
    })
}

/// 把能力 id 展开成 `{id, desc}`。
///
/// ⚠️ **当前无调用方**（仅单测）：审批事件的 `capabilities` 只发 id 数组，
/// 说明文案由 UI 经 `script_capabilities` 命令从 `GRANTABLE` 取
/// （见 CONTRACTS §2.2）。留在这个函数是为了万一以后要把 desc 嵌进事件时，
/// **在这里展开而不是在前端 TS 里另拄一张表** —— 两个拼法必然漂移。
pub fn describe(caps: &[String]) -> Vec<serde_json::Value> {
    caps.iter()
        .map(|id| {
            let desc = GRANTABLE
                .iter()
                .find(|(gid, _)| gid == id)
                .map(|(_, d)| *d)
                .unwrap_or("");
            serde_json::json!({ "id": id, "desc": desc })
        })
        .collect()
}

/// hostcall → 所需能力。**白名单**：表里没有的一律拒绝。
///
/// 用白名单而不是「先放行再排除」的原因：dispatch 每加一个 method，白名单会
/// **自动**把它拒在脚本之外（安全默认）；黑名单则会静默放行。
fn required_capability(method: &str, payload: &serde_json::Value) -> Result<&'static str, String> {
    let sub = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
    match (method, sub) {
        // 工作区文件
        ("tool", "read") | ("tool", "ls") | ("tool", "grep") | ("fs", "exists") => Ok("fs:read"),
        ("tool", "write") | ("tool", "edit") | ("tool", "mkdir") | ("fs", "remove") => {
            Ok("fs:write")
        }
        // 网络
        ("http", _) => Ok("net"),
        // 系统原生能力
        ("native", "contacts") => Ok("native:contacts"),
        ("native", "photos_list") => Ok("native:photos"),
        // 写相册与读相册**分开**：photos_save 会改用户相册（不可逆的数据变更），
        // 不能因为「允许看照片」顺带获得。与日历拆 read/write 同理。
        ("native", "photos_save") => Ok("native:photos:write"),
        ("native", "calendar_list") => Ok("native:calendar:read"),
        ("native", "calendar_create") => Ok("native:calendar:write"),
        ("native", "location") => Ok("native:location"),
        ("native", "clipboard") => Ok("native:clipboard"),
        ("native", "notify") => Ok("native:notify"),
        ("native", "weather") => Ok("native:weather"),
        // 表外一律拒绝。含 ping/log（脚本无正当理由用它们：console 由 runner
        // 在 VM 内捕获）与全部 NEVER_GRANTABLE 项。
        _ => Err(format!("hostcall '{method}' is not available to scripts")),
    }
}

/// 边界强制。**脚本的每一次 hostcall 都要先过这里。**
///
/// 返回 `Err(应答)` 时调用方必须原样回给 JS 并**不执行**原动作。
pub fn authorize(
    token: &str,
    method: &str,
    payload: &serde_json::Value,
) -> Result<(), serde_json::Value> {
    let mut guard = runs().lock().unwrap();
    let Some(run) = guard.get_mut(token) else {
        // 未知/已撤销/伪造的 token：**绝不回退成 agent 身份**。
        // 这条是 fail-closed 的关键——回退等于把洞开在授权判定里。
        return Err(deny(
            "invalid or expired script token",
            "this request is not attached to an active script run",
        ));
    };

    // 墙钟上界（见 grant 的注释：看门狗是 CPU 计费的，I/O 挂起不会触发）。
    if Instant::now() > run.deadline {
        guard.remove(token);
        return Err(deny(
            "script run exceeded its wall-clock limit",
            "the script was stopped; avoid blocking I/O and long loops in scripts",
        ));
    }

    // 调用次数配额：防「循环 1 万次读通讯录」把宿主拖死。
    if run.calls >= run.max_calls {
        let max = run.max_calls; // 先取出来：后面还要用 guard.remove，不能带着借用
        guard.remove(token);
        return Err(deny(
            &format!("script exceeded its hostcall quota ({max})"),
            "batch the work or split it across several smaller script runs",
        ));
    }
    run.calls += 1;

    let cap = match required_capability(method, payload) {
        Ok(c) => c,
        Err(msg) => return Err(deny(&msg, "use a normal tool for this instead")),
    };

    if !run.granted.contains(cap) {
        // 漏报 `needs` 是模型最常见的一种失败。错误信息必须点明「未声明」，
        // 否则模型只会原样重试（D14 明确要求）。
        return Err(deny(
            &format!("capability '{cap}' was not declared by this script"),
            &format!(
                "re-run with needs including '{cap}' so the user can approve it; \
                 declared: [{}]",
                run.requested.join(", ")
            ),
        ));
    }
    Ok(())
}

/// 应答体积记账（dispatch 收到应答后调用）。
pub fn record_response(token: &str, response: &serde_json::Value) -> Result<(), serde_json::Value> {
    let mut guard = runs().lock().unwrap();
    let Some(run) = guard.get_mut(token) else {
        return Err(deny(
            "invalid or expired script token",
            "run already finished",
        ));
    };
    run.bytes += response.to_string().len();
    if run.bytes > run.max_bytes {
        let max = run.max_bytes;
        guard.remove(token);
        return Err(deny(
            &format!("script exceeded its response budget ({max} bytes)"),
            "request less data per hostcall (e.g. smaller page sizes or fewer fields)",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn needs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn validate_needs_rejects_never_grantable_and_unknown() {
        // 永不可授予：必须挡在「给用户看审批卡」之前，否则等于让用户批准一件
        // 我们绝不会执行的事。
        let e = validate_needs(&needs(&["native:contacts", "approval_request"])).unwrap_err();
        assert!(e.contains("can never be granted"), "{e}");

        // 拼错的能力名必须报错，不能静默忽略。
        let e = validate_needs(&needs(&["native:contact"])).unwrap_err();
        assert!(e.contains("unknown capability"), "{e}");
        assert!(e.contains("native:contacts"), "应列出合法值: {e}");

        // 合法 + 去重 + 去空白
        let ok = validate_needs(&needs(&["fs:read", "fs:read", " net "])).unwrap();
        assert_eq!(ok, vec!["fs:read", "net"]);
    }

    #[test]
    fn forged_token_never_falls_back_to_agent_authority() {
        // fail-closed 的关键用例：未知 token 必须是拒绝，**不能**退化成
        // 「当 agent 处理」——那等于授权判定自己开了个洞。
        let r = authorize("script_deadbeef", "native", &json!({ "name": "contacts" }));
        let v = r.unwrap_err();
        assert_eq!(v["denied"], true);
        assert!(v["error"].as_str().unwrap().contains("invalid or expired"));
    }

    #[test]
    fn granted_capability_allows_but_ungranted_is_denied_with_hint() {
        let g = grant(&needs(&["native:contacts"]), None);

        assert!(authorize(&g.token, "native", &json!({ "name": "contacts" })).is_ok());

        // 未声明 → 拒绝，且提示必须点明「未声明」并给出 needs 指引
        let v = authorize(&g.token, "native", &json!({ "name": "photos_list" })).unwrap_err();
        assert_eq!(v["denied"], true);
        assert!(v["error"].as_str().unwrap().contains("native:photos"));
        assert!(v["hint"].as_str().unwrap().contains("needs"));
        assert!(v["hint"].as_str().unwrap().contains("native:contacts"));

        revoke(&g.token);
    }

    #[test]
    fn never_grantable_channels_denied_even_with_valid_token() {
        // 就算模型把 needs 里塞满，dispatch 的白名单也必须挡住。
        let g = grant(&needs(&["fs:read", "fs:write", "net"]), None);
        for (method, payload) in [
            ("agent_event", json!({})),
            ("approval_request", json!({ "tool": "write" })),
            ("ask_user_register", json!({})),
            ("creds_get", json!({})),
            ("creds_set", json!({})),
            ("creds_json_get", json!({})),
            ("oauth_pkce", json!({})),
            ("oauth_listen", json!({})),
            ("mcp_config", json!({})),
            ("goal_get", json!({})),
            ("skills_config", json!({})),
            ("native_capabilities", json!({})),
            // 无正当理由的通道也不给脚本
            ("ping", json!({})),
            ("log", json!({ "msg": "x" })),
        ] {
            let v = match authorize(&g.token, method, &payload) {
                Ok(()) => panic!("{method} 不该被放行"),
                Err(v) => v,
            };
            assert_eq!(v["denied"], true, "{method}");
        }
        revoke(&g.token);
    }

    #[test]
    fn mutating_native_actions_need_their_own_capability() {
        // 读相册的授权不能顺带给出写相册/写日历的能力（能力按动作分轴）。
        let g = grant(&needs(&["native:photos"]), None);
        assert!(authorize(&g.token, "native", &json!({ "name": "photos_list" })).is_ok());
        assert!(authorize(&g.token, "native", &json!({ "name": "photos_save" })).is_err());
        assert!(authorize(&g.token, "native", &json!({ "name": "calendar_create" })).is_err());
        revoke(&g.token);

        // 反向：只给写相册，不该能读相册
        let g = grant(&needs(&["native:photos:write"]), None);
        assert!(authorize(&g.token, "native", &json!({ "name": "photos_list" })).is_err());
        assert!(authorize(&g.token, "native", &json!({ "name": "photos_save" })).is_ok());
        revoke(&g.token);
    }

    #[test]
    fn wall_clock_deadline_denies_even_when_js_is_not_burning_cpu() {
        // spike 发现看门狗是 CPU 计费的 → I/O 挂起（await fetch 永不返回）时
        // 它不会响。所以这条墙钟账是本层自己的防线，必须独立成立。
        let g = grant(&needs(&["native:contacts"]), Some(0));
        let v = authorize(&g.token, "native", &json!({ "name": "contacts" })).unwrap_err();
        assert!(v["error"].as_str().unwrap().contains("wall-clock"));
        // 超时后 token 应被撤销：后续请求变成「无效 token」而不是再报超时。
        // （不要断言全局 active_runs()==0 —— 测试并行跑，全局表里有别的用例的
        //   run，那样断言会随机失败。）
        let v = authorize(&g.token, "native", &json!({ "name": "contacts" })).unwrap_err();
        assert!(
            v["error"].as_str().unwrap().contains("invalid or expired"),
            "超时后应已撤销: {v}"
        );
    }

    #[test]
    fn hostcall_quota_is_enforced() {
        let g = grant(&needs(&["fs:read"]), None);
        for _ in 0..DEFAULT_CALLS {
            assert!(authorize(&g.token, "tool", &json!({ "name": "read" })).is_ok());
        }
        let v = authorize(&g.token, "tool", &json!({ "name": "read" })).unwrap_err();
        assert!(v["error"].as_str().unwrap().contains("quota"));
        // 超配额即撤销（同样不断言全局计数）
        let v = authorize(&g.token, "tool", &json!({ "name": "read" })).unwrap_err();
        assert!(v["error"].as_str().unwrap().contains("invalid or expired"));
    }

    #[test]
    fn response_byte_budget_is_enforced() {
        let g = grant(&needs(&["fs:read"]), None);
        // 一大块数据不该把事件流/内存冲垮
        let big = json!({ "text": "x".repeat(DEFAULT_BYTES + 16) });
        let v = record_response(&g.token, &big).unwrap_err();
        assert!(v["error"].as_str().unwrap().contains("response budget"));
    }

    #[test]
    fn revoke_kills_the_token() {
        let g = grant(&needs(&["fs:read"]), None);
        assert!(authorize(&g.token, "tool", &json!({ "name": "read" })).is_ok());
        revoke(&g.token);
        assert!(authorize(&g.token, "tool", &json!({ "name": "read" })).is_err());
    }

    #[test]
    fn host_token_roundtrip_and_timing_safe_compare() {
        let t = init_host_token();
        assert!(host_token_valid(&t));
        assert!(!host_token_valid(""));
        assert!(!host_token_valid("host_deadbeef"));
        // 同长度但不同内容也必须拒（走的是逐字节累计 XOR，不短路）
        let mut same_len = t.clone();
        same_len.pop();
        same_len.push(if t.ends_with('a') { 'b' } else { 'a' });
        assert!(!host_token_valid(&same_len));
    }

    /// 展开函数本身仍要从 `GRANTABLE` 取文案（单一真源）。
    ///
    /// 注意：它当前只被本测试用 —— 审批事件已改为只发 id 数组，说明文案由 UI
    /// 经 `script_capabilities` 命令取。这里同时锁住「未知 id 不 panic」：宁可
    /// 在卡上留空，也不能因为一个拼错的能力名把整个审批流程炸掉。
    #[test]
    fn describe_pulls_labels_from_the_single_source() {
        let d = describe(&["native:contacts".to_string(), "net".to_string()]);
        assert_eq!(d[0]["id"], "native:contacts");
        assert_eq!(d[0]["desc"], "读取通讯录");
        assert_eq!(d[1]["desc"], "访问网络");
        // 未知 id 不 panic（宁可在卡上留空也不炸开审批流程）
        assert_eq!(describe(&["nope".to_string()])[0]["desc"], "");
    }

    #[test]
    fn catalog_exposes_both_halves() {
        // 只暴露「可授予」是不够的——「永不可授予」也要能审计
        let c = catalog();
        assert!(c["grantable"].as_array().unwrap().len() >= 10);
        let ng = c["neverGrantable"].as_array().unwrap();
        assert!(ng.iter().any(|x| x["id"] == "approval_request"));
    }
}
