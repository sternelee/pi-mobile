// AI provider/model selection flow test (pi-ai createModels architecture).
// Boots the bundle against a mock loopback in two phases:
//   BASE     — no providerConfig: fallback model + provider catalog globals
//              (providers_listed / models_refresh static / model hot-switch)
//   SELECTED — providerConfig {google-gemini, gemini-2.5-flash}: boot resolves
//              the full catalog model (Gemini compat remap: openai-completions)
// 断言前置：boot 的异步收尾需 sleep/轮询让 I/O 泵动（见 todo-test.js 注记）。
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

const PHASE = process.env.PHASE;

if (!PHASE) {
	// ---- driver ----
	const results = [];
	for (const phase of ["BASE", "SELECTED"]) {
		const root = mkdtempSync(path.join(tmpdir(), `pi-provider-test-${phase}-`));
		mkdirSync(path.join(root, "sessions"), { recursive: true });
		const b = spawnSync(process.execPath, [import.meta.path], {
			env: { ...process.env, PHASE: phase, SESSIONS_DIR: path.join(root, "sessions") },
			encoding: "utf8",
		});
		if (b.status !== 0) {
			console.error(`PROVIDER TEST FAILED (${phase})\n${b.stdout}\n${b.stderr}`);
			process.exit(1);
		}
		results.push(b.stdout.trim().split("\n").filter((l) => l.startsWith("OK")));
	}
	console.log(results.flat().join("\n"));
	console.log("OK provider flow: all pi-ai catalog assertions passed");
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
		} else if (method === "creds_get") {
			// 无凭证：返回结构化错误（CredentialStore.read → undefined）
			res.end(JSON.stringify({ error: "no credential" }));
		} else {
			res.end('{"ok":true}');
		}
	});
});
await new Promise((r) => srv.listen(19996, "127.0.0.1", r));

const assert = (cond, msg) => {
	if (!cond) {
		console.error(`ASSERT FAILED: ${msg}`);
		process.exit(1);
	}
	console.log(`OK ${msg}`);
};

globalThis.__PI_CONFIG =
	PHASE === "SELECTED"
		? {
				port: 19996,
				dataDir: "/data",
				providerConfig: { provider: "google-gemini", modelId: "gemini-2.5-flash" },
			}
		: { port: 19996, dataDir: "/data" };
await import("./dist/agent.js");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const bootDeadline = Date.now() + 10_000;
while (!globalThis.__pi_ready && Date.now() < bootDeadline) await sleep(100);
assert(globalThis.__pi_ready, `${PHASE}: boot ready`);
await sleep(300);

const call = (name, arg) => globalThis[name](arg);

if (PHASE === "BASE") {
	// ---- 兜底模型：未配置选择时回落 deepseek 测试目录项 ----
	const cur = JSON.parse(call("__pi_model_current"));
	assert(cur.provider === "deepseek" && cur.id === "deepseek-v4-flash", "BASE: fallback model is catalog deepseek");

	// ---- provider 目录：4 家，含 Gemini 兼容层重映射 ----
	call("__pi_providers_list");
	await sleep(200);
	const listed = events.find((e) => e.type === "providers_listed");
	assert(listed, "BASE: providers_listed emitted");
	assert(
		JSON.stringify(listed.providers.map((p) => p.id)) ===
			JSON.stringify(["openai", "openrouter", "deepseek", "google-gemini"]),
		"BASE: four providers in UI order",
	);
	const deepseek = listed.providers.find((p) => p.id === "deepseek");
	assert(deepseek.models.length > 0, "BASE: deepseek static catalog non-empty");
	const openai = listed.providers.find((p) => p.id === "openai");
	const openaiModel = openai.models.find((m) => m.id.includes("gpt"));
	assert(!!openaiModel, "BASE: openai catalog has gpt models");
	const gemini = listed.providers.find((p) => p.id === "google-gemini");
	const g25 = gemini.models.find((m) => m.id === "gemini-2.5-flash");
	assert(!!g25, "BASE: gemini catalog includes gemini-2.5-flash (upstream data)");

	// ---- 动态刷新 no-op（静态 provider）后 models_listed 回投 ----
	call("__pi_models_refresh", "deepseek");
	await sleep(200);
	const ml = events.find((e) => e.type === "models_listed" && e.provider === "deepseek");
	assert(ml && ml.models.length === deepseek.models.length, "BASE: static refresh returns full catalog");

	// ---- 未知模型拒绝 ----
	const bad = call("__pi_model_select", JSON.stringify({ provider: "openrouter", modelId: "nope" }));
	assert(bad.startsWith("unknown model"), "BASE: unknown model id rejected");

	// ---- Gemini 热切换：model_applied 事件 + 当前模型随动 ----
	const sel = call("__pi_model_select", JSON.stringify({ provider: "google-gemini", modelId: "gemini-2.5-flash" }));
	assert(sel === "started", "BASE: __pi_model_select kicks");
	await sleep(200);
	const applied = events.find((e) => e.type === "model_applied");
	assert(applied && applied.provider === "google-gemini" && applied.modelId === "gemini-2.5-flash", "BASE: model_applied emitted");
	const cur2 = JSON.parse(call("__pi_model_current"));
	assert(cur2.provider === "google-gemini" && cur2.id === "gemini-2.5-flash", "BASE: hot-switch visible in __pi_model_current");
} else {
	// ---- providerConfig boot：pi-ai 目录解析完整模型对象 ----
	const cur = JSON.parse(call("__pi_model_current"));
	assert(cur.provider === "google-gemini" && cur.id === "gemini-2.5-flash", "SELECTED: boot model resolved from providerConfig");
	assert(cur.name.length > 0, "SELECTED: catalog model has display name");
}

srv.close();
process.exit(0);
