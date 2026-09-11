#!/usr/bin/env bash
# ios-device-run.sh — 把已构建的 iOS app 装到真机并带控制台启动。
#
# 前置：
#   1. bash scripts/build-jsc-ios.sh && bun … ios-release && bash scripts/link-skal-ios.sh
#   2. bun tauri ios build --debug  → src-tauri/gen/apple/build/arm64/pi-mobile.ipa
#   3. iPhone 用 USB 连接、解锁、信任本机（首次需在手机端点「信任」）
#
# 用法:
#   scripts/ios-device-run.sh                 # 自动挑第一台可用设备
#   scripts/ios-device-run.sh <UDID>          # 指定设备
#   scripts/ios-device-run.sh --install-only  # 只装不启动
#
# 说明：iOS 侧 pi_bun 的日志走 println!（stdout），所以用
# `devicectl device process launch --console` 直接吃到，无需 idevicesyslog。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
IPA="${ROOT}/src-tauri/gen/apple/build/arm64/pi-mobile.ipa"
BUNDLE_ID="com.sternelee.pi-mobile"

step() { echo -e "\n\033[1;34m===>\033[0m \033[1m$*\033[0m"; }

INSTALL_ONLY=0
DEVICE=""
for arg in "$@"; do
  case "$arg" in
    --install-only) INSTALL_ONLY=1 ;;
    -*) echo "unknown flag: $arg" >&2; exit 2 ;;
    *) DEVICE="$arg" ;;
  esac
done

[[ -f "${IPA}" ]] || { echo "error: ${IPA} 不存在 —— 先跑 bun tauri ios build --debug" >&2; exit 1; }

# ── 选设备 ─────────────────────────────────────────────────────────────
if [[ -z "${DEVICE}" ]]; then
  step "查找可用 iOS 设备"
  # devicectl 的输出是列对齐文本；先按状态过滤出 available 的行。
  DEVICE="$(xcrun devicectl list devices 2>/dev/null \
    | awk '/available/ && !/unavailable/ {print $3; exit}')"
  if [[ -z "${DEVICE}" ]]; then
    echo "error: 没有可用设备。请：" >&2
    echo "  1. 用 USB 线连接 iPhone" >&2
    echo "  2. 解锁手机并在弹窗点「信任此电脑」" >&2
    echo "  3. 确认已开启开发者模式（设置 → 隐私与安全性 → 开发者模式）" >&2
    echo >&2
    echo "当前设备列表：" >&2
    xcrun devicectl list devices 2>&1 | sed 's/^/  /' >&2
    exit 1
  fi
  echo "  → ${DEVICE}"
fi

step "安装 ${IPA##*/}"
xcrun devicectl device install app --device "${DEVICE}" "${IPA}"

if [[ ${INSTALL_ONLY} -eq 1 ]]; then
  echo "✓ 已安装（未启动）"
  exit 0
fi

step "启动并附着控制台（Ctrl-C 退出；应用日志前缀 [pi-bun]）"
exec xcrun devicectl device process launch \
  --device "${DEVICE}" \
  --console \
  --terminate-existing \
  "${BUNDLE_ID}"
