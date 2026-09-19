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
//   http(callId, paramsJson)          出网请求（需该 callId 已握手；实现在 Rust）
//   mcpConfig()                       已配置的 MCP 服务器列表（宿主读文件）
//   skillsConfig()                    启用中的技能（复用 App 的注入半）
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
const pendingAsks = new Map();

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

// 内部读取（AGENTS.md、agents/*.md）也要走**同一套握手**：宿主侧的边界不能因为
// 「这是框架自己在读文件」就绕开。callId 用内部前缀，与 agent 的 callId 不冲突。
let internalCallSeq = 0;

async function internalRead(path) {
  const callId = `host-internal-${++internalCallSeq}`;
  const args = { path };
  await requestApproval(callId, "read", args);
  const result = JSON.parse(host.callTool(callId, "read", JSON.stringify(args)));
  if (result.isError) throw new Error(String(result.text || "read failed"));
  return String(result.text || "");
}

/// fetch 工具：网络在 Rust 侧（SSRF 防护、30s 超时、256KB 上限、HTML→文本）。
/// 只读，tier = auto（与 App 的 fetchTool 同档）。
const fetchTool = {
  name: "fetch",
  label: "Fetch",
  description:
    "Fetch an http/https URL from the open web and return its body as text (HTML pages are converted to readable text, 30s timeout, large bodies truncated). Read-only — no approval needed. Args: {url, method?, headers?, body?}",
  parameters: {
    type: "object",
    properties: {
      url: { type: "string" },
      method: { type: "string" },
      headers: { type: "object" },
      body: { type: "string" },
    },
    required: ["url"],
    additionalProperties: false,
  },
  executionMode: "sequential",
  execute: async (toolCallId, params) => {
    const callId = `fetch-${toolCallId}`;
    await requestApproval(callId, "fetch", params);
    const r = JSON.parse(host.http(callId, JSON.stringify(params || {})));
    if (r.error) return { content: [{ type: "text", text: `Fetch failed: ${r.error}` }], details: {} };
    const meta = [`status ${r.status}`, r.contentType || "no content-type"];
    if (r.truncated) meta.push("truncated at 256KB");
    return { content: [{ type: "text", text: `[${meta.join(" · ")}]\n\n${r.body ?? ""}` }], details: {} };
  },
};

/// agent 反问用户（pi-ask-user 原生化）：与审批同一套「id 出去、事件回来」。
/// 无人值守时宿主按 --yes/--deny 自动作答，所以这条路径在自动化里也能验。
const askUserTool = {
  name: "ask_user",
  label: "Ask User",
  description:
    "Ask the user a question with optional multiple-choice answers. Use when the user's intent is ambiguous, when a decision requires explicit input, or when multiple valid options exist. Ask exactly ONE focused question per call; before calling, gather context with tools and pass a short summary via context. The user must answer before the run continues.",
  parameters: {
    type: "object",
    properties: {
      question: { type: "string", description: "The question to ask the user" },
      context: { type: "string", description: "Relevant context to show before the question" },
      options: { type: "array", items: { type: "string" }, description: "Optional multiple-choice answers" },
      allowMultiple: { type: "boolean" },
      allowFreeform: { type: "boolean" },
      allowComment: { type: "boolean" },
    },
    required: ["question"],
    additionalProperties: false,
  },
  executionMode: "sequential",
  execute: async (_toolCallId, params) => {
    const id = host.askUser(
      JSON.stringify({
        question: params?.question ?? "",
        context: params?.context ?? "",
        options: params?.options ?? [],
        allowFreeform: params?.allowFreeform ?? true,
      }),
    );
    const reply = await new Promise((resolve) => pendingAsks.set(id, { resolve }));
    if (reply.cancelled) {
      return { content: [{ type: "text", text: "User cancelled the question (no answer)." }], details: {} };
    }
    return { content: [{ type: "text", text: String(reply.answer ?? "") }], details: {} };
  },
};

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
  // 从恢复的历史里取回上下文水位 —— 不取的话 `--resume` 之后的**第一轮**永远
  // 够不到压缩阈值（水位只在 assistant message_end 时才写，而 CLI 一进程一轮）。
  // 语义等同 pi 的 getLastAssistantUsage：以最后一条 assistant 的 usage 为准。
  // ⚠️ 这一条比 App 那份更严：那边 resume 后的第一轮同样处于盲区。
  for (const message of [...restoredMessages].reverse()) {
    if (message.role !== "assistant" || !message.usage) continue;
    lastContextTokens =
      message.usage.totalTokens ?? (message.usage.input ?? 0) + (message.usage.output ?? 0);
    break;
  }
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

