# pi-mobile 项目规划

> 在 Android / iOS 上运行的 Pi Coding Agent —— Tauri 2 + Bun + SolidJS 架构方案

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
| G2 | 本地运行 Pi agent loop | 复用 `pi-ai` + `pi-agent-core`，不依赖远端服务器 |
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
| Agent 核心 | **`pi-ai` + `pi-agent-core`**（npm 直接依赖） | 官方分层就是为嵌入设计的；pi-ai 明确支持浏览器运行 |
| JS 工具链 | **Bun**（workspace / test / build） | 本机 bun 1.3.14；与上游 pi-mono 的运行时假设一致 |
| 宿主服务 | **Rust（Tauri commands）** | 文件、搜索、凭证、HTTP 代理、会话索引全部宿主侧实现，JS 无原始权限 |
| 凭证 | `tauri-plugin-keyring` 或 `keyring` crate | Keychain / Keystore 抽象 |
| 网络 | `tauri-plugin-http` | WebView 内直连 LLM Provider 有 CORS 限制，走 Rust fetch 代理 |
| 存储 | `tauri-plugin-fs`（限定 scope）+ 自研 workspace 命令 | 路径校验、能力声明（对应 pocket-pi 的 storage roots） |
| 默认内置工具 | `tauri-plugin-http` / `-fs` / `-opener` / `-os` | 作为 pi 默认 tool 的原生执行层，见 D2.1 |
| MCP | `@modelcontextprotocol/sdk`（浏览器 streamable-http）+ git2-rs 拉取 | 仅 HTTP 协议，见 D11 |
| Skills | 自研 registry + git2-rs 安装器 | SKILL.md 注入式技能包，见 D12 |
| 用户配置 | `tauri-plugin-store` | JSON KV + autosave；键空间入契约，见 D13 |
| （远期）嵌入式运行时 | **libpi-bun**（skal 式 bun 静态库） | 见 D1 方案 C |

---

## 3. 总体架构

```
┌─────────────────────────────────────────────────────────┐
│  WebView（系统 WebView: Android WebView / iOS WKWebView） │
│                                                          │
│  SolidJS UI          pi Agent Core（同窗口 JS 线程）      │
│  ┌────────────┐      ┌──────────────────────────────┐   │
│  │ 会话列表     │      │ pi-ai        (LLM Provider) │   │
│  │ 聊天流      │◄────►│ pi-agent-core(agent loop)    │   │
│  │ diff 审批    │ 事件 │   └─ 工具适配层 tool-bridge  │   │
│  │ 文件树/编辑器 │      │      (pi tool 契约 → IPC)   │   │
│  └────────────┘      └──────────────┬───────────────┘   │
└─────────────────────────────────────┼───────────────────┘
                        Tauri IPC (invoke / event)
┌─────────────────────────────────────▼───────────────────┐
│  Rust 宿主（可信机制持有方，pocket-pi 范式）               │
│  ┌──────────────┬──────────────┬────────────────────┐   │
│  │ workspace 服务 │ 工具执行器     │ 凭证服务            │   │
│  │ 路径校验/沙箱   │ read/write/  │ Keychain/Keystore  │   │
│  │ 会话 JSONL 存储 │ edit/grep/   │ (keyring crate)    │   │
│  │ 会话索引       │ glob/ls/bash*│                    │   │
│  ├──────────────┼──────────────┼────────────────────┤   │
│  │ http 代理     │ 事件总线       │ 权限/审批策略        │   │
│  │ (plugin-http) │ (emit/listen) │ 工具白名单+确认门    │   │
│  └──────────────┴──────────────┴────────────────────┘   │
│  gen/android (Kotlin 壳)   gen/ios (Swift 壳)           │
└─────────────────────────────────────────────────────────┘

* bash：Android 有限支持 / iOS 降级为无；见 D6
```

**核心原则（继承 pocket-pi）**：

1. **宿主持有信任**：凭证、网络、存储根、工具执行全在 Rust；WebView 内 JS 只有通过 IPC 声明式获取的能力。
2. **人与 agent 共享同一动作面**：文件树/编辑器的人工操作与 agent 工具调用走同一套 workspace 命令，UI 上两者可互相审计。
3. **上游即真源**：不 fork pi-mono；通过 npm 依赖 + 薄适配层嵌入，适配层有独立契约测试。


---

## 4. 目录结构规划

