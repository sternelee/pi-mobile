# pi-mobile

> 把 Pi Coding Agent 装进口袋 —— 手机本地运行的 AI 编程助手，带完整工具调用能力。

Tauri 2 + **QuickJS** + SolidJS 构建的移动端 Pi Coding Agent，支持 Android / iOS / 桌面三平台。
Agent 运行时（`@earendil-works/pi-agent-core`）在设备本地的 **QuickJS 引擎**内执行，不依赖远端服务器；
引擎由 `rquickjs` 静态编进 Rust 二进制（**不需要任何外部 .so**）。工具调用（read / write / edit /
ls / grep / mkdir / rm）由 Rust 实现（`pi-host-tools`），**进程内直接调用**，沙箱隔离 + 路径越狱防护。

## 运行时：QuickJS（D18）

| | 现在（QuickJS） | 曾经（嵌入式 bun） |
|---|---|---|
| 引擎 | `rquickjs`（QuickJS 纯 C 库，静态链进二进制） | libskal = zig 交叉编译的 bun + JavaScriptCore |
| 额外产物 | 无 | **92 MB** `libskal.so`（Android）/ `.dylib`（iOS） |
| 模型传输 | Rust 按 API 家族实现（见下） | pi-ai 的 JS provider 栈（4 家厂商 SDK） |
| Agent JS 体积 | 385 KB | 1.4 MB（+ node 内建垫片） |
| iOS | 不需要 WebKit / JSC / 关 JIT | 需从源码构建 WebKit（~13 GB 磁盘） |

换引擎的原因与代价写在 [docs/PROGRESS.md](docs/PROGRESS.md) 2026-09-19（第十轮）与 2026-09-20
（第十五轮，bun 运行时已从 main 删除）；**bun 路线完整保留在 `backup/bun` 分支**。
[skal 工艺与研究笔记](docs/LIBPI-BUN-NOTES.md) 作为历史存档保留。

## 当前状态

| 里程碑 | 状态 | 内容 |
|--------|------|------|
| **M0** 脚手架 | ✅ | Tauri 2 mobile 初始化、插件接线、CI、Android 真机跑通 |
| **M1** 嵌入式运行时 | ✅ → 📦 | 原为嵌入 bun 的 libskal（真机跑通）；**已换成 QuickJS（D18）**，bun 路线归档在 `backup/bun` |
| **M2** Agent 运行时 | ✅ | pi-agent-core 在 QuickJS 内启动；真机端到端 LLM 对话 + 工具调用 round-trip |
| **M3** 审批与产品化 | 🔨 | 工具审批（ask/auto/diff/回滚）、会话列表与自动恢复、文件树已落地；命令面板、用量可视化进行中 |
| **M4** MCP + Skills | ✅ | MCP streamable-http 双 transport、skills 注入、goal / todo / subagent / ask_user |
| **M5** iOS + 桌面 | 🔄 | 引擎侧三端已验证（含 iOS 模拟器真执行）；**换引擎后 App 的 iOS 包尚未真机复验** |

📖 详细规划与决策记录见 [docs/PLAN.md](docs/PLAN.md)。

## 架构

```
┌──────────────────────────────────────────────────────────┐
│                    SolidJS UI (WebView)                   │
│  聊天流 · 工具审批卡 · 会话列表 · 文件树 · 命令面板 · 设置    │
├───────────── Tauri IPC：39 命令 + pi-agent-event ──────────┤
│                    Rust 宿主 (src-tauri)                   │
│  凭证 · 审批策略 · 会话持久化 · 工具执行(pi-host-tools)      │
│  workspace jail · MCP · skills · goal · native             │
│  ┌────────────────────────────────────────────────────┐   │
│  │ qjs/：QuickJS guest（worker 线程）                  │   │
│  │   pi-agent-core Agent + 纯 JS 插件（385KB bundle）   │   │
│  │   host.* 直接调上面那些服务（进程内，无 HTTP 桥）      │   │
│  │   catalog.rs 目录 · openai_responses.rs 传输         │   │
│  └────────────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────────┘
```

- **JS↔Rust 桥**：guest 通过 `globalThis.host.*` **进程内**调用既有服务（审批走 `approval.rs`、
  提问走 `ask_user.rs`、工具走 `pi_host_tools`、会话 fs 走同一份 `sessions_fs`）。事件由
  pi-agent-core **原样转发**成 `pi-agent-event`，UI 认的就是那套形状。
