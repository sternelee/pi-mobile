# 开发进度日志

> 持续更新。倒序记录，每条含日期、状态与下一步。

## 2026-09-20（第十三轮）— ✅ 真机：「配置完 key 一直停在 agent booting…」——qjs 把 boot 事件吃掉了

真机装上前一轮那个 release APK（`PI_AGENT_RUNTIME_DEFAULT=qjs`，40MB，无 libskal），
配好 DeepSeek + 模型之后界面**永远停在 `agent booting…`**（composer 灰着）。

### ① 根因：`Guest::start` 的热身 tick 把事件扔了

UI 的启动顺序是（`App.tsx` onMount）：

```
get_default_model → listen("pi-agent-event") → invoke("agent_init") → loadHistory()
```

也就是**监听先注册、boot 后发生** —— 所以 boot 期间发出的事件正是它等的那批。
而 qjs 这边：

```rust
for _ in 0..200 { guest.tick()?; std::thread::sleep(1ms) }   // ← 返回值直接扔了
```

`agent_ready`（UI 唯一用来解锁 composer 的信号）就在这批里被丢掉。**静态检查、
clippy、单测全绿都拦不住它** —— 因为「事件有没有到宿主」这件事没有任何东西在看。

### ② 同一类：`open_session` 的轮询也在吞事件

```rust
for event in self.tick()? { match ... }   // 只看自己等的那个，其余丢掉
```

那个窗口里可能夹着 model 增量、**`approval_required`** —— 后者被吞掉就是审批卡不弹、
agent 在另一头干等（「静静卡住」比报错难查得多）。

修法：加一个 `Guest::tick_and_emit()`（tick + 转发，与 worker 主循环同一条路），
boot 热身与 `open_session` 都走它。**规则写成注释钉在那里：任何「边 tick 边等某个
事件」的地方都必须转发。**

### ③ 顺手挖出两个连带缺口

| 缺口 | 症状 | 修法 |
|---|---|---|
| **boot 不恢复上次会话** | bun 路线在 bundle 顶部就 `restoreLatest()`；qjs 没有任何人调 `restore` → 重开 App 是空白对话（会话文件都在） | boot 后调 `restore()`；恢复是异步的，热身 tick 会推完 |
| JS `restore()` 补发**第二条** `session_restored`（无字段） | App 的处理器是 `setCurrentSession(ev.sessionId ?? null)` → 刚恢复出来的会话高亮**立刻被清掉**（spike 里补那句只是 CLI 要个完成信号） | 删掉：`restoreLatestSession()` 自己已经发了带 `found/sessionId/messages` 的那条 |

### 验证（新增了「跑得起来」的门）

| 验证 | 结果 |
|---|---|
| `qjs_globals_offline` | ✅ 0.44 s —— **新增断言：boot 期间 `agent_ready` 恰好 1 条、`session_restored` 恰好 1 条（`found:0`）** |
| `qjs_responses_mock_turn` | ✅ 0.43 s —— 新增 ④：`session_open` 重开刚落盘的会话，历史里仍有 user/assistant，且 `session_opened` 经事件到岸 |
| `qjs_live_turn`（真 DeepSeek） | ✅ 0.86 s |
| `cargo test` / clippy / fmt | ✅ 65（62+3 ignored）/ 31（HEAD 33）/ 15（HEAD 39） |
| bundle | 385,334 B |
| 新 APK | `app-universal-release.apk` 40 MB，sha256 `4dd2cd6684e2decb…`，16KB 对齐 ✓，签名 ✓，`PI_AGENT_RUNTIME_DEFAULT=qjs` ✓（cargo dep-info 记的） |

⚠️ **这一轮的真正教训是一条流程问题**：qjs 的这三个集成测试全都标着 `#[ignore]`
（`agent_init` 的 HOST/WORKER 是 `OnceLock`，一个进程只能 boot 一次），于是
`cargo test` 根本不覆盖 boot 路径 —— 而 qjs 出的两个 bug（boot 卡 30s、永远
booting）**恰好只有它们能发现**。所以新增 **`scripts/qjs-tests.sh`**：
一个测试一个进程地跑那两个离线的（不要 key、不要网络），`QJS_LIVE=1` 再带上真模型那轮。
**这是本轮唯一防止同类 bug 再犯的东西**，比任何注释都重要。

### ⏭ 仍未做（与上轮同）

`openai-completions` 泛化（openrouter 333 模型）／`anthropic-messages`／
`google-generative-ai`／codex 的 OAuth；真 OpenAI key 上的验证。

## 2026-09-20（第十二轮）— ✅ OpenAI **Responses** 传输落地；顺带修掉第三个同类坑（App 里 qjs 完全没有文件工具）

用户选的方向是「先做 openai-responses」（而不是 PROGRESS 原本列的 openai-completions），
理由是核对数据之后发现原来那条杠杆对 **UI 现有那 8 行**基本不成立：

| UI 那 8 行 | 模型数 | 真实 api 家族 |
|---|---:|---|
| openai | 38 | `openai-responses` ← 这一轮 |
| xai | 4 | `openai-responses` ← 这一轮 |
| openrouter | 333 | `openai-completions`（只覆盖到这 1 行） |
| deepseek | 3 | `openai-completions`（已实现） |
| google-gemini | 22 | `google-generative-ai` |
| anthropic / kimi-coding | 13 / 4 | `anthropic-messages` |
| openai-codex | 7 | `openai-codex-responses`（要 OAuth） |

工作量也不对称：`openai-completions.js` **1352 行**（8 种 `thinkingFormat` +
缓存/亲和/grammar/strict/deferred 一堆 quirk），`openai-responses.js` **286 行**。
所以先花小钱把 UI 第一行（openai）打通。

### ① 传输层改成**按 `model.api` 分派**（不是按 provider 名）

```rust
enum Transport { OpenAiCompletions, OpenAiResponses }   // qjs/catalog.rs
pub fn transport_for(model) -> Result<Transport, String>
```

一个家族实现一次就解锁一批 provider —— 之后「加一家 provider」只是目录里多一行。
`Err` 的两种情况分开表述（对用户的含义不同）：

- `anthropic-messages` / `google-*` / bedrock… → 「这一族没实现」；
- `openai-completions` 但 provider ≠ deepseek → 「这一族做了，但只做了 DeepSeek 的
  compat 档案」（openrouter 选模型时看到的就是这句）；
- `github-copilot` → 「要动态头 + OAuth，没做」——宁可说清楚，也不发一个必失败的请求。

### ② 新增 `src-tauri/src/qjs/openai_responses.rs`（≈400 行含测试）

逐条对齐 `openai-responses.js` + `openai-responses-shared.js`（pi-ai 0.84.4）：
input items（user / assistant→message+function_call / toolResult→function_call_output）、
**扁平** tool 形状、`store:false`、`max_output_tokens`（≥16）、
`reasoning.effort + summary:auto + include:[reasoning.encrypted_content]`、
`developer` vs `system` 角色、以及 `getSupportedThinkingLevels` / `clampThinkingLevel`
的**直译**（不做 clamp 会把 `high` 发给只认 `xhigh` 的模型 → 400）、
`mapStopReason` / usage（input 要减掉缓存读写）。

⚠️ 复核时抓到一处**只有对着上游才能发现**的分叉：serde_json 里「键缺失」与「键是
null」都取到 `Value::Null`，而 pi-ai 用的是 JS 的 `undefined`/`null` 区分 ——
低档位缺键**算支持**、显式 null 不算；`off` 缺键要发 `{effort:"none"}`、显式
null 则整个字段不发。今天能路由的模型里这处分叉**一个都没命中**（13 例全在没实现的
github-copilot 上），但已按上游语义改正并用单测钉住 —— 下一个 provider 就会踩到。

刻意不做的（每条都写进了模块头）：`prompt_cache_key` / 缓存保留、`service_tier`、
会话亲和头、copilot 动态头、grammar tools / strict schema 重写 / deferred tools、
图片输入、`textSignature`（我们的扁平结果契约没这个字段 → 回放时 message id 用 pi-ai
的兜底形态 `msg_pi_<n>`，phase 丢失，只影响 codex 系的 commentary/final_answer 区分）。
**多轮靠 `reasoning.encrypted_content` 回放**（store:false 的前提），这条通了。

### ③ 第三个同类坑：**App 里的 qjs 一个文件工具都没有**

`guest.rs` 的 boot config 漏了 `"tools"`，而 JS 侧是 `toolsFor(config.tools || [])`
——spike 一直在传（`tool_definitions()` 85 行），搬到 App 时漏了。后果：agent 手里
只有 fetch / todo / subagent / ask_user，**read/write/edit/ls/grep/mkdir/rm 全没有**。
前一轮的 M1–M3 表里那行「工具 → pi_host_tools」在 App 里其实不成立。

修法不是再抄一份：把工具表放进 **`pi-host-tools::tool_definitions()`**
（与 `run_tool` 的 dispatch 同一处），App 直接 `"tools": tool_definitions()`。
另加一个一致性测试（名字集合 = 7 个文件工具 + 每个都有 description 与 object schema），
因为 dispatch 是 `match name` 无法内省，改一处忘另一处就会再分叉。

### 验证（新增一个「单族验收」的测试形状）

| 验证 | 结果 |
|---|---|
| **新增** `qjs_responses_mock_turn`（本地 mock SSE，不要 key） | ✅ 0.44 s |
| `qjs_globals_offline`（8 个全局逐项） | ✅ 0.43 s |
| `qjs_live_turn`（真 DeepSeek 一整轮） | ✅ 0.90 s |
| `cargo test`（src-tauri） | ✅ 65 个（62 过 / 3 ignored）；新单测 11 个（responses）+ 2 个（catalog 分派） |
| `pi-host-tools` | ✅ 14 个（新增工具表一致性 1 个） |
| clippy | **31**（HEAD 33）——顺手清掉 `mut` 与死代码 `from_env` |
| fmt | **15**（HEAD 39）；`openai_responses.rs` / `catalog.rs` 全干净 |
| bundle | 未动（这轮纯 Rust），`agent-qjs.js` 385,376 B |

**`qjs_responses_mock_turn` 的价值**（本地 mock 起 SSE，把 baseUrl 指过去）：
它一次钉住三件真踩过的事 —— ① 换到 openai/gpt-5 之后请求体是 **Responses 形状**
（`input`/`store`/`reasoning`）而不是 completions 的 `messages`/`max_tokens`；
② **工具表真的传进了 guest**（漏传时这里就是空数组）；③ SSE 增量与终态真的以 UI
认的事件形状到岸。**一族的验收就应该长这样**：不需要真 key，但走完整条链。

顺带加的开发缝：`PI_<PROVIDER>_BASE_URL`（自建端点/ mock 用；旧名
`DEEPSEEK_BASE_URL` 只留给 DeepSeek，免得全局设了抢别家的端点）。

### ⚠️ 这轮没做 / 下一步

- **没在真 OpenAI key 上验过**（本机没有 key）—— 只有 mock 与单测。有 key 之后
  第一件事就是 `PI_OPENAI_API_KEY=… cargo test --lib qjs_responses_mock_turn` 换成真
  端点跑一轮（或直接在手机上选 openai）。
- `openai-completions` 泛化仍未做 → **openrouter（333 模型）还选不了**（会明确报
  「只做了 DeepSeek 的 compat 档案」）。那一族要决定是「正确性子集」还是逐字复刻。
- `anthropic-messages`（1074 行，覆盖 anthropic + kimi-coding）、
  `google-generative-ai`、codex（要 OAuth）都还没动。
- 跨家族的 `transformMessages`（孤儿 toolCall 修补、foreign id 归一）未移植：在
  responses↔completions 之间切 provider 时，历史里的 `call_x|fc_y` 会原样发给
  DeepSeek。低风险但记在案。

## 2026-09-20（第十一轮）— ✅ 修好 qjs 的**启动链路**与 **provider/模型/命令面**（真机报的两个问题）

用户反馈「qjs 还无法正常工作：未实现的全局调用 `__pi_models_refresh`」。查下去发现
**是两个 bug，而且第一个比报上来的那个更致命**。

### ① 致命：qjs 每次 boot 都白等 30 秒，最后报 `qjs boot timeout`

上一轮把 boot 结果改成「用 channel 如实回报」时，成功信号被写在了 `worker_main`
**返回之后**——而 worker 主循环是**常驻的**（正常路径永不返回）：

```
let result = worker_main(...);      // ← 永不返回（里面的 loop 常驻）
let _ = boot_tx.send(Ok(()));       // ← 到不了
```

后果很阴：**boot 必然超时，而 agent 其实是好的**（worker 线程活着，UI 照样能聊）。
所以它既不是「起不来」也不是「全部正常」，而是「启动报错 + 功能大半可用」——
`qjs_live_turn` 那个集成测试也因此一直是坏的，但它标了 `#[ignore]`，没人看见。

修法：信号移到 `Guest::start` 返回的那一刻（成功/失败都发），常驻循环之外；
worker 中途死掉仍然 `logcat` + `boot_error`。
**实测：0.36 s 起来（原来 30.0 s 超时）。**

### ② 用户报的那个：`pi_call_global` 只映射了 4 个名字，UI 调 8 个

| UI 调用 | 触发点 | 修前 | 修后 |
|---|---|---|---|
| `__pi_models_refresh` | 存 key / 模型快选 | ❌ 未实现 | ✅ 目录 → `models_listed` / `models_error` |
| `__pi_providers_list` | agent_ready / 抽屉 | ❌ 未实现 | ✅ 8 家 + 模型 → `providers_listed` |
| `__pi_model_current` | agent_ready | ❌ 静默 `.catch()` | ✅ Rust 里的当前模型 |
| `__pi_model_select` | 选模型 | ❌ 未实现 | ✅ 目录解析 + 热切换 → `model_applied` |
| `__pi_commands` | skills 之后 | ❌ 静默空 | ✅ 技能的 `/slug` 清单 |
| `__pi_oauth_login` | OAuth | ❌ 未实现 | ⛔ 明确报「未接」（不假装） |
| `__pi_skills_apply` / `__pi_goal_apply` | 热生效 | ⚠️ 空实现（"started"） | ✅ 真重读并重装 systemPrompt |
| `__pi_plan_start` / `__pi_btw_start` | /plan·/btw | ✅ | ✅ |

### 新东西：`src-tauri/src/qjs/catalog.rs`（目录终于有人用了）

`src-tauri/assets/models.json`（39 家 / 1290 模型 / 565 KB）上轮就生成了，**但仓库里
一处引用都没有** —— 这轮把它接上，作为 **Rust 侧的单一真源**：

- UI 的事件面（`providers_listed` / `models_listed`）与传输层
  （baseUrl / compat / thinkingLevelMap）**共用同一份**，不再有第二份抄本；
- 也不塞进 qjs bundle（551KB 会把它翻倍）—— 目录归 Rust 是体积上唯一划算的落点；
- `modelFor()` 里那份手抄的 DeepSeek 模型字面量退成兜底；boot 传给 JS 的
  `config.model` 现在是**目录解析出的完整对象**。

两个刻意的判断（都写进了注释）：

1. **展示名以 UI 为准**：`providers_listed` 会整份替换 UI 的静态清单，用 pi-ai 的名
   （"Anthropic API key" / "Google"）会让用户看到两套名字。目录只供 id 与模型。
2. **`google-gemini` → `google` 别名**：UI 的 id 是历史命名。bun 路线其实没做这个映射
   （`models.getProvider("google-gemini")` 取不到），那一行**一直是空的**。

### 传输边界：从「静默换一家」改成「明确报错」

上一轮 `guest.rs` 是：provider 不是 deepseek 就 **logcat 一句、静默回退 deepseek**
（"UI 里仍显示已选 provider"）。那是**最难查的一类不一致**：UI 显示 OpenAI，请求
实际打 DeepSeek。这轮改成两道门：

- `__pi_model_select`：没有传输的 provider → 直接报错（不返回 "started"）；
- `startModel`（每轮请求）：同样拒绝，错误直接进对话流；
- 顺带把凭证/baseUrl 按 `model.provider` 取（`PI_<PROVIDER>_API_KEY` /
  `creds::get(dir, provider)` / 目录的 `baseUrl`），为下一步泛化留好接口。

### 验证

| 验证 | 结果 |
|---|---|
| **新增** `qjs_globals_offline`（离线，`#[ignore]`） | ✅ 0.36 s：上面 8 个全局逐个走一遍，事件形状逐项核对 |
| `qjs_live_turn`（真 DeepSeek，`#[ignore]`） | ✅ **0.75 s**（修前必然 boot 超时） |
| `cargo test`（src-tauri） | ✅ 52 个（50 过 / 2 ignored），catalog 7 个新单测 |
| clippy | 33 = 改动前 33（**零新增**，`git stash` 对照） |
| fmt | 新文件 `catalog.rs` 干净；`guest.rs` 16 → **14** hunk（顺手少了两处存量） |
| bundle | `agent-qjs.js` 385,376 B（+175 B） |

⚠️ **教训**：boot 链路必须有一个**会跑的**测试看着。「改成如实回报」是个正确方向，
但它同时把成功路径写死了，而唯一能发现它的是个 `#[ignore]` 集成测试 ——
**等同于没有测试**。所以这轮把那条路径做成了默认就能跑的单元测试形状
（离线、不要 key），只把「真模型」留成 ignored。

### ⏭ 下一步（未变，仍是第十轮列的那两条）

1. **`deepseek.rs` 泛化成 `openai_completions.rs`**：按目录的 baseUrl / compat /
   thinkingFormat / 凭证取 provider —— 一次覆盖目录里 **26 个 provider**（现在
   选非 DeepSeek 会明确报错，就是因为这一步没做）；
2. 之后补 `anthropic-messages`（+10 provider，累计 74%）。

### 仍未接的（qjs 路线，与上轮同）

native 能力（9 个）/ preview / git（6 个）/ run_js 这四组工具**还没有 JS 壳**；
OAuth 订阅登录未接（现在会明确报错）。

## 2026-09-19（第十轮）— 🚧 **方向定了：保留 UI，用 QuickJS 换掉 bun**；后端落地，UI 零改动

用户明确目标：**采用 QuickJS 代替 Bun，但保留 Tauri WebView 的 UI/UX**。
所以这不是继续做 spike，而是**在 App 里换运行时**——等于 D1 的迁移开始落地。
仓库默认运行时仍是 bun（迁移期要能回退），打「默认走 QuickJS」的包用
`PI_AGENT_RUNTIME_DEFAULT=qjs`。

### 架构：新增 `qjs` 后端，与 `pi_bun` 并存

```
src/ (SolidJS, 5014 行)  ← 一行不改
      ↕ Tauri IPC：39 个命令 + pi-agent-event 事件流  ← 契约不变
src-tauri/
  ├─ pi_bun/   A 路线：dlopen libskal + loopback HTTP hostcall
  └─ qjs/      B 路线：rquickjs guest + **进程内**调用既有服务  ← 新增
        ├─ mod.rs     worker 线程 + 命令面 + 运行时开关 + 事件汇
        ├─ guest.rs   挂 host.*：审批→approval.rs / 提问→ask_user.rs /
        │             工具→pi_host_tools / 会话 fs→pi_host_tools::fs_op /
        │             目标技能 MCP→goal.rs/skills.rs/mcp.rs
        └─ deepseek.rs 模型传输（**换引擎的真实代价就在这**，见下）
pi-bundle/agent-qjs.js   QuickJS 版 agent 入口（由 spike 的 entry.js 演化，385KB）
```

运行时开关三级：`PI_AGENT_RUNTIME` 环境变量 → `{data_dir}/runtime.txt` → 编译期默认
（`PI_AGENT_RUNTIME_DEFAULT`，缺省 bun）。**`pi_bun` 的 9 个入口内部按开关分派**，
所以 lib.rs 与 UI 完全不用动。

### 已落地（M1–M3）：对话 / 工具 / 审批 / 会话 / 插件全在

| 能力 | 走哪个既有服务 |
|---|---|
| 流式对话 | pi-agent-core 的 Agent + 自定义 streamFn → Rust 传输 |
| 工具（read/write/edit/ls/grep/mkdir/rm） | `pi_host_tools`（与 bun 路线同一份） |
| **审批** | `approval.rs`：auto 放行 / ask 弹**既有审批卡 + diff + always**；执行权仍由 Rust 判（callId 握手，JS 绕过无效） |
| 会话持久化 | `JsonlSessionRepo` + `pi_host_tools::fs_op`（同一份，格式互通） |
| todo / subagent / 压缩 / plan / btw | agent-qjs.js（纯 JS，事件名对齐 UI） |
| goal / skills / MCP / ask_user | `goal.rs` / `skills.rs` / `mcp.rs` / `ask_user.rs` |
| 事件流 | **原样转发** pi-agent-core 的事件（UI 认的就是原始形状） |

### 验证

- **集成测试** `qjs_live_turn`（真 DeepSeek，标 `#[ignore]` 手工跑）：
  1.1s 跑完一轮，事件序列
  `agent_start → turn_start → message_start → message_update×3 → message_end → turn_end → agent_end`
  —— 正是 `src/lib/events.ts` 认的那套。
- **真机（MEY-AN00）**：release APK **39.6MB**（只有 `libpi_mobile_lib.so` 35.9MB，
  **没有 92MB 的 libskal**）装上后走 qjs，UI 起在了 provider 配置页（数据被卸载清掉）。
  对照：旧 debug 版 619MB。

### 途中修的四个问题（都是真跑才暴露的）

1. `agent_end` 重复：JS 里补的合成事件 + agent 自己发的那条 → 去掉合成的。
2. **凭证被当成 boot 硬门槛**：没 key 就 boot 失败，与 bun 路线（先起、UI 显示配置页、
   存完 key 就能用）不一致 → 改成**每次模型请求现读凭证**，存完 key 立即可用。
3. **真实 boot 错误被「agent_init 超时」盖住**（真机日志里只看到 timeout）→ boot 结果
   用 channel 如实回报。
4. （spike APK 里的）`Button` 继承 `TextView` 自带 `append()`，把日志写进了按钮文字。

### ⚠️ 换引擎的真实代价：provider 传输必须 Rust 重写

回答了「既然 QuickJS 能跑 pi-agent-core，为何不继续集成 pi-ai」：**pi-agent-core
设计上就是宿主无关的**（streamFn 与 tools 都是注入点，它自己不碰网络），所以能原样跑；
而 **pi-ai 的传输层要的是「一整个 Web/Node 平台」**：

- 依赖 4 家厂商 SDK（`openai` / `@anthropic-ai/sdk` / `@google/genai` /
  `@aws-sdk/client-bedrock-runtime`）+ smithy + proxy-agent；
- 这些 SDK 在 bun bundle 里对 **node 内建有 68 处调用**（fs/stream/https/net/crypto/
  buffer/util/os/http/events），bun 路线靠 `node-stdlib-browser` + 手写 `__require`
  垫片兜住（垫片里取不到的直接 throw）；
- QuickJS 既没有模块系统，也没有 fetch/ReadableStream/TextDecoderStream ——
  **这正是它 1MB vs bun 87MB 的原因**，补回来等于把 bun 已经给的东西再实现一遍。

**但 pi-ai 里有一半能集成**，而且已经在用：

| pi-ai 的组成 | 能否复用 | 现状 |
|---|---|---|
| 模型目录（39 provider / 1290 模型 / 551KB） | ✅ **纯数据** | 已生成 `src-tauri/assets/models.json`（`scripts/gen-models-catalog.py` 可重跑） |
| 协议编解码（convertMessages / parseChunkUsage / mapStopReason / thinkingFormat） | ✅ 纯函数 | 已在 `qjs/deepseek.rs` 逐字段复刻（对齐 openai-completions.js） |
| SDK 传输（streamSimple 那层） | ❌ | 必须 Rust 重写 |

**杠杆点（按目录统计）**：`openai-completions` 一族覆盖 **653/1290 个模型、26/39 个
provider**，而现有传输本来就是这种客户端，只是把 baseUrl/provider 写死了 →
泛化它 = 一次拿下过半 provider；再加 `anthropic-messages`（296 模型 / 10 provider）
就是 **74%**。剩下 bedrock / google / mistral 等按需再说。

### ⏭ 下一步

1. 把 `deepseek.rs` 泛化成 `openai_completions.rs`（读目录里的 baseUrl / compat /
   thinkingLevelMap），凭证按 provider 取 —— 覆盖 26 个 provider；
2. 接 `providers_listed` / `models_listed` 两个事件到这份目录（UI 的模型选择器就真了）；
3. 之后再补 `anthropic-messages`，以及 native / preview / git / run_js 的工具壳。

### 未做 / 已知缺口（qjs 路线上）

native 能力（9 个）、preview、git（6 个）、run_js 这四组工具**还没有 JS 壳**（Rust 实现都在）；
provider 传输目前只有 DeepSeek 一族；OAuth 订阅登录未接。

## 2026-09-19（第九轮）— ✅ QuickJS 版 release APK（9.1MB，不需要 libskal）

用户要「打包 apk release 版本」。这里有个容易混的点，值得记清：

**仓库里现有的 App 是 A 路线**（Tauri + bun bundle），它运行时 dlopen 92MB 的
`libskal.so`；我先按它走了一步，用户指出「QuickJS 版本并不需要 libskal.so」——对。
所以改成给 spike 做壳：**B 路线 APK 是自包含的**，引擎（QuickJS）静态编进二进制。

| | A 路线（主 App） | B 路线（本 spike） |
|---|---|---|
| release APK | 37.5 MB（**不含** libskal；补上 92MB 的 .so 后 ~130MB 量级） | **9.1 MB**（自包含） |
| 运行时依赖 | 92MB 外部 .so | 无 |
| UI | Tauri + WebView（完整产品） | 极简 Activity（显示进程输出 + 审批按钮） |

顺带把 A 路线 release 也打出来了（`app-universal-release.apk` 37.5MB + AAB），
它需要 libskal，而本机那份 92MB 的 `.so` 已经不在了（jniLibs 空），所以那个 APK
**现在装上去 agent 起不来** —— 要用得先 `bash scripts/fetch-libpi-bun.sh` 拉回来。

### 让 CLI 在 APK 里能跑的两条硬约束（都钉进注释）

1. 二进制必须以 **`lib*.so`** 命名进 jniLibs → 系统才解包到 `nativeLibraryDir`，
   而**只有那里可执行**（Android 10+ 禁 data 目录 exec；proot 等项目同款做法）。
