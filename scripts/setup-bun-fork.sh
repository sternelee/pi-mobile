#!/usr/bin/env bash
# setup-bun-fork.sh — vendor bun fork + WebKit fork（M1 第二段：从源码构建 libpi-bun）
#
# 复刻 skal 的 setup.sh 工艺（docs/LIBPI-BUN-NOTES.md §2/§5）：
#   1. clone skal-multiplatform/bun @ skal 分支，checkout 到 pin commit
#   2. clone skal-multiplatform/WebKit @ skal 分支（Android/iOS JSC 源码）
#   3. 放置 src/pi_entry.zig（我们的 C ABI 入口，替代 skal_entry.zig）
#   4. vendor/bun 内 bun install（codegen 依赖）
#
# pins 来自 skal libskal-dev release manifest（docs/LIBPI-BUN-NOTES.md §5）——
# 预构建产物即由这些 commit 构建；从源码复刻必须对齐。
#
# 用法: scripts/setup-bun-fork.sh [--no-webkit]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VENDOR="${ROOT}/vendor"
PATCHES_DIR="${ROOT}/patches"

BUN_PIN="dfcbb2bc610ac55b9dcb08e10bf482d8d87373bd"
WEBKIT_PIN="c1bdd50c5ead2dca7f582013b74ec154ca7f5dfc"
FORK_URL="https://github.com/skal-multiplatform"

NO_WEBKIT=0
[[ "${1:-}" == "--no-webkit" ]] && NO_WEBKIT=1

# GitHub 直连对 WebKit（~1.6GiB pack）不可靠：实测 3 次均在 ~1GB 处
# `fetch-pack: unexpected disconnect` / `fatal: early EOF`（各耗时 ~1h）。
# gh-proxy.com 镜像 9 分 18 秒跑完（~3.5MiB/s）。GitHub tarball 路线不可行
# ——仓库超限，codeload 对 skal 分支/任意 commit 一律返回 422。
# 覆盖：GIT_MIRROR=https://gh-proxy.com  (空串 = 直连)
GIT_MIRROR="${GIT_MIRROR-https://gh-proxy.com}"

step() { echo -e "\n\033[1;34m===>\033[0m \033[1m$*\033[0m"; }
note() { echo "     $*"; }

# mirror_url <https-url> → 经 GIT_MIRROR 前缀转发（空则原样）。
mirror_url() {
  if [[ -z "${GIT_MIRROR}" ]]; then echo "$1"; else echo "${GIT_MIRROR}/$1"; fi
}

# clone <url> 的 <branch> 到 <dir>，并 checkout 到 <pin>。
# pin 才是构建真源（分支会移动；构建不可漂移）。
# --depth 1 必须保留：WebKit 无浅克隆时要拉 3M+ 个 tree 对象（38KiB/s，
# 永不结束）；blobless 变体在 checkout 阶段按 blob 逐个下载，同样不可用。
clone_pinned() {
  local url="$1" branch="$2" dir="$3" pin="$4"
  mkdir -p "${VENDOR}"
  if [[ ! -d "${dir}/.git" ]]; then
    echo "  clone ${url#@*/} @ ${branch} → ${dir#"${ROOT}"/}"
    echo "  via ${GIT_MIRROR:-直连}"
    git -c http.postBuffer=524288000 -c http.lowSpeedLimit=0 -c http.lowSpeedTime=999999 \
      clone --branch "${branch}" --depth 1 --single-branch \
      "$(mirror_url "${url}")" "${dir}"
  fi
  local current
  current="$(git -C "${dir}" rev-parse HEAD)"
  if [[ "${current}" != "${pin}" ]]; then
    echo "  pin drift: ${current:0:12} → ${pin:0:12}（fetch + checkout）"
    git -C "${dir}" fetch --depth 1 origin "${pin}"
    git -C "${dir}" checkout --detach "${pin}"
  else
    echo "  ✓ ${dir#"${ROOT}"/} @ pin ${pin:0:12}"
  fi
}

step "1/5 vendor/bun（skal bun fork @ skal 分支，pin ${BUN_PIN:0:12}）"
clone_pinned "${FORK_URL}/bun.git" skal "${VENDOR}/bun" "${BUN_PIN}"

if [[ ${NO_WEBKIT} -eq 0 ]]; then
  step "2/5 vendor/WebKit（JSC 源码，Android/iOS 构建需要，pin ${WEBKIT_PIN:0:12}）"
  clone_pinned "${FORK_URL}/WebKit.git" skal "${VENDOR}/WebKit" "${WEBKIT_PIN}"
  # bun 的 ios-release/android profile 用 webkit:"local"，nested cmake 从
  # webkitSrcDir() 找源码（默认 vendor/bun/vendor/WebKit）→ 必须放 symlink。
  mkdir -p "${VENDOR}/bun/vendor"
  ln -sfn "${VENDOR}/WebKit" "${VENDOR}/bun/vendor/WebKit"
  note "symlink vendor/bun/vendor/WebKit → vendor/WebKit"
else
  step "2/5 跳过 WebKit（--no-webkit）"
fi

step "3/5 覆盖 src/skal_entry.zig ← patches/pi_entry.zig（最小 diff：fork 的 build.zig 固定引用该文件名）"
if [[ -f "${PATCHES_DIR}/pi_entry.zig" ]]; then
  cp "${PATCHES_DIR}/pi_entry.zig" "${VENDOR}/bun/src/skal_entry.zig"
  note "installed pi_entry.zig as src/skal_entry.zig（实现 pi_bun_* ABI）"
else
  note "patches/pi_entry.zig 缺失 —— 保留 fork 自带 skal_entry.zig（不可用于 pi_bun ABI）" >&2
  exit 1
fi

step "4/5 应用 bun 源码补丁（patches/*.patch，git apply）"
shopt -s nullglob
for p in "${PATCHES_DIR}"/*.patch; do
  if git -C "${VENDOR}/bun" apply --check "${p}" 2>/dev/null; then
    git -C "${VENDOR}/bun" apply "${p}"
    note "applied $(basename "${p}")"
  elif git -C "${VENDOR}/bun" apply --reverse --check "${p}" 2>/dev/null; then
    note "already applied: $(basename "${p}")"
  else
    echo "  error: $(basename "${p}") 无法应用（上游漂移？）" >&2
    echo "         调查：git -C vendor/bun apply --check -v ${p}" >&2
    exit 1
  fi
done
shopt -u nullglob

step "5/5 vendor/bun 内 bun install（bun 自身 codegen 需要）"
(cd "${VENDOR}/bun" && bun install --silent)

echo
echo "✓ vendor 就绪。下一步: scripts/build-libpi-bun.sh（ICU → JSC → bun android-release）"
