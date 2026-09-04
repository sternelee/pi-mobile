// pi-bundle/smoke2.js — M2 桥冒烟（预构建 skal ABI 阶段）
//
// 重要：不要从本脚本返回 Promise —— skal_evaluate 的 waitForPromise 会
// 阻塞 VM worker 线程，而 fetch 的 I/O 完成恰需要该线程 tick 事件循环
// （实测挂死）。改为：立即返回 "started"，异步结果写全局，宿主轮询。
globalThis.__smoke2 = { state: "started" };

(async () => {
  const t0 = Date.now();
  try {
    const res = await fetch(
      `http://127.0.0.1:${globalThis.__pi_config.port}/hostcall`,
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ method: "ping", payload: { from: "bun" } }),
        signal: AbortSignal.timeout(5000),
      },
    );
    const body = await res.json();
    globalThis.__smoke2 = {
      state: "done",
      status: res.status,
      body,
      roundtripMs: Date.now() - t0,
    };
  } catch (e) {
    globalThis.__smoke2 = { state: "error", error: String(e) };
  }
})();

"started";