2. AGP 默认 `useLegacyPackaging = false`（不落盘、直接映射）→ 必须显式开
   `useLegacyPackaging = true` + manifest `extractNativeLibs="true"`，否则磁盘上没文件可 exec。

### 壳做了什么

一行没改路线本身。壳只解决「手机上怎么按下去」：起进程贴输出；
**审批变按钮**（App 没 stdin → 管道 + 允许/拒绝/总是/全拒写 y/n/a/d）；
API key 存 SharedPreferences 经 env 传；workspace/data 在 filesDir，所以 `--resume` 可用。

### ⚠️ 未验证

设备在装之前从 USB 掉线了（系统层也看不到），所以
**「exec from nativeLibraryDir 在真机上真的能跑」这一步没有实测证据** ——
静态项全过（签名 v2 ✓、extractNativeLibs=true ✓、二进制 PIE + 16KB 对齐 ✓、
与本地构建逐字节一致 ✓），但这是运行期行为。若失败，退路是把 spike 编成 cdylib
走 JNI 在进程内调用。

## 2026-09-19（第八轮）— ✅ iOS 也跑通（编过 + 模拟器真执行）；顺带把「为什么要编 WebKit」讲清

B 路线最后一个没碰过的平台。结论：**iOS 上比 A 路线简单一个数量级**，因为根本不用碰 WebKit。

### 为什么 A 路线要编 WebKit、B 路线不用

用户问得很准：**WebKit 只是 A 路线（bun）的依赖。**

| | A（bun + JSC） | B（本 spike，QuickJS） |
|---|---|---|
| 引擎是什么 | **JavaScriptCore** = WebKit 源码树的一部分（`Source/JavaScriptCore`） | **QuickJS** = 独立纯 C 库，与浏览器引擎无关 |
| 引擎从哪来 | Android 用 skal 预构建；**iOS 真机没有可用预构建** → 自己 clone WebKit 编 | `rquickjs-sys` 自带源码：只有 4 个 `.c`（quickjs/libregexp/libunicode/dtoa），4.5MB |
| 磁盘 | WebKit ~8GB + JSC 构建 ~3GB + bun ~2GB ≈ **13GB** | 无额外源码树 |
| JIT 合规 | 引擎自带 JIT → 必须 `setenv("JavaScriptCoreUseJIT","0")`，且时序敏感 | **不适用**（纯解释器，没有 JIT 可关） |
| 本项目代价 | 见 README，搭起来花了一整个里程碑（M5） | **一个链接参数**，见下 |

### 实测

```
$ xcrun simctl spawn booted <ios-sim 产物> --net-check
  dns     ok   41.2 ms
  tls     ok  115.0 ms  HTTP 401（webpki 根编译进来，未用系统信任库）
  engine  ok   40.7 ms  QuickJS + bundle eval 33ms + 堆 1.78MB
```

完整一轮真 DeepSeek 也通了（建 src/ios.js、改 notes.md、todo 2 项、会话落盘），
`--resume` 26 条 **1.4ms**（比 macOS 1.8ms、Android 5.1ms 都快）。

### 两个 iOS 专属的坑（都封进 tools/ios-build.sh）

1. **`rquickjs-sys` 没有 iOS 的预生成绑定**（与 Android 同因）→ 开 bindgen；Xcode 不带
   可用 libclang → 用 homebrew llvm 的，并用 `BINDGEN_EXTRA_CLANG_ARGS_aarch64_apple_ios`
   指出 `--target` 与 iPhoneOS SDK。
2. **`___chkstk_darwin` 未定义**：Apple clang 对**大栈帧**函数生成这个栈探测调用
   （quickjs.c 的大 switch / 深递归），由 compiler-rt 提供；clang 驱动链接会自动带，
   **rustc 直连 ld 不会** → 显式把 `libclang_rt.ios.a` 加进链接参数。这是整个 iOS 构建里
   唯一需要「适配引擎」的地方。

### 平台账（现在三端齐了）

| | macOS | Android arm64（模拟器） | iOS arm64（模拟器） |
|---|---|---|---|
| 冷启动 | 26–60 ms | 84.6 ms | — |
| bundle eval | 138 ms | 250 ms | **33 ms** |
| 会话恢复 | 1.8 ms | 5.1 ms | **1.4 ms** |
| 完整一轮（真 DeepSeek） | ✅ | ✅ | ✅ |
| 产物体积 | 7.9 MB | 9.5 MB（strip 6.7） | 8.1 MB |

跑法：Android 用 `adb push` 到 `/data/local/tmp`（`tools/android-run.sh`）；
iOS 用 `xcrun simctl spawn booted <bin>`（环境变量要 `SIMCTL_CHILD_` 前缀传 —— 实测
第一次就是漏了这个才报「key 未设置」）。

## 2026-09-19（第七轮）— ✅ goal autoContinue + /plan·/btw + 会话切换；对齐清单全部清完

最后三项「可做未做」都补上了，bun 版的功能面到此**能对齐的都对齐了**。

### ① goal autoContinue（上游 Sisyphus 语义）

上限 10 次 / 模型逐字答 `GOAL_COMPLETE` 即停 / 用户 prompt 重置预算。
**一处实现差异**：bun 版用 `setTimeout` 重试「agent 还在 processing」，QuickJS 没有定时器
→ 改成宿主 tick 每拍试一次（上限 30 拍）。语义相同，形状更贴「循环归宿主」。

实测（真 DeepSeek）：
```
[goal] 自动续跑 1/10        ← 第一轮结束后自动继续
[goal] 模型报告 GOAL_COMPLETE，停止续跑
```
即模型真在续跑后自己判定完成并停下 —— 不是靠上限截断。

### ② `/plan` 与 `/btw`

`--plan <objective>` / `--btw <question>`，都是一次只读嵌套 run。实测 plan 真的先调查
再起草（它发现 `clamp` 已存在，还引用了 AGENTS.md 里「文件不超 40 行」那条规则）。

### ③ 会话列表 / 打开 / 新建

`--list-sessions`（`*` 标最新）/ `--open-session <id>`。**都走 kick 模式**：
`repo.list()` 是异步的，而 rquickjs 的 `call::<String>` 不能把 Promise 转成 String
（实测 `Error converting from js 'promise' into type 'string'`）→ JS 立即返回 `"started"`，
结果经事件回合。这与 App 在真机上被迫采用的形状**一致**（CONTRACTS 记的那条）——
同一个约束：异步 I/O 的结果不能靠返回值穿过桥。

### 对齐总账（相对 bun 版）

| 分类 | 项 |
|---|---|
| **已对齐**（16） | 文件工具 7 个 / fetch / todo / subagent / ask_user / MCP / skills 注入 / 审批 / 会话（持久化+列表+切换）/ AGENTS.md / goal（注入+续跑）/ 自动压缩 / 控制面 / plan / btw |
| **不可移植**（5） | nativeTools（Tauri 插件族）/ run_js（D14 沙箱）/ preview（axum+WebView）/ git（git2+openssl）/ skills 安装器（git2+zip） |
| **按设计不做**（1） | provider 目录与 OAuth（只做 DeepSeek 一家） |

bundle 385,201 B（App 2,950,176 B → 小 **7.7×**）。工具面 13。
测试：spike 10 / crate 13 / src-tauri 39，clippy 与 fmt 全干净（src-tauri 基线 fmt 14 /
clippy 26）。桌面与 Android 双验。

**下一步该考虑的不是再加功能**：这张表已经把 B 路线的成本与收益都量出来了，
要不要切 D1 是个决策（触发条件见 `docs/POCKET-PI-NOTES.md` §4），不是继续堆 spike。

## 2026-09-19（第六轮）— ✅ skills 注入对齐（复用「注入半」）；bundle 对齐基本收官

D12 的 skills 在实现上天然分两半，而这条切分线恰好就是「能不能复用」的答案：

| 半 | 内容 | 结论 |
|---|---|---|
| **注入** | registry 读取 / SKILL.md 解析（frontmatter + command slug）/ 双预算裁剪 | **能复用** → 抽进 `pi-host-tools::skills`，语义与数值逐项照抄（单技能 256KB、总量 64KB） |
| 安装 | git2 拉取 / zipball 解包 / sha256 / registry 写入 | **要重写**（绑定 git2+zip），留在 `src-tauri/src/skills.rs` |

实测（桌面 + Android）：`skills 1 个已注入 systemPrompt`，且模型**真的改用海盗腔回答**
（"Arr, this chart holds four files…"）—— 注入到行为改变这条链路是通的，不只是计数对。
spike 里没有安装器，所以 README 里给了手工放 fixture 的配方（不放的话这条路径永远跑不到）。

### 对齐进度（相对 bun 版的完整功能面）

已对齐：文件工具 / fetch / todo / subagent / ask_user / MCP（双 transport）/ 审批 /
会话持久化 / AGENTS.md / skills 注入 / goal 注入 / 自动压缩 / 控制面。
不可移植（各自绑定 Tauri 插件或 cargo 依赖）：nativeTools / run_js / preview / git / skills 安装器。
按设计不做：provider 目录与 OAuth（只做 DeepSeek）。
仍缺的「可做未做」：goal autoContinue、`/plan`·`/btw` 命令面、会话切换（open/new/delete）。

### 顺带

- 工具面 13；bundle 380,417 B（App 2,950,176 B → 小 **7.8×**）
- `pi-host-tools` 13 个测试（skills 注入半 4 个：command frontmatter / 启用位与缺失目录 /
  parse_skill_md / sanitize_id），crate clippy/fmt 干净
- src-tauri 39 测试（2 个注入测试随实现搬走），fmt 14（基线不变）、clippy **26**（比
  改动前少 1：http_tool 那个 `unused_mut` 随实现进了 crate 并被修掉）
- ⚠️ 又一次踩到 include_str 那个坑：改了 JS 但命令链被前面的补丁失败短路，没跑
  build.sh，于是「跑的是旧 JS」——看到的现象是 skills 完全没注入。README 里那条
  操作纪律值得再看一眼

## 2026-09-19（第五轮）— ✅ MCP 接入（streamable-http 双 transport 真跑通）+ 一个 SSRF 边界问题

对齐表里最后一块「未做但可做」的：MCP。客户端与 bun 版同构移植，出网改走 `host.http`。

### 结果

| 项 | 结果 |
|---|---|
| JSON transport | ✅ 真跑：initialize → notifications/initialized → tools/list → tools/call |
| SSE transport | ✅ 真跑：**整轮 4.6s**，而 mock 故意让 `tools/call` 的流挂 3 秒（没走 first-event 就会撞 30s 超时） |
| 工具注册 | 2 个 mock 工具 → 工具总数 **13**（`mcp__mock__echo` / `mcp__mock__add`） |
| 审批档位 | ✅ `[approval] mcp__mock__add (ask)` —— D11「MCP 工具默认全部 ask」在生效 |
| session 串接 | ✅ `mcp-session-id` 经 host.http 回传，后续请求都带上 |
| Android | ✅ 经 LAN 连宿主 mock（`10.0.2.2:8901`），SSE 模式，13 个工具，ask 审批 |

### 为 MCP 给 `pi-host-tools::http` 补的两个能力（对 fetch 透明）

1. **响应头回传**（`headers` 字段）—— MCP 靠 `mcp-session-id` 串后续请求，原来只回
   `{status, contentType, body, truncated}`。
2. **`readMode: "first-event"`** —— MCP 的 SSE 允许一直挂着不关，缓冲读取会挂到 30s
   超时；这个模式读到第一个完整 SSE 事件（空行分隔）就返回。

### ⚠️ 顺带撞出一个真边界问题：SSRF 防护会拦住本地 MCP

复用 `http_tool` 就**继承了它的 SSRF 策略**，而 MCP 服务器常常就在本机/局域网
（实测 `mcp mock: blocked private address: 127.0.0.1`）。bun 版没这问题（原生 fetch
没有 SSRF 防护）—— 也就是说**这条路线在安全上更严，但严到会误伤合法用法**。

解法不是放宽 fetch（那是给「模型可能被诱导去够内网」设的），而是**给用户显式配置过的
目标授权**：宿主从 `mcp.json` 读出各服务器的 `scheme://host:port`，传给 `host.http`，
只有**同源**的 URL 才跳过私网拒绝。授权源由宿主从配置读、**不是 payload 字段**
（否则 JS 自己就能给自己授权）。fetch 工具没有授权源，私网照旧一律拒。

这条对将来把 MCP 搬进 App 一样成立：**只要 Rust 侧统一做出网，就得先回答这个问题**。

### 顺带

- 工具面 11 → **13**；bundle 375KB → **380KB**（App 仍 2,950KB，小 7.8×）。
- `pi-host-tools` 9 个测试（新增授权源只认同源 / 响应头字段），crate 与 spike clippy/fmt
  全干净，src-tauri 基线不变（41 测试 / fmt 14 / clippy 27）。
- 重构：`Guest::start` 的 12 个参数收成 `HostDeps` + `GuestOptions`（clippy 的
  `too_many_arguments` 只是触发器，本来也该这么分）。
- 新增 `tools/mock-mcp.py`：JSON 与 SSE 两种模式，SSE 模式会先发一条无关通知再发响应
  （验证「跳过通知找匹配 id」）。

## 2026-09-19（第四轮）— ✅ spike 功能点对齐 bun 版（11 个工具 / 4 个已对齐的插件 / 压缩）

把 spike 的能力面往 `pi-bundle/agent-main.js` 靠。逐项表见 spike README（三类：已对齐 /
不可移植及原因 / 按设计不做）。

### 已对齐（真 DeepSeek + Android 双验）

| 能力 | 落法 |
|---|---|
| **fetch** | 抽 `src-tauri/http_tool.rs` → `pi-host-tools::http`（纯函数零改动，SSRF 防护 + HTML→文本一起搬来）。**又一层白拿的复用** |
| **AGENTS.md 注入** | 异步读（要过宿主握手）→ 重装 systemPrompt → `context_ready` 门控第一轮 prompt |
| **auto-compaction** | 同策略（窗口 60% + 保留 8 条）；抽 `textOfContent` 归一化；水位在 `--resume` 时从历史取回 |
| **subagent** | 内置 delegate/researcher/reviewer + `workspace/agents/*.md` 定义；**子代理的工具调用同样过宿主审批**（实测事件里逐条 `[approval] ls/read`） |
| **ask_user** | 与本机 `ask_user.rs` 同契约，决策源换终端；与审批共用「id 出去、事件回来」骨架 |
| 控制面 | `__spike.status/toolNames/history` 对齐 `__pi_status/__pi_tool_names/__pi_history` |

工具面从 7 → **11**：`read write edit ls grep mkdir rm fetch todo subagent ask_user`。
bundle 从 373KB → **375KB**（App 仍 2,950KB，小 7.9×）。

实测（真 DeepSeek）：subagent 委托 researcher 跑完并回报；fetch 拿到 example.com 的
HTTP 200；SSRF 防护拒掉 `127.0.0.1`（agent 还正确解释了为什么读不到）；ask_user 在
`--yes` 下自动选首项、在交互模式读到 `2` 后去读了 `src/app.js`；压缩在
`--compact-at 2000` 下真的触发了（摘要 29 条、保留 8 条，之后水位降到阈值以下正确地不再触发）。

Android（arm64 模拟器）同样跑通：11 个工具、subagent、**fetch 真出网拿到 HTTP 200**、
会话恢复 22 条 11.2ms、AGENTS.md 注入、goal 恢复。

### ⚠️ 对齐时发现 **bun 版的一个潜 bug**

`auto-compaction` 拼 transcript 时 `(m.content ?? []).filter(...)` —— **user 消息的
content 是字符串**，没有 `.filter` → `TypeError: not a function`。本 spike 真跑压缩时撞上，
加 `textOfContent` 归一化才通。bun 版同一段一样写，但阈值是 100 万 token 的 60%（≈60 万
token）**实际跑不到**，所以一直没暴露。**未改 bun 版**（那是设备验证过的在跑代码，改它该由
你定），仅记录。

### 不可移植项（各自绑定一个 Tauri 插件或 cargo 依赖）

nativeTools（Tauri 插件族）/ run_js（D14 隔离 runner）/ preview（axum + WebView 消费者）/
git（git2 + vendored openssl，正是 D16 卡住的那套）/ MCP（客户端待搬，宿主侧已够）/
skills 安装（git2 + zip + checksum）/ provider 目录与 OAuth（按设计只做 DeepSeek）。
这张表的价值同上：换宿主时，这些行每一项都要重写。

### 顺带

`pi-host-tools` 现在有 7 个测试（新增 HTTP 的 SSRF / HTML / 体积上限 3 个），
src-tauri 从 44 → 41（那 3 个测试随实现搬走了），fmt/clippy 基线不变（14 / 27）。
另修一处抽出的依赖：`http` 模块原来直接调 Tauri 的 `logcat`，改成可插拔日志汇
（宿主设 logcat，spike 默认 stderr）。

## 2026-09-19（第三轮）— ✅ spike 在 arm64 Android 上真机跑通（含会话恢复与审批）

用户去跑真机前，先把能自己验的验到位：spike 交叉编译到 Android，并在 **arm64 模拟器**
（API 32）上**真执行**——不是只做静态检查。仍在分支 `spike/quickjs-agent`。

### 结论：Android 上每一层都成立

```
$ ./quickjs-agent-spike --net-check
  dns     ok       32.3 ms  api.deepseek.com → 120.232.219.129, …
  tls     ok      143.1 ms  api.deepseek.com (HTTP 401) — 证书由编译进来的 webpki 根校验
  engine  ok      264.4 ms  QuickJS ok；bundle 356 KB eval 250 ms；堆 1.71 MB
```

完整一轮真 DeepSeek（8 次模型请求 / 6 次工具调用）在设备上跑完：`src/hello.js`
被创建、`notes.md` 被改写、**会话 JSONL 落在设备上**（13.9 KB）；第二轮 `--resume`
**恢复 20 条消息 5.1 ms**，agent 不调工具就答出上一轮的文件。

最值得记的两条（都是真机最可疑的点）：
- **DNS 通**：走 `std::net` 的 `getaddrinfo` → Bionic 解析器，**不是** bun 在 iOS 上
  被坑的 c-ares（那条路读不到 `/etc/resolv.conf`，于是去连 127.0.0.1:53 全挂）。
- **TLS 不依赖系统信任库**：`reqwest` 的 `rustls-tls` = `rustls-tls-webpki-roots`，
  Mozilla 根证书**静态编入二进制**（Cargo.lock 里 webpki-roots 1.0.9，无
  rustls-native-certs）。实测二进制里 `system/etc/security/cacerts` 出现 **0 次**、
  `libssl/libcrypto` 符号 **0 个**（strings 里的 openssl 字样是 ring 的 perlasm 署名）。
  → **D16 卡了 4 轮的那类问题在这条路上不存在**：不用按 Android hashed 目录拼 CA
  bundle、不用 `set_ssl_cert_file/dir`、不受 conscrypt 目录布局变化影响。

### 新增

- `tools/android-build.sh`：交叉编译 + 16KB 页对齐检查（复用 `scripts/check-elf-align.py`）。
  产物 `ELF 64-bit LSB pie executable`，动态依赖**只有 libc/libdl/libm**
  （QuickJS/ring/rustls 全静态），9.49 MB 未 strip / 6.73 MB strip 后。
- `tools/android-run.sh`：推送 + 设备上执行，支持真 key 与宿主 LAN mock；跑正式那轮前
  **自动先自检**。
- `--net-check`：把 dns / tls / engine 三层分开报（engine 完全不碰网络），失败时一眼
  看出是哪层，不用对着转圈的 agent 猜。
- `--data-dir`（真机不能依赖仓库相对路径）；mock 支持 bind host。

### 交叉编译的三个坑（封在脚本里）

1. 任何 cargo 命令都要 NDK 的 CC/AR/RANLIB/LINKER（同 `scripts/android-build.sh`）。
2. **rquickjs-sys 没有 android 的预生成绑定**，只能开 `bindgen`（Cargo.toml 按 target 开）；
   bindgen 要 libclang，而 **NDK 只带 `libClangdXPCLib`** → 用 homebrew llvm 的。
3. **bindgen 自己不传 `--target`**，不给就按宿主解析报 `'stdio.h' file not found`；
   要显式给 `--target`+`--sysroot`，且变量名是 `BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android`
   （下划线；bash 的 `export` 不接受带横线的名字）。

### ⚠️ 真机暴露出来的 bug

**二进制运行期还依赖 `dist/agent.js` 文件**：bundle 明明是 `include_str!` 编进去的，
启动时却去 `fs::metadata("spikes/quickjs-agent/dist/agent.js")` 只为打印体积 ——
桌面完全看不出来，**Android 上直接 FAIL**（真机没有仓库相对路径）。已改成从编译期常量取。
这就是「上设备」这一步的价值：静态检查全绿也盖不住它。

另修 `runtime.memory_usage()` 不能在 `context.with` 里调（runtime 的 RefCell 已借出，
再借 panic），以及 `--net-check` 原本「全部跑完才打印」导致 panic 时一行不输出。

> 模拟器启动踩坑（本机环境）：SDK 里 swiftshader 的 `libGLESv2/libEGL` 签名坏了，
> `-gpu swiftshader_indirect` 起不来。可用组合是 `-gpu host -feature -Vulkan`。

## 2026-09-19（第二轮）— ✅ spike 加会话持久化 + 审批 + goal/todo 插件；真 DeepSeek 全量验证

在第一轮 spike 上补四个能力。**全部真模型验证**（`DEEPSEEK_API_KEY` 在本机 `~/.zshrc`，
非交互 shell 不 source，所以第一轮没找到）。仍在分支 `spike/quickjs-agent`。

### ① 审批：分档由 Rust 持有并**强制**

分档表照抄 `approval.rs`（auto / ask / always_ask，`rm` 永不降级）。比 App 现有实现
多的一条：**工具执行的唯一入口 `host.callTool(callId,…)` 要求该 callId 先完成审批
握手**，与档位无关 —— JS 忘了问、或被改写后故意不问，一律执行不了
（同 D14「边界强制在 Rust 侧」）。JS 侧因此零策略代码。

决策源可换（spike 是终端 stdin，App 是 WebView 经 Tauri 命令），**协议同一套**：
`approval_request` 事件出去、`approval_decision` 回来。

实测一轮三档同现：
```
[approval] ls → allow (auto) / read → allow (auto)
[approval] rm (always_ask) — delete src/app.js — 1 file(s) in 0 dir(s), 33 bytes
[approval] edit (ask) — edit notes.md（带 unified diff）
```
**非阻塞有数字**：`--delay-approval 1000` 时 guest 跑了 **334 拍**（tick 计数器），
即等用户决策期间 VM 线程照常跑 —— M1 踩过的「VM 线程上等 I/O」坑没复现。

拒绝路径也验了：`--deny` 下 agent 明确说「不会换别的工具绕过同样的改动」。

### ② 会话持久化：**复用 App 的同一份实现**

上游 `JsonlSessionRepo` + `Session`，fs 后端在 Rust（`host.fs` → 新抽的
`pi-host-tools::sessions_fs`，即 `loopback.rs` 那 12 个 fs 方法，262 行同样脚本切片
抽出）。接法照搬 `agent-main.js`（设备验证过的那份）。

产物 **pi-v4 逐字段一致**，与 App 的会话文件可互相打开：
```
{"kind":"header","version":4,"id":"01a0b726-…","createdAt":…,"cwd":"…"}
{"kind":"entry","lane":"main","type":"message","id":"…","message":{…}}
```
`--resume` 恢复 18 条消息 **1.8 ms**，且 agent **不调工具**就答出上一轮创建的文件
（证明上下文真回来了，不是假装）。

### ③ goal / todo 插件

- **goal**：宿主持有 `goal.json`，JS 只拼进 systemPrompt 的 `# Current goal`（同 App 分工）。
- **todo**：4 态状态机 + `blockedBy` 依赖校验（未知/墓碑/自阻塞/成环）+ 6 动作 +
  `todo_updated` 事件；状态不写磁盘，从会话消息的 `details` 快照回放重建。
  ⚠️ 是**移植**不是共享，跟 App 那份平行（README 里标了）。

### 指标（macOS arm64 / release / 真 DeepSeek）

| 指标 | 本轮 | 第一轮 |
|---|---|---|
| JS bundle | **364,730 B**（含会话+todo） | 316,378 B |
| guest 冷启动 | 26 – 60 ms | 48 – 150 ms |
| QuickJS 堆 | 1.81 MB | 1.59 MB |
| 会话恢复 18 条 | 1.8 ms | — |
| 首增量（真网络） | 402 – 929 ms | 本地 mock 2 – 14 ms |
| 整轮（4 请求 + 3 工具） | 4.4 s | — |

App 的 bundle 是 2,950,176 B → 仍小 **8.1×**。

### 结论修正

第一轮说「B 的真实增量 = 重写 pi-ai 传输层 + 重建产品层」。这一轮把它落成一张
**能复用 / 要重写**的清单（README 末尾）：工具层与会话层**能复用**（零改动），
审批层与插件层**要重写**（协议与语义可照搬），provider 传输**要重写**。

### ⚠️ 本轮踩的坑（都记在 spike README）

1. **会话静默不落盘**：两个原因叠加 —— 宿主漏了「建 sessions 根目录」（App 在
   `lib.rs` 启动时建），且我照抄 `joinPath` 时**多拼了一次** `SESSIONS_ROOT`。
   靠「给 fs 通道加失败日志」才定位（第一版只看到 `FileError`，不知哪一步哪个路径）。
2. **deny 路径在指标里隐形**：被拒调用提前 return 不记 span，已补 `denied_calls`。
3. **JS 事件精简器丢了 `delta`** → 「模型答了但屏幕空白」。
4. **mock 在工具调用前发了 `[DONE]`** → 工具调用整段丢失。mock 也要当被测代码写。

## 2026-09-19 — ✅ PocketPi/PocketJS 调研 + B 方案（QuickJS 薄 JS）spike 跑通（分支 spike/quickjs-agent）

两件事：先调研社区同类实现，再把调研里最有价值的那条路线做成可运行的 spike。
**未合并 main**，全部在分支 `spike/quickjs-agent` 上。

### ① 调研（新增 `docs/POCKET-PI-NOTES.md`）

- **pocket-pi 不是移动端项目**：跑在 Waveshare ESP32-P4/S3 上，另一个 macOS
  模拟器。PLAN.md 第 8 行把它列为「职责划分范式」的启发来源没错，但**它的运行时
  选型与我们的 D1 相反** —— 这一点之前没记进文档，已补。
