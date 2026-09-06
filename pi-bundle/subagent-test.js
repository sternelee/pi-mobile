// Subagent delegation test (M4): boots the bundle with a fake OpenAI-compatible
// LLM endpoint (__PI_CONFIG.baseUrl) and a mock host serving workspace agent
// definitions. Verifies the subagent tool end-to-end without a real API key:
//   subagent(reviewer, task) → loads workspace/agents/reviewer.md → nested
//   Agent runs with restricted tools + reviewer system prompt → final text
//   returned as the tool result.
import { createServer } from "node:http";

const llmRequests = [];
let chatCalls = 0;

const REVIEWER_MD = `---
name: reviewer
description: Review specialist for test
tools: read, ls, grep
thinking: minimal
systemPromptMode: replace
---

You are a review subagent. Inspect the workspace and report findings with evidence.`;

const srv = createServer((req, res) => {
	let body = "";
	req.on("data", (c) => (body += c));
	req.on("end", () => {
		if (req.url?.endsWith("/chat/completions")) {
			// 假 LLM：单段文本 + stop
			chatCalls++;
			try {
				llmRequests.push(JSON.parse(body || "{}"));
			} catch {}
			res.setHeader("content-type", "text/event-stream");
			const chunk = (delta, finish) =>
				`data: ${JSON.stringify({
					id: "chatcmpl-test",
					object: "chat.completion.chunk",
					choices: [{ index: 0, delta, finish_reason: finish ?? null }],
				})}\n\n`;
			res.write(chunk({ role: "assistant", content: "Subagent report: " }));
			res.write(chunk({ content: "workspace reviewed, no issues found." }));
			res.write(chunk({}, "stop"));
			res.write("data: [DONE]\n\n");
			res.end();
			return;
		}
		if (req.url === "/hostcall") {
			const { method, payload } = JSON.parse(body || "{}");
			res.setHeader("content-type", "application/json");
			if (method === "agent_event") {
				res.end('{"ok":true}');
			} else if (method === "tool") {
				if (payload.name === "ls" && payload.args?.path === "agents") {
					res.end(JSON.stringify({ text: "- reviewer.md" }));
				} else if (payload.name === "read" && payload.args?.path === "agents/reviewer.md") {
					res.end(JSON.stringify({ text: REVIEWER_MD }));
				} else {
					res.end(JSON.stringify({ text: "(empty)" }));
				}
			} else if (method === "creds_get") {
				res.end('{"apiKey":"sk-test-fake"}');
			} else {
				res.end('{"ok":true}');
			}
			return;
		}
		res.statusCode = 404;
		res.end("{}");
	});
});
await new Promise((r) => srv.listen(19999, "127.0.0.1", r));

globalThis.__PI_CONFIG = {
	port: 19999,
	dataDir: "/data",
	baseUrl: "http://127.0.0.1:19999/llm",
};
await import("./dist/agent.js");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
await sleep(300); // 等 MCP/connect 等异步 boot 落定

// 1) 未知 agent 名 → 错误文案列出可用 agent
const unknown = await globalThis.__pi_tool_call("subagent", { agent: "nope", task: "x" });
if (!/Available:/.test(unknown.content?.[0]?.text ?? "")) {
	console.error("FAIL unknown agent:", (unknown.content?.[0]?.text ?? "").slice(0, 200));
	process.exit(1);
}
console.log("OK subagent: unknown agent lists available agents");

// 2) 委托 reviewer（workspace/agents/reviewer.md 定义）→ 假 LLM 生成最终回复
const result = await globalThis.__pi_tool_call("subagent", {
	agent: "reviewer",
	task: "review the workspace",
});
const text = result.content?.[0]?.text ?? "";
if (!text.includes("Subagent report") || !text.includes("no issues found")) {
	console.error("FAIL subagent result:", text.slice(0, 300));
	process.exit(1);
}
console.log("OK subagent: delegation returned the sub-agent final response");

// 3) 子代理确实以 reviewer 系统提示 + 受限工具集请求了 LLM
const sub = llmRequests.find(
	(r) => typeof r.messages?.[0]?.content === "string" && r.messages[0].content.includes("review subagent"),
);
if (!sub) {
	console.error("FAIL: no LLM request with reviewer system prompt");
	process.exit(1);
}
const toolNames = (sub.tools ?? []).map((t) => t.function?.name ?? t.name);
if (toolNames.includes("subagent") || toolNames.includes("write")) {
	console.error("FAIL: sub-agent tool set not restricted:", toolNames);
	process.exit(1);
}
if (!toolNames.includes("read") || !toolNames.includes("grep")) {
	console.error("FAIL: sub-agent missing read tools:", toolNames);
	process.exit(1);
}
console.log("OK subagent: nested run used reviewer prompt + restricted tool set");
srv.close();
process.exit(0);
