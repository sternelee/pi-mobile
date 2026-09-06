// todo extension test (@juicesharp/rpiv-todo mobile-native port).
// Boots the bundle against a mock loopback (todo tool itself makes no
// hostcalls — only agent_event emissions), then drives the `todo` tool via
// the __pi_tool_call seam and asserts upstream tool-schema.md parity:
// content strings, status machine, blockedBy graph validation, tombstones,
// snapshot-in-details envelope, session replay, prompt guidance.
//
// 时序注意：boot 的异步收尾（restoreLatest / refreshAgentsMd / refreshGoal）
// 与纯 JS 的工具调用（无 I/O，微任务即可跑完）竞争；事件 emit 是
// fire-and-forget。断言前必须 sleep 让 I/O 泵动、收尾前 flush。
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

const PHASE = process.env.PHASE;

if (!PHASE) {
	// ---- driver ----
	const root = mkdtempSync(path.join(tmpdir(), "pi-todo-test-"));
	mkdirSync(path.join(root, "sessions"), { recursive: true });
	const b = spawnSync(process.execPath, [import.meta.path], {
		env: { ...process.env, PHASE: "RUN", SESSIONS_DIR: path.join(root, "sessions") },
		encoding: "utf8",
	});
	if (b.status !== 0) {
		console.error(`TODO TEST FAILED\n${b.stdout}\n${b.stderr}`);
		process.exit(1);
	}
	console.log(b.stdout.trim().split("\n").filter((l) => l.startsWith("OK")).join("\n"));
	console.log("OK todo extension: all upstream-parity assertions passed");
	process.exit(0);
}

// ---- phase runner ----
const { createServer } = await import("node:http");
const events = [];
const srv = createServer((req, res) => {
	let body = "";
	req.on("data", (c) => (body += c));
	req.on("end", () => {
		const { method, payload } = JSON.parse(body || "{}");
		res.setHeader("content-type", "application/json");
		if (method === "agent_event") {
			events.push(payload);
			res.end('{"ok":true}');
		} else {
			// fs / creds_get / goal_get / mcp_config / tool：boot 只需"安静"的应答
			res.end('{"ok":true}');
		}
	});
});
await new Promise((r) => srv.listen(19998, "127.0.0.1", r));

globalThis.__PI_CONFIG = { port: 19998, dataDir: "/data" };
await import("./dist/agent.js");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const bootDeadline = Date.now() + 10_000;
while (!globalThis.__pi_ready && Date.now() < bootDeadline) await sleep(100);
if (!globalThis.__pi_ready) {
	console.error("FAIL: bundle never became ready");
	process.exit(1);
}
// 等 boot 异步收尾（applySystemPrompt 的 todo 节依赖 refreshAgentsMd/refreshGoal 跑完）
await sleep(500);

const todo = async (params) => globalThis.__pi_tool_call("todo", params);
const state = () => JSON.parse(globalThis.__pi_todo_state());
let failed = 0;
const expect = (cond, label) => {
	if (!cond) {
		failed++;
		console.error(`FAIL: ${label}`);
	}
};

// 1. create
let r = await todo({ action: "create", subject: "Research existing tool" });
expect(r.content[0].text === "Created #1: Research existing tool (pending)", `create content: ${r.content[0].text}`);
expect(r.details.tasks.length === 1 && r.details.nextId === 2, "create snapshot");
expect(r.details.tasks[0].status === "pending", "create starts pending");

// 2. update + activeForm（保持 #2 in_progress 供后续 list/get 断言）
await todo({ action: "create", subject: "Write the parser" });
r = await todo({ action: "update", id: 2, status: "in_progress", activeForm: "writing the parser" });
expect(r.content[0].text === "Updated #2 (pending → in_progress)", `update content: ${r.content[0].text}`);
expect(r.details.tasks.find((t) => t.id === 2).activeForm === "writing the parser", "activeForm stored");

// 3. list 行格式：状态 glyph + activeForm
r = await todo({ action: "list" });
expect(r.content[0].text.includes("[in_progress] #2 Write the parser (writing the parser)"), `list in_progress row: ${r.content[0].text}`);

// 4. no-op update
r = await todo({ action: "update", id: 2, status: "in_progress" });
expect(
	r.content[0].text === "No change: #2 already matches the requested values (status: in_progress)",
	`no-change content: ${r.content[0].text}`,
);

// 5. blockedBy 校验：未知依赖 / 自阻塞 / 环（拒绝时状态不动）
r = await todo({ action: "create", subject: "X", blockedBy: [99] });
expect(r.content[0].text === "Error: blockedBy: #99 not found", `unknown dep: ${r.content[0].text}`);
r = await todo({ action: "update", id: 1, addBlockedBy: [1] });
expect(r.content[0].text === "Error: cannot block #1 on itself", `self block: ${r.content[0].text}`);
r = await todo({ action: "update", id: 1, addBlockedBy: [2] });
expect(r.content[0].text.startsWith("Updated"), `valid dep ok: ${r.content[0].text}`);
r = await todo({ action: "update", id: 2, addBlockedBy: [1] });
expect(
	r.content[0].text === "Error: addBlockedBy would create a cycle in the blockedBy graph",
	`cycle: ${r.content[0].text}`,
);
expect(!state().tasks.find((t) => t.id === 2).blockedBy?.includes(1), "cycle rejected, state untouched");