```
pi-mobile/
├── docs/
│   ├── PLAN.md                  # 本文档
│   └── CONTRACTS.md             # IPC 契约（命令/事件 schema，M1 产出）
├── package.json                 # bun workspace 根
├── vite.config.ts
├── src/                         # SolidJS 前端 + agent 宿主 JS
│   ├── App.tsx
│   ├── ui/                      # 纯 UI 组件
│   │   ├── SessionList.tsx
│   │   ├── ChatStream.tsx       # 流式 token 渲染
│   │   ├── DiffApproval.tsx     # 工具调用审批（write/edit/bash）
│   │   ├── FileTree.tsx
│   │   ├── McpSettings.tsx      # MCP 服务器管理：URL/headers/启停/工具目录（D11）
│   │   ├── SkillsManager.tsx    # skills 安装/更新/启停/删除（D12）
│   │   └── Editor.tsx           # 只读/受限编辑器（M3）
│   ├── agent/                   # pi 嵌入层（唯一允许依赖 pi 包的地方）
│   │   ├── runtime.ts           # 启动 pi-agent-core，注入 host tools
│   │   ├── tools.ts             # pi tool 契约 → tauri invoke 适配实现
│   │   ├── sessions.ts          # 会话加载/保存/分支
│   │   ├── providers.ts         # pi-ai provider 注册 + 凭证注入
│   │   ├── mcp.ts               # MCP streamable-HTTP 客户端与服务器注册（D11）
│   │   └── skills.ts            # skills 加载与 system prompt 注入（D12）
│   ├── bridge/client.ts         # 类型安全 IPC 封装（invoke/event）
│   └── state/                   # signals / stores（settings 经 plugin-store 持久化，D13）
├── src-tauri/
│   ├── src/
│   │   ├── lib.rs               # Tauri builder + 插件注册
│   │   ├── commands/            # workspace / search / session / creds / shell / mcp / skills
│   │   ├── workspace/mod.rs     # WorkspaceGuard：路径规范化 + 沙箱校验
│   │   └── policy.rs            # 工具审批策略状态机
│   ├── capabilities/            # tauri 能力声明
│   └── tauri.conf.json
├── gen/                         # tauri android/ios init 生成（入库）
│   ├── android/  └── ios/
└── tests/
    ├── contract/                # IPC 契约测试（Rust ↔ TS 双侧）
    └── e2e/                     # 模拟器 E2E
```

---

## 5. 关键设计决策

### D1：agent 运行在哪里？（最重要的架构决策）

| 方案 | 描述 | 优点 | 代价 | 结论 |
|------|------|------|------|------|
| A. **WebView 内运行**（M1 采用） | `pi-ai` + `pi-agent-core` 随 Vite 打包进 WebView，工具经 IPC 委托 Rust | 零自定义运行时；pi-ai 官方支持浏览器；浏览器 devtools 直接调试；桌面/移动同构 | pi-agent-core 若 import `fs/child_process` 需适配层替换；WebView 挂起即丢内存态（会话持久化兜底） | ✅ M1-M4 |
| B. 远程 RPC（pi server 跑桌面，手机做客户端） | 手机仅 UI | 上游能力 100% | 违背离线目标 G2；依赖桌面在线 | ❌（仅留作调试后门） |
| C. **嵌入式 bun 库**（skal 式 libpi-bun） | zig 交叉编译 bun → aarch64 静态库，Rust FFI 加载，`pi-coding-agent` 原样跑 | 上游 100% 原生行为（含 extensions、bun API）；无 WebView 内存压力 | 构建链路重（skal 用 fork 的 bun + 补丁）；iOS 审核对嵌入式 JIT/FFI 的风险 | ⏳ M5 评估 |

**推荐路径**：A → C 渐进。`src/agent/tools.ts` 定义稳定的 host tool 契约；方案 C 落地时契约不变、只换执行端。

### D2：工具桥协议（pi tool → Tauri command）

| pi tool | Tauri command | 事件回流 |
|---------|---------------|----------|
| read | `workspace_read {path}` | — |
| write | `workspace_write {path, content}` | `tool:approval-required` |
| edit | `workspace_edit {path, old, new}` | 同上 |
| grep / find / ls | `search_grep` / `search_glob` / `workspace_ls` | — |
| bash | `shell_exec {cmd}`（仅 Android + 白名单） | 审批 + `shell:stdout` 流 |

- 全部命令/事件有 JSON Schema（`docs/CONTRACTS.md`），Rust 与 TS 双侧生成类型 + 契约测试防漂移。
- **审批流**（`policy.rs` 状态机）：`auto`（read/grep/ls）→ `ask`（write/edit，弹 diff 审批卡）→ `deny`（bash 默认拒绝）。可按会话/全局调整。

