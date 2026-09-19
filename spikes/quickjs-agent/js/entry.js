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
import { JsonlSessionRepo } from "../../../node_modules/@earendil-works/pi-agent-core/dist/harness/session/jsonl/repo.js";
import { FileError } from "../../../node_modules/@earendil-works/pi-agent-core/dist/harness/types.js";
import { AssistantMessageEventStream } from "../../../node_modules/@earendil-works/pi-ai/dist/utils/event-stream.js";

// ── 宿主面（Rust 注入 globalThis.host）────────────────────────────────
//   log(line)                         宿主日志
//   startModel(requestJson) -> id     发起一次模型请求（异步，结果经 poll 回来）
//   ensureApproval(callId,name,args)  审批握手（同步返回 id，决策经 poll 回来）
//   callTool(callId,name,argsJson)    执行工具（同步返回；Rust 侧校验执行权）
//   fs(op, payloadJson)               pi 的 12 个 fs 方法（会话持久化用）
//   goalGet()                         持久目标（goal.json，由宿主持有）
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
//
// 每次调用都要先过**审批握手**：策略与分档都在 Rust（approval.rs），JS 不做判断，
// 只是「问 → 等 → 再执行」。执行权记在 callId 上，所以绕过这段代码也没用 ——
// host.callTool 会拒绝没有握手的调用。
const pendingApprovals = new Map();

function requestApproval(toolCallId, name, args) {
  return new Promise((resolve, reject) => {
    try {
      if (!host) throw new Error("QuickJS host is unavailable");
      const id = host.ensureApproval(toolCallId, name, JSON.stringify(args || {}));
      pendingApprovals.set(id, { toolCallId, name, resolve, reject });
    } catch (error) {
      reject(error instanceof Error ? error : new Error(String(error)));
    }
  });
}

function toolsFor(definitions) {
  return definitions.map((definition) => ({
    name: definition.name,
    label: definition.label || definition.name,
    description: definition.description || "",
    parameters: definition.parameters || { type: "object", properties: {} },
    executionMode: "sequential",
    execute: async (toolCallId, args) => {
      // 1. 握手（Rust 决定：auto 档当场放行；write/edit/rm 则会问用户）
      const decision = await requestApproval(toolCallId, definition.name, args);
      if (decision !== "allow") {
        throw new Error(`${definition.name}: denied by the user`);
      }
      // 2. 执行（Rust 侧还会再校验一次执行权）
      const result = JSON.parse(host.callTool(toolCallId, definition.name, JSON.stringify(args || {})));
      if (result.isError) throw new Error(String(result.text || "tool failed"));
      return {
        content: [{ type: "text", text: String(result.text || "") }],
        details: result.details,
        terminate: Boolean(result.terminate),
      };
    },
  }));
}

// ── 会话持久化（pi-v4 JSONL，与 App 同一份 repo 实现）──────────────────
//
// 用的是上游 `JsonlSessionRepo` + `Session`，fs 后端全在 Rust（`host.fs` →
// `pi-host-tools::sessions_fs`）。因此**会话文件格式与 App 完全一致**，两边可以
// 互相打开 —— 这正是「复用而不是重写」在会话这一层的价值。
//
// 接法照搬 pi-bundle/agent-main.js（设备验证过的那份）：净化 undefined → appendMessage；
// assistant 在 message_end 落盘、toolResult 在 turn_end 落盘；恢复时 findEntries
// 按 seq 升序回放。
const SESSIONS_ROOT = "/pi-sessions"; // 与 pi-host-tools::SESSIONS_VIRTUAL_ROOT 一致

const fsOk = (value) => ({ ok: true, value });
const fsFail = (code, message, path) => ({ ok: false, error: new FileError(code, message, path) });

function fsCall(op, args) {
  const result = JSON.parse(host.fs(op, JSON.stringify(args || {})));
  if (result.ok) return result.value;
  throw new FileError(result.error?.code ?? "unknown", result.error?.message ?? "fs error", args?.path);
}

const stripRoot = (p) =>
  p === SESSIONS_ROOT ? "" : p.startsWith(`${SESSIONS_ROOT}/`) ? p.slice(SESSIONS_ROOT.length + 1) : null;

