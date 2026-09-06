// pi-goal autoContinue test (Sisyphus semantics with a runaway cap).
// Fake OpenAI-compatible LLM + mock loopback. Scenarios:
//   1. goal set -> first agent_end auto-continues (goal_auto_continue event,
//      second LLM request carries GOAL_CONTINUE_PROMPT)
//   2. model replies GOAL_COMPLETE verbatim -> goal_auto_done, no further runs
//   3. Stop mid-run -> suppression: the trailing agent_end does NOT auto-continue
import { createServer } from "node:http";

const llmRequests = [];
const events = [];
let goalOnServer = null;
let llmMode = "normal"; // "normal" | "complete" — controls the mock's reply

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
			const content = llmMode === "complete" ? "GOAL_COMPLETE" : "progress note";
			res.write(chunk({ role: "assistant", content }));
			res.write(chunk({}, "stop"));
			res.write("data: [DONE]\n\n");
			res.end();
			return;
		}
		if (req.url === "/hostcall") {
			const { method, payload } = JSON.parse(body || "{}");
			res.setHeader("content-type", "application/json");
			if (method === "agent_event") {
				events.push(payload);
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
await new Promise((r) => srv.listen(19994, "127.0.0.1", r));

globalThis.__PI_CONFIG = {
	port: 19994,
	dataDir: "/data",
	baseUrl: "http://127.0.0.1:19994/llm",
};
await import("./dist/agent.js");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
await sleep(200);
const waitFor = async (fn, ms = 15_000) => {
	const deadline = Date.now() + ms;
	while (Date.now() < deadline) {
		const v = fn();
		if (v) return v;
		await sleep(100);
	}
	return fn();
};
const assert = (cond, msg) => {
	if (!cond) {
		console.error(`ASSERT FAILED: ${msg}`);
		process.exit(1);
	}
	console.log(`OK ${msg}`);
};

// ── 场景 1：goal 存续 → 首个 agent_end 自动续跑 ──
goalOnServer = "finish the demo";
globalThis.__pi_goal_apply(); // boot 时 goal_get 为空，这里重新装载并等 goal_applied
assert(
	await waitFor(() => events.some((e) => e.type === "goal_applied" && e.objective === "finish the demo")),
	"setup: goal loaded into bundle",
);
globalThis.__pi_prompt("start working");
assert(
	await waitFor(() => events.some((e) => e.type === "goal_auto_continue" && e.count === 1)),
	"scenario 1: goal_auto_continue fired after first agent_end",
);
await waitFor(() => llmRequests.length >= 2);
const contReq = llmRequests[1];
const lastUser = [...(contReq.messages ?? [])].reverse().find((m) => m.role === "user");
assert(
	JSON.stringify(lastUser?.content ?? "").includes("Continue working toward the current goal"),
	"scenario 1: auto continuation prompt sent to LLM",
);

// ── 场景 2：模型逐字 GOAL_COMPLETE → goal_auto_done，不再续跑 ──
// 以 count===2 事件为切换点：该事件先于第三次 LLM 请求发出，
// 此处置 complete 保证第三回合答复逐字 GOAL_COMPLETE。
await waitFor(() => events.some((e) => e.type === "goal_auto_continue" && e.count === 2));
llmMode = "complete";
await waitFor(() => events.some((e) => e.type === "goal_auto_done"));
await sleep(400);
assert(llmRequests.length === 3, "scenario 2: GOAL_COMPLETE stops the loop (no further LLM calls)");

// ── 场景 3：用户 Stop → 抑制紧随的 agent_end，不再自动续跑 ──
llmMode = "normal";
const before = llmRequests.length;
globalThis.__pi_prompt("work again"); // 用户交互重置预算
await waitFor(() => llmRequests.length >= before + 2); // 首轮 + 第一次自动续跑
globalThis.__pi_stop(); // 第二轮进行中按 Stop
await sleep(800);
const autoEvents = events.filter((e) => e.type === "goal_auto_continue").length;
await sleep(600);
assert(
	llmRequests.length === before + 2 &&
		events.filter((e) => e.type === "goal_auto_continue").length === autoEvents,
	"scenario 3: Stop suppresses the trailing agent_end (no runaway)",
);

// ── 场景 4：永不完成 → 跑满上限（cap=10）后自动停 ──
llmMode = "normal";
const base = llmRequests.length;
globalThis.__pi_prompt("cap run"); // 重置预算
const capEvent = await waitFor(() =>
	events.find((e) => e.type === "goal_auto_continue" && e.count === 10 && e.cap === 10),
);
if (!capEvent) {
	console.error(
		"DEBUG scenario4 tail:",
		events.slice(-14).map((e) => e.type).join(","),
		"reqs:", llmRequests.length - base,
	);
}
assert(!!capEvent, "scenario 4: cap event (10/10) emitted");
await sleep(800);
assert(llmRequests.length === base + 11, "scenario 4: exactly cap continuations (1 user + 10 auto)");

srv.close();
process.exit(0);
