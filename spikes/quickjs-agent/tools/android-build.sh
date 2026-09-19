#!/usr/bin/env bash
# android-build.sh —— 把 spike 交叉编译成可在 Android 真机上直接跑的二进制。
#
# 为什么能直接跑：spike 是个**普通 CLI**（不依赖 Tauri / APK / WebView）。
# `adb push` 到 /data/local/tmp 再 `adb shell` 执行即可 —— 这正是为了让你在真机上
# 验「QuickJS + pi-agent-core + Rust 工具链」这一层，而不必先把整个 App 装好。
#
# ⚠️ 为什么不是一个裸 cargo 命令就能搞定（三个坑，全在下面显式处理）：
#   1. 任何 cargo 命令都需要 NDK 的 CC/AR/RANLIB/LINKER（同 scripts/android-build.sh
#      的教训：Android 上连 cargo check 都会因为没有 CC 而失败）。
#   2. rquickjs-sys **没有 android 的预生成绑定**，只能开 bindgen 现场生成
#      （见 Cargo.toml 的 target 段）。bindgen 要 libclang，而 NDK 只带
#      libClangdXPCLib、不带 libclang → 用 homebrew llvm 的。
#   3. bindgen 自己**不传 --target**（读的是 rquickjs-sys build.rs），不给的话它按宿主
#      解析，报 `'stdio.h' file not found`（本机 PATH 里 NDK clang 排在 Apple clang
#      前面，正好把这个坑放大）。所以要显式给 --target + NDK sysroot。
#
# 用法：
#     bash spikes/quickjs-agent/tools/android-build.sh            # aarch64（默认）
#     TARGET_TRIPLE=x86_64-linux-android bash …/android-build.sh  # 模拟器
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
TARGET="${TARGET_TRIPLE:-aarch64-linux-android}"

# API level 与 app 的 minSdk 对齐（不另写一份，避免漂移带来假绿）
GRADLE="${ROOT}/src-tauri/gen/android/app/build.gradle.kts"
API="$(grep -oE 'minSdk[[:space:]]*=[[:space:]]*[0-9]+' "$GRADLE" 2>/dev/null | grep -oE '[0-9]+' | head -1)"
API="${API:-28}"

NDK="${ANDROID_NDK_ROOT:-${ANDROID_HOME:-$HOME/Library/Android/sdk}/ndk/28.0.12433566}"
[[ -d "${NDK}" ]] || { echo "error: NDK 不存在: ${NDK}（用 ANDROID_NDK_ROOT 指定）" >&2; exit 1; }
TC="$(ls -d "${NDK}"/toolchains/llvm/prebuilt/*/bin | head -1)"
SYSROOT="$(cd "${TC}/.." && pwd)/sysroot"

# ── 坑 1：NDK 工具链 ──────────────────────────────────────────────
U="${TARGET//-/_}"
export "CC_${U}=${TC}/${TARGET}${API}-clang"
export "AR_${U}=${TC}/llvm-ar"
export "RANLIB_${U}=${TC}/llvm-ranlib"
export "CARGO_TARGET_$(echo "${U}" | tr 'a-z' 'A-Z')_LINKER=${TC}/${TARGET}${API}-clang"

# ── 坑 2：bindgen 要 libclang，NDK 不带 ───────────────────────────
if [[ -z "${LIBCLANG_PATH:-}" ]]; then
  for candidate in /opt/homebrew/opt/llvm/lib /opt/homebrew/opt/llvm@21/lib /usr/local/opt/llvm/lib; do
    if compgen -G "${candidate}/libclang*.dylib" > /dev/null 2>&1 || compgen -G "${candidate}/libclang*.so*" > /dev/null 2>&1; then
      export LIBCLANG_PATH="${candidate}"
      break
    fi
  done
fi
[[ -n "${LIBCLANG_PATH:-}" ]] || {
  echo "error: 找不到 libclang（bindgen 需要）。装一个：brew install llvm" >&2
  exit 1
}

# ── 坑 3：bindgen 要显式 --target + sysroot ───────────────────────
# ⚠️ 变量名里是**下划线**：bindgen 把 TARGET 的 `-` 换成 `_` 再查这张表；而且 bash
# 的 `export` 也不接受带横线的名字（写成 aarch64-linux-android 会报
# `not a valid identifier`）。
export "BINDGEN_EXTRA_CLANG_ARGS_${U}"="--target=${TARGET}${API} --sysroot=${SYSROOT} -I${SYSROOT}/usr/include -I${SYSROOT}/usr/include/${TARGET}"

echo "===> spike android build"
echo "     target    ${TARGET} (API ${API})"
echo "     cc        ${TC}/${TARGET}${API}-clang"
echo "     libclang  ${LIBCLANG_PATH}"
echo "     sysroot   ${SYSROOT}"

cd "${ROOT}"
cargo build --release --target "${TARGET}" --manifest-path spikes/quickjs-agent/Cargo.toml "$@"

BIN="${ROOT}/spikes/quickjs-agent/target/${TARGET}/release/quickjs-agent-spike"
[[ -f "${BIN}" ]] || { echo "error: 产物不在预期路径: ${BIN}" >&2; exit 1; }

echo
echo "===> 产物"
ls -la "${BIN}" | awk '{printf "     %s  %s bytes\n", $NF, $5}'
file "${BIN}" | sed 's/^/     /'

echo
echo "===> 16KB 页对齐（Android 15+ 真机要求）"
python3 "${ROOT}/scripts/check-elf-align.py" "${BIN}" && echo "     对齐 OK" || {
  echo "     ⚠️ 对齐不满足，真机可能起不来。加 -Wl,-z,max-page-size=16384 重链。" >&2
  exit 1
}

cat <<EOF

===> 下一步（真机）
     bash spikes/quickjs-agent/tools/android-run.sh --prompt "…"

     或手工：
       adb push ${BIN} /data/local/tmp/quickjs-agent-spike
       adb shell chmod +x /data/local/tmp/quickjs-agent-spike
       adb shell "cd /data/local/tmp && ./quickjs-agent-spike --prompt '…'"
EOF
