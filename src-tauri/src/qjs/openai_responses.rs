//! OpenAI **Responses API** 的 Rust 传输（`model.api == "openai-responses"`）。
//!
//! 覆盖目录里 105 个模型 / 6 个 provider（openai 38 是 UI 的第一行，xai 4，另有
//! opencode / cloudflare-ai-gateway / github-copilot——后两家的动态头没做，见下）。
//!
//! 为什么先做这一族：`openai-responses.js` 只有 286 行，而 `openai-completions.js`
//! 是 1352 行（8 种 thinkingFormat + 缓存/亲和/grammar/strict/deferred 一堆 quirk）。
//! 拿同样的力气先把 UI 最常用的一家打通。
//!
//! 对齐 `@earendil-works/pi-ai/dist/api/openai-responses.js` +
//! `openai-responses-shared.js`（pi-ai 0.84.4），逐项核对过的：
//!   · `buildParams`  → [`build_body`]（store:false / max_output_tokens / tools /
//!     reasoning.effort + include:reasoning.encrypted_content）
//!   · `convertResponsesMessages` → [`convert_messages`]（input items：user / assistant
//!     的 message 与 function_call / toolResult 的 function_call_output）
//!   · `convertResponsesTools` → [`convert_tools`]（**扁平** tool 形状）
//!   · `processResponsesStream` → [`complete`] 里的 SSE 分支
//!   · `mapStopReason` / `getSupportedThinkingLevels` / `clampThinkingLevel` 照抄
//!
//! 刻意**不**做的（都是优化或边缘能力，做了只会让第一版更难验；每条都写明了缺什么）：
//!   · `prompt_cache_key` / `prompt_cache_retention`（服务端提示缓存，省钱但不影响正确性）
//!   · `service_tier` / `temperature` / `samplingParams`（我们的 options 里没有）
//!   · 会话亲和头（`x-session-id` / `session_id`）、github-copilot 的动态头
//!   · grammar tools / strict 的自定义 schema 重写 / deferred tools（additional_tools、
//!     tool_search）—— 我们不发这些字段；`strict` 只在目录声明支持时按 false 带上
//!   · 图片输入（`input_image`）与图片工具结果：我们的工具全是文本
//!   · `textSignature`（pi-ai 把 message item 的 id/phase 存在正文块上）。我们的
//!     扁平结果契约（`{thinking, text, toolCalls}`）没有这个字段 → 回放时 message id
//!     用 pi-ai 的兜底形态 `msg_pi_<n>`，phase 丢失（只影响 codex 系模型的
//!     commentary/final_answer 区分）
//!
//! ⚠️ 与我们另一族（`deepseek.rs`）共用一个**扁平**结果契约，所以**多个 reasoning
//! item 会被合并成一个** thinking 串（正常一轮只有一个；codex 系可能两个）。

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::time::Duration;

use serde_json::{json, Value};

use super::{Config, StreamEvent};

/// pi-ai 的思考档位顺序（`EXTENDED_THINKING_LEVELS`）—— clamp 靠它。
const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
/// Responses API 不接受小于 16 的 `max_output_tokens`（pi-ai issue #6265）。
const MIN_OUTPUT_TOKENS: u64 = 16;

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        // 推理模型的「想」阶段可能很久，超时给得比 completions 更宽
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| format!("http client: {e}"))
}

// ── 请求体 ────────────────────────────────────────────────────────────────