- 它的方案是「薄 JS + 厚原生」：QuickJS guest 里只跑 `pi-agent-core` 的 Agent 类，
  pi-ai 只 import 一个 `AssistantMessageEventStream`；模型传输（4 家 provider，
  ≈42KB Rust）与工具（≈61KB Rust）全在原生侧。bundle **318KB**。
- **它反驳了我们淘汰 QuickJS 的理由**：`LIBPI-BUN-NOTES.md` §3 记的是「QuickJS 无
  fetch/Streams，需自建 Web API 数年」。pocket-pi 不补 Web API，而是把需要网络的
  那层整个搬到 Rust；prelude 只 polyfill 了 7 个东西。
- **Claude Code 已经没有 JS 运行时可移植了**：1.0.128 还是 `bin/cli.js`（node≥18），
  当前 2.1.277 的主包只有 184KB/7 文件，真身是 8 个平台包里的**原生二进制**
  （darwin-arm64 = 217,662,576 B 单个可执行文件），平台只有 linux/win32/darwin，
  **没有 android/ios**；官方 agent SDK 用 `child_process` spawn 它，同包
  `extractFromBunfs.js` 注释确认是 `bun build --compile` 产物。iOS 禁 fork/exec
  → 这条路是死的。
- 完整版 `pi-coding-agent` 是 JS（21.9MB/1056 文件，node≥22.19，要 pi-tui + exec）。
  逐条核对后的真实阻塞项：`photon-node` **不是**阻塞（纯 WASM），clipboard 可选，
  硬阻塞是「Node 运行时 + 终端 + iOS 上的 shell」。Node-on-mobile 现状：
  `nodejs-mobile` 最后提交 **2021-10**（事实停更）；唯一还活的是
  `puerts/backend-nodejs`（2025-07，从 nodejs/node 源码构建 libnode 给 iOS/Android）。

### ② B 方案 spike（`spikes/quickjs-agent/`）

rquickjs 宿主 + DeepSeek 一家传输（Rust）+ **复用现有 Rust 工具**。三条命题都成立：

| 指标 | 值 | 对照 |
|---|---|---|
| JS bundle | **316,378 B** | bun 路线 2,950,176 B → **小 9.3×** |
| guest 冷启动 | 48 – 150 ms | — |
| QuickJS 堆 | 1.59 MB | bun 路线 87MB .so |
| prompt → 首增量 | 2 – 14 ms | 本地 mock，纯开销 |
| 工具调用 | 0.1 – 5 ms | 走 `pi-host-tools` |
| 整轮（2 请求 + 1 工具） | 63 – 161 ms | — |

- **上游零改动**：pi-agent-core 0.84.4 的 Agent 类在 QuickJS 里直接跑，只提供
  `streamFn` + 工具壳（pocket-pi 用 0.81 也是这样）。
- **9.3 倍体积差全部来自 provider 栈**：只 import 一个事件流类，`@anthropic-ai/sdk` /
  `@aws-sdk/client-bedrock-runtime` / `@google/genai` / `openai` 一个都不进图。
- **工具那一半我们早就付过了**：把 `loopback.rs` 的工具实现抽成
  `crates/pi-host-tools`（root 显式传入），`loopback.rs` 保留同名转发，**签名与错误
  文案逐字不变** → src-tauri 44 个测试全绿（含 `write_backup_and_revert_roundtrip`）。
- **零新增 lint 债务**：`cargo fmt --check` 14 处 / `cargo clippy -D warnings` 27 个
  错误，与改动前的 HEAD 完全一致（stash 对照验证）。

**没验的**：本机无 DeepSeek key，**真模型那一轮没跑过**（用无状态 SSE mock 验证协议
形状与整条链）；**未在 iOS/Android 上构建**；无会话/审批/重试/取消。

**结论**：不必因此切 D1。B 的真实增量是「用 Rust 重写 pi-ai 传输层 + 重建产品层」，
不是「换个 JS 引擎」——而 A 已交付的会话持久化、审批回滚、8 家 provider quirk、
OAuth 登录，spike 一个都没碰。触发条件与三个方向见 `POCKET-PI-NOTES.md` §4。

### ⚠️ 顺带发现（未改动，记录在案）

1. **CI 已连续多个提交全红，且是「卡在第一步」**（查 GitHub Actions API 确认，
   最新 run 35058739402 / 415b0a0）：
   - `rust (ubuntu/macos)` → 挂在 **`cargo fmt`**，后面的 **`cargo clippy` 与
     `cargo test` 从未执行**（本地补测：27 个 clippy 错误、44 个测试通过）；
   - `lint` → `bun run lint`（biome）失败；
   - `android-cross-check` → `cargo check --target aarch64-linux-android` 失败；
   - `desktop-build` → `bun tauri build --no-bundle` 失败（3 平台）。

   即**四个 job 无一通过**，且至少从 2026-09-15 的提交起就这样。这件事本身比它
   揭出来的 lint 问题更值得处理：现在「CI 绿」不是可用信号，测试与 clippy 实际上
   处于无门禁状态。本地 `git stash` 对照确认这些 fmt/clippy 问题全部是存量，
   与本次改动无关（本次零新增）。
2. `jail_path_in(root, "")` **通过校验并解析为 workspace 根**。配合
   `rm {path:"", recursive:true}` 会指向整个 workspace（需显式 recursive 才会走到
   `remove_dir_all`）。抽取前后行为一致，已在新 crate 的测试里钉住现状并注释标记。
3. `loopback.rs` 里 `rm` 非空目录的错误文案中间夹着约 30 个空格（源码里字符串跨行
   续行留下的），会原样发给模型。抽取时**逐字保留**未改。

## 2026-09-16 — ✅ UI 代码库拆分 + 事件 payload 类型化；遗留 3 处小 bug 记录

纯重构日：零行为变化，typecheck / build / biome 全绿（biome errors 53→42，
其中 `noExplicitAny` 18→5，剩余全部为存量债务）。

### ✅ ① 清理脚手架残留

- 删除根目录误建的 `./~/`（solid-ui CLI 把 `~` alias 当字面路径的产物，
  且被全局 gitignore 的 `~` 模式掩盖，所以一直"隐形"）
- 删除未被引用的 `card.tsx` / `text-field.tsx` / `separator.tsx`
- 移除注释掉的 vconsole 及依赖

### ✅ ② App.tsx 拆分（2614 → 1127 行）

27 个新文件，按"纯逻辑 / domain 状态 / 展示组件"三层切：

- `src/lib/`：`types.ts`（全部类型）、`format.ts`、`providers.tsx`（provider
  常量 + 模块级 skillCmds 单例——语义不变）
- `src/state/`：`useProviders` / `useMcp` / `useSkills` / `useSessions` /
  `usePreview`，签名统一 `useXxx(push)`，依赖注入
- `src/components/`：17 个 props-in 展示组件（accessor 传值），SettingsSheet
  的 4 个 tab 视图拆到 `settings/` 子目录

所有中文设计注释（D14/D15/A3/M6）原样跟随代码搬迁。App.tsx 保留事件 switch、
跨域动作与 JSX 组装——这是 ~330 行 switch + ~230 行动作约束下的下限附近。

### ✅ ③ `pi-agent-event` payload 类型化（`src/lib/events.ts`）

`PiAgentEvent` 判别联合 ~50 个 variant，字段逐一与 emit 侧核对（pi-agent-core
透传 / bundle emit / Rust emit 三方来源在 events.ts 注释里标注）；content blocks
对齐 `pi-ai` 的 Text/Thinking/ToolCall/Image 结构；`parsePiEvent()` 替代内联
try/catch。`agent_history` 消息数组同批类型化。

### ✅ ④ a11y lint 清零

42 errors → 6（剩余全为 CSS/noExplicitAny 存量）。三类修法：

- `useButtonType` ×18：逐个核对 form 位置后全部 `type="button"`（无一是提交按钮）
- 可点击 div/span ×8：7 处直接转原生 `<button>`（Tailwind preflight 已重置
  UA 样式，`role="button"` 方案会触发 biome 的 `useSemanticElements`）；唯
  session-item 因嵌套删除按钮保留 div，用 `role="option"` + `tabIndex` +
  `activateOnKey`（新 `src/lib/a11y.ts`），并给删除按钮补 `onKeyDown`
  stopPropagation 防键盘冒泡误切换会话
- `noSvgWithoutTitle` ×2：dialog/sheet 关闭图标补 `aria-hidden`（按钮内已有
  `sr-only` accessible name）

### ⚠️ 顺带发现的疑似 bug（未改，记录在案）

1. `deleteSession` 删当前会话不重置 token 徽标（`newSession` 会重置）
2. `openDrawer` 中 mcp/skills 列表拉取失败也报 "session_list failed"
3. `/btw` 等命令路径不重置 goalAuto 预算，只有普通消息重置
4. `boot_error` 事件**无 emit 点**（Rust 走 `__pi_boot_error` 全局 → agent_init
   的 Err 串）——App 里的消费 case 是死代码

### ✅ ⑤ 工具卡 + composer 区重设计（参考 agent-workflow-ios 截图）

参考图是浅色主题，我们只移植布局与信息层次模式，**保持暗色单主题**（既有
刻意决策）。四个变化：

- **工具调用卡**：左状态圆图标（绿勾/琥珀转圈/红 !）+ "TOOL CALL" 微标签
  （10px  uppercase 0.08em）+ mono 工具名摘要；右侧 `success · 843 ms` 状态
  文本（新计时：`tool_execution_start/end` 在 App 层记 Date.now 差值入
  `ChatItem.durationMs`，`<1s` 显示 ms、≥1s 显示 s——`fmtDur`）。旧
  `.tool-card` 系样式删除（仅 ChatStream 使用，已确认）
- **composer 卡片化**:QuickBar chips 移入圆角 blur 材质的 composer 容器内部
  （参考图的「chips 在输入行上方」模式）；上方新增 trust-row 微文案
  （左 `ON-DEVICE · SANDBOXED`，右 `WRITES: ask · APPROVAL` 可点 → Settings
  Agent tab）
- **跳底钮改文字 pill**「↓ latest」
- **DEV `#demo` 种子**:App.tsx 加 dev-only 演示数据（8 条代表性消息），浏览器
  无 Tauri 后端也能预览聊天流，留作后续 UI 迭代工具

视觉验证：chrome-devtools MCP + 390×844 视口截图走查（工具卡三态、composer
卡片化、trust row、跳底 pill 均达标；截图 /tmp/pi-mobile-ui.png）。

### ✅ ⑥ 细节打磨：inline code chip + 审批 chip

- **inline code**（`.md code`）：亮底 + 1px 细边框 + 0.92em，全从现有 token
  派生；codeblock 内复位边框防继承。从正文清晰跳出
- **QuickBar 四 chip**:Files / Model ⌄ / **Approval**（FiShield + `writes ·
  ask`，点击直达 Settings Agent tab，与 trust-row 同目标）/ Todos；容器
  `overflow-x: auto` + 两端渐变 mask（iOS toolbar 惯例）兜底小屏溢出
- 验证：typecheck / build 通过，biome 维持 6 errors 基线，截图
  /tmp/pi-mobile-ui-v2.png 走查达标

### ✅ ⑦ streaming 光标 + thinking 动效

- `ChatItem.streaming`:`message_start/update` 置 true、`message_end` 置 false
  （两个 merge 分支都写），`turn_end` 兜底收掉
- **光标**：纯 CSS 伪元素（`.md.streaming` 末块 `::after`），0.5em×1em 圆角
  绿块，只动 `opacity`（1 → 0.15 → 1,1.1s infinite,compositor 友好）；围栏
  代码块场景光标落新行（可接受约定）
- **thinking**：斜体 "thinking" + 三点 staggered 弹跳（delay 0/0.15/0.3s）
- 两者均在现有 `prefers-reduced-motion` 块中关闭动画、静态呈现
- 机器验证：getComputedStyle 确认 `md-cursor`/`dot-bounce` animationName
  生效；截图 /tmp/pi-mobile-ui-v3.png

## 2026-09-15 — ✅ 一批 Agent/界面细节（D17）：删除工具 + 三处界面优化；⏸ rewind 待做

用户暂停 D16 的 TLS 阻塞，转来这批细节。**四项已做，一项记录待做**。

### ✅ ① 文件/目录删除（agent 工具）

Rust 侧本来就有 `fs` 通道的 `remove`（且已支持目录 + 显式 `recursive`），**缺的只是
agent 工具**。按既有约定（read/write/edit/ls/mkdir/grep 都在 `run_tool` 里）加 `rm` 到
`run_tool`，而不是走 `fs` 通道 —— 一个功能一种约定。

两个刻意的判断：
- **默认不递归**：删非空目录必须显式 `recursive: true`。这样「误删整棵树」需要一个
  明确动作，而不是默认后果。报错信息里写明「不可撤销」。
- **审批进 `ALWAYS_ASK`**（与 `git_pull` 同档），**不跟 write 基线**：删除不可逆，
  且审批卡没法像 diff 那样把「会失去什么」展示清楚 —— 降成 auto 等于让 agent 静默
  删掉用户的东西。与 write/edit 分开一档是刻意的。

### ✅ ② 模型选择列表
每行是独立卡片却**没有竖向间距** —— 与侧栏会话卡片当初同一个漏（相邻边框看起来像
一条粗线）。加 `.model-row { margin }`；当前模型的勾号改用主题色，让「选中」在列表里
一眼可辨。

### ✅ ③ 抽屉底部 Preview / Settings 间距
用**相邻兄弟选择器**限定在 `.mt-auto` 里（`.mt-auto > .settings-row + .settings-row`），
不去动全局 `.settings-row` —— 设置页里那些行本来有自己的节奏，全局改会连带影响。

### ✅ ⑤ Agent 配置面板版块样式
`.settings-section-title` 原来只有加粗文字，滚动时几个版块会糊在一起。加上分隔线 +
上边距（首块不要顶线）。

### ⏸ ④ 修改上一次消息并重新发送（rewind）—— 未做，附计划

**难点不在 UI，而在对话历史的位置**：`messages` 在 **bundle 的内存里**
（`pi-bundle/agent-main.js` 持有 `sessionId` 与消息数组，Rust 侧 `sessions.rs` 只
`list`/`delete` 文件）。所以只截断会话 JSONL **不够** —— agent 内存里的历史还在，
它会把已撤回的那轮当作上下文继续。**必须同时截断内存**。

有利条件：857 行注释提到上游有 `replayFromBranch` 概念（todo 回放即走此路），
所以「从某个分支点重放」在依赖里已有基础。

计划：
1. bundle：暴露 `rewindTo(index)` —— 截断 `messages` 到指定下标，并同步持久化
2. Rust：给 `sessions.rs` 加 `truncate(root, id, keep)`（按 JSONL 行数截断），
   供 bundle 经 hostcall 调用
3. UI：最后一条用户消息上给「编辑」入口 → 文本回到输入框 → 重发时先 rewind 再 prompt
4. 验收要点：rewind 后**内存与文件一致**（否则出现「界面上撤回了、下次回复却带旧上下文」
   这种最难查的不一致）

## 2026-09-15 — ⛔ D16 Git：https 远端被 Android 上的 TLS 证书加载卡住（4 轮未解，已暂停换方向）

**状态**：Git 工具本体（clone/pull/status/diff/log/commit）代码 + 44 tests 都好了，
安卓出包也正常；**但真机 `git_clone` 到 https 远端一直失败**。已连续 4 轮修复都在
同一症状上，现记录清楚后暂停试错。

### 当前症状（最新一轮，日志原文）

```
[pi-bun] [git] built CA bundle: 149 certs → /data/user/0/com.sternelee.pi_mobile/cacerts-v2.pem
[pi-bun] [git] CA bundle readable: …/cacerts-v2.pem (222448 bytes)
[pi-bun] [git] ERROR set_ssl_cert_file: OpenSSL error: failed to load certificates:
                error:05880020:x509 certificate routines::BIO lib; class=Ssl (16)
```

用户侧看到的：`Error: git: clone failed: the SSL certificate is invalid; code=Certificate (-17)`

### ✅ 已被证据排除的（别再重复走）

| 假设 | 如何排除 |
|---|---|
| libgit2 没有 TLS 后端 | 已加 git2 的 `https` feature（`default-features=false` 曾把它一起关掉）。修后 `libgit2.a` 里实测 **8 个 TLS 符号 + 31 个 http 符号**，且 `out/build/` 下有 `openssl.o`/`tls.o` |
| env 变量没设上 | 日志确认 `SSL_CERT_FILE`/`SSL_CERT_DIR` 都设了；但**设了也没用** |
| Android CA 路径/格式不对 | 实测 `/apex/com.android.conscrypt/cacerts` 与 `/system/etc/security/cacerts` 各 **149 个 hashed 证书**（`subject_hash.N`，正是 OpenSSL 目录格式），app 进程 `run-as` 可读 |
| bundle 文件不可读 | Rust 侧 `File::open` 成功、字节数 222448 |
| bundle 格式（尾部杂文本） | Android cacerts 的 `END CERTIFICATE` 之后**确实**跟着 `SHA1 Fingerprint=` 等文本，已改为只提取 PEM 块（773174 → 222448 字节）；但**宿主 OpenSSL 实测能正常解析带尾部文本的原版**（`crl2crl2pkcs7` 无报错、逐证书 0 失败）→ 该假设不成立 |

### ❌ 我两次被自己的推断误导（值得记）

1. 以为「OpenSSL 不读环境变量」→ 真错是**文件加载**失败，方向一直偏着
2. 以为「尾部杂文本导致解析失败」→ 宿主 OpenSSL 能解析，不成立

两次都是**在没有直接证据前就锁定了一个原因并围绕它改**。真正让定位前进的是**加了
自检日志**（把 `File::open` 结果、字节数、传给 OpenSSL 的字符串都打出来），而不是
继续推理。

### ⏭ 剩下两条（下次从这里接）

**② 改调 `set_ssl_cert_dir`（尚未试过，推荐先做）**
Android cacerts 目录本身就是 OpenSSL hashed 格式，走的是 `X509_LOOKUP_hash_dir`，
与 file 的 `BIO_new_file` 是**两条不同代码路径**。⚠️ 注意 `set_ssl_cert_file` 与
`set_ssl_cert_dir` 都往 `GIT_OPT_SET_SSL_CERT_LOCATIONS` 的另一个参数传 NULL，
**libgit2 是一次性覆盖两者**，所以只能调一个（两个都调会让后者清掉前者）。

**① 决断性实验：最小单证书文件**
主机生成一个只含 1 张证书的 PEM 放进 data dir，调 `set_ssl_cert_file`：
- 也失败 → 我们 vendored 的 OpenSSL 在 Android 上 file/BIO 层有问题 → 该换 TLS 路径或后端
- 成功 → 是我们那份 bundle 的问题，可缩小到具体证书

### 💡 不必卡在这里：不依赖网络的那半可以先用

`init` / `commit` / `status` / `log` / `diff` **完全不走 TLS**，且代码与测试都已就绪。
`workspace/gitdemo/`（**故意不是 git 仓库**）就是为它准备的：让 agent「把 gitdemo
提交一下」，应自动 init + stage + commit，随后 `git_status` 报 `clean`。
**这条至今也没验过**。若它通，说明 Git 集成本体是好的，只有 https 这一条路受阻 ——
可先把远端操作标为暂不可用、把本地工作流放出去用，TLS 单独当一个问题解决。

### 其它已确认的环境事实（与上面无关但会绊人）

- **Android 上任何 cargo 命令都需要那组 NDK env**（不只手 tauri build）：
  裸 `cargo check --target aarch64-linux-android` 会因 openssl-sys 的 build script 要 CC 而失败
  → 走 `scripts/android-build.sh`，或照抄它那四个 export
- `minSdk` 已 24 → **28**（OpenSSL 的 `getentropy` 需要 API 28）
- debug APK 已达 **571 MB**（三个静态 C 库 + 调试符号；release 会小很多）

## 2026-09-15 — ✅ D16 Git 工具本体已落地（clone/pull/status/diff/log/commit）

**状态**：Rust 侧 + hostcall + bundle 工具都写好了，44 tests 绿，双端交叉编译通过。
**未做真机验证**（没在设备上真 clone 过），**push 未实现**（见下）。

### 已实现

| 层 | 内容 |
|---|---|
| `src-tauri/src/git.rs` | libgit2 封装：status / diff / log / clone / pull / commit；5 个负向优先单测 |
| hostcall | `git_status` / `git_diff` / `git_log` / `git_clone` / `git_pull` / `git_commit`，**agent 主体专用**（不在 script 白名单 → 脚本自调自动被拒） |
| 审批 | `git_pull` 进 **ALWAYS_ASK**（永远问，不受 write 基线影响）；`git_commit` 进 ASK_TOOLS（跟 write 基线）；其余只读自动 |
| bundle | 6 个工具注册；pull/commit 标 `mutating: true`（**漏标就等于绕过审批**） |

三道边界都落在 Rust 侧（不在 JS 侧，同 D14/D15）：
1. **jail**：仓库路径必须落在 workspace 内（复用 `loopback::jail_path_in`）
2. **URL**：只允许 https + 复用 `http_tool::validate_url`（拒 loopback/私网）
3. **脱敏**：`scrub()` 把错误信息里的 URL 换成 `<remote>` —— git2 的报错有时会带上
   含 token 的 URL，而错误信息是要回给模型的

### 有意为之的几个判断

- **pull 只做快进，不自动合并**：冲突合并需要工作区干净 + 人工决策，agent 在这种
  场景更容易把事情搞坏。分叉时直接报错并说明怎么办。
- **commit 时目录存在但非仓库 → 就地 init**：「写文件然后提交」是很自然的流程，
  报错反而挡路。
- **author 缺省用 `pi-mobile agent`**：移动端通常没有 `user.name`，libgit2 会因此
  拒绝提交；给显式默认值比报错好，且提交历史里能看出是自动产生的。
- **clone 目标非空则拒**：`Repository::clone` 到非空目录要么失败要么留半成品，
  而那种报错对模型毫无指导意义。

### ⚠️ 顺带发现的运维事实

**Android 上任何 cargo 命令都需要那组 NDK env**，不只是 `tauri build`：
裸 `cargo check --target aarch64-linux-android` 现在也会失败（openssl-sys 的 build
script 要 CC）。**CI 里若不导出会红在一个看起来与业务无关的地方**（openssl-sys）。
已写进 `scripts/android-build.sh` 头注释。

### push：仍未实现

`git2` 本身**有** push 能力，所以不再是「库不支持」的问题，而是**没写**。
按 D16 的决策它本来就在 v1 之外（当时的前提是 gix 无 push，现在换 git2 后
技术障碍消失了 —— 但 push 是最需要谨慎设计的一项：把用户代码发到远端，
审批卡必须显示远端 URL）。**要不要做、按什么授权粒度做，待定。**

## 2026-09-15 — ⛔ D16 Git 集成：git2 vendored 与 minSdk 24 冲突，APK 构建当前**是坏的**

**状态**：spike 表面通过、实际暴露一个**真实的平台冲突**。依赖树里已加 git2
（vendored-openssl/libgit2/zlib），**`bun tauri android build` 现在失败**。
在解决之前 Android 无法出包。

### 冲突本身（不是配置疏漏）

```
src-tauri/gen/android/app/build.gradle.kts:23:  minSdk = 24
```

而 `getentropy` **需要 API 28**，OpenSSL 3.x 的 `providers/…/rands/seeding/rand_unix.c`
会直接调它：

| 做法 | 后果 |
|---|---|
| CC 用 API 24（= minSdk，Tauri 构建的做法） | **编不过**（`getentropy is unavailable: introduced in Android 28`） |
| CC 用 API 28（我第一次 spike 的做法） | 编得过，但**产出的代码在 API 24–27 设备上运行期失败** |

### 🔴 教训：我的第一次 spike「通过」是误导性的

我 export 了 `CC_aarch64_linux_android=…-android28-clang` 才让 Android 侧编过，
并据此宣布「双端交叉编译通过」。但**我手动指定的 API level 高于本 app 的 minSdk**，
所以那不是有效验证，而是**把不适用的配置测成了一个假绿**。

**下次验证 C 依赖时必须让 API level 与 minSdk 一致**，否则测的是另一个目标。
这与本项目已经吃过的「探针只断言 ok/失败不够、要断言返回形状」是同一类问题：
**测了，但测的不是那个东西**。

### 有效的部分（仍然成立）

- **iOS 侧通过**且无此问题（`cargo check --target aarch64-apple-ios` 1m21s，0 errors）
- 配方本身有效：`.cargo/config.toml` 的 `ZLIB_SRC=1` +
  `LIBGIT2_SYS_USE_PKG_CONFIG=0` 确实让三个 C 库走 vendored 交叉编译
- 另一个真坑（NDK 无带前缀的 `ranlib`，需 `RANLIB_aarch64_linux_android`）已确认

### ✅ 解决（2026-09-15，用户决定提 minSdk 到 28）

用户选择提 minSdk。改动与配套：

1. `src-tauri/gen/android/app/build.gradle.kts`：`minSdk 24 → 28`（附注释说明原因，
   并标注 `gen/android` 是 `tauri android init` 生成的、重新生成会丢）。代价已确认：
   放弃 Android 9.0 以下。
2. **新增 `scripts/android-build.sh`**：Android 构建必须走它，不能再用裸
   `bun tauri android build`。原因：**Tauri 传给 `cc` 的 API level 与 minSdk 不一致**
   —— 实测把 minSdk 提到 28 后，它**仍然**用低 API 包装器，所以 OpenSSL 依旧编不过。
   脚本做三件事：从 `build.gradle.kts` **读出 minSdk**（不另写一份，否则两边漂移
   就是又一次假绿）、据此选 `…-androidNN-clang`、并补齐 `AR`/`RANLIB`/`LINKER`。
3. 验证：`./scripts/android-build.sh --debug --target aarch64` → **Finished 1 APK**。

另记一条被否掉的办法：`ANDROID_API_LEVEL=28` 环境变量（想让 cc crate 自己选对包装器）
**实测无效**；只有显式 `CC_<target>` 那条路有效，所以必须用包装脚本而非纯 env 配置。

### ⏭ 原选项（存档）

1. **把 minSdk 提到 28**：最干净，但**放弃 Android 9 及以下**（2018 年及更早设备）。
   是否可接受是产品决定，不是技术决定。
