//! 工具审批 —— 分档策略 + 异步握手 + 终端决策源。
//!
//! 与本机 `src-tauri/src/approval.rs` 的关系：**分档表照抄**（同一个信任判断，
//! 三条档位的理由见那边注释），但流程形状按 QuickJS 这条路线重写了三点：
//!
//! 1. **分档由 Rust 持有并强制**，JS 侧没有策略。工具执行的**唯一**入口是
//!    `host.callTool(callId, …)`，它要求该 callId 先完成审批握手 —— 没握手就拒绝，
//!    与档位无关。这样「JS 忘了问」或「JS 被改写后故意不问」都执行不了
//!    （同 D14「边界强制在 Rust 侧」的思路）。
//! 2. **决策源可换**：本 spike 是终端（stdin），App 里是 WebView 经 Tauri 命令。
//!    协议一样（`approval_request` 事件出去、`approval_decision` 事件回来），
//!    换的只是谁回答 —— 所以这条链路在 spike 上验证过的部分可以直接搬。
//! 3. **不阻塞 guest**：提示在独立线程上读 stdin，guest 的 tick 循环照常跑
//!    （真机上这正是 M1 踩过的坑：在 VM 线程上等 I/O 会死锁）。

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::guest::Sink;

/// diff 回传长度上限（与 approval.rs 同口径：防超大文件冲垮事件流）。
const MAX_DIFF_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// 只读或无副作用：不打扰用户（但仍要走过握手，见模块头 ①）。
    Auto,
    /// 改用户已有内容：每次问，但可以被「always」降级。
    Ask,
    /// **永不降级**：删除不可逆，且卡片没法把「会失去什么」讲清楚。
    AlwaysAsk,
}

/// 分档表 —— 与 `src-tauri/src/approval.rs` 的 ASK/ALWAYS_ASK 保持一致
/// （这里只有 spike 已暴露的工具；多出来的名字留着给后续对齐用）。
pub fn tier_for(tool: &str) -> Tier {
    const ASK: &[&str] = &["write", "edit", "mkdir", "bash", "git_commit"];
    const ALWAYS_ASK: &[&str] = &["git_pull", "rm"];
    if ALWAYS_ASK.contains(&tool) {
        Tier::AlwaysAsk
    } else if ASK.contains(&tool) {
        Tier::Ask
    } else {
        Tier::Auto
    }
}

/// 谁来决定（CLI 的几种跑法）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionSource {
    /// 交互：在终端提问。
    Prompt,
    /// `--yes`：全部放行（自动化验证用；**仍然走完整握手**）。
    ApproveAll,
    /// `--deny`：全部拒绝（验证拒绝路径与「拒绝后 agent 怎么收场」）。
    DenyAll,
    /// `--delay-approval <ms>`：延迟这么久再放行，**不读 stdin**。
    ///
    /// 专门用来验证「等待期间 guest 没被阻塞」：管道输入是瞬时回答，等待窗口只有
    /// 微秒级，主循环的 2ms 拍子撞不上（第一次跑就是这样，`ticks while waiting`
    /// 是 0，看不出任何东西）。用固定延迟把窗口撑开到可观测。
    Delay(u64),
}

/// 终端上的四种回答。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    Allow,
    Always,
    Deny,
    DenyAll,
}

#[derive(Default)]
struct State {
    /// callId → 决策。**执行权的唯一凭据**。
    grants: HashMap<String, String>,
    /// 被「always」降级为 auto 的工具（持久化到 policy.json）。
    downgraded: HashSet<String>,
}

pub struct Approvals {
    workspace: PathBuf,
    policy_path: PathBuf,
    source: DecisionSource,
    sink: Arc<Sink>,
    state: Mutex<State>,
    next_id: AtomicI64,
}