/// `context.messages` + systemPrompt → Responses API 的 `input` items。
pub fn convert_messages(model: &Value, context: &Value) -> Vec<Value> {
    let mut items = Vec::new();
    let system_prompt = context["systemPrompt"].as_str().unwrap_or("");
    if !system_prompt.is_empty() {
        // 推理模型用 developer（OpenAI 的建议角色），其余用 system。
        // 目录里 supportsDeveloperRole 为 false 的走 system（我们这一族目前没有）。
        let supports_developer = model["compat"]["supportsDeveloperRole"] != json!(false);
        let role = if model["reasoning"] == json!(true) && supports_developer {
            "developer"
        } else {
            "system"
        };
        items.push(json!({ "role": role, "content": system_prompt }));
    }

    let empty = Vec::new();
    let messages = context["messages"].as_array().unwrap_or(&empty);
    for (msg_index, msg) in messages.iter().enumerate() {
        match msg["role"].as_str().unwrap_or_default() {
            "user" => {
                if let Some(text) = msg["content"].as_str() {
                    items.push(json!({
                        "role": "user",
                        "content": [{ "type": "input_text", "text": text }],
                    }));
                    continue;
                }
                let content: Vec<Value> = msg["content"]
                    .as_array()
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter(|b| b["type"] == "text")
                            .filter_map(|b| b["text"].as_str())
                            .map(|text| json!({ "type": "input_text", "text": text }))
                            .collect()
                    })
                    .unwrap_or_default();
                if content.is_empty() {
                    continue;
                }
                items.push(json!({ "role": "user", "content": content }));
            }
            "assistant" => {
                // 一条 assistant 消息会展开成**多个** input item（message / function_call）
                let same_model = msg["provider"] == model["provider"]
                    && msg["api"] == model["api"]
                    && msg["model"] == model["id"];
                let mut text_block_index = 0usize;
                for block in msg["content"].as_array().cloned().unwrap_or_default() {
                    match block["type"].as_str().unwrap_or_default() {
                        "thinking" => {
                            // 回放的其实是**宿主存下的整个 reasoning item**（含加密内容），
                            // 所以 thinkingSignature 就是它的 JSON —— 与 pi-ai 同约定。
                            if let Some(signature) = block["thinkingSignature"].as_str() {
                                if let Ok(item) = serde_json::from_str::<Value>(signature) {
                                    items.push(item);
                                }
                            }
                        }
                        "text" => {
                            let fallback = if text_block_index == 0 {
                                format!("msg_pi_{msg_index}")
                            } else {
                                format!("msg_pi_{msg_index}_{text_block_index}")
                            };
                            text_block_index += 1;
                            items.push(json!({
                                "type": "message",
                                "role": "assistant",
                                "content": [{
                                    "type": "output_text",
                                    "text": block["text"].as_str().unwrap_or_default(),
                                    "annotations": [],
                                }],
                                "status": "completed",
                                "id": fallback,
                            }));
                        }
                        "toolCall" => {
                            let id = block["id"].as_str().unwrap_or_default();
                            let (call_id, item_id) = split_tool_call_id(id);
                            let mut item = json!({
                                "type": "function_call",
                                "call_id": call_id,
                                "name": block["name"],
                                "arguments": block["arguments"].to_string(),
                            });
                            // item id 必须是 fc_*；跨模型回放时去掉，避免 OpenAI 的
                            // rs_*/fc_* 配对校验（pi-ai 同一条规则）。
                            if item_id.starts_with("fc_") && same_model {
                                item["id"] = json!(item_id);
                            }
                            items.push(item);
                        }
                        _ => {}
                    }
                }
            }
            "toolResult" => {
                let (call_id, _) =
                    split_tool_call_id(msg["toolCallId"].as_str().unwrap_or_default());
                let text: String = msg["content"]
                    .as_array()
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter(|b| b["type"] == "text")
                            .filter_map(|b| b["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let output = if text.is_empty() {
                    "(no tool output)".to_string()
                } else {
                    text
                };
                items.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output,
                }));
            }
            _ => {}
        }
    }
    items
}

/// pi-ai 把工具调用 id 存成 `"<call_id>|<item_id>"`（Responses 需要两个 id）。
/// 回放时按 `|` 拆；没有 item 部分（别的家族写的会话）就整体当 call_id。
fn split_tool_call_id(id: &str) -> (String, String) {
    match id.split_once('|') {
        Some((call_id, item_id)) => (call_id.to_string(), item_id.to_string()),
        None => (id.to_string(), String::new()),
    }
}

/// 工具 → Responses 的**扁平**形状（与 completions 的 `{type,function:{…}}` 不同）。
fn convert_tools(context: &Value, strict_supported: bool) -> Option<Value> {
    let tools = context["tools"].as_array()?;
    if tools.is_empty() {
        return None;
    }
    let list: Vec<Value> = tools
        .iter()
        .map(|tool| {
            let mut item = json!({
                "type": "function",
                "name": tool["name"],
                "description": tool["description"],
                "parameters": tool["parameters"],
            });
            // 目录声明 supportsStrictMode（openai 系为 true）时才带 strict；
            // 我们的工具 schema 不是 strict schema，所以按 pi-ai 的默认值发 false。
            if strict_supported {
                item["strict"] = json!(false);
            }
            item
        })
        .collect();
    Some(json!(list))
}

