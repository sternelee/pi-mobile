#!/usr/bin/env bash
# android-build.sh —— 带正确 NDK 工具链的 Android 构建封装（D16）
#
# 为什么要这个脚本：D16 起依赖树里有 **vendored OpenSSL**（libgit2 的 TLS 后端）。
# OpenSSL 3.x 的 providers/implementations/rands/seeding/rand_unix.c 直接调
# `getentropy()`，而它 **API 28 才引入**。
#
# 而 `bun tauri android build` 自己传给 `cc` 的 API level 低于本 app 的 minSdk
# （实测：minSdk 从 24 提到 28 后它仍然用低 API 包装器），于是 OpenSSL 编不过：
#     error: 'getentropy' is unavailable: introduced in Android 28
#
# ⚠️ 不要试图用「强行钉高 API」绕过：在 minSdk 仍是低版本时那样做会**编译通过但
#    运行期失败**。本脚本的 API level 必须与 app/build.gradle.kts 的 `minSdk`
#    保持一致（当前 28）——两者不一致时这里就是假绿。
#
# 用法（与 bun tauri android build 参数一致）：
#     ./scripts/android-build.sh --debug --target aarch64
#
# ⚠️ 不止 `tauri build`：**Android 上任何 cargo 命令都需要这组 env**
# （裸 `cargo check --target aarch64-linux-android` 也会失败 —— openssl-sys 的
# build script 要 CC）。CI 里同样要导出，否则会红在一个看起来与业务无关的地方。
# 需要单独跑 cargo 时，照抄本脚本下面那四个 export 即可。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GRADLE="${ROOT}/src-tauri/gen/android/app/build.gradle.kts"

# API level 从 minSdk 读出来，**不另写一份** —— 否则两边漂移就又变成假绿。
API="$(grep -oE 'minSdk[[:space:]]*=[[:space:]]*[0-9]+' "$GRADLE" | grep -oE '[0-9]+' | head -1)"
if [[ -z "${API}" ]]; then
  echo "error: 无法从 ${GRADLE} 读到 minSdk（脚本依赖它选 NDK 包装器）" >&2
  exit 1
fi

NDK="${ANDROID_NDK_ROOT:-${ANDROID_HOME:-$HOME/Library/Android/sdk}/ndk/28.0.12433566}"
if [[ ! -d "${NDK}" ]]; then
  echo "error: NDK 不存在: ${NDK}（用 ANDROID_NDK_ROOT 指定）" >&2
  exit 1
fi

# 宿主 tag 是 darwin-x86_64 / linux-x86_64 —— 取第一个存在的，不硬编码。
TC="$(ls -d "${NDK}"/toolchains/llvm/prebuilt/*/bin 2>/dev/null | head -1)"
if [[ -z "${TC}" ]]; then
  echo "error: 找不到 NDK 工具链目录: ${NDK}/toolchains/llvm/prebuilt/*/bin" >&2
  exit 1
fi

TARGET="${TARGET_TRIPLE:-aarch64-linux-android}"
CC="${TC}/${TARGET}${API}-clang"

for f in "${CC}" "${TC}/llvm-ar" "${TC}/llvm-ranlib"; do
  [[ -x "$f" ]] || { echo "error: 缺工具: $f" >&2; exit 1; }
done

# 变量名里的横线要变下划线（cc crate / openssl-sys 的约定）
U="${TARGET//-/_}"
export "CC_${U}=${CC}"
export "AR_${U}=${TC}/llvm-ar"
# NDK **没有**带目标前缀的 ranlib（只有无前缀的 llvm-ranlib），而 OpenSSL 的
# Makefile 按前缀名调用 —— 不显式给就会 `ranlib: command not found`。
export "RANLIB_${U}=${TC}/llvm-ranlib"
export "CARGO_TARGET_$(echo "${U}" | tr 'a-z' 'A-Z')_LINKER=${CC}"

echo "===> android build: minSdk=${API} target=${TARGET}"
echo "     CC=${CC}"
cd "${ROOT}"
exec bun tauri android build "$@"
