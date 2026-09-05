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
			if (payload.tool !== "write") {
				res.end(JSON.stringify({ error: `unexpected approval for ${payload.tool}` }));
				return;
			}
			if (!payload.args?.path || typeof payload.args.content !== "string") {
				res.end(JSON.stringify({ error: "approval payload missing path/content" }));
				return;
			}
			// deny 首次（除非测试通过 APPROVAL_POLICY=allow 要求直接放行）
			const decision = approvalsSeen === 1 && process.env.APPROVAL_POLICY !== "allow" ? "deny" : "allow";
			res.end(JSON.stringify({ decision }));
		} else if (method === "tool") {
			if (payload.name === "write") {
				writeFileSync(path.join(ws, payload.args.path), payload.args.content);
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

// 3) 只读工具不应触发审批
await globalThis.__pi_tool_call("ls", {});
if (approvalsSeen !== 2) {
	console.error(`FAIL: expected exactly 2 approval requests (write×2), saw ${approvalsSeen}`);
	process.exit(1);
}
console.log("OK read-only tools bypass approval");

srv.close();
console.log("OK approval flow complete");
