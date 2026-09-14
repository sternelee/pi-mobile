//! approval —— 工具审批（M3 主线）：policy 状态机 + pending 请求表 + diff。
//!
//! 流程（kick+事件注入模式，与 ask_user 同款——真机实测：长挂起 fetch +
//! AbortSignal 会触发嵌入 bun 的 HeapHelper 线程 SIGSEGV/断连，禁止长阻塞
//! hostcall）：
//! 1. bundle 内 mutating 工具执行前发 `approval_request` hostcall →
//!    本模块查 policy：`auto` 直接放行；`ask` 则 emit `approval_required`
//!    事件给 UI（带统一 diff），**立即返回 pending**
//! 2. bundle 工具在 pending promise 上等（120s 超时自动 deny）
//! 3. 用户决策 → `approval_respond` 命令 → resolver（pi_bun 注入的
//!    skal_evaluate 调 `__pi_approval_resolve(id, decision)`）反向解析
//!
//! policy 持久化在 `{data_dir}/policy.json`（M3 基线只有 write 一档；
//! 后续按会话/工具粒度扩展，键空间见 CONTRACTS §3）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// 走审批的工具集（D6：Android bash 暂未注册，保持集合完整以便后续）。
const ASK_TOOLS: &[&str] = &["write", "edit", "mkdir", "bash"];
/// 脚本执行工具（D14）。单独一档：它**永不参与 always 全局降级**，且每次都要
/// 把能力清单展示给用户。
const SCRIPT_TOOLS: &[&str] = &["run_js"];
/// diff 回传 UI 的长度上限（移动端卡片展示，防超大文件拖垮事件流）。
const MAX_DIFF_BYTES: usize = 16 * 1024;

static EVENT_SINK: OnceLock<Box<dyn Fn(&str) + Send + Sync>> = OnceLock::new();
static RESOLVER: OnceLock<Box<dyn Fn(&str, &str) + Send + Sync>> = OnceLock::new();
/// requestId → 触发审批的工具名（respond 时判定 always 是否降 write 基线）。
static PENDING: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static POLICY: OnceLock<Mutex<Policy>> = OnceLock::new();
static DATA_DIR: OnceLock<String> = OnceLock::new();
static SEQ: AtomicU64 = AtomicU64::new(0);

/// 注册 UI 事件转发（与 loopback 事件同一 `pi-agent-event` 通道）。
pub fn set_event_sink(f: impl Fn(&str) + Send + Sync + 'static) {
    EVENT_SINK.set(Box::new(f)).ok();
}

/// pi_bun 在 agent_init 时注入：把决策经 skal_evaluate 打回运行时。
pub fn set_resolver(f: impl Fn(&str, &str) + Send + Sync + 'static) {
    RESOLVER.set(Box::new(f)).ok();
}

