# PocketPi / PocketJS 调研笔记（2026-09-19）

> 📦 **历史存档（2026-09-20）**：本文档是**换引擎决策的输入**（2026-09-19 调研）。
> 结论已经落地：运行时换成了 QuickJS（见 [PLAN.md](PLAN.md) D18），bun 路线归档在
> `backup/bun`。文中对 pocket-pi / PocketJS / Claude Code / Node-on-mobile 的分析仍然有效，
> 可作为「薄 JS + 厚原生」这条范式的参考读物。


> 调研对象：[pocket-stack/pocket-pi](https://github.com/pocket-stack/pocket-pi)（60★，MIT，Rust）
> 与 [pocket-stack/pocketjs](https://github.com/pocket-stack/pocketjs)（1509★，MIT，TS）。
> **性质：D1（agent 运行时选型）的再评估输入，不是决策。** 结论未经真机验证，
> 全部来自源码阅读与包元数据（本文末尾列证据来源与未验证项）。

## 0. 先纠正一个前提

pocket-pi **不是移动端项目**：它跑在 Waveshare ESP32-P4 / ESP32-S3 触摸屏上
（ESP-IDF + Rust 固件），另有一个 macOS 模拟器。PLAN.md 第 8 行把 pocket-pi 列为
「职责划分范式」的启发来源——那条引用是对的（宿主持有可信机制）——但它的
**运行时选型与我们的 D1 相反**，这一点之前没写进文档。

## 1. 它的运行时方案：薄 JS + 厚原生

```
┌──────────────────────────────────────────────────────┐
│ QuickJS guest（PocketJS）                            │
│   pi-agent-core 的 Agent 类  ← 真实上游代码，非重写     │
│   pi-ai 只 import AssistantMessageEventStream（1 个类）  │
│   自定义 streamFn → host.startModel()                │
│   工具只是壳 → host.startTool()                       │
├──────────────────────────────────────────────────────┤
│ Rust 宿主（pocket-pi-* crates）                       │
│   模型传输 + 协议编解码（4 provider）                   │
│   工具实现（read/write/edit/find/grep/ls/bash/…）      │
│   workspace / SQLite / 凭证 / 调度                     │
└──────────────────────────────────────────────────────┘
```

关键点：**JS 里没有 HTTP、没有 provider SDK、没有 fs**。pi-ai 的 provider 栈被
整体绕开——传输层用 Rust 重写了。

### 证据（文件 / 大小）

| 部件 | 位置 | 大小 |
|---|---|---|
| JS 侧全部（含 prelude） | `crates/pocket-pi-embedded/js/src/entry.ts` + `prelude.js` | 15.3KB + 2.4KB |
| 打包产物 | `apps/pi-agent/dist/agent.js` = `crates/pocket-pi-embedded/js/pi-agent.bundle.js` | **318,349 B** |
| Rust↔JS 桥 | `crates/pocket-pi-embedded/src/lib.rs` | 28.0KB |
| provider 传输（4 家） | `crates/pocket-pi-protocols/src/`（openai_chat 20.6KB + anthropic_messages 12.0KB + codex_decision 5.4KB + model 3.7KB） | ≈ 42KB |
| 工具实现 | `crates/pocket-pi-tools/src/`（coding 22.7KB + schedule 15.5KB + workspace 5.3KB + shell 5.2KB + clock 5.0KB + lib 7.1KB） | ≈ 61KB |
| AgentOS（app 生命周期/SQLite/调度/视图） | `crates/pocket-pi-agentos/src/lib.rs` | 222KB **单文件** |

对照 pi-mobile：`pi-bundle/dist/agent.js` ≈ 1.41MB（PROGRESS 2026-09-16 口径）。
差值主要来自 pi-ai 的 provider 注册表——pocket-pi 只 import 了它的一个事件流类，
`bun build` 把 provider 全部 tree-shake 掉了。

### 桥的形态（与我们的 loopback 是同一个模式）

- `host.startModel(requestJson) -> id`（异步，立即返回）；Rust 在 worker 线程上跑
  HTTP+SSE，把 token 增量投进 mpsc channel。
- `host.startTool(callId, name, argsJson) -> id`；Rust **每个工具调用起一个线程**
  （ESP32 上栈只有 16KB！）执行后投同一 channel。
- `host.poll() -> json[]`：JS 侧 `tick()` 取一批事件；Rust 侧 `tick()` 是
  `call_agent("tick")` + `call_agent("drain")`，即**宿主驱动、kick+轮询**。
- 流式增量在 Rust 侧先 `coalesce_host_events` 按 request id 合并成一条再入 guest
  ——等价于我们 PLAN D5 的 16ms delta 合并。

结论：**我们在 M1 用血的代价发现的「eval 返回 Promise + waitForPromise 会死锁 VM
线程」，pocket-pi 的架构在结构上规避了**（宿主持有循环，guest 不 await 任何 I/O）。

### 它付出的代价（同样重要）

1. **只支持 4 家 provider**（openai / openrouter / anthropic / deepseek，
   `model.rs` 的 `WirelessProvider`）。我们目前内置 8 家 + OAuth 订阅登录 +
   prompt caching / thinking 细节。
2. **会话历史被裁剪**：`entry.ts:377` 的 `discardCompletedToolTrace()` 在每轮结束后
   删掉所有 toolResult 与带 toolCall 的 assistant 消息，只留文本。ESP32 的 PSRAM
   扛不住完整 trace——手机上无此必要，但这是「薄 JS」路线的连带设计。
3. **provider 细节要自己对**：`finishModel()` 里对 thinking/text 顺序、stopReason
   与 toolCalls 的一致性做了硬断言，不一致就报错。上游 SDK 替我们吸收的那些
   quirk，这条路线得自己吸收。
4. **没有 pi-coding-agent 的任何产品层**：无 TUI（它自己的 view SDK）、无
   extensions、无 `JsonlSessionRepo`（会话在 Rust 侧）。

## 2. PocketJS 的移动端战绩（这是最值得注意的一条）

PocketJS = QuickJS guest + Rust core（自绘，无 DOM/CSS/WebView）。其 README 的
硬件表记录了**已启动过的真实机器**，其中包含：

| 平台 | 提交层 | 凭据 |
|---|---|---|
| iOS 12.5.8 | arm64 | PR #278 |
| iOS, current | **NativeScript host** | PR #256 |
| Android 4.3 | JNI | PR #298 |
| ESP-IDF 6.0/6.1 | P4/S3 | 官方组件 |

即：**QuickJS 作为 JS 引擎在 iOS 与 Android 上都有真实落地证据**。这不是意外——
QuickJS 是纯解释器、无 JIT，因此没有 iOS 的 W^X 问题，也不需要 skal 那套
`setenv("JavaScriptCoreUseJIT","0")` 时序技巧；交叉编译就是个普通 C 库。

我们自己的笔记 `LIBPI-BUN-NOTES.md` §3 记的 skal 结论是「QuickJS-NG 无
fetch/URL/Streams，需自建 Web API 数年」而被淘汰——**pocket-pi 正是对这条淘汰理由
的实证反驳**：它不补 Web API，而是把需要网络的那一层整个搬到 Rust，JS 侧只留
agent loop。prelude.js 总共只 polyfill 了 6 个东西（queueMicrotask /
structuredClone / performance / AbortController / TextEncoder / TextDecoder / URL）。

⚠️ 可复用性限制：`pocket_mod`（PocketJS 的 QuickJS guest 层 crate）**未发布到
crates.io**，PocketJS 也不能作为库塞进已有 App（`hosts/android` 是独立 Activity 工程，
iOS 走 NativeScript host）。真要用这个模式，可复用的原语是
[rquickjs](https://crates.io/crates/rquickjs)（0.14.0，4.34M 下载，2026-09-18 仍在更新）。

## 3. 「跑完整版 pi coding agent / Claude Code」的可行性

### 3.1 Claude Code：已经没有 JS 运行时可移植了

| 版本 | 形态 | 证据 |
|---|---|---|
| 1.0.128（最后 1.x） | `bin/cli.js`，`engines: node>=18`，解包 78MB / 52 文件 | npm registry |
| **2.1.277（当前）** | **原生二进制**，主包只有 184KB / 7 文件（launcher + `install.cjs`），真身在 8 个平台包 | `optionalDependencies` |

平台包只有：linux-x64/arm64(-musl)、win32-x64/arm64、darwin-x64/arm64。
**没有 android / ios 目标。** darwin-arm64 包 = 单个 217,662,576 B 的 `claude` 可执行文件。

官方 `@anthropic-ai/claude-agent-sdk` 0.3.277 同理，且其 `bridge.mjs` 用
`child_process` 把它**当子进程拉起来**；同包的 `extractFromBunfs.js` 注释写得
很明白：

> Extracts a file from Bun's `$bunfs` virtual filesystem to a real temp directory
> so it can be spawned as a subprocess (child processes cannot access `$bunfs`).

即 Claude Code 是 `bun build --compile` 产物。**结论：这条路在 iOS 上是死的**
——不只是「没有 JS 引擎可移植」，而是 iOS 禁止 fork/exec，SDK 的存在方式就是
spawn 一个我们不掌握源码的二进制。Android 只有在 Anthropic 发布 android 目标时
才谈得上。

（顺带：`extractFromBunfs.js` 里有一条注释专门处理 "Android-on-Linux where /tmp
isn't writable"——说明 Anthropic 的代码里考虑了 Android/Termux 环境，iOS 没有。）

### 3.2 完整版 pi-coding-agent：是 JS，但要的不是 JS 引擎

`@earendil-works/pi-coding-agent@0.85.1`：`engines: node>=22.19.0`，解包
**21.9MB / 1056 文件**，`bin = dist/bundle/cli.js`。依赖里有 `pi-tui`（终端 UI）、
`cross-spawn`（要 fork/exec）、`jiti`（运行时 TS 加载）、`undici`、`highlight.js`、
`proper-lockfile`、`grok-mermaid`。

逐条核对后的真实阻塞项：

| 依赖 | 是否阻塞移动端 |
|---|---|
| `@silvia-odwyer/photon-node` | ❌ 不是阻塞——解包后是纯 WASM（`photon_rs_bg.wasm` 1.88MB）+ JS 胶水 |
| `@mariozechner/clipboard` | 可选依赖（optionalDependencies），可跳过 |
| `cross-spawn` / bash 工具 | **iOS 硬阻塞**（无 fork/exec）；Android 可行 |
| `pi-tui` | 需要终端：移动端得在 WebView 里塞终端仿真（可做但外道） |
| Node ≥22.19 运行时 | **核心待解项**，见下 |

所以「在 iOS/Android 跑完整版 pi coding agent」= 「在 iOS/Android 跑一个 Node ≥22
运行时」+ 终端 UI + iOS 上放弃 shell 工具。

### 3.3 Node 运行时的可选项盘点（2026-09 现状）

| 方案 | 状态 | 判断 |
|---|---|---|
| [JaneaSystems/nodejs-mobile](https://github.com/JaneaSystems/nodejs-mobile) | **最后提交 2021-10-27**，2598★，174 open issues | 事实停更 5 年，Node 版本落后，不建议 |
| [puerts/backend-nodejs](https://github.com/puerts/backend-nodejs) | 103★，最后提交 2025-07 | 从 nodejs/node 源码构建 libnode 给 iOS/Android/macOS/Windows/Linux，带 3 个补丁（含 iOS 的 ninja 构建与 V8 inspector 导出）。**唯一还活着的 libnode-for-mobile 配方**，但需要自己维护 Node 版本跟进 |
| linroid/libnode | 最后提交 2022-04 | 死 |
| Stremio/node-android | 2★，2026-06 | 未见规模验证 |
| Android：Termux | 活的，真 Node | Android 的最短路径，但**不是 App 集成**——是让用户在终端里跑，与「装进口袋的 App」产品形态不同 |
| [capacitor-mobile-claw](https://github.com/rogelioRuiz/capacitor-mobile-claw) | 12★，2026-06 | 反面证据：它从 OpenClaw（Node/TS）起步，README 明说「the agent core has since moved to a native Rust implementation」。**移动端 agent 的另一个团队也离开了 JS 运行时** |

## 4. 对我们（pi-mobile）的三个可选方向

| | 方案 | 收益 | 成本 / 风险 |
|---|---|---|---|
| **A** | 维持现状（bun + skal，D1 方案 C） | 已真机跑通；上游保真最高（8 provider、OAuth、extensions、JsonlSessionRepo、prompt caching） | 87MB 运行时、bun fork + zig + WebKit 构建链维护、iOS 走解释器模式、bundle 1.41MB |
| **B** | pocket-pi 式「薄 JS + 厚原生」：QuickJS(rquickjs) guest 只跑 Agent 类，provider 传输与工具下沉 Rust | 运行时降到 MB 级；iOS 天然合规（无 JIT、无时序把戏）；无 bun fork 维护；交叉编译简单；工具我们**已经在 Rust 里了** | 要重写 provider 传输（8 家 + OAuth + caching，pocket-pi 用 ≈42KB 换 4 家）；丢掉 pi-ai / pi-agent-core 的 JS 侧产品层（会话 repo、extension、OAuth 协调器）；上游 pi 升级时适配面变大 |
| **C** | companion 模式：手机不跑 agent，连桌面 pi（PLAN §10.2 #8 已有此条） | 零运行时成本 | 违背 G2 离线目标；pocket-pi 自己在 ESP32 上就是这么干的（UART backend 接 codex/claude-code）——**恰恰说明它也没法在设备上跑完整模型栈** |

**我的判断**：A 与 B 不是「对错」问题，是「把复杂度放在构建链还是放在适配层」。

- 我们已经付掉了 A 的主要成本（M1 的两段式 PoC、iOS 从源码构建 JSC、patches、
  真机排障），此时切 B 相当于把已沉没的工程重新分配。
- 但 B 有一条 A 结构上无法消除的优势：**iOS 合规与构建链的简单性**。A 的
  iOS 路线依赖「编译期仍构建 JIT 代码、运行时 setenv 关掉」，这是我们自己
  在 README 里承认需要向审核解释的部分。
- 且 B 的「厚原生」那一半我们**已经做完了**（read/write/edit/ls/grep/rm/git/
  preview/http/native 全在 Rust）。真正要重写的只有 provider 传输这一块。

### 建议的下一步（低成本、可决断）

做一次**桌面 spike**，边界与 M1 PoC 同构、先用最小证据：

1. rquickjs 宿主 + pocket-pi 的 `entry.ts` 改一版，喂我方 `__PI_CONFIG`；
2. 只用 1 家 provider（DeepSeek）在 Rust 侧实现传输；
3. 复用现有 Rust 工具（不重写），验证 pi-agent-core 的 toolCall 往返；
4. 通过则量三件事：bundle 体积、冷启动、单轮对话延迟——再决定是否值得动 D1。

**不要**在 spike 通过前改 PLAN.md 的 D1。

## 5. 证据来源与未验证项

证据来源：GitHub REST API（repo/contents/tree/readme/search）、npm registry 元数据与
tarball 检查、pocket-pi 与 pocketjs 的 README/ARCHITECTURE.md 原文、pocket-pi
源码（`entry.ts` / `embedded-lib.rs` / `coding.rs` / `model.rs` / `prelude.js` /
`app.json`）。

**未验证 / 需存疑**：

- 本机无 ESP32 硬件，pocket-pi **未实际运行过**——本文全部结论来自源码阅读。
- 本机 `git clone github.com:443` 被墙（API 通道可用），故未做完整 clone，
  只取关键文件；pocket-pi 的 222KB `agentos/lib.rs` 未通读。
- PocketJS 的 iOS/Android 战绩是**其 README 的自述 + PR 编号**，未独立复核。
- pocket-pi 的 ESP32 内存约束（16KB 工具线程栈、裁剪 tool trace）是它的语境，
  **不能直接外推到手机**——外推的只有架构形状，不是那些参数。
- 「QuickJS 在 iOS 上性能是否够」未测。pocket-pi 有硬件在跑真实 agent，
  但 ESP32 主频 400MHz 与手机不可比；JetStream 级别的对比数据没有。
