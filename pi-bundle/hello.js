// pi-bundle/hello.js — M1 PoC 负载
// 在 libpi-bun（预构建 libskal，skal ABI）内同步求值，探测嵌入式运行时的能力面。
// 结果是 JSON 字符串，由 Rust 侧透传给 UI 与 logcat。
(() => {
  const out = {
    engine: "libpi-bun (skal ABI prebuilt)",
    hasBunGlobal: typeof Bun !== "undefined",
    bunVersion: (typeof Bun !== "undefined" && Bun.version) || null,
    dataDir: globalThis.__skal_data_dir || null,
    hasFetch: typeof fetch === "function",
    hasTextEncoder: typeof TextEncoder === "function",
    compute: [21].map((x) => x * 2)[0],
    json: JSON.parse('{"ok":true}').ok,
  };
  return JSON.stringify(out);
})();
