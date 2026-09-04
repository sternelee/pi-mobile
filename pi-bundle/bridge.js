// pi-bundle/bridge.js — M2 桥（预构建 skal ABI 阶段）
// JS → Rust：HTTP loopback（bun 原生 fetch → 127.0.0.1 loopback 微服务）
// Rust → JS：skal_evaluate 同步调用本文件安装的全局函数
//
// 配置由宿主在求值本文件前注入：
//   globalThis.__pi_config = { port: <loopback 端口>, dataDir: "..." }
(() => {
  const cfg = globalThis.__pi_config;
  if (!cfg || !cfg.port) {
    throw new Error("__pi_config missing (host must evaluate config before bridge)");
  }
  const base = `http://127.0.0.1:${cfg.port}`;

  // 同步风格 hostcall（返回 Promise；skal_evaluate 会等待 Promise 落定）
  globalThis.__pi_hostcall = async function __pi_hostcall(method, payload) {
    const res = await fetch(`${base}/hostcall`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ method, payload: payload ?? null }),
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
