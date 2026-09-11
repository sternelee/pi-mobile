#!/usr/bin/env bash
# link-skal-ios.sh — Link bun's iOS-cross-compiled objects + JSC .a as
# libskal.dylib for real iOS device (aarch64-apple-ios).
#
# Adapted from skal-multiplatform/skal's scripts/link-skal-ios.sh for pi-mobile.
#
# Prerequisites:
#   1. bun ios-release configured + built (in vendor/bun):
#        cd vendor/bun
#        ln -sfn $PWD/../WebKit vendor/WebKit
#        PATH="$HOME/.cargo/bin:$PATH" bun scripts/build.ts \
#            --profile=ios-release --build-dir=build/ios-release --configure-only
#        PATH="$HOME/.cargo/bin:$PATH" ninja -C build/ios-release
#   2. JSC iOS build complete (scripts/build-jsc-ios.sh → build/skal-jsc-ios/lib/)
#
# Output: build/skal-ios-device/libskal.dylib (unsigned — Xcode signs at embed)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUN_DIR="${ROOT}/vendor/bun"
BUN_BUILD="${BUN_DIR}/build/ios-release"
SKAL_BUILD="${ROOT}/build/skal-ios-device"

step() { echo -e "\n\033[1;34m===>\033[0m \033[1m$*\033[0m"; }

# ── Toolchain discovery — Homebrew llvm@21 (matches bun's build) ──────
if ! command -v brew >/dev/null 2>&1; then
  echo "error: brew not found in PATH" >&2; exit 1
fi
LLVM_PREFIX="${LLVM_PREFIX:-$(brew --prefix llvm@21)}"
LLVM_BIN="${LLVM_PREFIX}/bin"
if [[ ! -x "${LLVM_BIN}/clang++" ]]; then
  echo "error: ${LLVM_BIN}/clang++ not found; run: brew install llvm@21" >&2
  exit 1
fi
CXX="${LLVM_BIN}/clang++"
SDK="$(xcrun --sdk iphoneos --show-sdk-path)"

mkdir -p "${SKAL_BUILD}"

if [[ ! -f "${BUN_BUILD}/build.ninja" ]]; then
  echo "error: bun iOS C++ build not configured at ${BUN_BUILD}" >&2
  echo "       run from vendor/bun (the fork's skal branch carries the iOS plumbing):" >&2
  echo "         ln -sfn \$PWD/../WebKit vendor/WebKit" >&2
  echo "         PATH=\"\$HOME/.cargo/bin:\$PATH\" bun scripts/build.ts \\" >&2
  echo "             --profile=ios-release \\" >&2
  echo "             --build-dir=build/ios-release \\" >&2
  echo "             --configure-only" >&2
  exit 1
fi

step "1/4 Extract link inputs from build.ninja"
# Capture the lines after `build bun-profile: link`, drop $-line-continuation
# tokens, stop at the first `key = value` variable binding (ldflags), drop the
# `|` implicit-input separator. The result is the exact same input set bun
# would have linked into bun-profile (which we don't produce on iOS — we want
# a dylib, not an executable).

INPUTS_FILE="${SKAL_BUILD}/skal-link-inputs.rsp"

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
  | awk '
      $0 == "|" { stop = 1; next }
      stop { next }
      $0 ~ /^[[:space:]]*$/ { next }
      $0 == "$" { next }
      { print }
    ' \
  > "${INPUTS_FILE}"

echo "$(wc -l < "${INPUTS_FILE}" | tr -d ' ') link inputs extracted from build.ninja"

step "2/4 Exported-symbols list (skal C ABI)"
# Only export the symbols our Rust host dlopens. pi-mobile currently uses 4
# (create_runtime / evaluate / free_string / runtime_was_reused) but we export
# the full skal surface for forward compatibility.
SKAL_SYMS="${SKAL_BUILD}/skal-exports.txt"
cat > "${SKAL_SYMS}" <<'EOF'
_skal_create_runtime
_skal_evaluate
_skal_free_string
_skal_runtime_was_reused
EOF

step "3/4 Link libskal.dylib (iOS device, arm64-apple-ios16.0)"
# -target arm64-apple-ios16.0 matches every input object's LC_BUILD_VERSION
# (bun's iOS C++ build was patched to emit this target via cfg.crossTarget).
# No vtool re-stamp needed (unlike the Simulator path).

LDFLAGS=(
  -dynamiclib
  -Wl,-ld_new
  -Wl,-no_compact_unwind
  -fno-keep-static-consts
  -target arm64-apple-ios16.0
  -isysroot "${SDK}"
  -dead_strip
  -dead_strip_dylibs
  -Wl,-install_name,@rpath/libskal.dylib
  -Wl,-exported_symbols_list,"${SKAL_SYMS}"
  # Anchor each C export so dead_strip doesn't drop them.
  -Wl,-u,_skal_create_runtime
  -Wl,-u,_skal_evaluate
  -Wl,-u,_skal_free_string
  -Wl,-u,_skal_runtime_was_reused
  -licucore
  -lresolv
)

UNSTRIPPED="${SKAL_BUILD}/libskal.unstripped.dylib"
OUT="${SKAL_BUILD}/libskal.dylib"
echo "linking ${UNSTRIPPED}"
cd "${BUN_BUILD}"
"${CXX}" "@${INPUTS_FILE}" "${LDFLAGS[@]}" -o "${UNSTRIPPED}"

step "4/4 Strip + verify"
xcrun strip -x -S -o "${OUT}" "${UNSTRIPPED}"

echo
echo "✓ libskal.dylib (iOS device, unsigned) produced:"
ls -la "${OUT}" "${UNSTRIPPED}"
file "${OUT}"
echo
echo "LC_BUILD_VERSION (must be IPHONEOS):"
xcrun vtool -show "${OUT}" | grep -A 5 "LC_BUILD_VERSION" || true
echo
echo "C ABI symbols (must all be present):"
"${LLVM_BIN}/llvm-nm" -gU "${OUT}" 2>/dev/null | grep -E "^[0-9a-f]+ T _skal_" || echo "(none — link probably failed)"
echo
echo "Next: embed libskal.dylib into the Tauri iOS app (gen/apple)"
echo "      Xcode's Embed Frameworks build phase will sign it automatically."
