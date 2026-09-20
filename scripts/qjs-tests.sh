#!/usr/bin/env bash
# qjs 路线的集成测试 —— 必须**一个进程跑一个**。
#
# 为什么单独一个脚本：`qjs::agent_init` 的 HOST/WORKER 是 `OnceLock`（一个进程只能
# boot 一次），所以这三个测试都标了 `#[ignore]`，被过滤在 `cargo test` 之外；
# 而 qjs 出过的两个 bug（boot 卡 30s、UI 永远停在 "agent booting…"）**恰好只能被
# 它们发现**。散在地上就等于没测 —— 这个脚本把「离线的那两个」变成一条命令。
#
# 用法：
#   bash scripts/qjs-tests.sh              # 离线两个（不要 key、不要网络）
#   QJS_LIVE=1 bash scripts/qjs-tests.sh   # 外加真 DeepSeek 一整轮（要
#                                          # PI_DEEPSEEK_API_KEY 或 DEEPSEEK_API_KEY）
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# 每个测试单独一次 cargo（不能加 --test-threads：问题不是一个进程内的并发，
# 而是同一个进程里第二次 agent_init 会拿到已经死掉的 worker sender）
run() {
  echo "===> $1"
  cargo test --manifest-path src-tauri/Cargo.toml --lib "$1" -- --ignored --nocapture
}

run qjs_globals_offline      # 目录/模型/命令面 + boot 事件（含 agent_ready）
run qjs_responses_mock_turn  # Responses 族的端到端（本地 mock SSE）

if [[ "${QJS_LIVE:-0}" == "1" ]]; then
  # 真模型那一轮：走真实网络。key 优先取 PI_DEEPSEEK_API_KEY，其次 DEEPSEEK_API_KEY
  export PI_DEEPSEEK_API_KEY="${PI_DEEPSEEK_API_KEY:-${DEEPSEEK_API_KEY:-}}"
  if [[ -z "${PI_DEEPSEEK_API_KEY}" ]]; then
    echo "error: QJS_LIVE=1 但 PI_DEEPSEEK_API_KEY / DEEPSEEK_API_KEY 都是空的" >&2
    exit 1
  fi
  run qjs_live_turn
fi

echo
echo "OK：qjs 集成测试全过（跳过 live 那一轮时用 QJS_LIVE=1 补上）"