### D2.1 默认内置工具：Tauri 官方插件作为原生执行层

pi 的默认 tool 不逐个自写 Rust command，而是以 **Tauri 官方插件为原生执行层**，由 `src/agent/tools.ts` 适配层包装成 pi tool 契约并套审批策略：

| pi 默认 tool | 原生实现 | 说明 |
|--------------|----------|------|
| read / write / edit / ls | `tauri-plugin-fs` | scope 限定 workspace 根目录；适配层做 pi 契约包装 + 审批门 |
| web_fetch（pi 的网页抓取） | `tauri-plugin-http` | Rust reqwest，绕 WebView CORS，与 LLM 流量同一代理通道 |
| open（打开 URL/文件/分享） | `tauri-plugin-opener` | URL → 系统浏览器；文件 → Android Intent / iOS 预览器 |
| os_info（平台环境信息） | `tauri-plugin-os` | platform / arch / version；同时注入 system prompt 上下文，模型无谓调用时可省一轮工具往返 |

**分层原则**：插件管"怎么执行"（原生能力、平台差异），适配层管"pi 怎么看"（tool 契约、schema、审批分类），capabilities 管"允不允许"（fs scope、权限白名单）——三层各司其职，替换执行端（D1 方案 C）时只动插件层。

依赖与注册已随规划完成接线（package.json / Cargo.toml / lib.rs / capabilities/default.json）；fs 的细粒度 scope（限定 workspace）在 M1 与 WorkspaceGuard 一起收紧为双保险。

### D3：会话持久化

- 兼容 pi 的 JSONL 会话格式，存 `app_data_dir/sessions/<id>.jsonl`；Rust 维护索引（标题/模型/时间）。
- 桌面 pi 会话可导入 → "桌面开题、手机续跑"。

### D4：凭证与安全

- API Key 只存 iOS Keychain / Android Keystore（`keyring` crate），JS 侧永不明文持有：pi-ai 初始化时由 `creds_get` 命令注入内存。
- `tauri.conf.json` 开启 CSP；capabilities 最小化白名单；fs 插件 scope 限定 workspace 根（与 WorkspaceGuard canonicalize+prefix 校验形成双保险）；workspace 路径防逃逸 —— 对应 pocket-pi "storage roots 只在宿主"原则。
- LLM 流量走 `tauri-plugin-http`（Rust reqwest），绕开 WebView CORS，统一日志与重试。

### D5：流式渲染

- pi-ai 的 SSE/delta 在 JS 内直接消费；SolidJS batch + 虚拟滚动应对高频 token 更新。
- Rust→UI 只传工具事件与审批请求，token 流不过 IPC（避免 IPC 成为瓶颈）。

### D6：bash 工具的平台现实

- **iOS**：系统禁止 fork/exec —— v1 不注册 bash 工具，工具目录对模型声明不可用（pi-agent-core 工具目录可配置）。
- **Android**：API 29+ 禁止 exec app-data 内二进制（W^X）。v1 同样不提供，靠 read/write/edit/grep 组合（多数代码任务可行）；M4 评估内嵌 toybox 到 nativeLibraryDir（Termux 模式，实验 flag，有审核风险）。
- 补偿：M3 提供 `git` 子集（Rust 侧 git2-rs：status/diff/commit/branch），工作区默认 `git init`，agent 写操作全部可回滚。

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

### D10：fetch 适配与流式降级（二次审查补充）

- pi-ai 在浏览器端用的是全局 `fetch`；适配层启动时把 `globalThis.fetch` patch 为 `@tauri-apps/plugin-http` 的 fetch（接口兼容、走 Rust reqwest）—— 一处 patch 同时覆盖 LLM 流量与 web_fetch 工具，并保留 AbortController/重试语义（取消 = UI 停止按钮）。
- SSE 流式依赖 fetch ReadableStream，Android System WebView 版本碎片化是真实风险：定最低 baseline（WebView/Chrome ≥ 120），CI 加老版本模拟器冒烟；降级路径 = Rust 侧流式代理（http 命令 + `llm:delta` 事件回传），适配层按能力探测自动切换。

### D11：MCP 接入（仅 HTTP 协议）

