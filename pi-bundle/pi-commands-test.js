// Command plugins test (M4): /plan, /btw and goal persistence via the fake
// OpenAI-compatible LLM endpoint. Verifies the nested runs use the right
// system prompts and that goal state round-trips through the hostcall.
import { createServer } from "node:http";

const llmRequests = [];
let goalOnServer = null;

const srv = createServer((req, res) => {
	let body = "";
	req.on("data", (c) => (body += c));
	req.on("end", () => {
		if (req.url?.endsWith("/chat/completions")) {
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
			res.write(chunk({ role: "assistant", content: "1. step one\n2. step two" }));
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
			} else if (method === "goal_get") {
				res.end(JSON.stringify({ objective: goalOnServer }));
			} else if (method === "creds_get") {
				res.end('{"apiKey":"sk-test-fake"}');
			} else if (method === "tool") {
				res.end(JSON.stringify({ text: "(empty)" }));
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
await sleep(200);

// 1) /plan 后端：规划提示 + 只读工具
const plan = await globalThis.__pi_plan("add a search feature");
if (!plan.includes("step one")) {
	console.error("FAIL plan result:", plan.slice(0, 200));
	console.error("DEBUG llm requests:", JSON.stringify(llmRequests).slice(0, 900));
	process.exit(1);
}
const planReq = llmRequests.find((r) =>
	typeof r.messages?.[0]?.content === "string" && r.messages[0].content.includes("planning subagent"),
);
if (!planReq) {
	console.error("FAIL: plan run missing planning system prompt");
	process.exit(1);
}
const planTools = (planReq.tools ?? []).map((t) => t.function?.name ?? t.name);
if (planTools.includes("write") || planTools.includes("subagent")) {
	console.error("FAIL: planning run must be read-only:", planTools);
	process.exit(1);
}
console.log("OK __pi_plan: nested read-only run produced the plan");

// 2) /btw 后端：带主对话上下文的旁路问答
const btw = await globalThis.__pi_btw("what did we decide?");
if (!btw.includes("step one")) {
	console.error("FAIL btw result:", btw.slice(0, 200));
	process.exit(1);
}
const btwReq = llmRequests.find((r) =>
	typeof r.messages?.[0]?.content === "string" && r.messages[0].content.includes("side-conversation"),
);
if (!btwReq || !JSON.stringify(btwReq.messages).includes("Main conversation so far")) {
	console.error("FAIL: btw run missing side-conversation context");
	process.exit(1);
}
console.log("OK __pi_btw: side conversation carries main context");

// 3) goal_get hostcall：模拟 Rust 侧已存目标 → __pi_goal_apply 应用
goalOnServer = "ship the m4 milestone";
globalThis.__pi_goal_apply();
await sleep(300);
const prompt = globalThis.__pi_system_prompt();
if (!prompt.includes("Current goal") || !prompt.includes("ship the m4 milestone")) {
	console.error("FAIL: goal not applied to system prompt:", prompt.slice(0, 300));
	process.exit(1);
}
console.log("OK __pi_goal_apply: goal injected into system prompt");
srv.close();
process.exit(0);
