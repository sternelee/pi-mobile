# quickjs-agent spike —— B 方案的可运行验证

> 「**薄 JS + 厚原生**」：pi-agent-core 的 Agent 类跑在 **QuickJS** guest 里，
> 模型传输（DeepSeek 一家）与全部工具实现都在 **Rust**。
> 背景与取舍见 [docs/POCKET-PI-NOTES.md](../../docs/POCKET-PI-NOTES.md)；
> 本目录是那份笔记 §4「建议的下一步」里那个 spike。

**这是一次实验，不是产品代码。** 它不进 CI、不进 App 构建、不改变 D1。
结论见文末「结论」。

## 它验证了什么

| # | 命题 | 结果 |
|---|---|---|
| 1 | 上游 `pi-agent-core` 的 `Agent` 类能在 QuickJS 里跑（非重写） | ✅ 真跑了：流式思考/文本、工具调用、多轮 |
| 2 | 模型传输可以整个搬到 Rust，JS 里不要 HTTP/provider 栈 | ✅ 316KB bundle，0 个 provider SDK |
| 3 | 工具可以直接**复用现有 Rust 实现**，不重写 | ✅ 与 Tauri 宿主同一份 `pi-host-tools` |
| 4 | 体积/冷启动是否可接受 | ✅ 见下表 |

## 结构

```
spikes/quickjs-agent/
├── js/
│   ├── prelude.js   QuickJS 缺什么补什么（实测清单在文件头）
│   ├── entry.js     agent 本体：Agent + 自定义 streamFn + 工具壳
│   └── build.sh     bun build --format=iife → dist/agent.js（+ 模块语法硬校验）
├── src/
│   ├── main.rs      CLI、循环、指标打印
│   ├── guest.rs     rquickjs 宿主：注入 host 面、tick + 泵微任务、度量
│   └── deepseek.rs  模型传输：请求编码 + SSE 解析（对齐 pi-ai openai-completions）
├── tools/
│   └── mock-deepseek.py  脚本化 SSE mock（无 key 也能端到端跑通）
└── crates/pi-host-tools（仓根）  抽出来的工具实现，Tauri 宿主共用同一份
```

数据流（与 pocket-pi 同构）：

```
Rust 主循环                            QuickJS guest
  │ boot(config) ─────────────────────────▶ __spike.boot
  │ prompt(text) ────────────────────────▶ __spike.prompt  → agent.prompt()
  │                                          └ streamFn → host.startModel(json) ─┐
  │ ◀──────────────────────────────────────────────────────────────────────────────┘
  │ 模型线程：HTTP + SSE ─→ 事件队列
  │ tick() ─────────────────────────────▶ __spike.tick（poll 取事件→喂流）
  │ execute_pending_job() ×N ──────────────▶ 跑微任务（await 继续）
  │ ◀── drain() 取 agent 事件 ──────────────┘
  │ 工具：host.callTool(name,args) ───────▶ pi-host-tools（同步执行）
```

## 跑起来

```bash
# 1. 打包 JS（bun build）
bash spikes/quickjs-agent/js/build.sh

# 2a. 不出网、不花钱的端到端（推荐先跑这个）
python3 spikes/quickjs-agent/tools/mock-deepseek.py 8899 &
DEEPSEEK_API_KEY=mock DEEPSEEK_BASE_URL=http://127.0.0.1:8899 \
  cargo run --release --manifest-path spikes/quickjs-agent/Cargo.toml -- \
  --prompt "Read notes.md and tell me what is in the workspace."

# 2b. 真打 DeepSeek
DEEPSEEK_API_KEY=sk-… \
  cargo run --release --manifest-path spikes/quickjs-agent/Cargo.toml -- --prompt "…"

# 可选参数：--model / --thinking / --workspace / --quiet
```

> mock 是**无状态**的：消息里没有 `role:"tool"` 就回一个 `read` 工具调用，有就回收尾
> 文本。所以同一句话可以反复跑，每次都会走完整的两轮 + 一次真实工具执行。

## 实测数字

macOS arm64 / release / 本地 mock（所以「首 token」是**纯开销**，不含网络）：

| 指标 | 值 |
|---|---|
| JS bundle（prelude + agent） | **316,378 B**；对照 `pi-bundle/dist/agent.js` **2,950,176 B** → **小 9.3×** |
| guest 冷启动（Runtime + prelude + bundle eval + boot） | **48 – 150 ms**（首次跑偏慢，热态 ~50ms） |
| QuickJS 堆 | **1.59 MB** 在用 / 1.90 MB malloc（bun 路线是 87MB 的 .so） |
| prompt → 首个增量 | **2 – 14 ms** |
| 工具调用（读 63B 文件，走 `pi-host-tools`） | **0.1 – 5 ms** |
| 整轮（2 次模型请求 + 1 次工具） | **63 – 161 ms** |
| token 计账 | 1800 in / 80 out（含 `prompt_cache_hit_tokens` 的 cacheRead 拆分） |

