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

/// 统一 diff（unified，context=2）。新文件 old 为空串。
fn unified_diff(path: &str, old: &str, new: &str) -> String {
    let diff = similar::TextDiff::from_lines(old, new);
    let mut out = diff
        .unified_diff()
        .context_radius(2)
        .header(&format!("--- {path}"), &format!("+++ {path}"))
        .to_string();

    if out.len() > MAX_DIFF_BYTES {
        out.truncate(MAX_DIFF_BYTES);
        out.push_str("\n… (diff truncated)");
    }
    out
}

/// 审批请求入口（loopback dispatch 调用）。非阻塞：ask 时 emit 事件并立即
/// 返回 pending，决策经 `__pi_approval_resolve` 注入（kick+resolve 模式）。
pub fn request(payload: &serde_json::Value) -> serde_json::Value {
    let tool = payload.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    // D11：MCP 工具（mcp__<server>__<tool>）默认全部 ask，不受 write 基线
    // 影响（per-server 降 auto 见 D11 完整版）；ASK_TOOLS 里的宿主工具
    // （write/edit/mkdir/bash）仍走 policy 状态机。
    let is_mcp = tool.starts_with("mcp__");
    let needs_ask = is_mcp
        || (ASK_TOOLS.contains(&tool)
            && POLICY
                .get()
                .map(|p| p.lock().unwrap().write == "ask")
                .unwrap_or(true));
    if !needs_ask {
        return serde_json::json!({ "decision": "allow", "policy": "auto" });
    }

    // write/edit：算 diff 随审批卡一起给 UI
    let mut diff = String::new();
    let mut path = String::new();
    if let Some(args) = payload.get("args") {
        path = args.get("path").and_then(|v| v.as_str()).unwrap_or("").into();
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
                        if let Ok(updated) =
                            crate::pi_bun::loopback::apply_edit(&content, old_text, new_text, replace_all)
                        {
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

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unified_diff_shows_add_and_remove() {
        let d = unified_diff(
            "t.txt",
            "line1\nline2\n",
            "line1\nchanged\nline3\n",
        );
        assert!(d.contains("-line2"));
        assert!(d.contains("+changed"));
        assert!(d.contains("+line3"));
    }
}