// repo 传 ["/", root, dir, file] 之类的混合段：逐段去斜杠再折叠，
// 避免 "/pi-sessions" 前面叠出 "//" 让 stripRoot 失配。
// ⚠️ **不要**再拼一次 SESSIONS_ROOT —— parts 里已经含它了（第一版就是在这里
// 叠出 "/pi-sessions/pi-sessions/..."，createDir 直接 ENOENT、会话静默不落盘）。
const joinParts = (parts) =>
  parts
    .map((s) => String(s ?? "").replace(/^\/+|\/+$/g, ""))
    .filter((s) => s !== "" && s !== ".")
    .join("/");

/// 每个 fs 方法都要把异常转成 pi 的 `{ok:false,error}` 形状（repo 靠它判断
/// 「不存在」这类正常状态，直接抛会变成会话级失败）。
const toFsResult = (path, fn) => {
  try {
    return fsOk(fn());
  } catch (error) {
    return fsFail(error?.code ?? "unknown", error?.message ?? String(error), path);
  }
};

const hostFs = {
  async absolutePath(path) {
    return fsOk(String(path).startsWith("/") ? String(path) : `${SESSIONS_ROOT}/${path}`);
  },
  async joinPath(parts) {
    return fsOk(`/${joinParts(parts)}`);
  },
  async readTextFile(path) {
    return toFsResult(path, () => fsCall("readTextFile", { path: needRel(path) }));
  },
  async readTextLines(path, options) {
    return toFsResult(path, () => fsCall("readTextLines", { path: needRel(path), maxLines: options?.maxLines }));
  },
  async writeFile(path, content) {
    if (typeof content !== "string") {
      return fsFail("not_supported", "binary write unsupported over hostcall", path);
    }
    return toFsResult(path, () => fsCall("writeFile", { path: needRel(path), content }));
  },
  async appendFile(path, content) {
    if (typeof content !== "string") {
      return fsFail("not_supported", "binary append unsupported over hostcall", path);
    }
    return toFsResult(path, () => fsCall("appendFile", { path: needRel(path), content }));
  },
  async renameFile(sourcePath, destinationPath) {
    return toFsResult(sourcePath, () =>
      fsCall("renameFile", { path: needRel(sourcePath), to: needRel(destinationPath) }),
    );
  },
  async fileInfo(path) {
    return toFsResult(path, () => fsCall("fileInfo", { path: needRel(path) }));
  },
  async listDir(path) {
    return toFsResult(path, () => fsCall("listDir", { path: needRel(path) }));
  },
  async exists(path) {
    return toFsResult(path, () => fsCall("exists", { path: needRel(path) }));
  },
  async createDir(path, options) {
    return toFsResult(path, () => fsCall("createDir", { path: needRel(path), recursive: Boolean(options?.recursive) }));
  },
  async remove(path, options) {
    return toFsResult(path, () =>
      fsCall("remove", { path: needRel(path), recursive: Boolean(options?.recursive), force: Boolean(options?.force) }),
    );
  },
};

function needRel(p) {
  const rel = stripRoot(String(p));
  if (rel === null) throw new FileError("invalid", `path outside the sessions namespace: ${p}`, String(p));
  return rel;
}

const repo = new JsonlSessionRepo({ fs: hostFs, sessionsRoot: SESSIONS_ROOT });
let session = null;
let sessionId = null;
let restoredMessages = [];
let sessionPromise = null;

function persistMessage(message) {
  if (!session || !message) return;
  // agent 消息带显式 undefined 属性（如 toolResult 的 usage），pi 的
  // assertJsonSerializable 会直接拒绝 —— JSON 一轮净化：undefined 被丢弃。
  const clean = JSON.parse(JSON.stringify(message));
  session.appendMessage(clean).catch((error) => {
    outbox.push({ type: "session_error", error: String(error?.message ?? error) });
  });
}

function ensureSession() {
  // single-flight：prompt 与 message_end 同帧时只建一个会话
  if (session) return Promise.resolve(session);
  if (!sessionPromise) {
    sessionPromise = repo
      .create({ cwd: config_workspace() })
      .then(async (created) => {
        session = created;
        sessionId = (await created.getMetadata()).id;
        outbox.push({ type: "session_created", sessionId });
        return created;
      })
      .catch((error) => {
        sessionPromise = null;
        throw error;
      });
  }
  return sessionPromise;
}

