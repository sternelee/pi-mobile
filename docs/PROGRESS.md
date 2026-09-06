# 开发进度日志

> 持续更新。倒序记录，每条含日期、状态与下一步。

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
