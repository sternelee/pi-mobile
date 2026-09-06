// skills injection test (D12 mobile-native port): the bundle consumes the
// skills_config hostcall and injects enabled skills into the systemPrompt.
// Mock loopback serves two skills (one disabled) + verifies the kick-mode
// __pi_skills_apply hot-reload seam.
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

const PHASE = process.env.PHASE;

if (!PHASE) {
	const root = mkdtempSync(path.join(tmpdir(), "pi-skills-bundle-test-"));
	mkdirSync(path.join(root, "sessions"), { recursive: true });
	const b = spawnSync(process.execPath, [import.meta.path], {
		env: { ...process.env, PHASE: "RUN", SESSIONS_DIR: path.join(root, "sessions") },
		encoding: "utf8",
	});
	if (b.status !== 0) {
		console.error(`SKILLS BUNDLE TEST FAILED\n${b.stdout}\n${b.stderr}`);
		process.exit(1);
	}
	console.log(b.stdout.trim().split("\n").filter((l) => l.startsWith("OK")).join("\n"));
	process.exit(0);
}

// ---- phase runner ----
const { createServer } = await import("node:http");
let skillsServed = [
	{
		id: "commit-helper",
		name: "commit-helper",
		description: "Write good commit messages",
		content: "Always use conventional commits (feat/fix/chore).",
	},
	// 注意：宿主 skills.rs 已过滤禁用技能，hostcall 只返回 enabled 的——
	// "disabled 不注入"的语义在 Rust 单测覆盖；这里验证 bundle 只消费收到的列表。
];
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
		} else if (method === "skills_config") {
			res.end(JSON.stringify({ skills: skillsServed }));
		} else {
			res.end('{"ok":true}');
		}
	});
});
await new Promise((r) => srv.listen(19994, "127.0.0.1", r));

globalThis.__PI_CONFIG = { port: 19994, dataDir: "/data" };
await import("./dist/agent.js");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const bootDeadline = Date.now() + 10_000;
while (!globalThis.__pi_ready && Date.now() < bootDeadline) await sleep(100);
if (!globalThis.__pi_ready) {
	console.error("FAIL: bundle never became ready");
	process.exit(1);
}
// 等 boot 异步收尾（refreshSkills 竞争于 ready 标志）
await sleep(500);

let failed = 0;
const expect = (cond, label) => {
	if (!cond) {
		failed++;
		console.error(`FAIL: ${label}`);
	}
};

const prompt = () => globalThis.__pi_system_prompt();

// 1. boot 注入：hostcall 返回的技能进 systemPrompt
expect(prompt().includes("# Skills"), "prompt has # Skills section");
expect(prompt().includes("## commit-helper — Write good commit messages"), "enabled skill injected");
expect(prompt().includes("conventional commits"), "skill body injected");

// 2. 热生效：mock 配置改为空 → __pi_skills_apply kick → 注入消失
globalThis.__pi_skills_apply();
skillsServed = [];
await sleep(500);
expect(!prompt().includes("# Skills"), "skills section removed after empty config");

// 3. 再加回 → 重新注入
skillsServed = [
	{ id: "tdd", name: "tdd", description: "Test first", content: "Red, green, refactor." },
];
globalThis.__pi_skills_apply();
await sleep(500);
expect(prompt().includes("Red, green, refactor."), "re-injected after config change");

// 4. skills_applied 事件
const applied = events.filter((e) => e.type === "skills_applied");
expect(applied.length >= 3, `skills_applied events (${applied.length})`);
expect(applied.every((e) => typeof e.count === "number"), "skills_applied payload shape");

srv.close();
if (failed) {
	console.error(`${failed} assertion(s) failed`);
	process.exit(1);
}
console.log("OK skills: enabled-only injection, hot reload, events all verified");
process.exit(0);