/// `getSupportedThinkingLevels` 的直译。
///
/// ⚠️ **`undefined` 与 `null` 在 serde_json 里都是 `Value::Null`**，而 pi-ai 用的是
/// JS 的两者区分：低档位**缺键**（undefined）算支持，**显式 null** 不算；
/// xhigh/max 两种都不算。所以这里必须走 `as_object().get()`（`Option`），
/// 不能写 `model["thinkingLevelMap"][level]` —— 后者会把缺键当成 null。
fn supported_thinking_levels(model: &Value) -> Vec<&'static str> {
    if model["reasoning"] != json!(true) {
        return vec!["off"];
    }
    let map = model["thinkingLevelMap"].as_object();
    THINKING_LEVELS
        .iter()
        .copied()
        .filter(|level| match map.and_then(|m| m.get(*level)) {
            // 显式 null → 不支持
            Some(Value::Null) => false,
            Some(_) => true,
            // 缺键：低档位算支持，xhigh/max 不算
            None => !matches!(*level, "xhigh" | "max"),
        })
        .collect()
}

/// `clampThinkingLevel` 的直译：不支持就往后找、再往前找。
/// 不做这一步会直接把错的 effort 发上去（例：gpt-5.2-chat-latest 只认 xhigh，
/// 而 UI 传 "high"）→ 400。
fn clamp_thinking_level(model: &Value, level: &str) -> String {
    let levels = supported_thinking_levels(model);
    if levels.contains(&level) {
        return level.to_string();
    }
    let Some(requested) = THINKING_LEVELS.iter().position(|l| *l == level) else {
        return levels.first().copied().unwrap_or("off").to_string();
    };
    for candidate in &THINKING_LEVELS[requested..] {
        if levels.contains(candidate) {
            return candidate.to_string();
        }
    }
    for candidate in THINKING_LEVELS[..requested].iter().rev() {
        if levels.contains(candidate) {
            return candidate.to_string();
        }
    }
    levels.first().copied().unwrap_or("off").to_string()
}

pub fn build_body(request: &Value) -> Result<Value, String> {
    let model = &request["model"];
    let context = &request["context"];
    let options = &request["options"];

    let mut body = json!({
        "model": model["id"],
        "input": convert_messages(model, context),
        "stream": true,
        // Responses 永远不带服务端存储（多轮靠 reasoning.encrypted_content 回放）
        "store": false,
    });

    if let Some(max_tokens) = options["maxTokens"]
        .as_u64()
        .or_else(|| model["maxTokens"].as_u64())
    {
        body["max_output_tokens"] = json!(max_tokens.max(MIN_OUTPUT_TOKENS));
    }

    let strict_supported = model["compat"]["supportsStrictMode"] == json!(true);
    if let Some(tools) = convert_tools(context, strict_supported) {
        body["tools"] = tools;
    }

    if model["reasoning"] == json!(true) {
        let requested = options["reasoning"].as_str().filter(|r| !r.is_empty());
        if let Some(level) = requested {
            let clamped = clamp_thinking_level(model, level);
            if clamped != "off" {
                let map = &model["thinkingLevelMap"][&clamped];
                let effort = map.as_str().unwrap_or(&clamped);
                body["reasoning"] = json!({ "effort": effort, "summary": "auto" });
                body["include"] = json!(["reasoning.encrypted_content"]);
            }
        } else if model["provider"] != json!("github-copilot") {
            // pi-ai 的原文是 `thinkingLevelMap.off !== null` —— JS 里 undefined 也算「发」，
            // 所以三条分支是不一样的行为：
            //  · off 是字符串 → 发那个值；
            //  · off 缺键（undefined）或不是字符串 → 发 "none"；
            //  · off **显式 null** → 整个 reasoning 字段不发。
            // ⚠️ serde 里缺键与 null 都取到 Value::Null —— 必须用 get() 区分。
            if !matches!(
                model["thinkingLevelMap"].as_object().map(|m| m.get("off")),
                Some(Some(Value::Null))
            ) {
                let off = model["thinkingLevelMap"]["off"]
                    .as_str()
                    .unwrap_or("none")
                    .to_string();
                body["reasoning"] = json!({ "effort": off });
            }
        }
        // xai 即使不指定 effort 也要拿回加密推理内容（否则多轮会 400）
        if model["provider"] == json!("xai") {
            body["include"] = json!(["reasoning.encrypted_content"]);
        }
    }
    Ok(body)
}

