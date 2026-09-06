//! approval —— 工具审批（M3 主线）：policy 状态机 + pending 请求表 + diff。
//!
//! 流程（PLAN D2 policy-hook 设计）：bundle 内 mutating 工具（write/edit/bash）
//! 执行前发 `approval_request` hostcall（阻塞等决策）→ 本模块查 policy：
//! `auto` 直接放行；`ask` 则 emit `approval_required` 事件给 UI（带统一 diff），
//! 挂在 channel 上等 `approval_respond` 命令回填决策，超时自动 deny。
//! loopback 每连接一线程，阻塞一个 hostcall 不影响其他通道。
//!
//! policy 持久化在 `{data_dir}/policy.json`（M3 基线只有 write 一档；
//! 后续按会话/工具粒度扩展，键空间见 CONTRACTS §3）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TIMEOUT: Duration = Duration::from_secs(120);
/// 走审批的工具集（D6：Android bash 暂未注册，保持集合完整以便后续）。
const ASK_TOOLS: &[&str] = &["write", "edit", "bash"];
/// diff 回传 UI 的长度上限（移动端卡片展示，防超大文件拖垮事件流）。
const MAX_DIFF_BYTES: usize = 16 * 1024;

static EVENT_SINK: OnceLock<Box<dyn Fn(&str) + Send + Sync>> = OnceLock::new();
static PENDING: OnceLock<Mutex<HashMap<String, Sender<String>>>> = OnceLock::new();
static POLICY: OnceLock<Mutex<Policy>> = OnceLock::new();
static DATA_DIR: OnceLock<String> = OnceLock::new();
static SEQ: AtomicU64 = AtomicU64::new(0);

/// 注册 UI 事件转发（与 loopback 事件同一 `pi-agent-event` 通道）。
pub fn set_event_sink(f: impl Fn(&str) + Send + Sync + 'static) {
    EVENT_SINK.set(Box::new(f)).ok();
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

/// 审批请求入口（loopback dispatch 调用）。返回 hostcall 应答。
pub fn request(payload: &serde_json::Value) -> serde_json::Value {
    let tool = payload.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    // D11：MCP 工具（mcp__<server>__<tool>）默认全部 ask，不受 write 基线
    // 影响（per-server 降 auto 见 D11 完整版）；ASK_TOOLS 里的宿主工具
    // （write/edit/bash）仍走 policy 状态机。
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
    let (tx, rx) = channel::<String>();
    let pending = PENDING.get_or_init(|| Mutex::new(HashMap::new()));
    pending.lock().unwrap().insert(id.clone(), tx);

    if EVENT_SINK.get().is_none() {
        // 无 UI（单测/无头）：立即拒绝并清表，避免 stale entry
        pending.lock().unwrap().remove(&id);
        return serde_json::json!({ "decision": "deny", "reason": "no ui attached" });
    }
    if let Some(sink) = EVENT_SINK.get() {
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
    }

    match rx.recv_timeout(TIMEOUT) {
        Ok(d) => {
            if d == "always" {
                if !is_mcp {
                    // “总是允许” = write 基线降为 auto 并持久化（M3 基线粒度）
                    if let Some(p) = POLICY.get() {
                        let mut g = p.lock().unwrap();
                        g.write = "auto".into();
                        save_policy(&g);
                    }
                    return serde_json::json!({ "decision": "allow", "policy": "always" });
                }
                // MCP 工具：放行本次，但不降 write 基线（per-server 粒度见 D11 完整版）
                return serde_json::json!({ "decision": "allow" });
            }
            serde_json::json!({ "decision": d })
        }
        Err(_) => {
            pending.lock().unwrap().remove(&id); // 超时：rx 即将析构，清表防 stale
            serde_json::json!({ "decision": "deny", "reason": "timeout" })
        }
    }
}

/// UI 决策回填（Tauri 命令调用）。decision: allow / deny / always。
pub fn respond(request_id: &str, decision: &str) -> Result<(), String> {
    if !matches!(decision, "allow" | "deny" | "always") {
        return Err(format!("invalid decision: {decision}"));
    }
    let sender = PENDING
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .remove(request_id)
        .ok_or_else(|| format!("unknown or resolved request: {request_id}"))?;
    sender
        .send(decision.to_string())
        .map_err(|e| format!("send decision: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn approval_ask_deny_then_always_persists_auto() {
        let dir = std::env::temp_dir().join(format!("pi-appr-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("workspace")).unwrap();
        configure(dir.to_str().unwrap());

        // 无 UI attach：ask 策略下立即 deny（不带 UI 的兜底路径）
        let r = request(&json!({ "tool": "write", "args": { "path": "a.txt", "content": "x" } }));
        assert_eq!(r["decision"], "deny");

        // MCP 工具默认 ask（D11）：进 pending，不受 write 基线影响。
        // 此处基线仍为 ask，点 always —— 验证 MCP 上的 always 不降 write 基线。
        set_event_sink(|_| {});
        let h = std::thread::spawn(|| {
            request(&json!({ "tool": "mcp__srv__echo", "args": { "q": "y" } }))
        });
        std::thread::sleep(Duration::from_millis(100));
        let pending = PENDING
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .keys()
            .next()
            .cloned()
            .unwrap();
        respond(&pending, "always").unwrap();
        let r = h.join().unwrap();
        assert_eq!(r["decision"], "allow");
        let saved = std::fs::read_to_string(dir.join("policy.json")).ok();
        assert!(
            !saved.as_deref().unwrap_or_default().contains("\"auto\""),
            "MCP always must not demote write baseline, policy.json: {saved:?}"
        );

        // 挂上 UI sink → ask 策略走 pending/respond 全流程
        let h = std::thread::spawn(|| {
            request(&json!({ "tool": "write", "args": { "path": "b.txt", "content": "y" } }))
        });
        std::thread::sleep(Duration::from_millis(100));
        // 从 sink 收不到（吞掉了），但 pending 表里有请求 —— 用 respond 测 always
        let pending = PENDING
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .keys()
            .next()
            .cloned()
            .unwrap();
        respond(&pending, "always").unwrap();
        let r = h.join().unwrap();
        assert_eq!(r["decision"], "allow");
        assert_eq!(r["policy"], "always");

        // always 后 write 基线已降为 auto：直接放行，不再进 pending
        let r = request(&json!({ "tool": "write", "args": { "path": "c.txt", "content": "z" } }));
        assert_eq!(r["decision"], "allow");
        assert_eq!(r["policy"], "auto");

        // policy.json 已持久化 auto
        let saved =
            std::fs::read_to_string(dir.join("policy.json")).unwrap();
        assert!(saved.contains("\"auto\""));

        // 只读工具永不审批
        let r = request(&json!({ "tool": "read", "args": { "path": "a.txt" } }));
        assert_eq!(r["decision"], "allow");

        // write 基线已降为 auto 后，MCP 工具依旧 ask（不受基线影响）
        let h = std::thread::spawn(|| {
            request(&json!({ "tool": "mcp__srv__echo", "args": { "q": "z" } }))
        });
        std::thread::sleep(Duration::from_millis(100));
        let pending = PENDING
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .keys()
            .next()
            .cloned()
            .unwrap();
        respond(&pending, "allow").unwrap();
        let r = h.join().unwrap();
        assert_eq!(r["decision"], "allow");

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
