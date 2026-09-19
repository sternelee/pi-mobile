// QuickJS guest 的 prelude —— 只补**实测缺失**的 Web API。
//
// 2026-09-19 用 rquickjs 0.12.2 实测（见 README 的探针输出）：
//   ✅ 已有：Promise / Proxy / Reflect / BigInt / queueMicrotask / performance /
//           Array.at / Object.hasOwn / String.replaceAll / Error.cause
//   ❌ 缺失：TextEncoder/TextDecoder / AbortController/AbortSignal / URL /
//           structuredClone / console / setTimeout / fetch / crypto / process
//
// 缺失清单里我们**不补 fetch** —— 那正是这条路线的取舍：网络在 Rust 侧，
// JS guest 里没有 HTTP 栈（`docs/POCKET-PI-NOTES.md` §1）。
// 定时器同样不补：循环由宿主驱动（Rust 调 tick + 泵微任务队列），
// guest 不自己等时钟。
//
// 写法参考 pocket-stack/pocket-pi 的 js/prelude.js（MIT）：同样按能力探测后
// 打补丁，因为不同 QuickJS 构建/版本带的 API 不一样。

(() => {
  if (typeof globalThis.console !== "object") {
    const line = (...args) =>
      globalThis.host?.log?.(args.map((a) => (typeof a === "string" ? a : String(a))).join(" "));
    globalThis.console = { log: line, info: line, warn: line, error: line, debug: line };
  }

  if (typeof globalThis.TextEncoder !== "function") {
    globalThis.TextEncoder = class {
      encode(input = "") {
        const text = unescape(encodeURIComponent(String(input)));
        const bytes = new Uint8Array(text.length);
        for (let i = 0; i < text.length; i += 1) bytes[i] = text.charCodeAt(i);
        return bytes;
      }
    };
  }

  if (typeof globalThis.TextDecoder !== "function") {
    globalThis.TextDecoder = class {
      decode(input) {
        if (!input) return "";
        const bytes = input instanceof Uint8Array ? input : new Uint8Array(input);
        let binary = "";
        for (let i = 0; i < bytes.length; i += 1) binary += String.fromCharCode(bytes[i]);
        return decodeURIComponent(escape(binary));
      }
    };
  }

  if (typeof globalThis.structuredClone !== "function") {
    globalThis.structuredClone = (value) =>
      value === undefined ? undefined : JSON.parse(JSON.stringify(value));
  }

  // AbortSignal：pi-agent-core 会传 signal 给 streamFn/tool.execute。我们没有
  // 真实取消（宿主没实现），但对象必须存在且 addEventListener 可用。
  if (typeof globalThis.AbortController !== "function") {
    class Signal {
      constructor() {
        this.aborted = false;
        this.reason = undefined;
        this.listeners = [];
      }
      addEventListener(type, listener) {
        if (type === "abort") this.listeners.push(listener);
      }
      removeEventListener(type, listener) {
        if (type === "abort") this.listeners = this.listeners.filter((l) => l !== listener);
      }
      throwIfAborted() {
        if (this.aborted) throw this.reason ?? new Error("aborted");
      }
    }
    globalThis.AbortController = class {
      constructor() {
        this.signal = new Signal();
      }
      abort(reason) {
        if (this.signal.aborted) return;
        this.signal.aborted = true;
        this.signal.reason = reason;
        for (const l of this.signal.listeners.slice()) l();
      }
    };
  }

  if (typeof globalThis.URL !== "function") {
    globalThis.URL = class {
      constructor(input, base = "") {
        const value = String(input);
        const root = String(base).replace(/[^/]*$/, "");
        this.href = /^[a-z][a-z0-9+.-]*:/i.test(value) ? value : root + value;
      }
      toString() {
        return this.href;
      }
      toJSON() {
        return this.href;
      }
    };
  }
})();
