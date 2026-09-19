//! DeepSeek 传输 —— QuickJS 路线里模型厂商协议的实现处（Rust 侧）。
//!
//! ⚠️ **这是换掉 bun 的真实代价所在**：bun 路线用 pi-ai 的 provider 目录（8 家 +
//! OAuth 订阅登录 + prompt caching），那套是 JS/SDK 重度依赖，QuickJS 里跑不了；
//! 所以 provider 传输必须在 Rust 重写。目前只有 DeepSeek 一家 —— 要让 UI 的
//! provider 列表全部可用，得一家家补（见 docs/POCKET-PI-NOTES.md §4）。
//!
//! 与 spike（spikes/quickjs-agent）里那份逐字相同。
//!
//! 对齐 `@earendil-works/pi-ai/dist/api/openai-completions.js` 的请求编码与流式
//! 解析，字段级逐一核对（2026-09-19，pi-ai 0.84.4）：
//!   · `convertMessages` → [`convert_messages`]（system/assistant/tool 三种角色）
//!   · `convertTools`    → tools 数组（`strict: false` 也照发，DeepSeek 支持）
//!   · `parseChunkUsage` → [`parse_usage`]（含 `prompt_cache_hit_tokens` 这一 DeepSeek 特有位置）
//!   · `mapStopReason`   → [`map_stop_reason`]
//!   · `thinkingFormat: "deepseek"` → 有 reasoning 时发 `thinking:{type:"enabled"}` +
//!     `reasoning_effort`，否则 `thinking:{type:"disabled"}`
//!
//! 刻意**不**做的事（spike 边界，见 README）：`transformMessages` 的 provider 归一化
//! （孤儿 toolCall 修补、连续 toolResult 合并）、prompt caching、`partial-json` 的
//! 流式半成品解析、失败重试。

use std::io::{BufRead, BufReader};
use std::time::Duration;

use serde_json::{json, Map, Value};

/// 模型流事件：只要这两类 —— 与 pocket-pi 的 `ModelStreamEvent` 同形。
#[derive(Clone, Debug)]
pub enum StreamEvent {
    Thinking(String),
    Text(String),
}

#[derive(Clone)]
pub struct DeepSeekConfig {
    pub api_key: String,
    pub base_url: String,
}

impl DeepSeekConfig {
    pub fn from_env() -> Result<Self, String> {
        let api_key = std::env::var("DEEPSEEK_API_KEY")
            .map_err(|_| "DEEPSEEK_API_KEY is not set".to_string())?;
        if api_key.trim().is_empty() {
            return Err("DEEPSEEK_API_KEY is empty".into());
        }
        Ok(Self {
            api_key,
            base_url: std::env::var("DEEPSEEK_BASE_URL")
                .unwrap_or_else(|_| "https://api.deepseek.com".into()),
        })
    }
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        // 总超时覆盖「连接 + 读完 body」，单轮对话足够；防挂死。
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| format!("http client: {e}"))
}