// ── subagents（pi-subagents 移动原生化）────────────────────────────────
//
// 移植自 pi-bundle/agent-main.js：内置 delegate/researcher/reviewer，外加
// workspace/agents/*.md 自定义定义（上游同格式：markdown + frontmatter）。
// 子代理是**独立上下文 + 受限工具集**的嵌套 Agent，跑完把最终回复作为工具结果返回。
// 关键性质（与本 spike 的桥天然契合）：子代理的工具调用也走 host.callTool，
// 因此**同样受宿主审批分档管辖** —— 子代理写文件一样会弹审批。
const BUILTIN_AGENTS = {
  delegate: {
    name: "delegate",
    description: "General-purpose helper subagent; inherits the parent tool set (minus delegation)",
    systemPromptMode: "append",
    tools: ["read", "write", "edit", "ls", "grep"],
    thinking: "low",
    body: "You are a delegated agent. Execute the assigned task using the provided tools. Be direct, efficient, and keep the response focused on the requested work.",
  },
  researcher: {
    name: "researcher",
    description: "Read-only research subagent; investigates and reports findings with evidence",
    systemPromptMode: "replace",
    tools: ["read", "ls", "grep"],
    thinking: "low",
    body: "You are a research subagent. Investigate using read-only tools (read/ls/grep) and report findings with evidence. You do not guess; you verify from the code, tests, or docs. Be concise and structured.",
  },
  reviewer: {
    name: "reviewer",
    description: "Review specialist for diffs, plans, and proposed solutions",
    systemPromptMode: "replace",
    tools: ["read", "ls", "grep"],
    thinking: "low",
    body: "You are a disciplined review subagent. Inspect, evaluate, and report findings with evidence. Verify implementation matches intent, code handles edge cases, and tests cover changes. Report issues by severity.",
  },
};

function parseAgentDef(text) {
  const m = text.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n?([\s\S]*)$/);
  if (!m) return null;
  const meta = {};
  for (const line of m[1].split("\n")) {
    const kv = line.match(/^(\w+):\s*(.*)$/);
    if (kv) meta[kv[1].trim()] = kv[2].trim();
  }
  return {
    name: meta.name,
    description: meta.description ?? "custom subagent",
    systemPromptMode: meta.systemPromptMode === "append" ? "append" : "replace",
    tools: (meta.tools ?? "").split(",").map((x) => x.trim()).filter(Boolean),
    thinking: meta.thinking ?? "minimal",
    body: m[2].trim(),
  };
}

async function loadAgentDefs() {
  const defs = {};
  for (const [name, def] of Object.entries(BUILTIN_AGENTS)) defs[name] = { ...def, name };
  try {
    const listing = await internalRead("agents");
    if (listing && listing !== "(empty)") {
      for (const line of listing.split("\n")) {
        const file = line.replace(/^-\s*/, "").trim();
        if (!file.endsWith(".md")) continue;
        const def = parseAgentDef(await internalRead(`agents/${file}`));
        if (def?.name) defs[def.name] = def;
      }
    }
  } catch {
    // workspace 里没有 agents/ 目录是常态，不是错误
  }
  return defs;
}

/// content 归一化成纯文本：字符串原样，content block 数组取 text 块。
/// （user 消息是字符串、assistant 是 block 数组 —— 两种都要能吃。）
function textOfContent(content) {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .filter((c) => c?.type === "text")
    .map((c) => c.text)
    .join(" ");
}

/// 子代理可用工具：从主 agent 当前工具集里取子集，且**禁用嵌套委托**（递归防护）。
function resolveSubTools(names) {
  const available = agent?.state.tools ?? [];
  return names
    .filter((name) => name !== "subagent")
    .map((name) => available.find((t) => t.name === name))
    .filter(Boolean);
}

/// 一次「收结果」的嵌套 run（压缩摘要与子代理共用）。
async function runNestedCollect(prompt, toolNames, systemPrompt, thinking) {
  const sub = new Agent({
    initialState: {
      model: agent.state.model,
      thinkingLevel: thinking ?? "minimal",
      systemPrompt,
      tools: resolveSubTools(toolNames),
    },
    streamFn: hostStream,
    toolExecution: "sequential",
  });
  await sub.prompt(prompt);
  const messages = sub.state.messages ?? [];
  const last = [...messages].reverse().find((m) => m.role === "assistant");
  return textOfContent(last?.content ?? []).trim();
}

