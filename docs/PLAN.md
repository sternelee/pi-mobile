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
| Agent 核心 | **`pi-coding-agent`（完整版）** 经 libpi-bun 嵌入 | 上游 100% 原生行为：extensions、bun API、fs 全部可用，适配层最小化 |
| JS 工具链 | **Bun**（workspace / test / build） | 本机 bun 1.3.14；与上游 pi-mono 的运行时假设一致 |
| Agent 运行时 | **libpi-bun**（skal 工艺：zig 交叉编译 bun 为平台库） | **核心决策 D1 = 方案 C**；Android 动态库（.so，JNI 加载）/ iOS 静态库（.a） |
| 宿主服务 | **Rust（Tauri commands）** | 文件、搜索、凭证、HTTP 代理、会话索引全部宿主侧实现，JS 无原始权限 |
| 凭证 | `tauri-plugin-keyring` 或 `keyring` crate | Keychain / Keystore 抽象 |
| 网络 | `tauri-plugin-http` | WebView 内直连 LLM Provider 有 CORS 限制，走 Rust fetch 代理 |
| 存储 | `tauri-plugin-fs`（限定 scope）+ 自研 workspace 命令 | 路径校验、能力声明（对应 pocket-pi 的 storage roots） |
| 默认内置工具 | `tauri-plugin-http` / `-fs` / `-opener` / `-os` | 作为 pi 默认 tool 的原生执行层，见 D2.1 |
| MCP | `@modelcontextprotocol/sdk`（浏览器 streamable-http）+ git2-rs 拉取 | 仅 HTTP 协议，见 D11 |
| Skills | 自研 registry + git2-rs 安装器 | SKILL.md 注入式技能包，见 D12 |
| 用户配置 | `tauri-plugin-store` | JSON KV + autosave；键空间入契约，见 D13 |

---

## 3. 总体架构（D1 = 方案 C：嵌入式 bun 运行时）

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

### D1：agent 运行在哪里？——已定稿为方案 C（嵌入式 bun 运行时）

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
- iOS：`libpi_bun.a` 静态链接进 Swift 壳（JIT 合规约束 → M5 专项评估，静态 bundle + 禁动态代码下载为默认姿态）；
- 通信：C ABI —— **hostcall**（bun→Rust：凭证、审批、open、通知）+ **事件回调**（Rust→bun：审批结果、UI 指令）；Rust 核心同时把 agent 事件 emit 给 WebView UI。
- 桥分两级：v1 JSON 消息通道（可调试、够用）；v2 skal 式零拷贝共享内存环（接口不变，纯优化）。

### D2：桥协议（bun ↔ Rust ↔ UI）

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

### D13：用户配置存储（tauri-plugin-store）

- **职责边界**：`tauri-plugin-store`（JSON KV + autosave，存 `app_data_dir`）负责**用户配置**——UI 偏好（主题/语言/字号）、默认 Provider 与模型、审批策略基线（policy 默认值）、onboarding 完成标记、MVP 级杂项开关。
- **不放 store 的数据**（仍走宿主文件服务，因需 schema 校验/原子写/契约测试）：会话 JSONL（D3）、skills registry（D12）、mcp.json（D11）——store 只做"键值偏好"，结构化数据归契约层。
- **键空间入契约**：store 的全部 key 在 `docs/CONTRACTS.md` 登记（如 `settings.theme`、`settings.defaultModel`、`policy.default.*`），与 IPC schema 同等对待，防键名漂移；`src/state/settings.ts` 做类型安全封装，UI 只读 signal，写经统一 setter。
- **迁移**：key 结构变更走版本字段（`settings.version`），Rust/TS 两侧共用迁移表。

---

## 6. 里程碑路线图（方案 C 形态：libpi-bun 为关键路径）

### M0 —— 走通 Tauri mobile ✅（收尾中）
- [x] bun 接管 workspace（tauri.conf.json 命令已改 bun）
- [x] `bun tauri android init` → gen/android 入库（**优先 Android**；ios init 顺延至 M5 前）
- [x] CI 骨架：biome + typecheck + cargo fmt/clippy/test + aarch64-android 交叉检查 + 桌面 build 矩阵（`.github/workflows/ci.yml`）
- [x] 插件接线：http / fs / opener / os / store（依赖、注册、capabilities）
- [ ] Android **真机**跑通模板（adb 已连 MEY-AN00 / arm64 / Android 16，`tauri android dev` 构建进行中）
- **出口条件**：真机显示模板 UI