impl Approvals {
    pub fn new(
        workspace: impl Into<PathBuf>,
        data_dir: impl Into<PathBuf>,
        source: DecisionSource,
        sink: Arc<Sink>,
    ) -> Self {
        let policy_path = data_dir.into().join("policy.json");
        let downgraded = std::fs::read_to_string(&policy_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
            .map(|list| list.into_iter().collect())
            .unwrap_or_default();
        Self {
            workspace: workspace.into(),
            policy_path,
            source,
            sink,
            state: Mutex::new(State {
                grants: HashMap::new(),
                downgraded,
            }),
            next_id: AtomicI64::new(0),
        }
    }

    /// 有效档位：AlwaysAsk 永不被 always 降级。
    fn effective_tier(&self, tool: &str) -> Tier {
        let tier = tier_for(tool);
        if tier == Tier::AlwaysAsk {
            return tier;
        }
        if self.state.lock().unwrap().downgraded.contains(tool) {
            Tier::Auto
        } else {
            tier
        }
    }

    /// JS 侧在每次工具调用前调用：**立即**返回 approval id，结果经事件回合。
    ///
    /// id 只是让 JS 把 `approval_decision` 对上号；**执行权**记在 callId 上
    /// （`ensure_granted`），所以 ID 被伪造也没用。
    pub fn request(self: &Arc<Self>, call_id: &str, tool: &str, args: &Value) -> i64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let tier = self.effective_tier(tool);

        if tier == Tier::Auto {
            self.settle(id, call_id, tool, "allow", "auto");
            return id;
        }

        let (summary, diff) = self.preview(tool, args);
        // 先让 guest 看见「有个审批在等」—— App 里这就是 UI 弹卡的那一刻。
        self.sink.push(json!({
            "type": "approval_request", "id": id, "callId": call_id, "tool": tool,
            "tier": if tier == Tier::AlwaysAsk { "always_ask" } else { "ask" },
            "summary": summary, "diff": diff,
        }));
        self.sink.set_pending_approval(true);

        match self.source {
            DecisionSource::ApproveAll => self.settle(id, call_id, tool, "allow", "flag --yes"),
            DecisionSource::DenyAll => self.settle(id, call_id, tool, "deny", "flag --deny"),
            DecisionSource::Delay(millis) => {
                let me = Arc::clone(self);
                let (call_id, tool) = (call_id.to_string(), tool.to_string());
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(millis));
                    me.settle(id, &call_id, &tool, "allow", "delayed");
                });
            }
            DecisionSource::Prompt => {
                // 关键：提示在**独立线程**上读 stdin，guest 的 tick 循环不停。
                let me = Arc::clone(self);
                let (call_id, tool) = (call_id.to_string(), tool.to_string());
                std::thread::spawn(move || {
                    // 非交互环境（管道/CI）读不到输入 → 按拒绝处理，别静默放行。
                    let answer = prompt_in_terminal(&tool, &summary, &diff).unwrap_or(Answer::Deny);
                    if answer == Answer::Always {
                        {
                            let mut state = me.state.lock().unwrap();
                            state.downgraded.insert(tool.clone());
                            let _ = persist_policy(&me.policy_path, &state.downgraded);
                        }
                        println!("  ↳ 「always」已持久化：{tool} 之后不再询问（rm/pull 永不降级）");
                    }
                    let (verdict, reason) = match answer {
                        Answer::Allow => ("allow", "user"),
                        Answer::Always => ("allow", "user:always"),
                        Answer::Deny => ("deny", "user"),
                        Answer::DenyAll => ("deny", "user:deny-all"),
                    };
                    me.settle(id, &call_id, &tool, verdict, reason);
                });
            }
        }
        id
    }

    /// 执行权检查：`host.callTool` 在跑任何工具前调用。
    pub fn ensure_granted(&self, call_id: &str, tool: &str) -> Result<(), String> {
        let state = self.state.lock().unwrap();
        match state.grants.get(call_id) {
            Some(decision) if decision == "allow" => Ok(()),
            Some(_) => Err(format!(
                "{tool}: denied by the user — do not retry it as-is"
            )),
            None => Err(format!(
                "{tool}: no approval handshake for this call — the host refuses to execute it"
            )),
        }
    }

    /// 记决策 + 回事件（唯一的收口，四条路径都走这里）。
    fn settle(&self, id: i64, call_id: &str, tool: &str, decision: &str, reason: &str) {
        self.state
            .lock()
            .unwrap()
            .grants
            .insert(call_id.to_string(), decision.to_string());
        self.sink.set_pending_approval(false);
        self.sink.push(json!({
            "type": "approval_decision", "id": id, "callId": call_id, "tool": tool,
            "decision": decision, "reason": reason,
        }));
    }

    /// 这次调用**会改变什么** —— 给用户看的摘要与 diff。
    ///
    /// 读的是 workspace 里的当前内容（审批发生在执行前），与 approval.rs 同策略：
    /// 写/改给统一 diff，删除给「会失去什么」的清单。
    fn preview(&self, tool: &str, args: &Value) -> (String, String) {
        let rel = args["path"].as_str().unwrap_or("");
        let full = self.workspace.join(rel);
        match tool {
            "write" => {
                let new = args["content"].as_str().unwrap_or("");
                let old = std::fs::read_to_string(&full).unwrap_or_default();
                if old.is_empty() && !full.exists() {
                    (format!("create {rel} ({} bytes)", new.len()), String::new())
                } else {
                    (
                        format!("overwrite {rel} ({} → {} bytes)", old.len(), new.len()),
                        unified_diff(&old, new),
                    )
                }
            }
            "edit" => {
                let old = std::fs::read_to_string(&full).unwrap_or_default();
                let (from, to) = (
                    args["oldText"].as_str().unwrap_or(""),
                    args["newText"].as_str().unwrap_or(""),
                );
                let updated = match pi_host_tools::apply_edit(
                    &old,
                    from,
                    to,
                    args["replaceAll"].as_bool().unwrap_or(false),
                ) {
                    Ok(text) => text,
                    // edit 非法（找不到/多处命中）时预览不到 diff，让用户看到原因
                    Err(error) => return (format!("edit {rel} — {error}"), String::new()),
                };
                (format!("edit {rel}"), unified_diff(&old, &updated))
            }
            "rm" => {
                let (mut files, mut dirs) = (0usize, 0usize);
                let mut bytes = 0u64;
                count_tree(&full, &mut files, &mut dirs, &mut bytes);
                let recursive = args["recursive"].as_bool().unwrap_or(false);
                (
                    format!(
                        "delete {rel} — {files} file(s) in {dirs} dir(s), {bytes} bytes{}{}",
                        if recursive { "" } else { " (not recursive)" },
                        if full.is_dir() { " — directory" } else { "" }
                    ),
                    String::new(),
                )
            }
            other => (format!("{other} {rel}"), String::new()),
        }
    }
}

