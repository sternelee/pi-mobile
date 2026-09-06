# pi-mobile 契约（CONTRACTS）

> 状态：M2 现状（2026-09-05）。预构建 skal ABI 阶段，桥为 loopback HTTP；
> 自有 `pi_entry.zig` 的 `pibun_*` C ABI 为后续形态（协议语义不变，仅换传输层）。
> 任何变更须双侧测试同步通过。

## 1. UI ↔ Rust IPC（Tauri commands / events）

### 1.1 Commands（已实现）

| 命令 | 参数 | 返回 | 说明 |
|------|------|------|------|
| `pi_bun_smoke` | `{}` | `{hello, smoke2}` | M1/M2 桥冒烟 |
| `agent_init` | `{}` | `{}` | 加载 bundle；等 `__pi_ready` + `__pi_restored`（会话回放完成） |
| `agent_prompt` | `{ text }` | `"started"` | kick；回复经 `pi-agent-event` 流回 |
| `agent_status` | `{}` | `{busy,lastError,queued}` | 轮询 |
| `agent_stop` | `{}` | `{}` | M3：中止当前运行（bundle `agent.abort()`） |
| `agent_history` | `{}` | `{sessionId,messages[]}` | boot 时从最新 JSONL 会话回放的历史 |
| `set_creds` | `{ provider, apiKey }` | `{}` | D4：桌面 keyring / Android 沙箱文件（creds.rs） |
| `approval_respond` | `{ requestId, decision }` | `{}` | M3 审批：decision ∈ allow/deny/always；唤醒阻塞中的 approval_request |
| `ask_user_respond` | `{ requestId, answer }` | `{}` | 扩展 ask_user：answer = `{response:{kind:"selection",selections[],comment?} \| {kind:"freeform",text,comment?}}` 或 `{response:null,cancelled:true}` |
| `workspace_revert` | `{ path }` | `u64`（字节数） | M3 回滚：恢复该文件最近一次覆盖写入前的内容（消费备份） |
| `workspace_backup_info` | `{ path }` | `{millis}` / `null` | M3 回滚 UI：该路径是否还有可回滚备份 |
| `session_list` | `{}` | `SessionMeta[]`（modifiedAt 倒序） | M3 会话列表：`{id,createdAt,cwd,modifiedAt,entries,size}` |
| `session_open` | `{ id }` | `{}` | M3 切换会话：kick `__pi_open_session`（同步返回 "started"）+ 轮询 `__pi_session_open_result`（禁止 eval 挂 I/O 的 Promise——waitForPromise 阻塞 VM 线程会桥死锁） |
| `session_new` | `{}` | `{}` | M3 新建空白会话（下一个 prompt 落新 JSONL） |
| `mcp_list` / `mcp_add` / `mcp_remove` | `{}` / `{ name, url }` / `{ name }` | `Server[]` / `{}` / `{}` | M4：MCP 服务器配置管理（重启后生效） |
| `goal_set` / `goal_clear` | `{ objective }` / `{}` | `{}` | M4：持久目标设置/清除（存 goal.json） |
| `pi_call_global` | `{ fnName, arg }` | `string` | 命令类插件后端：调用 bundle 全局（`__pi_plan_start` / `__pi_btw_start` / `__pi_goal_apply`，均为 kick 语义同步返回） |
| `workspace_tree` | `{}` | `{path,kind,size,mtimeMs}[]` | M3 文件树（深度 ≤6 / 条目 ≤500） |
| `workspace_read` | `{ path }` | `string` | M3 只读预览（上限 256KB，jail 在 workspace 内） |

### 1.2 Events（Rust → UI）

| 事件 | Payload | 说明 |
|------|---------|------|
| `pi-agent-event` | agent 事件 JSON（透传） | 见 §2.4 事件类型；含 `agent_ready` / `session_restored` / `session_created` / `session_error` / `agent_error` / `boot_error` / `approval_required` / `todo_updated`（rpiv-todo 移动原生化：`{tasks, nextId}` 全量快照，成功变更即发；回放/切会话/新建同步发出；UI 据此渲染常驻面板，`/todos` 命令手动开关） |

（原规划的 `agent:delta` 16ms 合并等随 M3 后续落地。）

## 2. Rust ↔ bun 桥（C ABI：`src-tauri/pi_bun/include/pi_bun.h`）

### 2.1 生命周期
`pibun_create_runtime(bundle_path, home_dir, tmp_dir, host_port)` → `pibun_start` → 消息循环 → `pibun_stop` / `pibun_destroy`。VM 在专用 worker 线程（skal 模型）。

### 2.2 hostcall（bun → Rust，当前形态：`fetch http://127.0.0.1:<port>/hostcall`，`{ method, payload }`）

