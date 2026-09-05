#!/usr/bin/env bash
# Build the pi agent bundle for the embedded bun runtime (libskal / libpi-bun).
#
# Device-verified constraints (docs/LIBPI-BUN-NOTES.md §6):
#   - format=cjs unusable: pi-ai subpath exports lack "require" conditions.
#   - format=esm works, but bun emits `var __require = import.meta.require`;
#     skal_evaluate runs source as a classic script where import.meta is a
#     SyntaxError. Patch it to a node-stdlib-browser-backed shim.
#   - Static imports only in the boot path: dynamic imports become pure
#     microtasks which skal only pumps during promise-awaiting evals.
#   - node-stdlib-browser must be imported FIRST in agent-main.js so its map
#     is ready before pi-agent-core's module body calls __require.
set -euo pipefail
cd "$(dirname "$0")/.."

bun build pi-bundle/agent-main.js \
  --target=bun --format=esm \
  --outfile pi-bundle/dist/agent.js

python3 - <<'PYEOF'
p = "pi-bundle/dist/agent.js"
s = open(p).read()

require_shim = r"""
var __require = typeof require === "function"
  ? require
  : (m) => {
      const stdlib = globalThis.__PI_NODE_STDLIB;
      const mod = stdlib?.[m] ?? stdlib?.["node:" + m];
      if (mod) return mod?.default ?? mod;
      if (m === "process") return globalThis.process;
      if (m === "buffer") return { Buffer: globalThis.Buffer, SlowBuffer: globalThis.Buffer };
      if (m === "crypto") return globalThis.crypto;
      if (m === "events") {
        class Emitter {
          constructor() { this._ee = new Map(); }
          on(n, f) { let l = this._ee.get(n); if (!l) { l = []; this._ee.set(n, l); } l.push(f); return this; }
          once(n, f) { const g = (...a) => { this.off(n, g); f(...a); }; return this.on(n, g); }
          off(n, f) { const l = this._ee.get(n); if (l) this._ee.set(n, l.filter((x) => x !== f)); return this; }
          removeListener(n, f) { return this.off(n, f); }
          removeAllListeners(n) { n ? this._ee.delete(n) : this._ee.clear(); return this; }
          emit(n, ...a) { const l = this._ee.get(n) ?? []; for (const f of [...l]) f(...a); return l.length > 0; }
          listeners(n) { return [...(this._ee.get(n) ?? [])]; }
          addListener(n, f) { return this.on(n, f); }
          setMaxListeners() { return this; }
        }
        return { EventEmitter: Emitter, default: { EventEmitter: Emitter } };
      }
      if (m === "util") {
        const u = { ...(stdlib?.util ?? {}), ...(stdlib?.util?.default ?? {}) };
        if (!u.promisify) u.promisify = (fn) => (...a) => new Promise((res, rej) => fn(...a, (e, v) => (e ? rej(e) : res(v))));
        if (!u.inspect) u.inspect = (v) => JSON.stringify(v, null, 2);
        if (!u.inherits) u.inherits = (a, b) => { Object.setPrototypeOf(a.prototype, b.prototype); };
        if (!u.deprecate) u.deprecate = (fn) => fn;
        if (!u.callbackify) u.callbackify = (fn) => (...a) => { const cb = a.pop(); fn(...a).then((v) => cb(null, v), cb); };
        if (!u.format) u.format = (f, ...a) => { let i = 0; return typeof f === "string" ? f.replace(/%[sdj]/g, (x) => (i < a.length ? (x === "%j" ? JSON.stringify(a[i++]) : String(a[i++])) : x)) : String(f); };
        if (!u.types) u.types = { isPromise: (v) => v instanceof Promise };
        if (!u.debuglog) u.debuglog = () => () => {};
        return u;
      }
      if (m === "module") return { createRequire: () => ({ resolve: () => "/pi-bundle/agent.js" }) };
      if (m === "path")
        return {
          join: (...p) => p.filter(Boolean).join("/"),
          resolve: (...p) => "/" + p.filter(Boolean).join("/"),
          dirname: (p) => p.split("/").slice(0, -1).join("/") || "/",
          basename: (p) => p.split("/").pop(),
          extname: (p) => { const b = p.split("/").pop() ?? ""; const i = b.lastIndexOf("."); return i > 0 ? b.slice(i) : ""; },
          sep: "/",
          isAbsolute: (p) => p.startsWith("/"),
          parse: (p) => { const i = p.lastIndexOf("/"); const base = p.slice(i + 1); const ei = base.lastIndexOf("."); return { root: "/", dir: p.slice(0, i) || "/", base, ext: ei > 0 ? base.slice(ei) : "", name: ei > 0 ? base.slice(0, ei) : base }; },
          format: (o) => o.dir + "/" + o.base,
          relative: () => "",
          posix: null,
        };
      {
        const thrower = (prop) => () => { throw new Error("node builtin '" + m + "." + String(prop) + "' unavailable in embedded runtime"); };
        const stub = new Proxy(function () {}, {
          get: (_t, prop) => {
            if (prop === "then" || prop === Symbol.toPrimitive) return undefined;
            if (prop === "default") return stub;
            return thrower(prop);
          },
          construct: () => { throw new Error("node builtin '" + m + "' unavailable in embedded runtime"); },
          apply: () => { throw new Error("node builtin '" + m + "' unavailable in embedded runtime"); },
        });
        return stub;
      }
    };
"""

s = s.replace("var __require = import.meta.require;", require_shim)
s = s.replace("import.meta.url", '"file:///pi-bundle/agent.js"')

# skal_evaluate runs source as a CLASSIC script: top-level static imports are
# SyntaxErrors on device (bun keeps node builtins/external ws as real imports
# for --target=bun, e.g. @google/genai's `import { createWriteStream } from
# "fs"`). Rewrite them to __require shim lookups; the shim's Proxy stub keeps
# unused provider paths (gemini) inert until actually called.
import re

def _import_to_var(m):
    clause, spec = m.group(1).strip(), m.group(2)
    req = f'__require("{spec}")'
    if clause.startswith("* as "):
        return f"var {clause[4:].strip()} = {req};"
    if clause.startswith("{"):
        return f"var {clause} = {req};"
    if "," in clause:  # default + named/namespace
        default, rest = clause.split(",", 1)
        return f"var {default.strip()} = {req}; var {rest.strip()} = {req};"
    return f"var {clause} = {req};"

s = re.sub(r'^import\s+([^"\']+?)\s+from\s+["\']([^"\']+)["\'];$', _import_to_var, s, flags=re.M)
s = re.sub(r'^import\s+["\']([^"\']+)["\'];$', r'__require("\1");', s, flags=re.M)

open(p, "w").write(s)
PYEOF

# Guard: no import.meta may survive (classic-script SyntaxError on device)
if grep -q 'import\.meta' pi-bundle/dist/agent.js; then
  echo "ERROR: import.meta survived the patch" >&2
  grep -n 'import\.meta' pi-bundle/dist/agent.js | head -5 >&2
  exit 1
fi

# Guard: no static import/export may survive (classic-script SyntaxError on device)
if grep -qE '^(import|export) ' pi-bundle/dist/agent.js; then
  echo "ERROR: static import/export survived the patch" >&2
  grep -nE '^(import|export) ' pi-bundle/dist/agent.js | head -5 >&2
  exit 1
fi

echo "OK pi-bundle/dist/agent.js ($(wc -c < pi-bundle/dist/agent.js) bytes)"
