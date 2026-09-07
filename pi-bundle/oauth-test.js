// OAuth 登录协调器测试：mock loopback 扮演宿主（oauth_pkce/oauth_listen/
// creds_json + 回调注入），oauthOverrides 把 token 端点指到本地。
// 覆盖：anthropic 标准 code 流 + openrouter 特例（callback_url、无
// client_id/state、交换换 api key）。
import { createServer } from "node:http";

const state = {
	callbackUrl: null,
	activeProvider: null,
	listen: null,
	tokenBody: null,
	stored: null,
};

const srv = createServer((req, res) => {
	let body = "";
	req.on("data", (c) => (body += c));
	req.on("end", () => {
		const { method, payload } = JSON.parse(body || "{}");
		res.setHeader("content-type", "application/json");
		if (method === "agent_event") {
			if (payload.type === "oauth_open_url") {
				state.callbackUrl = payload.url;
				state.activeProvider = payload.provider;
			}
			res.end('{"ok":true}');
		} else if (method === "creds_get") {
			res.end('{"apiKey":"sk-test-fake"}');
		} else if (method === "oauth_pkce") {
			res.end(JSON.stringify({ verifier: "v".repeat(64), challenge: "c".repeat(43) }));
		} else if (method === "oauth_listen") {
			state.listen = { port: payload.port, path: payload.path };
			// 模拟宿主捕获：按本次登录的回调形态注入 URL
			setTimeout(() => {
				const verifier = "v".repeat(64);
				const isAnthropic = state.activeProvider === "anthropic";
				const url = isAnthropic
					? `http://localhost:53692/callback?code=ac123&state=${verifier}`
					: `http://localhost:${payload.port}${payload.path}?code=or-code`;
				globalThis.__pi_oauth_callback(url);
			}, 300);
			res.end(JSON.stringify({ ok: true, port: payload.port || 53692 }));
		} else if (method === "creds_json_get") {
			res.end(JSON.stringify({ json: null }));
		} else if (method === "creds_json_set") {
			state.stored = payload;
			res.end('{"ok":true}');
		} else if (req.url === "/oauth/token") {
			state.tokenBody = body;
			res.end(
				JSON.stringify(
					state.activeProvider === "openrouter"
						? { key: "or-key-1" }
						: { refresh_token: "r1", access_token: "a1", expires_in: 3600 },
				),
			);
		} else {
			res.end('{"ok":true}');
		}
	});
});
await new Promise((r) => srv.listen(19996, "127.0.0.1", r));

globalThis.__PI_CONFIG = {
	port: 19996,
	oauthOverrides: {
		anthropic: { tokenUrl: "http://127.0.0.1:19996/oauth/token" },
		openrouter: { tokenUrl: "http://127.0.0.1:19996/oauth/token" },
	},
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

const waitFor = async (fn, ms = 10_000) => {
	const deadline = Date.now() + ms;
	while (Date.now() < deadline) {
		const v = fn();
		if (v) return v;
		await new Promise((r) => setTimeout(r, 100));
	}
	return fn();
};

// ---- anthropic 标准 code 流 ----
globalThis.__pi_oauth_login("anthropic");
await waitFor(() => state.callbackUrl);
assert(state.callbackUrl?.startsWith("https://claude.ai/oauth/authorize"), "authorize url built");
assert(state.callbackUrl.includes("code_challenge_method=S256"), "PKCE S256 in authorize url");

await waitFor(() => state.tokenBody);
assert(JSON.parse(state.tokenBody).code === "ac123", "authorization code passed to exchange");
assert(JSON.parse(state.tokenBody).client_id === "9d1c250a-e61b-44d9-88ed-5944d1962f5e", "anthropic client id");

await waitFor(() => state.stored);
assert(state.stored?.provider === "anthropic", "credential stored under anthropic");
const cred = JSON.parse(state.stored?.json ?? "{}");
assert(cred.type === "oauth" && cred.access === "a1" && cred.refresh === "r1", "oauth credential shape");

// ---- openrouter 特例：callback_url 授权参数 + api key 交换 ----
state.stored = null;
globalThis.__pi_oauth_login("openrouter");
await waitFor(() => state.callbackUrl && state.activeProvider === "openrouter");
const orUrl = new URL(state.callbackUrl);
assert(orUrl.origin === "https://openrouter.ai", "openrouter authorize host");
assert(
	orUrl.searchParams.get("callback_url")?.startsWith("http://localhost:"),
	"openrouter callback_url param carries localhost redirect",
);
assert(
	!orUrl.searchParams.has("client_id") && !orUrl.searchParams.has("state"),
	"openrouter omits client_id/state (pi-ai parity)",
);
assert(orUrl.searchParams.get("code_challenge_method") === "S256", "openrouter PKCE method");

await waitFor(() => state.stored);
const orCred = JSON.parse(state.stored?.json ?? "{}");
assert(orCred.access === "or-key-1", "openrouter api key credential stored");
console.log("oauth coordinator OK");
process.exit(0);