- **传输**：Streamable HTTP（MCP 规范 JSON-RPC over POST + SSE 事件流），兼容旧 HTTP+SSE；**stdio 不做**——两平台均无子进程（同 D6 结论）。JSON-RPC + SSE 天然复用 D10 的 fetch patch 与流式降级路径。
- **客户端位置**：WebView 内 JS（MCP 官方 SDK 的浏览器 streamable-http client），走 `globalThis.fetch` patch；移动网络切换频繁 → 断线重连、会话 resumable 由适配层处理，MCP 调用失败按工具错误返回，不阻塞 agent 主循环。
- **服务器注册**：设置界面添加 URL + 可选 headers；header 里的凭证经 creds 服务引用（配置文件只存引用，不明文）；配置存 `app_data/mcp.json`。
- **工具命名与审批**：`mcp__<server>__<tool>` 注册进 pi 工具目录；默认全部 `ask` 审批，用户可对单个 server 的只读工具降为 `auto`；每个 server 可独立启停。
- **安全**：仅 https；工具目录在 UI 完全可见可审计；上游 pi 的 MCP 能力是真源——若上游支持浏览器 transport，优先复用，适配层只做注册与策略。

### D12：Skills 管理

- **Skill 定义**：自包含技能包（SKILL.md 规范：指令 + 可选资源），作用是**注入 system prompt**（与 AGENTS.md 同层），不执行任意代码——包内脚本在移动端仅作参考文本注入，不运行（与 D6 一致的边界）。
- **存储**：`app_data/skills/<id>/`（内容）+ `registry.json`（来源 URL、版本、checksum、启停状态、作用域）；作用域分全局 / 单 workspace。
- **安装来源**：git URL（Rust git2 拉取）/ 直接 URL / 内置推荐目录；安装时版本 pin + checksum 校验，**参照 pi-mono 供应链纪律（不安装当天发布的版本）**。
- **管理界面**：安装 / 更新 / 启停 / 删除；禁用 = 不注入；删除后历史会话保留 id+version 引用记录。
- **成本联动**：启用 skills 的注入内容计入上下文预算，在 M3 用量可视化中可见。

### D13：用户配置存储（tauri-plugin-store）

- **职责边界**：`tauri-plugin-store`（JSON KV + autosave，存 `app_data_dir`）负责**用户配置**——UI 偏好（主题/语言/字号）、默认 Provider 与模型、审批策略基线（policy 默认值）、onboarding 完成标记、MVP 级杂项开关。
- **不放 store 的数据**（仍走宿主文件服务，因需 schema 校验/原子写/契约测试）：会话 JSONL（D3）、skills registry（D12）、mcp.json（D11）——store 只做"键值偏好"，结构化数据归契约层。
- **键空间入契约**：store 的全部 key 在 `docs/CONTRACTS.md` 登记（如 `settings.theme`、`settings.defaultModel`、`policy.default.*`），与 IPC schema 同等对待，防键名漂移；`src/state/settings.ts` 做类型安全封装，UI 只读 signal，写经统一 setter。
- **迁移**：key 结构变更走版本字段（`settings.version`），Rust/TS 两侧共用迁移表。

---

## 6. 里程碑路线图

### M0 —— 走通 Tauri mobile（~1 周）
- [ ] bun 接管 workspace：`bun install`；`tauri.conf.json` 的 beforeDevCommand/beforeBuildCommand 从 `pnpm` 改为 `bun`
- [ ] `bun tauri android init` + `bun tauri ios init` → gen/ 入库
- [ ] Android emulator / iOS simulator 跑通现有 SolidJS 模板
- [ ] CI 骨架：biome + typecheck + `cargo check` + 桌面 build
- **出口条件**：两平台模拟器显示模板 UI

### M1 —— 契约与骨架（~2 周）
- [ ] `docs/CONTRACTS.md`：全部 IPC schema；TS/Rust 双侧类型
- [x] 内置插件接线：http / fs / opener / os 的依赖、注册与 capabilities（已随规划完成，见 D2.1）
- [x] store 插件接线：用户配置持久化（见 D13）
- [ ] `src/state/settings.ts`：store 类型安全封装 + 键空间登记进 CONTRACTS.md + onboarding 标记
- [ ] fs scope 收紧到 workspace 根 + 适配层审批分类（read/write/edit/ls/web_fetch/open/os_info）落地
- [ ] **PoC（第一件事）**：扫描 `pi-agent-core` 对 Node/Bun API 的耦合面，必要时 vite alias 把 `node:fs` 等 shim 到 IPC
- [ ] Rust：WorkspaceGuard、workspace_read/ls、session_*、creds_*
- [ ] `src/agent/`：pi-ai / pi-agent-core 集成，host tools 接 read/grep/glob/ls
- [ ] `globalThis.fetch` patch → plugin-http + 流式能力探测/降级（D10）
- [ ] 最小聊天流：选 Provider → 填 Key → 对话 → 重启恢复（含首启 onboarding 最小向导：选 Provider → 填 Key → 建 workspace）
- **出口条件**：模拟器完成一次真实 LLM 对话并重启恢复

