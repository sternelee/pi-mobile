# pi-mobile 契约（CONTRACTS）

> 状态：**QuickJS 运行时**（2026-09-20，D18）。引擎由 `rquickjs` 静态编进 Rust 二进制，
> agent（`pi-agent-core`）跑在进程内的 QuickJS guest 里；宿主能力经 `globalThis.host.*`
> **进程内**调用，没有 HTTP 桥、没有 C ABI、没有外部 .so。
>
> 📦 本文档的 bun 路线形态（loopback HTTP hostcall + `pibun_*` C ABI + 脚本主体 token）
> 已随 `backup/bun` 分支归档，末尾 §6 只留要点备查。
>
> 变更纪律：IPC / 宿主通道的任何改动都要**双侧同步**（UI 侧 `src/lib/events.ts` 的类型
> 与 Rust emit 侧、JS `host.*` 调用点与 `qjs/guest.rs` 的挂载必须一起改）。

## 1. UI ↔ Rust IPC（Tauri commands / events）

### 1.1 Commands（40 个，`src-tauri/src/lib.rs` 的 `generate_handler!`）

| 命令 | 参数 | 返回 | 说明 |
|------|------|------|------|
| `agent_init` | `{}` | `{}` | boot QuickJS guest：`boot()` → `restore()`（恢复最近会话）→ 热身 tick。**boot 期间的事件会转发给 UI**（`agent_ready` 就是靠这条解锁 composer 的） |
| `agent_prompt` | `{ text }` | `"started"` | kick；回复经 `pi-agent-event` 流回 |
| `agent_status` | `{}` | `{phase,busy,pendingApprovals,messages,contextTokens,…}` | 轮询 |
| `agent_stop` | `{}` | `{}` | 中止当前运行（清队列，见 qjs `stop()`） |
| `agent_history` | `{}` | `{sessionId,messages[]}` | 当前会话历史（boot 已 `restore()` 过，所以重启后仍在） |
| `set_creds` | `{ provider, apiKey }` | `{}` | D4：桌面 keyring / Android 沙箱文件（`creds.rs`）。**按 UI provider id 存**（`google-gemini`），传输层查 key 时做 id 映射 |
| `has_creds` | `{ provider }` | `bool` | provider 选择流程：决定 UI 显示 key 输入还是直接拉模型列表 |
| `get_default_model` | `{}` | `{provider,modelId}` / `null`（JSON 串） | 默认模型选择回读（`{data_dir}/provider.json`） |
| `set_default_model` | `{ provider, modelId }` | `{}` | 默认模型选择持久化。**下一轮 boot 由 Rust 用目录解析成完整模型对象**（不再经 `__PI_CONFIG` 注入）；provider 为空串清除 |
| `approval_respond` | `{ requestId, decision }` | `{}` | 审批决策 ∈ allow/deny/always；回注到 guest 队列，唤醒等待中的工具调用 |
| `approval_policy_get` / `approval_policy_set` | `{}` / `{ write }` | `{write}` / `{}` | 审批基线（`{data_dir}/policy.json`，write: ask→auto 由 "always" 持久化） |
| `ask_user_respond` | `{ requestId, answer }` | `{}` | `answer = {response:{kind:"selection",selections[],comment?} \| {kind:"freeform",text,comment?}}` 或 `{response:null,cancelled:true}` |
| `workspace_tree` | `{}` | `{path,kind,size,mtimeMs}[]` | M3 文件树（深度 ≤6 / 条目 ≤500） |
| `workspace_read` | `{ path }` | `string` | M3 只读预览（≤256KB，jail 在 workspace 内） |
| `workspace_revert` | `{ path }` | `u64`（字节数） | M3 回滚：恢复该文件最近一次覆盖写入前的内容（消费备份） |
| `workspace_backup_info` | `{ path }` | `{millis}` / `null` | M3 回滚 UI：该路径是否还有可回滚备份 |
| `session_list` | `{}` | `SessionMeta[]`（modifiedAt 倒序） | `{id,createdAt,cwd,modifiedAt,entries,size}` |
| `session_open` | `{ id }` | `{}` | 切换会话：kick JS `openSession(id)` + 等 `session_opened`/`session_error`（**等待期间的事件照样转发给 UI**） |
| `session_new` | `{}` | `{}` | 新建空白会话（下一个 prompt 落新 JSONL） |
| `session_delete` | `{ id }` | `{}` | 删除会话文件 |
| `mcp_list` / `mcp_add` / `mcp_remove` / `mcp_reconnect` | `{}` / `{name,url}` / `{name}` / `{}` | `Server[]` / `{}` | M4：MCP 服务器配置（`{data_dir}/mcp.json`）；`mcp_reconnect` 是热重连（kick） |
| `goal_get` / `goal_set` / `goal_clear` | `{}` / `{objective}` / `{}` | `string` / `{}` | M4：持久目标（`{data_dir}/goal.json`）；boot 时注入 systemPrompt 的 "Current goal" 节 |
| `pi_call_global` | `{ fnName, arg }` | `string` | 命令类插件后端。见 §1.3 —— 名字与语义是**契约**，UI 一行不用改 |
| `skills_list` / `skills_install` / `skills_toggle` / `skills_remove` / `skills_reconnect` | … | `SkillMeta[]` / `SkillMeta` / `{}` | D12：技能包（https 直链 SKILL.md 或 github zipball）；`skills_reconnect` 热生效（kick `skillsApply`） |
| `native_capabilities` / `native_request_permission` | `{}` / `{capability}` | `Value` | M6：系统原生能力清单 + 权限请求（见 §2.4） |
| `preview_start` / `preview_targets` / `preview_open_external` | `{}` / `{}` / `{url}` | `u16` / `Value` / `{}` | D15：workspace 内 html/css/js 的本地预览（axum + WebView） |
| `script_capabilities` | `{}` | `Value` | D14：脚本可授予能力清单（UI 展示用）。⚠️ 脚本沙箱本身尚未实现（见 §2.5） |
| `greet` | `{ name }` | `string` | 脚手架残留 |

