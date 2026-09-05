// Approval flow test (M3): mutating tools must round-trip the approval
// hostcall before touching disk. Exercises the same execute path the agent
// loop uses, via the __pi_tool_call seam. Mock host policy: first write →
// deny, second → allow. Asserts deny keeps the file untouched.
import { createServer } from "node:http";
import { mkdtempSync, mkdirSync, readFileSync, existsSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

const ws = mkdtempSync(path.join(tmpdir(), "pi-appr-ws-"));
let approvalsSeen = 0;

const srv = createServer((req, res) => {
	let body = "";
	req.on("data", (c) => (body += c));
	req.on("end", () => {
		const { method, payload } = JSON.parse(body || "{}");
		res.setHeader("content-type", "application/json");
		if (method === "agent_event") {
			res.end('{"ok":true}');
		} else if (method === "fs") {
			// boot 恢复路径：空 sessions 目录
			const { op } = payload;
			const empty =
				op === "exists" ? "false" : op === "listDir" ? "[]" : op === "readTextLines" ? "[]" : "null";
			res.end(JSON.stringify({ ok: true, value: JSON.parse(empty) }));
		} else if (method === "approval_request") {
			approvalsSeen++;
			if (!["write", "edit"].includes(payload.tool)) {
				res.end(JSON.stringify({ error: `unexpected approval for ${payload.tool}` }));
				return;
			}
			const a = payload.args ?? {};
			if (typeof a.path !== "string" || (payload.tool === "write" ? typeof a.content !== "string" : typeof a.oldText !== "string")) {
				res.end(JSON.stringify({ error: "approval payload missing required args" }));
				return;
			}
			// deny 首次（write），其余放行
			const decision = approvalsSeen === 1 ? "deny" : "allow";
			res.end(JSON.stringify({ decision }));
		} else if (method === "ask_user") {
			// 模拟用户作答：选 B + 备注语
			res.end(
				JSON.stringify({
					response: { kind: "selection", selections: ["Option B"], comment: "because B" },
				}),
			);
		} else if (method === "tool") {
			if (payload.name === "write") {
				writeFileSync(path.join(ws, payload.args.path), payload.args.content);
				res.end(JSON.stringify({ text: "ok" }));
			} else if (payload.name === "edit") {
				const target = path.join(ws, payload.args.path);
				const cur = readFileSync(target, "utf8");
				const count = cur.split(payload.args.oldText).length - 1;
				if (count === 0) {
					res.end(JSON.stringify({ error: "oldText not found in file" }));
					return;
				}
				if (count > 1 && !payload.args.replaceAll) {
					res.end(JSON.stringify({ error: `oldText occurs ${count} times` }));
					return;
				}
				const next = payload.args.replaceAll
					? cur.split(payload.args.oldText).join(payload.args.newText)
					: cur.replace(payload.args.oldText, payload.args.newText);
				writeFileSync(target, next);
				res.end(JSON.stringify({ text: "ok" }));
			} else {
				res.end(JSON.stringify({ text: "ok" }));
			}
		} else {
			res.end('{"ok":true}');
		}
	});
});
await new Promise((r) => srv.listen(19999, "127.0.0.1", r));

globalThis.__PI_CONFIG = { port: 19999, dataDir: "/data" };
await import("./dist/agent.js");

const target = path.join(ws, "t.txt");
writeFileSync(target, "original");

// 1) deny：工具返回错误文案，文件未被触碰
const denied = await globalThis.__pi_tool_call("write", { path: "t.txt", content: "should not land" });
const deniedText = denied.content?.[0]?.text ?? "";
if (!/did not approve/i.test(deniedText)) {
	console.error("FAIL deny: unexpected tool result:", deniedText.slice(0, 200));
	process.exit(1);
}
if (readFileSync(target, "utf8") !== "original") {
	console.error("FAIL deny: file was modified despite denial");
	process.exit(1);
}
console.log("OK deny: write blocked, file untouched");

// 2) allow：写入成功
const allowed = await globalThis.__pi_tool_call("write", { path: "t.txt", content: "approved content" });
const allowedText = allowed.content?.[0]?.text ?? "";
if (allowedText !== "ok") {
	console.error("FAIL allow: unexpected tool result:", allowedText.slice(0, 200));
	process.exit(1);
}
if (readFileSync(target, "utf8") !== "approved content") {
	console.error("FAIL allow: file content wrong");
	process.exit(1);
}
console.log("OK allow: write executed");

// 3) edit：审批后精确替换
const edited = await globalThis.__pi_tool_call("edit", {
	path: "t.txt",
	oldText: "approved content",
	newText: "edited content",
});
if ((edited.content?.[0]?.text ?? "") !== "ok") {
	console.error("FAIL edit:", (edited.content?.[0]?.text ?? "").slice(0, 200));
	process.exit(1);
}
if (readFileSync(target, "utf8") !== "edited content") {
	console.error("FAIL edit: file content wrong");
	process.exit(1);
}
console.log("OK edit: snippet replaced");

// 4) edit：oldText 未找到 → 工具错误
const miss = await globalThis.__pi_tool_call("edit", { path: "t.txt", oldText: "nope", newText: "x" });
if (!/not found/i.test(miss.content?.[0]?.text ?? "")) {
	console.error("FAIL edit-miss:", (miss.content?.[0]?.text ?? "").slice(0, 200));
	process.exit(1);
}
console.log("OK edit: missing oldText reported as error");

// 5) ask_user（pi-ask-user 等价）：hostcall 返回答案 → 工具格式化结果
const asked = await globalThis.__pi_tool_call("ask_user", {
	question: "Which option?",
	options: [{ title: "Option A" }, { title: "Option B" }],
});
const askedText = asked.content?.[0]?.text ?? "";
if (!askedText.includes("Option B") || !askedText.includes("because B")) {
	console.error("FAIL ask_user:", askedText.slice(0, 200));
	process.exit(1);
}
console.log("OK ask_user: answer formatted into tool result");

// 6) 只读工具不应触发审批（write×2 + edit×2，含未命中 edit —— 审批在执行前）
await globalThis.__pi_tool_call("ls", {});
if (approvalsSeen !== 4) {
	console.error(`FAIL: expected exactly 4 approval requests (write×2 + edit×2), saw ${approvalsSeen}`);
	process.exit(1);
}
console.log("OK read-only tools bypass approval");

srv.close();
console.log("OK approval flow complete");
