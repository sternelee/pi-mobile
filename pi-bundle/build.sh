#!/usr/bin/env bash
# 构建 agent bundle（QuickJS 路线 —— src-tauri/src/qjs/ 用 include_str! 编进 Rust）。
#
# 为什么必须是 classic script：QuickJS 没有模块系统，rquickjs 用 `ctx.eval` 求值。
# 所以用 `--format=iife`，并在下面两道 grep 上把关（残留 import/export 或
# import.meta 都是运行期 SyntaxError，而不是构建期错误 —— 必须在这里拦住）。
#
# 产物 pi-bundle/dist/agent-qjs.js 是 .gitignore 的（dist/），**任何 cargo 构建之前
# 都要先跑这个脚本**：guest.rs 里是 `include_str!("../../../pi-bundle/dist/agent-qjs.js")`，
# 文件不存在就编译不过。CI 里同样（.github/workflows/ci.yml 的 bundle step）。
#
# 历史：曾经还有一条 bun 路线（agent-main.js → dist/agent.js，需要 node-stdlib-browser
# 垫片 + import.meta 改写）。它已随 backup/bun 分支归档并从 main 删除（见
# docs/PROGRESS.md 第十五轮），所以这里只剩 QuickJS 一条。
set -euo pipefail
cd "$(dirname "$0")/.."

# ── agent bundle 本体 ────────────────────────────────────────────────
# agent-qjs.js 的 import 图里没有 node 内建（只有 pi-agent-core 的 agent/session 与
# pi-ai 的 event-stream），所以 `--format=iife` 直接就是 classic script。
bun build pi-bundle/agent-qjs.js \
  --format=iife --target=browser \
  --outfile pi-bundle/dist/agent-qjs.js --minify-whitespace

if grep -qE '^[[:space:]]*(import|export)[[:space:](]' pi-bundle/dist/agent-qjs.js; then
  echo "ERROR: agent-qjs.js 里残留模块语法（QuickJS 只吃 classic script）" >&2
  grep -nE '^[[:space:]]*(import|export)[[:space:](]' pi-bundle/dist/agent-qjs.js | head -5 >&2
  exit 1
fi
if grep -q 'import\.meta' pi-bundle/dist/agent-qjs.js; then
  echo "ERROR: agent-qjs.js 里残留 import.meta" >&2
  exit 1
fi
echo "OK pi-bundle/dist/agent-qjs.js ($(wc -c < pi-bundle/dist/agent-qjs.js) bytes)"