/// 恢复最新会话（`--resume`）：把消息灌回 agent，并让 todo 状态随消息重建。
async function restoreLatestSession() {
  const metas = await repo.list();
  if (!metas.length) {
    outbox.push({ type: "session_restore", found: 0 });
    return;
  }
  metas.sort((a, b) => b.modifiedAt - a.modifiedAt);
  const latest = metas[0];
  session = await repo.open(latest);
  sessionId = latest.id;
  const entries = await session.findEntries();
  // findEntries 新序列在前（App 真机实测），回放按 seq 升序
  restoredMessages = entries
    .filter((e) => e.type === "message" && e.message)
    .sort((a, b) => (a.seq ?? 0) - (b.seq ?? 0))
    .map((e) => e.message);
  if (restoredMessages.length && agent) agent.state.messages = restoredMessages;
  replayTodos(restoredMessages);
  outbox.push({
    type: "session_restore",
    found: metas.length,
    sessionId: latest.id,
    messages: restoredMessages.length,
  });
}

/// 持久目标（pi-goal 移动原生化）：目标存在宿主侧（goal.json），
/// JS 只负责把它拼进 systemPrompt —— 与 App 同一分工。
let currentGoal = null;

// ── todo（@juicesharp/rpiv-todo 移动原生化）──────────────────────────
//
// 从 pi-bundle/agent-main.js 移植语义（4 态状态机 + blockedBy 依赖校验 +
// 6 动作 + 状态从会话消息回放重建）。⚠️ 这里是**移植**不是共享：真正的产品形态
// 应该让两个 bundle import 同一份实现，spike 阶段先把语义对齐。
const TODO_TRANSITIONS = {
  pending: ["in_progress", "completed", "deleted"],
  in_progress: ["pending", "completed", "deleted"],
  completed: ["deleted"],
  deleted: [],
};

let todoState = { tasks: [], nextId: 1 };

const todoTask = (id) => todoState.tasks.find((t) => t.id === id);

/// blockedBy 校验（先校验后变更，拒绝时状态不动）：依赖须存在且非墓碑、
/// 不得自阻塞、新增边不得成环。
function todoDepError(id, deps) {
  for (const d of deps) {
    const dep = todoTask(d);
    if (!dep) return `blockedBy: #${d} not found`;
    if (dep.status === "deleted") return `blockedBy: #${d} is deleted`;
    if (dep.id === id) return `cannot block #${id} on itself`;
  }
  const seen = new Set();
  const stack = [...deps];
  while (stack.length) {
    const cur = todoTask(stack.pop());
    if (!cur || seen.has(cur.id)) continue;
    if (cur.id === id) return "would create a cycle in the blockedBy graph";
    seen.add(cur.id);
    stack.push(...(cur.blockedBy || []));
  }
  return null;
}

function todoSnapshot() {
  return { tasks: todoState.tasks.map((t) => ({ ...t })), nextId: todoState.nextId };
}

function todoUpdated() {
  outbox.push({ type: "todo_updated", ...todoSnapshot() });
}

/// 从会话消息回放 todo 状态（每个 toolResult 的 details 带全量快照）。
function replayTodos(messages) {
  for (const message of messages) {
    if (message.role !== "toolResult" || message.toolName !== "todo") continue;
    const text = (message.content || [])
      .filter((c) => c.type === "text")
      .map((c) => c.text)
      .join("");
    const snapshot = message.details?.todoState;
    if (snapshot) todoState = { tasks: snapshot.tasks || [], nextId: snapshot.nextId || 1 };
    void text;
  }
  todoUpdated();
}

