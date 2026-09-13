// pi-bundle/netprobe.js — iOS 真机网络能力探测（M5 排障用）
//
// 为什么单独一个文件：真机上 "loading models" 卡住时，症状是 fetch 既
// 不 resolve 也不 reject（既没有 models_listed 也没有 models_error 事件）。
// 需要区分是 DNS、TLS、还是 loopback（bun 自带 HTTP 栈）的问题。
//
// 与 smoke2.js 同样的纪律：**不要从脚本返回 Promise** ——
// skal_evaluate 的 waitForPromise 会阻塞 VM worker 线程，而 fetch 的 I/O
// 完成恰需要该线程 tick 事件循环（实测挂死）。所以立即返回 "started"，
// 每步结果增量写进 globalThis.__netprobe，由宿主轮询。
globalThis.__netprobe = { state: "started", steps: {} };

(async () => {
  const steps = {};
  const flush = () => {
    globalThis.__netprobe.steps = steps;
  };

  async function step(name, fn) {
    const t0 = Date.now();
    try {
      const extra = await fn();
      steps[name] = { ok: true, ms: Date.now() - t0, ...(extra || {}) };
    } catch (e) {
      steps[name] = { ok: false, ms: Date.now() - t0, error: String(e) };
    }
    flush();
  }

  // 1) DNS 解析（bun 自己的 resolver）
  await step("dns", async () => {
    const r = await Bun.dns.lookup("api.deepseek.com");
    return { addrs: (r || []).map((a) => a.address).slice(0, 3) };
  });

  // 2) loopback —— 宿主 Rust 的 HTTP 桥（验证 bun 的 HTTP 客户端 + 服务端）
  //    注意：本探测跑在后台线程，可能早于 agent_init 里的
  //    loopback::configure()，那时 __pi_config 还没注入 → 显式报
  //    skipped，不要让一个假失败掩盖真问题（早期版本就这么误导过）。
  await step("loopback", async () => {
    const port = globalThis.__pi_config?.port;
    if (!port) return { skipped: "__pi_config.port not set yet (configure 未跑)" };
    const res = await fetch(`http://127.0.0.1:${port}/hostcall`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ method: "ping", payload: { from: "netprobe" } }),
      signal: AbortSignal.timeout(5000),
    });
    return { status: res.status };
  });

  // 3) 外网 HTTPS（域名 → 同时考 DNS + TCP + TLS）
  await step("https_host", async () => {
    const res = await fetch("https://api.deepseek.com/", {
      signal: AbortSignal.timeout(8000),
    });
    return { status: res.status };
  });

  // 4) 外网 HTTPS（IP → 绕过 DNS，只考 TCP + TLS + 证书）
  //    国内网络对 1.1.1.1 常屏蔽，超时不一定意味着我们有问题。
  await step("https_ip", async () => {
    const res = await fetch("https://1.1.1.1/", {
      signal: AbortSignal.timeout(8000),
    });
    return { status: res.status };
  });

  globalThis.__netprobe.state = "done";
})();

"started";
