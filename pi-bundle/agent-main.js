// pi-mobile agent bundle entry — runs INSIDE embedded bun (libskal).
// Agent loop from pi-agent-core; tools bridged to Rust host over loopback HTTP.
// IMPORTANT (deadlock lesson from smoke2): the evaluated script must return
// synchronously — no top-level await, no returned Promise. All async work is
// kicked inside an IIFE; the host polls via __pi_status() / agent events.
// Kick+poll contract:
//   globalThis.__pi_prompt(text) -> "started"
//   globalThis.__pi_status()     -> JSON string
const LOOPBACK = `http://127.0.0.1:${globalThis.__PI_CONFIG?.port ?? 19999}`;

async function hostcall(method, payload) {
	const res = await fetch(LOOPBACK + "/hostcall", {
		method: "POST",
		headers: { "content-type": "application/json" },
		body: JSON.stringify({ method, payload }),
		signal: AbortSignal.timeout(30_000),
	});
	if (!res.ok) throw new Error(`hostcall ${method}: HTTP ${res.status}`);
	return res.json();
}

function emit(event) {
	// fire-and-forget; failures must never break the agent loop
	hostcall("agent_event", event).catch(() => {});
}

(async () => {
	// ---- tools: workspace FS via Rust host (D2.1/D6: no exec, host owns trust) ----
	function hostTool(name, label, description, parameters) {
		return {
			name,
			label,
			description,
			parameters,
			async execute(args) {
				try {
					const r = await hostcall("tool", { name, args });
					if (r.error) throw new Error(r.error);
					return { content: [{ type: "text", text: r.text ?? "" }], details: {} };
				} catch (e) {
					return { content: [{ type: "text", text: `Error: ${e?.message ?? e}` }], details: {} };
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

	const tools = [
		hostTool("read", "Read", "Read a text file from the workspace. Args: {path}", obj({ path: { type: "string" } })),
		hostTool("write", "Write", "Write text to a file in the workspace (creates or overwrites). Args: {path, content}", obj({ path: { type: "string" }, content: { type: "string" } })),
		hostTool("ls", "List", "List directory entries in the workspace. Args: {path?} (default '.')", obj({ path: { type: "string" } }, [])),
		hostTool("grep", "Grep", "Regex search across workspace text files. Args: {pattern, path?}", obj({ pattern: { type: "string" }, path: { type: "string" } }, ["pattern"])),
	];

	// ---- streamFn: dispatch on model.api via per-api simple stream functions ----
	let apiKeyCache;
	async function getApiKey(provider) {
		if (!apiKeyCache) {
			const r = await hostcall("creds_get", { provider });
			apiKeyCache = r.apiKey;
		}
		return apiKeyCache;
	}

	const { Agent } = await import("@earendil-works/pi-agent-core");
	const [anthropic, openaiCompletions, openaiResponses, google] = await Promise.all([
		import("@earendil-works/pi-ai/api/anthropic-messages"),
		import("@earendil-works/pi-ai/api/openai-completions"),
		import("@earendil-works/pi-ai/api/openai-responses"),
		import("@earendil-works/pi-ai/api/google-generative-ai"),
	]);

	const STREAM_SIMPLE = {
		"anthropic-messages": anthropic.streamSimple,
		"openai-completions": openaiCompletions.streamSimple,
		"openai-responses": openaiResponses.streamSimple,
		"google-generative-ai": google.streamSimple,
	};

	const agent = new Agent({
		streamFn: async (model, context, options) => {
			const fn = STREAM_SIMPLE[model.api];
			if (!fn) throw new Error(`unsupported api: ${model.api}`);
			const apiKey = await getApiKey(model.provider);
			return fn(model, context, { ...options, apiKey });
		},
		getApiKey: (provider) => getApiKey(provider),
		tools,
	});

	agent.subscribe((event) => {
		emit(event);
	});

	// ---- host-facing controls (kick+poll contract) ----
	let lastError = null;
	let busy = false;

	globalThis.__pi_prompt = (text) => {
		try {
			const t = typeof text === "string" && text.trim().startsWith("{") ? JSON.parse(text) : String(text);
			busy = true;
			lastError = null;
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

	globalThis.__pi_ready = true;
	emit({ type: "agent_ready", tools: tools.map((t) => t.name) });
})().catch((e) => {
	globalThis.__pi_boot_error = String(e?.stack ?? e);
	emit({ type: "boot_error", error: globalThis.__pi_boot_error });
});
"agent-main kicked";