bundle 小的原因：**pi-ai 的 provider 栈整段被 tree-shake 掉**——JS 只 import 一个
`AssistantMessageEventStream` 类（事件流的队列实现），而 `@anthropic-ai/sdk`、
`@aws-sdk/client-bedrock-runtime`、`@google/genai`、`openai` 这些一个都不进图。

## 复用 Tauri 宿主的那份工具实现

工具实现从 `src-tauri/src/pi_bun/loopback.rs` 抽到 `crates/pi-host-tools`
（root 显式传入，不再读模块级 `OnceLock`），`loopback.rs` 保留同名转发：

- 签名与错误文案逐字不变 —— `src-tauri` 的 44 个测试全绿，含
  `pi_bun::loopback::tests::write_backup_and_revert_roundtrip`；
- crate 自带 2 个测试（越狱判定三类拒绝 + 备份/回滚/树/预览往返）；
- **没有任何一处 lint 债务增加**：`cargo fmt --check` 14 处、`cargo clippy -D warnings`
  27 个错误，与改动前的 HEAD 完全一致（这些存量问题见「已知问题」）。

## 与 pi-mobile 现有实现的差异（刻意为之）

| 维度 | bun 路线（`pi-bundle/agent-main.js`） | 本 spike |
|---|---|---|
| JS 引擎 | bun + JSC（87MB .so） | QuickJS（bundle 316KB + 引擎 ~1MB 级） |
| provider | 8 家 + OAuth + prompt caching | **1 家**（DeepSeek），Rust 手写协议 |
| 工具 | 经 loopback HTTP → Rust | **直接**调 Rust（同一份 `pi-host-tools`） |
| 会话 | pi `JsonlSessionRepo` + JSONL 落盘 | 无（内存态） |
| 审批 | approval.rs 状态机 + diff + 回滚 | 无（工具直通） |
| 循环驱动 | Rust eval + kick/poll | Rust tick + `execute_pending_job`（泵微任务） |
| 取消 | `agent.abort()` | 无（prelude 里的 AbortController 是空壳） |

前两项是本路线的**取舍**而非疏漏：provider 与产品层要么用 Rust 重写，要么就不要。

## 已知问题 / 未验证

- **未打真 DeepSeek**：本机没有 API key，真模型那一轮**没跑过**。mock 验证的是
  协议形状与整条链，不是 DeepSeek 的真实兼容性。跑法见上面 2b。
- **未在 iOS/Android 上构建**：rquickjs 是纯 C、无 JIT，跨平台编译预期简单
  （pocketjs 已在 iOS/Android 上跑过 QuickJS），但本 spike 只跑了 macOS。
  且 `rquickjs` **不要开 `bindgen` feature**：本机 PATH 里 NDK 的 clang 排在
  Apple clang 前面，bindgen 会拿 NDK 的 include 路径去找 `stdio.h` 而失败（实测）。
- **`transformMessages` 没实现**：pi-ai 在发请求前会做 provider 归一化（孤儿
  toolCall 修补、空 assistant 丢弃等）。本 spike 只做了三种角色 + 空 assistant 丢弃。
  长时间多轮后可能撞到边界。
- **流式 toolCall 的增量没有中途解析**：pi-ai 用 `partial-json` 让 UI 能提前看到
  正在生成的参数；这里只在 `finish_reason` 之后整体解析。UI 体感有差别。
- **无重试、无取消、无会话、无审批**：spike 边界内。
- **`cargo fmt`/`clippy` 的存量问题与本 spike 无关，但值得一提**：`main` 分支现状
  就有 14 处 fmt diff 与 27 个 clippy 错误（`git stash` 对照验证过），本目录零新增。
  更重要的是 **CI 已经连续多个提交全红**，且 `rust` job 卡在 `cargo fmt` 这第一步
  ——后面的 `clippy` 与 `test` 从未在 CI 上执行过。详见 `docs/PROGRESS.md`
  2026-09-19 条目的「顺带发现 ①」。

## 结论

三条命题都成立，且成本比预期低：

1. **QuickJS 能承载上游 agent**。pocket-pi 用 0.81、这里用 0.84.4，都不用改
   pi-agent-core 一行代码 —— 只要提供 `streamFn` 与工具壳。
2. **体积差的 9.3 倍全部来自 provider 栈**。这是「上游 100% 保真」在 bun 路线里
   的隐性账单：provider 目录、SDK monorepo、OAuth 流程都进了 bundle。
3. **工具这一半我们早就付过了**。抽 crate 只花了机械劳动，`loopback.rs` 的行为
   与测试完全不变 —— 说明 B 路线的真实增量只在「provider 传输 + 产品层」。

**但这不等于应该切 B。** 本 spike 未触及的正是 A 路线已经交付的东西：会话持久化、
审批与回滚、8 家 provider 的 quirk、OAuth 订阅登录。切 B 的实际工作量是
「用 Rust 重写 pi-ai 的传输层 + 重建产品层」，不是「换个 JS 引擎」。

下一步若要做，见 `docs/POCKET-PI-NOTES.md` §4 的三个方向与触发条件。