### 1.2 Events（Rust → UI，单一通道 `pi-agent-event`）

事件是 **pi-agent-core 的原始事件形状**（`message_start` / `message_update` / `message_end` /
`turn_start` / `turn_end` / `tool_execution_start` / `tool_execution_end` / `agent_start` /
`agent_end` …）**原样转发**，外加宿主与插件事件：

| 事件 | Payload | 说明 |
|------|---------|------|
| `agent_ready` | `{type}` | guest boot 完成 —— **UI 靠它把 composer 从 "agent booting…" 解锁** |
| `context_ready` | `{type}` | AGENTS.md / skills / MCP 都就绪（第一轮 prompt 的上下文完整） |
| `session_restored` | `{found,sessionId,messages}` | 会话恢复完成（`found:0` = 没有历史）。⚠️ **只发一条**：另一条无字段的会让 UI 的 `setCurrentSession(ev.sessionId ?? null)` 把高亮清掉 |
| `session_created` / `session_error` | `{sessionId}` / `{error}` | 落盘 |
| `approval_request` / `approval_resolved` | `{id,tool,tier,summary}` | 审批卡（diff 在 `approval_required` 里，上限 16KB） |
| `ask_user` | `{question,options}` | 提问卡（与 `pi-ask-user` 的 schema 对齐） |
| `todo_updated` | `{tasks,nextId}` | 全量快照（回放/切会话/新建也会发）；UI 据此渲染常驻面板 |
| `providers_listed` / `providers_error` | `{providers:[{id,name,models}]}` | provider + 模型目录（Rust 从 `assets/models.json` 生成） |
| `models_listed` / `models_error` | `{provider,models}` / `{provider,error}` | 某个 provider 的模型列表（`__pi_models_refresh`） |
| `model_applied` | `{provider,modelId,name}` | 模型热切换完成 |
| `mcp_ready` / `mcp_error` / `mcp_tools_registered` | `{server,tools}` / `{error}` | MCP 连接与工具注册（`mcp__<server>__<tool>`） |
| `skills_applied` / `agents_md_loaded` | `{count}` / `{bytes}` | 注入物变化（重装 systemPrompt） |
| `compaction_start` / `compaction_done` | `{tokens,messages}` / `{summarized,kept}` | 自动压缩 |
| `plan_drafted` / `plan_error`、`btw_thinking` / `btw_answer` / `btw_error` | … | `/plan` · `/btw` 的一次性嵌套 run |
| `subagent_start` / `subagent_end` | `{name,task}` / `{name}` | 子代理（其工具调用同样走审批） |
| `goal_auto_continue` / `goal_auto_done` / `goal_error` | … | pi-goal 自动续跑（上限 10 次 / 逐字 `GOAL_COMPLETE` 即停） |
| `oauth_open_url` / `oauth_progress` / `oauth_done` | … | ⚠️ qjs 路线未接 OAuth，目前只会返回明确错误（见 §1.3） |
| `boot_error` | `{error}` | boot 失败 / worker 中途死亡 |

