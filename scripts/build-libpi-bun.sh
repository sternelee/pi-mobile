#!/usr/bin/env bash
# build-libpi-bun.sh — 从源码构建 libpi-bun（M1 第二段）
#
# 流水线（复刻 skal 工艺，见 docs/LIBPI-BUN-NOTES.md §2）：
#   0. 前置检查：vendor/ 已由 setup-bun-fork.sh 克隆并 pin
#   1. ICU for Android（~10 min）      → build/icu-android/install
#   2. JSC for Android（~30-60 min）   → build/jsc-android/install
#   3. bun android-release 交叉构建    → vendor/bun/build/android/libskal.so
#      （fork 的 build.zig 会把 src/skal_entry.zig 编进产物 —— 我们已在
#        setup-bun-fork.sh 用 patches/pi_entry.zig 覆盖该文件）
#   4. 符号守卫：对照 src-tauri/pi_bun/include/pi_bun.h 用 nm 检查
#      pibun_* 导出（缺导出是静默的 —— skal 教训）
#   5. 安装 → gen/android jniLibs/libpi_bun.so
#
# 用法: scripts/build-libpi-bun.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NDK="${ANDROID_NDK_ROOT:-$HOME/Library/Android/sdk/ndk/28.0.12433566}"
ABI_HEADER="${ROOT}/src-tauri/pi_bun/include/pi_bun.h"
JNI="${ROOT}/src-tauri/gen/android/app/src/main/jniLibs/arm64-v8a"

step() { echo -e "\n\033[1;34m===>\033[0m \033[1m$*\033[0m"; }

[[ -d "${ROOT}/vendor/bun/.git" ]] || { echo "error: vendor/bun missing — run scripts/setup-bun-fork.sh first" >&2; exit 1; }
[[ -f "${ABI_HEADER}" ]] || { echo "error: ${ABI_HEADER} missing" >&2; exit 1; }

# ── 1. ICU ───────────────────────────────────────────────────────────
step "1/4 ICU for Android"
ICU_LIB="${ROOT}/build/icu-android/install/lib/libicuuc.a"
if [[ -f "${ICU_LIB}" ]]; then
  echo "  ✓ already built"
else
  ANDROID_NDK_ROOT="${NDK}" bash "${ROOT}/patches/reference/build-icu-android.sh"
fi

# ── 2. JSC ───────────────────────────────────────────────────────────
step "2/4 JSC for Android"
JSC_LIB="${ROOT}/build/jsc-android/install/lib/libJavaScriptCore.a"
if [[ -f "${JSC_LIB}" ]]; then
  echo "  ✓ already built"
else
  ANDROID_NDK_ROOT="${NDK}" \
  WEBKIT_SRC="${ROOT}/vendor/WebKit" \
  ICU_INSTALL="${ROOT}/build/icu-android/install" \
    bash "${ROOT}/patches/reference/build-jsc-android.sh"
fi

# ── 3. bun android-release ───────────────────────────────────────────
step "3/4 bun cross-build (android-release, entry = pi_entry.zig)"
SO_OUT="${ROOT}/vendor/bun/build/android"
if ls "${SO_OUT}"/*.so >/dev/null 2>&1; then
  echo "  ✓ already built: $(ls "${SO_OUT}"/*.so | head -1)"
else
  ANDROID_NDK_ROOT="${NDK}" bun --cwd "${ROOT}/vendor/bun" scripts/build.ts \
    --profile=android-release --build-dir="${SO_OUT}"
fi
SO_SRC="$(ls "${SO_OUT}"/*.so | head -1)"

# ── 4. 符号守卫 ──────────────────────────────────────────────────────
step "4/4 symbol guard (pi_bun.h ↔ .so)"
expected="$(grep -oE '\bpibun_[a-z0-9_]+[[:space:]]*\(' "${ABI_HEADER}" | grep -oE 'pibun_[a-z0-9_]+' | sort -u)"
actual="$( { nm -D --defined-only "${SO_SRC}" 2>/dev/null || nm -g "${SO_SRC}" 2>/dev/null; } | grep -oE 'pibun_[a-z0-9_]+' | sort -u)"
missing="$(comm -23 <(echo "${expected}") <(echo "${actual}"))"
if [[ -n "${missing}" ]]; then
  echo "  ! MISSING exports (stale/failed entry build):" >&2
  echo "${missing}" | sed 's/^/      /' >&2
  exit 1
fi
echo "  ✓ all pibun_* exports present"

# ── 5. 安装 ──────────────────────────────────────────────────────────
step "install → jniLibs"
mkdir -p "${JNI}"
cp "${SO_SRC}" "${JNI}/libpi_bun.so"
python3 "${ROOT}/scripts/check-elf-align.py" "${JNI}/libpi_bun.so"

echo
echo "✓ libpi_bun.so installed。Rust 侧切换：pi_bun/mod.rs 把 dlopen 目标从"
echo "  'libskal.so' 改为 'libpi_bun.so' 并换 pi_bun_* ABI 绑定。"