/// 终端提问。返回 None = 读不到输入（非交互）。
fn prompt_in_terminal(tool: &str, summary: &str, diff: &str) -> Option<Answer> {
    println!("\n┌─ approval required ────────────────────────────────────");
    println!("│ tool    {tool}");
    println!("│ change  {summary}");
    if !diff.is_empty() {
        for line in diff.lines().take(40) {
            println!("│ {line}");
        }
        if diff.lines().count() > 40 {
            println!("│ … (diff truncated in display)");
        }
    }
    print!("└─ allow? [y]es / [n]o / [a]lways / [d]eny-all: ");
    std::io::stdout().flush().ok()?;

    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).ok()? == 0 {
        println!("(no input — denying)");
        return None;
    }
    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(Answer::Allow),
        "a" | "always" => Some(Answer::Always),
        "d" | "deny-all" => Some(Answer::DenyAll),
        _ => Some(Answer::Deny),
    }
}

/// 统一 diff（与 approval.rs 同款：similar 的 unified 渲染，超长截断）。
fn unified_diff(old: &str, new: &str) -> String {
    if old == new {
        return String::new();
    }
    let diff = similar::TextDiff::from_lines(old, new);
    let rendered = diff
        .unified_diff()
        .context_radius(2)
        .header("before", "after")
        .to_string();
    if rendered.len() <= MAX_DIFF_BYTES {
        return rendered;
    }
    let mut cut = MAX_DIFF_BYTES;
    while !rendered.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}\n… (diff truncated at {MAX_DIFF_BYTES} bytes)",
        &rendered[..cut]
    )
}

