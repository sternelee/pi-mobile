#!/usr/bin/env bash
# link-skal-macos.sh — 把 bun 的 macOS 宿主对象链成可 dlopen 的 libskal.dylib。
#
# 手法与 scripts/link-skal-ios.sh 相同：从 build.ninja 抽出 `build bun-profile: link`
# 的**同一份输入**（含 prebuilt WebKit 的 JSC 静态库），只把最终动作从「链可执行
# 文件」换成「链 dynamiclib」——这样平台库、JSC、ICU 的路径全部沿用 bun 自己的
# 配置，不靠手写「差不多够」的列表。
#
# 前置：
#   cd vendor/bun && bun scripts/build.ts --profile=release \
#       --build-dir=build/host-release --configure-only && ninja -C build/host-release
#
# 产出：build/skal-macos-spike/libskal.dylib（宿主架构，未签名）
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUN_BUILD="${ROOT}/vendor/bun/build/host-release"
OUT_DIR="${ROOT}/build/skal-macos-spike"
mkdir -p "${OUT_DIR}"

[[ -f "${BUN_BUILD}/build.ninja" ]] || { echo "error: 未 configure：${BUN_BUILD}" >&2; exit 1; }

echo "===> 1/3 从 build.ninja 抽取 link 输入"
INPUTS_FILE="${OUT_DIR}/skal-link-inputs.rsp"
awk '
  /^build bun-profile: link / {
    capture = 1
    sub(/^build bun-profile: link /, "")
    sub(/[[:space:]]*\$$/, "")
    print
    next
  }
  capture {
    if ($0 ~ /^[[:space:]]+[a-zA-Z_][a-zA-Z_0-9]*[[:space:]]*=/) { capture = 0; next }
    sub(/^[[:space:]]+/, "")
    sub(/[[:space:]]*\$$/, "")
    print
  }
' "${BUN_BUILD}/build.ninja" \
  | tr ' ' '\n' \
  | awk '$0=="|"{stop=1;next} stop{next} $0~/^[[:space:]]*$/{next} $0=="$"{next} {print}' \
  > "${INPUTS_FILE}"
echo "    $(wc -l < "${INPUTS_FILE}" | tr -d ' ') 个输入"

echo "===> 2/3 导出符号表（4 个 skal_* + 脚本 runner）"
SYMS="${OUT_DIR}/skal-exports.txt"
cat > "${SYMS}" <<'EOF'
_skal_create_runtime
_skal_evaluate
_skal_free_string
_skal_runtime_was_reused
_pibun_run_script
EOF

echo "===> 3/3 链接 dynamiclib"
CXX="$(xcrun -f clang++)"
# macOS SDK sysroot：不加的话 `-licucore` / `-lresolv` 找不到（Xcode 平时自动加，
# 直接调 clang++ 得自己给）。iOS 版没这个问题是因为它在 LDFLAGS 里显式带了
# `-isysroot "$(xcrun --sdk iphoneos --show-sdk-path)"`。
SDK="$(xcrun --show-sdk-path)"
UNSTRIPPED="${OUT_DIR}/libskal.unstripped.dylib"
OUT="${OUT_DIR}/libskal.dylib"
cd "${BUN_BUILD}"
# 不加 -dead_strip：pibun_run_script 没有内部引用者，被 strip 掉就白搭；
# 宿主 dylib 体积大点无所谓（本地验证用）。
"${CXX}" "@${INPUTS_FILE}" \
  -dynamiclib \
  -isysroot "${SDK}" \
  -Wl,-install_name,@rpath/libskal.dylib \
  -Wl,-exported_symbols_list,"${SYMS}" \
  -Wl,-u,_skal_create_runtime \
  -Wl,-u,_skal_evaluate \
  -Wl,-u,_skal_free_string \
  -Wl,-u,_skal_runtime_was_reused \
  -Wl,-u,_pibun_run_script \
  -licucore -lresolv \
  -o "${OUT}"

echo "OK: ${OUT}"
ls -la "${OUT}"
file "${OUT}"
nm -gU "${OUT}" | grep -E "pibun_run_script|skal_" | head -12
