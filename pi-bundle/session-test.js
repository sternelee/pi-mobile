// Session persistence round-trip test (M2/M3): pi-native JSONL over fs hostcall.
// Driver (no PHASE): phase A ×2 (each: new session → prompt + sanitized
// toolResult persisted) then phase B (fresh process: restores latest, switches
// back to the first session via __pi_open_session, asserts history).
// Uses a real temp dir as the fs backend — same contract as loopback.rs `fs`.
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readdirSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

const PHASE = process.env.PHASE;

if (!PHASE) {
	// ---- driver ----
	const root = mkdtempSync(path.join(tmpdir(), "pi-sess-test-"));
	const sessions = path.join(root, "sessions");
	mkdirSync(sessions);
	const tokenA = `PERSIST-A-${Date.now()}`;
	const tokenB = `PERSIST-B-${Date.now()}`;
	const env = (phase, token) => ({
		...process.env,
		PHASE: phase,
		SESSIONS_DIR: sessions,
		PI_TEST_TOKEN: token,
	});

	for (const token of [tokenA, tokenB]) {
		const a = spawnSync(process.execPath, [import.meta.path], { env: env("A", token), encoding: "utf8" });
		if (a.status !== 0) {
			console.error(`PHASE A (${token}) FAILED\n` + a.stdout + a.stderr);
			process.exit(1);
		}
	}
	console.log("PHASE A OK ×2 — two sessions created, user message + sanitized toolResult persisted");

	// extract session ids per token from the JSONL files
	const idFor = (token) => {
		for (const dir of readdirSync(sessions)) {
			for (const f of readdirSync(path.join(sessions, dir))) {
				const c = readFileSync(path.join(sessions, dir, f), "utf8");
				if (c.includes(token)) return JSON.parse(c.split("\n")[0]).id;
			}
		}
		return null;
	};
	const idA = idFor(tokenA);
	const idB = idFor(tokenB);
	if (!idA || !idB || idA === idB) {
		console.error("driver: cannot resolve two distinct session ids", { idA, idB });
		process.exit(1);
	}

	const b = spawnSync(process.execPath, [import.meta.path], {
		env: { ...env("B"), PI_ID_A: idA, PI_ID_B: idB, PI_TOKEN_A: tokenA, PI_TOKEN_B: tokenB },
		encoding: "utf8",
	});
	if (b.status !== 0) {
		console.error("PHASE B FAILED\n" + b.stdout + b.stderr);
		process.exit(1);
	}
	console.log(b.stdout.trim().split("\n").filter((l) => l.startsWith("PHASE") || l.startsWith("OK")).join("\n"));

	// pi-format evidence: first line of a session file is a v4 header
	for (const dir of readdirSync(sessions)) {
		const file = path.join(sessions, dir, readdirSync(path.join(sessions, dir))[0]);
		const header = JSON.parse(readFileSync(file, "utf8").split("\n")[0]);
		if (header.kind !== "header" || header.version !== 4) {
			console.error("NOT a pi v4 session header:", header);
			process.exit(1);
		}
	}
	console.log(`OK session JSONL is pi-v4 format; round-trip + switch complete (${idA} ↔ ${idB})`);
	process.exit(0);
}

// ---- phase runner (A: persist into a fresh session, B: restore + switch) ----
const { createServer } = await import("node:http");
const fsp = await import("node:fs");
const sessionsDir = process.env.SESSIONS_DIR;
const token = process.env.PI_TEST_TOKEN;

const strip = (p) =>
	p === "/pi-sessions" ? "" : p.startsWith("/pi-sessions/") ? p.slice("/pi-sessions/".length) : p;
const J = (rel) => path.join(sessionsDir, rel);

function fsOp(payload) {
	const rel = strip(payload.path ?? "");
	try {
		switch (payload.op) {
			case "readTextFile":
				return { ok: true, value: fsp.readFileSync(J(rel), "utf8") };
			case "readTextLines": {
				const s = fsp.readFileSync(J(rel), "utf8").replace(/\n$/, "");
				return { ok: true, value: s.split("\n").slice(0, payload.maxLines ?? undefined) };
			}
			case "writeFile":
				fsp.writeFileSync(J(rel), payload.content);
				return { ok: true, value: null };
			case "appendFile":
				fsp.appendFileSync(J(rel), payload.content);
				return { ok: true, value: null };
			case "renameFile":
				fsp.renameSync(J(rel), J(strip(payload.to)));
				return { ok: true, value: null };
			case "fileInfo": {
				const st = fsp.statSync(J(rel));
				return {
					ok: true,
					value: {
						name: path.basename(rel),
						path: payload.path,
						kind: st.isDirectory() ? "directory" : "file",
						size: st.size,
						mtimeMs: st.mtimeMs,
					},
				};
			}
			case "listDir": {
				const items = fsp.readdirSync(J(rel), { withFileTypes: true }).map((e) => {
					const st = fsp.statSync(path.join(J(rel), e.name));
					const childRel = rel ? `${rel}/${e.name}` : e.name;
					return {
						name: e.name,
						path: `/pi-sessions/${childRel}`,
						kind: e.isDirectory() ? "directory" : "file",
						size: st.size,
						mtimeMs: st.mtimeMs,
					};
				});
				items.sort((x, y) => x.name.localeCompare(y.name));
				return { ok: true, value: items };
			}
			case "exists":
				return { ok: true, value: fsp.existsSync(J(rel)) };
			case "createDir":
				payload.recursive ? fsp.mkdirSync(J(rel), { recursive: true }) : fsp.mkdirSync(J(rel));
				return { ok: true, value: null };
			case "remove":
				fsp.rmSync(J(rel), { recursive: payload.recursive ?? false });
				return { ok: true, value: null };
			default:
				return { ok: false, error: { code: "invalid", message: `unknown op ${payload.op}` } };
		}
	} catch (e) {
		return {
			ok: false,
			error: { code: e.code === "ENOENT" ? "not_found" : "unknown", message: String(e) },
		};
	}
}

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
		} else if (method === "fs") {
			res.end(JSON.stringify(fsOp(payload)));
		} else if (method === "creds_get") {
			res.end('{"apiKey":"sk-test-fake"}');
		} else {
			res.end('{"ok":true}');
		}
	});
});
await new Promise((r) => srv.listen(19999, "127.0.0.1", r));

