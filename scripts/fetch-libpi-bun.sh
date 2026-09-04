#!/usr/bin/env bash
# fetch-libpi-bun.sh — M1 PoC 快速通道：下载 skal 官方预构建的 libskal
# （CI 产物，release-libskal workflow），装入 Android jniLibs。
#
# 从源码构建（我们自己的 pi_entry.zig + pi_bun_* ABI）走
# setup-bun-fork.sh / build-libpi-bun.sh（M1 后半，pins 见 manifest）。
#
# 用法: scripts/fetch-libpi-bun.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TAG="libskal-dev"
REPO="skal-multiplatform/skal"
DL="build/libpi-bun-prebuilt"
JNI="src-tauri/gen/android/app/src/main/jniLibs/arm64-v8a"

mkdir -p "$DL" "$JNI"

echo "→ downloading release '${TAG}' from ${REPO}"
for asset in libskal-android-arm64.so checksums-android.txt manifest.json; do
  if [[ ! -f "${DL}/${asset}" ]]; then
    echo "  ↓ ${asset}"
    curl -fsSL --retry 3 -C - -o "${DL}/${asset}" \
      "https://github.com/${REPO}/releases/download/${TAG}/${asset}"
  else
    echo "  ✓ ${asset} cached"
  fi
done

echo "→ verifying checksum"
(cd "$DL" && shasum -a 256 -c checksums-android.txt)

echo "→ pins (for from-source builds later):"
sed 's/^/    /' "$DL/manifest.json"

echo "→ installing into jniLibs"
cp "${DL}/libskal-android-arm64.so" "${JNI}/libskal.so"

echo "✓ libskal.so installed (dlopen'd by src-tauri/src/pi_bun/mod.rs as 'libskal.so')"