const subagentTool = {
  name: "subagent",
  label: "Subagent",
  description:
    "Delegate a focused task to a named subagent. The subagent runs with its own context and a restricted tool set, then its final response is returned as this tool's result. Use for research, review, or self-contained subtasks that would otherwise pollute the main conversation. Available agents are listed in the error message when unknown.",
  parameters: {
    type: "object",
    properties: {
      agent: { type: "string", description: "Name of the subagent to run (e.g. delegate, researcher, reviewer)" },
      task: { type: "string", description: "Complete, self-contained task description for the subagent" },
    },
    required: ["agent", "task"],
    additionalProperties: false,
  },
  // 与上游一致：委托运行期间阻塞同回合其他工具
  executionMode: "sequential",
  execute: async (_toolCallId, params) => {
    const fail = (text) => ({ content: [{ type: "text", text }], details: {} });
    try {
      const defs = await loadAgentDefs();
      const def = defs[params.agent];
      if (!def) {
        return fail(`Unknown subagent "${params.agent}". Available: ${Object.keys(defs).join(", ")}`);
      }
      outbox.push({ type: "subagent_start", name: def.name, task: params.task });
      const text = await runNestedCollect(
        params.task,
        def.tools,
        def.systemPromptMode === "append" ? `${baseSystemPrompt}\n\n${def.body}` : def.body,
        def.thinking,
      );
      outbox.push({ type: "subagent_end", name: def.name });
      return { content: [{ type: "text", text: text || "(subagent returned no text)" }], details: {} };
    } catch (error) {
      outbox.push({ type: "subagent_end", name: params.agent });
      return fail(`Error: ${error?.message ?? error}`);
    }
  },
};

// ── auto-compaction（会话 token 自动压缩）─────────────────────────────
//
// 与 App 同一策略：上下文水位超过窗口的 60% 时，把较早的消息压成一段摘要
// （经只读嵌套 Agent 生成），保留最近 8 条；JSONL 仍保留完整历史。
// `--compact-at <tokens>` 可显式给阈值 —— 否则真跑一轮永远够不到 100 万 token 的 60%，
// 这条路径就没法验（可测性优先于「参数看起来多余」）。
const COMPACT_RATIO = 0.6;
const COMPACT_KEEP = 8;
let lastContextTokens = 0;
let compactThreshold = 0;

async function autoCompactIfNeeded() {
  const windowSize = agent?.state.model?.contextWindow ?? 128_000;
  const threshold = compactThreshold || windowSize * COMPACT_RATIO;
  const messages = agent.state.messages ?? [];
  // 判据必须可见：压缩不触发时要能一眼看出是水位不够、阈值没传到、还是消息太少。
  // （同 D16 的教训：先加自检，别围着推断改。）
  outbox.push({
    type: "compaction_check",
    tokens: lastContextTokens,
    threshold,
    messages: messages.length,
    keep: COMPACT_KEEP,
  });
  if (!lastContextTokens || lastContextTokens < threshold) return;
  if (messages.length <= COMPACT_KEEP + 2) return;
  outbox.push({ type: "compaction_start", tokens: lastContextTokens, messages: messages.length });
  // 步骤标记：压缩是「压缩期间还能报点东西」的唯一路径，卡住时靠它定位
  // （QuickJS 的 error.stack 是空的，JS 侧异常只能这样二分）。
  const step = (n) => outbox.push({ type: "compaction_step", step: n });
  step("collecting");
  const older = messages.slice(0, -COMPACT_KEEP);
  const keep = messages.slice(-COMPACT_KEEP);
  const transcript = older
    .map((m) => {
      // ⚠️ user 消息的 content 是**字符串**，不是 content block 数组 —— 直接
      // `.filter` 会 `not a function`。App 那份 bundle 同一段也这么写（见 README
      // 「对齐时发现的问题」）：它的阈值是 100 万 token 的 60%，实际跑不到，所以一直没暴露。
      const text = textOfContent(m.content);
      return `${m.role}: ${text.slice(0, 600)}`;
    })
    .filter((line) => !line.endsWith(": "))
    .join("\n");
  step("summarizing");
  const summary = await runNestedCollect(
    `Summarize this conversation segment for continuation. Keep: user goals and decisions, file paths touched, key outcomes, open tasks. Be dense.\n\n${transcript.slice(0, 60_000)}`,
    [],
    "You compress conversation segments into dense continuation summaries.",
    "minimal",
  );
  step("replacing");
  agent.state.messages = [
    {
      role: "user",
      content: `[auto-compacted] Summary of the earlier conversation:\n${summary}`,
      timestamp: Date.now(),
    },
    ...keep,
  ];
  lastContextTokens = Math.round(lastContextTokens * 0.3);
  outbox.push({ type: "compaction_done", summarized: older.length, kept: keep.length });
}