### 1.3 `pi_call_global` 的全局名（契约面）

UI 调 8 个名字；qjs 侧分两类：**目录类**由 Rust 直接作答，**命令类**映射到 guest 的 `__spike.*`。

| 全局名 | 谁答 | 语义 |
|--------|------|------|
| `__pi_providers_list` | Rust | → `providers_listed`（8 家 UI provider + 模型；`google-gemini` 是目录 `google` 的别名） |
| `__pi_models_refresh(providerId)` | Rust | → `models_listed` / `models_error`。**不联网**（目录是静态数据） |
| `__pi_model_current()` | Rust | 当前生效模型的 JSON `{provider,id,name}`（provider 是 **UI id**） |
| `__pi_model_select({provider,modelId})` | Rust → JS `setModel` | 查目录 → 查传输 → 热切换 → `model_applied`。没有传输的家族**明确报错**（不静默换一家） |
| `__pi_commands()` | JS | 声明了 `command` 的技能 → `[{cmd,name,description}]` |
| `__pi_skills_apply` | JS `skillsApply` | 重读 registry + 重装 systemPrompt |
| `__pi_goal_apply` | JS `goalApply` | 重读 goal + 重装 systemPrompt |
| `__pi_plan_start(obj)` / `__pi_btw_start(q)` | JS `draftPlan` / `askByTheWay` | 一次性只读嵌套 run → `plan_drafted` / `btw_answer` |
| `__pi_oauth_login` | — | ⛔ 未接：返回明确错误（bun 路线的 OAuth 随之归档） |

> 未映射的名字一定返回 `qjs: 未实现的全局调用 <name>` —— 绝不静默成功（曾经的 bug 就是
> 缺映射时 UI 只看到一句含糊的失败）。

## 2. Rust ↔ agent 运行时（QuickJS）

### 2.1 线程模型

`Runtime` / `Context`（rquickjs）**不是 `Send`**，所以 guest 独占一个 worker 线程：

```
Tauri 命令线程 ──mpsc(Job)──► worker 线程：处理命令 → tick（送队列事件 + 泵微任务）→ 投 pi-agent-event
UI 决策（审批/提问）──► 共享队列（Arc<Mutex<Vec<Value>>>）──► worker 的 tick 经 host.poll() 取走
```

⚠️ **两条纪律**（各对应一次真机事故）：

1. **绝不在 VM 线程上等 I/O**：审批/提问的决策来自别的线程，只能塞队列，不能直接碰 `Context`。
2. **任何「边 tick 边等某个事件」的地方都必须转发事件**（`Guest::tick_and_emit`）：
   自己拿着看而不转发，等于把 UI 正在等的东西吃掉 —— `agent_ready` 被吃掉就是永远停在
   "agent booting…"，`approval_required` 被吃掉就是审批卡不弹、agent 干等。

### 2.2 `globalThis.host.*`（guest → Rust，进程内直调既有服务）