- **执行权在 Rust**：工具执行的唯一入口要求该 `callId` 先完成审批握手 —— JS 忘了问、或被改写后
  故意不问，一律执行不了（审批分档与 diff 也在 Rust）。
- **模型传输在 Rust**（换引擎的真实代价）：按 `model.api` 分派家族 ——
  `openai-responses`（openai / xai / opencode…）与 `openai-completions`（目前仅 DeepSeek 的
  compat 档案）。未实现的家族会**明确报错**，不会静默换一家去发。
  provider / 模型目录是 pi-ai 的目录**当数据**搬进 `src-tauri/assets/models.json`，
  事件面与传输层共用同一份。
- **工具沙箱**：文件工具由 Rust 实现，jail 到 `app_data/workspace`，无 exec（D6）。

## 开发环境

### 前置依赖

- [Bun](https://bun.sh) 1.3+ — JS 工具链（构建 agent bundle、跑前端、Tauri CLI 入口）
- [Rust](https://rustup.rs) — stable，附 `aarch64-linux-android` target
- [Node.js](https://nodejs.org) 20+ — Tauri CLI 依赖
- **Android**：Android Studio + NDK r28 + platform-tools（adb）
- **iOS**（可选）：Xcode 15+（完整安装，需 iPhoneOS SDK）
- [biome](https://biomejs.dev) — 代码检查（已集成于 CI）

### 安装

```bash
git clone <repo-url> pi-mobile
cd pi-mobile
bun install
```

### 构建 agent bundle（任何 cargo 构建之前都要先跑）

```bash
bun run bundle:build          # = bash pi-bundle/build.sh
```

产物 `pi-bundle/dist/agent-qjs.js` 是 **gitignore 的**，而 `src-tauri/src/qjs/guest.rs` 用
`include_str!` 把它编进二进制 —— 文件不存在就编译不过。所以它被挂进 `tauri.conf.json` 的
`beforeDevCommand` / `beforeBuildCommand`（本地开发不必记这条纪律），CI 的各 job 里也显式跑一次。

### Android

```bash
# 直接构建 release APK（不需要任何前置下载 —— 引擎是静态链接的 QuickJS）
./scripts/android-build.sh --target aarch64
# → src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk（~40 MB）

# dev（自动编译 Rust → Gradle 打包 → 装到设备）
# ⚠️ 手机与电脑需同一 Wi-Fi（关闭 AP 隔离），tauri-cli 用 LAN IP 做 devUrl
TAURI_DEV_HOST=<your-lan-ip> bun tauri android dev
```

> **必须走 `scripts/android-build.sh`**，不能裸 `bun tauri android build`：依赖树里有 vendored
> OpenSSL，它调 `getentropy()`（API 28 才有），而 Tauri 传给 `cc` 的 API level 低于 minSdk。
> 脚本从 `build.gradle.kts` 读 minSdk 来选 NDK 包装器 —— 两者不一致时会「编译过、运行挂」。
> 同样地，**Android 上任何 cargo 命令都需要那组 NDK env**（脚本头部有四个 export）。

> **网络注意**：Honor/部分路由器默认开启 AP 隔离，导致手机 ping 不通电脑。需关闭 AP 隔离或改用手机热点。

### iOS

```bash
bun tauri ios build --debug
# → src-tauri/gen/apple/build/arm64/pi-mobile.ipa
```

引擎是 QuickJS，静态链接 —— **不需要构建 WebKit、不需要 libskal.dylib、也没有 JIT 要关**
（iOS 的 W^X 限制对纯解释器天然满足）。Xcode 工程与 `project.yml` 里那条 libskal embed
已随 bun 路线删除。

> **签名**：`project.yml` 的 `DEVELOPMENT_TEAM` 必须匹配 Xcode 里已登录的账号
> （查 `defaults read com.apple.dt.Xcode IDEProvisioningTeamByIdentifier`）。

> **真机排障**：iOS 上 `println!` 进统一日志，但 `devicectl` 不转 stdout；Android 上部分 ROM
> 会加密/丢弃 logcat。所以日志**同时写文件** `<data_dir>/pi-agent.log`，拉回：
> ```bash
> # iOS
> xcrun devicectl device copy from --device <UDID> \
>   --domain-type appDataContainer --domain-identifier com.sternelee.pi-mobile \
>   --source Library/Application\ Support/com.sternelee.pi-mobile/pi-agent.log --destination /tmp/pi-agent.log
> # Android（debug 包）
> adb shell run-as com.sternelee.pi_mobile cat files/pi-agent.log
> ```

### 桌面（开发调试宿主）

```bash
bun tauri dev            # 或 bun tauri build --no-bundle
```

## 测试

```bash
cd src-tauri && cargo test        # 单元测试
bash scripts/qjs-tests.sh         # qjs 集成测试（离线；一个进程一个，见脚本头注释）
QJS_LIVE=1 bash scripts/qjs-tests.sh   # 再加真 DeepSeek 一整轮（要 key）
```

> `cargo test` **不覆盖 qjs 的 boot 路径**：那几个集成测试都带 `#[ignore]`（`agent_init` 的
> HOST/WORKER 是 `OnceLock`，一个进程只能 boot 一次）。而 qjs 出过的两个 bug（启动白等 30 s、
> UI 永远停在 "agent booting…"）恰好只有它们能发现 —— 所以有 `scripts/qjs-tests.sh` 这条命令，
> CI 也显式跑它。

## 项目结构

```
pi-mobile/
├── src/                       # SolidJS 前端（聊天 UI · 审批 · 文件树 · 命令面板）
├── src-tauri/
│   ├── src/
│   │   ├── qjs/               # QuickJS 运行时：mod（worker/命令面）· guest（host.* 挂载）
│   │   │                      #   catalog.rs（provider/模型目录）· openai_responses.rs（传输）
│   │   │                      #   deepseek.rs（openai-completions 的 DeepSeek compat 档案）
│   │   ├── workspace.rs       # 宿主路径登记 + 越狱判定 + UI 侧文件操作
│   │   ├── logcat.rs          # 日志汇（文件 + 平台通道）
│   │   └── …                  # approval · sessions · creds · mcp · skills · goal · native · preview
│   ├── assets/models.json     # pi-ai 目录（数据；由 scripts/gen-models-catalog.py 生成）
│   ├── gen/android/           # Tauri Android 工程
│   └── gen/apple/             # Tauri iOS 工程
├── pi-bundle/
│   ├── agent-qjs.js           # Agent 入口（pi-agent-core + 纯 JS 插件 + host 桥工具）
│   ├── build.sh               # bun build --format=iife（classic script，两道 grep 把关）
│   └── dist/agent-qjs.js      # 构建产物（gitignored，include_str! 嵌入 Rust）
├── crates/pi-host-tools/      # 工具实现 + jail + 会话 fs + fetch（宿主无关，可复用）
├── spikes/quickjs-agent/      # 换引擎的 spike（QuickJS 路线从这里长出来）
├── scripts/
│   ├── android-build.sh       # NDK 工具链封装（Android 构建的唯一正确入口）
│   ├── qjs-tests.sh           # qjs 集成测试（一个进程一个）
│   └── gen-models-catalog.py  # 从 pi-ai 生成 assets/models.json
└── docs/
```

## 文档

| 文档 | 内容 |
|------|------|
| [docs/PLAN.md](docs/PLAN.md) | 架构设计、技术决策（D1–D18）、里程碑路线图 —— **含已作废的 D1 记录，现状见 D18** |
| [docs/PROGRESS.md](docs/PROGRESS.md) | 开发进度日志（倒序，含真机调试踩坑记录） |
| [docs/CONTRACTS.md](docs/CONTRACTS.md) | UI↔Rust IPC 契约（commands / events / 宿主通道） |
| [docs/LIBPI-BUN-NOTES.md](docs/LIBPI-BUN-NOTES.md) | 📦 历史存档：skal / JSC 工艺、构建链接、iOS 合规路径（bun 路线） |
| [docs/POCKET-PI-NOTES.md](docs/POCKET-PI-NOTES.md) | 📦 历史存档：pocket-pi / PocketJS 调研（换引擎决策的输入） |

## 致谢

- [earendil-works/pi](https://github.com/earendil-works/pi) — Pi Agent 核心（pi-ai 多 Provider LLM + pi-agent-core agent 运行时）
- [QuickJS](https://bellard.org/quickjs/) / [rquickjs](https://github.com/DelSkayn/rquickjs) — 嵌入式 JS 引擎
- [Tauri](https://tauri.app) — 跨平台原生 App 框架
- [skal-multiplatform/skal](https://github.com/skal-multiplatform/skal) — 📦 曾经的运行时工艺来源（见 LIBPI-BUN-NOTES）