globalThis.__PI_CONFIG = { port: 19999, dataDir: "/data" };
await import("./dist/agent.js");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// 生产时序对齐：agent_init 会等 __pi_restored 才放行 prompt
const bootDeadline = Date.now() + 10_000;
while (!globalThis.__pi_restored && Date.now() < bootDeadline) await sleep(100);

const waitUntil = async (fn, ms = 15_000) => {
	const deadline = Date.now() + ms;
	while (Date.now() < deadline) {
		if (fn()) return true;
		await sleep(200);
	}
	return fn();
};
const sessionsContain = (needle) => () => {
	try {
		for (const dir of fsp.readdirSync(sessionsDir)) {
			for (const f of fsp.readdirSync(path.join(sessionsDir, dir))) {
				if (readFileSync(path.join(sessionsDir, dir, f), "utf8").includes(needle)) return true;
			}
		}
	} catch {}
	return false;
};

if (PHASE === "A") {
	// 每次进新会话：tokenA/tokenB 各落一个独立 JSONL
	globalThis.__pi_new_session();
	globalThis.__pi_prompt(token);
	globalThis.__pi_persist_direct({
		role: "toolResult",
		toolCallId: "call_test_1",
		toolName: "ls",
		content: [{ type: "text", text: `tool result ${token}` }],
		details: {},
		usage: undefined,
		addedToolNames: undefined,
		isError: false,
		timestamp: Date.now(),
	});
	const hit = await waitUntil(sessionsContain(token) && sessionsContain(`tool result ${token}`));
	srv.close();
	if (!hit) {
		console.error("user message / sanitized toolResult never hit disk. events:", JSON.stringify(events).slice(0, 800));
		process.exit(1);
	}
	console.log(`PHASE A OK (${token})`);
	process.exit(0);
}

if (PHASE === "B") {
	const idA = process.env.PI_ID_A;
	const idB = process.env.PI_ID_B;
	const tokenA = process.env.PI_TOKEN_A;
	const tokenB = process.env.PI_TOKEN_B;

	// boot 后应自动恢复最新会话（B）
	let h = JSON.parse(globalThis.__pi_history());
	if (h.sessionId !== idB || !JSON.stringify(h.messages).includes(tokenB)) {
		console.error(`FAIL: expected latest session ${idB} restored, got`, JSON.stringify(h).slice(0, 300));
		process.exit(1);
	}
	console.log("PHASE B OK — boot restored latest session (B)");

	// 切回 A：历史变为 A 的内容（kick+轮询——生产时序对齐 Rust session_open：
	// eval 不得返回挂 I/O 的 Promise，结果经 __pi_session_open_result 轮询）
	if (globalThis.__pi_open_session(idA) !== "started") {
		console.error("FAIL: __pi_open_session did not kick (expected 'started')");
		process.exit(1);
	}
	const opened = await waitUntil(() => globalThis.__pi_session_open_result !== null);
	if (!opened) {
		console.error("FAIL: __pi_session_open_result never settled");
		process.exit(1);
	}
	h = JSON.parse(globalThis.__pi_history());
	if (h.sessionId !== idA || !JSON.stringify(h.messages).includes(tokenA) || JSON.stringify(h.messages).includes(tokenB)) {
		console.error("FAIL: switch back to A failed:", JSON.stringify(h).slice(0, 400));
		process.exit(1);
	}
	console.log("PHASE B OK — __pi_open_session switched back to session A");

	// 新建空会话
	globalThis.__pi_new_session();
	h = JSON.parse(globalThis.__pi_history());
	if (h.sessionId !== null || h.messages.length !== 0) {
		console.error("FAIL: new session not empty:", JSON.stringify(h).slice(0, 300));
		process.exit(1);
	}
	srv.close();
	console.log("PHASE B OK — __pi_new_session starts blank");
	process.exit(0);
}