2. **找 OpenSSL 的 configure 开关避开 getentropy**（若有，最优：保留 minSdk 24）。
   需查 OpenSSL 3.x 在 Android < 28 的官方姿态。
3. **强钉 API 28 编** —— **不可取**：编译通过但运行期在 24–27 挂，属「把问题推到用户手上」。
4. **换掉 vendored-openssl**：libgit2 在 Android 上的 HTTPS 需要 TLS 后端，
   Android 侧可选项很窄（mbedtls/schannel 都不合适），**大概率仍是 openssl**。
5. **回退 git2**：立刻恢复可出包，Git 集成改用 gix（但有 push 缺口）或延后。

**恢复可出包的最短路径是 5；把功能做成的最短路径取决于 1 是否可接受。**
## 2026-09-15 — ✅ D15 产物预览：真机跑通（pi 自己写五子棋 + 调预览工具）

**状态**：用户的完整流程已验证——「让 pi 写一个五子棋 → pi 用 `write` 写
html/js/css → pi 调 `preview` 工具 → 面板自动弹出且可玩」。**效果确认不错。**

### 已入库

| 提交 | 内容 |
|------|------|
| `be8ac08` | D15 决策（含用户选的「允许脚本 + 允许联网」） |
| `1fc5dd2` `f649920` | 手写 HTTP 预览服务 + 纯函数重构（修测试间全局竞态） |
| `79e8142` | **换 axum + `tower-http::ServeDir`**（替掉手写 HTTP 解析） |
| `7ec576f` | agent 侧 `preview` 工具（hostcall `preview_open`） |
| `69fed59` | 工具栏安全区 + 补上 `preview_open` 的 UI 半边 |

### 🏗 关键决策：为什么必须是**真实回环 HTTP 源**

用户曾提议参考 `tauri-axum-htmx`。读完后发现那个项目**不是 HTTP 服务**：它跑的是
axum 的 **Router**，请求经 **Tauri command（IPC）隧道**转发。

它**不能用于预览**：`<link href>`、`import "./app.js"`、`<img src>` 这些是**浏览器
引擎自己发起**的请求，**不经过页面 JS**，所以 JS 层拦截根本看不见（要装 Service
Worker，而它需要 secure context，在移动端自定义 scheme 上不可靠）。

→ 结论：**架构不变**（真实回环 HTTP 源，相对路径/ES module/图片才能正常加载），
**实现换掉**（用 axum + ServeDir，不再手写 HTTP 解析）。那个项目的启发只有
「别手写 HTTP」这一半。

### 🔴 两个坑（都是「换库/加覆盖层」时才出现的那类）

**① `ServeDir` 会跟随符号链接 —— 换库不等于安全自动到手**

换的时候就说好要审默认行为，审出两条：
- **不列目录**（ServeDir 无此能力，只补 `index.html`）→ 不泄露文件名 ✓
- **会跟随符号链接** ⚠️ —— 而且这是 ServeDir 与手写版**共有**的风险：只查字符串
  挡不住 `workspace/link -> ../../creds.json`。

→ 新增 `deny_escape` 中间件做 canonicalize + 前缀校验。测试同时验反向
（工作区内指向工作区内的符号链接仍放行，避免过度拦截）。

**② `position: fixed` 绕过 safe-area（预览工具栏遮状态栏）**

```css
.app { padding-top: var(--safe-top); }        /* app 布局靠这条避开状态栏 */
.preview-sheet { position: fixed; inset: 0; }  /* ← 相对视口定位，跳过了它 */
```

这类 bug 只在**新增 fixed/absolute 覆盖层**时出现。修法：安全区加在工具栏
**自己**上（底色铺到屏幕顶边更像原生），sheet 补 `padding-bottom`。

**③（环境）Android release 的 `usesCleartextTraffic=false` 会拦掉整个 iframe**

debug=`true` / release=`false`（`build.gradle.kts:21` 默认值未被覆盖）→ WebView
拒绝 `http://127.0.0.1`，表现为「debug 能预览、release 打开是空白」。
→ 新增 `res/xml/network_security_config.xml` **只对 127.0.0.1/localhost 放开明文**
（不是把 release 整体放开——那会让整个 WebView 允许任意明文）。

### 安全姿态（有意接受的）

用户选择**允许脚本 + 允许联网**。预览页可以把数据发到任意外网，本模块**不拦**
网络。能外泄的只有页面自己能生成的、或先经审批写进 workspace 的东西；预览页
够不到 app DOM、拿不到 host token（`REQUIRE_HOST_TOKEN` 已真机验证）、调不了
任何工具。面板刻意做足辨识度（`PREVIEW` 徽标 + 独立底色），否则它就是一个
现成的钓鱼面。

### 另一个记录问题（已如实写进提交信息）

`69fed59` 提交时把 `src/App.tsx` 的 `preview_open` 处理一并带走（它一直「已暂存
未提交」）—— 所以 **`7ec576f` 当时并非端到端完整**：Rust 发事件但 UI 不认，
agent 调 preview 时面板不会自动打开。现已补齐。

### ⚠️ 边界情况清单（未修，按可能咬人的程度排序）

#### A. 我怀疑是真问题的

**A1. ✅ 已修且真机验证通过（2026-09-15）**

沙箱化后文档是 **opaque origin**，它向自己那个源发 `fetch('./data.json')` 会被视为
跨源（`Origin: null`），而服务端**没有发 CORS 头**（当时 `preview.rs` 里 0 处
`CorsLayer`）。`<script src>` / `<link>` / `<img>` 不受影响（非 CORS 约束），
**但 fetch/XHR 会失败**。

**修法**：`CorsLayer` 只放行 **`Origin: null`**（`AllowOrigin::exact`），**不是 `*`**。
理由：普通网页发的是真实 origin，于是读不到这个工作区；沙箱预览恰好发的就是
`null`。只读静态服务在最小授权下已经够用。

**验证链（两层，缺一不可）**：
1. 单测 `allows_opaque_origin_but_not_the_whole_web`：服务端确实回
   `ACAO: null`，且**真实 origin 不被放行**
2. **真机**（用户确认）：`workspace/fetchtest/index.html` 显示绿字
   `FETCH OK → {"msg":"fetch works","n":42}`

两层都必要的理由：单测只能证明**服务端发了正确的头**，不能证明 **WebView 真的
接受它**（沙箱 + opaque origin 下浏览器行为才是最终判据）。这类「客户端是否接受」
的断言在真机上才成立 —— 与 D14 的 host token 是同一教训。

**A2. `start()` 把端口缓存得早于「服务真的起来」**

`preview.rs:65` 的 `PORT.set(port)` 在 `set_nonblocking`/`from_std`/`axum::serve`
（`70`/`74`/`87` 行）**之前**。任一步失败 → 端口已缓存 → `start()` 永远返回 Ok，
UI 拼出一个指向死服务的 iframe，且**永不重试**（完全静默）。
修法：失败时清缓存（或 `OnceLock<Result>`），并让 `start()` 等到服务就绪再返回。

**A3. ⚠️ 预览页里的死循环会冻住整个 app（真机已确认，2026-09-15）**

用户实测：`hangtest/index.html` 进入 `while(true){}` 后**整个 App 无法响应**——即
iframe 与 app **共用 WebView 主线程**，没有被站点隔离到独立渲染进程。

**为什么进程内缓解都不可行**（逐条否掉，免得后人重复尝试）：

| 想法 | 为何不行 |
|---|---|
| 页面内加看门狗 | 同步死循环占住主线程，`setTimeout` 根本不会触发 |
| 父页面检测并关闭 | 父页面**同一条主线程**，一起被冻住 |
| Rust 侧 `eval` 强制关闭 | 排队的脚本同样跑不了 |
| Worker 监测到静止后导航 iframe | Worker 不能导航文档，只能 postMessage 给已被冻住的父页面 |
| 静态分析拦 `while(true)` | 抓不住正则回溯、依赖内部的循环；且这是模型自己的 bug，不是对抗场景 |

**唯一真正独立的手段是换进程。** 故已加一个逃生口：工具栏的「在系统浏览器打开」
（`preview_open_external`，系统浏览器是独立进程）。

**但这个逃生口救不了已经卡死的现场**（那时这个按钮也点不动）—— 它是**事前选择**，
事后只能靠系统手势划掉 app。这条局限必须写明，不能让人以为它是兜底。

**可能的前置防护（未做，待评估）**：Rust 侧心跳监测 → 检测到 UI 静默后调
`webview.reload()`。Rust 确实在独立线程上，但 **`reload()` 能否解开一个 JS 线程
已卡住的 Android WebView 未经实验**（这是关键未知，不是实现细节）。另一个更保守
的选项：让重页默认走浏览器打开。

**A4. 沙箱禁掉了一批模型常用的 API（且工具描述没告知）**

| 缺失的属性 | 后果 |
|---|---|
| `allow-modals` | `alert()` / `confirm()` / `prompt()` **被静默忽略** |
| `allow-same-origin` | `localStorage` / `sessionStorage` 访问**抛 SecurityError** |
| `allow-popups` | `window.open` 被拒 |
| （默认无 top-navigation） | 点 `<a href>` 若无 `target=` **静默无反应** |

模型写游戏时这几样都很常见，而现在的工具描述只说「scripts run」——**不够**。
应该把这几条写进 `preview` 的描述，否则模型会写出“看起来对、实际无反应”的代码，
而它看不见报错（见下面的下一步 1），只能靠用户复述。

#### B. 功能缺口

5. **`preview` 传目录会被拒**（`resolve_in` 要求 `is_file`）—— 其实应允许目录并补
   `index.html`（现在是报错 + 列出 html）
6. **`/app` 与 `/app/` 的差别**：`append_index_html_on_directories` 只在**带尾斜杠**
   时补 index。不带尾斜杠时会怎样（重定向？404？）**需实测** —— 若是 404/无重定向，
   而页面里用相对路径，基准目录会错到父级，表现为「CSS/JS 全 404」
7. `targets()` 有深度 4 / 50 条上限，但**服务本身没有** → agent 能预览一个选择器里
   看不到的文件（不一致，不致命）
8. **无文件大小上限**：预览一个很大的文件会整块读进内存（ServeDir 本身是流式的，
   但未验证）
9. `resolve_in` 允许任意文件（`.md`/`.txt`/`.svg`）→ `.md` **不会渲染成 markdown**
   （当纯文本）。可能反直觉，值得在工具描述里说一句
10. 预览页 `fetch('/../pi-bun.log')` 被 jail 拒 ✓，但 `fetch('/')` 会拿到根提示文本

#### C. 只在特定路径出现

11. 重启 app 后面板不恢复（预览不持久化）
12. release 构建依赖 `res/xml/network_security_config.xml` —— 若日后动 manifest，
    要连它一起看，否则回到「debug 能预览、release 空白」

### ⏭ 下一步候选

0. **A 组的战果**：A1 ✅ 已修 + 真机验证（fetch JSON 型游戏可写）；A2 ✅ 已修
   （端口不再早缓存）；A3 ⚠️ 确认为真 + 已加逃生口，并归档了「进程内缓解都不可行」
   的逐条否证；A4 ✅ 已改工具描述告知沙箱限制。
   **A3 的候选解法（未做）**：先做一个便宜实验——Rust 侧心跳 + `webview.reload()`
   能否解开一个 JS 线程已卡死的 Android WebView。**结果决定后面几小时的活该不该干**，
   所以先实验再动工。
1. **把预览页的 console error / `window.onerror` 回传给 agent**（`postMessage` →
   hostcall）—— 现在页面报错**用户看得见、pi 看不见**，只能靠猜；补上才能闭环
   「写 → 跑 → 自己看报错 → 修」。这是这条流程下一个真正的痛点。
2. 截图回传（让模型“看”到自己画的棋盘）—— 更重，移动端 WebView 截图要先看平台能力
3. 清理：`cfg-diag`（`1754a88`）是临时诊断，定位后应删

## 2026-09-14 — ✅ D14 脚本执行：安全核心 + 隔离 runner 完成，host token 真机打通；⏸ run_js 端到端待自建产物

**状态**：方案 C（D14）的**安全模型在真机上成立了**。剩最后一块：`run_js`
从未在真机上跑过一次——因为**两端的嵌入式产物都还是旧的**（见下面「环境事实」）。

### 已入库

| 提交 | 内容 |
|------|------|
| `211bac6` | D14 决策 + spike 结论 |
| `74af6dc` | spike：第二 VM + 执行时限（宿主验证） |
| `56cf4a3` `e4c343c` | 安全核心（能力表/token/边界强制/配额）+ 契约登记 |
| `8a0e659` `e471317` | UI 审批卡（能力清单 + 脚本源码 + script 标志） |
| `3593420` | zig 隔离 runner + 双超时（父会话独立复现过） |
| `25bf4f7` | `script_run` 通路 + `run_js` + 翻 flag |
| `a53ea32` | `cargo fmt` 全量（既有债，**单独一次**） |
| `daad1e2` `188a389` `1754a88` `6f43a8e` | host token：接入 → 排查 → 修根因 → 真机全绿 |

### 🔴 根因归档：host token 恒 ABSENT（读错了嵌套层）

**症状**：翻 `REQUIRE_HOST_TOKEN=true` 后真机上 agent 的 hostcall **全被拒**
（一次启动 23 次 `deny (bad host token)`）；诊断恒报 `host token ABSENT`。

**根因**：hostcall 请求体是 `{ method, payload, __hostToken | __scriptToken }`
—— 两个 token 都是 `payload` 的**同级**字段。而 `handle_conn` 传给 `dispatch`
的是**内层 `payload`**，于是永远读不到 token。**客户端一直是对的。**

```rust
// 错：token 在 v 里，不在 payload 里
let payload = v.get("payload").cloned()...;
dispatch(&m, &payload)
// 对：另传整个 body
let payload = v.get("payload").cloned()...;
dispatch(&m, &payload, &v)
```

**它制造的假矛盾**（白花了好几轮）：`cfg-diag` 实测 `__PI_CONFIG` 完全正常
（`{"k":[...hostToken...],"t":"string","l":69}`），可请求恒 ABSENT。于是
一直在怀疑「注入对不对 / 顺序对不对 / 客户端发没发」—— 全是**我写过的**
地方，唯独没怀疑「传参的嵌套层」。

> **教训**：证据说「发送方正常」而「接收方说没有」时，第三个可能——
> **接收方看错了地方** —— 应该更早进入候选。

### 🔴 两个流程陷阱（同类：把「没有坏消息」当成「好消息」）

1. **跳过了自己定的分阶段验证**。第 1 步提交信息里写了「未验：Android
   真机」，第 2 步却直接翻 flag —— 而那次验证正是分阶段设计的唯一目的。
   分阶段的价值不在「少改」，而在**让每次失败只剩一种解释**。
2. **差点把空日志读成「已修好」**。设备 USB 掉线时日志为空，`grep -c` 输出
   `ABSENT: 0`，看着像修好了。同批命令里 `adb` 报的是 `device not found`。
   → 结论前先确认观测通道活着（`adb devices` + `system_profiler` 双验）。

### 已加的两道防线

- `dispatch` 的诊断**分 `ABSENT`（没发送）与 `MISMATCH`（发了但对不上）**
  —— 两者修法完全不同（改客户端 vs 改签发/传递），混在一起会白烧一轮构建。
- 翻 flag 的前置条件写进了 `script.rs` 注释：**真机诊断日志完全静默**。

### 已修的真 bug（顺手）

- `netprobe.js` 读**小写** `__pi_config` → loopback 那项一直静默 `skipped`，
  从未真正测过（即启动自检「全绿」里这一项没跑）。已修，真机现为 `status 200`。
- `bridge.js` 是**第二个 hostcall 客户端**，两处都不对：读小写 `__pi_config`
  （→ 在真实 app 里根本装不起来，也是 netprobe 当初读小写的源头）、裸 fetch
  不带 token。已修（两个全局都认 + 调用时读取 token）。

### 💡 环境事实（重要，别再搞错对象）

| 平台 | 嵌入式产物 | 有 `pibun_run_script` 吗 |
|------|-----------|----------------------|
| **Android** | **上游预构建的 `libskal.so`**（`fetch-libpi-bun.sh`，91,935,608 字节，sha256 `5cdc391b…`） | ❌ 真机日志：`WARN pibun_run_script missing — script execution disabled (stale build?)` |
| iOS | 自建 `libskal.dylib`（但 `ios-release/*.o` 是**旧入口**编的） | ❌ 实测 0 次 |

我一度误以为 Android 有 runner —— 因为 grep 的是 Rust 自己的
`libpi_mobile_lib.so`，而不是 `jniLibs/libskal.so`。**查符号要看对文件。**
软绑定在这里救了场：缺符号只让脚本能力不可用，agent 其余功能照常。

### ⏭ 下一步（按顺序）

1. **跑 `scripts/build-libpi-bun.sh`** 用自建产物替换 Android 的 `libskal.so`
   （会先编 JSC-for-Android，耗时较长），然后装包确认日志里**不再有那条 WARN**。
2. **`run_js` 端到端真机验证（至今一次没跑过）**：
   - 正向：`needs:["fs:read"]` 的脚本能读到工作区文件
   - 负向：未声明能力被 **Rust 侧**拒；伪造/未知 `__scriptToken` 被拒且**不回退成 agent**
   - 两条超时：`while(1){}`（JSC 看门狗）与 `await new Promise(()=>{})`
     （墙钟看护，看门狗在此**不触发**）→ 终止且 **app 不冻**
3. **iOS**：需先重新登录 Apple ID（profile 已于 09-13 16:02 过期）；且要
   重交叉编译 bun（`--profile=ios-release`）+ 在 `scripts/link-skal-ios.sh` 补
   `_pibun_run_script` 导出与 `-Wl,-u,_pibun_run_script`（见 `3593420` 说明）。
4. **清理**：spike 脚手架已删；`cfg-diag`（`1754a88`）是临时诊断，**定位后应删**。

### 仍挂着（与本次无关）

- **463MB 检查点 ref**：`refs/pi-checkpoints/*`(50) + `refs/cline/checkpoints/*`(12)
  钉着重写前的旧对象（main 本身已干净：0 产物文件、0 相关提交）。删了会让
  rewind/cline 的回退失效，**待用户决定**。
- **`biome check` 在 `App.tsx` 本身就有 33 个 error**（既有债，量过没动；
  `cargo fmt` 那笔已清）。

## 2026-09-13 — ⏸ 已知问题：Android photos_list 读不到相册（已记录，暂停排查）

**状态**：iOS 侧 `photos_list` / `photos_save` 真机验证通过（138ms 返回真实
照片元数据）。**Android 侧读不到照片**，根因未确定，按用户决定暂停并记录。
当前 Android 行为：快速返回 `{"photos": [], "totalSeen": 0, "source": "..."}`
（**不挂起** —— 中途一版改动会让它挂到 JS 侧 30s 超时，已回退）。

### 已收集的证据（决定性，复现时直接用）

环境：Honor MEY-AN00（`A22CVB5B14008470`），**Android 16 / API 36**。

| 观测 | 结果 |
|------|------|
| shell 查 `content://media/external/images/media` | 能看到 **10+ 行**，`is_trashed=0`，在 `DCIM/Camera/`、`DCIM/Alipay/` |
| 应用权限 `READ_MEDIA_IMAGES` | `granted=true` |
| appops `READ_MEDIA_IMAGES` | `allow` |
| 清单里另存在 `READ_MEDIA_VISUAL_USER_SELECTED` | 也是 `granted=true`（**我并未声明它**，是清单合并进来的 —— 且它是 Android 14+「部分照片访问」的权限，值得怀疑） |
| 应用查同一 URI | **0 行**，且**不抛异常**（`totalSeen: 0`） |
| 用户手动改成「允许访问所有照片」后重测 | **仍然 0 行** |

关键机制：**MediaStore 在访问受限时是静默返回空游标，不抛 SecurityException**
—— 所以「查得到 0 行」无法区分「真的没照片」与「被过滤」，必须靠对照实验。

### 已试过且无效/有害的改动（复现时别再走一遍）
1. `EXTERNAL_CONTENT_URI` → `getContentUri(VOLUME_EXTERNAL)`：**URI 字符串完全
   相同**（`content://media/external/images/media`），无效
2. `query()` 返回 null 时显式报错：有价值（防「ok 但内容错」），但本问题里
   cursor **非 null**，只是 0 行，故不解决问题
3. **试过 `VOLUME_EXTERNAL_PRIMARY` 并加 5 条诊断查询（含
   `MediaStore.Files` 全表）→ 调用挂到 JS 侧 30s 超时，比原问题更糟，已回退**
   - 教训：**诊断查询也必须是有界的**，否则诊断本身成为故障源

### 下一步的候选假设（按可能性排序，复现时一次只动一个变量）
1. **合并卷 vs 个体卷**：`VOLUME_EXTERNAL`（合并视图）在部分 ROM 上对普通应用
   不返回行 → 试 `VOLUME_EXTERNAL_PRIMARY`，但**只做一条有界查询**
   （`projection = arrayOf(_ID)`、`selection = null`、**不排序**），不要在同一个
   版本里堆多条查询
2. **`MediaStore.getExternalVolumeNames()`**：先枚举真实卷名，再针对每个卷查，
   能直接看出「卷选择」还是「权限过滤」
3. **Honor ROM 的额外媒体门控**：`appops` 与 `dumpsys package` 都显示已授权，
   但国产 ROM 可能在 MediaProvider 层做额外过滤（本次 `READ_MEDIA_VISUAL_USER_SELECTED`
   的存在很可疑 —— 应用可能被平台当成「仅选中照片」模式，而用户从未选过任何照片）
4. **投影列**：`WIDTH`/`HEIGHT`/`DATE_ADDED` 在 API 36 上的可用性

### 产品层面的欠账（与根因无关，恢复时必须一并修）
现在 Android 上「读不到」表现为**静默返回空列表** —— 模型会据此得出「用户相册
里没有照片」这个**错误结论**（用户实际有照片）。这与 iOS 侧已实现的 `.limited`
如实上报是同一类问题。恢复时应当：
- 无法确定是否读到全部时，返回明确的 `limited`/`unavailable` 标记 + 说明，
  **而不是空列表**
- 参考 iOS：`photos_list` 在受限时带 `limited: true` + note

### 顺带修好的东西（已提交，与本问题无关）
探针 `step()` 的 `fullText` 通道原先存的是**已截断**的值，导致 `contacts_get`
永远解析失败并静默走 skipped —— **那一步等于从未验证过**。改为截断前先留原文。
修复后 Android 真机首次真正验证到 `contacts_get`：
```
contacts_get  OK  11ms  {"contact":{"displayName":"Sora Kasugano","id":"27"}}
```

## 2026-09-13 — Android 验证：发现 photos_list 真 bug（加固未验证）

### Android 真机 12 项自检（上一版构建）
定位/天气×2/日历读+写/通讯录/照片/剪贴板×2/通知/权限态 全部 OK。
其中**新验证到的**：
```
contacts_search  OK  68ms   {"displayName":"Sora Kasugano","id":"27"} 等真实联系人
calendar_create  OK  42ms   {"id":"14169","calendarId":15,"timeZone":"Asia/Shanghai"}
capabilities     OK         calendar.permission=granted contacts.permission=granted
                            platform=android
```

### 【真 bug，待验证修复】Android photos_list 返回空数组
设备上**确实有照片**（`content query content://media/external/images/media`
作为 shell 能看到 _id=1022/1027/1031…），应用权限也**确实已授**
（`READ_MEDIA_IMAGES: granted=true`），但 `photos_list` 返回
`{"photos": []}`。

根因未最终确定，已按两个最可疑点加固：
1. `MediaStore.Images.Media.EXTERNAL_CONTENT_URI` 在 Android 10+ 是兼容别名，
   某些 ROM 上可能解析到不含全部卷的旧 URI → 改用
   `getContentUri(VOLUME_EXTERNAL)`（API 29+）
2. **`query()` 返回 null 时原本静默给出空数组** → 现在显式报错。这正是本项目
   被坑过两次的「ok 但形状是错的」模式：模型会把「查询失败」当成「相册里没有
   照片」，进而给出错误结论

同时加了 `totalSeen` / `source` 两个字段：能区分「相册真的空」「被 limit 截断」
「查询看到了行但没取出来」，也便于下次直接定位。

**加固未验证** —— 构建安装后设备 USB 掉线（`device not found`），未跑成。
注意：期间一度看到「应用未运行 + 日志文件为空」，我差点据此判定崩溃，
实际是**掉线导致的假象**（后续 `adb shell getprop` 才暴露设备已不在）。

### 顺带修掉一个验证盲区
探针 `step()` 把返回文本截断到 300 字符便于日志可读，但后续步骤要解析它拿 id
（contacts_get）→ 因为截断而 `JSON.parse` 失败 → **那一步一直静默走 skipped
分支，等于从未真正验证过**。现在另开一个不截断、不进日志的 `fullText` 通道
供步骤间传递。

## 2026-09-13 — M6 1b 照片（只读）—— 双端实现完成，iOS 未授权路径已验证## 2026-09-13 — M6 1b 照片（只读）—— 双端实现完成，iOS 未授权路径已验证

新增 `photos_list` / `photos_save` 两个工具（1b 第 1 批最后一项）。双端实现：
iOS Photos.framework / Android MediaStore。

### 工具划分与信任边界
- `photos_list`：只返回**元数据**（id/文件名/时间/尺寸/有无 GPS），永不返回图像
  字节 —— 读类自动放行。
- `photos_save`：把某张照片的**原图字节**复制进 workspace，走审批。
- **save 的路径校验刻意只在一处**：Rust 用 `loopback::jail_path`（与 read/write
  同一套越狱防护）算出绝对路径后传给原生侧；原生侧只写字节、不做任何路径判断。
  这样两个平台上不存在第二份路径逻辑。
- **不提供「保存到用户相册」**：那需要额外权限（iOS 需
  NSPhotoLibraryAddUsageDescription）且风险收益不对称。

### 踩坑：iOS 可用性标注（编译期，非运行时）
`PHPhotoLibrary.authorizationStatus(for:)` 与 `PHAuthorizationStatus.limited`
都是 **iOS 14+** API，而 **swift-rs 的实际编译部署目标低于 14**（即使
Package.swift 写了 `.iOS(.v14)`）→ 直接编译失败，且 swift-rs 把错误藏在
「Failed to compile swift package」后面。