// ── 流式解析 ──────────────────────────────────────────────────────────────

/// 一个工具调用的累积状态（Responses 的参数是**整段续传**，不是增量拼接）。
#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

/// 某个 `output_index` 上是哪一类内容。
#[derive(PartialEq)]
enum Slot {
    Thinking,
    Text,
    ToolCall(usize),
}

fn parse_usage(response: &Value) -> Value {
    let usage = &response["usage"];
    let num = |v: &Value| v.as_u64().unwrap_or(0);
    let cached = num(&usage["input_tokens_details"]["cached_tokens"]);
    let cache_write = num(&usage["input_tokens_details"]["cache_write_tokens"]);
    // OpenAI 的 input_tokens 含缓存读/写，pi-ai 把两者都减掉
    let input = num(&usage["input_tokens"]).saturating_sub(cached + cache_write);
    let output = num(&usage["output_tokens"]);
    json!({
        "input": input,
        "output": output,
        "cacheRead": cached,
        "cacheWrite": cache_write,
        "reasoning": num(&usage["output_tokens_details"]["reasoning_tokens"]),
        "totalTokens": num(&usage["total_tokens"]),
        "cost": { "input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0 },
    })
}

fn map_stop_reason(status: Option<&str>, incomplete: Option<&str>) -> Result<String, String> {
    Ok(match status {
        None | Some("completed") => "stop".to_string(),
        Some("incomplete") => {
            if incomplete == Some("max_output_tokens") {
                "length".to_string()
            } else {
                return Err(match incomplete {
                    Some(reason) => format!("response incomplete: {reason}"),
                    None => "response incomplete without a provider reason".to_string(),
                });
            }
        }
        Some("failed") | Some("cancelled") => {
            return Err(format!("response {}", status.unwrap_or_default()))
        }
        // 上游注释称这两个是 wonky 的（流没走完就到了终态）
        Some("in_progress") | Some("queued") => "stop".to_string(),
        Some(other) => return Err(format!("unhandled response status: {other}")),
    })
}

/// 一次完整请求：阻塞跑完 SSE，边跑边把增量交给 `emit`，返回扁平结果 JSON
/// （形状即 JS 侧 `finishModel` 期待的 `{thinking, thinkingSignature, text,
/// toolCalls, usage, stopReason}`）。
pub fn complete(
    cfg: &Config,
    request_json: &str,
    emit: &mut dyn FnMut(StreamEvent),
) -> Result<String, String> {
    let request: Value =
        serde_json::from_str(request_json).map_err(|e| format!("bad model request: {e}"))?;
    let body = build_body(&request)?;

    let url = format!("{}/responses", cfg.base_url.trim_end_matches('/'));
    let response = client()?
        .post(&url)
        .header("Authorization", format!("Bearer {}", cfg.api_key))
        .header("Content-Type", "application/json")
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
    let mut thinking_signature: Option<String> = None;
    let mut text = String::new();
    let mut tool_calls: Vec<PartialToolCall> = Vec::new();
    let mut slots: HashMap<u64, Slot> = HashMap::new();
    let mut usage = json!({});
    let mut stop_reason: Option<String> = None;
    let mut saw_terminal = false;

    let reader = BufReader::new(response);
    for line in reader.lines() {
        let line = line.map_err(|e| format!("stream read: {e}"))?;
        // Responses 的 SSE 每行形如 `event: response.output_text.delta` / `data: {…}`。
        // data 里的 `type` 字段与 event 名一致，所以只看 data 就够（含注释与空行）。
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let event: Value = serde_json::from_str(data).map_err(|e| {
            format!(
                "bad SSE chunk ({e}): {}",
                data.chars().take(200).collect::<String>()
            )
        })?;
        let kind = event["type"].as_str().unwrap_or_default();
        let output_index = event["output_index"].as_u64().unwrap_or(0);

        match kind {
            "response.created" => {}
            "response.output_item.added" => {
                let item = &event["item"];
                match item["type"].as_str().unwrap_or_default() {
                    "reasoning" => {
                        slots.insert(output_index, Slot::Thinking);
                    }
                    "message" => {
                        if item["phase"] == json!("final_answer") {
                            stop_reason = Some("stop".into());
                        }
                        slots.insert(output_index, Slot::Text);
                    }
                    "function_call" => {
                        tool_calls.push(PartialToolCall {
                            id: format!(
                                "{}|{}",
                                item["call_id"].as_str().unwrap_or_default(),
                                item["id"].as_str().unwrap_or_default()
                            ),
                            name: item["name"].as_str().unwrap_or_default().to_string(),
                            arguments: item["arguments"].as_str().unwrap_or_default().to_string(),
                        });
                        slots.insert(output_index, Slot::ToolCall(tool_calls.len() - 1));
                    }
                    _ => {}
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if slots.get(&output_index) != Some(&Slot::Thinking) {
                    continue;
                }
                let delta = event["delta"].as_str().unwrap_or_default();
                thinking.push_str(delta);
                emit(StreamEvent::Thinking(delta.to_string()));
            }
            "response.reasoning_summary_part.done" => {
                if slots.get(&output_index) != Some(&Slot::Thinking) {
                    continue;
                }
                thinking.push_str("\n\n");
                emit(StreamEvent::Thinking("\n\n".into()));
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                if slots.get(&output_index) != Some(&Slot::Text) {
                    continue;
                }
                let delta = event["delta"].as_str().unwrap_or_default();
                text.push_str(delta);
                emit(StreamEvent::Text(delta.to_string()));
            }
            "response.function_call_arguments.delta" => {
                if let Some(Slot::ToolCall(index)) = slots.get(&output_index) {
                    let call = &mut tool_calls[*index];
                    // 上游是把 delta 拼进 partialJson；done 事件里可能给整段，
                    // 所以这里只累积，最终以 done/completed 的值为准。
                    call.arguments
                        .push_str(event["delta"].as_str().unwrap_or_default());
                }
            }
            "response.function_call_arguments.done" => {
                if let Some(Slot::ToolCall(index)) = slots.get(&output_index) {
                    if let Some(arguments) = event["arguments"].as_str() {
                        tool_calls[*index].arguments = arguments.to_string();
                    }
                }
            }
            "response.output_item.done" => {
                let item = &event["item"];
                if item["phase"] == json!("final_answer") {
                    stop_reason = Some("stop".into());
                }
                match item["type"].as_str().unwrap_or_default() {
                    "reasoning" => {
                        // item.done 里的 summary/content 是权威值（流式增量可能只有 summary）
                        let joined = |key: &str| -> String {
                            item[key]
                                .as_array()
                                .map(|parts| {
                                    parts
                                        .iter()
                                        .filter_map(|p| p["text"].as_str())
                                        .collect::<Vec<_>>()
                                        .join("\n\n")
                                })
                                .unwrap_or_default()
                        };
                        let summary = joined("summary");
                        let content = joined("content");
                        if !summary.is_empty() {
                            thinking = summary;
                        } else if !content.is_empty() {
                            thinking = content;
                        }
                        thinking_signature = Some(item.to_string());
                        slots.remove(&output_index);
                    }
                    "message" => {
                        let body_text: String = item["content"]
                            .as_array()
                            .map(|parts| {
                                parts
                                    .iter()
                                    .map(|p| {
                                        p["text"]
                                            .as_str()
                                            .or_else(|| p["refusal"].as_str())
                                            .unwrap_or_default()
                                    })
                                    .collect::<Vec<_>>()
                                    .join("")
                            })
                            .unwrap_or_default();
                        text = body_text;
                        slots.remove(&output_index);
                    }
                    "function_call" => {
                        if let Some(Slot::ToolCall(index)) = slots.get(&output_index) {
                            // 整段参数（比流式拼的更权威）
                            if let Some(arguments) = item["arguments"].as_str() {
                                tool_calls[*index].arguments = arguments.to_string();
                            }
                            if let Some(name) = item["name"].as_str() {
                                tool_calls[*index].name = name.to_string();
                            }
                            if let Some(id) = item["id"].as_str() {
                                let call_id = item["call_id"].as_str().unwrap_or_default();
                                tool_calls[*index].id = format!("{call_id}|{id}");
                            }
                            slots.remove(&output_index);
                        }
                    }
                    _ => {}
                }
            }
            "response.completed" | "response.incomplete" => {
                saw_terminal = true;
                let response = &event["response"];
                usage = parse_usage(response);
                let incomplete = response["incomplete_details"]["reason"].as_str();
                if let Some(mapped) = stop_reason.clone() {
                    // 前面已定过（message phase = final_answer），除非终态是别的
                    stop_reason = Some(mapped);
                } else {
                    stop_reason = Some(map_stop_reason(response["status"].as_str(), incomplete)?);
                }
                // 终态响应里的 reasoning item 是最完整的（encrypted_content 只在这里
                // 出现的情况上游是回填、我们直接以它为准）
                if thinking_signature.is_none() && !thinking.is_empty() {
                    if let Some(item) = response["output"]
                        .as_array()
                        .and_then(|items| items.iter().find(|i| i["type"] == json!("reasoning")))
                    {
                        thinking_signature = Some(item.to_string());
                    }
                }
            }
            "error" => {
                return Err(format!(
                    "error code {}: {}",
                    event["code"].as_str().unwrap_or("unknown"),
                    event["message"].as_str().unwrap_or("no message")
                ))
            }
            "response.failed" => {
                // 不置 saw_terminal：这一支直接 return Err，随后不会再检查它
                let response = &event["response"];
                let detail = response["error"]["message"]
                    .as_str()
                    .or_else(|| response["incomplete_details"]["reason"].as_str());
                return Err(match detail {
                    Some(detail) => format!(
                        "{}: {detail}",
                        response["error"]["code"].as_str().unwrap_or("failed")
                    ),
                    None => "response failed without error details".to_string(),
                });
            }
            _ => {} // 其它事件（文本 done、reasoning part added…）不参与扁平结果
        }
    }

    if !saw_terminal {
        return Err("OpenAI Responses stream ended before a terminal response event".into());
    }
    let mut stop_reason = stop_reason.unwrap_or_else(|| "stop".to_string());
    // 有工具调用就必须是 toolUse（否则 JS 的 finishModel 会拒收）
    if !tool_calls.is_empty() && stop_reason == "stop" {
        stop_reason = "toolUse".to_string();
    }
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

    Ok(json!({
        "thinking": thinking,
        "thinkingSignature": thinking_signature,
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

    fn model(id: &str) -> Value {
        match id {
            // 推理模型、off:null（不显式发 reasoning 关）
            "gpt-5" => json!({
                "id": "gpt-5", "provider": "openai", "api": "openai-responses",
                "baseUrl": "https://api.openai.com/v1", "reasoning": true,
                "maxTokens": 128000, "contextWindow": 400000,
                "compat": { "supportsStrictMode": true, "supportsOpenAIGrammarTools": true },
                "thinkingLevelMap": { "off": null, "minimal": "minimal", "low": "low",
                                      "medium": "medium", "high": "high", "xhigh": null, "max": null },
            }),
            // 只认 xhigh（用来验 clamp）
            "gpt-5.2-chat" => json!({
                "id": "gpt-5.2-chat-latest", "provider": "openai", "api": "openai-responses",
                "baseUrl": "https://api.openai.com/v1", "reasoning": true,
                "maxTokens": 16384, "contextWindow": 128000,
                "compat": { "supportsStrictMode": true },
                "thinkingLevelMap": { "off": null, "medium": "medium", "high": null, "xhigh": "xhigh" },
            }),
            // 非推理模型：角色要用 system
            "gpt-4o" => json!({
                "id": "gpt-4o", "provider": "openai", "api": "openai-responses",
                "baseUrl": "https://api.openai.com/v1", "reasoning": false,
                "maxTokens": 16384, "contextWindow": 128000,
                "compat": { "supportsStrictMode": true }, "thinkingLevelMap": {},
            }),
            "grok" => json!({
                "id": "grok-4.5", "provider": "xai", "api": "openai-responses",
                "baseUrl": "https://api.x.ai/v1", "reasoning": true,
                "maxTokens": 500000, "contextWindow": 500000,
                "compat": {}, "thinkingLevelMap": { "off": null, "high": "high" },
            }),
            other => panic!("unknown fixture model: {other}"),
        }
    }

    fn request(id: &str, reasoning: Value, messages: Value) -> Value {
        json!({
            "model": model(id),
            "context": {
                "systemPrompt": "be brief",
                "messages": messages,
                "tools": [{
                    "name": "read",
                    "description": "read a file",
                    "parameters": { "type": "object", "properties": { "path": { "type": "string" } } },
                }],
            },
            "options": { "reasoning": reasoning },
        })
    }

    #[test]
    fn body_uses_responses_wire_shape() {
        let body = build_body(&request("gpt-5", json!("high"), json!([]))).unwrap();
        assert_eq!(body["model"], "gpt-5");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false, "Responses 恒不带服务端存储");
        assert_eq!(body["max_output_tokens"], 128000);
        // tools 是**扁平**形状（completions 那族是 {type,function:{…}}）
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "read");
        assert!(body["tools"][0].get("function").is_none());
        assert_eq!(
            body["tools"][0]["strict"], false,
            "openai 声明了 supportsStrictMode"
        );
        // 推理：effort 取映射 + summary=auto + 要拿回加密推理内容
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["reasoning"]["summary"], "auto");
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        // systemPrompt：推理模型用 developer
        assert_eq!(body["input"][0]["role"], "developer");
        assert_eq!(body["input"][0]["content"], "be brief");
    }

    #[test]
    fn non_reasoning_model_uses_system_role_and_no_reasoning_field() {
        let body = build_body(&request("gpt-4o", json!("high"), json!([]))).unwrap();
        assert_eq!(body["input"][0]["role"], "system");
        assert!(body.get("reasoning").is_none());
        assert!(body.get("include").is_none());
    }

    #[test]
    fn reasoning_off_respects_null_vs_string_off_map() {
        // off 映射是 null → 整个字段不发（pi-ai 的分支就是这样）
        let body = build_body(&request("grok", Value::Null, json!([]))).unwrap();
        assert!(body.get("reasoning").is_none(), "{body}");
        // xai 即使不发 effort 也要 include（否则多轮 400）
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    }

    #[test]
    fn thinking_level_map_distinguishes_missing_key_from_explicit_null() {
        // 缺键（pi-ai 的 undefined）：低档位算支持，xhigh/max 不算
        let bare = json!({ "reasoning": true, "thinkingLevelMap": {} });
        assert_eq!(
            supported_thinking_levels(&bare),
            vec!["off", "minimal", "low", "medium", "high"]
        );
        // 显式 null：不支持，clamp 往前退一档
        let nulled = json!({ "reasoning": true, "thinkingLevelMap": {
            "off": null, "low": null, "medium": "medium", "high": null } });
        assert_eq!(
            supported_thinking_levels(&nulled),
            vec!["minimal", "medium"]
        );
        assert_eq!(clamp_thinking_level(&nulled, "high"), "medium");
        // 非推理模型只有 off
        assert_eq!(
            supported_thinking_levels(&json!({ "reasoning": false })),
            vec!["off"]
        );
    }

    #[test]
    fn off_map_missing_key_sends_none_but_explicit_null_sends_nothing() {
        // 缺键 → 发 {effort:"none"}（pi-ai 的 `off !== null` 对 undefined 成立）
        let mut req = request("gpt-5", Value::Null, json!([]));
        req["model"]["thinkingLevelMap"] = json!({ "low": "low", "high": "high" });
        assert_eq!(build_body(&req).unwrap()["reasoning"]["effort"], "none");
        // 显式 null → 整个 reasoning 字段不发
        let mut req = request("gpt-5", Value::Null, json!([]));
        req["model"]["thinkingLevelMap"] = json!({ "off": null, "high": "high" });
        let body = build_body(&req).unwrap();
        assert!(body.get("reasoning").is_none(), "{body}");
    }

    #[test]
    fn clamps_unsupported_effort_instead_of_sending_it() {
        // gpt-5.2-chat-latest 不支持 high（映射为 null）→ clamp 到 xhigh
        assert_eq!(
            clamp_thinking_level(&model("gpt-5.2-chat"), "high"),
            "xhigh"
        );
        let body = build_body(&request("gpt-5.2-chat", json!("high"), json!([]))).unwrap();
        assert_eq!(body["reasoning"]["effort"], "xhigh");
        // 支持的就原样
        assert_eq!(clamp_thinking_level(&model("gpt-5"), "high"), "high");
        assert_eq!(clamp_thinking_level(&model("gpt-5"), "low"), "low");
    }

    #[test]
    fn converts_user_assistant_and_tool_result_items() {
        let messages = json!([
            { "role": "user", "content": "hi" },
            { "role": "assistant", "provider": "openai", "api": "openai-responses", "model": "gpt-5",
              "content": [
                { "type": "thinking", "thinking": "hmm",
                  "thinkingSignature": "{\"type\":\"reasoning\",\"id\":\"rs_1\",\"encrypted_content\":\"xx\"}" },
                { "type": "text", "text": "let me look" },
                { "type": "toolCall", "id": "call_1|fc_1", "name": "read", "arguments": { "path": "a.txt" } }
              ] },
            { "role": "toolResult", "toolCallId": "call_1|fc_1", "toolName": "read",
              "content": [{ "type": "text", "text": "file body" }] },
        ]);
        let body = build_body(&request("gpt-5", json!("high"), messages)).unwrap();
        let input = body["input"].as_array().unwrap();
        // system + user + (reasoning + message + function_call) + function_call_output
        assert_eq!(input.len(), 6, "{input:?}");
        assert_eq!(input[1]["role"], "user");
        assert_eq!(input[1]["content"][0]["type"], "input_text");
        // reasoning item 原样回放（加密内容回来了，store:false 才成立）
        assert_eq!(input[2]["type"], "reasoning");
        assert_eq!(input[2]["encrypted_content"], "xx");
        assert_eq!(input[3]["type"], "message");
        assert_eq!(input[3]["role"], "assistant");
        assert_eq!(input[3]["content"][0]["type"], "output_text");
        assert_eq!(
            input[3]["id"], "msg_pi_1",
            "扁平契约没有 textSignature → pi-ai 的兜底 id"
        );
        assert_eq!(input[4]["type"], "function_call");
        assert_eq!(input[4]["call_id"], "call_1");
        assert_eq!(input[4]["id"], "fc_1", "同模型回放要带上 fc_ id");
        assert_eq!(input[4]["arguments"], "{\"path\":\"a.txt\"}");
        assert_eq!(input[5]["type"], "function_call_output");
        assert_eq!(input[5]["call_id"], "call_1");
        assert_eq!(input[5]["output"], "file body");
    }

    #[test]
    fn drops_foreign_fc_id_and_falls_back_for_empty_tool_output() {
        let messages = json!([
            { "role": "assistant", "provider": "deepseek", "api": "openai-completions", "model": "deepseek-v4-flash",
              "content": [{ "type": "toolCall", "id": "call_9|fc_9", "name": "read", "arguments": {} }] },
            { "role": "toolResult", "toolCallId": "call_9|fc_9", "toolName": "read", "content": [] },
        ]);
        let body = build_body(&request("gpt-5", json!("high"), messages)).unwrap();
        let input = body["input"].as_array().unwrap();
        let call = input.iter().find(|i| i["type"] == "function_call").unwrap();
        assert!(call.get("id").is_none(), "跨模型回放要丢掉 fc_ id：{call}");
        assert_eq!(call["call_id"], "call_9");
        let result = input
            .iter()
            .find(|i| i["type"] == "function_call_output")
            .unwrap();
        assert_eq!(result["output"], "(no tool output)");
    }

    #[test]
    fn maps_stop_reasons() {
        assert_eq!(map_stop_reason(Some("completed"), None).unwrap(), "stop");
        assert_eq!(
            map_stop_reason(Some("incomplete"), Some("max_output_tokens")).unwrap(),
            "length"
        );
        assert!(map_stop_reason(Some("incomplete"), Some("content_filter")).is_err());
        assert!(map_stop_reason(Some("failed"), None).is_err());
        assert_eq!(map_stop_reason(None, None).unwrap(), "stop");
    }

    #[test]
    fn parses_usage_subtracting_cached_tokens() {
        let response = json!({ "usage": {
            "input_tokens": 1000, "output_tokens": 200, "total_tokens": 1200,
            "input_tokens_details": { "cached_tokens": 300 },
            "output_tokens_details": { "reasoning_tokens": 50 },
        }});
        let usage = parse_usage(&response);
        assert_eq!(usage["input"], 700, "input 要减掉缓存读/写");
        assert_eq!(usage["cacheRead"], 300);
        assert_eq!(usage["output"], 200);
        assert_eq!(usage["reasoning"], 50);
        assert_eq!(usage["totalTokens"], 1200);
    }

    #[test]
    fn tool_call_id_round_trips_between_parse_and_replay() {
        let (call_id, item_id) = split_tool_call_id("call_abc|fc_xyz");
        assert_eq!((call_id.as_str(), item_id.as_str()), ("call_abc", "fc_xyz"));
        // 别的家族写的会话（没有 `|`）不能崩，整体当 call_id
        let (call_id, item_id) = split_tool_call_id("call_abc");
        assert_eq!((call_id.as_str(), item_id.as_str()), ("call_abc", ""));
    }
}