| 函数 | 参数 | 返回 | 说明 |
|------|------|------|------|
| `poll()` | — | `Value[]`（JSON 串） | 取走队列事件（模型增量、审批/提问决策）。**每拍调一次** |
| `log(line)` | `string` | — | 落 `{data_dir}/pi-agent.log` |
| `startModel(requestJson)` | `{model,context,options}` | `id` | 起一次模型请求（**异步**：结果经队列的 `model_progress` / `model_done` / `model_error` 回来）。每个 id 对应一个 promise 槽 |
| `ensureApproval(callId,name,argsJson)` | … | `requestId` | 审批握手第一步：按 `approval.rs` 的分档判定 —— `auto` 档当场放行；`ask` 档 emit `approval_required`（含 diff）并挂起 |
| `callTool(callId,name,argsJson)` | … | `{text,isError,details?}` | **工具执行的唯一入口**：该 `callId` 必须先完成握手（授权记在 callId 上），与档位无关 —— JS 忘了问、或被改写后故意不问，一律执行不了 |
| `fs(op, payloadJson)` | … | `{ok,value}` / `{ok,error}` | pi `JsonlSessionRepo` 的 FileSystem 后端（`pi_host_tools::fs_op`，与 bun 路线同一份），jail 到 `{dataDir}/sessions`；JS 侧虚拟根 `/pi-sessions` |
| `http(payloadJson)` | `{url,method?,headers?,body?,readMode?}` | `{status,headers,contentType,body,truncated}` | 所有出网的宿主侧通道（rustls + 编译进来的 webpki 根；30s 超时、256KB 上限、HTML→文本）。**SSRF 防护**：拒 loopback/私网/链路本地，除非该源在**宿主从 `mcp.json` 读出的授权列表**里（fetch 工具没有授权源，私网照旧拒） |
| `goalGet()` | — | `string` | 持久目标（`goal.json`，单一真源在 `goal.rs`） |
| `skillsConfig()` | — | `{skills:[{id,name,description,content,command?}]}` | 启用中的技能全集（禁用已由宿主过滤）；总量预算 64KB |
| `mcpConfig()` | — | `{servers:[{name,url}]}` | MCP 服务器配置（`mcp.json`） |
| `creds`（经 Rust 内部服务） | — | — | 凭证**不出宿主内存**：JS 不读 key，模型请求由 Rust 侧带 key 发出 |

### 2.3 guest 暴露的控制面（Rust → JS，`__spike.*`）

`boot(configJson)` / `prompt(text)` / `tick()` / `drain()` / `restore()` / `setModel(json)` /
`commands()` / `skillsApply()` / `goalApply()` / `draftPlan(obj)` / `askByTheWay(q)` /
`mcpReconnect()` / `openSession(id)` / `newSession()` / `listSessions()` / `setAutoContinue(b)` /
`status()` / `toolNames()` / `history()` / `sessionInfo()`。

事件流向是**单向**的：JS 把事件推进 outbox，`drain()` 交给 Rust，Rust emit 成 `pi-agent-event`。
反向只有 `host.poll()` 取队列（模型结果 + 决策）。

**异步 I/O 的结果不能靠返回值穿过桥**：`repo.list()` 之类是异步的，"started" 是立即返回的
kick 值，结果走事件。这条形状是 rquickjs 逼出来的（`call::<String>` 不能把 Promise 转成
String），与 bun 路线在真机上被迫采用的形状一致。

### 2.4 `native` 能力（M6）

实现纪律（承 `keepalive.rs` 两次真机事故）：**能用官方 Tauri 插件就用插件** —— 插件把
Android JNI / iOS ObjC 管线封在各自原生侧，Rust 只调 `run_mobile_plugin`。

| 工具 | args | 审批 | 实现 |
|------|------|------|------|
| `clipboard` | `{op:"read"\|"write"\|"clear",text?}` | read 自动；write/clear **ask** | `tauri-plugin-clipboard-manager` |
| `notify` | `{title,body?}` | **ask**（可 always） | `tauri-plugin-notification` |
| `location` | `{highAccuracy?}` | 自动 | `tauri-plugin-geolocation` |
| `weather` | `{latitude?,longitude?,days?}` | 自动 | 无系统 API → Open-Meteo（纯 Rust HTTP） |

