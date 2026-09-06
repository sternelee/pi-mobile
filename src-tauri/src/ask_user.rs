//! ask_user —— pi-ask-user 插件的移动原生化（扩展能力层 #1）。
//!
//! 原 npm:pi-ask-user 的交互层是 pi-tui 终端 UI，无法在嵌入式 WebView 环境
//! 运行；这里保持工具语义（schema 见 agent-main.js，对齐上游）并把提问 UI
//! 换成宿主事件 → WebView 提问卡。模型视角与桌面 pi 一致：调用 ask_user →
//! 用户作答 → 工具结果返回回答文本。
//!
//! 通信为 kick+事件注入模式（真机实测：长挂起 fetch + AbortSignal 会触发
//! 嵌入 bun 的 HeapHelper 线程 SIGSEGV，禁止长阻塞 hostcall）：
//! 1. bundle 工具 execute → `ask_user_register` hostcall（立即返回 id）
//! 2. 本模块 emit `ask_user` 事件 → WebView 提问卡
//! 3. 用户作答 → `ask_user_respond` 命令 → resolver（pi_bun 注入的
//!    skal_evaluate 调 `__pi_ask_resolve(id, answer)`）反向解析 pending
//!    promise → 工具 continuation 由 waitForPromise 泵动

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static EVENT_SINK: OnceLock<Box<dyn Fn(&str) + Send + Sync>> = OnceLock::new();
static RESOLVER: OnceLock<Box<dyn Fn(&str, &str) + Send + Sync>> = OnceLock::new();
static PENDING: OnceLock<Mutex<HashMap<String, ()>>> = OnceLock::new();
static SEQ: AtomicU64 = AtomicU64::new(0);

pub fn set_event_sink(f: impl Fn(&str) + Send + Sync + 'static) {
    EVENT_SINK.set(Box::new(f)).ok();
}

/// pi_bun 在 agent_init 时注入：把答案经 skal_evaluate 打回运行时。
pub fn set_resolver(f: impl Fn(&str, &str) + Send + Sync + 'static) {
    RESOLVER.set(Box::new(f)).ok();
}

/// 注册提问（`ask_user_register` hostcall）。立即返回 id，交互异步完成。
pub fn register(payload: &serde_json::Value) -> serde_json::Value {
    let id = format!(
        "ask_{:x}_{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        SEQ.fetch_add(1, Ordering::Relaxed),
    );
    PENDING
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .insert(id.clone(), ());

    let Some(sink) = EVENT_SINK.get() else {
        PENDING
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .remove(&id);
        return serde_json::json!({ "id": id, "state": "cancelled", "reason": "no ui attached" });
    };
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
    serde_json::json!({ "id": id, "state": "pending" })
}

/// UI 回填答案 → 经 resolver 注入运行时（answer_json 为 `{response: ...}`）。
pub fn respond(request_id: &str, answer_json: &str) -> Result<(), String> {
    if serde_json::from_str::<serde_json::Value>(answer_json).is_err() {
        return Err("answer must be valid JSON".into());
    }
    let existed = PENDING
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .remove(request_id)
        .is_some();
    if !existed {
        return Err(format!("unknown or resolved request: {request_id}"));
    }
    let resolver = RESOLVER
        .get()
        .ok_or_else(|| "resolver not configured (agent not initialized)".to_string())?;
    resolver(request_id, answer_json);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;

    #[test]
    fn register_emit_and_respond_injects_via_resolver() {
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("pi-ask-test"));

        // 无 UI：注册即取消
        let r = register(&json!({ "question": "q?" }));
        assert_eq!(r["state"], "cancelled");

        // 有 UI：emit 事件 → respond 经 resolver 注入（'static 闭包 → Arc 所有权）
        let seen: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let seen_sink = Arc::clone(&seen);
        set_event_sink(move |s| seen_sink.lock().unwrap().push(s.to_string()));
        let injected: Arc<StdMutex<Vec<(String, String)>>> = Arc::new(StdMutex::new(Vec::new()));
        let injected_resolver = Arc::clone(&injected);
        set_resolver(move |id, answer| {
            injected_resolver
                .lock()
                .unwrap()
                .push((id.to_string(), answer.to_string()));
        });

        let r = register(&json!({ "question": "pick one", "options": [{"title": "A"}, {"title": "B"}] }));
        assert_eq!(r["state"], "pending");
        let id = r["id"].as_str().unwrap().to_string();

        let events = seen.lock().unwrap();
        assert_eq!(events.len(), 1);
        let ev: serde_json::Value = serde_json::from_str(&events[0]).unwrap();
        assert_eq!(ev["type"], "ask_user");
        assert_eq!(ev["question"], "pick one");
        drop(events);

        let answer = json!({ "response": { "kind": "selection", "selections": ["B"] } }).to_string();
        respond(&id, &answer).unwrap();
        {
            let injected = injected.lock().unwrap();
            assert_eq!(injected.len(), 1);
            assert_eq!(injected[0].0, id);
            assert_eq!(injected[0].1, answer);
        } // guard 先释放 —— 下面的 respond 会经 resolver 拿同一把锁

        // 重复 respond 同一 id → 已消费，报错
        assert!(respond(&id, &answer).is_err());
    }
}