排查手法：`swiftc -parse -sdk <iphoneos sdk> -target arm64-apple-ios14.0 <file>`
逐个文件做语法检查（都干净）→ 说明是模块级错误 → 再跑一次 cargo check 拿完整
Swift 输出（这次打印了具体行号）。
修法：给 `PhotosBridge` 加 `@available(iOS 14.0, *)`，并在 3 个调用点做
`guard #available` 守卫。

### 其它实现要点
- **iOS 14+ 的「受限访问」(.limited) 如实报 limited**，向上折叠成 denied 让 UI
  引导补全；不报 granted（理由同通讯录：会让「没找到这张照片」被误读成「相册
  里没有」）
- **iCloud 原图**：`PHImageRequestOptions.isNetworkAccessAllowed = true`。
  不允许的话会**静默返回降级缩略图** —— 用户以为存了原图实际是压缩版，是最糟
  的失败方式。
- **`PHImageResultIsDegradedKey`**：该 API 可能回调两次（先低清再高清），用
  settled 标志位保证只结算一次。
- **保存上限 20MB**：超限直接拒绝并告知，而不是写一半留坏文件。Android 侧先查
  SIZE 再决定是否开始读，避免把几十 MB 拉进内存才拒绝。
- **Android 13 分水岭**：API 33+ 用 READ_MEDIA_IMAGES，≤32 用
  READ_EXTERNAL_STORAGE（清单里带 `android:maxSdkVersion="32"`，否则新系统上会
  多出一个用户看不懂的权限请求）。
- **DATE_ADDED 是秒**：MediaStore 用秒，对外协议一律毫秒，换算集中在一处，
  避免各处漏乘 1000。
- **save 先写 `.part` 再 rename**：中途出错不会在 workspace 里留半个坏图片。
- **探针不跑 save**：save 会往 workspace 写真实文件，每启动一次写一张会把用户的
  工作区无声堆满 —— 只验证 list。
- 顺带修掉探针一处误导性文案：早期版本在 `photos_list` **失败**时也报
  「library is empty」，把「没授权」伪装成「相册是空的」。这类误导文案已坑过两次
  （先被旧日志骗、再被 ok-but-wrong-shape 骗），现在只在成功时才补备注。

### iOS 真机 12 项全绿（授权后）
```
photos_list  OK  138ms  {"photos":[{"createdMs":1784262224000,
                          "filename":"B8EABA85-…-AF543AB5D0FE.JPG","favorite":false,…}]}
```
（未授权时是 13ms 快速失败 + 可执行指引。）

### ✅ M6 1b 第 1 批完成 —— 最初列的 7 项全部落地
剪贴板 · 通知 · 定位 · 天气 · 日历 · 通讯录 · 照片，**双端实现 + iOS 真机验证**。

剩余：**1b 之外的批次**
- 批次 2（iOS 专属，免费账号可验）：提醒事项（EventKit，可复用日历骨架）、蓝牙（CoreBluetooth）
- 批次 3（需付费开发者账号）：HealthKit / HomeKit / NFC
- 批次 4：Android 无障碍自动化
- 欠账：Android 的 permissionState / contacts / photos 尚未上机验证

## 2026-09-13 — M6 1b 通讯录（只读）✅ iOS 真机全绿## 2026-09-13 — M6 1b 通讯录（只读）✅ iOS 真机全绿

**踩坑（值得单独记）：未取 formatter 所需 key → ObjC 异常 → 整个 app 崩溃**

真机表现极具误导性：`contacts` 调用后日志停在 args 行，**既无成功也无失败，
连 JS 侧 30s 超时都不触发** —— 看起来像「挂死」。

排查过程与结论：
1. 先怀疑日志被截断 → 加「日志是否真的增长」校验（发现上一轮我读的是**旧
   记录**：数值逐字节相同。这个校验以后每次都要做）
2. 再怀疑 Swift 没编进去 → `nm` 查 `ContactsBridge` 符号（7 个，在），排除
3. 查系统崩溃日志目录 → 发现**多个 pi-mobile 崩溃报告**，且进程已不在运行
   → 真相是**崩溃**不是挂起（进程已死，所以 JS 超时永不触发）

崩溃栈（决定性证据）：
```
-[CNContactFormatter fullNameForContact:attributes:style:]
  → -[CNContact contactType] → NSException → std::terminate → SIGTRAP
```
`CNContactFormatter.string(from:)` 会读 `contactType` 与姓名前后缀/拼音等
字段，而**我手写的 keysToFetch 列表里没有** → Contacts 抛 ObjC 异常。
**Swift 的 try/catch 抓不到 ObjC 异常**，于是穿透到 `std::terminate` 把整个
app 带崩。

修法：用官方 `CNContactFormatter.descriptorForRequiredKeys(for: .fullName)`
（框架自己维护所需 key 集），并显式加 `CNContactTypeKey`。

**这条与 keepalive.rs 的教训同源**：原生代码里的失败会杀死宿主进程，而不是
变成可处理的错误。区别是 keepalive 那次是 Rust panic 杀线程，这次是 ObjC
异常杀进程。防法也一样 —— 只用框架文档化的 API（descriptor / 明确的 key 集），
不要手写看起来「差不多够」的字段列表。

**修复后 iOS 真机 11 项全绿**：
```
contacts_search  OK  41ms  {"displayName":"啊连","id":"2CC8F90B-...","phones":[...]}
capabilities     OK   calendar.permission=granted contacts.permission=granted
```

**已知小缺口**：探针 `step()` 把返回文本截断到 300 字符，导致 `contacts_get`
那一步 `JSON.parse` 失败而走了 skipped 分支（产品功能没问题，是探针的限制）。

### 其它实现取舍（首轮已记）
> 以下为首次提交时记录的取舍，保留于此便于回溯。



新增 `contacts` 工具（`op: search|get`），双端实现：iOS Contacts.framework /
Android ContactsContract。

**只读**：不支持写入。agent 误改/误删联系人是不可逆的社交损失，风险与收益
严重不对称，且无产品需求。Android 侧也只申请 `READ_CONTACTS`。

**iOS 未授权路径已验证**（真机）：`contacts_search` 9ms 快速失败 + 指引
（"tap Allow for 通讯录 in Settings → Agent"），`contacts_get` 在无数据时
正确跳过而不是伪造 id 让这一步假失败。

### 与日历一致的纪律与两处特有处理
- 工具**绝不主动弹权限框**（同日历的教训：弹窗会把 agent 调用挂到 JS 侧 30s 超时）
- **iOS 18 的「部分授权」（.limited）如实报 `limited` 而不是 `granted`**：
  报 granted 会让「查不到某人」被误读成「此人不在通讯录里」，进而给出错误结论。
  向上折叠成 denied 让 UI 引导补全（对用户来说动作一致）
- 默认 limit **25 比日历的 50 更低**：通讯录是最容易撑爆上下文的数据源，
  一个号码可能关联十几条字段（手机/工作/家庭/邮箱/地址…）
- **不取 `CNContactNoteKey`**：备注需要 `com.apple.developer.contacts.notes`
  entitlement（免费账号拿不到），带上它会让整个 fetch 抛异常
- **Android 走 Data 表两次查询**（先按 DISPLAY_NAME 查 contact id，再按 id 批量取
  字段）——ContactsContract 的经典模型；`LIKE` 的 `%` 作为参数绑定而不是拼进
  selection（避免注入与转义问题）
- 只输出有值的字段：通讯录里大量字段是空的，全量输出会把上下文浪费在
  `"givenName": ""` 这类噪声上

## 2026-09-13 — M6 1b 日历：Android 真机全绿 ✅（iOS 待验）

新增 `calendar_list` / `calendar_create` 两个工具（`plugins/pi-native` 的
第一个双端能力实现）。

**Android 真机 8 项自检全绿**，日历两条：
```
calendar_list    OK  50ms   真实日程（系统「Message」日历里的银行还款提醒）
calendar_create  OK  40ms   {"id":"14168","title":"pi-mobile self-check",
                             "calendarId":15,"timeZone":"Asia/Shanghai",...}
```
同一轮 location 还拿到了**实时 fix**（`fromLastKnown:false, provider:network`,
3726ms）—— 证明上一步的修复不只依赖缓存回退。

### 设计取舍
- **拆成两个工具**，而不是一个带 `op` 参数的：审批策略只看工具名。合成一个的话
  「读」要么被迫标 mutating（每次都弹审批），要么写操作失去审批。拆开后
  读自动放行、写走审批，与其它能力一致。
- **时间一律 epoch 毫秒**：模型不用猜时区/日期格式，两端也不用各自解析
  ISO8601 —— 这类地方最容易出现「差一天/差几小时」的静默错误。
- **未给 endMs 时默认 +1 小时**：日历事件必须有 end，让模型每次算一遍容易
  造出 0 长度事件。
- **权限未授返回可执行指引，而不是空列表**：空列表会让模型得出「用户最近
  没有安排」这个**错误结论**。
- list 默认 7 天窗口 + 50 条上限：一次查询可能命中数百条，全塞进上下文既
  费 token 又淹掉真正相关的那几条。

### iOS 17 的日历权限坑（已处理）
iOS 17 把日历权限拆成「完整访问」与「仅写」，Info.plist 的键也随之拆分。
**只声明老的 `NSCalendarsUsageDescription` 时，在 iOS 17+ 上请求完整访问会被
系统直接拒绝** —— 不弹窗、不报错，就是拿不到权限。三个键都已在
`project.yml` 声明，`Calendar.swift` 按系统版本分流
（`requestFullAccessToEvents` / `requestAccess`），并在
`.writeOnly` 状态下如实报错而不是返回空列表。

### Android 用 Instances 而非 Events 表
按时间窗口查询必须走 `CalendarContract.Instances`（重复事件的每一次发生
才会展开），`Events` 表只有原始行的 DTSTART。写入前先确认存在**可写**日历
（`CALENDAR_ACCESS_LEVEL >= CONTRIBUTOR`）—— 只读的节假日/订阅日历会导致
写入失败，且报出的底层错误对模型毫无意义。

### 踩坑：`parseArgs(JSObject::class.java)` 静默给出空对象
真机表现极具迷惑性：`calendar_create` 返回了和 `calendar_list` **一模一样**
的 3819 字节列表，且因为走的是 list 成功路径而**毫无报错**。

根因是 Tauri Android 的一个 API 陷阱：`parseArgs` 用 Jackson 把 argsJson
反序列化进 `JSObject`（org.json 风格，构造器吃 JSON 字符串），实测得到**空
对象**；于是 `args.optString("op", "list")` 落到默认值 "list"。正确 API 是
`invoke.getArgs()`（直接用 argsJson 构造）。

**教训（已影响后续验证方式）**：探针只断言 ok/失败是不够的 —— 它当时报了
ok，但结果形状是错的。通讯录/照片会按「断言返回形状」而不只是「断言成功」
来验证（例如 create 必须返回带 id 的小对象，而不是事件列表）。

### 待办
- [ ] **iOS 日历真机验证**（代码已写、三端编译通过、Info.plist 三键已就位，
      但尚未上机跑过）
- [ ] 1b 剩余：通讯录 / 照片
- [ ] Android 定位已修（自建 LocationManager）；剪贴板/通知走官方插件可用
## 2026-09-13 — M6 系统原生能力第一批 1a：剪贴板/通知/定位/天气 ✅

**iOS 真机全绿**（授权后自检实测）：
```
location         OK  31ms   22.5676,113.8910  精度 11.7m  海拔 13.4m
weather          OK 996ms   Open-Meteo 真实数据（小雨 26.7°C / 体感 30.7°C / 湿度 86%）
clipboard_write  OK  30ms
clipboard_read   OK  43ms   读回 "pi-mobile self-check"（双向可用）
notify           OK  18ms   通知发送成功
```

### 架构：为什么另开 `native` 通道而不是塞进 `tool`
`loopback.rs` 的 `tool` 是 **workspace jail 内的文件操作**（D6：无 exec、
路径越狱防护）；系统能力读的是**真实用户数据**（位置/剪贴板/通讯录）。
两套信任模型混在一起会让 jail 语义变模糊，审计时也看不清哪些调用碰了
真实数据。所以单开 `native` hostcall，工具名与权限在
`src-tauri/src/native/mod.rs` 的 `CAPABILITIES` 里统一登记（单一真源）。

### 原生实现的选型纪律（承 keepalive.rs 两次真机事故）
`keepalive.rs` 的记录：从 Rust 走 `ndk_context` 裸 JNI 会因 panic 杀死宿主
线程（审批决策丢失）或带崩 wry 事件循环。因此本层纪律是「**能用官方插件
就用插件**」—— 插件把 Android JNI / iOS ObjC 管线封在各自原生侧，Rust 只
调 `run_mobile_plugin`。当前用插件：剪贴板 / 通知 / 定位。

选型前的关键侦察：**插件的 JS API 面向 WebView，而 agent 跑在 bun 里** ——
所以必须先确认插件暴露可用的 Rust API。三个插件都有
（`ClipboardExt` / `NotificationExt` / `GeolocationExt`），故 4 项能力
（含天气）**零自定义原生代码**即可落地。日历/通讯录/照片没有插件，
才需要自建 Tauri 插件（1b）。

### Bug 1【致命】同步 Tauri 命令 + 需要主队列的插件 = 死锁
症状：设置页点「定位 Allow」→ 整屏卡死。

根因：Tauri 同步命令在宿主线程执行，而 `run_mobile_plugin` 是阻塞的
（等原生侧回调）。`tauri-plugin-geolocation` 的 iOS 实现在
`.notDetermined` 时把 invoke **挂起直到用户作答**，并且弹窗走
`DispatchQueue.main.async` —— 同步命令占着主线程等回复、弹窗等主线程，
互等成环。通知插件同样挂起 invoke，但它的弹窗不需要主线程，所以
「其他权限正常、只定位卡死」，正好指向这个机制。

修法：两个 UI 命令改成 `async fn` + `spawn_blocking`，与仓库里
`pi_bun_smoke` 的既有纪律一致（「任何同步阻塞调用都不得占主线程」）。

附带确认：agent 工具侧是安全的 —— loopback HTTP 的 handler 跑在每连接
独立的 `thread::spawn` 上，不占主线程，所以模型调 `location` 不会触发
同类死锁。

### Bug 2 设置页 Agent 标签无法滚动
SheetContent 是 `flex flex-col` + `h-full`，每个标签页必须自带
`flex-1 overflow-y-auto` 才能滚。Providers/MCP/Skills 都有，Agent 页漏了
—— 之前内容短没暴露，加了 Device 卡片后就溢出。已按 Providers 同款写法
整页包进滚动容器。

### 设置页信息架构：Device 标签取消，并入 Agent
审批策略管「工作区内的文件改动」，设备能力管「工作区外的真实用户数据」
—— 两者都是 agent 的权限边界，放同一页才不会让用户以为还有第二组开关。
现在标签页：Providers / MCP / Skills / Agent（内含 Agent behavior +
Device access 两个分组）。

### 平台声明（缺了就崩，不是返回错误）
iOS Info.plist 逐项补用途描述，文案原则是「agent 拿它做什么」而非
「我们需要此权限」（审核会读，用户也会在系统弹窗看到）。Android 侧只加了
定位两个权限（`ACCESS_COARSE_LOCATION` 必须一起声明，否则 Android 12+
弹窗会略过「仅大致位置」选项）。

**流程坑（已踩）**：`tauri ios build` **不会**从 `project.yml` 重新生成
Info.plist —— 改完必须手跑 `xcodegen generate`，否则用途描述不进包、
运行时直接崩。

### 开发期自检（debug 构建才跑）
`pi-bundle/netprobe.js` + `nativeprobe.js`：直接打 `hostcall` 而非经模型，
避免「模型可能不调/调错」带来的验证不确定性。两者都遵守同一条纪律：
**脚本不能返回 Promise**（`skal_evaluate` 的 `waitForPromise` 会阻塞 VM
worker 线程，而被探测的 fetch/插件回调恰好靠该线程 tick），立即返回
`"started"`、结果增量写全局槽位、宿主轮询。统一收敛到
`pi_bun::run_probe`，`#[cfg(debug_assertions)]` 下在后台线程跑（不阻塞启动）。

### 1b 前置侦察：iOS 插件的 Swift 是怎么进的包（决定自建插件形态）
排查时先看到的现象很容易误导：`project.pbxproj` 里**没有**任何 Swift Package
引用，Podfile 也没有插件，`Sources/` 只有 `main.mm` —— 但
`nm` 能查到 `_$s24tauri_plugin_geolocation10initPlugin…`、类名
`GeolocationPlugin`。

真相在 `tauri-plugin::Builder::try_build()` 的 `build/mobile.rs`：
*iOS 分支*（`#[cfg(target_os = "macos")]`，即 macOS 主机交叉编译 iOS 时）
调 `tauri_utils::build::link_apple_library(name, ios_path)`，把 `ios/` 下的
Swift 包当作静态库**直接链进 Rust 产物**，同时把 `tauri-api` 拷进
`ios/.tauri/`。所以 Swift 符号在 `libapp.a` 里，Xcode 工程保持干净。
*Android 分支*则把 `android/` 的 `cargo:android_library_path` 交给 gradle。

**结论（1b 怎么做）**：自建插件可以完全自包含，**不需要改 Xcode 工程**
—— 与「改完 `project.yml` 要手跑 `xcodegen generate`」的现有流程也不冲突。
需要的骨架（照 `tauri-plugin-geolocation` 抄）：
```
plugins/pi-native/
├── Cargo.toml          # links = "pi-native"，build-dependencies tauri-plugin (features=["build"])
├── build.rs            # Builder::new(COMMANDS).ios_path("ios").android_path("android").try_build()
├── ios/Package.swift   # 依赖 ../.tauri/tauri-api，target path = "Sources"
├── ios/Sources/*.swift # @objc Plugin 子类 + 各能力实现（EventKit/Contacts/Photos）
├── android/build.gradle.kts  # namespace + implementation(project(":tauri-android"))
├── android/src/main/AndroidManifest.xml
├── android/src/main/java/…/*.kt  # @TauriPlugin + @Command + app.tauri.plugin.{Invoke,Plugin,JSObject}
└── permissions/default.toml
```

### Android 真机验证结果（1a）

| 工具 | Android | 证据 |
|------|---------|------|
| `clipboard` write/read | ✅ | `hostcall native: clipboard ok (28/20 bytes)` |
| `notify` | ✅ | `hostcall native: notify ok (39 bytes)` |
| `location` | ❌ | `get_current_position: Location unavailable.`，且会间歇性挂死 30s |
| `weather` | ❌（依赖定位） | 传播了上面的定位错误 |

#### 定位为何在 Android 上坏掉（环境 + 插件缺陷叠加）
- 权限与开关都正常：`ACCESS_FINE/COARSE_LOCATION: granted=true`、
  `settings get secure location_mode` = 3。
- 设备环境：`dumpsys location` 显示 `realProvider=AMAP_WIFI`（国内 ROM 用
  高德代理网络定位），**GPS provider 不可用**，且唯一的上次位置是 21 小时前。
- 插件实现：`LocationServices.getFusedLocationProviderClient(context)
  .getCurrentLocation(prio, null)` —— **传了 null CancellationToken、无超时**
  （插件文档也写明 Android 上 `timeout` 参数会被忽略）。国内 GMS 定位不可用
  时拿不到 fix，回调既不 success 也不 failure → 永久挂起，最后被 JS 侧
  30s hostcall 超时打断，报出无信息量的 `The operation timed out`。

综合结论：Android 侧定位不能靠 Google fused provider，得自己用
`LocationManager`（这个 ROM 会把它桥到高德代理）+ 真超时 + last-known 回退。
这是 `plugins/pi-native`（1b 本就要建）的第一个 Android 实现。

#### 顺带修掉：Android 侧一直是盲调
Honor 的 logcat 又一次丢弃了 tag 输出（`logcat -s pibun` 只剩 1 行 ——
笔记里记过这个 ROM 会加密/丢任意 tag），而文件通道在 Android 上也失效
（`HOME` 在应用进程里不存在 → 回退到不可写的 `/tmp`）。
现改为统一写 **data_dir/pi-bun.log**，两端都可靠：
- iOS：`devicectl device copy from --domain-type appDataContainer`
- Android：`adb shell run-as com.sternelee.pi_mobile cat pi-bun.log`
注意 Android 上 `app_data_dir()` 返回的是 **data 根目录**（日志/`sessions`/
`workspace`/`provider.json` 都在那里），不是 `files/`。

### 待办
- [x] 1a iOS 真机验证（剪贴板/通知/定位/天气 全绿）
- [ ] 1a Android 定位修复（→ `plugins/pi-native` 的 Kotlin 实现；
      剪贴板/通知已验证可用）
- [ ] 1b：日历 / 通讯录 / 照片 —— 自建 `plugins/pi-native`（骨架见上）
- [ ] bundle 体积：`dist/agent.js` 已 2.94MB 且未 minify —— 对无 JIT 的
      iOS 解释器是每次冷启的解析成本。build.sh 的后处理依赖**未压缩**源码
      形态（正则匹配 `import.meta.require`、`^import …`），开 minify 需先
      重构后处理，别直接加 flag。

---

## 2026-09-13 — M5 iOS 真机跑通（自测成功）+ 三个真机 bug 复盘 ✅

**成果**：iPhone SE（iOS 26.5.2）上 pi-mobile 完整跑起来 —— `libskal.dylib`
从源码构建、dlopen 成功、agent bundle 求值完成、tools/skills/providers
全部注册、首条对话可用。用户自测确认。

### 真机日志的关键证据
```
[pi-bun] runtime up: handle=4521553920 reused=0 bun-pins=1.3.14
[pi-zig] eval enter → enqueue → vm-thread run enter → vm-thread evaluated → wait returned
[pi-bun] agent_event: agent_ready
[pi-bun] agent_event: mcp_tools_registered
[pi-bun] agent bundle kicked
[pi-bun] agent_event: providers_listed
```

### Bug 1【致命】SyncReply 整体赋值 ⇒ skal_evaluate 永久挂死
真机症状：`runtime up` 之后一片静默 —— agent 不就绪、"loading models"
无限转圈、"Sign in with Kimi" 无反应。

根因：宿主线程阻塞在 `reply.done.wait()` 期间，worker 线程执行
`reply.* = .{ .result_buf = ..., .is_error = ... }` —— **整体结构体赋值把
同步原语 `done` 一并重置成全新的 ResetEvent**。在 waiter 等待期间覆写
同步原语是数据竞争，唤醒丢失 ⇒ 永久挂起。
修法：逐字段写 + 最后 `set()`（`SyncReply.complete`）。

为何之前一直没暴露：Android 走 skal **预构建** dylib（用 skal 自己的
evaluate 实现），我们这层 `skal_*` 兼容导出是本次真机才第一次被真正调用。

教训：**给 waiter 用的同步对象不能被「整体赋值」碰** —— 先写载荷，
最后发信号。

### Bug 2【致命】iOS 选错 DNS 后端 ⇒ 所有外网请求失败
`vendor/bun/src/dns/dns.zig` 的 default backend 只给 `.mac/.windows`
选 `.system`，其余（含 `.ios`）落到 `.c_ares`。而那段代码的注释自己就写了
c-ares 为何不可用：「can't discover nameservers (no /etc/resolv.conf)」
—— iOS app 沙箱同样读不到 resolv.conf，于是 c-ares 去连 127.0.0.1:53，
实测 `DNSException: getaddrinfo ECONNREFUSED`。

修法：`.ios => .system`（libc getaddrinfo → 系统 resolver）。
修后错误变成 `ENOTFOUND`，确认已切到系统 resolver 路径。

补丁落 `patches/bun-dns-ios-system.patch`，由 `setup-bun-fork.sh` 统一
`git apply`（幂等：已应用则跳过）—— 否则 vendor/ 的改动会丢。

### Bug 3 iOS 聚焦输入框页面自动放大
WKWebView 老行为：聚焦 `font-size < 16px` 的表单控件时自动放大。我们正好
踩上：`.composer textarea` 0.92rem≈14.7px、`.ask-input` 0.82rem≈13px、
`.session-search` 0.9rem≈14.4px。

修法：把控件字号抬到 16px，**而不是** viewport 写 `maximum-scale=1`
（后者会一并禁掉用户捐合缩放，牺牲无障碍）。限定在
`@supports (-webkit-overflow-scrolling: touch)`；规则放文件末尾且不在任何
`@layer` 内（非 layer 声明优先于 layer 内声明，且同特异性下后写胜出）。

### 配套的可观测性（真机排障必需）
- **iOS 日志通道**：`println` 进统一日志但 `devicectl` 拉不到，
  `idevicesyslog` 在 CoreDevice 隧道占用 uSMux 后连不上设备。
  ⇒ `logcat` 现在同时写 `<HOME>/Documents/pi-bun.log`，用
  `devicectl device copy from --domain-type appDataContainer` 拉回。
- **Zig 侧 `trace()`**（同文件同格式）—— 直接拿到 VM 线程内部时序，
  就是它坐实了 Bug 1。
- `agent_init` 失败串（含 JS 侧 `boot_error`）现在进设备日志。
- `pi-bundle/netprobe.js`：DNS / loopback / 外网 HTTPS（域名）/ HTTPS（IP）
  四步探测，iOS 启动时自动跑一次 —— 就是它定位了 Bug 2。
- `scripts/ios-device-run.sh`：一键装机 + 控制台启动。

### 其他真机确认项
- **JIT 已正确关闭**：`workerMain` 里在 `bun.jsc.initialize()` 前
  `setenv("JavaScriptCoreUseJIT", "0")`。注意 `getenv` ≠ Zig
  `std.os.environ`（后者是启动时快照，而 `JSCInitialize` 读的正是它）——
  所以 `BUN_JSC_*` 那套在此无效。
- **HOME 不能改**：早期版本在 `skal_create_runtime` 里 `setenv("HOME", dir)`，
  导致 Tauri `app_data_dir()`（= `$HOME/Library/Application Support/<id>`）
  算出双层嵌套路径 `<dir>/Library/Application Support/<id>`，两份
  sessions/workspace 分裂。改回 skal 上游做法：不碰环境变量，
  只装 JS 全局 `__pi_data_dir` / `__skal_data_dir`（bundle 实际走
  `__PI_CONFIG.dataDir`）。
- **iOS 签名 team**：`project.yml` 必须是 Xcode 已登录账号的 team。
  开发证书 CN 括号里的号（WRZ67HMJUL）与 OU（UJ8NW4N779）不一致 ——
  以 `defaults read com.apple.dt.Xcode IDEProvisioningTeamByIdentifier`
  为准。卸载重装后需重新在设备上「信任」开发者。