/// 启动时配置 data_dir 并加载 policy.json。
pub fn configure(data_dir: &str) {
    DATA_DIR.set(data_dir.into()).ok();
    let policy = std::fs::read_to_string(std::path::Path::new(data_dir).join("policy.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Policy>(&s).ok())
        .unwrap_or(Policy::default());
    POLICY.set(Mutex::new(policy)).ok();
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Policy {
    /// write 类工具基线："ask"（默认）| "auto"
    write: String,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            write: "ask".into(),
        }
    }
}

fn save_policy(policy: &Policy) {
    let Some(dir) = DATA_DIR.get() else { return };
    let path = std::path::Path::new(dir).join("policy.json");
    if let Ok(json) = serde_json::to_string(policy) {
        let _ = std::fs::write(&path, json);
    }
}

/// 当前 write 基线（UI 设置页展示）。未配置时默认 "ask"。
pub fn policy_get() -> String {
    POLICY
        .get()
        .map(|p| p.lock().unwrap().write.clone())
        .unwrap_or_else(|| "ask".into())
}

/// 设置 write 基线（"ask" | "auto"）并持久化——UI 的审批策略开关。
/// 等价于把某个工具上点 "always"/重置的总开关。
pub fn policy_set(policy: &str) -> Result<(), String> {
    if !matches!(policy, "ask" | "auto") {
        return Err(format!("invalid policy: {policy}"));
    }
    let p = POLICY
        .get()
        .ok_or_else(|| "approval not configured".to_string())?;
    let mut g = p.lock().unwrap();
    g.write = policy.to_string();
    save_policy(&g);
    Ok(())
}

/// 统一 diff（unified，context=2）。新文件 old 为空串。
fn unified_diff(path: &str, old: &str, new: &str) -> String {
    let diff = similar::TextDiff::from_lines(old, new);
    let mut out = diff
        .unified_diff()
        .context_radius(2)
        .header(&format!("--- {path}"), &format!("+++ {path}"))
        .to_string();
    truncate_card_text(&out)
}

/// 审批卡片文本的截断上限。UTF-8 边界安全：中文内容几乎必然落在多字节
/// 字符中间，直接 truncate 会 panic。
fn truncate_card_text(text: &str) -> String {
    if text.len() <= MAX_DIFF_BYTES {
        return text.to_string();
    }
    let mut cut = MAX_DIFF_BYTES;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n… (truncated)", &text[..cut])
}

/// 审批请求入口（loopback dispatch 调用）。非阻塞：ask 时 emit 事件并立即
/// 返回 pending，决策经 `__pi_approval_resolve` 注入（kick+resolve 模式）。
pub fn request(payload: &serde_json::Value) -> serde_json::Value {
    let tool = payload.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    // D11：MCP 工具（mcp__<server>__<tool>）默认全部 ask，不受 write 基线
    // 影响（per-server 降 auto 见 D11 完整版）；ASK_TOOLS 里的宿主工具
    // （write/edit/mkdir/bash）仍走 policy 状态机。
    let is_mcp = tool.starts_with("mcp__");
    let is_script = SCRIPT_TOOLS.contains(&tool);
    let needs_ask = is_mcp
        || is_script
        || (ASK_TOOLS.contains(&tool)
            && POLICY
                .get()
                .map(|p| p.lock().unwrap().write == "ask")
                .unwrap_or(true));
    if !needs_ask {
        return serde_json::json!({ "decision": "allow", "policy": "auto" });
    }

    // D14：`needs` 必须在**给用户看审批卡之前**校验。让用户去批准一件我们
    // 绝不会执行的事（永不可授予的能力），既白花它的注意力，又给了「批了却
    // 不生效」的错误预期。校验失败直接把可执行指引回给模型。
    let mut capabilities: Vec<String> = Vec::new();
    let mut script_code = String::new();
    if is_script {
        let args = payload.get("args");
        let needs: Vec<String> = args
            .and_then(|a| a.get("needs"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        match crate::script::validate_needs(&needs) {
            Ok(valid) => capabilities = valid,
            Err(e) => return serde_json::json!({ "decision": "deny", "reason": e }),
        }
        // 审批「一个脚本」却不给看代码是没意义的——用户批的就是这段代码。
        // 与 diff 同额截断（都是移动端卡片展示，不能让超大文本拖垮事件流）。
        if let Some(src) = args.and_then(|a| a.get("code")).and_then(|v| v.as_str()) {
            script_code = truncate_card_text(src);
        }
    }

    // write/edit：算 diff 随审批卡一起给 UI
    let mut diff = String::new();
    let mut path = String::new();
    if let Some(args) = payload.get("args") {
        path = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .into();
        let old = crate::pi_bun::loopback::read_workspace_rel(&path);
        match payload.get("tool").and_then(|v| v.as_str()) {
            Some("write") => {
                if let Some(content) = args.get("content").and_then(|v| v.as_str()) {
                    diff = unified_diff(&path, old.as_deref().unwrap_or(""), content);
                }
            }
            Some("edit") => {
                if let (Some(old_text), Some(new_text)) = (
                    args.get("oldText").and_then(|v| v.as_str()),
                    args.get("newText").and_then(|v| v.as_str()),
                ) {
                    let replace_all = args
                        .get("replaceAll")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if let Some(content) = old {
                        if let Ok(updated) = crate::pi_bun::loopback::apply_edit(
                            &content,
                            old_text,
                            new_text,
                            replace_all,
                        ) {
                            diff = unified_diff(&path, &content, &updated);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let id = format!(
        "apr_{:x}_{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        SEQ.fetch_add(1, Ordering::Relaxed),
    );
    let Some(sink) = EVENT_SINK.get() else {
        // 无 UI（单测/无头）：立即拒绝
        return serde_json::json!({ "decision": "deny", "reason": "no ui attached" });
    };
    PENDING
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .insert(id.clone(), tool.to_string());

    sink(
        &serde_json::json!({
            "type": "approval_required",
            "requestId": id,
            "tool": tool,
            "path": path,
            "diff": diff,
            // 脚本：审批卡要展示的**就是这份清单**（id 数组）。
            //
            // 说明文案由 UI 经 `script_capabilities` 命令从 `script.rs::GRANTABLE`
            // 取（单一真源）——所以这里**只发 id**，不把 desc 再嵌一份：
            // 同一个概念两种拼法，两份一旦不一，用户看到的就是与实际授权集
            // 不同的东西，而审批卡的全部意义就在「所见即所授」。
            "capabilities": capabilities,
            // 脚本源码：审一个看不见的脚本没意义，用户批的就是这段代码。
            "code": script_code,
            // 显式标志，不让前端去硬编码工具名「run_js」（那是把
            // SCRIPT_TOOLS 复制一份，两处必然漂移）。
            "script": is_script,
        })
        .to_string(),
    );
    // M4 保活：常驻通知切高优先级（审批在 agent 运行期内，前台服务已升）
    crate::keepalive::on_approval_pending(tool);

    serde_json::json!({ "requestId": id, "pending": true })
}

/// UI 决策回填（Tauri 命令调用）。decision: allow / deny / always。
/// "always" 在宿主侧降 write 基线（MCP 工具除外），注入运行时的统一为
/// allow/deny —— bundle 侧只认这两个值。
pub fn respond(request_id: &str, decision: &str) -> Result<(), String> {
    if !matches!(decision, "allow" | "deny" | "always") {
        return Err(format!("invalid decision: {decision}"));
    }
    let tool = PENDING
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .remove(request_id)
        .ok_or_else(|| format!("unknown or resolved request: {request_id}"))?;

    let mut effective = decision.to_string();
    if decision == "always" {
        if tool.starts_with("mcp__") {
            // MCP 工具：放行本次，但不降 write 基线（per-server 粒度见 D11 完整版）
            effective = "allow".into();
        } else if SCRIPT_TOOLS.contains(&tool.as_str()) {
            // D14：脚本的 always **只对本次生效**，不降全局基线。脚本的授权
            // 语义是「这份能力清单」；把它降成基线等于永久交出任意能力。
            effective = "allow".into();
        } else {
            // “总是允许” = write 基线降为 auto 并持久化（M3 基线粒度）
            if let Some(p) = POLICY.get() {
                let mut g = p.lock().unwrap();
                g.write = "auto".into();
                save_policy(&g);
            }
            effective = "allow".into();
        }
    }
    crate::keepalive::on_approval_resolved();

    let resolver = RESOLVER
        .get()
        .ok_or_else(|| "resolver not configured (agent not initialized)".to_string())?;
    resolver(request_id, &effective);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;

    #[test]
    fn approval_kick_resolve_flow_and_always_demotes_write() {
        let dir = std::env::temp_dir().join(format!("pi-appr-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("workspace")).unwrap();
        configure(dir.to_str().unwrap());

        // 无 UI attach：ask 策略下立即 deny
        let r = request(&json!({ "tool": "write", "args": { "path": "a.txt", "content": "x" } }));
        assert_eq!(r["decision"], "deny");

        // 有 UI：request 立即返回 pending（无阻塞 hostcall —— 真机断连修复）
        let seen: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let seen_sink = Arc::clone(&seen);
        set_event_sink(move |s| seen_sink.lock().unwrap().push(s.to_string()));
        let injected: Arc<StdMutex<Vec<(String, String)>>> = Arc::new(StdMutex::new(Vec::new()));
        let injected_resolver = Arc::clone(&injected);
        set_resolver(move |id, decision| {
            injected_resolver
                .lock()
                .unwrap()
                .push((id.to_string(), decision.to_string()));
        });

        let r = request(&json!({ "tool": "write", "args": { "path": "b.txt", "content": "y" } }));
        assert_eq!(r["pending"], true);
        let id = r["requestId"].as_str().unwrap().to_string();
        {
            let events = seen.lock().unwrap();
            let ev: serde_json::Value = serde_json::from_str(events.last().unwrap()).unwrap();
            assert_eq!(ev["type"], "approval_required");
            assert_eq!(ev["requestId"], id.as_str());
        }

        // respond → resolver 注入 allow；always 降 write 基线并持久化
        respond(&id, "always").unwrap();
        {
            let injected = injected.lock().unwrap();
            assert_eq!(injected.len(), 1);
            assert_eq!(injected[0], (id.clone(), "allow".to_string()));
        }
        assert_eq!(POLICY.get().unwrap().lock().unwrap().write, "auto");
        assert!(std::fs::read_to_string(dir.join("policy.json"))
            .unwrap()
            .contains("\"auto\""));

        // 基线 auto 后 write 直接放行；mkdir（ASK_TOOLS）在 auto 下也放行
        let r = request(&json!({ "tool": "write", "args": { "path": "c.txt", "content": "z" } }));
        assert_eq!(r["decision"], "allow");
        let r = request(&json!({ "tool": "mkdir", "args": { "path": "d" } }));
        assert_eq!(r["decision"], "allow");

        // 只读工具永不审批
        let r = request(&json!({ "tool": "read", "args": { "path": "a.txt" } }));
        assert_eq!(r["decision"], "allow");

        // MCP 工具默认 ask：pending 后 respond always —— 放行但不降基线
        let r = request(&json!({ "tool": "mcp__srv__echo", "args": { "q": "z" } }));
        assert_eq!(r["pending"], true);
        let id = r["requestId"].as_str().unwrap().to_string();
        respond(&id, "always").unwrap();
        {
            let injected = injected.lock().unwrap();
            assert_eq!(injected.last().unwrap().1, "allow");
        }
        assert_eq!(POLICY.get().unwrap().lock().unwrap().write, "auto");

        // 重复 respond 同一 id → 已消费，报错
        assert!(respond(&id, "allow").is_err());

        // ── D14：脚本执行（run_js）的审批语义 ──────────────────────────
        //
        // 这几条断言必须**复用上面这份 harness**（configure / event_sink /
        // resolver 全是 OnceLock，另起一个 #[test] 去抢全局单例会与本用例互踩，
        // 表现为随机失败）。

        // 前面的用例已把基线降成 auto；先显式归位——否则下面的「run_js 的
        // always 不改动基线」就恒真了，等于没测。
        policy_set("ask").unwrap();

        // (a) needs 里带永不可授予的能力 → 必须在展示审批卡之前就拒
        let before = seen.lock().unwrap().len();
        let r = request(&json!({
            "tool": "run_js",
            "args": { "code": "x", "needs": ["native:contacts", "creds_get"] }
        }));
        assert_eq!(r["decision"], "deny");
        assert!(r["reason"]
            .as_str()
            .unwrap()
            .contains("can never be granted"));
        assert_eq!(seen.lock().unwrap().len(), before, "不该弹卡");

        // (b) 合法 needs → 弹卡，卡上带着**用户实际要批准的那份清单**
        let r = request(&json!({
            "tool": "run_js",
            "args": { "code": "x", "needs": ["fs:read", "net"] }
        }));
        assert_eq!(r["pending"], true);
        let sid = r["requestId"].as_str().unwrap().to_string();
        {
            let events = seen.lock().unwrap();
            assert!(events.len() > before, "应发出 approval_required");
            let ev: serde_json::Value = serde_json::from_str(events.last().unwrap()).unwrap();
            let caps: Vec<&str> = ev["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            assert_eq!(caps, vec!["fs:read", "net"]);
            // 代码也要给用户看（审一个看不见的脚本没意义），并带 script 标志
            assert_eq!(ev["code"], "x");
            assert_eq!(ev["script"], true);
        }

        // (c) 即使基线是 auto，run_js 也必须走审批（不能因为 write 都放行了
        //     就顺带把「跑任意脚本」也放行）
        policy_set("auto").unwrap();
        let r = request(&json!({
            "tool": "run_js",
            "args": { "code": "y", "needs": [] }
        }));
        assert_eq!(r["pending"], true, "基线 auto 时 run_js 仍应弹卡");
        if let Some(i) = r["requestId"].as_str() {
            let _ = respond(i, "deny");
        }

        // (d) 关键：脚本的 always **不得**降全局基线。脚本的授权语义是「这份
        //     能力清单」，降成基线等于永久交出任意能力。
        policy_set("ask").unwrap();
        respond(&sid, "always").unwrap();
        assert_eq!(
            POLICY.get().unwrap().lock().unwrap().write,
            "ask",
            "run_js 的 always 不得降全局基线"
        );
        // 而普通 write 仍按基线行事
        let r = request(&json!({ "tool": "write", "args": { "path": "z", "content": "z" } }));
        assert_eq!(r["pending"], true, "write 应仍走 ask");
        if let Some(i) = r["requestId"].as_str() {
            let _ = respond(i, "deny");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unified_diff_shows_add_and_remove() {
        let d = unified_diff("t.txt", "line1\nline2\n", "line1\nchanged\nline3\n");
        assert!(d.contains("-line2"));
        assert!(d.contains("+changed"));
        assert!(d.contains("+line3"));
    }
}
