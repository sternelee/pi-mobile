// pi-mobile agent bundle entry — runs INSIDE embedded bun (libskal).
// Agent loop from pi-agent-core; tools bridged to Rust host over loopback HTTP.
// CRITICAL (device-verified lessons):
//  1. NEVER return a Promise from the evaluated script (waitForPromise deadlocks).
//  2. NO top-level await / dynamic imports: bundled dynamic imports become pure
//     microtasks, which skal only pumps during promise-awaiting evals — a sync
//     poll loop would starve them forever. Static imports execute synchronously.
// Async work is kicked inside an IIFE whose continuations hang off bun's I/O
// machinery (proven to tick between evals by smoke2). Host polls __pi_status.
// Node builtin polyfills for CJS interop (`__require("fs")` etc.) under the
// embedded runtime — see pi-bundle/build.sh for the __require patch.
// MUST be imported FIRST: pi-agent-core's module body calls __require during
// its own evaluation, before any later import would have run.
import * as __piNodeStdlib from "node-stdlib-browser";
globalThis.__PI_NODE_STDLIB = __piNodeStdlib;
import { Agent, JsonlSessionRepo, FileError } from "@earendil-works/pi-agent-core";
import * as anthropic from "@earendil-works/pi-ai/api/anthropic-messages";
import * as openaiCompletions from "@earendil-works/pi-ai/api/openai-completions";
import * as openaiResponses from "@earendil-works/pi-ai/api/openai-responses";
// bisect: google-generative-ai temporarily disabled (brings @google/genai node-builtin
// imports that crash skal JSC on device). re-enable once gemini path is fixed.
// import * as google from "@earendil-works/pi-ai/api/google-generative-ai";

const LOOPBACK = `http://127.0.0.1:${globalThis.__PI_CONFIG?.port ?? 19999}`;

async function hostcall(method, payload, opts = {}) {
	const res = await fetch(LOOPBACK + "/hostcall", {
		method: "POST",
		headers: { "content-type": "application/json" },
		body: JSON.stringify({ method, payload }),
		signal: opts.noTimeout ? undefined : AbortSignal.timeout(30_000),
	});
	if (!res.ok) throw new Error(`hostcall ${method}: HTTP ${res.status}`);
	return res.json();
}

function emit(event) {
	// fire-and-forget; failures must never break the agent loop
	hostcall("agent_event", event).catch(() => {});
}

// ---- core tools: workspace FS via Rust host (D2.1/D6: no exec, host owns trust) ----
// Mutating tools ask the host for approval before executing (PLAN D2 policy-hook):
// Rust policy gates ask/auto, the UI gets the diff, denial returns as tool error.
const errContent = (text) => ({ content: [{ type: "text", text }], details: {} });

function hostTool(name, label, description, parameters, opts = {}) {
	return {
		name,
		label,
		description,
		parameters,
		// pi AgentTool.execute 签名： (toolCallId, params, signal, onUpdate)
		// —— 第一参数是调用 ID，参数在第二位（真机实测踩坑：ls 不依赖参数
		// 掩盖了错位，write 报 path? 才暴露）。
		async execute(toolCallId, params) {
			try {
				if (opts.mutating) {
					const apr = await hostcall("approval_request", { tool: name, args: params });
					if (apr.error) return errContent(`approval failed: ${apr.error}`);
					if (apr.decision !== "allow")
						return errContent(
							`User did not approve the ${name} of "${params?.path}" (${apr.reason ?? apr.decision}). Nothing was written — choose another approach or ask the user.`,
						);
				}
				const r = await hostcall("tool", { name, args: params });
				if (r.error) throw new Error(r.error);
				return { content: [{ type: "text", text: r.text ?? "" }], details: {} };
			} catch (e) {
				return errContent(`Error: ${e?.message ?? e}`);
			}
		},
	};
}

const obj = (props, required) => ({
	type: "object",
	properties: props,
	required: required ?? Object.keys(props),
	additionalProperties: false,
});

const coreTools = [
	hostTool("read", "Read", "Read a text file from the workspace. Args: {path}", obj({ path: { type: "string" } })),
	hostTool("write", "Write", "Write text to a file in the workspace (creates or overwrites). Requires user approval — if the user denies, do not retry the same write. Args: {path, content}", obj({ path: { type: "string" }, content: { type: "string" } }), { mutating: true }),
	hostTool("edit", "Edit", "Replace an exact text snippet inside a workspace file. oldText must match the file content exactly (including whitespace) and be unique unless replaceAll=true. Requires user approval. Args: {path, oldText, newText, replaceAll?}", obj({ path: { type: "string" }, oldText: { type: "string" }, newText: { type: "string" }, replaceAll: { type: "boolean" } }, ["path", "oldText", "newText"]), { mutating: true }),
	hostTool("ls", "List", "List directory entries in the workspace. Args: {path?} (default '.')", obj({ path: { type: "string" } }, [])),
	hostTool("grep", "Grep", "Regex search across workspace text files. Args: {pattern, path?}", obj({ pattern: { type: "string" }, path: { type: "string" } }, ["pattern"])),
];

// ---- extension capability layer（npm:pi-* 插件的移动原生化）----
// 原 npm 插件是 pi-coding-agent 扩展，交互层绑死 pi-tui 终端 UI，无法在嵌入
// 式 WebView 环境运行。这里保持工具名与 schema 对齐上游（模型视角一致），
// 交互层由宿主 UI（WebView 组件）承担。每个扩展 = 一组 AgentTool。
// 已覆盖：pi-ask-user（ask_user）。路线图：pi-mcp-adapter（HTTP transport）、
// pi-subagents（委托）、@devkade/pi-plan / pi-goal / pi-btw（命令类）。

