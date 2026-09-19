// QuickJS guest 里的 agent —— 「薄 JS + 厚原生」路线的 JS 半边。
//
// 与 `pi-bundle/agent-main.js`（bun 路线）的关键差别：
//   · 这里**没有 pi-ai 的 provider 栈**，streamFn 直接把请求交给 Rust；
//   · 这里**没有 fs / 网络 / 定时器**，工具调用是一次同步的 host.callTool；
//   · 循环由宿主驱动：Rust 反复调 tick() 并泵微任务队列，guest 不自己等时钟。
//
// 结构对照 pocket-stack/pocket-pi 的 crates/pocket-pi-embedded/js/src/entry.ts
// （MIT，2026-09 的同类实现）——同一套「宿主 start* → 事件批量 poll」形态；
// 本文件按 pi-mobile 的 host 面（callTool 同步返回）与 pi-agent-core 0.84 重写。
//
// 打包含义见 js/build.sh；Rust 侧见 src/guest.rs。

import { Agent } from "../../../node_modules/@earendil-works/pi-agent-core/dist/agent.js";
import { AssistantMessageEventStream } from "../../../node_modules/@earendil-works/pi-ai/dist/utils/event-stream.js";

// ── 宿主面（Rust 注入 globalThis.host）────────────────────────────────
//   log(line)                         宿主日志
//   startModel(requestJson) -> id     发起一次模型请求（异步，结果经 poll 回来）
//   callTool(name, argsJson) -> json  执行工具（同步返回，复用 pi-host-tools）
//   poll() -> json[]                  取走一批宿主事件
const host = globalThis.host;

const emptyUsage = () => ({
  input: 0,
  output: 0,
  cacheRead: 0,
  cacheWrite: 0,
  reasoning: 0,
  totalTokens: 0,
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
});

// DeepSeek 的模型对象按 pi-ai 的 model catalog 手写（providers/data/deepseek.json）：
// 走这条路线就不再把 provider 目录打进 bundle，catalog 的字段在 Rust 侧同样用到
// （compat: max_tokens / system role / reasoning_content 必填）。
function modelFor(config) {
  return {
    id: config.model || "deepseek-v4-flash",
    name: config.model || "DeepSeek V4 Flash",
    provider: "deepseek",
    api: "openai-completions",
    baseUrl: config.baseUrl || "https://api.deepseek.com",
    reasoning: true,
    thinkingLevelMap: { off: null, minimal: null, low: "low", medium: null, high: "high", max: "max" },
    input: ["text"],
    cost: { input: 0.14, output: 0.28, cacheRead: 0.0028, cacheWrite: 0 },
    contextWindow: 1_000_000,
    maxTokens: 384_000,
  };
}

// ── 工具的 JS 半边：只是壳，实现在 Rust（pi-host-tools）─────────────────
function toolsFor(definitions) {
  return definitions.map((definition) => ({
    name: definition.name,
    label: definition.label || definition.name,
    description: definition.description || "",
    parameters: definition.parameters || { type: "object", properties: {} },
    executionMode: "sequential",
    // 同步调用 + 立即 resolve：工具是本地 fs 操作（微秒级），没有可并行的等待。
    // 真需要长任务（clone/网络）时再改成 startTool + poll，形态与模型请求一致。
    execute: (toolCallId, args) =>
      new Promise((resolve, reject) => {
        try {
          if (!host) throw new Error("QuickJS host is unavailable");
          const result = JSON.parse(host.callTool(definition.name, JSON.stringify(args || {})));
          if (result.isError) {
            reject(new Error(String(result.text || "tool failed")));
            return;
          }
          resolve({
            content: [{ type: "text", text: String(result.text || "") }],
            details: result.details,
            terminate: Boolean(result.terminate),
          });
        } catch (error) {
          reject(error instanceof Error ? error : new Error(String(error)));
        }
      }),
  }));
}

// ── 流式：把 Rust 报回来的增量喂进 pi-agent-core 要的 AssistantMessageEventStream ──
const pendingModels = new Map();
let agent = null;
let baseSystemPrompt = "";
let nextRequestId = 0;
const outbox = [];

