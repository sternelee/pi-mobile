# 开发进度日志

> 持续更新。倒序记录，每条含日期、状态与下一步。

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