const askUserTool = {
	name: "ask_user",
	label: "Ask User",
	description:
		"Ask the user a question with optional multiple-choice answers. Use when the user's intent is ambiguous, when a decision requires explicit input, or when multiple valid options exist. Ask exactly ONE focused question per call; before calling, gather context with tools and pass a short summary via context. The user must answer before the run continues.",
	parameters: {
		type: "object",
		properties: {
			question: { type: "string", description: "The question to ask the user" },
			context: { type: "string", description: "Relevant context to show before the question (summary of findings)" },
			options: {
				type: "array",
				description: "Options for the user to choose from",
				items: {
					type: "object",
					properties: {
						title: { type: "string", description: "Short title for this option" },
						description: { type: "string", description: "Longer description explaining this option" },
					},
					required: ["title"],
					additionalProperties: false,
				},
			},
			allowMultiple: { type: "boolean", description: "Allow selecting multiple options. Default: false" },
			allowFreeform: { type: "boolean", description: "Offer a freeform text answer. Default: true" },
			allowComment: { type: "boolean", description: "Allow an optional extra comment. Default: false" },
		},
		required: ["question"],
		additionalProperties: false,
	},
	// 与上游一致：ask_user 未决时阻塞同回合其他工具，防止用户未看到提问
	// 就先执行有副作用的操作
	executionMode: "sequential",
	async execute(toolCallId, params) {
		try {
			// kick+事件注入模式（禁用长挂起 fetch）：真机实测长 pending fetch +
			// AbortSignal 定时器会触发嵌入 bun 的 HeapHelper 线程 SIGSEGV。
			// 注册即返回 → Rust 侧等用户作答 → 经 __pi_ask_resolve 反向注入。
			const reg = await hostcall("ask_user_register", {
				question: params.question,
				context: params.context,
				options: params.options ?? [],
				allowMultiple: params.allowMultiple ?? false,
				allowFreeform: params.allowFreeform ?? true,
				allowComment: params.allowComment ?? false,
			});
			const reply = await new Promise((resolve) => {
				pendingAsks.set(reg.id, resolve);
				// 看门狗：UI 崩溃/重启导致 resolve 永不注入时，防 agent 永久挂死
				// （kick 模式约束：续体不得只挂在 JS 定时器上——此处 setTimeout 仅作
				// 兜底超时，正常路径由 Rust 注入；超时后清 pendingAsks 防 stale resolve）。
				// unref：兜底定时器不挂住事件循环（否则本地测试进程要等满 12 分钟才退）。
				const watchdog = setTimeout(() => {
					if (pendingAsks.delete(reg.id)) resolve(null);
				}, 12 * 60 * 1000);
				watchdog?.unref?.();
			});
			const resp = reply?.response ?? null;
			if (!resp) {
				return errContent(
					"The user dismissed the question. Continue with your best judgment and clearly state the assumption you are making.",
				);
			}
			let text =
				resp.kind === "freeform"
					? `(wrote) ${resp.text ?? ""}`
					: `✓ ${(resp.selections ?? []).join(", ")}`;
			if (resp.comment) text += `\nComment: ${resp.comment}`;
			return { content: [{ type: "text", text: `User answered: ${text}` }], details: {} };
		} catch (e) {
			return errContent(`Error: ${e?.message ?? e}`);
		}
	},
};

const extensionTools = [askUserTool];

const tools = [...coreTools, ...extensionTools];

// ---- subagents（pi-subagents 移动原生化：agent 委托）----
// 上游的 fleet/workflow/mission 机制基于 pi-server 运行时，移动端取其核心
// 能力：subagent 工具把任务委托给具名子代理（独立上下文 + 受限工具集），
// 跑完把最终回复作为工具结果返回。agent 定义与上游同格式（markdown +
// frontmatter）：内置 delegate/researcher/reviewer，外加 workspace/agents/*.md。
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
		tools: (meta.tools ?? "").split(",").map((s) => s.trim()).filter(Boolean),
		thinking: meta.thinking ?? "minimal",
		body: m[2].trim(),
	};
}

async function loadAgentDefs() {
	const defs = {};
	for (const [name, def] of Object.entries(BUILTIN_AGENTS)) defs[name] = { ...def, name };
	// workspace/agents/*.md 自定义定义（与上游 pi-subagents 格式兼容）
	try {
		const ls = await hostcall("tool", { name: "ls", args: { path: "agents" } });
		if (!ls.error && ls.text && ls.text !== "(empty)") {
			for (const line of ls.text.split("\n")) {
				const file = line.replace(/^-\s*/, "").trim();
				if (!file.endsWith(".md")) continue;
				const src = await hostcall("tool", { name: "read", args: { path: `agents/${file}` } });
				if (src.error) continue;
				const def = parseAgentDef(src.text);
				if (def?.name) defs[def.name] = def;
			}
		}
	} catch {}
	return defs;
}

function resolveSubTools(def) {
	const available = agent.state.tools ?? tools;
	const out = [];
	for (const name of def.tools) {
		if (name === "subagent") continue; // 递归防护：子代理不得再委托
		const t = available.find((x) => x.name === name);
		if (t) out.push(t);
	}
	return out;
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
	async execute(toolCallId, params) {
		try {
			const defs = await loadAgentDefs();
			const def = defs[params.agent];
			if (!def) {
				return errContent(
					`Unknown subagent "${params.agent}". Available: ${Object.keys(defs).join(", ")}`,
				);
			}
			emit({ type: "subagent_start", name: def.name, task: params.task });
			const sub = new Agent({
				initialState: {
					model: DEFAULT_MODEL,
					thinkingLevel: def.thinking ?? "minimal",
					systemPrompt:
						def.systemPromptMode === "append"
							? `${BASE_SYSTEM_PROMPT()}\n\n${def.body}`
							: def.body,
					tools: resolveSubTools(def),
				},
				streamFn: sharedStreamFn,
				getApiKey: (provider) => getApiKey(provider),
			});
			// 主 agent 停止时级联中止子代理（listener 第二参数 = 当前运行 signal）
			let cascade = undefined;
			const unsubCascade = agent.subscribe((_ev, signal) => {
				cascade = signal;
			});
			// 子代理事件不上屏（保持主对话可读），仅记日志
			sub.subscribe((ev) => {
				if (ev.type === "agent_error") {
					hostcall("log", { msg: `subagent ${def.name} error: ${ev.error ?? ""}` }).catch(() => {});
				}
			});
			if (cascade?.aborted) sub.abort();
			const onCascadeAbort = () => sub.abort();
			cascade?.addEventListener("abort", onCascadeAbort, { once: true });
			try {
				await sub.prompt(params.task);
			} finally {
				unsubCascade();
				cascade?.removeEventListener("abort", onCascadeAbort);
			}
			const msgs = sub.state.messages ?? [];
			const lastAssistant = [...msgs].reverse().find((m) => m.role === "assistant");
			const text = (lastAssistant?.content ?? [])
				.filter((c) => c.type === "text")
				.map((c) => c.text)
				.join("\n")
				.trim();
			emit({ type: "subagent_end", name: def.name });
			return { content: [{ type: "text", text: text || "(subagent returned no text)" }], details: {} };
		} catch (e) {
			emit({ type: "subagent_end", name: params.agent });
			return errContent(`Error: ${e?.message ?? e}`);
		}
	},
};

extensionTools.push(subagentTool);
tools.push(subagentTool); // agent 在下方构造，确保初始工具集包含 subagent

