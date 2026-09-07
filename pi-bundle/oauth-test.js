// OAuth 代码流冒烟：mock loopback 扮演宿主（oauth_pkce/oauth_listen/creds_json
// + 回调注入），oauthOverrides 把 token 端点指到本地；走完整 anthropic code 流。
import { createServer } from "node:http";

const state = { callbackUrl: null, tokenBody: null, stored: null, tokenHits: 0 };
const srv = createServer((req, res) => {
	let body = "";
	req.on("data", (c) => (body += c));
	req.on("end", () => {
		const { method, payload } = JSON.parse(body || "{}");
		res.setHeader("content-type", "application/json");
		if (method === "agent_event") {
			if (payload.type === "oauth_open_url") state.callbackUrl = payload.url;
			res.end('{"ok":true}');
		} else if (method === "creds_get") {
			res.end('{"apiKey":"sk-test-fake"}');
		} else if (method === "oauth_pkce") {
			res.end(JSON.stringify({ verifier: "v".repeat(64), challenge: "c".repeat(43) }));
		} else if (method === "oauth_listen") {
			// 模拟宿主捕获：延迟注入回调 URL
			setTimeout(() => {
				const verifier = "v".repeat(64);
				globalThis.__pi_oauth_callback(`http://localhost:53692/callback?code=ac123&state=${verifier}`);
			}, 300);
			res.end(JSON.stringify({ ok: true, port: payload.port || 53692 }));
		} else if (method === "creds_json_get") {
			res.end(JSON.stringify({ json: null }));
		} else if (method === "creds_json_set") {
			state.stored = payload;
			res.end('{"ok":true}');
		} else if (req.url === "/oauth/token") {
			state.tokenHits += 1;
			state.tokenBody = body;
			res.end(JSON.stringify({ refresh_token: "r1", access_token: "a1", expires_in: 3600 }));
		} else {
			res.end('{"ok":true}');
		}
	});
});
await new Promise((r) => srv.listen(19996, "127.0.0.1", r));

globalThis.__PI_CONFIG = {
	port: 19996,
	oauthOverrides: { anthropic: { tokenUrl: "http://127.0.0.1:19996/oauth/token" } },
};
await import("./dist/agent.js");
const assert = (c, m) => {
	if (!c) {
		console.error("FAIL:", m);
		process.exit(1);
	}
	console.log("OK:", m);
};
assert(globalThis.__pi_ready === true, "bundle ready");

const loginDone = new Promise((resolve) => {
	const orig = globalThis.__pi_oauth_login;
	// 事件顺序：先启动登录，等授权 URL 产生后再注入回调
});
globalThis.__pi_oauth_login("anthropic");
for (let i = 0; i < 50 && !state.callbackUrl; i++) await new Promise((r) => setTimeout(r, 100));
assert(state.callbackUrl?.startsWith("https://claude.ai/oauth/authorize"), "authorize url built");
assert(state.callbackUrl.includes("code_challenge_method=S256"), "PKCE S256 in authorize url");

for (let i = 0; i < 100 && !state.tokenBody; i++) await new Promise((r) => setTimeout(r, 100));
assert(state.tokenBody, "token exchange hit mock endpoint");
const sent = JSON.parse(state.tokenBody);
assert(sent.code === "ac123", "authorization code passed to exchange");
assert(sent.client_id === "9d1c250a-e61b-44d9-88ed-5944d1962f5e", "anthropic client id");

for (let i = 0; i < 50 && !state.stored; i++) await new Promise((r) => setTimeout(r, 100));
assert(state.stored?.provider === "anthropic", "credential stored under anthropic");
const cred = JSON.parse(state.stored?.json ?? "{}");
assert(cred.type === "oauth" && cred.access === "a1" && cred.refresh === "r1", "oauth credential shape");
process.exit(0);