function hostStream(model, context, options = {}) {
  const stream = new AssistantMessageEventStream();
  const partial = {
    role: "assistant",
    content: [],
    api: model.api,
    provider: model.provider,
    model: model.id,
    usage: emptyUsage(),
    stopReason: "stop",
    timestamp: Date.now(),
  };
  const pending = {
    stream,
    model,
    partial,
    started: false,
    thinkingStarted: false,
    textStarted: false,
    thinking: "",
    text: "",
  };
  try {
    if (!host) throw new Error("QuickJS host is unavailable");
    const id = host.startModel(JSON.stringify({ model, context, options }));
    pendingModels.set(id, pending);
  } catch (error) {
    pushModelError(stream, model, String(error));
  }
  return stream;
}

function ensureStarted(p) {
  if (p.started) return;
  p.started = true;
  p.stream.push({ type: "start", partial: { ...p.partial } });
}

function syncContent(p, toolCalls = []) {
  const content = [];
  if (p.thinkingStarted) {
    content.push({ type: "thinking", thinking: p.thinking, thinkingSignature: "reasoning_content" });
  }
  if (p.textStarted) content.push({ type: "text", text: p.text });
  for (const call of toolCalls) content.push(call);
  p.partial.content = content;
}

function pushThinkingDelta(p, delta) {
  if (!delta) return;
  ensureStarted(p);
  if (!p.thinkingStarted) {
    p.thinkingStarted = true;
    syncContent(p);
    p.stream.push({ type: "thinking_start", contentIndex: 0, partial: { ...p.partial } });
  }
  p.thinking += delta;
  syncContent(p);
  p.stream.push({ type: "thinking_delta", contentIndex: 0, delta, partial: { ...p.partial } });
}

function textIndex(p) {
  return p.thinkingStarted ? 1 : 0;
}

function pushTextDelta(p, delta) {
  if (!delta) return;
  ensureStarted(p);
  if (!p.textStarted) {
    p.textStarted = true;
    syncContent(p);
    p.stream.push({ type: "text_start", contentIndex: textIndex(p), partial: { ...p.partial } });
  }
  p.text += delta;
  syncContent(p);
  p.stream.push({ type: "text_delta", contentIndex: textIndex(p), delta, partial: { ...p.partial } });
}

function pushModelError(stream, model, message) {
  stream.push({
    type: "error",
    reason: "error",
    error: {
      role: "assistant",
      content: [],
      api: model.api,
      provider: model.provider,
      model: model.id,
      usage: emptyUsage(),
      stopReason: "error",
      errorMessage: message,
      timestamp: Date.now(),
    },
  });
}

/// 结果里若带全量文本/思考（可能是宿主把增量合并后的补全），只补差值不重复推。
function appendRemaining(current, complete, push, label) {
  if (current === complete) return;
  if (!complete.startsWith(current)) {
    throw new Error(`${label} stream does not match final result`);
  }
  push(complete.slice(current.length));
}