### M2 —— 写路径与审批（~2 周）
- [ ] write/edit 工具 + DiffApproval UI + policy 状态机
- [ ] 工作区 git init（git2-rs）+ 变更历史/回滚视图
- [ ] 会话导入导出（与桌面 pi 互认）
- [ ] AGENTS.md / 项目规则：随 workspace 打开读取并注入 system prompt（pi 核心概念，移动端不可缺席）
- [ ] Provider OAuth + deep-link 回调（tauri-plugin-deep-link，D9）
- **出口条件**：agent 完成"改文件 → 用户审批 → diff 可看可回滚"闭环

### M3 —— 编辑器与产品化（~2-3 周）
- [ ] 文件树 + 代码预览/受限编辑；grep 结果跳转
- [ ] 命令面板（模型/Provider 切换、上下文压缩、分支重放）
- [ ] git 子集（git2-rs）：status/diff/commit/branch，工作区默认 git init
- [ ] iOS TestFlight + Android APK 内测；checkpoint/恢复（D8）
- [ ] 用量与成本可视化：token 计量 / 费用估算 / 本地累计统计（移动端用户对成本更敏感）
- [ ] 产品化基线：i18n、无障碍（动态字号、VoiceOver/TalkBack）、深色模式
- [ ] 桌面自更新通道（tauri-plugin-updater）；mobile 走 TestFlight/APK 分发
- **出口条件**：真机日常使用一周，会话/凭证/审批全部可靠

### M4 —— 平台深化与生态：MCP + Skills（~3-4 周）
- [ ] Android 前台服务保流；iOS 后台任务最优利用
- [ ] SAF（Android）/ 文档选择器（iOS）打开外部目录为 workspace
- [ ] **MCP**：streamable-HTTP 客户端 + McpSettings 界面 + `mcp__` 工具注册与审批分类（D11）
- [ ] **Skills**：git/URL 安装器（git2-rs + checksum pin）+ SkillsManager 界面 + system prompt 注入（D12）
- [ ] Android bash 实验通道（toybox，feature flag）
- [ ] 通知：审批请求、长任务完成
- **出口条件**：锁屏/切后台不丢流；外部仓库可打开；接入 1 个真实 MCP 服务器完成端到端工具调用；安装 1 个真实 skill 包并影响 agent 行为

### M5 —— 运行时升级评估（探索）
- [ ] PoC：skal 式 zig 交叉编译 bun → libpi-bun（D1 方案 C）
- [ ] 若成功：`pi-coding-agent` 原样嵌入，适配层契约复用
- [ ] 不成功则固化方案 A，投入扩展生态（pi extensions 移动端子集）

---

## 7. 风险清单

| 风险 | 等级 | 缓解 |
|------|------|------|
| pi-agent-core 深度耦合 Node/Bun API（fs、process） | 高 | M1 第一件事做耦合面扫描 PoC；vite alias 把 `node:fs` shim 到 IPC |
| pi 上游破坏性变更 | 中 | 锁定版本 + 契约测试 + 每周 canary 升级作业 |
| WebView 内存压力（大上下文 + 长 session） | 中 | 分页渲染、虚拟滚动、提前触发 pi 自带上下文压缩 |
| iOS 审核（内嵌 LLM、JS eval） | 中 | v1 走 TestFlight；CSP 收紧；无动态代码下载 |
| Android bash 通道不可行 | 低 | 已降级为实验 flag，核心闭环不依赖 |
| gen/ 双平台工程漂移 | 低 | gen/ 入库 + CI 双平台模拟器冒烟 |
| Android System WebView 版本碎片化（流式 API 差异） | 中 | 最低 baseline WebView ≥ 120；老版本模拟器冒烟；Rust 流式代理降级（D10） |
| Provider OAuth 流程上游变更 | 低 | OAuth 限定 M2 起逐 Provider 开放；API Key 路径始终可用 |
| MCP / skills 供应链与安全 | 中 | 仅 https + per-server 启停 + MCP 工具默认 ask 审批 + skills 版本 pin/checksum、不装当天发布版本（D11/D12） |

---

## 8. 测试与 CI

- **单元**：`bun test`（agent 适配层、policy、UI store）+ `cargo test`（WorkspaceGuard、session 索引）
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


