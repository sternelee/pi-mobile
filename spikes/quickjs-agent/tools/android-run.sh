#!/usr/bin/env bash
# android-run.sh —— 把 spike 推到 Android 真机上跑。
#
# 为什么值得这么跑：spike 是普通 CLI，不需要 APK / Tauri / WebView。在真机上直接
# 执行它，验证的是**这一层是否成立** —— QuickJS 引擎能起来、pi-agent-core 能跑、
# Rust 工具与会话 fs 在 Android 上能写盘、审批往返不卡、以及网络/证书能不能出去。
# 也就是说：在动整个 App 集成之前，先确认这条路线在真机上不塌。
#
# 两种模型来源：
#   1. 真 DeepSeek   —— 需要 DEEPSEEK_API_KEY（本脚本从环境或 --key-file 读）
#   2. 宿主的 mock    —— 不出网、不花钱。设备经 LAN 连你电脑（需同一 Wi-Fi，
#                        且 mock 绑 0.0.0.0：tools/mock-deepseek.py 8899 0.0.0.0）
#
# 用法：
#   bash spikes/quickjs-agent/tools/android-run.sh --prompt "…"            # 真模型
#   MOCK=1 bash spikes/quickjs-agent/tools/android-run.sh --prompt "…"     # 走宿主 mock
#
# 其余参数原样透传给 spike（--yes / --deny / --resume / --goal / --delay-approval …）。
# 不带 --yes/--deny 时是本目录的默认交互审批：`adb shell` 有 pty，可以直接敲 y/n/a/d。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
BIN="${ROOT}/spikes/quickjs-agent/target/aarch64-linux-android/release/quickjs-agent-spike"
REMOTE_DIR="/data/local/tmp/pi-spike"
REMOTE_BIN="/data/local/tmp/quickjs-agent-spike"

[[ -f "${BIN}" ]] || {
  echo "error: 还没有 Android 产物。先跑：" >&2
  echo "       bash spikes/quickjs-agent/tools/android-build.sh" >&2
  exit 1
}

command -v adb >/dev/null || { echo "error: 找不到 adb（装 platform-tools）" >&2; exit 1; }
DEVICES="$(adb devices | awk 'NR>1 && $2=="device" {print $1}')"
[[ -n "${DEVICES}" ]] || { echo "error: 没有已授权设备（adb devices 看看）" >&2; exit 1; }
echo "===> 设备: $(echo "${DEVICES}" | tr '\n' ' ')  ($(adb shell getprop ro.build.version.release 2>/dev/null | tr -d '\r') / $(adb shell getprop ro.product.cpu.abi 2>/dev/null | tr -d '\r'))"

# ── 模型来源 ────────────────────────────────────────────────────────────
ENV_FILE="$(mktemp)"
trap 'rm -f "${ENV_FILE}"' EXIT

if [[ -n "${MOCK:-}" ]]; then
  # 宿主 LAN IP：设备要用它连回这台机器
  LAN_IP="$(ipconfig getifaddr en0 2>/dev/null || ipconfig getifaddr en1 2>/dev/null || true)"
  [[ -n "${LAN_IP}" ]] || { echo "error: 取不到 LAN IP（en0/en1）" >&2; exit 1; }
  PORT="${MOCK_PORT:-8899}"
  echo "===> 模型: 宿主 mock http://${LAN_IP}:${PORT}"
  echo "     ⚠️ 另开一个终端跑：python3 spikes/quickjs-agent/tools/mock-deepseek.py ${PORT} 0.0.0.0"
  {
    echo "export DEEPSEEK_API_KEY=mock"
    echo "export DEEPSEEK_BASE_URL=http://${LAN_IP}:${PORT}"
  } > "${ENV_FILE}"
else
  # 真 key：环境里没有就从 ~/.zshrc 读（非交互 shell 不 source 它，这是本机实情）
  KEY="${DEEPSEEK_API_KEY:-}"
  if [[ -z "${KEY}" && -f "${HOME}/.zshrc" ]]; then
    KEY="$(grep -E '^[[:space:]]*(export[[:space:]]+)?DEEPSEEK_API_KEY=' "${HOME}/.zshrc" | tail -1 | sed -E 's/^[^=]*=//; s/^["'"'"']//; s/["'"'"']$//')"
  fi
  [[ -n "${KEY}" ]] || { echo "error: 没有 DEEPSEEK_API_KEY（或 MOCK=1 走宿主 mock）" >&2; exit 1; }
  echo "===> 模型: 真 DeepSeek（key ${#KEY} 字符）"
  # 走 env 文件而不是命令行参数：命令行会在设备 ps 里短暂可见
  echo "export DEEPSEEK_API_KEY=${KEY}" > "${ENV_FILE}"
fi

# ── 推送 ────────────────────────────────────────────────────────────────
echo "===> 推送二进制（$(du -h "${BIN}" | cut -f1)）"
adb push "${BIN}" "${REMOTE_BIN}" > /dev/null
adb shell "chmod 755 ${REMOTE_BIN}"
adb push "${ENV_FILE}" "${REMOTE_DIR}/env.sh" > /dev/null 2>&1 || {
  adb shell "mkdir -p ${REMOTE_DIR}"
  adb push "${ENV_FILE}" "${REMOTE_DIR}/env.sh" > /dev/null
}
adb shell "chmod 600 ${REMOTE_DIR}/env.sh"

# ── 先自检（真机上最想知道「是网不通还是引擎没起来」）────────────────────
# 只花约 300ms（DNS + 一次 GET /models + 引擎自检），失败就直接停在这里，
# 不会让你对着一个转圈的 agent 猜。SKIP_NETCHECK=1 可跳过。
if [[ -z "${SKIP_NETCHECK:-}" ]]; then
  echo "===> net-check（先分层确认网络与引擎）"
  adb shell "cd /data/local/tmp && . ${REMOTE_DIR}/env.sh && ${REMOTE_BIN} --net-check" || {
    echo >&2
    echo "error: net-check 没过 —— 按上面 dns/tls/engine 哪一行失败定位（SKIP_NETCHECK=1 可跳过）" >&2
    exit 1
  }
  echo
fi

# ── 跑 ──────────────────────────────────────────────────────────────────
# workspace / data 都放在设备上 spike 自己的目录里（默认值是仓库相对路径，真机没有）
ARGS=("$@")
HAS_WORKSPACE=0
for a in "${ARGS[@]}"; do [[ "$a" == "--workspace" || "$a" == "--data-dir" ]] && HAS_WORKSPACE=1; done
if [[ "${HAS_WORKSPACE}" == "0" ]]; then
  ARGS+=(--workspace "${REMOTE_DIR}/workspace" --data-dir "${REMOTE_DIR}/data")
fi

QUOTED=""
for a in "${ARGS[@]}"; do QUOTED+=" $(printf '%q' "$a")"; done

echo "===> 在设备上执行"
echo "     ${REMOTE_BIN}${QUOTED}"
echo
# adb shell 有 pty：不传 --yes/--deny 时审批可以直接在终端里敲 y/n/a/d
adb shell "cd /data/local/tmp && . ${REMOTE_DIR}/env.sh && ${REMOTE_BIN}${QUOTED}"
STATUS=$?

echo
echo "===> 设备侧留下的东西（下一轮 --resume 会用到）"
adb shell "ls -la ${REMOTE_DIR}/data/sessions/*/ 2>/dev/null | tail -3" || true
echo "     二进制 ${REMOTE_BIN}；workspace/data 在 ${REMOTE_DIR}/"
echo "     清理：adb shell rm -rf ${REMOTE_DIR} ${REMOTE_BIN}"
exit ${STATUS}