// ── MCP 适配器（pi-mcp-adapter 移动原生化，仅 streamable-http）──────────
//
// 移植自 pi-bundle/agent-main.js，**唯一的结构差异**：那里用 JS 原生 fetch，
// 这里没有 fetch —— 全部出网走 `host.http(callId, params)`。两个后果：
//   1. 流式响应要用 `readMode: "first-event"`：MCP 的 SSE 可能一直挂着不关，
//      默认的缓冲模式会一路挂到 30s 超时；
//   2. 响应头要能从宿主拿回来（`mcp-session-id` 靠它串后续请求）。
// 这两条都是为 MCP 给 `pi-host-tools::http` 加的，对 fetch 工具透明。
const MCP_PROTOCOL_VERSION = "2025-06-18";

function mcpClient(name, url, headers, timeoutMs) {
  let nextId = 1;
  let sessionId = null;
  const timeout = timeoutMs > 0 ? timeoutMs : 30_000;
  let callSeq = 0;

  function post(body, readMode) {
    const callId = `mcp-${name}-${++callSeq}`;
    return { callId, payload: { url, method: "POST", headers: { ...(headers ?? {}) }, body, readMode } };
  }

  async function rpc(method, params) {
    const id = nextId++;
    const body = JSON.stringify({ jsonrpc: "2.0", id, method, params: params ?? {} });
    const hdrs = {
      "content-type": "application/json",
      accept: "application/json, text/event-stream",
      ...(sessionId ? { "mcp-session-id": sessionId } : {}),
    };
    const { callId, payload } = post(body);
    payload.headers = hdrs;
    payload.timeoutMs = timeout;
    void payload.timeoutMs; // 宿主侧超时固定 30s；这里保留字段以便将来透传

    // 握手用工具名 = 具体 MCP 工具名，档位由 Rust 的 tier_for("mcp__…") 决定（ask）
    const approveAs = method === "tools/call" ? `mcp__${name}__${params?.name ?? ""}` : `mcp__${name}__*`;
    await requestApproval(callId, approveAs, params ?? {});

    const res = JSON.parse(host.http(callId, JSON.stringify(payload)));
    if (res.error) throw new Error(`mcp ${name}: ${res.error}`);
    const sid = res.headers?.["mcp-session-id"];
    if (sid) sessionId = sid;
    if (!res.ok && res.status >= 400) throw new Error(`mcp ${name}: HTTP ${res.status}`);
    const ct = res.contentType ?? "";
    let message;
    if (ct.includes("text/event-stream")) {
      message = readSseResponse(res.body ?? "", id, name);
    } else {
      message = JSON.parse(res.body ?? "{}");
    }
    if (message.error) throw new Error(`mcp ${name}.${method}: ${message.error.message ?? "error"}`);
    return message.result;
  }

  /// 从**已读回**的 SSE 文本里找匹配 id 的响应（跳过通知）。
  /// 与 bun 版逐行等价，只是读流那半在 Rust 侧完成了（readMode=first-event）。
  function readSseResponse(body, id, server) {
    const normalized = body.replace(/\r\n/g, "\n");
    for (const chunk of normalized.split("\n\n")) {
      const data = chunk
        .split("\n")
        .filter((line) => line.startsWith("data:"))
        .map((line) => line.slice(5).trim())
        .join("\n");
      if (!data) continue;
      let msg;
      try {
        msg = JSON.parse(data);
      } catch {
        continue;
      }
      if (msg.id === id) return msg;
    }
    throw new Error(`mcp ${server}: stream ended without response (id ${id})`);
  }

  async function notify(method) {
    const { callId, payload } = post(JSON.stringify({ jsonrpc: "2.0", method }));
    payload.headers = {
      "content-type": "application/json",
      accept: "application/json, text/event-stream",
      ...(sessionId ? { "mcp-session-id": sessionId } : {}),
    };
    await requestApproval(callId, `mcp__${name}__*`, {});
    try {
      host.http(callId, JSON.stringify(payload));
    } catch {
      // 通知的应答常是 202 空体，失败无所谓（与 bun 版一致）
    }
  }

  return {
    name,
    async connect() {
      await rpc("initialize", {
        protocolVersion: MCP_PROTOCOL_VERSION,
        capabilities: {},
        clientInfo: { name: "pi-mobile-spike", version: "0.1.0" },
      });
      await notify("notifications/initialized");
    },
    async listTools() {
      const result = await rpc("tools/list", {});
      return (result?.tools ?? []).map((t) => ({
        name: t.name,
        description: t.description ?? "",
        inputSchema: t.inputSchema ?? { type: "object", properties: {} },
      }));
    },
    async callTool(toolName, args) {
      const result = await rpc("tools/call", { name: toolName, arguments: args ?? {} });
      const text = (result?.content ?? [])
        .filter((c) => c.type === "text")
        .map((c) => c.text)
        .join("\n");
      return { text, isError: Boolean(result?.isError) };
    },
  };
}