function todoExecute(action, args) {
  const fail = (message) => ({ content: [{ type: "text", text: message }], details: {}, isError: true });
  const ok = (text) => ({
    content: [{ type: "text", text }],
    details: { todoState: todoSnapshot() },
    isError: false,
  });

  switch (action) {
    case "create": {
      const created = [];
      for (const item of args.tasks || []) {
        if (!item?.subject) return fail("create: each task needs a subject");
        const task = {
          id: todoState.nextId++,
          subject: item.subject,
          description: item.description,
          activeForm: item.activeForm,
          status: "pending",
          blockedBy: [],
        };
        todoState.tasks.push(task);
        if (item.blockedBy?.length) {
          const error = todoDepError(task.id, item.blockedBy);
          if (error) return fail(`create #${task.id}: ${error}`);
          task.blockedBy = [...item.blockedBy];
        }
        created.push(`#${task.id} ${task.subject}`);
      }
      todoUpdated();
      return ok(`Created ${created.length} task(s): ${created.join(", ")}`);
    }
    case "update": {
      const task = todoTask(args.id);
      if (!task) return fail(`update: #${args.id} not found`);
      const mutable = ["status", "subject", "description", "activeForm"];
      if (!mutable.some((field) => args[field] !== undefined) && !args.addBlockedBy && !args.removeBlockedBy) {
        return fail("update: needs a mutable field (status/subject/description/activeForm/blockedBy)");
      }
      if (args.status !== undefined) {
        const allowed = TODO_TRANSITIONS[task.status] || [];
        if (!allowed.includes(args.status)) {
          return fail(`update #${task.id}: ${task.status} → ${args.status} is not a legal transition`);
        }
      }
      let deps = task.blockedBy || [];
      if (args.addBlockedBy?.length) {
        deps = [...new Set([...deps, ...args.addBlockedBy])];
        const error = todoDepError(task.id, args.addBlockedBy);
        if (error) return fail(`update #${task.id}: ${error}`);
      }
      if (args.removeBlockedBy?.length) deps = deps.filter((d) => !args.removeBlockedBy.includes(d));
      if (args.subject !== undefined) task.subject = args.subject;
      if (args.description !== undefined) task.description = args.description;
      if (args.activeForm !== undefined) task.activeForm = args.activeForm;
      task.blockedBy = deps;
      if (args.status !== undefined) task.status = args.status;
      todoUpdated();
      return ok(`Updated #${task.id} → ${task.status}`);
    }
    case "list": {
      const includeDeleted = Boolean(args.includeDeleted);
      const visible = todoState.tasks.filter(
        (t) => (includeDeleted || t.status !== "deleted") && (!args.status || t.status === args.status),
      );
      if (!visible.length) return ok("(no tasks)");
      return ok(
        visible
          .map((t) => `#${t.id} [${t.status}] ${t.subject}${t.blockedBy?.length ? ` (blocked by ${t.blockedBy.join(", ")})` : ""}`)
          .join("\n"),
      );
    }
    case "get": {
      const task = todoTask(args.id);
      return task ? ok(JSON.stringify(task)) : fail(`get: #${args.id} not found`);
    }
    case "delete": {
      const task = todoTask(args.id);
      if (!task) return fail(`delete: #${args.id} not found`);
      task.status = "deleted";
      todoUpdated();
      return ok(`Deleted #${task.id} (tombstone)`);
    }
    case "clear": {
      todoState = { tasks: [], nextId: todoState.nextId };
      todoUpdated();
      return ok("Cleared all tasks");
    }
    default:
      return fail(`todo: unknown action ${JSON.stringify(action)}`);
  }
}

const TODO_PROMPT_GUIDELINES = [
  "Use `todo` for complex work with 3+ steps, when the user gives you a list of tasks, or immediately after receiving new instructions to capture requirements.",
  "When starting a task from the todo list, mark it in_progress BEFORE beginning work. Mark it completed IMMEDIATELY when done — never batch completions.",
  "Never mark a task completed if the implementation is partial or you hit unresolved errors — keep it in_progress and create a new task for the blocker instead.",
  "To change a task's status, call update with the task id and the target status, e.g. {\"action\":\"update\",\"id\":3,\"status\":\"completed\"}.",
];

const todoTool = {
  name: "todo",
  label: "Todo",
  description: [
    "Manage a task list to track multi-step progress. Actions: create / update / list / get / delete / clear.",
    "Status is a 4-state machine: pending → in_progress → completed, plus deleted as a tombstone.",
    "blockedBy expresses dependencies; use addBlockedBy / removeBlockedBy on update (additive merge). Cycles are rejected.",
    `Guidelines:\n${TODO_PROMPT_GUIDELINES.map((g) => `- ${g}`).join("\n")}`,
  ].join(" "),
  parameters: {
    type: "object",
    properties: {
      action: { type: "string", enum: ["create", "update", "list", "get", "delete", "clear"] },
      id: { type: "number" },
      status: { type: "string", enum: ["pending", "in_progress", "completed", "deleted"] },
      subject: { type: "string" },
      description: { type: "string" },
      activeForm: { type: "string" },
      tasks: { type: "array", items: { type: "object" } },
      blockedBy: { type: "array", items: { type: "number" } },
      addBlockedBy: { type: "array", items: { type: "number" } },
      removeBlockedBy: { type: "array", items: { type: "number" } },
      includeDeleted: { type: "boolean" },
    },
    required: ["action"],
    additionalProperties: false,
  },
  executionMode: "sequential",
  // 纯 JS 工具：不碰 host（与 App 一致 —— 上游把 todo 做成纯 JS 工具，状态从
  // 会话消息回放重建，不写磁盘、不需要审批）。
  execute: async (_toolCallId, args) => todoExecute(args?.action, args || {}),
};

