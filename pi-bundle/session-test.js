// Session persistence round-trip test (M2 收尾): pi-native JSONL over fs hostcall.
// Driver (no PHASE): runs phase A (prompt → user message persisted) then
// phase B (fresh process, same dir → restored via __pi_history) as subprocesses.
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
	const token = `PERSIST-ME-${Date.now()}`;
	const env = (phase) => ({
		...process.env,
		PHASE: phase,
		SESSIONS_DIR: sessions,
		PI_TEST_TOKEN: token,
	});

	const a = spawnSync(process.execPath, [import.meta.path], { env: env("A"), encoding: "utf8" });
	if (a.status !== 0) {
		console.error("PHASE A FAILED\n" + a.stdout + a.stderr);
		process.exit(1);
	}
	console.log(a.stdout.trim());

	const b = spawnSync(process.execPath, [import.meta.path], { env: env("B"), encoding: "utf8" });
	if (b.status !== 0) {
		console.error("PHASE B FAILED\n" + b.stdout + b.stderr);
		process.exit(1);
	}
	console.log(b.stdout.trim());

	// pi-format evidence: first line of the session file is a v4 header
	const cwdDir = readdirSync(sessions).find((n) => n.startsWith("--"));
	const file = path.join(sessions, cwdDir, readdirSync(path.join(sessions, cwdDir))[0]);
	const header = JSON.parse(readFileSync(file, "utf8").split("\n")[0]);
	if (header.kind !== "header" || header.version !== 4) {
		console.error("NOT a pi v4 session header:", header);
		process.exit(1);
	}
	console.log(`OK session JSONL is pi-v4 format: ${path.relative(root, file)}`);
	console.log(`OK round-trip complete (${token})`);
	process.exit(0);
}

// ---- phase runner (A: persist, B: restore) ----
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

if (PHASE === "A") {
	globalThis.__pi_prompt(token);
	// poll until the user message is on disk (LLM call may fail — irrelevant)
	const deadline = Date.now() + 15_000;
	let hit = false;
	while (Date.now() < deadline) {
		await sleep(200);
		try {
			for (const dir of fsp.readdirSync(sessionsDir)) {
				for (const f of fsp.readdirSync(path.join(sessionsDir, dir))) {
					if (readFileSync(path.join(sessionsDir, dir, f), "utf8").includes(token)) hit = true;
				}
			}
		} catch {}
		if (hit) break;
	}
	srv.close();
	if (!hit) {
		console.error("user message never hit disk. events:", JSON.stringify(events).slice(0, 800));
		process.exit(1);
	}
	console.log("PHASE A OK — user message persisted to session JSONL");
	process.exit(0);
}

if (PHASE === "B") {
	const deadline = Date.now() + 15_000;
	let restored = null;
	while (Date.now() < deadline) {
		await sleep(200);
		try {
			const h = JSON.parse(globalThis.__pi_history());
			if (h.messages?.length) {
				restored = h;
				break;
			}
		} catch {}
	}
	srv.close();
	if (!restored) {
		console.error("history never restored. events:", JSON.stringify(events).slice(0, 800));
		process.exit(1);
	}
	const user = restored.messages.find((m) => m.role === "user");
	if (!user || !JSON.stringify(user.content).includes(token)) {
		console.error("restored messages missing token:", JSON.stringify(restored).slice(0, 600));
		process.exit(1);
	}
	console.log(
		`PHASE B OK — restored session ${restored.sessionId} with ${restored.messages.length} message(s), user content matches`,
	);
	process.exit(0);
}