### M1 —— libpi-bun PoC（~3 周，最高风险前置，skal 挑战复刻）
- [ ] `scripts/setup-bun-fork.sh`：vendor bun fork（参照 skal 补丁工艺），锁定版本，全自动可复现
- [ ] zig 交叉编译 aarch64-android → `libpi_bun.so`，一键脚本产物进 `gen/android` jniLibs
- [ ] Rust `pi_bun/` 模块：FFI 装载、生命周期、消息泵；宿主注入 HOME/TMPDIR=app_data 子目录（沙箱语义对齐）
- [ ] C ABI echo PoC：hostcall + 事件回调往返；真机 logcat 验证 bun 执行 hello-world JS
- **出口条件**：真机 logcat 出现嵌入式 bun 的 JS 执行输出；桥往返（JSON 通道）< 5ms

### M2 —— pi bundle 与最小聊天流（~2 周）
- [ ] `pi-bundle/entry.ts`：pi-coding-agent 官方 bundle 在 libpi-bun 内以 headless/RPC 模式启动
- [ ] `docs/CONTRACTS.md` 三份契约（UI↔Rust IPC / Rust↔bun 桥 / store 键空间）+ 双侧类型 + 契约测试
- [ ] creds 服务（keyring crate）+ hostcall 凭证注入；`src/state/settings.ts`（store 封装 + onboarding 标记）
- [ ] 最小聊天流 UI：onboarding（选 Provider → 填 Key）→ 对话 → 重启恢复（会话 JSONL 由 pi 原生落盘）
- **出口条件**：真机完成一次真实 LLM 对话并重启恢复

### M3 —— 审批与产品化（~2-3 周）
- [ ] `pi-bundle/policy-hook.ts`（pi extension）：write/edit/bash 审批上报 + DiffApproval UI + policy 状态机
- [ ] 会话列表/索引（Rust 侧）、文件树（fs 插件 scope 收紧至 workspace）、命令面板、用量成本可视化
- [ ] AGENTS.md（pi 原生支持，随 workspace 生效）；Provider OAuth + deep-link（D9）
- [ ] 产品化基线：i18n / 无障碍 / 深色模式；Android APK 内测分发；checkpoint/恢复兜底（D8）
- **出口条件**：真机日常使用一周，会话/凭证/审批全部可靠；agent 完成"改文件 → 审批 → diff 可回滚"闭环

### M4 —— 平台深化与生态：MCP + Skills（~3-4 周）
- [ ] **MCP**：pi 原生客户端（bun 内）HTTP transport + McpSettings 界面 + `mcp__` 审批分类（D11）；Android stdio 实验通道
- [ ] **Skills**：git/URL 安装器（git2-rs + checksum pin）+ SkillsManager + system prompt 注入（D12）
- [ ] Android 前台服务保流；SAF 打开外部目录；bash 命令黑名单兜底（D6）
- [ ] 通知：审批请求、长任务完成
- **出口条件**：接入 1 个真实 MCP 服务器端到端；安装 1 个真实 skill 并影响 agent 行为；锁屏/切后台不丢流

### M5 —— iOS、桌面同构与桥优化
- [ ] `bun tauri ios init` + iOS `libpi_bun.a` 静态库 + 审核合规评估（嵌入式 JIT，见风险表）
- [ ] 桌面 Tauri 同构验证（同一 libpi-bun 跑 darwin，桌面作开发调试宿主）
- [ ] v2 桥：skal 式零拷贝共享内存环（接口不变）
- [ ] Backlog 排期：git 远程工作流 / Share Sheet / 会话云同步 / 端侧小模型（见 §10.2）

---

## 7. 风险清单

| 风险 | 等级 | 缓解 |
|------|------|------|
| **bun fork/补丁链维护成本**（skal 同款挑战：zig 构建、上游 bun 月度发版、补丁冲突） | 高 | 锁定 bun 版本；补丁最小化（只加 platform-lib 入口与链接配置）；`setup-bun-fork.sh` 全自动可复现；每月跟进上游 rebase 作业 |
| iOS 审核（嵌入式 bun JIT、动态加载） | 中-高 | iOS 用静态库 + 只执行打包 bundle、禁动态代码下载；上架前 App Store 问询；TestFlight 先行 |
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