function jsonSchemaToObj(schema) {
  // MCP inputSchema 即 JSON Schema；属性描述透传给模型
  const props = schema?.properties ?? {};
  const required = schema?.required ?? Object.keys(props);
  const out = { type: "object", properties: {}, required, additionalProperties: false };
  for (const [key, val] of Object.entries(props)) {
    out.properties[key] = {
      type: val.type ?? "string",
      ...(val.description ? { description: val.description } : {}),
    };
  }
  return out;
}

function mcpTool(server, tool) {
  const fullName = `mcp__${server.name}__${tool.name}`;
  return {
    name: fullName,
    label: `${server.name}: ${tool.name}`,
    description: `${tool.description}\n(via MCP server "${server.name}")`,
    parameters: jsonSchemaToObj(tool.inputSchema),
    executionMode: "sequential",
    // 工具本身不再自己判审批：上面 rpc() 里已经按 mcp__ 档做过握手，
    // 而且出网那一步由宿主卡（host.http 要求同一 callId 已授权）。
    execute: async (_toolCallId, params) => {
      try {
        const r = await server.client.callTool(tool.name, params);
        return { content: [{ type: "text", text: r.text }], details: {} };
      } catch (error) {
        return { content: [{ type: "text", text: `Error: ${error?.message ?? error}` }], details: {} };
      }
    },
  };
}

/// boot 后异步连接所有已配置的 MCP 服务器并注册工具（幂等：先剔除旧 mcp__ 工具）。
async function connectMcpServers() {
  try {
    const cfg = JSON.parse(host.mcpConfig());
    const servers = cfg.servers ?? [];
    if (!servers.length) {
      outbox.push({ type: "mcp_tools_registered", count: 0 });
      return;
    }
    const mcpTools = [];
    for (const server of servers) {
      try {
        outbox.push({ type: "mcp_connecting", server: server.name });
        const client = mcpClient(server.name, server.url, server.headers, server.timeoutMs);
        await client.connect();
        const toolDefs = await client.listTools();
        for (const tool of toolDefs) mcpTools.push(mcpTool({ name: server.name, client }, tool));
        outbox.push({
          type: "mcp_ready",
          server: server.name,
          tools: toolDefs.map((t) => t.name),
        });
      } catch (error) {
        outbox.push({ type: "mcp_error", server: server.name, error: String(error?.message ?? error) });
      }
    }
    if (agent) {
      const existing = (agent.state.tools ?? []).filter((t) => !t.name.startsWith("mcp__"));
      agent.state.tools = [...existing, ...mcpTools];
    }
    outbox.push({ type: "mcp_tools_registered", count: mcpTools.length });
  } catch (error) {
    outbox.push({ type: "mcp_error", server: "(config)", error: String(error?.message ?? error) });
  }
}

// ── system prompt 组装：基础 + skills 位置 + AGENTS.md + 目标 + todo 引导 ──
//
// 顺序与 App 的 applySystemPrompt 对齐（todo → skills → AGENTS.md → goal）。
// skills 这一项在 spike 里留空：它的安装/校验在 App 侧由 Rust 的 skills.rs 管
// （git2 + zip + checksum），不属于本 spike 的范围 —— 见 README 的差距表。
let agentsMdCache = null;
let skillsCache = [];

function composeSystemPrompt(base) {
  let prompt = base.trim();
  prompt += `\n\n# Todo list\n\nManage a task list to track multi-step progress (the \`todo\` tool):\n${TODO_PROMPT_GUIDELINES.map((g) => `- ${g}`).join("\n")}`;
  // 段序与 App 的 applySystemPrompt 一致：todo → skills → AGENTS.md → goal
  if (skillsCache.length) {
    prompt += `\n\n# Skills\n\n${skillsCache
      .map((skill) => `## ${skill.name} — ${skill.description}\n\n${skill.content}`)
      .join("\n\n")}`;
  }
  if (agentsMdCache) prompt += `\n\n# Project instructions (AGENTS.md)\n\n${agentsMdCache}`;
  if (currentGoal) {
    prompt += `\n\n# Current goal\n\nWork persistently toward this objective across turns until the user clears it: ${currentGoal}`;
  }
  return prompt;
}