**状态**：能力层（`native::status` / `native::request` + UI 的 `native_capabilities` /
`native_request_permission`）是活的；但**这 4 个工具还没接进 agent**（`native::tool` 目前无
调用者）—— bun 路线靠 hostcall 调它，那条链已归档。接壳时把 `native/mod.rs` 的
`allow(dead_code)` 去掉即可（见 §2.5）。

### 2.5 尚无线上的能力（"孤儿"清单）

删掉 bun 运行时后，以下实现**保留但暂无调用者**（原来只被 bun 的 hostcall 调），
各模块用 `#![allow(dead_code)]` + 注释显式标出，接壳时去掉即可：

| 能力 | 位置 | 缺什么 |
|------|------|--------|
| git 工具 6 个（status/diff/log/clone/pull/commit） | `git.rs` | JS 工具壳 |
| native 4 个工具 | `native/mod.rs` | 同上 |
| D14 脚本沙箱（授权边界） | `script.rs` | 隔离 runner（bun 的整 VM 隔离随路线归档；qjs 需要一个新方案） |
| OAuth 订阅登录 | `oauth.rs` | 未接（`__pi_oauth_login` 明确报错） |

## 3. 持久化（都在 `{data_dir}/` 下）

| 路径 | 内容 | 读写方 |
|------|------|--------|
| `provider.json` | 默认模型选择 `{provider,modelId}` | `get/set_default_model` → boot 时解析成模型对象 |
| `policy.json` | 审批基线（`write: ask\|auto`） | `approval.rs` |
| `mcp.json` | MCP 服务器列表 | `mcp.rs` |
| `goal.json` | 持久目标 | `goal.rs` |
| `sessions/*.jsonl` | pi-v4 会话（与 bun 路线同一格式，互通） | `sessions_fs` + pi 的 `JsonlSessionRepo` |
| `workspace/` | agent 的 jail 根 | `pi-host-tools` |
| `backups/` | 覆盖写入前的备份（供回滚） | `pi-host-tools` |
| `pi-agent.log` | 日志（真机排障唯一可靠通道） | `logcat.rs` |
| `creds`（keyring 或沙箱文件） | provider 凭证 / OAuth JSON | `creds.rs`（JS 不可见） |

> ⚠️ **`tauri-plugin-store` 目前没有任何读写方**（D13 的 store 键空间没落地，持久化实际都走了
> 上面的 JSON 文件）。要么落地要么把插件删掉 —— 见 docs/PROGRESS.md 第十五轮。

## 4. 变更纪律

- **IPC / 宿主通道**：改一侧必须同时改另一侧并同步本文档；`src/lib/events.ts` 是事件类型的
  单一真源（判别联合，字段与 emit 侧逐项核对过）。
- **安全边界只在 Rust**：审批分档、jail、SSRF、执行权（callId 握手）都在宿主侧；JS 侧的同名
  检查只算 UX。判据永远用「真正持有权限的那一侧」。
- **文档**：架构或运行时变化要更新 README 的架构图 + 本文档的通道表 + PROGRESS 的进展条目。

## 5. 历史（bun 路线，已归档到 `backup/bun`）

换引擎前的桥是 **loopback HTTP**：guest 是完整 bun VM，自带原生 `fetch`，所以宿主能力得开一个
`127.0.0.1:<随机端口>/hostcall` 端点，JS 用 fetch 打进来；Rust → JS 用 `skal_evaluate` 注入。
由此派生出两块只在那个形态下成立的东西：

- **`pibun_*` C ABI**（`pi_bun.h`：create_runtime / start / evaluate / free_string / run_script），
  以及 `build-libpi-bun.sh` 的符号守卫（防静默缺导出）。
- **D14 的主体与授权**：请求体带 `__scriptToken`（脚本主体，走 `script::authorize` 能力表）或
  `__hostToken`（agent 主体）。⚠️ 当时那条「**`/hostcall` 端点自身必须认证**」的结论仍然成立，
  只是对象换成了「谁能在进程内调到 `host.*`」：qjs 路线里 guest 是自己人、脚本 runner 尚未实现，
  所以当前不可利用 —— **将来实现脚本沙箱时，这条必须先回答**（`script.rs` 的策略层因此保留）。
