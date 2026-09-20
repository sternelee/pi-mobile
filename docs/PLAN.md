# pi-mobile 项目规划

> 在 Android / iOS 上运行的 Pi Coding Agent —— Tauri 2 + **QuickJS** + SolidJS。
>
> ⚠️ **本文档是规划与决策记录，含有已作废的方案**。运行时已从「嵌入式 bun（libskal）」
> 换成 **QuickJS（rquickjs 静态链接）** —— 见下面的 **D18**；bun 路线（含 D1/D2/D10/D14
> 的部分内容）完整归档在 `backup/bun` 分支，要点见 [LIBPI-BUN-NOTES.md](LIBPI-BUN-NOTES.md)。
> **当前架构以 [README](../README.md) 与 [CONTRACTS.md](CONTRACTS.md) 为准。**

受以下项目启发：

- [skal-multiplatform/skal](https://github.com/skal-multiplatform/skal)：将 bun + JavaScriptCore 经 zig 交叉编译为原生库（libskal）嵌入 Flutter 宿主，SolidJS UI 通过零拷贝共享内存桥驱动原生渲染。证明 **bun 可作为库跑在 Android/iOS 上**，且 SolidJS 是移动端 JS UI 的成熟选择。
- [pocket-stack/pocket-pi](https://github.com/pocket-stack/pocket-pi)：agent-native runtime 的职责划分范式 —— **宿主持有可信机制**（凭证、网络、存储根、生命周期），Pi Agent core harness 作为常驻部分运行，工具目录（workspace / time / shell / schedule）由宿主实现，JS 应用与 agent 共享同一套 actions 与数据。先行构建 macOS 产品契约模拟器（esp32-sim）验证契约再上真机。

Pi Agent 本体来自 [earendil-works/pi](https://github.com/badlogic/pi-mono)（原 badlogic/pi-mono）：

- `@earendil-works/pi-ai`：统一多 Provider LLM API（OpenAI / Anthropic / Google…），**可在浏览器与 Node/Bun 运行**
- `@earendil-works/pi-agent-core`：agent 运行时（工具调用循环 + 状态管理），纯 TypeScript
- `@earendil-works/pi-coding-agent`：交互式 coding agent CLI（TUI 壳，官方用 `bun build --compile` 产出独立可执行文件）

---

## 1. 项目定位

**一句话**：把 Pi Coding Agent 装进口袋 —— 手机本地运行的、带完整工具调用能力的 AI 编程助手，可离线编辑沙箱工作区内的代码，多 Provider 自带 Key。

### 目标

| # | 目标 | 说明 |
|---|------|------|
| G1 | Android / iOS 双平台原生 App | Tauri 2 mobile（gen/android、gen/ios） |
| G2 | 本地运行完整 Pi agent | **libpi-bun**（嵌入式 bun 运行时）内运行完整 `pi-coding-agent`，不依赖远端服务器 |
| G3 | 完整工具集 | read / write / edit / grep / glob / ls 必备；bash 分平台降级 |
| G4 | 会话持久化与恢复 | 兼容 pi 的 JSONL session 格式，App 数据目录存储 |
| G5 | 凭证安全 | API Key 存 iOS Keychain / Android Keystore，由 Rust 宿主持有 |
| G6 | 沙箱工作区 | agent 只能触碰用户授权的工作区目录（pocket-pi 的 workspace 模型） |
| G7 | 桌面同构 | 同一份前端在 macOS/Windows/Linux 桌面 Tauri 上可用作开发调试宿主 |
| G8 | MCP（HTTP）与 Skills 生态 | MCP Streamable HTTP 接入 + SKILL.md 技能包管理（D11/D12，M4 落地） |

### 非目标（v1 明确不做）

- 不做远程设备控制（"手机当桌面 agent 的遥控器"是另一个产品形态）
- 不做 iOS 上的任意 shell —— iOS 沙箱禁止 fork/exec，工具目录必须降级
- 不做 App Store 上架合规评审（先 TestFlight / 侧载 / APK 直发）
- 不重新实现 agent loop / provider 层 —— 上游 pi-mono 是唯一真源，只做嵌入

---

## 2. 技术选型与理由

| 层 | 选型 | 理由 |
|----|------|------|
| App 壳 | **Tauri 2（mobile）** | 一份 Rust core + WebView，`tauri android init` / `tauri ios init` 生成 gen/ 工程；Rust 侧天然承担"可信宿主"角色（对应 pocket-pi 的 native host） |
| UI | **SolidJS + vite-plugin-solid** | 脚手架已就位；skal 同款选型，细粒度响应式适合流式 token 渲染；体积小于 React |
| Agent 核心 | **`pi-agent-core`**（agent 循环）跑在嵌入式 JS 引擎里 | 上游 100% 原生行为：宿主无关（streamFn 与 tools 都是注入点，它自己不碰网络），所以能原样跑 |
| JS 工具链 | **Bun**（workspace / test / build） | 本机 bun 1.3.14；与上游 pi-mono 的运行时假设一致 |
| Agent 运行时 | **QuickJS（rquickjs，静态链进二进制）** —— 见 **D18** | 原选型 libpi-bun（D1）已作废；换引擎的收益是体积（去掉 92MB libskal）与 iOS（不再需要 WebKit/JSC），代价是模型传输必须用 Rust 重写 |
| 宿主服务 | **Rust（Tauri commands）** | 文件、搜索、凭证、HTTP 代理、会话索引全部宿主侧实现，JS 无原始权限 |
| 凭证 | `tauri-plugin-keyring` 或 `keyring` crate | Keychain / Keystore 抽象 |
| 网络 | `tauri-plugin-http` | WebView 内直连 LLM Provider 有 CORS 限制，走 Rust fetch 代理 |
| 存储 | `tauri-plugin-fs`（限定 scope）+ 自研 workspace 命令 | 路径校验、能力声明（对应 pocket-pi 的 storage roots） |
| 默认内置工具 | `tauri-plugin-http` / `-fs` / `-opener` / `-os` | 作为 pi 默认 tool 的原生执行层，见 D2.1 |
| MCP | `@modelcontextprotocol/sdk`（浏览器 streamable-http）+ git2-rs 拉取 | 仅 HTTP 协议，见 D11 |
| Skills | 自研 registry + git2-rs 安装器 | SKILL.md 注入式技能包，见 D12 |
| 用户配置 | 自研 JSON 文件（`{data_dir}/*.json`） | D13 原选 `tauri-plugin-store`，**该插件目前无读写方**；实际持久化是 provider.json / policy.json / mcp.json / goal.json，见 CONTRACTS §3 |

---

## 3. 总体架构（原 D1 = 方案 C；**已被 D18 取代，此节仅存历史**）

> 下面的图是 bun 时代的形态：guest 是完整 bun VM，宿主能力经 loopback HTTP hostcall 进出。
> 现在的形态是「QuickJS guest + `globalThis.host.*` 进程内直调」，图见 README。

```
┌──────────────────────────────────────────────────────────┐
│ WebView（SolidJS 纯 UI，不含 agent 逻辑）                  │
│  会话列表 / 聊天流 / diff 审批 / 文件树 / MCP·Skills 设置    │
└──────────────────── Tauri IPC (invoke / event) ───────────┘
┌──────────────────────────────────────────────────────────┐
│ Rust 宿主核心（消息总线 + 可信机制持有方，pocket-pi 范式）    │
│  凭证服务（Keychain/Keystore）   审批策略（policy 状态机）    │
│  用户配置（store）  会话索引  open URL/通知（tauri 插件）     │
│  libpi-bun 生命周期：加载 / 启动 / 消息泵 / 关停              │
└─────────────── C ABI：hostcall ↓ / 事件回调 ↑ ─────────────┘
┌──────────────────────────────────────────────────────────┐
│ libpi-bun（skal 工艺：zig 交叉编译 bun 为平台库）            │
│  Android: libpi_bun.so（JNI 加载）  iOS: libpi_bun.a       │
│  ┌────────────────────────────────────────────────────┐  │
│  │ pi-coding-agent（完整版）+ pi-ai + extensions       │  │
│  │ 工具原生执行：read/write/edit/grep/glob/bash*       │  │
│  │ 会话 JSONL 原生读写（app_data/sessions）            │  │
│  │ workspace 沙箱 = OS App 沙箱 + cwd 约束 + 审批钩子   │  │
│  └────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────┘

* bash：Android 经 /system/bin/sh 可 exec（系统二进制不受 W^X 限制）；iOS 无。见 D6
```

**核心原则**：

1. **宿主持有信任**（pocket-pi）：凭证、审批策略、生命周期在 Rust；bun 内 agent 经 hostcall 取凭证、上报审批请求，无原生密钥落盘。
2. **上游即真源**：完整 `pi-coding-agent` 原样运行在嵌入式 bun 内（官方 `bun build --compile` 同源 bundle），适配只发生在桥层——不做工具 shim、不做 API 替身。
3. **UI 即客户端**：WebView 退为纯 UI 终端，agent 状态与 WebView 生命周期解耦（D8 由主机制降级为兜底）。
4. **桥分两级**：v1 用 JSON 消息通道（C ABI hostcall + 事件回调，够用且可调试）；skal 式零拷贝共享内存环作为 v2 优化项，接口不变。


---

## 4. 目录结构规划（方案 C 形态）

```
pi-mobile/
├── docs/
│   ├── PLAN.md                  # 本文档
│   └── CONTRACTS.md             # 三份契约：UI↔Rust IPC / Rust↔bun 桥 / store 键空间
├── package.json                 # bun workspace 根
├── vite.config.ts
├── pi-bundle/                   # 跑在 libpi-bun 内的 agent 侧代码（非 WebView）
│   ├── entry.ts                 # 入口：启动 pi-coding-agent，接 hostcall 桥
│   ├── bridge.ts                # C ABI hostcall 绑定（凭证/审批/UI 事件）
│   └── policy-hook.ts           # pi extension：把工具审批上报宿主（D2.1）
├── vendor/bun                   # bun fork（skal 式 patches，gitignored，setup 脚本拉取）
├── patches/                     # bun 补丁集（platform-lib 入口、android/ios 链接）
├── scripts/
│   ├── setup-bun-fork.sh        # clone bun fork + 应用 patches
│   ├── build-libpi-bun.sh       # zig 交叉编译 → gen/android jniLibs / gen/ios
│   └── build-pi-bundle.sh       # bun build --compile 同源 bundle 产物
├── src/                         # SolidJS 纯 UI（无 agent 逻辑）
│   ├── App.tsx
│   ├── ui/                      # SessionList / ChatStream / DiffApproval / FileTree
│   │                            # McpSettings / SkillsManager / Editor
│   ├── bridge/client.ts         # 类型安全 Tauri IPC 封装
│   └── state/                   # signals / stores（settings 经 plugin-store，D13）
├── src-tauri/
│   ├── src/
│   │   ├── lib.rs               # Tauri builder + 插件注册
│   │   ├── pi_bun/              # libpi-bun 装载：FFI 声明、生命周期、消息泵
│   │   ├── bridge.rs            # 桥协议：bun 事件 → UI emit；UI 请求 → hostcall
│   │   ├── commands/            # creds / policy / mcp 配置 / skills registry
│   │   └── workspace/mod.rs     # 会话索引 + workspace 路径策略（供 UI 浏览）
│   ├── capabilities/            # tauri 能力声明
│   └── tauri.conf.json
├── gen/                         # tauri android/ios 工程（入库）
│   ├── android/  └── ios/
└── tests/
    ├── contract/                # 桥协议契约测试（bun 侧 / Rust 侧 / UI 侧）
    └── e2e/                     # 真机与模拟器 E2E
```

---

## 5. 关键设计决策

### D1：agent 运行在哪里？——已定稿为方案 C（嵌入式 bun 运行时）　⛔ **已被 D18 取代，存档**

> 2026-09-04 决策定稿：直接采用方案 C，不再考虑方案 A 与 B。这正是 skal 项目已经验证过的挑战路线——本项目的核心工程价值所在。

| 方案 | 结论 |
|------|------|
| A. WebView 内运行（pi-ai + pi-agent-core 打包进前端，工具过 IPC） | ❌ 放弃 —— 需为 fs/child_process 做全套工具 shim；agent 状态与 WebView 生命周期耦合；上游行为保真度低 |
| B. 远程 RPC（桌面 pi server，手机做客户端） | ❌ 放弃 —— 违背 G2 离线目标 |
| C. **嵌入式 bun 库（libpi-bun，skal 工艺）** | ✅ **定稿** —— zig 交叉编译 bun 为平台库，`pi-coding-agent` 完整版原样运行 |

**定稿理由**：

1. **上游 100% 保真**：extensions、bun API、fs、子进程（Android）全部原生可用，适配层收缩到"桥"一层——不做工具 shim、不做 API 替身。
2. **agent 状态独立于 WebView**：D8 生命周期问题从"主机制"降级为"兜底"。
3. **可行性已被 skal 验证**：bun + JavaScriptCore 经 zig 交叉编译为 libskal，Android/iOS 双平台 + 补丁集 + C ABI 桥；挑战是工程复刻与长期维护，不是未知技术风险。
4. **代价已知并接受**：bun fork/补丁维护、构建管线复杂、iOS 审核合规评估——全部列入风险表与里程碑（M1 前置最高风险项）。

**运行时模型**：

- Android：`libpi_bun.so` 进 `gen/android` jniLibs，Rust 宿主 FFI/JNI 加载；
- iOS：`libpi_bun.a` 静态链接进 Swift 壳；**JSC 以 interpreter + bytecode cache 模式运行（无 JIT）即满足 App Store 约束**——skal 已验证此路径（React Native 同款先例），.jsc 字节码缓存同时解决冷启动解析成本；
- 体积预期：bun + JSC 静态链 ≈ 87 MB（Android arm64，skal 实测口径）；App 体积是可接受代价，记录在案；
- 通信：C ABI —— **hostcall**（bun→Rust：凭证、审批、open、通知）+ **事件回调**（Rust→bun：审批结果、UI 指令）；Rust 核心同时把 agent 事件 emit 给 WebView UI；
- 桥分两级：v1 JSON 消息通道（可调试、够用）；v2 skal 式零拷贝共享内存环（接口不变，纯优化）。

**构建工艺（复刻 skal 实践）**：

- `vendor/`：clone bun fork（pin 到 fork 的专用分支 tip，gitignored，setup 脚本可复现拉取）+ WebKit/JSC 源；
- `patches/`：fork 分支上的 commits（platform-lib 入口 `pi_entry.zig`、Android/iOS 链接配置）——不在本仓库维护 diff 文件，而是维护 fork 分支引用；
- `build/`：各平台 link inputs（gitignored）；`build-libpi-bun.sh` 一键产出 .so/.a 并放进 gen/ 工程；
- JSC 版本耦合：.jsc 字节码缓存与 JSC 版本强绑定（skal 教训），bundle 与运行时同版本构建。

### D2：桥协议（bun ↔ Rust ↔ UI）　⛔ **已失效，存档**（现为 `globalThis.host.*` 进程内调用，见 CONTRACTS §2）

| 通道 | 方向 | 内容 |
|------|------|------|
| hostcall | bun → Rust | `creds_get`（凭证注入，不落 JS 明文盘）、`approval_request`（工具审批）、`open_url`、`notify`、`config_get` |
| 事件回调 | Rust → bun | 审批结果、UI 指令（停止/继续/新会话/模型切换/上下文压缩）、workspace 变更通知 |
| Tauri IPC | UI ↔ Rust | 会话列表/加载（Rust 侧索引）、聊天流事件（`agent:delta` / `agent:tool` / `agent:done`）、审批交互、MCP/skills 配置管理 |

- read/write/edit/grep/glob/bash 等 **pi 原生工具不再过桥**——在 bun 内直接执行，OS App 沙箱为边界；写操作经 `pi-bundle/policy-hook.ts`（pi extension）上报宿主走 `approval_request`。
- 三份契约入 `docs/CONTRACTS.md`：UI↔Rust IPC、Rust↔bun 桥、store 键空间；双侧类型生成 + 契约测试防漂移。
- **审批分类**（policy 状态机）：`auto`（read/grep/ls）→ `ask`（write/edit/bash，UI 弹 diff 审批卡）→ 可按会话/全局调整。

### D2.1 Tauri 官方插件：从"默认工具执行层"转为"宿主能力通道"

> D1 定稿方案 C 后（2026-09-04），pi 原生工具在 bun 内执行，插件角色随之调整。接线保留不变。

| 插件 | 方案 C 下的角色 |
|------|----------------|
| `tauri-plugin-fs` | UI 侧文件树/编辑器浏览 workspace（scope 限定 workspace 根）；agent 工具不再经它 |
| `tauri-plugin-http` | 宿主侧辅助请求（更新检查等）；LLM / web_fetch 流量走 bun 原生 fetch（无 CORS） |
| `tauri-plugin-opener` | hostcall `open_url` 的执行端：URL → 系统浏览器；文件 → Intent / iOS 预览器 |
| `tauri-plugin-os` | 平台信息注入 system prompt 上下文 + UI 平台适配判断 |
| `tauri-plugin-store` | 用户配置持久化（D13，角色不变） |

### D3：会话持久化

- 兼容 pi 的 JSONL 会话格式，存 `app_data_dir/sessions/<id>.jsonl`；Rust 维护索引（标题/模型/时间）。
- 桌面 pi 会话可导入 → "桌面开题、手机续跑"。

### D4：凭证与安全

- API Key 只存 iOS Keychain / Android Keystore（`keyring` crate），JS 侧永不明文持有：pi-ai 初始化时由 `creds_get` 命令注入内存。
- `tauri.conf.json` 开启 CSP；capabilities 最小化白名单；fs 插件 scope 限定 workspace 根（与 WorkspaceGuard canonicalize+prefix 校验形成双保险）；workspace 路径防逃逸 —— 对应 pocket-pi "storage roots 只在宿主"原则。
- LLM 流量走 `tauri-plugin-http`（Rust reqwest），绕开 WebView CORS，统一日志与重试。

### D5：流式渲染

- token 流在 bun 内原生产生（pi-ai + bun 原生 fetch，无 CORS、无 WebView 版本依赖），经桥以 `agent:delta` 事件回传 Rust → emit 给 UI。
- 桥层按 16ms 窗口批量合并 delta，避免高频回调压垮 UI；UI 用 SolidJS batch + 虚拟滚动。
- 取消/停止：UI → Rust → 事件回调 `abort` → bun 内 AbortController，语义与桌面 pi 一致。

### D6：bash 工具的平台现实

- **Android**：`/system/bin/sh` 是系统二进制，**不受 API 29+ W^X 限制、可 exec** —— bun 的 child_process 在嵌入式运行时内可用，toybox 提供常用命令。仍按 `ask` 审批 + 命令黑名单（rm -rf / 等危险模式）兜底。
- **iOS**：系统禁止 fork/exec —— 不注册 bash 工具，工具目录对模型声明不可用（pi 工具目录可配置）。
- iOS 补偿：git 子集（Rust 侧 git2-rs：status/diff/commit/branch），工作区默认 `git init`，agent 写操作全部可回滚。

### D7：UI 信息架构（pi TUI → 手机）

| pi TUI 概念 | 手机形态 |
|-------------|----------|
| session 列表/切换 | 首页会话列表（左滑删除） |
| 对话流 | 聊天流（markdown + 代码高亮 + 复制） |
| 工具调用展示 | 可折叠工具卡（read 显示文件名，edit 显示 diff） |
| 审批 y/n | 底部审批 Sheet：diff + 批准/拒绝/总是允许 |
| 文件浏览器 | 文件树 Tab + 只读预览（M3 受限编辑） |
| /命令 | 命令面板（模型切换、上下文压缩、分支重放） |
| MCP / skills 管理 | 设置页：MCP 服务器列表（启停/工具目录/连接状态）+ 技能包安装/启停/更新（D11/D12） |

### D8：移动端生命周期约束

- **iOS 后台**：WebView 挂起即流式中断 —— 进后台时 agent loop 主动 checkpoint（session 追加 + UI 恢复点），回前台续跑。
- **Android**：M4 用 gen/android Kotlin 侧前台服务保流；v1 先做 checkpoint/恢复。
- 长请求期间全屏态防误触返回。

### D9：Provider OAuth 与深链回调（二次审查补充）

- D4 只覆盖了 API Key；部分 Provider 支持 OAuth 授权（如 Claude Pro/Max 订阅制）。移动端没有 `localhost` redirect —— 注册 custom URL scheme（`pimobile://`，`tauri-plugin-deep-link`），授权页在系统浏览器打开，回调经 deep-link 插件送回 WebView 完成交换。
- 凭证服务统一抽象：API Key 与 OAuth token（含 refresh）都进 Keychain/Keystore，refresh 由 Rust 侧处理，JS 只拿短期访问凭证。
- M1 只做 API Key Provider；OAuth 落地 M2，且限定已验证的 Provider 逐个开。

### D10：fetch 适配与流式降级（已随 D1=C 定稿失效，存档）

- 方案 A 时代的设计：patch `globalThis.fetch` → plugin-http、SSE 老 WebView 降级。方案 C 定稿后 agent 跑在 bun 内，原生 fetch 无 CORS、无 WebView 版本依赖，本决策失效存档；plugin-http 保留宿主侧用途（D2.1）。

### D11：MCP 接入（HTTP 为主干，Android stdio 实验通道）

- **传输**：Streamable HTTP（MCP 规范 JSON-RPC over POST + SSE 事件流）为主，兼容旧 HTTP+SSE；stdio 仅 Android 可行（经 `/system/bin/sh`，D6 修订），iOS 不可行——HTTP 为主干、Android stdio 作实验通道。
- **客户端位置**：bun 内运行（pi 原生 MCP 客户端），HTTP transport 原生支持；移动网络切换频繁 → 断线重连、会话 resumable 由桥层处理，MCP 调用失败按工具错误返回，不阻塞 agent 主循环。
- **服务器注册**：设置界面添加 URL + 可选 headers；header 里的凭证经 creds 服务引用（配置文件只存引用，不明文）；配置存 `app_data/mcp.json`。
- **工具命名与审批**：`mcp__<server>__<tool>` 注册进 pi 工具目录；默认全部 `ask` 审批，用户可对单个 server 的只读工具降为 `auto`；每个 server 可独立启停。
- **安全**：仅 https；工具目录在 UI 完全可见可审计；上游 pi 的 MCP 能力是真源——若上游支持浏览器 transport，优先复用，适配层只做注册与策略。

### D12：Skills 管理

- **Skill 定义**：自包含技能包（SKILL.md 规范：指令 + 可选资源），作用是**注入 system prompt**（与 AGENTS.md 同层），不执行任意代码——包内脚本在移动端仅作参考文本注入，不运行（与 D6 一致的边界）。
- **存储**：`app_data/skills/<id>/`（内容）+ `registry.json`（来源 URL、版本、checksum、启停状态、作用域）；作用域分全局 / 单 workspace。
- **安装来源**：git URL（Rust git2 拉取）/ 直接 URL / 内置推荐目录；安装时版本 pin + checksum 校验，**参照 pi-mono 供应链纪律（不安装当天发布的版本）**。
- **管理界面**：安装 / 更新 / 启停 / 删除；禁用 = 不注入；删除后历史会话保留 id+version 引用记录。
- **成本联动**：启用 skills 的注入内容计入上下文预算，在 M3 用量可视化中可见。

### D13：用户配置存储（tauri-plugin-store）　⚠️ **未落地**（实际用 JSON 文件，见 CONTRACTS §3）

- **职责边界**：`tauri-plugin-store`（JSON KV + autosave，存 `app_data_dir`）负责**用户配置**——UI 偏好（主题/语言/字号）、默认 Provider 与模型、审批策略基线（policy 默认值）、onboarding 完成标记、MVP 级杂项开关。
- **不放 store 的数据**（仍走宿主文件服务，因需 schema 校验/原子写/契约测试）：会话 JSONL（D3）、skills registry（D12）、mcp.json（D11）——store 只做"键值偏好"，结构化数据归契约层。
- **键空间入契约**：store 的全部 key 在 `docs/CONTRACTS.md` 登记（如 `settings.theme`、`settings.defaultModel`、`policy.default.*`），与 IPC schema 同等对待，防键名漂移；`src/state/settings.ts` 做类型安全封装，UI 只读 signal，写经统一 setter。
- **迁移**：key 结构变更走版本字段（`settings.version`），Rust/TS 两侧共用迁移表。

### D14：脚本执行（agent 自写 JS 并运行）——显式能力授予　⚠️ **隔离 runner 随 bun 归档**（策略层 `script.rs` 保留，见 D18）

- **定位**：这是本项目**唯一的 exec 面**，是 D6「无 exec」的定向例外。价值不在「能跑代码」，而在**一次脚本替代 N 次工具往返**——移动端的瓶颈是 LLM 往返延迟，不是解释器速度，故 iOS 无 JIT 在此可接受。**不引入子进程**：`fork`/`exec` 在 iOS 被禁，脚本跑在**进程内独立 JS context**。
- **核心安全不变式**：*脚本永远不能做超出「用户在审批卡上看到的那份能力清单」的事。* 三条支撑缺一不可：
  1. **隔离**——脚本跑独立 context/VM，agent 的 global 与工具包装函数**不可达**。这条是承载性的：若脚本能调 agent 的工具，它就继承了 agent 的全部授权，门等于没装。
  2. **不可伪造的主体**——Rust 每次运行发 per-run token；脚本侧 hostcall 包装带该 token；Rust 由 token 反解身份与授权表 → 脚本无法冒充 agent。
  3. **边界强制**——判定发生在 **Rust 侧 hostcall dispatch**，不在 JS 侧。JS 侧的检查只算 UX，不算安全。
- **能力清单**（`needs` 字段，**默认全拒**）：`fs:read` / `fs:write`（workspace jail 内）、`native:contacts` / `native:photos` / `native:calendar:read` / `native:calendar:write` / `native:location` / `native:clipboard` / `native:clipboard:write` / `native:notify` / `native:weather` / `net`。
- **永不可授予**（脚本拿到即可冒充 agent 或窃取凭证）：`agent_event`、`approval_request`（否则能自问自答绕过人）、`ask_user_register`、`creds_get`/`creds_set`/`creds_json_*`、`oauth_*`、`mcp_config`/`goal_get`/`skills_config`。**列清「永不可授予」与列清「可授予」同等重要**——只写后者等于默认其余可给。
- **`needs` 由模型声明，不做推断**（静态读字段）。漏报的后果是脚本**运行时报错**（fail-safe 方向）而非越权；但报错信息必须明说「用了未声明的能力”，否则模型只会反复重试（同 CONTRACTS §2.2 权限指引纪律：给可执行指引而非空结果）。
- **审批粒度**：脚本**不参与 `always` 全局降级**。现有 `always` 会把 `policy.json` 的 `write` 降成 `auto`，用在脚本上等于永久交出任意能力 → v1 只有 allow once / deny。
- **资源上界**（全部 Rust 侧，防 DoS）：`JSContextGroupSetExecutionTimeLimit` 硬超时（默认 5s）；hostcall 次数上限与单次响应字节上限；返回值 + console 捕获的字节上限。
- **验收以负向测试为准**：正向「脚本跑通」极易假绿，必须逐条验证——越权 hostcall 被拒、伪造 token 无效、`while(1){}` 到点被终止且 app 不冻、agent 全局对象不可达、`needs` 漏报时明确报错。
- **实施依赖（C 是 B 的超集）**：隔离与超时是 C 的前置而非可选项。**两个未知已由读 fork 源码基本解决**（2026-09-13 spike）：
  - ① **VM 是 per-thread 的**：`src/jsc/VirtualMachine.zig:327` 为 `pub threadlocal var vm: ?*VirtualMachine = null` → **一线程一 VM**。所以隔离 = **起一条专用线程 + 在那里 init 新 VM**，agent 的 `globalThis` 是另一个 `JSGlobalObject`，结构性不可达（不靠「约定」）。同进程多 VM 在 bun 内有现成先例：`src/js_parser_jsc/Macro.zig`、`src/jsc/Debugger.zig`、`src/cli/repl_command.zig` 都各自 `VirtualMachine.init`。
  - ② **超时用 bun 自己的包装，不要裸调 JSC**：`src/jsc/VM.zig` 已封装 `hasExecutionTimeLimit` / `setExecutionTimeLimit(vm, f64)` / `clearExecutionTimeLimit`（底层即 `JSC__VM__*ExecutionTimeLimit`），且 `src/runtime/api/bun/js_bun_spawn_bindings.zig:561` 已有使用先例 → 属 bun **支持的**机制，无「与 bun 内部 watchdog 冲突」之障。仍遵守「能用框架 API 就不碰底层」纪律，不直接调 `JSContextGroupSetExecutionTimeLimit`。
  - **spike 已验证（2026-09-14，macOS 宿主）**，两个前置都成立，并带回四条会改设计的发现：
    - **Q1 第二 VM 可行**：起线程后 init 仅 2ms。**隔离是结构性的**：第二 VM 里 `typeof globalThis.__pi_hostcall` / `typeof globalThis.__PI_CONFIG` 均为 `undefined` —— agent 的 hostcall 与配置不可达，这正是本决策的承载性前提。
    - **⚠️ 陷阱：第二 VM 必须 `is_main_thread = false`**。`VMHolder.main_thread_vm`（`VirtualMachine.zig:329`）**不是** threadlocal，传 `true` 会覆盖真正的 agent VM 指针（bun 内部按它做判断的地方全错），并给这个一次性 VM 装上 `ParentDeathWatchdog`。
    - **Q2 执行时限可行，且 VM 可复用**：1500ms 限时在 **1505ms** 触发，进程存活；`clearExecutionTimeLimit()` 后同一 VM 继续正常 eval（`recovered:2`）→ 「一次运行一个 VM」不是被迫的。
    - **超时回传形态（实现依据）**：`out_is_error=1` + `out_result="JavaScript execution terminated."`（32 字节）。但**不要**用 `JSValue.isTerminationException` 判别——`Bun__REPL__evaluate` 已把终止转成普通 Error（实测 `isTerminationException=false`）。要么认这条文案，要么用「elapsed ≈ limit」自己记账。
    - **⚠️ 看门狗是 CPU 计费的（超出原设计）**：`Watchdog::shouldTerminate` 除墙钟还比对 `CPUTime::forCurrentThread()` 与 `m_cpuDeadline`。CPU 打满的死循环准时触发，但**脚本阻塞在 I/O（`await fetch` 永不返回）不消耗 CPU → 看门狗不响**。→ **仍需在 hostcall 层加一条墙钟/空闲看护**，不能只靠它。
    - **⚠️ `setExecutionTimeLimit` 在 bun 内零调用点**（只有 `hasExecutionTimeLimit()` 被 spawnSync 快速路径用作守卫）→ 这条通路**没被 bun 自己压测过**，我们是第一个真实使用者。好处：不会与内部 watchdog 打架；坏处：无前例背书。
    - 其他实现细节：时限是 per-VM（`vm->watchdog()`）；新线程上**必须先 `bun.Output.Source.setInit(...)`**（threadlocal；漏了会在 VM init 的 `console.init` 读未初始化 threadlocal 时带走承载进程，这是从 `workerMain` 照搬的教训）；`bun.jsc.initialize(false)` **不需要**再调（进程一次，agent 线程已做）。
- **spike 跑在哪**：宿主（darwin-aarch64 / Release / 预构建 WebKit），`scripts/link-skal-macos.sh` + `scripts/spike-harness.c` 可复现。**iOS 真机仍验不了**（设备掉线 + profile 过期）→ 「iOS 关 JIT 下看门狗是否照常生效」仍未证（看门狗是独立线程、不需 JIT，理论上无碍）。Android 未跑（同引擎路径；`PI_SPIKE=1` 自动触发钩子已留在 `skal_create_runtime` 尾部备用）。
- **清理义务**：spike 脚手架（`patches/pi_entry.zig` 新增 239 行）**会随 fork 进 iOS dylib**。默认惰性（`PI_SPIKE` 未设时不触发），但**上线前必须删除**；D14 真实 runner 落地时会自然取代它。

### D15：产物预览（agent 自写 html/js/css → iframe 预览）

- **定位**：让 agent 写出的页面能直接看。**编写侧已有**——`write`/`mkdir` 本来就
  jail 在 `{data_dir}/workspace`，不需要新工具；本决策只解决「**看**」。
- **为什么不能只把文件内联成 `srcdoc`**：多文件项目靠相对路径互相引用（html → css/js），
  内联会打断相对路径，且要手工处理转义。必须要一个真实的 HTTP 源。
- **服务方式**：Rust 侧新增**独立端口**的只读静态服务（`127.0.0.1:<previewPort>`），
  路径 jail 到 workspace。**独立端口是刻意的**：让预览与 `/hostcall` **不同源**，
  这是纵深防御；真正的防线仍是 `REQUIRE_HOST_TOKEN`（预览页拿不到 host token，
  所以它即便自己去打 hostcall 也是拒）。
- **iframe sandbox**：只给 `allow-scripts` + `allow-forms`。**不给** `allow-same-origin`
  （预览页是 opaque origin，够不到 app 的 DOM、也没 localStorage）、**不给**
  `allow-top-navigation`（不能把 app 导航走）、**不给** `allow-popups`。
- **网络：允许**（用户决定，2026-09-14）。可引 CDN/字体/公开 API，交互最完整。
- **⚠️ 这个选择下必须明确写下的后果**：**agent 写的页面可以把数据发到任意外网**。
  这是本决策里唯一不可控的一条，而且它与其他工具的能力**叠加**——比如脚本先经
  `fs:read` 读到工作区文件，再由预览页把内容 POST 出去。有意接受，因为：
  (a) 预览页仍然够不到 app DOM、拿不到 host token、无法调任何工具；
  (b) 能外泄的只有页面自己能生成的（或先经审批写进 workspace 的东西）；
  (c) 预览是**用户主动打开**的可见面。
- **需要的配套**：
  1. 预览必须**看得见是预览**（顶栏/边框标识 + 当前路径），不能与 app 自身 UI 混淆
     —— 否则它就是一个现成的钓鱼面。
  2. 只读服务，且只服务 workspace 内文件（复用 `loopback::jail_path`）；
     不得因路径穿越读到 `creds.json`/`sessions/` 等。
  3. 响应带合理头部（`Content-Type` 按扩展名；不缓存，或短缓存 + 重开时带
     版本参数）。
- **agent 侧 `preview` 工具：已实现**（`7ec576f`）。用户的流程是「让 pi 写一个
  五子棋 → 它写 html/js/css → **它调预览工具**」，所以工具是必需的，不是可选项。
  hostcall `preview_open` 为 **agent 主体专用**（不在 `script.rs` 白名单 → 脚本
  自调自动被拒：预览是 UI 动作）；**不标 mutating**（不改任何东西，用户当场看到）。
  路径**必须已存在**，否则错误里直接列出「工作区现有哪些 html」——不校验的话模型
  填错时面板空白、而它从返回值看到 ok 就以为成功，会在错误前提上继续调试。
- **实现从手写 HTTP 换成 axum + `tower-http::ServeDir`**：手写 HTTP 是经典 bug
  重灾区。注意 **`ServeDir` 会跟随符号链接**（与手写版共有的风险），故额外加
  `deny_escape` 中间件做 canonicalize + 前缀校验；它不列目录（✓ 不泄露文件名）。
- **真机已验**（2026-09-15）：pi 写五子棋 + 调 preview → 面板自动弹出且可玩。

### D16：Git 集成（工作区内的 clone/pull/push）

- **平台硬约束（先于一切选型）**：移动端**没有 `git` 二进制、没有 shell 可执行**
  （D6 无 exec）→ 必须用**库**实现，不能 shell out，也不能像桌面那样靠 sidecar。
- **库选型：`gix`（gitoxide，纯 Rust），不用 `git2`/libgit2。**
  这不是口味问题，是**构建风险**问题：`git2` 是 C 库绑定，交叉编译到
  arm64-android + arm64-ios 要 C 工具链 + cmake，`libgit2-sys` 的 vendored 构建在
  iOS 真机上已知麻烦 —— 正是我们刚在「Android 从源码构建 JSC」上踩过的那类坑
  （那条流水线的脚本甚至从未存在过，见 docs/PROGRESS.md 2026-09-14）。
  **D12 当时已经为此刻意不引 git2**（理由同上，改用 codeload zipball）—— 本决策
  不推翻它，只是补上「要真 git 能力时该用什么」。`gix` 无 C 依赖，与现有 Rust
  代码同一条交叉编译路径。
- **❌ 已验证：`gix` 0.87.1 没有 push。**（2026-09-15，源码实测，不是猜）
  `remote/connection/` 下只有 `fetch/`；全 crate 只有 `PushRefSpec`（配置解析）与
  `Push`（配置段），**无 `prepare_push`**。所以 clone / fetch / pull 可用，**推送不可用**。

  这条验证在**写任何集成代码之前**做掉了（本决策自己定的「先 spike 再动工」），
  省掉了后面几百行白写 —— 与 A3 那条纪律同一来源。

- **🔄 外部证据推翻了我的初步推荐（2026-09-15，待用户定）**：[GitSync]
  (https://github.com/ViscousPot/GitSync) 是 **Flutter + Rust core**（同一架构形态：
  Rust 核心 + 移动壳，只是壳用 Flutter），支持 **Android 5+ / iOS 13+**，功能含
  clone/fetch/pull/commit/**push**/合并冲突/网络恢复重试，认证支持 HTTPS/SSH/OAuth。
  它的 Rust `Cargo.toml` 写的是：

  ```toml
  git2 = { version = "0.20.4" }
  libssh2-sys = { version = "0.3.1" }
  [features]
  default = ["vendored"]
  vendored = ["git2/vendored-libgit2", "git2/vendored-openssl"]
  ```

  → **`git2` + vendored libgit2 + vendored OpenSSL 在 aarch64-android 与
  aarch64-ios 上是跑得通的**（连 OpenSSL 都自带编译，那是最难的一环）。也就是说
  D12 当年判为「有风险」的那条路**已有生产级先例**，而当时的风险评估针对的是
  napi-rs/@google/genai 那一族，不能直接外推到 libgit2。

  **于是推荐改为 `git2`（vendored）**，理由：gix 缺 push（上面刚实测），而 push 是
  本需求明确要的一项；git2 一次拿到 push + SSH + HTTPS，且交叉编译有先例可循。
  **✅ 构建配方已拿到（照抄，别自己试）**：GitSync 的 `rust/.cargo/config.toml`
  全部有效内容就两个环境变量：

  ```toml
  [env]
  ZLIB_SRC = "1"                        # zlib 也从源码编（libz-sys 的约定）
  LIBGIT2_SYS_USE_PKG_CONFIG = "0"      # 别去找 pkg-config，走 vendored
  ```

  也就是说「C 交叉编译风险」实际是**两个 env + `vendored` features**，且在
  Android 5+ / iOS 13+ 上已验证。这两个变量恰好是最容易踩的那两个坑（vendored
  误用 pkg-config；zlib 也得一起编），照抄即省掉几小时的试错。

  **仍需自己验的一条**：iOS 上 libgit2 的 HTTPS 是否走 Security.framework
  （若走，`vendored-openssl` 可能只为 Android 而开）。但 GitSync 默认在双端都开
  vendored-openssl，**先用「和它一样」的配置验证，再考虑优化**——不要一上手就
  自作聪明地裁掉 openssl。

  **push 的三条退路（若仍选 gix 则适用，按代价排序）**：
  1. **v1 不做 push**：clone/pull/status/diff/log/commit 先用 gix 落地，push 延后。
     代价最低且不引入新风险，但「让 agent 把成果推上去」这个诉求要等。
  2. **手写 smart-HTTP push**（`git-receive-pack`）：协议本身不复杂（一个
     POST + pkt-line + 对象打包复用 gix 已有的 pack 能力），但要自己处理
     认证与协商，属**真活**。
  3. **为 push 单独引入 git2**：等于**同一个功能两套 git 实现**，且把 D12 刻意避开
     的 NDK 交叉编译风险请回来 —— 不推荐（仅当 2 也不可接受时）。

  另一条可选路线：**不做 push，改用现有 HTTP 能力把产物经 GitHub API 提交**
  （`PUT /repos/{o}/{r}/contents/{path}`）—— 语义比 push 弱（不能推历史、一次一文件），
  但对「把生成的页面传上去」这类真实需求可能够用，且复用现存依赖。
- **工具集与审批：按「后果」分档，不按 API 分**

  | 工具 | 后果 | 审批 |
  |---|---|---|
  | `git_status` / `git_diff` / `git_log` | 只读 | 自动 |
  | `git_clone` | 读网络 + 写工作区（不覆盖已有文件） | 自动，但目标目录非空则拒 |
  | `git_pull` | **覆盖工作区文件** | **ask** |
  | `git_commit` | 只改本地仓库（与 `write` 同级后果） | 跟 `write` 同一基线 |
  | `git_push` | **把用户代码发到远端（数据外泄）** | **ask，且卡上必须显示远端 URL** |

- **URL 纪律**：复用 `http_tool::validate_url`（已拒 loopback/私网/链路本地）
  **+ 只允许 https**。远端的理由不止 SSRF：远端地址是本功能最大的外泄面，而
  `git://`/`ssh://`/`file://` 各自带一套不同的信任假设，v1 只认 https。
- **凭证**：走现有 `creds`（`git:<host>` 作 provider key；iOS=keyring /
  Android=沙箱文件，见 src/creds.rs）。**凭证绝不得进脚本 VM** → git 工具对 D14
  脚本**永不可授予**（写进 `script.rs::NEVER_GRANTABLE` 的审计清单）。
- **jail**：仓库根必须在 workspace 内；`clone` 的目标路径同样过 `jail_path`。
- **待定**：`git_push` 的授权粒度（每次 ask / 按 host 记住 / 远端预先登记）；
  `git_commit` 的 author 身份从哪来（无 `user.name` 配置）。
- **⛔ 当前阻塞（2026-09-15）**：**https 远端在 Android 上被 TLS 证书加载卡住** ——
  `set_ssl_cert_file` 报 `error:05880020 … ::BIO lib`，4 轮修复未解。已排除「无 TLS
  后端 / env 没设上 / CA 路径格式错 / 文件不可读 / bundle 格式」五种假设，剩下
  `set_ssl_cert_dir`（未试）与「最小单证书」决断实验。**详细证据链与教训见
  docs/PROGRESS.md 2026-09-15 那条**。
  注意：**本地操作（init/commit/status/log/diff）不走 TLS**，不受此阻塞影响 ——
  可先把远端操作标为暂不可用，把本地工作流放出去用。

---

### D18：换运行时 —— 嵌入式 bun → QuickJS（2026-09-20，**当前形态**）

**决策**：agent 运行时不再用「skal 工艺交叉编译的 bun 库（libskal）」，改为 **QuickJS**
（`rquickjs`，纯 C 引擎，静态编进 Rust 二进制）。bun 路线归档在 `backup/bun` 分支。

**为什么换**（都是量出来的，不是偏好）：

| 维度 | 嵌入式 bun | QuickJS |
|---|---|---|
| 额外产物 | **92 MB** libskal.so / .dylib | **0**（引擎在 app 二进制里） |
| iOS | 从源码构建 WebKit（~13 GB 磁盘、关 JIT 的时序坑、DNS 后端补丁） | 一个链接参数（`libclang_rt.ios.a`） |
| agent JS 体积 | 1.4 MB（+ node 内建垫片） | 385 KB |
| 启动 | — | 0.36 s（修掉一个 boot 信号 bug 后实测） |
| Android 依赖 | jniLibs 里的 .so 必须在 `nativeLibraryDir` 且可 exec | 无 |

**代价（这是这条路线真正的账）**：pi-ai 的**传输层**要的是「一整个 Web/Node 平台」（4 家厂商
SDK + 68 处 node 内建 + fetch/Streams），QuickJS 恰恰不提供 —— 所以**模型传输必须用 Rust
重写，一家族一家族地补**：

| 家族 | 状态 | 覆盖（pi-ai 目录） |
|---|---|---|
| `openai-responses` | ✅ | 105 模型 / 6 provider（含 UI 第一行 openai、xai） |
| `openai-completions` | 🔨 只做了 DeepSeek 的 compat 档案 | 653 模型 / 26 provider（openrouter 333 个尚未解锁） |
| `anthropic-messages` / `google-generative-ai` / `openai-codex-responses` / bedrock… | ⛔ 未做 | 剩下那些 |

**没跟着搬过来的**（工具壳，实现都在、只差接到 agent 上）：native 4 个工具、git 6 个、
D14 脚本沙箱（隔离 runner 是 bun 的整 VM，需要重新设计）、OAuth 订阅登录、preview 工具。
详见 CONTRACTS §2.5 与 [PROGRESS](PROGRESS.md) 2026-09-20（第十五轮）。

**保留下来的既有决策**：D3（会话 JSONL 格式，两路线互通）、D4（凭证只在宿主）、D6（无 exec）、
D11（MCP）、D12（Skills）、D15（预览）、D16（git）、D17（UI）。**D1/D2/D10 作废**，
D13 未落地，D14 待重新设计。

**教训（值得单独记）**：换引擎本身不难，难的是**「宿主该给 agent 的东西」有没有全给到**。
这条路上连撞四次同类问题（boot 信号白等 30 s、UI 永远停在 booting、agent 没有文件工具、
UI 命令报 workspace not configured）——每一个都只在真机上暴露，且都属于「事件/能力没接到
UI 上」。所以现在 **`scripts/qjs-tests.sh` 是必跑的**，CI 也显式跑它。

## 6. 里程碑路线图（原方案 C 形态：libpi-bun 为关键路径）　⛔ **关键路径已作废**（D18 后它消失了）

### M0 —— 走通 Tauri mobile ✅（收尾中）
- [x] bun 接管 workspace（tauri.conf.json 命令已改 bun）
- [x] `bun tauri android init` → gen/android 入库（**优先 Android**；ios init 顺延至 M5 前）
- [x] CI 骨架：biome + typecheck + cargo fmt/clippy/test + aarch64-android 交叉检查 + 桌面 build 矩阵（`.github/workflows/ci.yml`）
- [x] 插件接线：http / fs / opener / os / store（依赖、注册、capabilities）
- [x] Android **真机**跑通模板（MEY-AN00 / arm64 / Android 16：Rust aarch64 交叉编译 + Gradle 8.14.3 构建 + 安装启动 + WebView 加载 Vite dev server，用户确认首页显示正常）
- **出口条件**：真机显示模板 UI ✅（2026-09-04 达成）

### M1 —— libpi-bun PoC（~3 周，最高风险前置，skal 挑战复刻）
- [x] skal 工艺研究 → `docs/LIBPI-BUN-NOTES.md`（入口形态、构建链接、符号守卫、JSC 合规路径）
- [x] **PoC 两段式启动**：第一段用 skal CI 预构建产物（`scripts/fetch-libpi-bun.sh`，pin 记录见 NOTES §5）+ Rust dlopen（`pi_bun/mod.rs`，skal ABI 4 符号）真机打通；第二段切自有 pi_entry.zig 从源码构建
- [x] Rust `pi_bun/` 模块 + `pi_bun_smoke` 命令（libloading dlopen、spawn_blocking、logcat 输出）+ UI 冒烟入口（App.tsx）
- [x] aarch64-linux-android 交叉编译检查通过（含 libloading）
- [x] 预构建 .so（92MB）下载完成 → sha256 校验通过（`5cdc391b…`）→ 16KB 对齐检测通过（p_align 0x4000/0x10000，NDK readelf 复核）→ 装入 jniLibs
- [x] **M1 完成标记（2026-09-04）**：第一段真机验证 + M2 桥打通后，从源码构建降为后台任务（脚本就绪：`setup-bun-fork.sh`/`build-libpi-bun.sh`，WebKit 克隆暂停可恢复；产物过符号守卫后无缝替换预构建 .so）
- [x] **M2 桥 ✅ 真机验证**：JS→Rust = bun 原生 fetch → loopback HTTP 微服务（`loopback.rs`，**13ms 往返**）；Rust→JS = `skal_evaluate` 注入。**关键发现：eval 返回 Promise + waitForPromise 会死锁 VM 线程（阻塞了 fetch I/O 依赖的 tick），必须用 kick+轮询/事件模式**——验证了 `pibun_*` ABI（start/post_event/wake）的设计正确性
### M1 第二段（调整 2026-09-04）：预构建产物上直接进 M2，从源码构建降为后台任务

> 决策：WebKit（JSC 源码）克隆暂停。理由：它是 bun 在 Android 上的 JS 引擎编译期依赖（非 UI 组件），但 M1 第一段已用预构建 libskal 打通全链路；M2 的桥接需求可用「skal_evaluate（Rust→JS）+ HTTP loopback fetch（JS→Rust）」在预构建 ABI 上完整实现，无需重构建。从源码构建（scripts/ 已就绪）择机后台补做，产物通过符号守卫后无缝替换。

- [x] M1 第一段出口条件达成（见上）
- [x] `patches/pi_entry.zig` + 从源码构建流水线已备好（`setup-bun-fork.sh` / `build-libpi-bun.sh`，WebKit 克隆暂停，随时可恢复）
- [ ] **M2 桥（预构建 ABI）**：Rust loopback HTTP 微服务（127.0.0.1，hostcall 通道）+ `skal_evaluate` 事件注入 + JS `bridge.ts`（fetch 封装）
- [ ] （后台/择机）WebKit 克隆 + `build-libpi-bun.sh` 从源码构建 → 符号守卫 → 替换预构建产物为自有 `pibun_*` ABI
- ⚠️ 网络注意：手机与宿主必须同网段（devUrl 走 LAN IP；不要设 TAURI_DEV_HOST=localhost——Android WebView 的 tauri.localhost 不走 adb reverse）

- [ ] `scripts/setup-bun-fork.sh`：vendor bun fork（参照 skal 补丁工艺），锁定版本，全自动可复现
- [ ] zig 交叉编译 aarch64-android → `libpi_bun.so`，一键脚本产物进 `gen/android` jniLibs
- [ ] Rust `pi_bun/` 模块：FFI 装载、生命周期、消息泵；宿主注入 HOME/TMPDIR=app_data 子目录（沙箱语义对齐）
- [ ] C ABI echo PoC：hostcall + 事件回调往返；真机 logcat 验证 bun 执行 hello-world JS
- **出口条件**：真机 logcat 出现嵌入式 bun 的 JS 执行输出；桥往返（JSON 通道）< 5ms

### M2 —— pi bundle 与最小聊天流（~2 周）✅（2026-09-05 出口条件达成）
- [x] `pi-bundle/agent-main.js`：`@earendil-works/pi-agent-core` Agent 在 libpi-bun 内
  headless 启动（kick+poll 契约；实际形态为 Agent 核心 + host 桥工具，非完整 pi-coding-agent
  CLI——TUI 壳不适用移动端，agent loop/工具/会话全部上游真源）
- [x] JS↔Rust 桥（loopback HTTP + hostcall，13ms 往返）；契约见 `docs/CONTRACTS.md`（M2 现状版）
- [x] **会话 JSONL 落盘**（2026-09-05 收尾）：pi 原生 `JsonlSessionRepo` + hostcall `fs` 后端
  （磁盘 I/O 留 Rust，jail 到 `app_data/sessions`）；pi-v4 格式与桌面兼容；
  boot 自动回放最新会话，`agent_history` 供 UI 渲染；本机 `pi-bundle/session-test.js`
  两阶段往返验证（落盘 → 新进程恢复 → v4 header 校验）
- [x] creds 服务（D4 部分落地）：桌面 keyring（Keychain/Credential Manager/keyutils）+
  旧 creds.json 迁移；**Android 暂为沙箱文件态（0600）**——keyring v3 无 Android Keystore
  后端，迁移需 tauri 插件走 JNI，落 M3
- [x] 最小聊天流 UI：API key 输入 → 对话（流式 delta/工具调用/状态气泡）→ 重启恢复
- [x] **出口条件（真机，2026-09-05）**：DeepSeek V4 Flash 真实 LLM 对话 +
  `ls` 工具调用 round-trip（Honor 真机，提交 `2facaec`）；重启恢复本机验证通过
- 遗留 → M3：Android Keystore 凭证加密、google-generative-ai provider 重接
  （@google/genai 触发 skal JSC SIGSEGV，需 bun plugin 构建期内联 node-builtin）、
  会话列表 UI（Rust 侧索引）

### M3 —— 审批与产品化（~2-3 周）
- [~] `pi-bundle` 工具审批（2026-09-05 主线落地）：mutating 工具 execute 前过
  `approval_request` hostcall；Rust `approval.rs` policy 状态机（ask/auto，
  `{data_dir}/policy.json`，"always" 持久化）+ pending 表 + unified diff；
  DiffApproval UI（Deny/Always/Allow）；写前备份 + `workspace_revert` 回滚命令。
  本机测试（cargo test + approval-test.js）✅；真机验证 ✅（2026-09-06：
  弹卡 → Allow → 写入 + 备份；Always 持久化 auto；重置恢复 ask）。
  edit 工具已接入同一审批点（2026-09-06，含 edit diff）。
  Android bash 通道（M4 前置）待做。
- [~] 会话列表/索引（Rust 侧）✅（2026-09-06：session_list/open/new + 会话抽屉 UI）；
  文件树 + 只读预览 ✅（2026-09-06：workspace_tree/read + 📁 面板）；
  命令面板、用量成本可视化待做
- [ ] AGENTS.md（pi 原生支持，随 workspace 生效）；Provider OAuth + deep-link（D9）
- [ ] 产品化基线：i18n / 无障碍 / 深色模式；Android APK 内测分发；checkpoint/恢复兜底（D8）
- **出口条件**：真机日常使用一周，会话/凭证/审批全部可靠；agent 完成"改文件 → 审批 → diff 可回滚"闭环

### M4 —— 平台深化与生态：MCP + Skills（~3-4 周）
- [~] **MCP**：扩展能力层已就位（2026-09-06）；等价实现采用 bundle 内
  `@modelcontextprotocol/client` 仅 streamable-http（stdio 的 cross-spawn/
  native 依赖在 JSC 不可用），先 spike SDK 兼容性（吸取 @google/genai
  SIGSEGV 教训）+ McpSettings 界面 + `mcp__` 审批分类（D11）
- [~] **插件能力层**（2026-09-06 起，替代直接嵌入 npm:pi-* 扩展——上游交互
  层绑死 pi-tui 终端，移动原生化策略见 PROGRESS 2026-09-06 02:20 条目）：
  pi-ask-user ✅；pi-subagents（delegate 工具）、@devkade/pi-plan、pi-goal、
  pi-btw 按命令面板进度跟进
- [ ] **Skills**：git/URL 安装器（git2-rs + checksum pin）+ SkillsManager + system prompt 注入（D12）
- [ ] Android 前台服务保流；SAF 打开外部目录；bash 命令黑名单兜底（D6）
- [ ] 通知：审批请求、长任务完成
- **出口条件**：接入 1 个真实 MCP 服务器端到端；安装 1 个真实 skill 并影响 agent 行为；锁屏/切后台不丢流

### M5 —— iOS、桌面同构与桥优化
- [x] **iOS 开工（2026-09-06）**：`tauri ios init`（gen/apple）+ 双 iOS 目标编译绿
  + 模拟器构建/安装/启动验证；agent 运行时 iOS 门控（init 返回明确错误，
  其余能力全量可用）。真机自动签名配置进 project.yml（DEVELOPMENT_TEAM）。
- [ ] iOS `libpi_bun.a` 静态库：从源码构建 WebKit JSC（skal `build-jsc-ios.sh` +
  `link-skal-ios.sh` 工艺，见 LIBPI-BUN-NOTES §2）+ pi_bun 模块静态链接路径
  + 审核合规评估（嵌入式 JIT，见风险表）——本项为 M5 关键路径
- [ ] 桌面 Tauri 同构验证（同一 libpi-bun 跑 darwin，桌面作开发调试宿主）
- [ ] v2 桥：skal 式零拷贝共享内存环（接口不变）
- [ ] Backlog 排期：git 远程工作流 / Share Sheet / 会话云同步 / 端侧小模型（见 §10.2）

---

## 7. 风险清单

| 风险 | 等级 | 缓解 |
|------|------|------|
| **bun fork/补丁链维护成本**（skal 同款挑战：zig 构建、上游 bun 月度发版、补丁冲突） | 高 | 锁定 bun 版本；补丁最小化（只加 platform-lib 入口与链接配置）；`setup-bun-fork.sh` 全自动可复现；每月跟进上游 rebase 作业 |
| iOS 审核（嵌入式 JS 运行时） | 中 | **skal 已验证路径**：JSC interpreter + bytecode cache（无 JIT）合规，React Native 同款先例；静态库 + 只执行打包 bundle；TestFlight 先行验证 |
| zig/android-ndk/gradle 构建管线复杂 | 中 | `build-libpi-bun.sh` 一键产出；CI 缓存 zig/gradle；产物哈希入库 |
| libpi-bun 与 App 沙箱的文件系统语义差异（bun 假设 $HOME、/tmp） | 中 | 启动时由宿主注入 HOME/TMPDIR=app_data 子目录；PoC 阶段验证 |
| pi 上游破坏性变更 | 中 | 锁定版本 + 契约测试 + 每周 canary 升级作业 |
| UI 长会话渲染内存（聊天流虚拟滚动） | 中低 | 分页渲染、虚拟滚动、提前触发 pi 自带上下文压缩 |
| Android bash 通道被 ROM/SELinux 收紧 | 低 | 审批 + 黑名单兜底；失败按工具错误返回 |
| gen/ 双平台工程漂移 | 低 | gen/ 入库 + CI 双平台冒烟 |
| MCP / skills 供应链与安全 | 中 | 仅 https + per-server 启停 + MCP 工具默认 ask 审批 + skills 版本 pin/checksum、不装当天发布版本（D11/D12） |

---

## 8. 测试与 CI

- **单元**：`bun test`（pi-bundle 桥绑定、policy、UI store）+ `cargo test`（pi_bun 装载、桥协议、会话索引）
- **契约**：tests/contract 双侧跑同一组 IPC fixture，schema 漂移即 fail
- **E2E**：maestro 驱动两平台模拟器冒烟（新建会话→对话→审批→恢复）
- **CI**（GitHub Actions）：macOS runner 出 iOS；Ubuntu + JDK 21 出 Android；桌面三平台矩阵
- **金标准回放**：录真实 pi 会话 JSONL 作 fixture，断言移动端工具执行结果与桌面一致
- **MCP/skills**：用本地 mock MCP 服务器（streamable-http）跑工具调用往返；skills 安装器用 fixture 仓校验 checksum pin / 拒装当天版本逻辑

---

## 9. 立即行动清单（M0）

```bash
cd /Users/sternelee/www/github/pi-mobile
bun install
# tauri.conf.json: beforeDevCommand / beforeBuildCommand 由 pnpm 改为 bun
bun tauri android init
bun tauri ios init
bun tauri android dev    # Android 模拟器
bun tauri ios dev        # iOS 模拟器
```

环境已验证：bun 1.3.14 / rustc 1.97.1 / Xcode 26.6 / JDK 21 —— 满足 Tauri mobile 全部前置条件。

---

## 10. 二次审查（2026-09-04）：遗漏修正与扩展方向

### 10.1 已纳入主路线的遗漏项（本次修正落点）

| 遗漏项 | 落点 |
|--------|------|
| Provider OAuth 登录回调（移动端无 localhost redirect） | 新增 D9；M2 |
| `globalThis.fetch` → plugin-http 适配点；SSE 在老 WebView 的降级路径 | 新增 D10；M1 |
| Android WebView 碎片化 | 风险表新增；D10 定 baseline |
| AGENTS.md / 项目规则注入 | M2 |
| 首启 onboarding 向导 | M1（并入最小聊天流） |
| 用量/成本可视化 | M3 |
| i18n / 无障碍 / 深色模式基线 | M3 |
| 更新通道（桌面 updater / mobile 分发） | M3 |

### 10.2 扩展方向 Backlog（M4 之后按价值/成本比评估，不承诺时间）

| # | 方向 | 价值 | 依赖/成本 | 备注 |
|---|------|------|-----------|------|
| 1 | **git 远程工作流**：clone / pull / push（token 凭证）+ GitHub API 建 PR | 高 —— 手机升级为完整 repo 终端 | git2-rs HTTPS + 凭证服务扩展 | 打通后 #2、#4 顺带受益 |
| 2 | **Share Sheet / URL scheme 接入**：浏览器分享 GitHub repo → 一键 clone 成 workspace | 高 —— 最短获客路径 | #1；Android intent filter / iOS Share Extension | 移动端独有入口 |
| 3 | **语音 + 图片输入**：键盘听写/Whisper 转写、相册截图 → vision 模型 | 中高 —— 移动端独有交互 | 中 | pi-ai 已支持多模态消息 |
| 4 | **会话云同步**：iCloud（iOS）/ 私有 git 仓（跨平台） | 中 —— 多设备接续 | 低-中；JSONL 格式天然适合 | "桌面开题手机续跑"的自动版 |
| 5 | ~~MCP 支持~~ **已提升至主路线（D11，M4 落地）**：仅 HTTP/SSE transport | — | — | 详见 D11/M4 |
| 6 | **端侧小模型**（iOS MLX / llama.cpp 经 Rust）：会话标题、摘要、上下文压缩 | 中 —— 省 token 费用 | 重 | 隐私友好 |
| 7 | **WASM 工具沙箱**（wasmtime）：iOS 上的"bash-lite"，受限命令集 | 中 —— 补 D6 短板 | 重 | 审核风险需先评估 |
| 8 | **Companion 模式**（pocket-term 启发）：显式连接桌面 pi RPC 干重活，手机审阅 | 中 —— 重任务体验 | 中 | 可选非默认，不违背 G2（用户显式开启） |
| 9 | 团队 prompt / skill 库共享（社区技能包目录 + 订阅更新） | 低-中 | 依赖 D12 落地 | 远期 |

### 10.3 明确不做（维持并强化非目标）

- 自动后台常驻 agent（系统限制 + 隐私 + 电量）
- 端侧训练 / 微调
- 自建 Provider 网关 —— 桌面 `pi-proxy` 已覆盖该需求，App 直接连


