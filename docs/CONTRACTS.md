# pi-mobile 契约（CONTRACTS）

> 状态：骨架（M1/M2 产出后填充 schema 并双侧生成类型）。三份契约 + 一份 C ABI 镜像，任何变更须双侧测试同步通过。

## 1. UI ↔ Rust IPC（Tauri commands / events）

### 1.1 Commands

| 命令 | 参数 | 返回 | 说明 |
|------|------|------|------|
| `session_list` | `{}` | `SessionMeta[]` | Rust 侧索引（id/标题/模型/时间/消息数） |
| `session_load` | `{ id, offset? }` | `SessionPage` | 分页加载 JSONL |
| `approval_respond` | `{ requestId, decision }` | `{}` | decision: `allow` / `deny` / `always` |
| `agent_send` | `{ sessionId, text }` | `{}` | 经桥转发到 bun |
| `agent_abort` | `{ sessionId }` | `{}` | 事件回调 `abort` |
| `mcp_*` / `skills_*` | M4 | | 配置 CRUD（D11/D12） |

### 1.2 Events（Rust → UI）

| 事件 | Payload | 说明 |
|------|---------|------|
| `agent:delta` | `{ sessionId, seq, text }` | 桥层 16ms 合并后的 token 批 |
| `agent:tool` | `{ sessionId, tool, input, output?, phase }` | 工具调用卡（可折叠） |
| `agent:done` | `{ sessionId, usage, cost }` | 回合结束 + 用量 |
| `tool:approval-required` | `{ requestId, tool, diff? }` | 审批请求 → DiffApproval |

## 2. Rust ↔ bun 桥（C ABI：`src-tauri/pi_bun/include/pi_bun.h`）

### 2.1 生命周期
`pibun_create_runtime(bundle_path, home_dir, tmp_dir, host_port)` → `pibun_start` → 消息循环 → `pibun_stop` / `pibun_destroy`。VM 在专用 worker 线程（skal 模型）。

### 2.2 hostcall（bun → Rust，经 `pibun_host_port_t` 回调）

| method | 参数 | 应答 |
|--------|------|------|
| `creds_get` | `{ provider }` | `{ apiKey }` 或错误（凭证不出宿主内存） |
| `approval_request` | `{ tool, input, diff? }` | `{ decision: allow/deny/always }` |
| `open_url` | `{ url }` | `{}` |
| `notify` | `{ title, body }` | `{}` |
| `config_get` | `{ key }` | store 键空间取值（D13） |

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
