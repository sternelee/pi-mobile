#!/usr/bin/env bash
# ios-build.sh —— 把 spike 交叉编译到 iOS（真机 arm64 / 模拟器 arm64）。
#
# ⚠️ 与 A 路线（bun）的 iOS 构建**完全不是一回事**，别被名字带跑：
#   · A 路线要在 iOS 上跑 **JavaScriptCore**，而 JSC 是 WebKit 的一部分 →
#     必须 clone WebKit 源码（~8GB shallow）+ 从源码编 JSC + 处理「无 JIT」的
#     setenv 时序（见 README 的 iOS 段与 scripts/build-jsc-ios.sh）；
#   · B 路线跑 **QuickJS** —— 一个独立的纯 C 库（4 个 .c：quickjs.c / libregexp.c /
#     libunicode.c / dtoa.c，由 rquickjs-sys 自带）。**不需要 WebKit、不需要 JSC**，
#     也没有 JIT 要关（QuickJS 是纯解释器，iOS 的 W^X 限制天然满足）。
#   所以这个脚本做的事只有：用 iPhoneOS SDK 的 clang 编那 4 个 C 文件 + Rust 侧。
#
# 用法：
#     bash spikes/quickjs-agent/tools/ios-build.sh              # 真机 arm64
#     TARGET_TRIPLE=aarch64-apple-ios-sim bash …/ios-build.sh   # 模拟器 arm64
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
TARGET="${TARGET_TRIPLE:-aarch64-apple-ios}"

case "${TARGET}" in
  aarch64-apple-ios) SDK=iphoneos; MIN=16.0 ;;
  aarch64-apple-ios-sim) SDK=iphonesimulator; MIN=16.0 ;;
  *) echo "error: 不支持的 TARGET_TRIPLE=${TARGET}（用 aarch64-apple-ios / aarch64-apple-ios-sim）" >&2; exit 1 ;;
esac

command -v xcrun >/dev/null || { echo "error: 需要 Xcode 命令行工具" >&2; exit 1; }
SYSROOT="$(xcrun --sdk "${SDK}" --show-sdk-path)"
[[ -d "${SYSROOT}" ]] || { echo "error: 找不到 ${SDK} SDK（Xcode 装全了吗）" >&2; exit 1; }

U="${TARGET//-/_}"
export "CC_${U}=$(xcrun --sdk "${SDK}" -f clang)"
export "AR_${U}=$(xcrun --sdk "${SDK}" -f ar)"

# bindgen 要 libclang；Xcode 不带可用的 libclang（与 Android 那边 NDK 的情况同型）
if [[ -z "${LIBCLANG_PATH:-}" ]]; then
  for candidate in /opt/homebrew/opt/llvm/lib /opt/homebrew/opt/llvm@21/lib /usr/local/opt/llvm/lib; do
    if compgen -G "${candidate}/libclang*.dylib" > /dev/null 2>&1; then
      export LIBCLANG_PATH="${candidate}"
      break
    fi
  done
fi
[[ -n "${LIBCLANG_PATH:-}" ]] || { echo "error: 找不到 libclang（bindgen 需要）：brew install llvm" >&2; exit 1; }

# 变量名是**下划线**形式（bindgen 把 TARGET 的 '-' 换成 '_' 再查表）
export "BINDGEN_EXTRA_CLANG_ARGS_${U}=--target=arm64-apple-ios${MIN} -isysroot ${SYSROOT}"

# ── 唯一一处 iOS 专属适配：补 compiler-rt 的栈探测符号 ──────────────────
# Apple 的 clang 对**大栈帧**函数会生成 `___chkstk_darwin` 调用（quickjs.c 里那些
# 大 switch / 深递归就有），该符号由 compiler-rt 提供。clang 驱动链接时会自动带上，
# 而 rustc 直接调 ld → 报单条 `Undefined symbols: ___chkstk_darwin`。
# 显式把那个静态库加进链接参数即可（本次实测就是这一条把 iOS 构建卡住的）。
CLANG_RES="$("$(xcrun --sdk "${SDK}" -f clang)" -print-resource-dir)"
case "${SDK}" in
  iphoneos) RT=libclang_rt.ios.a ;;
  iphonesimulator) RT=libclang_rt.iossim.a ;;
esac
if [[ -f "${CLANG_RES}/lib/darwin/${RT}" ]]; then
  export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=${CLANG_RES}/lib/darwin/${RT}"
  echo "     compiler-rt ${RT}（补 ___chkstk_darwin）"
else
  echo "warning: 没找到 ${CLANG_RES}/lib/darwin/${RT} —— 链接可能报 ___chkstk_darwin 未定义" >&2
fi

echo "===> spike ios build"
echo "     target   ${TARGET}"
echo "     sdk      ${SDK} ${MIN}  (${SYSROOT})"
echo "     libclang ${LIBCLANG_PATH}"
echo "     引擎     QuickJS（纯 C，与 WebKit/JSC 无关）"

cd "${ROOT}"
rustup target add "${TARGET}" >/dev/null 2>&1 || true
cargo build --release --target "${TARGET}" --manifest-path spikes/quickjs-agent/Cargo.toml "$@"

BIN="${ROOT}/spikes/quickjs-agent/target/${TARGET}/release/quickjs-agent-spike"
[[ -f "${BIN}" ]] || { echo "error: 产物不在预期路径: ${BIN}" >&2; exit 1; }
echo
echo "===> 产物"
ls -la "${BIN}" | awk '{printf "     %s  %s bytes\n", $NF, $5}'
file "${BIN}" | sed 's/^/     /'
