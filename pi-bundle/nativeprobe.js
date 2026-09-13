// pi-bundle/nativeprobe.js — 系统原生能力自检（M6 真机排障用，debug 构建才跑）
//
// 为什么需要它：native 工具最终由模型触发，人工对话验证一轮要几分钟且不稳定
// （模型可能不调、可能调错）。这里直接打 `hostcall("native", …)` —— 与工具
// 完全相同的宿主通路 —— 一次拿到四个能力的真实结果，写进设备日志。
//
// 与 netprobe.js 同样的纪律：**不要从脚本返回 Promise**（skal_evaluate 的
// waitForPromise 会阻塞 VM worker 线程，fetch/插件回调都靠该线程 tick）。
// 立即返回 "started"，结果增量写 globalThis.__nativeprobe，宿主轮询。
globalThis.__nativeprobe = { state: "started", steps: {} };

(async () => {
  const steps = {};
  const flush = () => {
    globalThis.__nativeprobe.steps = steps;
  };

  async function step(name, args, label) {
    // label 缺省用工具名；同一工具多次调用必须给不同 label，
    // 否则后一次结果会覆盖前一次（clipboard read/write 踩过）。
    const key = label ?? name;
    const t0 = Date.now();
    try {
      const r = await hostcall("native", { name, args });
      steps[key] = r.error
        ? { ok: false, ms: Date.now() - t0, error: String(r.error) }
        : { ok: true, ms: Date.now() - t0, text: String(r.text ?? "").slice(0, 300) };
    } catch (e) {
      steps[key] = { ok: false, ms: Date.now() - t0, error: String(e) };
    }
    flush();
  }

  // 只读类：应直接成功（定位可能因未授权而返回可读错误 —— 那也是正确行为）
  await step("location", { highAccuracy: false });
  // 天气：不给坐标 → 走当前定位；定位不可用时应给出清晰错误
  await step("weather", { days: 1 });
  // 剪贴板：写入 → 读回，验证两个方向
  await step("clipboard", { op: "write", text: "pi-mobile self-check" }, "clipboard_write");
  await step("clipboard", { op: "read" }, "clipboard_read");
  // 通知：未授权时返回可读错误（预期行为，不是 bug）
  await step("notify", {
    title: "pi-mobile self-check",
    body: "native tools are wired up",
  });

  globalThis.__nativeprobe.state = "done";
})();

"started";
