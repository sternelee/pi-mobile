#!/usr/bin/env bash
# 把 js/entry.js 打成 QuickJS 能 eval 的单文件 classic script。
#
# 与 pi-bundle/build.sh（bun 路线）的区别：那边打 ESM 再打补丁把 import 改写掉，
# 因为 skal 的求值模式是 classic script；这里直接产 IIFE，源头就不带 import/
# export/import.meta —— QuickJS 不认识它们。末尾有一道硬校验。
set -euo pipefail
cd "$(dirname "$0")"

mkdir -p ../dist
bun build entry.js \
    --format=iife \
    --target=browser \
    --outfile ../dist/agent.js \
    --minify-whitespace

# 硬校验：IIFE 产物里不该再有任何模块语法（bare import / 顶层 export / import.meta）
if grep -nE '^[[:space:]]*(import|export)[[:space:](]' ../dist/agent.js; then
    echo "FAIL: bundle still contains top-level module syntax" >&2
    exit 1
fi
if grep -n 'import\.meta' ../dist/agent.js; then
    echo "FAIL: bundle still references import.meta" >&2
    exit 1
fi

SIZE=$(wc -c < ../dist/agent.js | tr -d ' ')
echo "OK spikes/quickjs-agent/dist/agent.js (${SIZE} bytes)"