// ── pi-goal autoContinue（上游 Sisyphus 自动续跑，带上限防失控）────────
//
// 语义照搬 bun 版：goal 存续期间每回合结束自动续跑，直到模型逐字答复
// GOAL_COMPLETE、达到上限。**一处实现差异**：那边用 `setTimeout` 做「agent 还在
// processing」的退避重试，而 QuickJS 没有定时器 —— 改成由宿主 tick 驱动（每拍试一次），
// 与整条路线「循环归宿主」的形状一致。
const GOAL_AUTO_CAP = 10;
const GOAL_CONTINUE_PROMPT =
  "Continue working toward the current goal. If the goal is fully achieved, reply with exactly GOAL_COMPLETE and nothing else.";
let goalAutoCount = 0;
let goalAutoStopped = false;
let goalAutoEnabled = true;
let goalAutoPending = false;
let goalAutoTries = 0;

function lastAssistantText() {
  const messages = agent?.state.messages ?? [];
  const last = [...messages].reverse().find((m) => m.role === "assistant");
  return textOfContent(last?.content ?? []).trim();
}

/// 由 tick 调用：agent 空闲时提交续跑 prompt；还在忙就下一拍再试。
/// （bun 版这里是 setTimeout 退避 30×100ms；我们没有定时器。）
function pumpGoalAutoContinue() {
  if (!goalAutoPending) return;
  if (goalAutoTries >= 30) {
    goalAutoPending = false;
    outbox.push({ type: "goal_error", error: "agent stayed busy — auto-continue skipped" });
    outbox.push({ type: "agent_idle" });
    return;
  }
  goalAutoTries += 1;
  if (agent?.state.isStreaming || pendingModels.size > 0) return; // 下一拍再试
  goalAutoPending = false;
  agent.prompt(GOAL_CONTINUE_PROMPT).then(
    () => {},
    (error) => {
      const message = String(error?.message ?? error ?? "");
      if (message.includes("already processing")) {
        goalAutoPending = true; // 退避：交给下一拍
      } else {
        outbox.push({ type: "goal_error", error: message });
        outbox.push({ type: "agent_idle" });
      }
    },
  );
}

/// agent_end 时判定：继续跑还是收工。**终态必然发 agent_idle**，宿主靠它停循环。
function decideGoalContinue() {
  if (goalAutoStopped) {
    goalAutoStopped = false;
    outbox.push({ type: "agent_idle" });
    return;
  }
  const done = lastAssistantText() === "GOAL_COMPLETE";
  if (done) outbox.push({ type: "goal_auto_done" });
  if (!goalAutoEnabled || !currentGoal || done || goalAutoCount >= GOAL_AUTO_CAP) {
    if (currentGoal && goalAutoCount >= GOAL_AUTO_CAP) {
      outbox.push({ type: "goal_error", error: `auto-continue 达到上限 ${GOAL_AUTO_CAP}` });
    }
    outbox.push({ type: "agent_idle" });
    return;
  }
  goalAutoCount += 1;
  goalAutoTries = 0;
  goalAutoPending = true;
  outbox.push({ type: "goal_auto_continue", count: goalAutoCount, cap: GOAL_AUTO_CAP });
}

// ── /plan 与 /btw（pi-plan / pi-btw 的移动原生化，命令面由宿主驱动）────
// 两者都是「一次只读的嵌套 run」：不动主对话、不写文件。
async function draftPlan(objective) {
  outbox.push({ type: "plan_drafting", objective });
  try {
    const plan = await runNestedCollect(
      `Draft an implementation plan for this objective. Investigate the workspace with read-only tools first. Output numbered steps, each one line with the files involved. No code unless essential.\n\nObjective: ${objective}`,
      ["read", "ls", "grep"],
      "You are a planning subagent. Using read-only tools, investigate what is needed and draft a concise, actionable plan. No code unless essential.",
      "minimal",
    );
    outbox.push({ type: "plan_drafted", objective, content: plan || "(planning produced no output)" });
  } catch (error) {
    outbox.push({ type: "plan_error", objective, error: String(error?.message ?? error) });
  }
}

