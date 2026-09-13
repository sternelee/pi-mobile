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
    return steps[key];
  }

  // 只读类：应直接成功（定位可能因未授权/无可用来源而返回可读错误 —— 那也是正确行为）
  await step("location", { highAccuracy: false });
  // 天气故意拆两步测：
  //   1) 传明确坐标 —— 验证 Open-Meteo 通路与格式化（不依赖定位）
  //   2) 不给坐标 —— 验证「走当前定位」的隐式依赖（定位挂了这步会跟着挂）
  // 国内 ROM 上这两步会分道扬镳（定位无 fix，但天气本身没问题），
  // 合成一步就看不出到底是哪个坏了。
  await step("weather", { latitude: 22.5431, longitude: 114.0579, days: 1 }, "weather_coords");
  await step("weather", { days: 1 }, "weather_via_location");
  // 日历：读（含权限流程）→ 写。
  // 写入用「一年后的凌晨」并只写一条，避免污染用户真实日程；验证完就留在
  // 那里由用户自行删除（探针不该顺手删用户数据，也不该假设自己有权删）。
  await step("calendar_list", { limit: 3 }, "calendar_list");
  const inAYear = Date.now() + 365 * 24 * 3600 * 1000;
  await step(
    "calendar_create",
    { title: "pi-mobile self-check", startMs: inAYear, endMs: inAYear + 3600_000 },
    "calendar_create",
  );

  // 通讯录：搜索（顺带验证权限流程）。
  // 只做 search 不做 get：get 需要真实 id，而设备上的联系人不可预期，
  // 「搜到就顺手 get 第一个」能在有数据时顺便覆盖 get 路径，没数据也不失败。
  const searchRes = await step("contacts", { op: "search", limit: 3 }, "contacts_search");
  // get 需要真实 id —— 从 search 的返回里取第一个。设备上可能一个联系人都
  // 没有（新机/未同步），那就跳过 get 而不是伪造 id 让这一步假失败。
  let firstId = null;
  try {
    firstId = JSON.parse(searchRes?.text ?? "{}")?.contacts?.[0]?.id ?? null;
  } catch {}
  if (firstId) {
    await step("contacts", { op: "get", id: firstId }, "contacts_get");
  } else {
    steps.contacts_get = { ok: true, ms: 0, text: "skipped: no contacts on device" };
    flush();
  }

  // 剪贴板：写入 → 读回，验证两个方向
  await step("clipboard", { op: "write", text: "pi-mobile self-check" }, "clipboard_write");
  await step("clipboard", { op: "read" }, "clipboard_read");
  // 通知：未授权时返回可读错误（预期行为，不是 bug）
  await step("notify", {
    title: "pi-mobile self-check",
    body: "native tools are wired up",
  });

  // 权限态查询（同步、不弹窗）：验证设置页显示的状态来源是真的在问系统。
  // 这一步是修 bug 加的 —— 早期 permission_state 硬编码 "unknown"，UI 于是
  // 永远显示 Allow，用户授权后看不到状态更新。
  try {
    const caps = await hostcall("native_capabilities", {});
    const cal = (caps.capabilities || []).find((c) => c.id === "calendar");
    steps.capabilities = {
      ok: !!cal,
      ms: 0,
      text: `calendar.permission=${cal ? cal.permission : "?"} platform=${caps.platform}`,
    };
  } catch (e) {
    steps.capabilities = { ok: false, ms: 0, error: String(e) };
  }
  flush();

  globalThis.__nativeprobe.state = "done";
})();

"started";