/// 文本块拼接（assistant 的 content 以纯字符串发给 OpenAI 兼容端点）。
fn text_of(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// `context.messages` → OpenAI Chat Completions 的 `messages`。
///
/// 三条规则与 pi-ai 一致，且都有理由：
///  1. assistant 的 `content` 发**纯字符串**（发 content-block 数组会让部分模型
///     把结构照抄进输出，pi-ai 注释里点名 DeepSeek V3.2 via NVIDIA NIM）；
///  2. `requiresReasoningContentOnAssistantMessages`（DeepSeek 为真）→ assistant
///     消息必须有 `reasoning_content` 字段，没有思考就发空串；
///  3. 无 content 且无 tool_calls 的 assistant 消息丢弃（部分端点不接受空 assistant）。
pub fn convert_messages(context: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let Some(messages) = context["messages"].as_array() else {
        return out;
    };
    for message in messages {
        match message["role"].as_str() {
            Some("user") => {
                let text = text_of(message.get("content"));
                if !text.is_empty() {
                    out.push(json!({ "role": "user", "content": text }));
                }
            }
            Some("assistant") => {
                let text = text_of(message.get("content"));
                let thinking = message["content"]
                    .as_array()
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter(|b| b["type"] == "thinking")
                            .filter_map(|b| b["thinking"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let tool_calls: Vec<Value> = message["content"]
                    .as_array()
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter(|b| b["type"] == "toolCall")
                            .map(|b| {
                                json!({
                                    "id": b["id"],
                                    "type": "function",
                                    "function": {
                                        "name": b["name"],
                                        "arguments": b["arguments"].to_string(),
                                    }
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                let mut entry = Map::new();
                entry.insert("role".into(), json!("assistant"));
                entry.insert(
                    "content".into(),
                    if text.is_empty() {
                        Value::Null
                    } else {
                        json!(text)
                    },
                );
                // DeepSeek：assistant 必须带 reasoning_content（无则空串）
                entry.insert(
                    "reasoning_content".into(),
                    json!(if thinking.is_empty() {
                        String::new()
                    } else {
                        thinking
                    }),
                );
                if !tool_calls.is_empty() {
                    entry.insert("tool_calls".into(), json!(tool_calls));
                }
                // 无 content 且无 tool_calls → 丢弃
                if text.is_empty() && tool_calls.is_empty() {
                    continue;
                }
                out.push(Value::Object(entry));
            }
            Some("toolResult") => {
                let text = text_of(message.get("content"));
                let text = if text.is_empty() {
                    "(no tool output)".to_string()
                } else {
                    text
                };
                out.push(json!({
                    "role": "tool",
                    "content": text,
                    "tool_call_id": message["toolCallId"],
                }));
            }
            // 其余角色（system 之外的自定义角色）本路线不支持
            _ => {}
        }
    }
    out
}

fn convert_tools(context: &Value) -> Option<Vec<Value>> {
    let tools = context["tools"].as_array()?;
    if tools.is_empty() {
        return None;
    }
    Some(
        tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t["name"],
                        "description": t["description"],
                        "parameters": t["parameters"],
                        "strict": false,
                    }
                })
            })
            .collect(),
    )
}

fn parse_usage(raw: &Value) -> Value {
    let num = |v: &Value| v.as_u64().unwrap_or(0);
    let prompt_tokens = num(&raw["prompt_tokens"]);
    let cache_read = raw["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .or_else(|| raw["prompt_cache_hit_tokens"].as_u64())
        .or_else(|| raw["cached_tokens"].as_u64())
        .unwrap_or(0);
    let cache_write = num(&raw["prompt_tokens_details"]["cache_write_tokens"]);
    let input = prompt_tokens.saturating_sub(cache_read + cache_write);
    let output = num(&raw["completion_tokens"]);
    let reasoning = num(&raw["completion_tokens_details"]["reasoning_tokens"]);
    json!({
        "input": input,
        "output": output,
        "cacheRead": cache_read,
        "cacheWrite": cache_write,
        "reasoning": reasoning,
        "totalTokens": input + output + cache_read + cache_write,
        "cost": { "input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0 },
    })
}

fn map_stop_reason(reason: &str) -> Result<&'static str, String> {
    match reason {
        "stop" | "end" => Ok("stop"),
        "length" => Ok("length"),
        "function_call" | "tool_calls" => Ok("toolUse"),
        other => Err(format!("provider finish_reason: {other}")),
    }
}

/// 累计的 tool call（流式里靠 `index` 归并，与 OpenAI 分片语义一致）。
#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

fn build_body(request: &Value) -> Result<Value, String> {
    let model = &request["model"];
    let context = &request["context"];
    let options = &request["options"];

    let mut body = Map::new();
    body.insert("model".into(), json!(model["id"]));
    let mut messages = Vec::new();
    let system_prompt = context["systemPrompt"].as_str().unwrap_or("");
    if !system_prompt.is_empty() {
        // DeepSeek: supportsDeveloperRole=false → 用 "system"
        messages.push(json!({ "role": "system", "content": system_prompt }));
    }
    messages.extend(convert_messages(context));
    body.insert("messages".into(), json!(messages));
    body.insert("stream".into(), json!(true));
    body.insert("stream_options".into(), json!({ "include_usage": true }));

    // maxTokensField = "max_tokens"（DeepSeek）
    let max_tokens = options["maxTokens"]
        .as_u64()
        .or_else(|| model["maxTokens"].as_u64())
        .unwrap_or(16_384);
    body.insert("max_tokens".into(), json!(max_tokens));

    if let Some(tools) = convert_tools(context) {
        body.insert("tools".into(), json!(tools));
    }

    // thinkingFormat = "deepseek"
    let reasoning = options["reasoning"].as_str().filter(|r| !r.is_empty());
    match reasoning {
        Some(level) => {
            body.insert("thinking".into(), json!({ "type": "enabled" }));
            let mapped = model["thinkingLevelMap"][level].as_str().unwrap_or(level);
            body.insert("reasoning_effort".into(), json!(mapped));
        }
        None => {
            body.insert("thinking".into(), json!({ "type": "disabled" }));
        }
    }
    Ok(Value::Object(body))
}

/// 一次完整请求：阻塞跑完流，边跑边把增量交给 `emit`，返回最终结果 JSON
/// （形状即 JS 侧 `finishModel` 期待的 `ModelResult`）。
pub fn complete(
    cfg: &DeepSeekConfig,
    request_json: &str,
    emit: &mut dyn FnMut(StreamEvent),
) -> Result<String, String> {
    let request: Value =
        serde_json::from_str(request_json).map_err(|e| format!("bad model request: {e}"))?;
    let model = request["model"].clone();
    let body = build_body(&request)?;

    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let response = client()?
        .post(&url)
        .header("Authorization", format!("Bearer {}", cfg.api_key))
        .header("Content-Type", "application/json")
        // 应用侧 reqwest 没开 `json` feature（spike 里开了）——Content-Type 已显式设过，
        // 这里直接给序列化后的字符串，避免为这一处改全局依赖特性。
        .body(serde_json::to_string(&body).map_err(|e| format!("serialize body: {e}"))?)
        .send()
        .map_err(|e| format!("request failed: {e}"))?;

    let status = response.status();
    if !status.is_success() {
        let text = response.text().unwrap_or_default();
        return Err(format!(
            "HTTP {status}: {}",
            text.chars().take(400).collect::<String>()
        ));
    }

    let mut thinking = String::new();
    let mut text = String::new();
    let mut tool_calls: Vec<PartialToolCall> = Vec::new();
    let mut usage = json!({});
    let mut stop_reason: Option<String> = None;

    let reader = BufReader::new(response);
    for line in reader.lines() {
        let line = line.map_err(|e| format!("stream read: {e}"))?;
        let Some(data) = line.strip_prefix("data:") else {
            continue; // 空行 / 注释 / event: 行
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        if data.is_empty() {
            continue;
        }
        let chunk: Value = serde_json::from_str(data).map_err(|e| {
            format!(
                "bad SSE chunk ({e}): {}",
                data.chars().take(200).collect::<String>()
            )
        })?;
        if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
            usage = parse_usage(u);
        }
        let Some(choice) = chunk["choices"].as_array().and_then(|c| c.first()) else {
            continue;
        };
        if let Some(reason) = choice["finish_reason"].as_str() {
            stop_reason = Some(map_stop_reason(reason)?.to_string());
        }
        let delta = &choice["delta"];
        if let Some(content) = delta["content"].as_str().filter(|s| !s.is_empty()) {
            text.push_str(content);
            emit(StreamEvent::Text(content.to_string()));
        }
        // DeepSeek 用 reasoning_content；兼容字段顺序与 pi-ai 相同
        let reasoning_field = ["reasoning_content", "reasoning", "reasoning_text"]
            .into_iter()
            .find(|f| delta[*f].as_str().is_some_and(|s| !s.is_empty()));
        if let Some(field) = reasoning_field {
            let piece = delta[field].as_str().unwrap_or_default();
            thinking.push_str(piece);
            emit(StreamEvent::Thinking(piece.to_string()));
        }
        if let Some(calls) = delta["tool_calls"].as_array() {
            for call in calls {
                let index = call["index"].as_u64().unwrap_or(0) as usize;
                while tool_calls.len() <= index {
                    tool_calls.push(PartialToolCall::default());
                }
                let slot = &mut tool_calls[index];
                if let Some(id) = call["id"].as_str().filter(|s| !s.is_empty()) {
                    slot.id = id.to_string();
                }
                if let Some(name) = call["function"]["name"].as_str().filter(|s| !s.is_empty()) {
                    slot.name = name.to_string();
                }
                if let Some(args) = call["function"]["arguments"].as_str() {
                    slot.arguments.push_str(args);
                }
            }
        }
    }

    // 没有 finish_reason 就拿内容推断（pi-ai 的 supportsFinishReason=false 分支）
    let stop_reason = match stop_reason {
        Some(r) => r,
        None => {
            if tool_calls.is_empty() {
                "stop".to_string()
            } else {
                "toolUse".to_string()
            }
        }
    };

    let mut calls = Vec::new();
    for call in &tool_calls {
        let arguments: Value = if call.arguments.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&call.arguments)
                .map_err(|e| format!("tool call arguments are not valid JSON: {e}"))?
        };
        calls.push(json!({
            "id": call.id,
            "name": call.name,
            "arguments": arguments,
        }));
    }

    let signature = if thinking.is_empty() {
        Value::Null
    } else {
        json!("reasoning_content")
    };
    let _ = model; // 目前只用于日志/成本，占位避免未读警告
    Ok(json!({
        "thinking": thinking,
        "thinkingSignature": signature,
        "text": text,
        "toolCalls": calls,
        "usage": usage,
        "stopReason": stop_reason,
    })
    .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Value {
        json!({
            "model": {
                "id": "deepseek-v4-flash",
                "maxTokens": 384000,
                "thinkingLevelMap": { "low": "low", "high": "high", "max": "max" }
            },
            "context": {
                "systemPrompt": "be brief",
                "messages": [
                    { "role": "user", "content": [{ "type": "text", "text": "hi" }] },
                    { "role": "assistant", "content": [
                        { "type": "thinking", "thinking": "hmm", "thinkingSignature": "reasoning_content" },
                        { "type": "text", "text": "hello" },
                        { "type": "toolCall", "id": "c1", "name": "read", "arguments": { "path": "a.txt" } }
                    ] },
                    { "role": "toolResult", "toolCallId": "c1", "toolName": "read",
                      "content": [{ "type": "text", "text": "file body" }] }
                ],
                "tools": [{ "name": "read", "description": "read a file", "parameters": { "type": "object" } }]
            },
            "options": { "reasoning": "high" }
        })
    }

    #[test]
    fn body_matches_pi_ai_openai_completions_for_deepseek() {
        let body = build_body(&request()).unwrap();
        assert_eq!(body["model"], "deepseek-v4-flash");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["max_tokens"], 384000);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["reasoning_effort"], "high");
        assert!(body.get("store").is_none(), "DeepSeek: supportsStore=false");

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        // assistant：content 是纯字符串 + reasoning_content 必填 + tool_calls 转 JSON 串
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"], "hello");
        assert_eq!(messages[2]["reasoning_content"], "hmm");
        assert_eq!(messages[2]["tool_calls"][0]["id"], "c1");
        assert_eq!(messages[2]["tool_calls"][0]["type"], "function");
        assert_eq!(
            messages[2]["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"a.txt\"}"
        );
        // toolResult → role:"tool" + tool_call_id
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "c1");
        assert_eq!(messages[3]["content"], "file body");
        assert_eq!(body["tools"][0]["function"]["name"], "read");
        assert_eq!(body["tools"][0]["function"]["strict"], false);
    }

    #[test]
    fn thinking_disabled_and_tool_only_assistant_is_kept() {
        let mut req = request();
        req["options"] = json!({});
        let body = build_body(&req).unwrap();
        assert_eq!(body["thinking"]["type"], "disabled");
        assert!(body.get("reasoning_effort").is_none());

        // 无文本、只有 toolCall 的 assistant 必须保留（否则 tool 结果会变成孤儿）
        let messages = body["messages"].as_array().unwrap();
        assert!(messages.iter().any(|m| m["role"] == "assistant"));
    }

    #[test]
    fn usage_maps_deepseek_cache_hit_field() {
        let usage = parse_usage(&json!({
            "prompt_tokens": 1000,
            "completion_tokens": 50,
            "prompt_cache_hit_tokens": 800,
            "completion_tokens_details": { "reasoning_tokens": 20 }
        }));
        assert_eq!(usage["cacheRead"], 800);
        assert_eq!(usage["input"], 200); // 1000 - 800，与 pi-ai 同口径
        assert_eq!(usage["output"], 50);
        assert_eq!(usage["reasoning"], 20);
        assert_eq!(usage["totalTokens"], 1050);
    }

    #[test]
    fn stop_reason_mapping() {
        assert_eq!(map_stop_reason("stop").unwrap(), "stop");
        assert_eq!(map_stop_reason("tool_calls").unwrap(), "toolUse");
        assert_eq!(map_stop_reason("length").unwrap(), "length");
        assert!(map_stop_reason("content_filter").is_err());
    }
}
