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
globalThis.__pi_tool_call = async (name, args) => {
	const tool = tools.find((t) => t.name === name);
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
	baseUrl: "https://api.deepseek.com",
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

const agent = new Agent({
	initialState: { model: DEFAULT_MODEL, thinkingLevel: "minimal", systemPrompt: "You are pi, a coding agent running on a mobile device. You have tools to access the user's workspace: ls (list files), read (read a file), write (write a file), edit (replace an exact text snippet in a file), grep (search files). write and edit require user approval. When the user asks you to do something with files, ALWAYS use the appropriate tool rather than saying you cannot. For example, to list files, call the ls tool with path '.'. To change a file, prefer edit with an exact oldText snippet; use write only to create files or rewrite them entirely. The workspace is a sandboxed directory on the device.", tools },
	streamFn: async (model, context, options) => {
		const fn = STREAM_SIMPLE[model.api];
		if (!fn) throw new Error(`unsupported api: ${model.api}`);
		const apiKey = await getApiKey(model.provider);
		hostcall("log", { msg: `streamFn: model=${model.id} api=${model.api} tools=${context.tools?.length ?? 0}` }).catch(() => {});
		return fn(model, context, { ...options, apiKey });
	},
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
	emit({ type: "session_restored", sessionId: latest.id, messages: restoredMessages.length });
}

const BASE_SYSTEM_PROMPT = () => agent.state.systemPrompt.split("\n\n# Project instructions")[0].trim();

// AGENTS.md 注入（pi 语义：workspace 规则进 system prompt）。boot/切会话时刷新。
async function refreshAgentsMd() {
	try {
		const r = await hostcall("tool", { name: "read", args: { path: "AGENTS.md" } });
		if (r.error || !r.text?.trim()) return;
		agent.state.systemPrompt = `${BASE_SYSTEM_PROMPT()}\n\n# Project instructions (AGENTS.md)\n\n${r.text}`;
		emit({ type: "agents_md_loaded", bytes: r.text.length });
	} catch {
		// 无 AGENTS.md —— 保持基线 prompt
	}
}

const sessionPersistError = (e) =>
	emit({ type: "session_error", error: String(e?.message ?? e) });
// agent_init 等 __pi_restored 再返回，保证 UI 的 agent_history 读到回放结果
restoreLatest()
	.catch(sessionPersistError)
	.finally(() => {
		globalThis.__pi_restored = true;
	});
refreshAgentsMd().catch(() => {});

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

// 会话切换（D7 会话列表）：open 指定会话并回放；new 清空指针，下一 prompt 落新 JSONL
globalThis.__pi_open_session = async (id) => {
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
		emit({ type: "session_restored", sessionId: meta.id, messages: restoredMessages.length });
		return "ok";
	} catch (e) {
		return JSON.stringify({ error: String(e?.message ?? e) });
	}
};

globalThis.__pi_new_session = () => {
	session = null;
	sessionId = null;
	restoredMessages = [];
	ensurePromise = null;
	agent.state.messages = [];
	emit({ type: "session_new" });
	return "ok";
};

globalThis.__pi_ready = true;
emit({ type: "agent_ready", tools: tools.map((t) => t.name) });

