// MCP adapter test (M4): boots the bundle against a mock streamable-http MCP
// server (plain-JSON responses) and verifies end-to-end:
//   mcp_config → initialize → tools/list → mcp__mock__echo registered into
//   agent.state.tools → execute runs approval + tools/call → text result.
import { createServer } from "node:http";

const rpcOk = (id, result) => JSON.stringify({ jsonrpc: "2.0", id, result });
const toolCalls = [];

const srv = createServer((req, res) => {
	let body = "";
	req.on("data", (c) => (body += c));
	req.on("end", () => {
		if (req.url === "/mcp") {
			res.setHeader("content-type", "application/json");
			const msg = JSON.parse(body || "{}");
			if (msg.method === "initialize") {
				res.end(
					rpcOk(msg.id, {
						protocolVersion: "2025-06-18",
						capabilities: { tools: {} },
						serverInfo: { name: "mock", version: "1.0.0" },
					}),
				);
			} else if (msg.method === "tools/list") {
				res.end(
					rpcOk(msg.id, {
						tools: [
							{
								name: "echo",
								description: "Echo the given text back",
								inputSchema: {
									type: "object",
									properties: { input: { type: "string", description: "text to echo" } },
									required: ["input"],
								},
							},
						],
					}),
				);
			} else if (msg.method === "tools/call") {
				toolCalls.push(msg.params);
				res.end(
					rpcOk(msg.id, {
						content: [{ type: "text", text: `echo: ${msg.params.arguments.input}` }],
					}),
				);
			} else if (msg.id === undefined || msg.id === null) {
				res.statusCode = 202; // notification (initialized)
				res.end();
			} else {
				res.end(
					JSON.stringify({ jsonrpc: "2.0", id: msg.id, error: { message: "method not found" } }),
				);
			}
			return;
		}
		if (req.url === "/hostcall") {
			const { method, payload } = JSON.parse(body || "{}");
			res.setHeader("content-type", "application/json");
			if (method === "agent_event") {
				res.end('{"ok":true}');
			} else if (method === "mcp_config") {
				res.end(
					JSON.stringify({ servers: [{ name: "mock", url: "http://127.0.0.1:19999/mcp" }] }),
				);
			} else if (method === "approval_request") {
				res.end(JSON.stringify({ decision: "allow" }));
			} else if (method === "fs") {
				const { op } = payload;
				const empty =
					op === "exists" ? "false" : op === "listDir" ? "[]" : op === "readTextLines" ? "[]" : "null";
				res.end(JSON.stringify({ ok: true, value: JSON.parse(empty) }));
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

globalThis.__PI_CONFIG = { port: 19999, dataDir: "/data" };
await import("./dist/agent.js");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// connectMcpServers 异步跑在 fetch I/O 上：轮询 agent.state.tools 直到 echo 注册
const deadline = Date.now() + 10_000;
let registered = false;
while (Date.now() < deadline) {
	await sleep(200);
	registered = (globalThis.__pi_tool_names?.() ?? []).includes("mcp__mock__echo");
	if (registered) break;
}
if (!registered) {
	console.error("FAIL: mcp__mock__echo never registered. tools:", globalThis.__pi_tool_names?.());
	srv.close();
	process.exit(1);
}
console.log("OK mcp: mock server connected, mcp__mock__echo registered");

const called = await globalThis.__pi_tool_call("mcp__mock__echo", { input: "hello-mcp" });
const text = called.content?.[0]?.text ?? "";
if (text !== "echo: hello-mcp") {
	console.error("FAIL mcp echo:", text.slice(0, 300));
	srv.close();
	process.exit(1);
}
if (
	toolCalls.length !== 1 ||
	toolCalls[0].name !== "echo" ||
	toolCalls[0].arguments.input !== "hello-mcp"
) {
	console.error("FAIL mcp tools/call params:", JSON.stringify(toolCalls));
	srv.close();
	process.exit(1);
}
console.log("OK mcp: tools/call executed with approval and returned text result");
srv.close();
process.exit(0);
