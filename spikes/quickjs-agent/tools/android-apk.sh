#!/usr/bin/env bash
# android-apk.sh —— 把 spike 打成 **QuickJS 版** 的 release APK。
#
# ⚠️ 与 `scripts/android-build.sh`（打主 App，A 路线 = Tauri + bun）完全无关：
#   · A 路线 APK 需要 92MB 的 libskal.so（嵌入式 bun 运行时）；
#   · 本 APK **不需要 libskal** —— 引擎是 QuickJS，由 rquickjs 静态编进 spike 二进制；
#     整个 APK 只有一个小 Activity + 那一个二进制。
#
# 做法：spike 是 CLI，Android 上要让它能跑有两条约束：
#   1. 二进制必须以 `lib*.so` 的名字进 jniLibs，系统才会把它解包到
#      nativeLibraryDir —— **只有那里可执行**（Android 10+ 禁止从 data 目录 exec）；
#   2. AGP 默认 useLegacyPackaging=false（库不落盘、直接从 APK 映射），
#      所以要显式 `useLegacyPackaging = true` + `extractNativeLibs="true"`
#      （见 app/build.gradle.kts 与 AndroidManifest.xml 的注释）。
#
# 用法：
#     bash spikes/quickjs-agent/tools/android-apk.sh              # 构建 release APK
#     bash spikes/quickjs-agent/tools/android-apk.sh --install    # 顺带 adb install
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
ANDROID_DIR="${ROOT}/spikes/quickjs-agent/android"
TARGET=aarch64-linux-android
INSTALL=0
[[ "${1:-}" == "--install" ]] && INSTALL=1

echo "===> 1/3 编 spike 二进制（复用 tools/android-build.sh 的 NDK/bindgen 配方）"
bash "${ROOT}/spikes/quickjs-agent/tools/android-build.sh" > /dev/null
BIN="${ROOT}/spikes/quickjs-agent/target/${TARGET}/release/quickjs-agent-spike"
[[ -f "${BIN}" ]] || { echo "error: 没有产物 ${BIN}" >&2; exit 1; }

JNI="${ANDROID_DIR}/app/src/main/jniLibs/arm64-v8a"
mkdir -p "${JNI}"
# 名字必须是 lib*.so —— 这是让系统解包并给出可执行权限的关键
cp "${BIN}" "${JNI}/libquickjsagent.so"
echo "     → ${JNI}/libquickjsagent.so ($(du -h "${BIN}" | cut -f1))"

echo "===> 2/3 gradle assembleRelease（keystore 复用仓库里那份 keystore.properties）"
cd "${ANDROID_DIR}"
./gradlew --quiet assembleRelease

APK="${ANDROID_DIR}/app/build/outputs/apk/release/app-release.apk"
[[ -f "${APK}" ]] || { echo "error: 没找到 APK: ${APK}" >&2; exit 1; }
printf "     体积: %.1f MB\n" "$(echo "scale=2; $(stat -f%z "${APK}") / 1048576" | bc)"

echo "===> 3/3 校验 APK 里的东西"
unzip -l "${APK}" | grep -E "libquickjsagent|classes.dex" | sed 's/^/     /'

if [[ "${INSTALL}" == "1" ]]; then
  echo "===> adb install -r"
  adb install -r "${APK}"
  echo "     启动：adb shell am start -n dev.sterne.quickjsagent/.MainActivity"
fi

echo
echo "APK: ${APK}"