fn count_tree(path: &std::path::Path, files: &mut usize, dirs: &mut usize, bytes: &mut u64) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if meta.is_dir() {
        *dirs += 1;
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                count_tree(&entry.path(), files, dirs, bytes);
            }
        }
    } else {
        *files += 1;
        *bytes += meta.len();
    }
}

fn persist_policy(path: &std::path::Path, downgraded: &HashSet<String>) -> Result<(), String> {
    let mut list: Vec<&String> = downgraded.iter().collect();
    list.sort();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("policy dir: {e}"))?;
    }
    std::fs::write(
        path,
        serde_json::to_string(&list).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("policy write: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_match_the_tauri_host() {
        assert_eq!(tier_for("read"), Tier::Auto);
        assert_eq!(tier_for("ls"), Tier::Auto);
        assert_eq!(tier_for("grep"), Tier::Auto);
        assert_eq!(tier_for("write"), Tier::Ask);
        assert_eq!(tier_for("edit"), Tier::Ask);
        assert_eq!(tier_for("mkdir"), Tier::Ask);
        assert_eq!(tier_for("rm"), Tier::AlwaysAsk);
        assert_eq!(tier_for("git_pull"), Tier::AlwaysAsk);
    }

    #[test]
    fn always_ask_tools_are_never_downgraded() {
        let dir = std::env::temp_dir().join(format!("pi-approval-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sink = Arc::new(Sink::new(crate::deepseek::DeepSeekConfig {
            api_key: "x".into(),
            base_url: "http://127.0.0.1:1".into(),
        }));
        let approvals = Arc::new(Approvals::new(
            dir.join("ws"),
            &dir,
            DecisionSource::ApproveAll,
            sink.clone(),
        ));

        // write 走一次 always → 降级为 auto；rm 在同一次里也必须仍是 always_ask
        approvals
            .state
            .lock()
            .unwrap()
            .downgraded
            .insert("write".into());
        approvals
            .state
            .lock()
            .unwrap()
            .downgraded
            .insert("rm".into());
        assert_eq!(approvals.effective_tier("write"), Tier::Auto);
        assert_eq!(
            approvals.effective_tier("rm"),
            Tier::AlwaysAsk,
            "rm 被写了 always 也必须保持 always_ask"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn execution_requires_a_handshake() {
        let dir =
            std::env::temp_dir().join(format!("pi-approval-handshake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sink = Arc::new(Sink::new(crate::deepseek::DeepSeekConfig {
            api_key: "x".into(),
            base_url: "http://127.0.0.1:1".into(),
        }));

        let deny = Arc::new(Approvals::new(
            dir.join("ws"),
            &dir,
            DecisionSource::DenyAll,
            sink.clone(),
        ));
        deny.request(
            "call-1",
            "write",
            &json!({ "path": "a.txt", "content": "hi" }),
        );
        assert!(
            deny.ensure_granted("call-1", "write").is_err(),
            "拒绝后不得执行"
        );
        assert!(
            deny.ensure_granted("call-2", "write").is_err(),
            "没握手的 callId 不得执行"
        );

        let allow = Arc::new(Approvals::new(
            dir.join("ws"),
            &dir,
            DecisionSource::ApproveAll,
            sink.clone(),
        ));
        allow.request(
            "call-3",
            "write",
            &json!({ "path": "a.txt", "content": "hi" }),
        );
        assert!(allow.ensure_granted("call-3", "write").is_ok());
        // auto 档也要走握手（模块头 ①）：没 request 过就是不能执行
        assert!(allow.ensure_granted("call-4", "read").is_err());
        allow.request("call-4", "read", &json!({ "path": "a.txt" }));
        assert!(allow.ensure_granted("call-4", "read").is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diff_is_truncated_at_the_cap() {
        let old = "a\n".repeat(4000);
        let new = "b\n".repeat(4000);
        let rendered = unified_diff(&old, &new);
        assert!(rendered.len() <= MAX_DIFF_BYTES + 64, "{}", rendered.len());
        assert!(rendered.contains("truncated"));
    }
}
