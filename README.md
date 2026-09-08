# pi-mobile

> 把 Pi Coding Agent 装进口袋 —— 手机本地运行的 AI 编程助手，带完整工具调用能力。

Tauri 2 + 嵌入式 Bun (JavaScriptCore) + SolidJS 构建的移动端 Pi Coding Agent，支持 Android / iOS / 桌面三平台。Agent 运行时（`@earendil-works/pi-agent-core` + `pi-ai`）在设备本地的嵌入式 bun 内执行，不依赖远端服务器；工具调用（read / write / ls / grep）经 Rust 宿主 loopback 桥执行，沙箱隔离 + 路径越狱防护。

## 当前状态

| 里程碑 | 状态 | 内容 |
|--------|------|------|
| **M0** 脚手架 | ✅ | Tauri 2 mobile 初始化、插件接线、CI、Android 真机跑通 |
| **M1** 嵌入式 Bun | ✅ | 预构建 libskal（bun 1.3.14 + JSC）装入 jniLibs，真机验证完整 JS 执行链 |
| **M2** Agent Bundle | ✅ | pi-agent-core 在嵌入式 bun 内 headless 启动；真机端到端 LLM 对话 + 工具调用 round-trip 验证通过 |
| **M3** 审批与产品化 | 🔨 | 工具审批（ask/auto/diff/回滚）、会话列表、文件树预览已落地；命令面板、用量可视化进行中 |
| **M4** MCP + Skills | 📋 | MCP streamable-http 接入、pi-subagents/pi-goal/pi-ask-user 插件能力层、Skills 安装器 |
| **M5** iOS + 桌面 | 📋 | iOS `tauri ios init` 已完成；真机需从源码构建 WebKit JSC（~7GB 磁盘 + 2h 编译） |

📖 **详细规划见 [docs/PLAN.md](docs/PLAN.md)** —— 架构、设计决策、里程碑路线图。

## 架构

```
┌─────────────────────────────────────────────────────────┐
│                      SolidJS UI (WebView)                │
│   聊天流 · 工具审批 · 会话列表 · 文件树 · 命令面板         │
├────────────────── Tauri IPC ─────────────────────────────┤
│                    Rust 宿主 (src-tauri)                  │
│  loopback HTTP · 工具执行 · 凭证 · 会话持久化 · 审批策略   │
├─────────────────────────────────────────────────────────┤
│              libpi-bun (嵌入式 bun + JSC)                 │
│   pi-agent-core Agent · pi-ai streamSimple · host 桥工具  │
└─────────────────────────────────────────────────────────┘
```

- **Agent 运行时**：`libpi-bun`（skal 工艺：zig 交叉编译 bun 为平台原生库），Android 用 `.so`（JNI dlopen），iOS 用 `.a`（静态链接）
- **JS↔Rust 桥**：bun 原生 fetch → loopback HTTP（127.0.0.1 随机端口）→ Rust hostcall 分发；Rust→JS 经 `skal_evaluate` 事件注入
- **工具沙箱**：read / write / ls / grep 由 Rust 实现，路径越狱防护（jail 到 `app_data/workspace`），无 exec（D6 安全决策）
- **LLM Provider**：经 `@earendil-works/pi-ai` 统一分发，支持 Anthropic / OpenAI / DeepSeek 等 OpenAI-compatible API

## 开发环境

### 前置依赖