// 6. create 携带初始依赖 + list 显示 ⛓ 边
r = await todo({ action: "create", subject: "Ship it", blockedBy: [1, 2] });
expect(r.content[0].text === "Created #3: Ship it (pending)", `create with deps: ${r.content[0].text}`);
r = await todo({ action: "list" });
expect(r.content[0].text.includes("⛓ #1,#2"), "list shows blockedBy edges");

// 7. get：blockedBy + 反向 blocks 边（须在 delete #3 之前）
r = await todo({ action: "get", id: 1 });
expect(r.content[0].text.includes("blockedBy: #2"), "get shows blockedBy");
expect(r.content[0].text.includes("blocks: #3"), "get derives reverse blocks edges");

// 8. 非法迁移：completed → in_progress 拒绝且状态不动
r = await todo({ action: "update", id: 2, status: "completed" });
expect(r.content[0].text === "Updated #2 (in_progress → completed)", `complete: ${r.content[0].text}`);
r = await todo({ action: "update", id: 2, status: "in_progress" });
expect(r.content[0].text === "Error: illegal transition completed → in_progress", `illegal content: ${r.content[0].text}`);
expect(r.details.error === "illegal transition completed → in_progress", "details.error carries bare message");
expect(state().tasks.find((t) => t.id === 2).status === "completed", "rejected call left state untouched");

// 9. 墓碑：delete 后 list 隐藏、includeDeleted 可见、重复 delete 报错
r = await todo({ action: "delete", id: 3 });
expect(r.content[0].text === "Deleted #3: Ship it", `delete content: ${r.content[0].text}`);
r = await todo({ action: "list" });
expect(!r.content[0].text.includes("#3"), "tombstone hidden from list");
r = await todo({ action: "list", includeDeleted: true });
expect(r.content[0].text.includes("#3"), "includeDeleted reveals tombstone");
r = await todo({ action: "delete", id: 3 });
expect(r.content[0].text === "Error: #3 is already deleted", `re-delete: ${r.content[0].text}`);

// 10. 空态 list
r = await todo({ action: "list", status: "deleted" });
expect(r.content[0].text === "No tasks", `empty list: ${r.content[0].text}`);

// 11. clear
r = await todo({ action: "clear" });
expect(r.content[0].text === "Cleared 3 tasks", `clear content: ${r.content[0].text}`);
expect(state().tasks.length === 0 && state().nextId === 1, "clear resets id counter");

// 12. 会话回放：最后一个快照胜出（上游 replayFromBranch 语义）
const snap = (tasks, nextId) => ({
	role: "toolResult",
	toolCallId: `c${nextId}`,
	toolName: "todo",
	content: [{ type: "text", text: "snapshot" }],
	details: { action: "create", params: {}, tasks, nextId },
});
const messages = [
	{ role: "user", content: [{ type: "text", text: "hi" }] },
	snap(
		[
			{ id: 1, subject: "a", status: "completed" },
			{ id: 2, subject: "b", status: "pending" },
		],
		3,
	),
	{ role: "assistant", content: [{ type: "text", text: "working" }] },
	snap([{ id: 1, subject: "a", status: "completed" }], 2),
];
globalThis.__pi_todo_replay(JSON.stringify(messages));
expect(state().tasks.length === 1 && state().tasks[0].id === 1, "replay takes last snapshot");
expect(state().nextId === 2, "replay restores nextId");
// 回放后继续 create：id 从恢复的 nextId 续
r = await todo({ action: "create", subject: "next" });
expect(r.details.tasks.find((t) => t.subject === "next").id === 2, "ids continue after replay");

// 13. prompt 引导注入 + 工具注册
expect(globalThis.__pi_tool_names().includes("todo"), "todo tool registered");
expect(globalThis.__pi_system_prompt().includes("# Todo list"), "prompt guidance section present");
expect(globalThis.__pi_system_prompt().includes("never batch completions"), "guidelines injected");

// flush：fire-and-forget 的 emit fetch 需要事件循环泵动才能落进 mock 服务
await sleep(400);

// 14. todo_updated 事件流（成功变更都发出）
const updates = events.filter((e) => e.type === "todo_updated");
expect(updates.length >= 12, `todo_updated events emitted (${updates.length})`);
expect(
	updates.every((e) => Array.isArray(e.tasks) && typeof e.nextId === "number"),
	"todo_updated payload shape",
);

srv.close();
if (failed) {
	console.error(`${failed} assertion(s) failed`);
	process.exit(1);
}
console.log("OK todo tool: create/update/no-op/status-machine/deps/tombstone/get/clear/replay/prompt/events all verified");
process.exit(0);
