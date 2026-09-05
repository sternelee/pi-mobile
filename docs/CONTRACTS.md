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
| `agent_history` | `{}` | `{sessionId,messages[]}` | boot 时从最新 JSONL 会话回放的历史 |
| `set_creds` | `{ provider, apiKey }` | `{}` | D4：桌面 keyring / Android 沙箱文件（creds.rs） |
| `approval_respond` | `{ requestId, decision }` | `{}` | M3 审批：decision ∈ allow/deny/always；唤醒阻塞中的 approval_request |
| `workspace_revert` | `{ path }` | `u64`（字节数） | M3 回滚：恢复该文件最近一次覆盖写入前的内容（消费备份） |

### 1.2 Events（Rust → UI）

| 事件 | Payload | 说明 |
|------|---------|------|
| `pi-agent-event` | agent 事件 JSON（透传） | 见 §2.4 事件类型；含 `agent_ready` / `session_restored` / `session_created` / `session_error` / `agent_error` / `boot_error` / `approval_required` |

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
