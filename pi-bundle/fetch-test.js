// fetch 工具测试（方案 B：宿主 reqwest）。
// 走 mock loopback 的 http hostcall 分支：
//   1. 工具已注册（__pi_tool_call 可达）
//   2. 正常 GET：宿主应答 {status, contentType, body, truncated} 透传为工具结果
//   3. 宿主 SSRF 拒绝（{error}）转 errContent
//   4. 参数透传：url/method 原样到达 hostcall
import { createServer } from "node:http";

const events = [];
const httpCalls = [];

const srv = createServer((req, res) => {
	let body = "";
	req.on("data", (c) => (body += c));
	req.on("end", () => {
		const { method, payload } = JSON.parse(body || "{}");
		res.setHeader("content-type", "application/json");
		if (method === "agent_event") {
			events.push(payload);
			res.end('{"ok":true}');
		} else if (method === "creds_get") {
			res.end('{"apiKey":"sk-test-fake"}');
		} else if (method === "fs") {
			const { op } = payload;
			const empty =
				op === "exists" ? "false" : op === "listDir" ? "[]" : op === "readTextLines" ? "[]" : "null";
			res.end(JSON.stringify({ ok: true, value: JSON.parse(empty) }));
		} else if (method === "http") {
			httpCalls.push(payload);
			if (/^http:\/\/127\./.test(payload?.url ?? "")) {
				// 模拟宿主 SSRF 防护：loopback 目标拒绝
				res.end(JSON.stringify({ error: "blocked private address: 127.0.0.1" }));
			} else if (payload?.url === "https://example.com/page") {
				res.end(
					JSON.stringify({
						status: 200,
						contentType: "text/html; charset=utf-8",
						body: "Title\n\nHello & world\n• item",
						truncated: false,
					}),
				);
			} else {
				res.end(JSON.stringify({ error: "unknown test url" }));
			}
		} else {
			res.end('{"ok":true}');
		}
	});
});
await new Promise((r) => srv.listen(19997, "127.0.0.1", r));

globalThis.__PI_CONFIG = { port: 19997 };
await import("./dist/agent.js");
const assert = (cond, msg) => {
	if (!cond) {
		console.error(`FAIL: ${msg}`);
		process.exit(1);
	}
	console.log(`OK: ${msg}`);
};
assert(globalThis.__pi_ready === true, "bundle ready");

// 1. 工具已注册
const r0 = await globalThis.__pi_tool_call("fetch", { url: "https://nope.invalid" });
assert(typeof r0 === "object" && "content" in r0, "fetch tool registered (reaches execute)");

// 2. 正常 GET：透传宿主应答
const r1 = await globalThis.__pi_tool_call("fetch", { url: "https://example.com/page" });
const text1 = r1?.content?.[0]?.text ?? "";
assert(r1?.isError !== true, "normal fetch is not an error");
assert(text1.includes("status 200"), "status surfaced");
assert(text1.includes("Hello & world"), "body text surfaced");
assert(httpCalls.at(-1)?.url === "https://example.com/page", "url passed to hostcall");

// 3. SSRF：宿主拒绝 → 错误内容（errContent 形态：文本含拒绝原因，无正常 body）
const r2 = await globalThis.__pi_tool_call("fetch", { url: "http://127.0.0.1:19997/hostcall" });
const text2 = r2?.content?.[0]?.text ?? "";
assert(text2.includes("blocked private address"), "SSRF refusal surfaces as tool error");
assert(!text2.includes("status 200"), "refusal carries no fake success");

srv.close();
process.exit(0);