// ---- todo（@juicesharp/rpiv-todo 移动原生化：Claude-Code 对齐任务清单）----
// 语义逐条对齐上游 tool-schema.md：6 动作（create/update/list/get/delete/clear）、
// 4 态状态机（deleted 为墓碑）、blockedBy 依赖图校验（未知/墓碑/自阻塞/环）。
// 持久化走上游同款哲学：每个 toolResult 的 details 携带全量快照，状态从会话
// 消息回放重建（restoreLatest / 切会话 / new），不写磁盘。纯 JS 工具，零 hostcall。
// 交互层：todo_updated 事件 → WebView 常驻面板（上游 TUI overlay 的移动形态）。

const TODO_TRANSITIONS = {
	pending: ["in_progress", "completed", "deleted"],
	in_progress: ["pending", "completed", "deleted"],
	completed: ["deleted"],
	deleted: [],
};

let todoState = { tasks: [], nextId: 1 };
const todoTask = (id) => todoState.tasks.find((t) => t.id === id);

// blockedBy 校验（先校验后变更，拒绝时状态不动）：依赖须存在且非墓碑、
// 不得自阻塞、新增边不得成环（从新依赖沿 blockedBy DFS，回到自身即环——
// 已有图无环是不变量，故只需检查新边）。
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
		if (cur.id === id) return "addBlockedBy would create a cycle in the blockedBy graph";
		seen.add(cur.id);
		for (const b of cur.blockedBy ?? []) stack.push(b);
	}
	return null;
}