function finishModel(p, result) {
  ensureStarted(p);
  if (typeof result.thinking !== "string" || typeof result.text !== "string") {
    throw new Error("model result is missing thinking or text");
  }
  if (!Array.isArray(result.toolCalls)) throw new Error("model result is missing toolCalls");
  if (result.thinking && typeof result.thinkingSignature !== "string") {
    throw new Error("thinking result is missing thinkingSignature");
  }
  if (!["stop", "length", "toolUse", "error"].includes(result.stopReason)) {
    throw new Error(`model result has an invalid stopReason: ${result.stopReason}`);
  }
  if (result.stopReason === "error") {
    throw new Error(result.errorMessage || "model request failed");
  }
  appendRemaining(p.thinking, result.thinking, (d) => pushThinkingDelta(p, d), "thinking");
  appendRemaining(p.text, result.text, (d) => pushTextDelta(p, d), "text");

  const toolCalls = result.toolCalls;
  p.partial.usage = { ...emptyUsage(), ...result.usage };
  if (toolCalls.length > 0 && result.stopReason !== "toolUse") {
    throw new Error("tool calls require toolUse stopReason");
  }
  if (toolCalls.length === 0 && result.stopReason === "toolUse") {
    throw new Error("toolUse stopReason requires tool calls");
  }
  const blocks = toolCalls.map((call) => {
    if (!call.id || !call.name || !call.arguments || typeof call.arguments !== "object") {
      throw new Error("model result contains an invalid tool call");
    }
    return { type: "toolCall", id: call.id, name: call.name, arguments: call.arguments };
  });
  syncContent(p, blocks);

  if (p.thinkingStarted) {
    p.partial.content[0].thinkingSignature = result.thinkingSignature;
    p.stream.push({
      type: "thinking_end",
      contentIndex: 0,
      content: p.thinking,
      partial: { ...p.partial },
    });
  }
  if (p.textStarted) {
    p.stream.push({
      type: "text_end",
      contentIndex: textIndex(p),
      content: p.text,
      partial: { ...p.partial },
    });
  }
  for (let i = 0; i < blocks.length; i += 1) {
    const contentIndex = (p.thinkingStarted ? 1 : 0) + (p.textStarted ? 1 : 0) + i;
    p.stream.push({ type: "toolcall_start", contentIndex, partial: { ...p.partial } });
    p.stream.push({ type: "toolcall_end", contentIndex, toolCall: blocks[i], partial: { ...p.partial } });
  }
  if (p.partial.content.length === 0) throw new Error("model result contains no decision");

  p.partial.stopReason = result.stopReason;
  p.stream.push({ type: "done", reason: result.stopReason, message: { ...p.partial } });
}

// ── 宿主驱动的一拍 ────────────────────────────────────────────────────
function tick() {
  const batch = JSON.parse(host?.poll() || "[]");
  for (const event of batch) {
    const pending = pendingModels.get(event.id);
    if (event.type === "model_progress") {
      if (!pending) continue;
      pushThinkingDelta(pending, event.thinkingDelta);
      pushTextDelta(pending, event.textDelta);
    } else if (event.type === "model_done") {
      if (!pending) continue;
      pendingModels.delete(event.id);
      try {
        finishModel(pending, JSON.parse(event.result));
      } catch (error) {
        pushModelError(pending.stream, pending.model, String(error && error.message ? error.message : error));
      }
    } else if (event.type === "model_error") {
      if (!pending) continue;
      pendingModels.delete(event.id);
      pushModelError(pending.stream, pending.model, event.error);
    } else if (event.type === "log") {
      outbox.push({ type: "log", message: event.message });
    }
  }
}

function drain() {
  return JSON.stringify({
    phase: agent?.state.isStreaming ? "thinking" : agent ? "ready" : "idle",
    messages: agent?.state.messages.length || 0,
    pendingModels: pendingModels.size,
    events: outbox.splice(0, outbox.length),
  });
}

// ── 生命周期 ──────────────────────────────────────────────────────────
function boot(configJson) {
  const config = JSON.parse(configJson);
  const model = modelFor(config);
  const tools = toolsFor(config.tools || []);
  baseSystemPrompt = config.systemPrompt || "You are a coding agent on a mobile device.";
  agent = new Agent({
    initialState: {
      systemPrompt: baseSystemPrompt,
      model,
      thinkingLevel: config.thinkingLevel || "high",
      tools,
    },
    streamFn: hostStream,
    toolExecution: "sequential",
  });
  agent.subscribe((event) => {
    const compact = { type: event.type };
    if (event.type === "message_update") {
      compact.kind = event.assistantMessageEvent?.type;
      compact.delta = event.assistantMessageEvent?.delta;
    } else if (event.type === "message_end") {
      compact.role = event.message?.role;
      compact.stopReason = event.message?.stopReason;
      compact.errorMessage = event.message?.errorMessage;
    } else if (event.type === "tool_execution_start" || event.type === "tool_execution_end") {
      compact.name = event.toolName;
      compact.isError = Boolean(event.isError);
    }
    outbox.push(compact);
  });
  outbox.push({ type: "agent_ready" });
}

function prompt(text) {
  if (!agent) throw new Error("prompt before boot");
  // 不 await：prompt() 的 continuation 挂在微任务队列上，由宿主泵。
  void agent.prompt(text).then(
    () => outbox.push({ type: "agent_end" }),
    (error) => outbox.push({ type: "agent_error", message: String(error) }),
  );
}

globalThis.__spike = { boot, prompt, tick, drain };