### 待办
- [ ] 真机端到端对话 + 工具 round-trip 截图存档
- [ ] 模拟器路径（预构建 iossim dylib）一并验证
- [ ] 脚本幂等性：二次跑 build-jsc-ios.sh / ninja 应为秒级跳过
- [ ] 清理：`pi-bundle/netprobe.js` 的启动自跑可改为按需触发

---

## 2026-09-12 00:40 — M5 出口条件达成：iOS 真机 libskal.dylib 从源码构建 ✅

**成果**：`build/skal-ios-device/libskal.dylib`（64.4MB，platform IOS，
minos 16.0）已嵌入 `build/arm64/pi-mobile.ipa`，deep codesign verify 通过。

### 管线（三个脚本，全部可重跑）
```
WebKit(skal 分支 pin c1bdd50) ──┐
                               ├─► scripts/build-jsc-ios.sh ─► libJavaScriptCore.a
bun fork(skal 分支 pin dfcbb2b)─┤        (cmake JSCOnly + ninja jsc)
                               └─► bun ios-release ─► 1124 个 .o + bun-zig.{0..11}.o
                                                        │
                          scripts/link-skal-ios.sh ◄────┘
                          (llvm@21 clang++ -target arm64-apple-ios16.0)
                                        │
                                        ▼
                          build/skal-ios-device/libskal.dylib
                                        │
                    gen/apple/project.yml (Embed Frameworks) ─► pi-mobile.ipa
```

### 关键技术发现
1. **WebKit 仓库必须走镜像**：直连 GitHub clone 反复在 ~1GB 处
   `early EOF`（3 次尝试，各 ~1h）；GitHub tarball 因仓库超限返回 422。
   用 `https://gh-proxy.com/https://github.com/...` 前缀 9 分 18 秒完成
   （1.64GiB，464897 文件，~3.5MiB/s）。**这是关键路径上唯一的阻塞点。**2. **WebKit 单独构建极快**：M2 Pro 12 核上 `ninja jsc` 仅 **4 分 24 秒**
   （3080 targets），远低于文档预估的 1-2h。bun ios-release 内含 WebKit
   nested cmake 会重复编译，故 JSC 只需构建一次。
3. **bun iOS 的 `bun-profile` link 步骤本身是坏的**：build.ninja 的
   `bun-profile.rsp` 里没有任何 `-target`/`-isysroot`，链接器按 macOS 目标
   处理 iOS object → `building for 'macOS', but linking in object file built
   for 'iOS'`。这正是 skal 用独立 link 脚本取 `.o` + `-rsp` 自行链接的原因。
   **object 文件本身完全正确**，只有 bun 的最后一步 link 不可用。
4. **`skal_entry.zig` 从未被编译过**（直到本次）：M1/M2 走的是 skal 预构建
   `libskal.so`，那个文件绕过了自建 zig 源码。首次真编译暴露 4 处错误：
   - `AnyTask.New(EventPumpTask, run)` → `run` 未限定（应 `EventPumpTask.run`）
   - `@intFromPtr` 返回 `usize`，函数声明 `i64` → 需 `@intCast`
   - 本 zig 版本 `std.posix` 无 `setenv` → 改 `extern "c" fn setenv`
   - **我们的 zig 导出 `pibun_*`，而 Rust 侧绑定 `skal_*`** → 补 4 个
     `skal_*` 兼容导出（create/evaluate/free_string/runtime_was_reused）。
     这是 ABI 三方镜像（pi_bun.h / ffi.rs / bridge.ts）之外的第 4 处
     一致性要求，已记录在代码注释。
5. **iOS 签名 team 修正**：`project.yml` 原写 `WRZ67HMJUL`，但 Xcode 实际
   账号是 `UJ8NW4N779`（Personal Team）→ `No Account for Team` 报错。
   注意证书 CN 里的括号号（WRZ67HMJUL）与 OU（UJ8NW4N779）不一致，
   以 Xcode 账号列表（`IDEProvisioningTeamByIdentifier`）为准。
6. **iOS dylib 搜索路径**：XcodeGen 的 `framework:` 依赖只加
   `-lskal`，不会自动补 `Externals/arm64`（原配置只有
   `Externals/arm64/$(CONFIGURATION)` 供 libapp.a 用）→ 需显式加
   `$(PROJECT_DIR)/Externals/arm64`。

### 改动文件
- `scripts/build-jsc-ios.sh`、`scripts/link-skal-ios.sh`（新增，从 skal 适配）
- `scripts/link-skal-ios.sh`：导出符号列表裁剪为实际存在的 4 个 skal_*
- `src-tauri/gen/apple/project.yml`：Embed libskal.dylib + 部署目标 16.0
  + DEVELOPMENT_TEAM 修正 + `**/*.dylib` 从 sources 排除（防重复拷贝）
- `src-tauri/src/pi_bun/mod.rs`：iOS 分支从「返回不支持」改为 dlopen
  `@rpath/libskal.dylib`（移除 `cfg(target_os="ios")` 的错误分支与
  `cfg_attr(ios, allow(dead_code))`）
- `vendor/bun/src/skal_entry.zig`：4 处编译修复 + skal_* 兼容导出

### 验证
- `cargo build --target aarch64-apple-ios --release` ✅（1m13s）
- `bun tauri ios build --debug` ✅ 零警告 → `build/arm64/pi-mobile.ipa`
- `codesign --verify --deep --strict` ✅
- app 与 dylib 同 team `UJ8NW4N779`，dylib install_name
  `@rpath/libskal.dylib` 与 Rust 侧 dlopen 字串一致

### 待办
- [ ] **真机安装验证**：设备（iPhone SE, `00008030-000A21391A83802E`,
      iOS 26.5.2）当前未连接，CoreDevice 不可达（error 1011）。接上 USB
      后跑 `xcrun devicectl device install app --device <UDID> <ipa>`。
- [ ] 真机验证 agent boot：dlopen 成功 → `runtime up: handle=…` →
      `agent bundle kicked` → 填 key → 首条对话（含工具 round-trip）
- [ ] 复跑脚本的幂等性验证（二次运行应秒级跳过）
- [x] 文档：README 的 iOS 章节已补「从源码构建 JSC」流程 + JIT 合规说明

### JIT 合规排查（重要）
初版构建并未处理 JIT。排查后发现 `jitEnabledByDefault()` 返回
`isAddress64Bit()` —— arm64 上恒为 true，且 `ENABLE_JIT=ON`（已确认
CMakeCache），而 bun 又硬编码 `JSC::Options::useJIT() = true`。

WebKit 在真机靠两处降级（`runtime/VM.cpp: enableAssembler`）：
- **① `getenv("JavaScriptCoreUseJIT")`**（VM.cpp:206）
- ② `isJITEnabled()` 检查 `dynamic-codesigning` /
  `com.apple.developer.cs.allow-jit` entitlement
  （ExecutableAllocator.cpp:146）

只依赖 ② 不稳：reservation 为空但 `isValid()` 可能仍为 true，
分配 exec 页时才失败。**采用 ①**：在 `workerMain` 里于
`bun.jsc.initialize()` 前 `setenv("JavaScriptCoreUseJIT", "0", 1)`。

两个易错点（已写入代码注释）：
- **`getenv` ≠ Zig `std.os.environ`**：后者是启动时快照的切片，而
  `bun.jsc.initialize` 传给 `JSCInitialize` 的正是它 —— 所以
  `BUN_JSC_*` 那套（JSCInitialize 内的 setOption 循环）在此**无效**。
- **时序**：`canUseAssembler()` 用 `std::call_once` 缓存，由
  `JSC::initialize()` 内的 `VM::computeCanUseJIT()` 触发 —— 都在
  `bun.jsc.initialize()` 里，setenv 必须排在它之前。

结果：`Options::useJIT()=false` + `notifyOptionsChanged()` 级联关掉
useWasm 等依赖项，全程解释器。编译期仍构建 JIT 代码（DOMJIT/DFG
类型依赖），不执行 —— 与 bun Android 预构建同策略，App Store 合规
（RN 同款先例）。已用 `strings` 验证字符串进入 `bun-zig.2.o` 与最终
`libskal.dylib`。

---

## 2026-09-07 13:10 — 双代理代码审查 + 配置/用量/压缩五项 ✅

### 双代理审查（bundle + Rust 全量并行审查）
- **[高] MCP 工具绕过审批**：ASK_TOOLS 白名单不含 `mcp__` 前缀，bundle 发的
  审批请求被直接 allow → 已加 `starts_with("mcp__")` 纳入 ask（D11 语义恢复）。
- **[中高] diff 截断 UTF-8 panic**：中文内容必然踩多字节边界 → char_boundary
  安全截断。
- **[中] Content-Length 无上限**：畸形声明一行打崩（vec![0u8; usize::MAX]）→
  8MB 上限 + 413。
- **[中] __pi_prompt JSON 嗅探**：用户粘贴的普通 JSON 被当消息对象（无 role
  被 convertToLlm 静默丢弃 + 写入畸形 JSONL）→ 校验 role 字段。
- **[中] read_workspace_rel 漏反斜杠检查**（approval diff 可越狱读文件）→ 补齐。
- **[低] mcp.json 原子写（tmp+rename）+ 损坏取证**；**session_list 改
  spawn_blocking**（原同步命令在主线程逐行扫全量会话）。
- `pi_call_global` 注册 / open_session kick 模式 / provider 选择 / skills /
  oauth 已由并行开发完成，审查确认无同类问题。
- 已知未修（记录在案）：备份名 `a/b` vs `a__b` 理论碰撞；approval "always"
  无 UI 恢复入口；`__pi_tool_call` 异步缝仅测试用。

### 五项体验功能
- **MCP 粘贴 JSON**：抽屉 MCP 区 "{ } Paste JSON"——支持单服务器对象、数组、
  `{"mcpServers":{...}}`（Claude Desktop 同款）批量导入 + 自动重连。
- **API key 输入框改 `type="text"`**（首启表单 + 设置抽屉两处）。
- **Token 用量显示**：顶栏徽标（`12.3k tok`）——实时累计 assistant
  usage.totalTokens；boot/切会话从恢复历史重算；新建会话清零。
- **自动压缩**：上下文水位（最近 assistant usage.totalTokens）超过模型窗口
  60% 时，下次 prompt 前把较早消息经只读嵌套 Agent 压成摘要（保留最近 8 条），
  JSONL 保留完整历史；UI compaction 状态行。
- **Provider 自定义**：验证并行开发已落地的抽屉 provider/key/model catalog +
  `__pi_model_select` 热切换 + oauth 接入无遗漏。

---

## 2026-09-07 12:40 — OAuth 订阅登录（Anthropic/OpenAI/Kimi/xAI + pimobile://）✅

### provider 登录从"仅 API key"到"订阅 OAuth"
- **8 家 provider**：原 4 家（OpenAI/OpenRouter/DeepSeek/Gemini）+ 4 家 OAuth
  订阅型——Anthropic (Claude Pro/Max)、OpenAI Codex (ChatGPT)、
  Kimi For Coding、xAI (SuperGrok/X Premium)。pi-ai provider 自带
  lazyOAuth：refresh/toAuth 纯 fetch，`registerBunOAuthFlows()` 静态内嵌
  流程模块后自动续期。
- **回调捕获双通道**（pi-ai login() 硬绑 node:http，嵌入运行时没有）：
  1. **本地回调（主）**：宿主一次性 HTTP server（`oauth_listen`，同步
     bind 支持端口 0=OS 分配）——provider client_id 只注册了
     `http://localhost:<port>` 回调，redirect_uri 原样保留；捕获后经
     evaluate_blocking 注入 `__pi_oauth_callback(url)`，浏览器回成功页。
  2. **`pimobile://` deep link（辅）**：AndroidManifest intent-filter +
     tauri-plugin-deep-link，`pimobile://oauth/callback?...` → 同一注入
     通道（给未来允许自定义 scheme 的 provider）。
- **PKCE 宿主生成**（`oauth_pkce`）——嵌入 JSC 的 crypto.subtle 可用性
  不赌。device 流（kimi/xai/codex 设备码）无回调，直接复用 pi-ai
  `oauth.login(interaction)`，notify(device_code) → 宿主自动开验证页。
- **凭证**：OAuth JSON 走独立 `creds_json` hostcall（`{provider}#oauth`
  隔离条目）；`has_creds` 兼容双形态。UI provider 卡片新增
  "Sign in with …" 按钮；oauth_open_url 事件宿主自动唤起浏览器。
- **测试**：Rust 18/18（PKCE 形状/回调捕获含关停验证）；bundle 11/11
  （oauth-test：mock 宿主全流程——授权 URL/PKCE/交换/落库）。

---

## 2026-09-07 10:30 — 技能自定义指令（pi TUI /commit-it 语义）✅

### Skills → 斜杠命令
- **frontmatter `command: <slug>`**（可选）：声明后技能暴露为自定义指令
  `/slug`（如 `/commit-it`）。Rust `skills_config` 注入时携带 `command`
  字段；未声明的技能保持纯 systemPrompt 注入。
- **bundle 展开**：`__pi_prompt("/commit-it stage files")` → 匹配 skillsCache
  → 展开为 `Follow the "<name>" skill …: <args>`（技能正文已在 systemPrompt，
  展开只发意图 + 参数，不重复注入）；未匹配的 /x 原样透传。
  新增 `__pi_commands()` 诊断缝。
- **UI**：`skills_applied` 事件后回读命令清单，命令面板合并展示内置 +
  技能命令；未知斜杠命令匹配技能时透传 agent_prompt，否则提示。
- **测试**：Rust 16/16（command frontmatter 注入暴露）；bundle 10/10
  （pi-commands-test 增 `__pi_commands` 清单 + /commit-it 展开断言）。

---

## 2026-09-07 09:40 — agent `fetch` 工具（方案 B：宿主 reqwest）✅

### 网络能力收敛到宿主
- **agent 新增第 6 个内置工具 `fetch`**：`{url, method?, headers?, body?}` →
  `{status, contentType, body, truncated}`。hostcall `http` →
  `http_tool.rs` reqwest blocking + rustls（与 skills 安装器同栈）：30s 超时、
  响应体 256KB 上限（take+truncate）、UA `pi-mobile-agent/0.1`。
