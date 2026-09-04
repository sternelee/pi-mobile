// Local sanity harness: mini loopback + load bundle + fake prompt run
import { createServer } from "node:http";
import { readFileSync } from "node:fs";

const events = [];
const srv = createServer((req, res) => {
	let body = "";
	req.on("data", (c) => (body += c));
	req.on("end", () => {
		const { method, payload } = JSON.parse(body || "{}");
		res.setHeader("content-type", "application/json");
		if (method === "agent_event") {
			events.push(payload);
			if (payload.type === "agent_end" || payload.type === "agent_error" || payload.type === "agent_start") {
				console.log("EVENT:", payload.type, JSON.stringify(payload).slice(0, 160));
			}
			res.end('{"ok":true}');
		} else if (method === "creds_get") {
			res.end(JSON.stringify({ apiKey: process.env.ANTHROPIC_API_KEY ?? "sk-test-fake" }));
		} else if (method === "tool") {
			res.end(JSON.stringify({ text: `host-tool ${payload.name} ok` }));
		} else {
			res.end('{"ok":true}');
		}
	});
});
await new Promise((r) => srv.listen(19999, "127.0.0.1", r));

globalThis.__PI_CONFIG = { port: 19999 };
await import("./dist/agent.js");
console.log("ready:", globalThis.__pi_ready, "status:", globalThis.__pi_status());

if (!process.env.ANTHROPIC_API_KEY) {
	console.log("no API key — bundle loaded OK, skipping live prompt");
	srv.close();
	process.exit(0);
}

globalThis.__pi_prompt("Reply with exactly: hello from embedded pi. Then stop.");
for (let i = 0; i < 60 && (globalThis.__pi_status(), JSON.parse(globalThis.__pi_status()).busy); i++) {
	await new Promise((r) => setTimeout(r, 250));
}
console.log("final status:", globalThis.__pi_status(), "events:", events.length);
srv.close();
