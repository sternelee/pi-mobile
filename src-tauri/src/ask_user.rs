//! ask_user —— pi-ask-user 插件的移动原生化（扩展能力层 #1）。
//!
//! 原 npm:pi-ask-user 的交互层是 pi-tui 终端 UI，无法在嵌入式 WebView 环境
//! 运行；这里保持工具语义（schema 见 agent-main.js，对齐上游）并把提问 UI
//! 换成宿主事件 → WebView 提问卡。模型视角与桌面 pi 一致：调用 ask_user →
//! 用户作答 → 工具结果返回回答文本。
//!
//! 流程：bundle ask_user 工具 execute → `ask_user` hostcall（阻塞）→ 本模块
//! emit `ask_user` 事件 → UI `ask_user_respond` 命令回填 → hostcall 返回
//! `{ response: {...} }`（cancelled 时 response 为 null）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TIMEOUT: Duration = Duration::from_secs(600);

static EVENT_SINK: OnceLock<Box<dyn Fn(&str) + Send + Sync>> = OnceLock::new();
static PENDING: OnceLock<Mutex<HashMap<String, Sender<String>>>> = OnceLock::new();
static SEQ: AtomicU64 = AtomicU64::new(0);

pub fn set_event_sink(f: impl Fn(&str) + Send + Sync + 'static) {
    EVENT_SINK.set(Box::new(f)).ok();
}

/// 审批请求入口（loopback dispatch 调用）。payload 原样转发给 UI。
pub fn request(payload: &serde_json::Value) -> serde_json::Value {
    let id = format!(
        "ask_{:x}_{:x}",
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
        pending.lock().unwrap().remove(&id);
        return serde_json::json!({ "response": null, "reason": "no ui attached" });
    }
    if let Some(sink) = EVENT_SINK.get() {
        sink(
            &serde_json::json!({
                "type": "ask_user",
                "requestId": id,
                "question": payload.get("question").cloned().unwrap_or(serde_json::json!("")),
                "context": payload.get("context").cloned().unwrap_or(serde_json::Value::Null),
                "options": payload.get("options").cloned().unwrap_or(serde_json::json!([])),
                "allowMultiple": payload.get("allowMultiple").cloned().unwrap_or(serde_json::json!(false)),
                "allowFreeform": payload.get("allowFreeform").cloned().unwrap_or(serde_json::json!(true)),
                "allowComment": payload.get("allowComment").cloned().unwrap_or(serde_json::json!(false)),
            })
            .to_string(),
        );
    }

    match rx.recv_timeout(TIMEOUT) {
        Ok(answer) => serde_json::from_str::<serde_json::Value>(&answer)
            .unwrap_or(serde_json::json!({ "response": null, "reason": "bad answer" })),
        Err(_) => {
            pending.lock().unwrap().remove(&id);
            serde_json::json!({ "response": null, "reason": "timeout" })
        }
    }
}

/// UI 决策回填（Tauri 命令调用）。answer_json 为 `{response: ...}` JSON。
pub fn respond(request_id: &str, answer_json: &str) -> Result<(), String> {
    if serde_json::from_str::<serde_json::Value>(answer_json).is_err() {
        return Err("answer must be valid JSON".into());
    }
    let sender = PENDING
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .remove(request_id)
        .ok_or_else(|| format!("unknown or resolved request: {request_id}"))?;
    sender
        .send(answer_json.to_string())
        .map_err(|e| format!("send answer: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ask_user_respond_roundtrip_and_cancel() {
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("pi-ask-test"));
        // 无 UI：立即返回 cancelled
        let r = request(&json!({ "question": "q?" }));
        assert_eq!(r["response"], serde_json::Value::Null);

        // 有 UI：respond 回填答案
        set_event_sink(|_| {});
        let h = std::thread::spawn(|| request(&json!({ "question": "pick one", "options": [{"title": "A"}, {"title": "B"}] })));
        std::thread::sleep(Duration::from_millis(100));
        let id = PENDING
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .keys()
            .next()
            .cloned()
            .unwrap();
        respond(
            &id,
            &json!({ "response": { "kind": "selection", "selections": ["B"] } }).to_string(),
        )
        .unwrap();
        let r = h.join().unwrap();
        assert_eq!(r["response"]["kind"], "selection");
        assert_eq!(r["response"]["selections"][0], "B");

        // 取消路径：response null + cancelled
        let h = std::thread::spawn(|| request(&json!({ "question": "again" })));
        std::thread::sleep(Duration::from_millis(100));
        let id = PENDING
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .keys()
            .next()
            .cloned()
            .unwrap();
        respond(&id, &json!({ "response": null, "cancelled": true }).to_string()).unwrap();
        let r = h.join().unwrap();
        assert_eq!(r["response"], serde_json::Value::Null);
        assert_eq!(r["cancelled"], true);
    }
}