| method | 参数 | 应答 | 说明 |
|--------|------|------|------|
| `ping` | 任意 | `{pong, echo, ts}` | 连通性 |
| `log` | `{ msg }` | `{ok}` | logcat（tag `pibun`） |
| `tool` | `{ name, args }` | `{ text }` / `{ error }` | read/write/ls/grep，jail 到 `{dataDir}/workspace`，D6 无 exec |
| `creds_get` | `{ provider }` | `{ apiKey }` / `{ error }` | 凭证不出宿主内存，JS 仅注入运行时内存 |
| `fs` | `{ op, path, … }` | `{ ok, value }` / `{ ok, error: { code, message } }` | pi `JsonlSessionRepo` 的 FileSystem 后端；jail 到 `{dataDir}/sessions`；JS 侧虚拟根 `/pi-sessions`（agent-main.js 与 loopback.rs 同款常量）；op ∈ readTextFile/readTextLines/writeFile/appendFile/renameFile/fileInfo/listDir/exists/createDir/remove |
| `agent_event` | agent 事件 JSON | `{ok}` | Rust sink → `emit("pi-agent-event")` |
| `approval_request` | `{ tool, args }` | `{ decision: allow/deny, reason? }`（阻塞至 UI 决策/超时 120s） | M3：mutating 工具（write/edit/bash）执行前调用；Rust policy 状态机（`{data_dir}/policy.json`，write: ask→auto 经 "always" 持久化）；ask 时 emit `approval_required`（含 unified diff，上限 16KB） |
| `ask_user` | `{ question, context?, options?[{title,description?}], allowMultiple?, allowFreeform?, allowComment? }` | `{ response: {kind:"selection",selections} \| {kind:"freeform",text} \| null, reason?, cancelled? }`（阻塞至用户作答/跳过/超时 600s） | 扩展能力层 #1（pi-ask-user 移动原生化）：emit `ask_user` 事件 → 提问卡；schema 与 npm:pi-ask-user 对齐 |
| `ask_user_register` | 同 `ask_user` | `{ id, state: "pending"/"cancelled" }`（立即返回） | ask_user 的 kick+事件注入形态（禁长挂起 fetch）；作答经 `ask_user_respond` → `__pi_ask_resolve` 注入 |
| `mcp_config` | `{}` | `{ servers: [{name, url}] }` | M4：MCP 服务器配置（存 `{data_dir}/mcp.json`）；bundle boot 时逐个 streamable-http 连接，工具注册为 `mcp__<server>__<tool>`（默认 ask 审批） |
| `goal_get` | `{}` | `{ objective: string \| null }` | 扩展能力层 #3（pi-goal 移动原生化）：boot 注入 systemPrompt "Current goal" 节；持久化 `{data_dir}/goal.json` |

### 2.3 事件（Rust → bun，`pibun_post_event`）

| type | Payload | 说明 |
|------|---------|------|
| `user_message` | `{ sessionId, text }` | 新用户输入 |
| `approval_result` | `{ requestId, decision }` | 审批结果回传 |
| `abort` | `{ sessionId }` | AbortController 语义 |
| `command` | `{ name, args }` | 模型切换/上下文压缩/新会话 |

### 2.4 bun → Rust 事件（worker 线程异步上报，与 hostcall 分离）

| type | Payload | 说明 |
|------|---------|------|
| `agent:delta` / `agent:tool` / `agent:done` | 同 §1.2 | Rust 转手 emit 给 UI |
| `runtime:log` | `{ level, msg }` | logcat/统一日志 |

## 3. store 键空间（tauri-plugin-store，D13）

| key | 类型 | 默认 | 说明 |
|-----|------|------|------|
| `settings.version` | `int` | `1` | 迁移版本字段 |
| `settings.theme` | `"system"\|"light"\|"dark"` | `"system"` | |
| `settings.language` | `string` | `"zh-CN"` | |
| `settings.defaultProvider` / `settings.defaultModel` | `string` | — | |
| `policy.default.write` | `"ask"` | `"ask"` | 审批基线（read=auto 固定） |
| `policy.default.bash` | `"deny"` | `"deny"` | Android 起步为 ask（M4） |
| `onboarding.completed` | `bool` | `false` | |

## 4. 变更纪律

- C ABI：头文件为唯一真源；`build-libpi-bun.sh` 的符号守卫从头文件解析期望集（skal 实践，防静默缺导出）。
- IPC/桥：schema 漂移由 `tests/contract` 双侧 fixture 卡死。
- store：新增 key 必须先登记再使用；结构变更走 `settings.version` 迁移表。