// ── system prompt 组装：基础 + 持久目标 + todo 引导 ────────────────────
function composeSystemPrompt(base) {
  let prompt = base.trim();
  prompt += `\n\n# Todo list\n\nManage a task list to track multi-step progress (the \`todo\` tool):\n${TODO_PROMPT_GUIDELINES.map((g) => `- ${g}`).join("\n")}`;
  if (currentGoal) {
    prompt += `\n\n# Current goal\n\nWork persistently toward this objective across turns until the user clears it: ${currentGoal}`;
  }
  return prompt;
}

let baseSystemPrompt = "";
let workspaceForSessions = "";

function config_workspace() {
  return workspaceForSessions || "/workspace";
}

// ── 流式：把 Rust 报回来的增量喂进 pi-agent-core 要的 AssistantMessageEventStream ──
const pendingModels = new Map();
let agent = null;
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
    } else if (event.type === "approval_decision") {
      const waiting = pendingApprovals.get(event.id);
      if (!waiting) continue;
      pendingApprovals.delete(event.id);
      waiting.resolve(event.decision);
      outbox.push({
        type: "approval_resolved",
        tool: event.tool || waiting.name,
        decision: event.decision,
        reason: event.reason,
      });
    } else if (event.type === "approval_request") {
      // 交给宿主/UI 展示；这里只让事件流里看得见（App 里就是弹卡那一刻）。
      outbox.push({
        type: "approval_request",
        tool: event.tool,
        tier: event.tier,
        summary: event.summary,
      });
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
  // Rust 提供的文件工具 + JS 侧的 todo（纯 JS 工具，与 App 同一分工）
  const tools = [...toolsFor(config.tools || []), todoTool];
  baseSystemPrompt = config.systemPrompt || "You are a coding agent on a mobile device.";
  workspaceForSessions = config.workspace || "/workspace";
  currentGoal = config.goal || null;
  agent = new Agent({
    initialState: {
      systemPrompt: composeSystemPrompt(baseSystemPrompt),
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
      // D3 落盘：assistant 消息在 message_end 追加（toolResult 走 turn_end，免双写）
      if (event.message?.role === "assistant") {
        ensureSession()
          .then(() => persistMessage(event.message))
          .catch((error) => outbox.push({ type: "session_error", error: String(error?.message ?? error) }));
      }
    } else if (event.type === "turn_end") {
      for (const result of event.toolResults ?? []) persistMessage(result);
    } else if (event.type === "tool_execution_start" || event.type === "tool_execution_end") {
      compact.name = event.toolName;
      compact.isError = Boolean(event.isError);
    }
    outbox.push(compact);
  });
  outbox.push({ type: "agent_ready" });
  // 恢复由宿主触发（`--resume` 时才调 restore），因为「哪个会话」是宿主的选择。
}

/// `--resume` 时由宿主调用：把最新会话的消息灌回 agent。
function restore() {
  if (!agent) throw new Error("restore before boot");
  void restoreLatestSession().then(
    () => outbox.push({ type: "restore_done" }),
    (error) => outbox.push({ type: "session_error", error: `restore: ${error?.message ?? error}` }),
  );
}

function prompt(text) {
  if (!agent) throw new Error("prompt before boot");
  // 用户消息先落会话（与会话创建 single-flight，顺序由存储内部队列保证）
  ensureSession()
    .then((s) => s.appendMessage({ role: "user", content: text, timestamp: Date.now() }))
    .catch((error) => outbox.push({ type: "session_error", error: String(error?.message ?? error) }));
  // 不 await：prompt() 的 continuation 挂在微任务队列上，由宿主泵。
  void agent.prompt(text).then(
    () => outbox.push({ type: "agent_end" }),
    (error) => outbox.push({ type: "agent_error", message: String(error) }),
  );
}

/// 会话 id（宿主在结束时打印，便于下一轮 `--resume`）。
function sessionInfo() {
  return JSON.stringify({
    sessionId,
    restoredMessages: restoredMessages.length,
    messages: agent?.state.messages.length ?? 0,
    todos: todoState.tasks.length,
    goal: currentGoal,
  });
}

globalThis.__spike = { boot, prompt, tick, drain, restore, sessionInfo };