- **SSRF 防护**：只允许 http/https；拒绝 localhost/*.local/0.0.0.0/私网/
  环回/链路本地/IPv6 ULA——否则模型可经 fetch 打进本机 loopback hostcall
  （creds_get 端口）。已知边界：无 DNS 解析级校验（rebinding 理论绕过），
  白名单/审计留策略层。
- **HTML → 纯文本**：regex 剥 script/style/noscript/svg/注释/标签、常见
  实体解码、空白折叠——LLM 读正文不读原始标记。
- **审批面**：只读类，不走 approval（未来域名白名单在宿主策略层做）。
- **测试**：Rust 15/15（SSRF 黑名单 / HTML 转文本 / 截断）；bundle 11/11
  （新增 fetch-test：注册 / hostcall 透传 / SSRF 拒绝面）。

---

## 2026-09-07 00:20 — UI/UX 2.0：ChatGPT 移动端参考重设计（对话/会话列表/设置页）✅

### 设计原则（apple-design 技能落地）
- **材质分层**：topbar 与 composer 改半透明 + `backdrop-filter: blur(20px)
  saturate(180%)`——内容从其下滚过，层级靠材质而非硬分割线。
- **气泡层级**（ChatGPT 式）：user 右对齐 `surface-3` 胶囊（20px 圆角）；
  assistant 全宽无框排版，copy 动作右上角。
- **按压即时反馈**：所有可点卡/按钮 `:active` 即刻 `scale(0.97)`（100ms
  ease-out）——反馈发生在 pointer-down 而非松手。
- **可访问性双降级**：`prefers-reduced-motion` 全动效转淡入淡出；
  `prefers-reduced-transparency` 材质面转实底。

### 三个界面
- **对话界面**：composer 胶囊化（textarea 与发送键同住 24px 圆角胶囊，
  focus 描边）；滚离底部 >240px 浮出跳底按钮（`@solid-primitives/scroll`
  的 `createScrollPosition` 响应式跟踪，跳转按 `prefers-reduced-motion`
  选择 smooth/auto——`@solid-primitives/media`）。
- **会话列表**：搜索过滤（id 前缀）+ Today/Yesterday/Earlier 分组 + 提级
  的 "＋ New chat" 主按钮；设置入口为行式导航项。
- **设置页**：从会话抽屉独立成整页导航（根列表 → AI Model / MCP Servers /
  Skills 三个子页，行式导航 + chevron + 返回），根列表行内直接显示当前
  模型 / 配置数 / 启用数。会话抽屉回归纯粹的会话管理。
- **草稿持久化**：composer 输入经 `@solid-primitives/storage` 的
  `makePersisted` 落 localStorage——误杀进程不丢草稿。

### 验证
- tsc ✅；vite build ✅（CSS 28.2KB / JS 126KB）；bundle 测试不涉及 UI 层。
- APK 构建成功；装机待设备重连（本次 adb 断连）。

### 下一步
- [ ] 装机走查新 UI（跳底按钮、搜索分组、设置页导航、材质层次）
- [ ] 真机交互验证存量项：provider 选择流程、锁屏保活、/goal autoContinue
- [ ] MCP per-server 审批粒度（D11 完整版）
- [ ] M5：libpi-bun iOS 静态链路

---

## 2026-09-06 23:30 — M4：pi-goal autoContinue 自动续跑（带上限）✅

### 状态机（bundle，上游 Sisyphus 语义 + 移动端安全边界）
- **触发**：goal 存续期间每个 `agent_end` 自动续跑（"Continue working
  toward the current goal. If the goal is fully achieved, reply with
  exactly GOAL_COMPLETE and nothing else."）。
- **四条退出路径**：① 模型逐字答复 `GOAL_COMPLETE`（→ `goal_auto_done`，
  状态行提示 goal achieved）；② 用户 Stop（抑制紧随的 agent_end）；
  ③ 上限 `GOAL_AUTO_CAP = 10` 次续跑；④ 运行错误（goal_error）。
- **预算重置**：任何用户手动 prompt / goal 变更（`__pi_goal_apply`）都把
  计数归零——失控有界（每轮用户交互最多 10 次自动续跑），交互即重新交权。
- **时序坑（真行为差异）**：`agent_end` 发出时 prompt promise 尚未
  resolve——立即 `agent.prompt()` 报 "already processing"。goalAutoRun
  带退避重试（100ms × 30），每轮重试前复查 Stop 标志。
- **abort 行为差异（测试抓出）**：`agent.abort()` 中止 LLM 流时**不经过**
  failure-message 路径（那只在流错误时发 agent_end），抑制标志会残留到
  下一轮用户交互。修复：`__pi_prompt` 一并重置抑制标志——用户 prompt
  本身就是"重新交回控制权"的语义。
- **UI**：goal 横幅内联 `· auto 3/10` 计数（goal_auto_continue 事件驱动），
  goal achieved / 续跑失败走状态行。

### 测试
- 新增 `pi-bundle/goal-auto-test.js` 四场景（假 LLM + mock loopback）：
  自动续跑触发与续跑 prompt 断言、GOAL_COMPLETE 停机、Stop 抑制（无失控）、
  上限 10 次跑满即停。全量 10/10 bundle 测试 ✅；tsc ✅。

### 下一步
- [ ] 真机交互验证：/goal + 多步任务自动续跑、锁屏存活（配合前台服务）
- [ ] MCP per-server 审批粒度（D11 完整版）
- [ ] M5：libpi-bun iOS 静态链路

---

## 2026-09-06 22:35 — M4：前台服务保活 + 通知（keep-alive）✅

### Kotlin ForegroundService（gen/android）
- **静态入口设计**：`start(context, label)` / `notify(context, channel, title, text)` /
  `stop(context)` 全部 `@JvmStatic`——Rust `call_static_method` 直接可达，
  不需要 service 实例或 binder。`onStartCommand` 每次重跑 `startForeground`
  即通知内容热切换（审批待决 ↔ 工作态）。
- **双通道**：`pi_agent_work`（IMPORTANCE_LOW，常驻不打扰）/
  `pi_agent_approval`（IMPORTANCE_HIGH，抬头提醒）。点通知回 App。
- **Manifest**：`FOREGROUND_SERVICE` + `FOREGROUND_SERVICE_DATA_SYNC` +
  `POST_NOTIFICATIONS` 权限；`<service foregroundServiceType="dataSync" exported="false">`
  （shell `am start-foreground-service` 被拒 = 只有应用自身 UID 能拉起，
  预期姿态）。
- **MainActivity**：Android 13+ 运行时请求 POST_NOTIFICATIONS（首启弹一次）。

### Rust keepalive.rs（JNI 桥，非 Android 全 no-op）
- `ndk_context` 拿 WryActivity → `JavaVM::attach_current_thread` →
  `call_static_method` 驱动 Kotlin 静态方法。失败仅记 logcat（best-effort，
  不阻塞 agent 流程）。
- **挂钩点（全自动，无 UI 改动）**：loopback `agent_event` sink 拦
  `agent_start`→start、`agent_end|agent_error`→stop；approval.rs
  `request()`→on_approval_pending、`respond()`/超时→on_approval_resolved。
- **依赖**：`[target.'cfg(target_os = "android")'.dependencies]` jni 0.21 +
  ndk-context 0.1（均在 tauri 依赖树内，零新增体积）。

### 验证
- 桌面 cargo 12/12 ✅；aarch64-linux-android + aarch64-apple-ios 双目标
  check ✅（keepalive 非 Android 全 no-op）；APK 构建 + 装机成功，冷启动
  序列干净（无 AndroidRuntime FATAL）。
- **端到端待用户**：首条消息触发 agent_start 升前台（通知栏出现
  "pi mobile — agent working"），锁屏跑长任务验证不被冻结；审批弹卡时
  通知升高优先级。

### 下一步
- [ ] 真机交互验证：锁屏长任务存活、审批通知、provider 选择流程
- [ ] pi-goal autoContinue（带上限）——配合保活的"口袋 agent"闭环
- [ ] MCP per-server 审批粒度（D11 完整版）
- [ ] M5：libpi-bun iOS 静态链路

---

## 2026-09-06 22:10 — provider 功能真机装机 + 启动验证 ✅（交互验证待用户）

- **装机**：`bun tauri android build --debug` → universal debug APK
  `adb install -r -g` 流式安装成功 → 冷启动进程稳定（pid 稳定存活）。
- **启动序列完整**：skills_applied → goal_applied → **providers_listed**
  （pi-ai 目录在真机工作）→ todo_updated → session_restored → mcp_ready
  + tools_registered，无报错无崩溃。
- **包名踩坑备忘**：applicationId 是 `com.sternelee.pi_mobile`（下划线），
  `am start -n com.sternelee.pi-mobile/.MainActivity` 会报 Activity 不存在；
  用 `monkey -p com.sternelee.pi_mobile -c android.intent.category.LAUNCHER 1`
  免记忆组件名。
- **待用户交互验证**：四家 provider 各配 key 拉列表 → 选模型 → 对话验证
  热切换 → 杀进程重启验证 provider.json 恢复。旧 DeepSeek key 保留但无
  "选择"记录，首启卡片会出现一次，选完即持久化不再出现。

### 下一轮方向（已定：M4 收尾——前台服务保活 + 通知）
1. **▶ M4 收尾：后台保活 + 通知**——前台服务 keep-alive（agent 长任务
   不被系统杀）+ 通知（审批待决 / 长任务进行中）。这是 agent 应用的
   核心生产痛点：锁屏/切后台 1-2 分钟即被冻结，多步任务必废。
2. **pi-goal autoContinue（带上限）**——多步任务自动续跑，与 keep-alive
   配合才真正"口袋 agent"。上限防失控（如单会话 ≤10 次续跑）。
3. **MCP per-server 审批粒度**——D11 完整版：per-server auto 降级 +
   per-tool 记忆。
4. **M5 关键路径：libpi-bun iOS 静态链路**——源码构建 WebKit JSC +
   libpi_bun.a（数 GB 下载、数小时构建，可提前挂后台跑资料下载）。

---

## 2026-09-06 19:40 — M4：AI provider 配置与模型选择（OpenAI/OpenRouter/DeepSeek/Gemini）✅

### 上游即真源：pi-ai createModels 架构
- **provider 语义零手写**：bundle 侧 `createModels({ credentials: hostCredsStore })`
  + 内置 provider 工厂（openai/openrouter/deepseek）。模型目录（compat/
  contextWindow/thinkingLevel）、动态列表刷新（OpenRouter）、凭证解析、
  streamSimple 分发全部来自 `@earendil-works/pi-ai`。旧 STREAM_SIMPLE
  手写分发与 getApiKey 缓存删除。
- **Gemini 兼容层**：内置 google provider 内部驱动 @google/genai（设备实测
  嵌入 JSC SIGSEGV），改用 pi-ai 的 `createProvider` + 自带 openai-completions
  实现 + Gemini 官方 OpenAI 兼容端点；模型数据仍取自 pi-ai 生成的 google
  catalog（仅重映射 api/baseUrl）。
- **选择流程**：UI 首启卡片 + 抽屉 "AI model" 节 → 选 provider → key 入
  creds（D4）→ 自动拉模型列表（静态目录即时；OpenRouter 走
  `models.refresh` 网络刷新）→ 点选模型：`__pi_model_select` 热切换 +
  `set_default_model` 落 provider.json，重启经 `__PI_CONFIG.providerConfig`
  生效。子 agent 取 `agent.state.model` 运行时快照，跟随热切换。
- **Rust**：`has_creds` / `get_default_model` / `set_default_model` 命令；
  `creds_set` hostcall（CredentialStore.modify 写路径）。
- **测试**：pi-bundle/provider-test.js 两阶段（兜底模型 + 目录断言 /
  providerConfig boot 解析），15 断言；全量 9/9 bundle 测试、12 Rust、
  tsc、Android 交叉编译全绿。

### 下一步
- [ ] 真机验证：四家 provider 各配 key 拉列表、切换后对话、重启恢复
- [ ] M4 余项：前台服务 keep-alive、通知（审批/长任务）、pi-goal autoContinue、MCP 按服务器审批粒度
- [ ] M5：libpi-bun iOS 静态链路（WebKit JSC 源码构建）

---

## 2026-09-06 17:25 — iOS 真机跑通：签名闭环 + 首启目录修复 ✅

### 真机端到端（Honor 替换为 iPhone：Sterne 的 iPhone se）
- **Xcode 登录后 CLI 构建走通**：`tauri ios build --target aarch64 --debug`
  产出 pi-mobile.ipa → `devicectl device install app` 装机 → 首启需在
  设置 → 通用 → VPN 与设备管理里信任开发者证书（免费账号标准流程）→
  二次启动正常（devicectl launch 验证）。
- **真机首启日志**：WebView 页面加载完成，无 panic、无 session 错误。
- **顺带修复 session_list ENOENT**：sessions/workspace 目录原先只在
  agent_init 里建——iOS 上运行时门控提前返回导致无人建目录，UI 报
  "session_list failed: ENOENT"。修复：① app_data_dir 无条件创建标准
  子目录（sessions/workspace）；② sessions::list 对缺失根目录返回空数组
  （iOS 首启防御）+ 单测。

### 下一步
- [ ] **libpi-bun iOS 静态链路（M5 关键路径）**：源码构建 WebKit JSC +
  libpi_bun.a + pi_bun 模块静态链接（cfg ios 分支换实现）
- [ ] 桌面同构验证；v2 零拷贝桥

---

## 2026-09-06 14:46 — M5 开工：iOS 平台支持（工程 + 编译 + 模拟器）✅

### iOS 工程落地
- `bun tauri ios init` → `src-tauri/gen/apple`（xcodegen 工程，Podfile/Externals/
  Sources 模板齐备）。Tauri 的 iOS 模板目录名就叫 `apple`，与 Android 并列。
- **Rust 侧 iOS 门控**：`pi_bun::init` 在 `cfg(target_os = "ios")` 下返回明确
  错误（"libpi-bun static link pending"）——libpi-bun 静态库需从源码构建
  WebKit JSC（skal build-jsc-ios.sh + link-skal-ios.sh 工艺，LIBPI-BUN-NOTES
  §2 已预留链路），dlopen .so 路径在 iOS 不可用。App 其余能力（workspace
  工具、会话、MCP/Skills 配置、审批流 UI）在 iOS 全量编译。
- **双目标编译绿**：`aarch64-apple-ios`（真机）+ `aarch64-apple-ios-sim`
  （模拟器）cargo check 通过；桌面/Android 测试 11/11 无回归。
- **模拟器端到端**：`tauri ios build --target aarch64-sim --debug` 产出
  pi-mobile.app → simctl 安装 iPhone 16（iOS 18.5）→ 启动进程稳定存活，
  WKWebView 正常挂载。（CLI target 名与 rust triple 不同：`aarch64-sim`。）
- **真机签名**：project.yml 配 DEVELOPMENT_TEAM（自动签名，team ID 来自本机
  证书）；xcodeproj 需 `xcodegen` 重生成才生效（tauri CLI 复用已有 pbxproj，
  改 project.yml 后要手动重生成——踩坑记录）。

### 真机部署卡点（用户 GUI 一步）
- xcodebuild 报 "No Account for Team"：本机无任何 provisioning profile，自动
  生成需要 Xcode 账号会话。team 配置写进 project.yml（DEVELOPMENT_TEAM +
  CODE_SIGN_STYLE Automatic，target settings.base 层级——项目级不传导）。
- **FORCE_COLOR 陷阱（通用坑）**：tauri 生成的 Xcode 构建脚本含
  `${FORCE_COLOR}`；本环境导出 FORCE_COLOR=0，展开成位置参数 "0"，
  tauri-cli 把它当 arch 解析直接报 "Arch specified by Xcode was invalid"。
  修复：project.yml 脚本里删掉该参数 + xcodegen 重生成。
- **改 project.yml 后必须手动 `xcodegen` 重生成 pbxproj**——tauri CLI 复用
  已有 pbxproj，不会自动重生成（踩坑两次）。
- 用户路径：Xcode 打开 gen/apple/pi-mobile.xcodeproj → Signing 选 team →
  ▶ Run（注册设备 + 生成 profile）→ 之后 CLI 构建即可走通。

### 下一步
- [ ] 真机装机验证（等用户 Xcode 账号登录）
- [ ] **libpi-bun iOS 静态链路（M5 关键路径）**：源码构建 WebKit JSC +
  libpi_bun.a + pi_bun 模块静态链接（cfg ios 分支换实现）
- [ ] 桌面同构验证；v2 零拷贝桥

---

## 2026-09-06 14:04 — M4：Skills 管理（D12）✅

### Rust `skills.rs`（安装器 + registry + 宿主过滤）
- **存储**：`{data_dir}/skills/<id>/SKILL.md`（+ 资源文件）+ `registry.json`
  （id/name/description/source/version/checksum/enabled/installedAt）。
- **安装来源（v1）**：https 直链 SKILL.md（frontmatter 必须），或
  `github.com/{owner}/{repo}[/tree/{ref}]` → archive zipball 下载解包，
  取路径最浅的 SKILL.md 所在目录整体入包。**不引 git2-rs**——原生构建在
  Android NDK 有风险（@google/genai / @napi-rs keyring 同族教训），zipball
  覆盖 github 主流场景；其余 git host 暂不支持。供应链：sha256 checksum、
  version/ref 记录（更新 = 重装同 id）、下载 8MB / 单文件 256KB 上限、
  zip-slip 防护。依赖新增 zip/sha2/reqwest（reqwest 本就在依赖树内）。
- **命令**：skills_list / skills_install（网络，spawn_blocking）/
  skills_toggle / skills_remove / skills_reconnect（kick `__pi_skills_apply`
  热生效，复用已修复注册的 pi_call_global 通道）。
- **注入预算**：hostcall `skills_config` 只返回启用中的技能（宿主过滤），
  单技能 256KB、总预算 64KB——上下文成本按 D12 预留用量可视化联动。

### bundle + UI
- systemPrompt 组装追加 "# Skills" 节（`## name — description` + 正文，
  BASE_SYSTEM_PROMPT 同步剥离防叠加）；boot 即注入，`__pi_skills_apply`
  热生效（skills_applied 事件带注入数量）。
- 会话抽屉 Skills 管理节（镜像 MCP 节）：列表（启停状态/version/checksum）、
  Enable/Disable、Remove、URL 安装表单（Installing… 态）。

### 测试
- Rust 11/11（新增 4 个 skills 单测：raw md 安装与 registry 往返、zipball
  最浅 SKILL.md 胜出 + 资源随行、下载 URL 形态解析、注入跳过禁用/目录缺失）；
  aarch64-linux-android 交叉编译 check ✅（新依赖上机无忧）。
- 新增 `pi-bundle/skills-test.js`：enabled-only 注入、空配置移除节、
  热重载再注入、skills_applied 事件——全绿。全回归 7 项 ✅；tsc ✅。

### 下一步
- [ ] 真机验证：安装 github 技能包 → 注入 → 启停热生效 → 删除
- [ ] M4 剩余：Android 前台服务保流、通知；自动续跑（autoContinue 带上限）；
      MCP per-server 审批粒度
- [ ] Skills 后续（D12 完整版）：内置推荐目录、版本 pin 升级检查、
      作用域（全局/单 workspace）、用量可视化联动

## 2026-09-06 15:40 — 扩展能力层 IV：@juicesharp/rpiv-todo 移动原生化 ✅

### 上游语义（tool-schema.md 逐条对齐，模型视角不变）
- **`todo` 工具**：6 动作（create/update/list/get/delete/clear）、4 态状态机
  （pending → in_progress → completed，deleted 为墓碑；非法迁移拒绝、同状态
  no-op 报 "No change"）、blockedBy 依赖图校验（未知/墓碑/自阻塞/环，先校验
  后变更）、content 字符串与错误文案逐字对齐上游（`Created #3: … (pending)` /
  `Updated #3 (pending → in_progress)` / `⛓ #1,#2` 行格式等）。
- **持久化 = 上游同款"不落盘"哲学**：每个 toolResult 的 details 携带全量
  快照，状态从会话消息回放重建（restoreLatest / 切会话 / new session 三处
  接入 replayTodos；新会话清空任务槽）。纯 JS 工具，零 hostcall、零审批。
- **prompt 引导**：上游 8 条 promptGuidelines 原文注入 systemPrompt
  （"# Todo list" 节，BASE_SYSTEM_PROMPT 同步剥离，应用/重装不叠加）。

### 移动形态（上游 TUI overlay → WebView 常驻面板）
- `todo_updated` 事件（全量快照）驱动 UI：列表非空自动弹面板、清空自动收起；
  ✓ 完成（划线）/ ◐ 进行中（activeForm 斜体）/ ○ 待办；`/todos` 命令手动开关
  （命令面板第 4 项）。

### 测试
- 新增 `pi-bundle/todo-test.js`（mock loopback）：content 文案、状态机、
  依赖图三拒、墓碑、get 反向 blocks 边、clear 重置 id、合成消息回放 +
  回放后续号、prompt 注入、todo_updated 事件流，14 组断言全绿。
- 排坑：纯 JS 工具调用的测试全是微任务，不泵 I/O——boot 异步收尾
  （applySystemPrompt 依赖 refresh* 完成）与 fire-and-forget 的 emit 必须显式
  sleep 等待/flush，否则断言竞态（prompt 缺节、事件计数 0）。
- 全回归 approval/mcp/local/pi-commands/subagent/session ✅；tsc ✅；
  bundle 1.45MB ✅。lint 报错为基线既有（HEAD 同样报错），未新增。

### 下一步
- [ ] 真机验证：多步任务 todo 面板联动、切会话/重启后面板随快照恢复
- [ ] M4 剩余：Skills（D12）、前台服务保流、通知
- [ ] 自动续跑（pi-goal autoContinue 带上限）、MCP per-server 审批粒度

---

## 2026-09-06 13:07 — 审查必修三连：桥死锁 / MCP 审批绕过 / 命令未注册 ✅

### 审查确认（三条全部属实，#3 与"真机能用"的矛盾也已厘清）
1. **`__pi_open_session` 从 eval 返回挂 I/O 的 Promise → 桥死锁**：Rust
   `session_open` 直接 `evaluate_blocking("globalThis.__pi_open_session(id)")`，
   skal 对 Promise 走 waitForPromise 阻塞 VM 线程，而 repo.list/open/findEntries
   的 fs hostcall（fetch → loopback）恰需该线程 tick → 点会话列表即冻结
   （smoke2/ask_user 同族教训）。是全部被 eval 调用的全局里唯一一处违规
   （逐一核查：goal_apply/plan_start/btw_start/mcp_reconnect 均已 kick，其余同步）。
2. **MCP 工具绕过审批**：bundle 的 mcpTool 确实先发 `approval_request`（D11
   语义没丢），但 Rust `ASK_TOOLS = ["write","edit","bash"]` 匹配不到
   `mcp__<server>__<tool>` 前缀 → 直接返回 allow。D11"默认全部 ask"实际落空。
3. **`pi_call_global` 从未注册进 generate_handler**：1b5bc0f 引入时只加了命令
   定义，handler 列表里从来没有它（`git log -S` 全历史确认）。真机上 /plan
   /btw /goal 经该命令调用必然 reject——PROGRESS 里"真机验证三个命令"一直
   挂待办，所谓"能用"从未真正验证过。审查与代码相符。

### 修复
- **#1（bundle + Rust）**：`__pi_open_session` 改 kick+轮询（`__pi_persist_direct`
  同款）——同步返回 "started"，结果 JSON 落 `__pi_session_open_result`；
  Rust `session_open` 轮询该全局（每 200ms 一次 eval，泵 VM 事件循环驱动
  fs hostcall），30s 超时。session-test phase B 同步改 kick+轮询断言。
- **#2（Rust approval.rs）**：`mcp__` 前缀工具无条件 ask（不受 write 基线
  影响）；MCP 上的 "always" 放行本次但不降 write 基线（per-server 粒度留给
  D11 完整版）。单测并入 approval 全状态机测试：MCP ask → always 不写 auto →
  write 降 auto 后 MCP 依旧 ask。
- **#3（Rust lib.rs）**：`pi_call_global` 注册进 `generate_handler!`。
- **顺带**：ask_user 看门狗 setTimeout（暂存区改动，12 分钟兜底）加 `unref`——
  不再挂住本地测试进程事件循环（此前 approval-test 断言全过后进程挂 12 分钟）。

### 教训
- 测试并发踩坑：两个 approval 单测并发共用全局 PENDING 表，`keys().next()`
  互相偷请求 → 120s 超时假失败。审批断言合并进单测试函数（天然串行）。
- "真机验证过"要留证据（logcat/截图），PROGRESS 待办与口头记忆冲突时以待办为准。

### 验证
- cargo test 7/7 ✅；bundle 回归 approval / mcp / local / pi-commands /
  subagent / session 全绿且进程正常退出 ✅；tsc ✅；bundle 重建 1.44MB ✅。
- 真机待验证：会话列表切换不再冻结、MCP 调用弹审批卡、/plan /btw /goal 首次真机走通。

## 2026-09-06 12:20 — /btw 对齐 pi-btw 并行语义（agent 输出时可旁问）✅

- **行为对齐**：`/btw` 不再受 busy 拦截——主任务流式输出期间可直接发送。
  `/plan` 同样随时可起草。
- **后端改 kick+事件回投**：`__pi_plan_start` / `__pi_btw_start` 立即返回，
  嵌套 Agent 与主任务**并行**跑（共享 bun 事件循环，各自 fetch 在 eval 间隙
  泵动——与主任务流式同款已验证形态），完成后 emit `plan_drafted` /
  `btw_answer`（或 *_error）事件。此前的阻塞式 eval 会占住 runtime 锁，
  旁问期间 Stop 将失灵——kick 模式下锁不被占用。
- **UI**：busy 时 composer 显示 Stop（红）+ 💬（输入以 /btw 开头时）双按钮；
  答案经 `btw_answer` 事件渲染为 💬 卡片；计划经 `plan_drafted` 弹出计划卡。
- **测试**：pi-commands-test 改为 kick+事件断言（轮询 events）。

---

## 2026-09-06 11:50 — 扩展能力层 III：pi-plan / pi-goal / pi-btw ✅（六插件全就位）

### 命令基础设施（D7 命令面板的最小形态）
- 输入 `/` 前缀弹出命令面板（/plan /btw /goal，含描述，点击回填）；
  composer 发送时拦截命令进 `handleCommand`，不污染会话历史。
- Rust 新增通用 `pi_call_global(fn, arg)` 命令：eval bundle 内返回字符串的
  全局函数（skal waitForPromise 等待 Promise 落定，plan/btw 的嵌套 Agent
  运行期间事件仍经 loopback 流动）。

### 三个插件的移动原生化
- **/plan（@devkade/pi-plan 等价）**：`__pi_plan(objective)` 起草只读规划
  agent（read/ls/grep，禁止代码输出）→ 计划卡（markdown 渲染 +
  Discard / **▶ Approve & run**）。批准 = 计划文本作为普通 prompt 进入主
  对话执行（上游 approval-based execution 语义）。
- **/goal（pi-goal 等价）**：Rust `goal.rs` 持久化 `{data_dir}/goal.json`；
  boot 时 `goal_get` hostcall 注入 systemPrompt "Current goal" 节（与
  AGENTS.md 统一由 applySystemPrompt 组装）；UI 黄色 goal 横幅（目标 +
  ▶ Continue + ✕ 清除），set/clear 经 `__pi_goal_apply` 热生效。
  autoContinue（上游 Sisyphus 自动续跑）暂以手动 ▶ 替代，防失控。
- **/btw（pi-btw 等价）**：`__pi_btw(question)` 旁路子代理——带主对话
  近 12 条消息摘要作为上下文 + 只读工具，答案以 💬 assistant 卡片显示，
  **不写入会话/不进主任务上下文**。

### 测试
- `pi-commands-test.js`（假 LLM）：plan 只读运行产出计划、btw 带主上下文、
  goal 经 `__pi_goal_apply` 注入 systemPrompt（`__pi_system_prompt` 诊断缝）。
- 途中修复：mock 缺 apiKey 导致静默失败（agent.prompt 出错时 resolve 不
  reject——写测试时要注意查 agent_error 事件）。
- cargo 7/7；全回归 approval/session/mcp×2/subagent/local ✅；tsc ✅。

### 下一步
- [ ] 真机验证三个命令（/plan 批准执行、/goal 横幅、/btw 旁答）
- [ ] 自动续跑（pi-goal autoContinue，带上限）、/todos 面板
- [ ] M4 剩余：Skills（D12）、前台服务保流、通知

---

## 2026-09-06 11:10 — 扩展能力层 II：pi-subagents 移动原生化 ✅

### subagent 工具（pi-subagents 核心能力）
- **语义对齐上游**：工具名 `subagent`、主参数 `agent` + `task`（上游
  fleet/workflow/mission 机制基于 pi-server 运行时，移动端取单委托核心）；
  agent 定义与其同格式——markdown + frontmatter（name/description/tools/
  thinking/systemPromptMode）。
- **嵌套 Agent**：`new Agent({ 独立 systemPrompt + 受限工具集, streamFn:
  sharedStreamFn, getApiKey })` —— 子代理有干净上下文，跑完把最后一条
  assistant 文本作为工具结果返回。递归防护：子代理工具集剔除 `subagent`。
- **内置三代理**：delegate（继承父工具，append 模式）/ researcher（只读
  read/ls/grep，replace）/ reviewer（只读 + 审查纪律提示词）。自定义：
  放 `workspace/agents/*.md`（boot 时经 read 工具加载，frontmatter 解析）。
- **事件隔离**：子代理 delta 不上屏（保持主对话可读），仅
  subagent_start/subagent_end 状态行 + 错误进 logcat。
- **重构**：streamFn 抽为 `sharedStreamFn` 主/子共用；`__PI_CONFIG.baseUrl`
  可覆盖（本地测试假 LLM 端点）；`__pi_tool_call` 优先查 agent.state.tools
  （能探到运行期注册的 MCP/subagent 工具）；新增 `__pi_tool_names`。

### 测试
- 新增 `pi-bundle/subagent-test.js`：假 OpenAI 兼容 LLM 端点（SSE chunk）
  + mock hostcall 提供 workspace/agents/reviewer.md，全链路验证——未知
  agent 报可用列表、委托返回子代理最终回复、子代理请求带 reviewer 系统
  提示 + 受限工具集（无 subagent/write，有 read/grep）。
- 全回归：approval / session / mcp（JSON+SSE 双模式）/ local / tsc ✅。

### 下一步
- [ ] 真机验证 subagent（让 pi 委托 researcher 调研 workspace）
- [ ] 命令面板 UI（/plan /goal /btw 的移动形态）→ 接入剩余三插件
- [ ] MCP per-server 审批粒度（auto 降级，D11 完整版）

---

## 2026-09-06 10:30 — ask_user 闪退修复 + M4 开工：MCP 插件支持 ✅

### 真机闪退定位与修复（ask_user）
- **现象**：模型调用 ask_user 瞬间 SIGSEGV（空指针），线程 `HeapHelper`
  （嵌入 bun 内部 GC 线程）。
- **根因**：长挂起 fetch——hostcall 在 Rust 侧阻塞等用户作答（最长 600s），
  JS 侧 fetch 挂着 + `AbortSignal.timeout(30_000)` 定时器。此前所有 hostcall
  都是 13ms 级短往返，从未暴露。与"动态 import 是纯微任务"同级的嵌入式
  runtime 约束：**禁止长阻塞 fetch（含长时间 armed 的 AbortSignal）**。
- **修复**：改 kick+事件注入（仓库已验证模式）——
  `ask_user_register` hostcall 立即返回 id → JS 把 resolver 挂进 pendingAsks
  Map → 用户作答后 Rust 命令 `ask_user_respond`（spawn_blocking）经注入的
  resolver 反向 `skal_evaluate("globalThis.__pi_ask_resolve(id, answer)")`，
  该函数 resolve pending promise 后返回一个两拍微任务才 settle 的 promise，
  让 waitForPromise 把工具 continuation 泵完。长阻塞 fetch 与 AbortSignal
  定时器从 ask 路径彻底消失。
- 教训固化：PROGRESS 约束清单 +1。

### MCP 插件支持（pi-mcp-adapter 移动原生化，M4 主菜第一块）
- **决策**：手写最小 MCP streamable-http 客户端（~150 行，零新依赖），不用
  @modelcontextprotocol SDK——其 node 内建依赖与原生模块在嵌入 JSC 不可用
  （SIGSEGV 前科）。stdio 明确不支持（D11 修订）。长轮询类调用一律 SSE 流式
  （头部/数据持续流动 = 与 LLM 流同款已验证安全形态），tools/call 不挂
  AbortSignal。
- **bundle**：`mcpClient(name, url, headers)`（initialize / notifications/
  initialized / tools/list / tools/call，mcp-session-id 透传，SSE 响应解析）；
  boot 异步 `connectMcpServers()`：`mcp_config` hostcall 读服务器列表 →
  逐个连接（失败 emit mcp_error 继续）→ 工具注册为 `mcp__<server>__<tool>`
  → `agent.state.tools` 动态并入（D11：MCP 工具默认全部走 ask 审批）。
- **Rust `mcp.rs`**：`{data_dir}/mcp.json` 配置增删查 +
  `mcp_list/mcp_add/mcp_remove` 命令 + `mcp_config` hostcall；单测覆盖
  重名/非法 url/不存在删除。
- **UI**：会话抽屉底部 MCP servers 管理节（列表 + 删除 + 添加表单，
  标注"重启后生效、调用需审批"）。
- **测试**：`pi-bundle/mcp-test.js` 本地 mock MCP 服务器端到端（JSON-RPC
  initialize → tools/list → 注册 `mcp__mock__echo` → 带审批的 tools/call →
  文本结果断言）✅；cargo 6/6 ✅。
- 修了个自死锁测试 bug（ask_user 单测持锁跨 respond——sample 工具定位）。

### 下一步
- [ ] 真机验证：MCP 服务器添加 → 重启 → 工具注册 → 审批调用
- [ ] 真实 MCP 服务器实测（如 Context7 / 自建 echo server）
- [ ] MCP 工具审批粒度（per-server 降 auto，D11 完整版）
- [ ] pi-subagents 等价物；命令面板（/plan /goal /btw）

---

## 2026-09-06 02:20 — M3 扩展能力层 I：插件架构 + pi-ask-user 移动原生化 ✅

### 调研结论（决定架构）
用户点名的 6 个 npm:pi-* 插件（pi-ask-user / pi-mcp-adapter / pi-subagents /
@devkade/pi-plan / pi-btw / @capyup/pi-goal）全部是 **pi-coding-agent 扩展**，
交互层绑死 **pi-tui 终端 UI**（ask-user 的选择面板、goal 的编辑器悬浮层……），
部分还带原生依赖（pi-mcp-adapter 的 @napi-rs/keyring、cross-spawn stdio）。
**无法直接跑进嵌入式 JSC bundle**（无终端渲染，native 模块不可用）。

### 路线：扩展能力层（桥层适配，模型视角不变）
- bundle 内建扩展宿主：`coreTools` + `extensionTools` 合并进 Agent；
  每个扩展 = 一组 AgentTool（工具名/schema 对齐上游）+ 所需 hostcall。
- 交互层由宿主 UI（WebView 组件）承担——TUI 面板 → 移动卡片。
- 路线图：pi-ask-user ✅ → pi-mcp-adapter（仅 HTTP transport，需验证 MCP SDK
  在 JSC 的兼容性）→ pi-subagents（嵌套 Agent 委托）→ plan/goal/btw
  （命令类，等命令面板 UI）。

### pi-ask-user ✅（扩展能力层 #1）
- **Rust `ask_user.rs`**：pending 表 + 600s 超时 + 事件转发（复用 approval 的
  channel 模式）；`ask_user` hostcall 分发 + `ask_user_respond` 命令；
  单测覆盖应答/取消/无 UI 三路径。
- **bundle `ask_user` 工具**：schema 对齐上游
  （question/context/options{title,description}/allowMultiple/allowFreeform/
  allowComment），`executionMode: "sequential"`（提问未决时阻塞同回合其他
  工具——上游同款防乱序语义）；结果格式化
  （`✓ 选项…` / `(wrote) 自由文本` / Comment / 取消→按假设继续）。
- **UI 提问卡**：问题 + 上下文 + 选项按钮（单选/多选）+ 自由输入 + 备注 +
  Answer/Skip。
- **测试**：Rust 5/5；approval-test 增 ask_user 往返（mock 应答 → 工具结果
  含选项与备注）；tsc ✅。APK 已构建（设备断连，重连后装机）。

### 下一步
- [ ] 真机验证 ask_user 提问卡
- [ ] **pi-mcp-adapter 等价物**（仅 streamable-http；先 spike @modelcontextprotocol/
  client 在 JSC 的加载兼容性——吸取 @google/genai SIGSEGV 教训）
- [ ] pi-subagents 等价物（delegate 工具，嵌套 Agent 复用当前 streamFn）
- [ ] 命令面板 UI（/plan /todos /goal /btw 的移动形态）

---

## 2026-09-06 02:00 — M3 UX 专项 II：接入 solid-ui 组件体系 ✅

### 基建（补齐 solid-ui CLI 缺项）
- `@kobalte/core@0.13.13`（无头可访问组件基座）、`postcss.config.cjs`、
  vite `resolve.alias`：`~ → src`（tsconfig paths 已有）、`src/lib/utils.ts`
  的 `cn()`（clsx + tailwind-merge）。
- tailwind.config.cjs（CLI 生成）保持标准 solid-ui 形态：HSL 令牌映射、
  border-radius、accordion/content-show 动画、tailwindcss-animate。

### 新增组件（`src/components/ui/`，solid-ui 源码模式）
- `button`（cva 变体 default/destructive/outline/secondary/ghost/link + 尺寸）
- `sheet`（Kobalte Dialog 侧滑抽屉，left/right 滑入动画 + overlay + 关闭钮 +
  focus trap + ESC）——会话列表（右）/ 文件树（左）从手写 overlay 换成 Sheet
- `collapsible`（工具卡折叠改用 Kobalte 受控 open）
- `text-field`（API key 输入框）、`badge`（工具卡状态：pending 黄/失败红/成功绿）
- `card`、`separator`（备用）
- Kobalte 泛型 JSX 类型在包装组件处收敛（Overlay/Content 边界断言，调用侧
  类型精确）。

### 令牌合并
- App.css 顶部 @tailwind 三件套 + solid-ui HSL 令牌（**暗色为默认值**，
  本 App 单暗色主题），调色板与聊天配色一致；聊天/工具卡/审批卡/markdown
  自定义类与 tailwind 共存。