async function askByTheWay(question) {
  outbox.push({ type: "btw_thinking", question });
  try {
    const context = (agent?.state.messages ?? [])
      .slice(-12)
      .map((m) => `${m.role}: ${textOfContent(m.content).slice(0, 400)}`)
      .filter((line) => !line.endsWith(": "))
      .join("\n");
    const answer = await runNestedCollect(
      `Main conversation so far:\n${context || "(empty)"}\n\nQuestion: ${question}`,
      ["read", "ls", "grep"],
      "You are a side-conversation assistant. The user asks a quick question ('by the way') while the main task continues. Answer briefly using the main-conversation context above and read-only tools if needed. Do not continue the main task.",
      "minimal",
    );
    outbox.push({ type: "btw_answer", question, answer: answer || "(no answer)" });
  } catch (error) {
    outbox.push({ type: "btw_error", question, error: String(error?.message ?? error) });
  }
}

/// 会话列表（对齐 App 的 session_list）：按 modifiedAt 倒序。
///
/// ⚠️ **kick 模式**：`repo.list()` 是异步的，而宿主侧 `call::<String>` 不能把 Promise
/// 转成 String（rquickjs 会报 "Error converting from js 'promise' into type 'string'"）。
/// 所以这里立即返回 "started"，结果经 `session_list` 事件回合 —— 与 restore / plan
/// 同一形状，也是 App 在真机上被迫采用的那套（CONTRACTS 里记的 kick+轮询）。
function listSessions() {
  void repo
    .list()
    .then((metas) => {
      metas.sort((a, b) => b.modifiedAt - a.modifiedAt);
      outbox.push({
        type: "session_list",
        sessions: metas.map((m) => ({
          id: m.id,
          modifiedAt: m.modifiedAt,
          cwd: m.cwd,
          entries: m.entries ?? null,
        })),
      });
    })
    .catch((error) => outbox.push({ type: "session_error", error: `list: ${error?.message ?? error}` }));
  return "started";
}

/// 打开指定会话（对齐 App 的 session_open / __pi_open_session）。同样 kick 模式。
function openSession(id) {
  void (async () => {
    const metas = await repo.list();
    const meta = metas.find((m) => m.id === id);
    if (!meta) throw new Error(`no session with id ${id}`);
    session = await repo.open(meta);
    sessionId = meta.id;
    const entries = await session.findEntries();
    restoredMessages = entries
      .filter((e) => e.type === "message" && e.message)
      .sort((a, b) => (a.seq ?? 0) - (b.seq ?? 0))
      .map((e) => e.message);
    if (restoredMessages.length && agent) agent.state.messages = restoredMessages;
    for (const message of [...restoredMessages].reverse()) {
      if (message.role !== "assistant" || !message.usage) continue;
      lastContextTokens =
        message.usage.totalTokens ?? (message.usage.input ?? 0) + (message.usage.output ?? 0);
      break;
    }
    replayTodos(restoredMessages);
    outbox.push({ type: "session_opened", sessionId: meta.id, messages: restoredMessages.length });
  })().catch((error) =>
    outbox.push({ type: "session_error", error: `open: ${error?.message ?? error}` }),
  );
  return "started";
}

/// 新建空白会话（对齐 App 的 session_new）。
function newSession() {
  session = null;
  sessionPromise = null;
  sessionId = null;
  restoredMessages = [];
  lastContextTokens = 0;
  todoState = { tasks: [], nextId: todoState.nextId };
  if (agent) agent.state.messages = [];
  outbox.push({ type: "session_new" });
}

/// 启用中的技能 → systemPrompt（注入逻辑在 Rust：复用 App 的 skills::enabled_for_injection，
/// 所以「哪些技能被注入 / 截断预算」两边完全一致）。
async function refreshSkills() {
  try {
    skillsCache = JSON.parse(host.skillsConfig()).skills ?? [];
  } catch {
    skillsCache = [];
  }
  if (agent) agent.state.systemPrompt = composeSystemPrompt(baseSystemPrompt);
  outbox.push({ type: "skills_applied", count: skillsCache.length });
}

