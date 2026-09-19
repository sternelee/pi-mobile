//! ask_user —— agent 反问用户（pi-ask-user 移动原生化的 CLI 形态）。
//!
//! 与本机 `src-tauri/src/ask_user.rs` 的**契约相同**：注册 → emit `ask_user` 事件
//! （带 question/context/options/allowMultiple/allowFreeform/allowComment）→
//! UI 作答 → 经事件把答案送回。这里只换决策源（终端 stdin 而非 WebView）。
//!
//! 与审批的分工：审批回答「准不准」，ask_user 回答「要哪个」。两者共用同一套
//! 「id + 事件出去、答案事件回来」的异步骨架 —— 所以 guest 在等答案期间照常跑
//! （真机上就是「等用户输入时 UI 不冻结」）。

use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::approval::DecisionSource;
use crate::guest::Sink;

pub struct AskUser {
    source: DecisionSource,
    sink: Arc<Sink>,
    next_id: AtomicI64,
    /// 未作答的请求（--deny 时要一次性取消）。
    pending: Mutex<Vec<i64>>,
}

impl AskUser {
    pub fn new(source: DecisionSource, sink: Arc<Sink>) -> Self {
        Self {
            source,
            sink,
            next_id: AtomicI64::new(0),
            pending: Mutex::new(Vec::new()),
        }
    }

    /// JS 侧调用：**立即**返回请求 id，答案经事件回合（与审批同一个模式）。
    pub fn register(self: &Arc<Self>, payload: &Value) -> i64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let question = payload["question"].as_str().unwrap_or("").to_string();
        let context = payload["context"].as_str().unwrap_or("").to_string();
        let options: Vec<String> = payload["options"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let allow_freeform = payload["allowFreeform"].as_bool().unwrap_or(true);

        // 让 guest 看见「有个问题在等」（App 里这就是弹卡那一刻）
        self.sink.push(json!({
            "type": "ask_user", "id": id, "question": question, "context": context,
            "options": options, "allowFreeform": allow_freeform,
        }));
        self.pending.lock().unwrap().push(id);

        match self.source {
            DecisionSource::ApproveAll => {
                // 无人值守：选第一项（有的话），否则一个明确的占位回答
                let answer = options
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "(unattended: no preference)".to_string());
                self.settle(id, &answer, "flag --yes");
            }
            DecisionSource::DenyAll => self.cancel(id, "flag --deny"),
            DecisionSource::Delay(millis) => {
                let me = Arc::clone(self);
                let answer = options
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "(unattended: no preference)".to_string());
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(millis));
                    me.settle(id, &answer, "delayed");
                });
            }
            DecisionSource::Prompt => {
                let me = Arc::clone(self);
                std::thread::spawn(move || {
                    match prompt_in_terminal(&question, &context, &options, allow_freeform) {
                        Some(answer) => me.settle(id, &answer, "user"),
                        None => me.cancel(id, "no input"),
                    }
                });
            }
        }
        id
    }

    fn settle(&self, id: i64, answer: &str, reason: &str) {
        self.pending.lock().unwrap().retain(|p| *p != id);
        self.sink.push(json!({
            "type": "ask_user_decision", "id": id, "answer": answer, "cancelled": false, "reason": reason,
        }));
    }

    fn cancel(&self, id: i64, reason: &str) {
        self.pending.lock().unwrap().retain(|p| *p != id);
        self.sink.push(json!({
            "type": "ask_user_decision", "id": id, "answer": Value::Null, "cancelled": true, "reason": reason,
        }));
    }
}

/// 终端提问。返回 None = 读不到输入（管道/EOF）。
fn prompt_in_terminal(
    question: &str,
    context: &str,
    options: &[String],
    allow_freeform: bool,
) -> Option<String> {
    println!("\n┌─ agent 提问 ───────────────────────────────────────────");
    if !context.trim().is_empty() {
        for line in context.lines().take(12) {
            println!("│ {line}");
        }
        println!("│");
    }
    println!("│ {question}");
    for (index, option) in options.iter().enumerate() {
        println!("│   {}) {option}", index + 1);
    }
    let hint = if options.is_empty() {
        "回答".to_string()
    } else if allow_freeform {
        format!("1-{} 或直接输入文字", options.len())
    } else {
        format!("1-{}", options.len())
    };
    print!("└─ {hint}: ");
    std::io::stdout().flush().ok()?;

    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line).ok()?;
    let answer = line.trim();
    if read == 0 || answer.is_empty() {
        println!("(no input — cancelling)");
        return None;
    }
    if let Ok(pick) = answer.parse::<usize>() {
        if pick >= 1 && pick <= options.len() {
            return Some(options[pick - 1].clone());
        }
    }
    if !options.is_empty() && !allow_freeform {
        // 给了选项且不允许自由输入：非编号的回答视为无效，退回第一项（明确、可预期）
        println!("(无效选择，按第一项处理)");
        return Some(options[0].clone());
    }
    Some(answer.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deepseek::DeepSeekConfig;

    fn sink() -> Arc<Sink> {
        Arc::new(Sink::new(DeepSeekConfig {
            api_key: "x".into(),
            base_url: "http://127.0.0.1:1".into(),
        }))
    }

    #[test]
    fn unattended_answer_picks_the_first_option() {
        let sink = sink();
        let asks = Arc::new(AskUser::new(DecisionSource::ApproveAll, sink.clone()));
        asks.register(&json!({ "question": "which one?", "options": ["alpha", "beta"] }));
        let batch = sink.drain();
        let decision = batch
            .iter()
            .find(|e| e["type"] == "ask_user_decision")
            .expect("应有决策事件");
        assert_eq!(decision["answer"], "alpha");
        assert_eq!(decision["cancelled"], false);
    }

    #[test]
    fn deny_mode_cancels() {
        let sink = sink();
        let asks = Arc::new(AskUser::new(DecisionSource::DenyAll, sink.clone()));
        asks.register(&json!({ "question": "?", "options": ["a"] }));
        let batch = sink.drain();
        let decision = batch
            .iter()
            .find(|e| e["type"] == "ask_user_decision")
            .expect("应有决策事件");
        assert_eq!(decision["cancelled"], true);
    }
}