### 现状
- tsc ✅、vite build ✅（CSS 22KB / JS 105KB，Kobalte 进包）、bundle 测试回归 ✅。
- APK 已构建；设备断连未装机——重连后 `adb install -r -g` 装机验证 Sheet 手势
  （ESC/点外关闭、focus trap）、Collapsible 动画、Badge 状态。

---

## 2026-09-06 01:40 — M3 UX 专项：移动端 agent chat 体验重构 ✅

### 设计体系
- **App.css 全面重写**：设计令牌（--bg/--surface/--text/--accent/…）+ 类名
  体系，App.tsx 去掉全部内联样式；暗色移动优先，`100dvh` 布局，
  `env(safe-area-inset-*)` 适配手势条/刘海，内容区 max-width 760px（平板居中）。
- **index.html**：viewport-fit=cover + 主题色 + 标题改 pi-mobile。

### 聊天体验
- **Markdown 渲染**（新组件 `src/ui/Markdown.tsx`，零依赖）：围栏代码块
  （语言标签 + copy 按钮）、标题/列表/引用/粗斜体/行内代码/链接；
  escape-first 自有转换，无注入面。
- **assistant 气泡**：markdown + 悬停/常显 copy 按钮；**thinking 态**——
  message_update 只有 thinking 块时显示斜体 "thinking…"；调工具前清掉滞留
  思考泡（修掉此前截图里的 "(empty)" 空气泡：user/toolResult 的消息事件
  不再误渲染为 assistant 气泡）。
- **工具卡折叠**：toolCallId 合并 start/end，收起态一行
  （状态符 + 摘要 + 箭头），展开看参数/result；pending 黄/成功绿/失败红
  左边条，失败自动展开；回滚 chip 移入展开区。
- **自动滚动**：贴底跟随（新内容自动滚），用户上翻即暂停跟随。
- **流式状态 + 停止**：agent_start→busy，发送键变红色 ■ Stop（脉冲动画），
  经 `agent_stop` → bundle `agent.abort()`（AbortController 语义）；
  agent_error 显示错误状态行。
- **输入区**：单行 input → 自增高 textarea（Enter 发送 / Shift+Enter 换行，
  IME composing 防误发）；布局换 flex composer。
- **空状态**：π logo + 欢迎语 + 3 个建议 chips（点击直接发送）。

### 修复
- user/toolResult 的 message_start/update/end 事件此前会渲染成空气泡/重复
  气泡 → 只处理 assistant 角色。

### 待真机验证（设备已断开；重连后 `adb install -r -g` + 重启即装）
- markdown/代码块渲染、思考态、工具卡折叠、Stop 中断、chips、自动滚动、
  键盘下 composer 位置（安全区）。

---

## 2026-09-06 01:20 — M3 第三块：edit 工具 + 文件树/预览 + AGENTS.md ✅

### edit 工具（审批自动复用）
- Rust `run_tool("edit")`：`{path, oldText, newText, replaceAll?}` 精确替换；
  未找到 / 多处出现且未 replaceAll → 工具错误；覆盖写走 write_with_backup
  （备份自动生成，回滚链路复用）。`apply_edit` 抽为宿主函数，approval 与
  工具执行共用同一语义。
- approval `request()` 支持 edit diff：按 oldText/newText 在当前内容上模拟
  替换后生成 unified diff，审批卡直接展示改动。
- bundle：edit 工具注册（mutating → 审批点自动生效）；systemPrompt 更新
  （列出 edit、建议优先 edit 改文件）；approval-test 扩展 edit 场景
  （deny→错误、allow→替换、未命中→错误、只读工具零审批）。

### 文件树 + 只读预览（D7）
- `workspace_tree`（递归扁平列表，深度 ≤6 / 条目 ≤500）+ `workspace_read`
  （只读预览，256KB 上限，jail 复用）；UI 标题栏 📁 → 左侧文件树面板
  （目录蓝色加粗、缩进、⟳ 刷新）→ 点文件弹预览 overlay（等宽滚动，✕ 关闭）。

### AGENTS.md 注入（pi 语义对齐）
- boot/restore 后经 read 工具读 workspace/AGENTS.md，追加到 systemPrompt
  （`# Project instructions (AGENTS.md)` 节）；无文件静默保持基线。
  会话共享同一 workspace，无需按会话刷新。

### 验证
- cargo test 4/4（edit 唯一/未命中/replaceAll + 备份 + 树 + 预览 +
  越狱拒绝）；approval-test 全绿（含 edit）；session-test、local-test 回归
  通过；tsc ✅；桌面 + Android check ✅。真机待用户验证。

### 下一步（M3 剩余）
- [ ] 真机验证：edit 审批卡 diff、文件树/预览、AGENTS.md（放一个进 workspace）
- [ ] google provider 重接（bun plugin 构建期内联 node-builtin import）
- [ ] AGENTS.md / OAuth + deep-link（D9）
- [ ] i18n / 深色模式 / 无障碍基线；checkpoint/恢复兜底（D8）
- [ ] （已记录边界）agent 运行中切换会话、历史卡片无回滚 chip

---

## 2026-09-06 01:00 — M3 第二块：会话列表/切换 + 工具卡回滚 ✅

### 会话管理（D7 首行落地）
- **Rust `sessions.rs`**：扫描 `sessions/<cwd-encoded>/*.jsonl`，解析 v4 header
  （id/createdAt/cwd）+ mtime + message 条数 → `session_list` 命令（modifiedAt
  倒序）；单测覆盖排序与解析。
- **bundle 缝**：`__pi_open_session(id)`（repo.open + findEntries 回放进
  agent.state.messages 与 __pi_history）、`__pi_new_session()`（清指针，下一
  prompt 落新 JSONL）；Rust `session_open` / `session_new` 命令驱动。
- **UI 会话抽屉**：标题栏 ☰ → 右侧抽屉列出会话（id 前 8 位、时间、条数，
  当前会话高亮）→ 点击切换（`session_open` + `loadHistory` 重渲染）；「＋ New」
  开新会话。头栏显示当前会话 id 前缀。

### 回滚 UI（"diff 可回滚"闭环最后一环）
- `workspace_backup_info` 命令（该路径最新备份时间戳/null）；工具卡在
  tool_execution_end 时查询，覆盖写显示「↩ Revert」chip；点击 `workspace_revert`
  → 文件回退 + 消费备份 → 刷新同路径所有卡的 chip（还有更早备份可继续回退），
  卡片显示 "↩ reverted"。新文件写入无备份不显示 chip。
- 事件处理重构：工具卡按 `toolCallId` 合并 start/end（此前 end 另起新气泡）。

### 已知边界（记入 M3 剩余）
- 会话切换在 agent 运行中执行会把后续落盘写进新会话（MVP 可接受，后续
  busy 时禁用切换入口）。
- 历史会话里的 write 卡片暂无回滚 chip（历史渲染没有 toolCallId；后续把
  entry id 透出后补）。

### 下一步（M3 剩余）
- [ ] 真机验证：会话切换/新建 + Revert chip（等用户操作）
- [ ] 文件树 + 只读预览（D7）
- [ ] edit 工具 + 审批复用；Android bash 通道（M4 前置）
- [ ] google provider 重接（bun plugin 构建期内联 node-builtin import）
- [ ] AGENTS.md 随 workspace 生效；Provider OAuth + deep-link（D9）
- [ ] i18n / 深色模式 / 无障碍基线；checkpoint/恢复兜底（D8）

---

## 2026-09-06 00:50 — 真机验证收官：审批流 + 重启恢复全链路 ✅（4 个真机 bug 修复）

### 验证结果（Honor 真机，debug APK，`bun tauri android build --debug`）
- **审批流闭环**：写文件 → 审批卡（diff）→ **Allow** → 写入执行 → 文件落盘；
  点 **Always** 后 `policy.json` 持久化 `{"write":"auto"}`，后续写入免卡；
  删除 policy.json + 重启恢复 ask——策略状态机真机行为全部符合设计。
- **写前备份**：覆盖 `hello.txt` 自动生成 `backups/{millis}__hello.txt`。
- **重启恢复**：杀进程/重装后 `session_restored`，UI "history loaded — N messages
  from previous run"，会话随对话增长（7 → 27 条）持续落盘。
- **真实 LLM 对话**：DeepSeek 流式（含 thinking 块）、ls/write 工具往返，
  会话 JSONL 为 pi-v4 格式（header + message entries + usage/cost）。

### 真机修复的 4 个 bug（本地测试没抓到、只有真机 LLM 全链路才暴露）
1. **`AgentTool.execute` 签名错位（最重要）**：pi 的签名是
   `execute(toolCallId, params, signal, onUpdate)` —— hostTool 把第一参数当
   args 用了。ls 不依赖参数掩盖了错位，write 报 `Error: path?` 才暴露
   （Rust 收到的 args 是工具调用 ID 字符串）。`__pi_tool_call` 测试缝也按
   正确签名调用。教训：接口签名要对照 .d.ts，不能靠行为猜。
2. **toolResult 落盘失败**：agent 消息带显式 undefined 属性（usage 等），
   pi 的 assertJsonSerializable 拒绝 → "Durable payload contains undefined"。
   persistMessage 先 JSON 净化（undefined 属性丢弃）。新增
   `__pi_persist_direct` 测试缝 + session-test 覆盖该形状。
3. **saveKey provider 错配**：UI 把 key 存在 anthropic 名下而默认模型是
   deepseek —— 新装用户填 key 必失败（此前真机有旧数据所以没暴露）。
4. **恢复历史倒序 + 空气泡**：findEntries 新序列在前 → 按 seq 升序重排；
   纯 toolCall 的 assistant 消息渲染为 `⚒ name(args)` 摘要行。

### 其他
- `ensureSession` 单飞（并发首调只建一个会话——session-test 并发触发暴露）。
- adb 驱动真机输入的坑：`adb shell input text` 的引号被 host shell 剥掉后
  设备端按空格切词（只进第一个词）；空格需 `\ ` 转义，或由用户手动输入
  （已约定：消息发送由用户操作，自动化负责构建/安装/日志/文件验证）。

### 下一步（M3 剩余）
- [ ] 会话列表/索引（Rust session_list + UI）、文件树、回滚按钮进工具卡
- [ ] edit 工具 + 审批复用；Android bash 通道（M4 前置）
- [ ] google provider 重接（bun plugin 构建期内联 node-builtin import）
- [ ] AGENTS.md 随 workspace 生效；Provider OAuth + deep-link（D9）
- [ ] i18n / 深色模式 / 无障碍基线；checkpoint/恢复兜底（D8）
- [ ] （运维）bundle dist 变更后 Gradle 可能不重打包 → rm -rf
  src-tauri/gen/android/app/build（本次未遇到，留存备忘）

---

## 2026-09-05（深夜）— M3 开工：审批流闭环（write 审批 + diff + 可回滚）✅

### 审批流（M3 主线第一块，PLAN D2 policy-hook 落地）
- **拦截点**：mutating 工具（write）execute 前发 `approval_request` hostcall
  （阻塞等决策；loopback 每连接一线程，不阻塞其他通道）。pi 的 `beforeToolCall`
  钩子评估过但没采用——它只在 agent 循环内生效，工具执行路径内拦截对直连调用
  （诊断缝）同样有效。
- **Rust `approval.rs`**：policy 状态机（`{data_dir}/policy.json`，write 基线
  ask/auto）+ pending 请求表（channel 应答）+ unified diff（`similar` crate，
  context=2，16KB 截断）。ask 时 emit `approval_required`（requestId/tool/path/diff）；
  超时 120s 自动 deny；"always" 把 write 基线持久化为 auto；无 UI attach 兜底
  立即 deny 并清表（防 stale entry——单测抓出来的坑）。
- **bundle**：`hostTool` 增 mutating 标记，execute 先过审批，拒绝以工具错误文案
  返回给模型（并提示别重试同一写入）；write 工具描述告知需审批。新增
  `__pi_tool_call` 诊断/测试缝（与 agent 循环同一 execute 路径）。
- **UI**：底部审批卡（路径 + 红/绿 diff + Deny/Always/Allow），决策经
  `approval_respond` 命令回填。
- **可回滚**：write 工具覆盖已有文件前自动备份到 `{data_dir}/backups/
  {millis}__{rel}`；新命令 `workspace_revert` 恢复最近一次备份并消费之
  （连续调用逐级回退）。回滚 UI 入口随后续文件树/工具卡落地。
- **测试**：Rust 单测 ×2（approval 全状态机：ask→deny→always→auto→持久化、
  只读工具直放；备份/回滚 round-trip 含子目录）；本地 `approval-test.js`
  （deny 不落盘 / allow 写入 / 只读工具不触发审批）。cargo test 3/3 ✅。

### 下一步（M3 剩余）
- [ ] 真机验证：审批卡真机弹卡 → 决策 → 写入/拒绝（含重启恢复回归）
- [ ] edit 工具（apply_string_edit 类）+ 审批；Android bash 通道（M4 前置）
- [ ] 回滚 UI 入口（工具卡上"Revert"按钮）
- [ ] 会话列表/索引（Rust session_list + UI）、文件树、用量可视化
- [ ] AGENTS.md 随 workspace 生效验证；Provider OAuth + deep-link（D9）
- [ ] i18n / 深色模式 / 无障碍基线；checkpoint/恢复兜底（D8）

---

## 2026-09-05（晚）— M2 收尾：会话 JSONL 落盘 + 凭证 keyring 化 ✅

### 会话持久化（D3，pi 原生格式）
- **方案**：bundle 内用 pi 自己的 `JsonlSessionRepo` + `Session`（`@earendil-works/pi-agent-core`
  harness/session），`FileSystem` 能力经 **`fs` hostcall** 实现——磁盘 I/O 全部留在 Rust
  （jail 到 `{data_dir}/sessions`）。真机 eval 上下文拿不到真实 `node:fs`（`__require` shim
  是浏览器 stub），hostcall 边界本就是架构要求。
- **格式兼容**：文件为 pi-v4 JSONL（首行 header `{kind:"header",version:4,...}`，
  文件名 `{timestamp}_{uuid}.jsonl`，cwd 编码目录），与桌面 pi 会话同格式，D3 的
  「桌面开题、手机续跑」后续可直接吃这个目录。
- **落盘点**：用户消息在 `__pi_prompt`（先于 agent.prompt）、assistant 在 `message_end`、
  tool 结果在 `turn_end`（不走 toolResult 的 message_end，避免双写）。
- **重启恢复**：boot kick `restoreLatest()`（list → modifiedAt 最新 → open → findEntries
  → 回放进 `agent.state.messages`），完成后置 `__pi_restored`；Rust `agent_init` 等
  ready 后再等该标志（≤5s），UI 经新命令 `agent_history` 拉历史渲染。
- **本机验证**：新增 `pi-bundle/session-test.js` 两阶段往返（A：prompt 落盘；B：新进程
  同目录恢复 + v4 header 断言）——全绿。修复 `joinPath` 折叠斜杠 bug（repo 传
  `["/", root, dir]` 会叠出 `//pi-sessions`，stripRoot 失配 → EISDIR）。

### 凭证迁移（D4 部分）
- 新增 `src-tauri/src/creds.rs`：**桌面/iOS 用 keyring**（apple-native/windows-native/
  linux-native，service `pi-mobile`，account=provider），读取时自动迁移清理旧
  `creds.json`；**Android（cfg 隔离）暂为沙箱文件态 0600**。
- 关键事实：**keyring v3 没有 Android Keystore 后端**（支持 mac/win/linux/ios）。
  Android 迁移需 tauri 插件经 JNI 调 Keystore，排 M3；keyring 依赖按 target 门控，
  Android 构建不编译它。
- `creds_get` hostcall / `set_creds` 命令切到 creds 模块；`loopback::configure` 改收
  `data_dir`（内部派生 sessions 目录）。

### 契约与文档
- `docs/CONTRACTS.md` 更新到 M2 现状：commands（agent_init/prompt/status/history/
  set_creds）、`pi-agent-event` 单事件通道、hostcall 全集（ping/log/tool/creds_get/
  **fs**/agent_event）。
- `docs/PLAN.md` M2 标记 ✅（出口条件达成），遗留项移 M3。

### 验证
- `bash pi-bundle/build.sh`：1.41MB，import/import.meta 守卫通过。
- `bun pi-bundle/local-test.js`：boot 无回归。
- `bun pi-bundle/session-test.js`：两阶段往返 ✅。
- `cargo check`：桌面 + aarch64-linux-android 双目标 ✅（Android 需 NDK 环境变量，
  本机 NDK 为 darwin-x86_64 版；`CC_aarch64_linux_android=<ndk>/…/aarch64-linux-android24-clang`）。
- `bunx tsc --noEmit` ✅。

### 下一步（M3 开工项）
- [ ] 真机验证：重启恢复（杀进程重开 → 历史回显 + 续聊）
- [ ] Android Keystore 凭证加密（tauri 插件 + JNI）
- [ ] google provider 重接（bun plugin 构建期内联 @google/genai 的 node-builtin import）
- [ ] 审批流（policy-hook + DiffApproval UI + policy 状态机，M3 主菜）
- [ ] 会话列表 UI（Rust 侧 session_list 索引）

---

## 2026-09-05 22:00 — M2 出口条件达成 ✅（真机端到端对话 + 工具调用 round-trip）

### 验证结果
- **真机端到端对话跑通**：DeepSeek V4 Flash（OpenAI-completions API），
  流式 delta → turn_end → agent_end 完整事件链，多轮对话无 error。
- **工具调用 round-trip 验证通过**：模型调 `ls` 工具 → Rust loopback
  执行 `run_tool("ls")` → 结果回传模型 → 最终回复。事件流：
  `toolcall_start` → `toolcall_end` → `tool_execution_start` →
  `tool_execution_end` → `message_start(toolResult)` → `turn_end`。
- **agent bundle 真机 boot**：`runtime up` → `agent bundle kicked` →
  `agent_event: agent_ready`，进程稳定存活。

### 本轮修复（4 个关键 bug）
1. **top-level 静态 import 触发 SyntaxError**（skal_evaluate 以 classic script
   模式求值）：bun build `--target=bun` 保留 node-builtin 静态 import（来自
   `@google/genai`：`import { createWriteStream } from "fs"` 等 7 行）。build.sh
   新增后处理：用正则将 `import X from "m"` / `import * as ns` / `import { a }`
   改写为 `var X = __require("m")`，并加 grep 守卫确保无 `^import` / `^export` 残留。
2. **`@google/genai` 触发 skal JSC 原生段错误**（SIGSEGV fault `0xAAAAAAAAAAAAAAAA`，
   WKFastMalloc 区）：禁用 google-generative-ai provider（注释掉 import 和
   STREAM_SIMPLE 条目），bundle 从 2.47MB 降到 1.41MB，崩溃消除。
3. **Agent tools 未传入 streamFn context**（`context.tools` 始终为空数组）：
   pi-agent-core 的 Agent 从 `initialState.tools` 读取工具列表，而 agent-main.js
   错误地将 `tools` 作为顶层 AgentOptions 传入。修复：移入 `initialState: { tools }`。
   本地验证：`streamFn: tools count = 1`，`toolcall_end` 事件出现。
4. **DeepSeek reasoning 模式下工具调用走 DSML 文本格式**（`<｜｜DSML｜｜tool_calls>`）
   而非 OpenAI 标准 `tool_calls` 字段，pi-ai 不解析 DSML。设置
   `thinkingLevel: "minimal"`（映射到 null = 关闭推理），工具调用恢复结构化 API。
   补充 systemPrompt 告知模型有工具可用，避免模型拒绝调用。

### 其他改动
- loopback `dispatch("tool")` 加 logcat 诊断日志（hostcall tool: name/args/ok/err）。
- logcat tag 从 `pi-bun` 改为 `pibun`（Honor Android 16 间歇性加密含连字符的 tag）。
- 默认 model 配置为 deepseek-v4-flash（catalog 条目硬编码在 agent-main.js）。
- `bun tauri android dev` 热重载未触发 Rust 重编译（watcher 未检测 include_str! 依赖
  变更）；workaround：touch mod.rs 强制 cargo 重建。后续改用打包 APK 模式可避免。

### 下一步（M2 收尾 → M3）
- [ ] 提交 M2 收官 commit + 更新 PLAN.md 里程碑状态
- [ ] 会话 JSONL 落盘 `app_data/sessions/`
- [ ] 凭证迁 keystore（D4）
- [ ] 重新接入 google provider（需解决 @google/genai JSC 崩溃，可能需 bun plugin
  在 build 时内联 node-builtin 而非保留外部 import）

---

## 2026-09-05 — M2 主体攻坚中（嵌入式 pi agent 上机）

### 已完成
- **M1 收官**（提交 `090496d`）：预构建 libskal 装入 jniLibs，真机验证嵌入式 bun
  完整执行链（`Bun.version=1.3.14`、fetch、TextEncoder；热路径求值 0ms）。
  16KB 页对齐复核（p_align 0x4000/0x10000）。sha256 pin 记录于 LIBPI-BUN-NOTES §5。
- **M2 JS↔Rust 桥真机验证**（`08045a5`）：loopback HTTP（127.0.0.1 随机端口）
  + `bridge.js` hostcall，真机往返 13ms。
- **M2 agent bundle**（`d904ffd` + 后续）：`pi-bundle/agent-main.js` 嵌入
  `@earendil-works/pi-agent-core` Agent（static import），4 个 host 工具
  （read/write/ls/grep，Rust 侧路径越狱防护，D6 无 exec）、`creds_get`
  hostcall 取凭证、pi-ai `streamSimple` 按 model.api 分发、agent 事件经
  loopback → Rust `emit("pi-agent-event")` → WebView 聊天流。
  Rust 命令：`agent_init` / `agent_prompt` / `agent_status` / `set_creds`。
  Solid 聊天 UI（user/assistant 流式 delta/工具调用/状态气泡 + API key 输入）。
- **本机 bundle 验证通过**：`pi-bundle/build.sh` 产出 2.47MB 单文件，
  真 bun 下 `__pi_ready=true` 完整启动。

### 真机部署踩坑记录（全部已解决并固化到脚本/文档）
1. **`skal_evaluate` + 返回 Promise = 自死锁**：waitForPromise 阻塞 VM worker
   线程，而 await 的 I/O 恰需该线程 tick。结论：eval 脚本必须同步返回，
   异步一律 kick+轮询/事件模式（smoke2 首证，agent bundle 沿用）。
2. **CJS 格式不可用**：pi-ai subpath exports 无 `require` 条件，
   `bun build --format=cjs` 无法解析 `@earendil-works/pi-ai/api/*`。
   用 ESM + 后处理补丁。
3. **`import.meta` 在经典脚本中是 SyntaxError**（真机实测）：bun ESM 产物
   含 `var __require = import.meta.require` 互操作标记。`build.sh` 补丁：
   替换为 `__require` shim（优先 `node-stdlib-browser` 映射表 →
   process/buffer/crypto 特例 → Proxy 惰性 stub），并将残余
   `import.meta.url` 替换为固定 blob 路径；构建末尾 grep 守卫确保无残留。
4. **动态 import 是纯微任务**（真机实测）：skal 只在「求值脚本本身返回
   Promise」时泵微任务，同步轮询 eval 会饿死异步引导 → boot 卡死
   （`__pi_ready` 永远 false、无 boot_error）。结论：bundle 顶层只能用
   静态 import；async IIFE 的续体必须挂在真实 I/O 上。
5. **`agent_init` 改为轮询 `__pi_ready`**（20s 超时 + null-result 重试），
   CJS/ESM wrapper 完成值不可作就绪信号。
6. **node builtin 依赖**：google-auth-library 等 eager require
   child_process/util/events/fs/path/module → 由 `node-stdlib-browser`
   映射 + `util` 增强（promisify/inspect/inherits/…）+ 手写 EventEmitter
   + path 增补 parse/format + 兜底 Proxy stub 解决；`module.createRequire`
   返回哑实现。
7. **Gradle 缓存陷阱**：jniLibs 里的 .so 经软链更新后 Gradle 不重打包 →
   需 `rm -rf src-tauri/gen/android/app/build` 强制。devUrl 变更同理。
8. **vite 扫描器陷阱**：`optimizeDeps.entries` 必须限定应用入口，
   否则 vendor/bun 的上千 html/js 搞挂 dev 扫描。
9. **荣耀真机安装**：USB 安装需逐次屏幕确认（`INSTALL_FAILED_ABORTED`），
   `adb install -r -g` 可预授权；建议开「USB 安装」开关。
10. **AP 隔离导致 dev 断连（当前卡点）**：手机/Mac 同在 192.168.31.x 但
    ping 不通（路由器 AP 隔离）。tauri-cli 2.11.4 android 路径无条件把
    devUrl 替换为 LAN IP（`TAURI_DEV_HOST` 被忽略），无法走 adb reverse。
    **对策（进行中）**：改用 `bun tauri android build --debug` 打前端
    已打包 APK，绕开 dev server；顺带修复
    `tauri.conf.json beforeBuildCommand` 为 `bun run build`（原 `bun build`
    缺 entrypoint）与 package.json build script。

### 进行中
- `bun tauri android build --debug` 编译中（reqwest 阶段）。
- 完成后：`adb install -r -g`（手机端需点确认）→ 启动 → logcat 验证
  `agent bundle kicked` → 屏幕出现 agent ready → 填 Anthropic key →
  真机首条 pi 对话（M2 出口条件）。

### 下一步（M2 剩余）
- [ ] 真机端到端对话验证（含工具调用 round-trip）
- [ ] 会话 JSONL 落盘 `app_data/sessions/`
- [ ] 凭证迁 keystore（D4，M2 收尾或 M3 初）
- [ ] 提交 M2 收官 commit + 更新 PLAN.md 里程碑状态

### 网络环境备忘
- 当前 Wi-Fi 192.168.31.x 开了 AP 隔离，手机(20)↔Mac(219) 互不可达；
  dev 模式（`bun tauri android dev`）需关闭 AP 隔离或换手机热点网络；
  打包 APK 模式无此依赖（前端内嵌，LLM 流量走手机自身网络）。

---

## 2026-09-04 — M1 完成（历史）
- M0：脚手架、五插件接线、CI、Android 真机跑通模板（`2298e23`）。
- D1 定稿为方案 C（嵌入式 bun），skal 工艺笔记入库。
- M1 第一段：预构建 libskal 真机执行验证（`090496d`）。
- 详见 git log 与 docs/PLAN.md。