- [Bun](https://bun.sh) 1.3+ — JS 工具链 & 运行时
- [Rust](https://rustup.rs) — stable，附 `aarch64-linux-android` target
- [Node.js](https://nodejs.org) 20+ — Tauri CLI 依赖
- **Android**：Android Studio + NDK r28+ + platform-tools（adb）
- **iOS**（可选）：Xcode 15+（完整安装，需 iPhoneOS SDK）
- [biome](https://biomejs.dev) — 代码检查（已集成于 CI）

### 安装

```bash
git clone <repo-url> pi-mobile
cd pi-mobile
bun install
```

### Android 真机开发

```bash
# 1. 下载预构建 libskal（嵌入式 bun 运行时，~92MB）
bun run scripts/fetch-libpi-bun.sh

# 2. 构建 agent bundle
bash pi-bundle/build.sh

# 3. 启动 dev（自动编译 Rust → Gradle 构建 APK → 安装到设备）
#    ⚠️ 手机与电脑需同一 Wi-Fi（关闭 AP 隔离），tauri-cli 用 LAN IP 做 devUrl
TAURI_DEV_HOST=<your-lan-ip> bun tauri android dev
```

> **网络注意**：Honor/部分路由器默认开启 AP 隔离，导致手机 ping 不通电脑。需关闭 AP 隔离或改用手机热点。打包 APK 模式（`bun tauri android build --debug`）无此依赖但 devUrl 仍会烘焙 LAN IP。

### iOS 开发

```bash
# 模拟器（预构建 libskal-iossim，~63MB）
# TODO: scripts/fetch-libpi-bun-ios.sh

# 真机（需从源码构建 WebKit JSC）
# 1. 克隆 WebKit fork（~2GB shallow clone）
bun run scripts/setup-bun-fork.sh
# 2. 构建 JSC for iOS（~2h，需完整 Xcode）
bash scripts/build-jsc-ios.sh  # TODO
# 3. tauri ios build
bun tauri ios build
```

> **iOS 真机空间需求**：WebKit 源码 ~2GB + 构建目录 ~3-5GB = 约 7GB 额外磁盘。模拟器路径仅需 ~63MB 预构建产物。

## 构建 agent bundle

agent bundle 是嵌入 `include_str!` 的单文件 JS（~1.4MB），由 bun build 产出后经 build.sh 后处理（`import.meta` 补丁 + 静态 import 改写为 `__require` shim），确保在 skal 的 classic-script 求值模式下不报 SyntaxError。

```bash
bash pi-bundle/build.sh
# 验证：bun -e 'await import("./pi-bundle/dist/agent.js"); console.log(globalThis.__pi_ready)'
```

## 项目结构

```
pi-mobile/
├── src/                      # SolidJS 前端（聊天 UI · 审批 · 文件树 · 命令面板）
├── src-tauri/
│   ├── src/                  # Rust 宿主（pi_bun · loopback · approval · sessions · creds · mcp · skills）
│   ├── gen/android/          # Tauri Android 工程
│   └── gen/apple/            # Tauri iOS 工程
├── pi-bundle/
│   ├── agent-main.js         # Agent 入口（pi-agent-core + host 桥工具 + streamFn）
│   ├── build.sh              # bun build + 后处理补丁管线
│   └── dist/agent.js         # 构建产物（include_str! 嵌入 Rust）
├── scripts/
│   ├── fetch-libpi-bun.sh    # 下载预构建 libskal（Android arm64，~92MB）
│   ├── setup-bun-fork.sh     # vendor bun fork + WebKit（从源码构建用）
│   └── build-libpi-bun.sh    # 从源码构建 libpi-bun（ICU + JSC + bun 交叉编译）
├── docs/
│   ├── PLAN.md               # 架构方案与里程碑路线图
│   ├── PROGRESS.md           # 开发进度日志（倒序）
│   ├── CONTRACTS.md          # UI↔Rust IPC 契约
│   └── LIBPI-BUN-NOTES.md    # skal 工艺研究笔记
└── vendor/                   # gitignored：bun fork + WebKit 源码
```

## 文档

| 文档 | 内容 |
|------|------|
| [docs/PLAN.md](docs/PLAN.md) | 架构设计、技术决策、里程碑路线图（M0–M5） |
| [docs/PROGRESS.md](docs/PROGRESS.md) | 开发进度日志（倒序，含真机调试踩坑记录） |
| [docs/CONTRACTS.md](docs/CONTRACTS.md) | UI↔Rust IPC 契约（commands / events / hostcall） |
| [docs/LIBPI-BUN-NOTES.md](docs/LIBPI-BUN-NOTES.md) | skal 工艺研究、JSC ABI、构建链接、iOS 合规路径 |

## 致谢

- [skal-multiplatform/skal](https://github.com/skal-multiplatform/skal) — bun + JavaScriptCore 经 zig 交叉编译为原生库的工艺先驱
- [earendil-works/pi](https://github.com/earendil-works/pi) — Pi Agent 核心（pi-ai 多 Provider LLM + pi-agent-core agent 运行时）
- [Tauri](https://tauri.app) — 跨平台原生 App 框架