const todoRow = (t) =>
	`[${t.status}] #${t.id} ${t.subject}` +
	(t.activeForm ? ` (${t.activeForm})` : "") +
	(t.blockedBy?.length ? ` ⛓ ${t.blockedBy.map((b) => `#${b}`).join(",")}` : "");

function todoApply(action, p) {
	const snapshot = () => JSON.parse(JSON.stringify(todoState));
	const envelope = (text, error) => ({
		text,
		details: { action, params: p, tasks: snapshot().tasks, nextId: todoState.nextId, ...(error ? { error } : {}) },
	});
	// 拒绝：content 带 "Error: …"，details.error 带裸消息，状态不动
	const fail = (msg) => ({ ...envelope(`Error: ${msg}`, msg), error: msg });

	switch (action) {
		case "create": {
			const subject = (p.subject ?? "").trim();
			if (!subject) return fail("subject required for create");
			if (p.blockedBy?.length) {
				const err = todoDepError(todoState.nextId, p.blockedBy);
				if (err) return fail(err);
			}
			const task = { id: todoState.nextId++, subject, status: "pending" };
			if (p.description != null) task.description = p.description;
			if (p.activeForm != null) task.activeForm = p.activeForm;
			if (p.owner != null) task.owner = p.owner;
			if (p.metadata != null) task.metadata = p.metadata;
			if (p.blockedBy?.length) task.blockedBy = [...p.blockedBy];
			todoState.tasks.push(task);
			return envelope(`Created #${task.id}: ${subject} (pending)`);
		}
		case "update": {
			if (p.id == null) return fail("id required for update");
			const task = todoTask(p.id);
			if (!task) return fail(`#${p.id} not found`);
			const mutable = ["subject", "description", "activeForm", "status", "owner", "metadata", "addBlockedBy", "removeBlockedBy"];
			if (!mutable.some((k) => p[k] !== undefined))
				return fail(
					"update requires at least one mutable field: subject, description, activeForm, status, owner, metadata, addBlockedBy, or removeBlockedBy",
				);
			// 状态机：同状态 = no-op；非法迁移拒绝（先校验后变更）
			if (p.status != null && p.status !== task.status && !TODO_TRANSITIONS[task.status].includes(p.status))
				return fail(`illegal transition ${task.status} → ${p.status}`);
			if (p.addBlockedBy?.length || p.removeBlockedBy?.length) {
				const add = p.addBlockedBy ?? [];
				if (add.length) {
					const err = todoDepError(task.id, add);
					if (err) return fail(err);
				}
				const cur = new Set(task.blockedBy ?? []);
				for (const d of add) cur.add(d);
				for (const d of p.removeBlockedBy ?? []) cur.delete(d);
				if (cur.size) task.blockedBy = [...cur].sort((a, b) => a - b);
				else delete task.blockedBy;
			}
			let changed = false;
			for (const k of ["subject", "description", "activeForm", "owner"]) {
				if (p[k] !== undefined && p[k] !== task[k]) {
					if (p[k] === "") delete task[k];
					else task[k] = p[k];
					changed = true;
				}
			}
			if (p.metadata != null) {
				task.metadata ??= {};
				for (const [k, v] of Object.entries(p.metadata)) {
					if (v === null) delete task.metadata[k];
					else task.metadata[k] = v;
				}
				if (!Object.keys(task.metadata).length) delete task.metadata;
				changed = true;
			}
			const prevStatus = task.status;
			if (p.status != null && p.status !== prevStatus) {
				task.status = p.status;
				changed = true;
			}
			if (p.addBlockedBy?.length || p.removeBlockedBy?.length) changed = true;
			if (!changed)
				return envelope(`No change: #${task.id} already matches the requested values (status: ${task.status})`);
			return envelope(
				p.status != null && p.status !== prevStatus
					? `Updated #${task.id} (${prevStatus} → ${p.status})`
					: `Updated #${task.id}`,
			);
		}
		case "list": {
			let tasks = todoState.tasks.filter((t) => t.status !== "deleted" || p.includeDeleted);
			if (p.status) tasks = tasks.filter((t) => t.status === p.status);
			return envelope(tasks.length ? tasks.map(todoRow).join("\n") : "No tasks");
		}
		case "get": {
			if (p.id == null) return fail("id required for get");
			const task = todoTask(p.id);
			if (!task) return fail(`#${p.id} not found`);
			const lines = [todoRow(task)];
			if (task.description) lines.push(task.description);
			if (task.blockedBy?.length) lines.push(`blockedBy: ${task.blockedBy.map((b) => `#${b}`).join(",")}`);
			const blocks = todoState.tasks.filter((t) => (t.blockedBy ?? []).includes(task.id) && t.status !== "deleted");
			if (blocks.length) lines.push(`blocks: ${blocks.map((b) => `#${b.id}`).join(",")}`);
			return envelope(lines.join("\n"));
		}
		case "delete": {
			if (p.id == null) return fail("id required for delete");
			const task = todoTask(p.id);
			if (!task) return fail(`#${p.id} not found`);
			if (task.status === "deleted") return fail(`#${p.id} is already deleted`);
			task.status = "deleted";
			return envelope(`Deleted #${task.id}: ${task.subject}`);
		}
		case "clear": {
			const n = todoState.tasks.length;
			todoState = { tasks: [], nextId: 1 };
			return envelope(`Cleared ${n} tasks`);
		}
		default:
			return fail(`unknown action: ${action}`);
	}
}

// 会话回放：取最后一个携带 details.tasks 的 todo toolResult（上游 replayFromBranch
// 同语义——全量快照在 details 里，walk 分支取最后一份）
function replayTodos(messages) {
	let found = null;
	for (const m of messages) {
		if (m?.role === "toolResult" && m.toolName === "todo" && Array.isArray(m.details?.tasks)) found = m.details;
	}
	todoState = found
		? { tasks: found.tasks, nextId: found.nextId ?? 1 }
		: { tasks: [], nextId: 1 };
	emit({ type: "todo_updated", tasks: todoState.tasks, nextId: todoState.nextId });
}

// prompt 引导（上游 DEFAULT_PROMPT_SNIPPET / DEFAULT_PROMPT_GUIDELINES 原文）
const TODO_PROMPT_GUIDELINES = [
	"Use `todo` for complex work with 3+ steps, when the user gives you a list of tasks, or immediately after receiving new instructions to capture requirements. Skip it for single trivial tasks and purely conversational requests.",
	"When starting a task from the todo list, mark it in_progress BEFORE beginning work. Mark it completed IMMEDIATELY when done — never batch completions. Exactly one task in_progress at a time.",
	"Never mark a task completed if tests are failing, the implementation is partial, or you hit unresolved errors — keep it in_progress and create a new task for the blocker instead.",
	"Task status is a 4-state machine: pending → in_progress → completed, plus deleted as a tombstone. Pass activeForm (present-continuous label, e.g. 'researching existing tool') when marking in_progress.",
	'To change a task\'s status, call update with the task id and the target status, e.g. {"action":"update","id":3,"status":"completed"} or {"action":"update","id":3,"status":"in_progress","activeForm":"writing tests"}. status is the field that changes the task; an update without a mutable field (status or another) is rejected.',
	"Use blockedBy to express dependencies (A is blocked by B). On create, pass blockedBy as the initial set. On update, use addBlockedBy / removeBlockedBy (additive merge — do not resend the full array). Cycles are rejected.",
	"list hides tombstoned (deleted) tasks by default; pass includeDeleted:true to see them. Pass status to filter by a single status.",
	"Subject must be short and imperative (e.g. 'Research existing tool'); description is for long-form detail. activeForm is a present-continuous label shown while in_progress.",
];

const todoTool = {
	name: "todo",
	label: "Todo",
	description:
		"Manage a task list for tracking multi-step progress. Actions: create (new task), update (change status/fields/dependencies), list (all tasks, optionally filtered by status), get (single task details), delete (tombstone), clear (reset all). Status: pending → in_progress → completed, plus deleted tombstone. Use this to plan and track multi-step work like research, design, and implementation.",
	parameters: {
		type: "object",
		properties: {
			action: { type: "string", enum: ["create", "update", "list", "get", "delete", "clear"], description: "The operation to perform" },
			subject: { type: "string", description: "(create/update) short imperative task title" },
			description: { type: "string", description: "(create/update) long-form detail" },
			activeForm: { type: "string", description: "(create/update) present-continuous label shown while in_progress, e.g. 'writing tests'" },
			owner: { type: "string", description: "(create/update) agent/owner assigned to this task" },
			metadata: { type: "object", description: "(create/update) arbitrary key-value; on update, null deletes a key" },
			blockedBy: { type: "array", items: { type: "integer" }, description: "(create) ids this task waits on" },
			addBlockedBy: { type: "array", items: { type: "integer" }, description: "(update) additive merge into blockedBy" },
			removeBlockedBy: { type: "array", items: { type: "integer" }, description: "(update) additive removal from blockedBy" },
			id: { type: "integer", description: "(update/get/delete) task id" },
			status: { type: "string", enum: ["pending", "in_progress", "completed", "deleted"], description: "(update) target status; (list) filter" },
			includeDeleted: { type: "boolean", description: "(list) include tombstoned tasks. Default: false" },
		},
		required: ["action"],
		additionalProperties: false,
	},
	async execute(toolCallId, params) {
		const r = todoApply(params?.action ?? "", params ?? {});
		if (!r.error) {
			emit({ type: "todo_updated", tasks: todoState.tasks, nextId: todoState.nextId });
		}
		return { content: [{ type: "text", text: r.text }], details: r.details };
	},
};

extensionTools.push(todoTool);
tools.push(todoTool);

// ---- MCP adapter（pi-mcp-adapter 移动原生化，仅 streamable-http）----
// 手写最小 MCP 客户端：JSON-RPC over POST，响应兼容 application/json 与
// text/event-stream。不用 @modelcontextprotocol SDK——其 node 内建依赖与
// 原生模块在嵌入 JSC 不可用（@google/genai SIGSEGV 前科）。stdio 不支持。
// 工具命名 mcp__<server>__<tool>（D11），执行前走 ask 审批。

const MCP_PROTOCOL_VERSION = "2025-06-18";

function mcpClient(name, url, headers, timeoutMs) {
	let nextId = 1;
	let sessionId = null;
	const timeout = timeoutMs > 0 ? timeoutMs : 30_000;

	async function rpc(method, params) {
		const id = nextId++;
		const res = await fetch(url, {
			method: "POST",
			headers: {
				"content-type": "application/json",
				accept: "application/json, text/event-stream",
				...(sessionId ? { "mcp-session-id": sessionId } : {}),
				...(headers ?? {}),
			},
			body: JSON.stringify({ jsonrpc: "2.0", id, method, params: params ?? {} }),
			signal: AbortSignal.timeout(timeout),
		});
		const sid = res.headers.get("mcp-session-id");
		if (sid) sessionId = sid;
		if (!res.ok) throw new Error(`mcp ${name}: HTTP ${res.status}`);
		const ct = res.headers.get("content-type") ?? "";
		let message;
		if (ct.includes("text/event-stream") && res.body) {
			message = await readSseResponse(res, id);
		} else {
			message = await res.json();
		}
		if (message.error) throw new Error(`mcp ${name}.${method}: ${message.error.message ?? "error"}`);
		return message.result;
	}

	// SSE 流里找匹配 id 的 JSON-RPC 响应（跳过通知），找到即断开。
	// 注意：SSE 规范用 \r\n 换行（真机 deepwiki 实测），切分前统一归一化。
	async function readSseResponse(res, id) {
		const reader = res.body.getReader();
		const decoder = new TextDecoder();
		let buf = "";
		const takeEvent = () => {
			const idx = buf.indexOf("\n\n");
			if (idx === -1) return null;
			const chunk = buf.slice(0, idx);
			buf = buf.slice(idx + 2);
			const data = chunk
				.split("\n")
				.filter((l) => l.startsWith("data:"))
				.map((l) => l.slice(5).trim())
				.join("\n");
			return data || null;
		};
		try {
			for (;;) {
				const { done, value } = await reader.read();
				if (value) buf += decoder.decode(value, { stream: true });
				buf = buf.replace(/\r\n/g, "\n");
				for (;;) {
					const data = takeEvent();
					if (data === null) break;
					const msg = JSON.parse(data);
					if (msg.id === id) return msg;
				}
				if (done) break;
			}
			// 流结束但缓冲里还有残缺事件：尽力解析最后一段
			if (buf.includes("data:")) {
				const msg = JSON.parse(buf.split("\n").filter((l) => l.startsWith("data:")).map((l) => l.slice(5).trim()).join(""));
				if (msg.id === id) return msg;
			}
		} finally {
			reader.releaseLock?.();
			try {
				res.body.cancel();
			} catch {}
		}
		throw new Error(`mcp ${name}: stream ended without response`);
	}

	return {
		name,
		async connect() {
			await rpc("initialize", {
				protocolVersion: MCP_PROTOCOL_VERSION,
				capabilities: {},
				clientInfo: { name: "pi-mobile", version: "0.1.0" },
			});
			// initialized 通知（无 id）：服务器可能回 202 空体，忽略解析失败
			await fetch(url, {
				method: "POST",
				headers: {
					"content-type": "application/json",
					accept: "application/json, text/event-stream",
					...(sessionId ? { "mcp-session-id": sessionId } : {}),
					...(headers ?? {}),
				},
				body: JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }),
			}).catch(() => {});
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
		label: `${server}: ${tool.name}`,
		description: `${tool.description}\n(via MCP server "${server}")`,
		parameters: jsonSchemaToObj(tool.inputSchema),
		// D11：MCP 工具默认全部 ask 审批
		async execute(toolCallId, params) {
			try {
				const apr = await hostcall("approval_request", { tool: fullName, args: params });
				if (apr.error) return errContent(`approval failed: ${apr.error}`);
				if (apr.decision !== "allow")
					return errContent(`User did not approve the ${fullName} call. Nothing was executed.`);
				const r = await server.client.callTool(tool.name, params);
				return { content: [{ type: "text", text: r.text }], details: {} };
			} catch (e) {
				return errContent(`Error: ${e?.message ?? e}`);
			}
		},
	};
}

// boot 后异步连接所有已配置的 MCP 服务器并注册工具（挂在真实网络 I/O 上）。
// 幂等：重连时先剔除旧 mcp__ 工具再并入新的。
async function connectMcpServers() {
	try {
		const cfg = await hostcall("mcp_config", {}, { noTimeout: true });
		const servers = cfg.servers ?? [];
		const mcpTools = [];
		for (const s of servers) {
			try {
				const client = mcpClient(s.name, s.url, s.headers, s.timeoutMs);
				emit({ type: "mcp_connecting", server: s.name });
				await client.connect();
				const toolDefs = await client.listTools();
				for (const t of toolDefs) {
					const wrapped = mcpTool({ name: s.name, client }, t);
					mcpTools.push(wrapped);
				}
				emit({ type: "mcp_ready", server: s.name, tools: toolDefs.map((t) => t.name) });
			} catch (e) {
				emit({ type: "mcp_error", server: s.name, error: String(e?.message ?? e) });
			}
		}
		const existing = (agent.state.tools ?? []).filter((t) => !t.name.startsWith("mcp__"));
		agent.state.tools = [...existing, ...mcpTools];
		emit({ type: "mcp_tools_registered", count: mcpTools.length });
	} catch (e) {
		emit({ type: "mcp_error", server: "(config)", error: String(e?.message ?? e) });
	}
}
connectMcpServers().catch(() => {});

// 抽屉"Reconnect"按钮：改完服务器配置后热重连，无需重启 App
globalThis.__pi_mcp_reconnect = () => {
	connectMcpServers().catch(() => {});
	return "started";
};

// ask_user 的 kick+事件注入：Rust 在用户作答后经 skal_evaluate 调
// __pi_ask_resolve(id, answerJson) 反向解析 pending promise。返回两拍后才
// settle 的 promise，让 waitForPromise 把工具 continuation 泵完。
const pendingAsks = new Map();
globalThis.__pi_ask_resolve = (id, answerJson) => {
	const resolve = pendingAsks.get(id);
	if (!resolve) return "no such ask";
	pendingAsks.delete(id);
	let parsed;
	try {
		parsed = JSON.parse(answerJson);
	} catch {
		parsed = { response: null, reason: "bad answer" };
	}
	resolve(parsed);
	return new Promise((done) => {
		queueMicrotask(() => queueMicrotask(done));
	});
};

// 诊断/测试缝：直接执行一个工具（与 agent 循环同一 execute 路径，含审批）。
// 优先查 agent.state.tools（含运行期注册的 MCP 工具），回退静态列表。
globalThis.__pi_tool_call = async (name, args) => {
	const tool =
		(agent.state.tools ?? []).find((t) => t.name === name) ??
		tools.find((t) => t.name === name);
	if (!tool) return { error: `unknown tool: ${name}` };
	return tool.execute("test-call-id", args ?? {});
};

// 中止当前运行（UI 停止按钮）：AbortController 语义，agent_end(aborted) 收尾
globalThis.__pi_stop = () => {
	try {
		agent.abort();
		return "ok";
	} catch (e) {
		return `error: ${e?.message ?? e}`;
	}
};

// ---- streamFn: dispatch on model.api via per-api simple stream functions ----
let apiKeyCache;
async function getApiKey(provider) {
	if (!apiKeyCache) {
		const r = await hostcall("creds_get", { provider });
		apiKeyCache = r.apiKey;
	}
	return apiKeyCache;
}

// Default model for M2 device verification: DeepSeek V4 Flash (OpenAI-completions
// compatible, reasoning). Catalog entry from @earendil-works/pi-ai providers.
const DEFAULT_MODEL = {
	id: "deepseek-v4-flash",
	name: "DeepSeek V4 Flash",
	api: "openai-completions",
	provider: "deepseek",
	// baseUrl 可被 __PI_CONFIG.baseUrl 覆盖（本地测试用假 LLM 端点）
	baseUrl: globalThis.__PI_CONFIG?.baseUrl ?? "https://api.deepseek.com",
	reasoning: true,
	input: ["text"],
	cost: { input: 0.14, output: 0.28, cacheRead: 0.0028, cacheWrite: 0 },
	contextWindow: 1000000,
	maxTokens: 384000,
	compat: {
		supportsStore: false,
		supportsDeveloperRole: false,
		maxTokensField: "max_tokens",
		requiresReasoningContentOnAssistantMessages: true,
		thinkingFormat: "deepseek",
	},
	thinkingLevelMap: { minimal: null, low: "low", medium: null, high: "high", max: "max" },
};

const STREAM_SIMPLE = {
	"anthropic-messages": anthropic.streamSimple,
	"openai-completions": openaiCompletions.streamSimple,
	"openai-responses": openaiResponses.streamSimple,
	// "google-generative-ai": google.streamSimple,
};

// streamFn 主/子 agent 共用（按 model.api 分发 + 凭证注入 + 诊断日志）
async function sharedStreamFn(model, context, options) {
	const fn = STREAM_SIMPLE[model.api];
	if (!fn) throw new Error(`unsupported api: ${model.api}`);
	const apiKey = await getApiKey(model.provider);
	hostcall("log", { msg: `streamFn: model=${model.id} api=${model.api} tools=${context.tools?.length ?? 0}` }).catch(() => {});
	return fn(model, context, { ...options, apiKey });
}

const agent = new Agent({
	initialState: { model: DEFAULT_MODEL, thinkingLevel: "minimal", systemPrompt: "You are pi, a coding agent running on a mobile device. You have tools to access the user's workspace: ls (list files), read (read a file), write (write a file), edit (replace an exact text snippet in a file), grep (search files). write and edit require user approval. When the user asks you to do something with files, ALWAYS use the appropriate tool rather than saying you cannot. For example, to list files, call the ls tool with path '.'. To change a file, prefer edit with an exact oldText snippet; use write only to create files or rewrite them entirely. The workspace is a sandboxed directory on the device.", tools },
	streamFn: sharedStreamFn,
	getApiKey: (provider) => getApiKey(provider),
});

agent.subscribe((event) => {
	emit(event);
	// D3 落盘：assistant 消息在 message_end、tool 结果在 turn_end 追加；
	// agent_end 带全量消息，不重复落。toolResult 不走 message_end 以免双写。
	if (event.type === "message_end" && event.message?.role === "assistant") {
		persistMessage(event.message);
	}
	if (event.type === "turn_end") {
		for (const tr of event.toolResults ?? []) persistMessage(tr);
	}
});

// ---- session persistence: pi-native JSONL (D3) over host-backed fs ----
// JsonlSessionRepo + Session are pi's own classes (format-compatible with
// desktop sessions). Real disk I/O stays in Rust: `hostFs` implements the
// FileSystem capability over the `fs` hostcall (jailed to {dataDir}/sessions).
// Device constraint: `require` is unavailable in the eval context, so real
// node:fs is unreachable from JS — the hostcall boundary is mandatory anyway.
const SESSIONS_ROOT = "/pi-sessions"; // must match loopback.rs SESSIONS_VIRTUAL_ROOT
const WORKSPACE = `${globalThis.__PI_CONFIG?.dataDir ?? "/data"}/workspace`;

const fsOk = (value) => ({ ok: true, value });
const fsFail = (code, message, path) => ({
	ok: false,
	error: new FileError(code, message, path),
});

async function fsCall(op, args, path) {
	const r = await hostcall("fs", { op, ...args });
	if (r.ok) return r.value;
	throw Object.assign(new Error(r.error?.message ?? "fs error"), { code: r.error?.code ?? "unknown" });
}

const needRel = (p) => {
	const rel = stripRoot(p);
	if (rel === null) throw Object.assign(new Error(`path outside sessions namespace: ${p}`), { code: "invalid" });
	return rel;
};

const stripRoot = (p) =>
	p === SESSIONS_ROOT ? "" : p.startsWith(`${SESSIONS_ROOT}/`) ? p.slice(SESSIONS_ROOT.length + 1) : null;

const toFsResult = async (path, fn) => {
	try {
		return fsOk(await fn());
	} catch (e) {
		return fsFail(e?.code ?? "unknown", e?.message ?? String(e), path);
	}
};

const hostFs = {
	async absolutePath(path) {
		return fsOk(path.startsWith("/") ? path : `${SESSIONS_ROOT}/${path}`);
	},
	async joinPath(parts) {
		// repo 传 ["/", root, dir, file] 之类的混合段：逐段去斜杠再折叠，
		// 避免 "/pi-sessions" 前面叠出 "//"、"///" 让 stripRoot 失配。
		const joined = parts
			.map((s) => String(s ?? "").replace(/^\/+|\/+$/g, ""))
			.filter((s) => s !== "" && s !== ".")
			.join("/");
		return fsOk(`/${joined}`);
	},
	readTextFile(path) {
		return toFsResult(path, () => fsCall("readTextFile", { path: needRel(path) }, path));
	},
	readTextLines(path, options) {
		return toFsResult(path, () =>
			fsCall("readTextLines", { path: needRel(path), maxLines: options?.maxLines }, path),
		);
	},
	writeFile(path, content) {
		if (typeof content !== "string") return Promise.resolve(fsFail("not_supported", "binary write unsupported over hostcall", path));
		return toFsResult(path, () => fsCall("writeFile", { path: needRel(path), content }, path));
	},
	appendFile(path, content) {
		if (typeof content !== "string") return Promise.resolve(fsFail("not_supported", "binary write unsupported over hostcall", path));
		return toFsResult(path, () => fsCall("appendFile", { path: needRel(path), content }, path));
	},
	renameFile(sourcePath, destinationPath) {
		return toFsResult(sourcePath, () =>
			fsCall("renameFile", { path: needRel(sourcePath), to: needRel(destinationPath) }, sourcePath),
		);
	},
	fileInfo(path) {
		return toFsResult(path, () => fsCall("fileInfo", { path: needRel(path) }, path));
	},
	listDir(path) {
		return toFsResult(path, () => fsCall("listDir", { path: needRel(path) }, path));
	},
	exists(path) {
		return toFsResult(path, () => fsCall("exists", { path: needRel(path) }, path));
	},
	createDir(path, options) {
		return toFsResult(path, () =>
			fsCall("createDir", { path: needRel(path), recursive: options?.recursive ?? false }, path),
		);
	},
	remove(path, options) {
		return toFsResult(path, () =>
			fsCall("remove", { path: needRel(path), recursive: options?.recursive ?? false }, path),
		);
	},
};

const repo = new JsonlSessionRepo({ fs: hostFs, sessionsRoot: SESSIONS_ROOT });
let session = null;
let sessionId = null;
let restoredMessages = [];

function persistMessage(message) {
	if (!session || !message) return;
	// agent 消息带显式 undefined 属性（如 toolResult 的 usage/addedToolNames），
	// pi 的 assertJsonSerializable 直接拒绝（真机实测 "Durable payload contains
	// undefined"）—— JSON 一轮净化：undefined 属性被丢弃，其余保真。
	const clean = JSON.parse(JSON.stringify(message));
	session.appendMessage(clean).catch((e) =>
		emit({ type: "session_error", error: String(e?.message ?? e) }),
	);
}

let ensurePromise = null;
function ensureSession() {
	// single-flight：并发首调（prompt 与 persist_direct 同帧）只建一个会话
	if (session) return Promise.resolve(session);
	if (!ensurePromise) {
		ensurePromise = repo
			.create({ cwd: WORKSPACE })
			.then(async (created) => {
				session = created;
				const meta = await created.getMetadata();
				sessionId = meta.id;
				emit({ type: "session_created", sessionId: meta.id });
				return created;
			})
			.catch((e) => {
				ensurePromise = null; // 失败可重试
				throw e;
			});
	}
	return ensurePromise;
}

async function restoreLatest() {
	const metas = await repo.list();
	if (!metas.length) return;
	metas.sort((a, b) => b.modifiedAt - a.modifiedAt);
	const latest = metas[0];
	const opened = await repo.open(latest);
	session = opened;
	sessionId = latest.id;
	const entries = await opened.findEntries();
	// findEntries 新序列在前（真机实测），回放按 seq 升序
	restoredMessages = entries
		.filter((e) => e.type === "message" && e.message)
		.sort((a, b) => (a.seq ?? 0) - (b.seq ?? 0))
		.map((e) => e.message);
	if (restoredMessages.length) {
		agent.state.messages = restoredMessages;
	}
	replayTodos(restoredMessages); // todo 状态随会话回放重建（上游 replayFromBranch 语义）
	emit({ type: "session_restored", sessionId: latest.id, messages: restoredMessages.length });
}

const BASE_SYSTEM_PROMPT = () =>
	agent.state.systemPrompt
		.split("\n\n# Project instructions")[0]
		.split("\n\n# Current goal")[0]
		.split("\n\n# Todo list")[0]
		.split("\n\n# Skills")[0]
		.trim();

// AGENTS.md + 持久目标（pi-goal 移动原生化）+ Skills（D12）统一组装 systemPrompt
let agentsMdCache = null;
let currentGoal = null;
let skillsCache = [];

async function applySystemPrompt() {
	let prompt = BASE_SYSTEM_PROMPT();
	prompt += `\n\n# Todo list\n\nManage a task list to track multi-step progress (the \`todo\` tool):\n${TODO_PROMPT_GUIDELINES.map((g) => `- ${g}`).join("\n")}`;
	if (skillsCache.length)
		prompt += `\n\n# Skills\n\n${skillsCache
			.map((s) => `## ${s.name} — ${s.description}\n\n${s.content}`)
			.join("\n\n")}`;
	if (agentsMdCache) prompt += `\n\n# Project instructions (AGENTS.md)\n\n${agentsMdCache}`;
	if (currentGoal)
		prompt += `\n\n# Current goal\n\nWork persistently toward this objective across turns until the user clears it: ${currentGoal}`;
	agent.state.systemPrompt = prompt;
}

async function refreshAgentsMd() {
	try {
		const r = await hostcall("tool", { name: "read", args: { path: "AGENTS.md" } });
		agentsMdCache = r.error || !r.text?.trim() ? null : r.text;
	} catch {
		agentsMdCache = null;
	}
	await applySystemPrompt();
	emit({ type: "agents_md_loaded", bytes: agentsMdCache?.length ?? 0 });
}

async function refreshGoal() {
	try {
		const r = await hostcall("goal_get", {}, { noTimeout: true });
		currentGoal = r.objective ?? null;
	} catch {
		currentGoal = null;
	}
	await applySystemPrompt();
	emit({ type: "goal_applied", objective: currentGoal });
}
globalThis.__pi_goal_apply = () => {
	refreshGoal().catch(() => {});
	return "started";
};

// Skills（D12）：启用中的技能包注入（宿主 skills.rs 安装/启停，此处只消费）。
// kick 模式：__pi_skills_apply 立即返回，skills_applied 事件携带注入数量。
async function refreshSkills() {
	try {
		const r = await hostcall("skills_config", {}, { noTimeout: true });
		skillsCache = r.skills ?? [];
	} catch {
		skillsCache = [];
	}
	await applySystemPrompt();
	emit({ type: "skills_applied", count: skillsCache.length });
}
globalThis.__pi_skills_apply = () => {
	refreshSkills().catch(() => {});
	return "started";
};

const sessionPersistError = (e) =>
	emit({ type: "session_error", error: String(e?.message ?? e) });
// agent_init 等 __pi_restored 再返回，保证 UI 的 agent_history 读到回放结果
restoreLatest()
	.catch(sessionPersistError)
	.finally(() => {
		globalThis.__pi_restored = true;
	});
refreshAgentsMd().catch(() => {});
refreshGoal().catch(() => {});
refreshSkills().catch(() => {});

// ── 命令类插件后端：/plan（只读规划）与 /btw（旁路问答）──
// kick+事件回投模式：立即返回，嵌套 Agent 与主流式并行跑（共享 bun 事件循环，
// 各自的 fetch 在 eval 间隙泵动），完成后 emit plan_drafted / btw_answer 事件。
// 不占用 runtime 锁 —— 旁问期间 Stop 等操作保持可用。
async function runNestedCollect(prompt, tools, systemPrompt, thinking) {
	const sub = new Agent({
		initialState: {
			model: DEFAULT_MODEL,
			thinkingLevel: thinking ?? "minimal",
			systemPrompt,
			tools: resolveSubTools({ tools }),
		},
		streamFn: sharedStreamFn,
		getApiKey: (provider) => getApiKey(provider),
	});
	await sub.prompt(prompt);
	const msgs = sub.state.messages ?? [];
	const last = [...msgs].reverse().find((m) => m.role === "assistant");
	return (last?.content ?? [])
		.filter((c) => c.type === "text")
		.map((c) => c.text)
		.join("\n")
		.trim();
}

globalThis.__pi_plan_start = (objective) => {
	emit({ type: "plan_drafting", objective });
	(async () => {
		try {
			const plan = await runNestedCollect(
				`Draft an implementation plan for this objective. Investigate the workspace with read-only tools first. Output numbered steps, each one line with the files involved. No code unless essential.\n\nObjective: ${objective}`,
				["read", "ls", "grep"],
				"You are a planning subagent. Using read-only tools, investigate what is needed and draft a concise, actionable plan. No code unless essential.",
				"minimal",
			);
			emit({ type: "plan_drafted", objective, content: plan || "(planning produced no output)" });
		} catch (e) {
			emit({ type: "plan_error", objective, error: String(e?.message ?? e) });
		}
	})();
	return "started";
};

globalThis.__pi_btw_start = (question) => {
	emit({ type: "btw_thinking", question });
	(async () => {
		try {
			const ctx = (agent.state.messages ?? [])
				.slice(-12)
				.map((m) => {
					const t = (m.content ?? [])
						.filter((c) => c.type === "text")
						.map((c) => c.text)
						.join(" ");
					return `${m.role}: ${t.slice(0, 400)}`;
				})
				.filter((l) => !l.endsWith(": "))
				.join("\n");
			const answer = await runNestedCollect(
				`Main conversation so far:\n${ctx || "(empty)"}\n\nQuestion: ${question}`,
				["read", "ls", "grep"],
				"You are a side-conversation assistant. The user asks a quick question ('by the way') while the main task continues. Answer briefly using the main-conversation context above and read-only tools if needed. Do not continue the main task.",
				"minimal",
			);
			emit({ type: "btw_answer", question, answer: answer || "(no answer)" });
		} catch (e) {
			emit({ type: "btw_error", question, error: String(e?.message ?? e) });
		}
	})();
	return "started";
};

// ---- host-facing controls (kick+poll contract) ----
let lastError = null;
let busy = false;

globalThis.__pi_prompt = (text) => {
	try {
		const t = typeof text === "string" && text.trim().startsWith("{") ? JSON.parse(text) : String(text);
		busy = true;
		lastError = null;
		// 用户消息先落会话（ensureSession 异步建会话；存储内部队列保证顺序）
		const userMsg =
			typeof t === "string" ? { role: "user", content: t, timestamp: Date.now() } : t;
		ensureSession()
			.then((s) => s.appendMessage(userMsg))
			.catch(sessionPersistError);
		agent
			.prompt(t)
			.catch((e) => {
				lastError = String(e?.message ?? e);
				emit({ type: "agent_error", error: lastError });
			})
			.finally(() => {
				busy = false;
			});
		return "started";
	} catch (e) {
		return `error: ${e?.message ?? e}`;
	}
};

globalThis.__pi_status = () =>
	JSON.stringify({ busy, lastError, queued: agent.hasQueuedMessages() });

// 诊断：当前 agent 全量工具名（含运行期注册的 MCP 工具）
globalThis.__pi_tool_names = () => (agent.state.tools ?? []).map((t) => t.name);
globalThis.__pi_system_prompt = () => agent.state.systemPrompt;

// todo 诊断/测试缝：状态快照 + 合成消息回放（同步、无 I/O——eval 安全）
globalThis.__pi_todo_state = () => JSON.stringify(todoState);
globalThis.__pi_todo_replay = (messagesJson) => {
	replayTodos(JSON.parse(messagesJson));
	return "ok";
};

// 重启恢复给 UI 的历史（boot 时从最新会话回放；同步求值用内存副本）
globalThis.__pi_history = () =>
	JSON.stringify({ sessionId, messages: restoredMessages });

// 诊断/测试缝：直接把一条消息写入当前会话（走 persistMessage 同一净化路径）
globalThis.__pi_persist_direct = (message) => {
	ensureSession()
		.then(() => persistMessage(message))
		.catch(sessionPersistError);
	return "started";
};

// 会话切换（D7 会话列表）：open 指定会话并回放；new 清空指针，下一 prompt 落新 JSONL。
// open 必须 kick+轮询（与 __pi_persist_direct / __pi_goal_apply 同款）：skal_evaluate
// 对求值结果为 Promise 时走 waitForPromise——阻塞 VM 线程，而 repo.list/open/findEntries
// 的 fs hostcall（fetch → loopback）恰需该线程 tick 才能完成 → 整个桥死锁
// （smoke2/ask_user 同族教训：eval 不得返回挂 I/O 的 Promise）。因此同步返回
// "started"，结果 JSON 落 __pi_session_open_result 供 Rust session_open 轮询。
globalThis.__pi_session_open_result = null;
const doOpenSession = async (id) => {
	try {
		const metas = await repo.list();
		const meta = metas.find((m) => m.id === id);
		if (!meta) return JSON.stringify({ error: `no such session: ${id}` });
		const opened = await repo.open(meta);
		session = opened;
		sessionId = meta.id;
		ensurePromise = null;
		const entries = await opened.findEntries();
		restoredMessages = entries
			.filter((e) => e.type === "message" && e.message)
			.sort((a, b) => (a.seq ?? 0) - (b.seq ?? 0))
			.map((e) => e.message);
		if (restoredMessages.length) agent.state.messages = restoredMessages;
		replayTodos(restoredMessages); // 切会话：todo 状态随目标会话重建
		emit({ type: "session_restored", sessionId: meta.id, messages: restoredMessages.length });
		return "ok";
	} catch (e) {
		return JSON.stringify({ error: String(e?.message ?? e) });
	}
};
globalThis.__pi_open_session = (id) => {
	globalThis.__pi_session_open_result = null;
	doOpenSession(id)
		.then((r) => {
			globalThis.__pi_session_open_result = r;
		})
		.catch((e) => {
			globalThis.__pi_session_open_result = JSON.stringify({ error: String(e?.message ?? e) });
		});
	return "started";
};

globalThis.__pi_new_session = () => {
	session = null;
	sessionId = null;
	restoredMessages = [];
	ensurePromise = null;
	agent.state.messages = [];
	todoState = { tasks: [], nextId: 1 }; // 新会话 = 空任务槽（上游按 sessionId 分槽）
	emit({ type: "todo_updated", tasks: [], nextId: 1 });
	emit({ type: "session_new" });
	return "ok";
};

globalThis.__pi_ready = true;
emit({ type: "agent_ready", tools: tools.map((t) => t.name) });