/// 启动后异步取 AGENTS.md 并重装 systemPrompt（与 App 的 refreshAgentsMd 同形）。
async function refreshAgentsMd() {
  try {
    const text = await internalRead("AGENTS.md");
    agentsMdCache = text.trim() ? text : null;
  } catch {
    agentsMdCache = null; // 没有 AGENTS.md 是常态
  }
  if (agent) agent.state.systemPrompt = composeSystemPrompt(baseSystemPrompt);
  outbox.push({ type: "agents_md_loaded", bytes: agentsMdCache?.length ?? 0 });
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
  // 每拍检查一次是否需要续跑（取代 bun 版的 setTimeout 退避）
  pumpGoalAutoContinue();
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
    } else if (event.type === "ask_user_decision") {
      const waiting = pendingAsks.get(event.id);
      if (!waiting) continue;
      pendingAsks.delete(event.id);
      waiting.resolve({ answer: event.answer, cancelled: Boolean(event.cancelled) });
    } else if (event.type === "ask_user") {
      // 交给宿主/UI 展示（App 里就是弹卡；CLI 由 Rust 在终端提问）
      outbox.push({ type: "ask_user", question: event.question, options: event.options });
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
  // Rust 提供的文件工具 + JS 侧的工具（fetch 走 Rust 的 http 通道；todo/subagent 纯 JS）
  const tools = [...toolsFor(config.tools || []), fetchTool, todoTool, subagentTool, askUserTool];
  baseSystemPrompt = config.systemPrompt || "You are a coding agent on a mobile device.";
  workspaceForSessions = config.workspace || "/workspace";
  currentGoal = config.goal || null;
  compactThreshold = config.compactAt || 0;
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
        // 上下文水位（自动压缩阈值用）：assistant 的 usage.totalTokens 反映本轮请求规模
        const usage = event.message?.usage;
        if (usage) lastContextTokens = usage.totalTokens ?? (usage.input ?? 0) + (usage.output ?? 0);
      }
    } else if (event.type === "turn_end") {
      for (const result of event.toolResults ?? []) persistMessage(result);
    } else if (event.type === "tool_execution_start" || event.type === "tool_execution_end") {
      compact.name = event.toolName;
      compact.isError = Boolean(event.isError);
    }
    outbox.push(compact);
    // 回合真正结束：判定是否往目标续跑（终态由它发 agent_idle）
    if (event.type === "agent_end") decideGoalContinue();
  });
  outbox.push({ type: "agent_ready" });
  // 启动期的两件异步事：读 AGENTS.md（重装 systemPrompt）+ 连 MCP（注册工具）。
  // **都完成**才发 context_ready —— 否则第一轮 prompt 可能少看到 MCP 工具。
  void Promise.all([
    refreshAgentsMd().catch(() => {}),
    refreshSkills().catch(() => {}),
    connectMcpServers().catch(() => {}),
  ]).finally(() => outbox.push({ type: "context_ready" }));
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
  // 水位超阈值先压缩再提交（与 App 同序：压缩 → prompt）。
  // 不 await：continuation 挂在微任务队列上，由宿主泵。
  void autoCompactIfNeeded()
    .catch((error) =>
      // 带 stack：JS 侧的 TypeError 只看 message 定位不到行号（第一次就是这个坑）
      outbox.push({
        type: "session_error",
        error: `compact: ${error?.message ?? error}\n${error?.stack ?? ""}`,
      }),
    )
    .finally(() => {
      agent.prompt(text).then(
        () => outbox.push({ type: "agent_end" }),
        (error) => outbox.push({ type: "agent_error", message: String(error) }),
      );
    });
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

// ── 宿主控制面（对齐 App 的 __pi_status / __pi_tool_names / __pi_history）──
function status() {
  return JSON.stringify({
    phase: agent?.state.isStreaming ? "thinking" : agent ? "ready" : "idle",
    busy: Boolean(agent?.state.isStreaming) || pendingModels.size > 0,
    pendingModels: pendingModels.size,
    pendingApprovals: pendingApprovals.size,
    messages: agent?.state.messages.length ?? 0,
    contextTokens: lastContextTokens,
    compactThreshold: compactThreshold || (agent?.state.model?.contextWindow ?? 0) * COMPACT_RATIO,
    errorMessage: agent?.state.errorMessage ?? null,
  });
}

function toolNames() {
  return JSON.stringify((agent?.state.tools ?? []).map((t) => t.name));
}

function history() {
  return JSON.stringify({ sessionId, messages: agent?.state.messages ?? [] });
}

globalThis.__spike = {
  boot,
  prompt,
  tick,
  drain,
  restore,
  sessionInfo,
  status,
  toolNames,
  history,
  // 命令面（对齐 App 的 pi_call_global：__pi_plan_start / __pi_btw_start / session_*）
  draftPlan,
  askByTheWay,
  listSessions,
  openSession,
  newSession,
  setAutoContinue: (enabled) => {
    goalAutoEnabled = Boolean(enabled);
    return "ok";
  },
};
