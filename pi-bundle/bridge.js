// pi-bundle/bridge.js — M2 桥（预构建 skal ABI 阶段）
// JS → Rust：HTTP loopback（bun 原生 fetch → 127.0.0.1 loopback 微服务）
// Rust → JS：skal_evaluate 同步调用本文件安装的全局函数
//
// 配置由宿主在求值本文件前注入：
//   globalThis.__pi_config = { port: <loopback 端口>, dataDir: "..." }
(() => {
  // 两个全局都认：真实 app 走 agent_init 的**大写** `__PI_CONFIG`，而 smoke()
  // 路径设的是小写 `__pi_config`（mod.rs:611）。原先只读小写 → bridge 在真实
  // app 里根本装不起来（抛 __pi_config missing），也解释了 netprobe 当初为何
  // 读小写：它是照着这条路径写的。
  const cfg = globalThis.__PI_CONFIG ?? globalThis.__pi_config;
  if (!cfg || !cfg.port) {
    throw new Error("__PI_CONFIG missing (host must evaluate config before bridge)");
  }
  const base = `http://127.0.0.1:${cfg.port}`;

  // 同步风格 hostcall（返回 Promise；skal_evaluate 会等待 Promise 落定）
  //
  // D14：必须带 `__hostToken`，且**调用时读取**——/hostcall 端点自身要认证，
  // 无 token 的请求会被拒（fail-closed）。这是第二个 hostcall 客户端，改
  // agent-main.js 时漏了它，真机上表现为 34 次 `host token ABSENT`。
  globalThis.__pi_hostcall = async function __pi_hostcall(method, payload) {
    const res = await fetch(`${base}/hostcall`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        method,
        payload: payload ?? null,
        __hostToken: (globalThis.__PI_CONFIG ?? globalThis.__pi_config)?.hostToken ?? null,
      }),
    });
    if (!res.ok) {
      throw new Error(`hostcall ${method} failed: HTTP ${res.status}`);
    }
    return res.json();
  };

  // Rust → JS 事件分发入口（宿主经 skal_evaluate 调用）
  const listeners = new Map();
  globalThis.__pi_on = function __pi_on(event, fn) {
    if (!listeners.has(event)) listeners.set(event, new Set());
    listeners.get(event).add(fn);
  };
  globalThis.__pi_dispatch = function __pi_dispatch(event, payloadJson) {
    const fns = listeners.get(event);
    const payload = payloadJson == null ? null : JSON.parse(payloadJson);
    if (fns) for (const fn of fns) fn(payload);
    return { delivered: fns ? fns.size : 0 };
  };

  globalThis.__pi_bridge_ready = true;
})();
