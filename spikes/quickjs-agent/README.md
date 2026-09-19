# quickjs-agent spike —— B 方案的可运行验证

> 「**薄 JS + 厚原生**」：pi-agent-core 的 Agent 类跑在 **QuickJS** guest 里，
> 模型传输（DeepSeek 一家）、工具、会话 fs、审批策略全在 **Rust**。
> 背景与取舍见 [docs/POCKET-PI-NOTES.md](../../docs/POCKET-PI-NOTES.md)；
> 本目录是那份笔记 §4「建议的下一步」里那个 spike。

**这是一次实验，不是产品代码。** 它不进 CI、不进 App 构建、不改变 D1。

## 它验证了什么

| # | 命题 | 结果 |
|---|---|---|
| 1 | 上游 `pi-agent-core` 的 `Agent` 类能在 QuickJS 里跑（非重写） | ✅ 流式思考/文本、工具调用、多轮 |
| 2 | 模型传输可以整个搬到 Rust，JS 里不要 HTTP/provider 栈 | ✅ 364KB bundle，0 个 provider SDK |
| 3 | 工具可以直接**复用现有 Rust 实现**，不重写 | ✅ 与 Tauri 宿主同一份 `pi-host-tools` |
| 4 | 体积/冷启动是否可接受 | ✅ bundle 小 8.1×，堆 1.8MB vs 87MB 的 .so |
| 5 | **审批**能在不阻塞 guest 的前提下完成往返 | ✅ 1000ms 等待里 guest 跑了 **334 拍** |
| 6 | **会话持久化**能复用 App 的同一份实现 | ✅ pi-v4 JSONL 逐字段一致，`--resume` 1.8ms 恢复 18 条 |
| 7 | **goal / todo 插件**能按 App 语义落地 | ✅ 目标注入 prompt；todo 四态 + 依赖校验 + 回放重建 |

**全部用真 DeepSeek 验证过**，不是只跑 mock。

## 结构

```
spikes/quickjs-agent/
├── js/
│   ├── prelude.js   QuickJS 缺什么补什么（实测清单在文件头）
│   ├── entry.js     agent 本体：Agent + streamFn + 工具壳 + 会话 + todo
│   └── build.sh     bun build --format=iife → dist/agent.js（+ 模块语法硬校验）
├── src/
│   ├── main.rs      CLI、循环、指标打印
│   ├── guest.rs     rquickjs 宿主：注入 host 面、tick + 泵微任务、度量
│   ├── approval.rs  审批：分档表 + 异步握手 + 终端决策源 + diff
│   └── deepseek.rs  模型传输：请求编码 + SSE 解析（对齐 pi-ai openai-completions）
├── tools/
│   └── mock-deepseek.py  无状态 SSE mock（无 key 也能端到端跑通）
└── crates/pi-host-tools（仓根）  抽出来的实现，Tauri 宿主共用同一份
```

数据流：

```
Rust 主循环                            QuickJS guest
  │ boot(config) ─────────────────────────▶ __spike.boot
  │ prompt(text) ────────────────────────▶ __spike.prompt → agent.prompt()
  │                                          └ streamFn → host.startModel(json) ─┐
  │ 模型线程：HTTP + SSE ─→ 事件队列 ◀─────────────────────────────────────────────┘
  │ tick() ─────────────────────────────▶ __spike.tick（poll 取事件 → 喂流）
  │ execute_pending_job() ×N ──────────────▶ 跑微任务（await 继续）
  │                                        └ 工具：host.ensureApproval → host.callTool
  │ 审批线程：stdin/diff ─→ 事件队列      （Rust 侧校验执行权，JS 绕过无效）
  │ ◀── drain() 取 agent 事件 ──────────────┘
```

## 跑起来

```bash
# 1. 打包 JS（bun build）
bash spikes/quickjs-agent/js/build.sh

# 2a. 不出网、不花钱的端到端（推荐先跑这个）
python3 spikes/quickjs-agent/tools/mock-deepseek.py 8899 &
DEEPSEEK_API_KEY=mock DEEPSEEK_BASE_URL=http://127.0.0.1:8899 \
  cargo run --release --manifest-path spikes/quickjs-agent/Cargo.toml -- --prompt "…"

# 2b. 真打 DeepSeek
export DEEPSEEK_API_KEY=sk-…
cargo run --release --manifest-path spikes/quickjs-agent/Cargo.toml -- \
  --goal "Keep the workspace tidy" \
  --prompt "Track as multi-step: (1) create src/pick.js, (2) note it in notes.md."
```

| 参数 | 作用 |
|---|---|
| `--goal <text>` | 写入 `goal.json`（宿主持有），注入 system prompt 的 `# Current goal` |
| `--resume` | 从最新会话恢复（消息灌回 agent + todo 状态重建） |
| `--yes` / `--deny` | 审批全放行 / 全拒绝（**仍走完整握手**，无人值守也能验证） |
| `--delay-approval <ms>` | 延迟放行，用来观测「等待期间 guest 没被阻塞」 |
| `--net-check` | 只验 dns / tls / engine 三层，不起 agent（真机自诊先用它） |
| `--data-dir <dir>` | 数据目录（真机上不能依赖仓库相对路径；run 脚本会显式传） |
| （默认） | 审批在终端交互：`y` / `n` / `a`(always) / `d`(deny-all)，write/edit 带 diff |
| `--model` / `--thinking` / `--workspace` / `--quiet` | 模型、思考档、工作区、静音 |

> mock 是**无状态**的：消息里没有 `role:"tool"` 就回一个 `read` 工具调用，有就回收尾
> 文本。所以同一句话可以反复跑，每次都会走完整的两轮 + 一次真实工具执行。

## 与 bun 版（`pi-bundle/agent-main.js`）的功能对齐

逐项对齐的结果。**三类**：已对齐 / 不可移植（并说明原因）/ 按设计不做。

| bun 版功能点 | 本 spike | 备注 |
|---|---|---|
| read/write/edit/ls/grep/mkdir/rm | ✅ 同一份实现 | `pi-host-tools`，Tauri 宿主共用 |
| **fetch**（http/https + SSRF 防护 + HTML→文本） | ✅ 抽出来复用 | `pi-host-tools::http`（原 `src-tauri/http_tool.rs`，纯函数零改动） |
| **todo**（4 态 + blockedBy + 回放重建） | ✅ 移植 | 语义逐项对齐，含 `todo_updated` 事件 |
| **subagent**（delegate/researcher/reviewer + `agents/*.md`） | ✅ 移植 | 子代理的工具调用**同样过宿主审批** |
| **ask_user**（多选 + 自由输入） | ✅ 移植 | 契约与 `ask_user.rs` 相同，决策源换成终端 |
| **AGENTS.md 注入** | ✅ 对齐 | 异步读 + 重装 systemPrompt + `context_ready` 门控 |
| **goal**（持久目标注入） | ⚠️ 部分 | 注入 + 持久化已对齐；**autoContinue 未做**（上游 Sisyphus 自动续跑） |
| **auto-compaction** | ✅ 对齐 | 同阈值策略（窗口 60%）+ 保留 8 条；另见下方「发现的 bun 版潜 bug」 |
| **会话持久化** | ✅ 同一份实现 | 同一个 `JsonlSessionRepo` + 同一份 Rust fs，pi-v4 格式互通 |
| **审批**（分档 + diff + always） | ✅ 且更强 | 分档表照抄；额外多了「Rust 强制握手」（见「审批」一节） |
| 控制面 `__pi_status` / `__pi_tool_names` / `__pi_history` | ✅ 对齐 | `__spike.status/toolNames/history` |
| 会话切换 `__pi_open_session` / `__pi_new_session` | ❌ 未做 | 只有 `--resume` 取最新；切换要加一层「选哪个」 |
| `/plan` `/btw` 嵌套 run | ❌ 未做 | 底座 `runNestedCollect` 已就位，缺的是命令面（那是 UI 驱动的东西） |
| `nativeTools`（剪贴板/通知/定位/日历/通讯录/照片/天气） | ❌ **不可移植** | M6 走 Tauri 插件（ClipboardExt/NotificationExt/GeolocationExt…）。CLI 在桌面上没有这些能力，要验得在 App 里 |
| `run_js`（D14 脚本沙箱） | ❌ 不可移植 | 要搬 `script.rs` 的隔离 runner + 能力授予 + per-run token（一套独立的安全核心） |
| `preview`（D15） | ❌ 不可移植 | 要搬 `preview.rs`（axum 静态服务 + 端口管理），且它的消费者是 WebView UI |
| git 工具（D16） | ❌ 不可移植 | 要 git2 + vendored libgit2/openssl（就是 D16 在 Android 上卡住的那套） |
| **MCP**（streamable-http） | ❌ 未做 | `host.http` 已经够（Rust 侧全都有），缺客户端实现；bun 版是 fetch-based，搬过来要改成过宿主 |
| skills 注入 | ❌ 未做 | 安装/校验在 `skills.rs`（git2 + zip + checksum）；只做「读 SKILL.md 注入」的话很轻 |
| provider 目录 / OAuth 订阅登录 | ❌ 按设计不做 | 本路线只做 DeepSeek 一家（8 家 + OAuth 的复刻成本见 `docs/POCKET-PI-NOTES.md`） |

**读法**：这张表本身就是 B 路线的成本清单 —— 左边一列里「同一份实现」的行是**已经沉没、
可以白拿**的部分；「不可移植」的行各自绑定一个 Tauri 插件或一个 cargo 依赖，换宿主就得重写；
「按设计不做」的行是这条路线的取舍。

### 对齐时发现的 bun 版一个潜 bug

`auto-compaction` 里拼 transcript 时写的是：

```js
const t = (m.content ?? []).filter((c) => c.type === "text").map((c) => c.text).join(" ");
```

**user 消息的 `content` 是字符串**（`{role:"user", content:"…"}`），字符串没有 `.filter`
→ `TypeError: not a function`。本 spike 在**真跑压缩**时一头撞上，改成先归一化
（[`textOfContent`](js/entry.js) ）才通。

bun 版同一段代码一样写，但它的阈值是「上下文窗口 100 万 token 的 60%」= 60 万 token，
**实际跑不到**，所以这个 bug 一直没暴露。本 spike 加了 `--compact-at <tokens>` 才能把这条
路径真的走一遍 —— 这也是为什么那个参数不是多余的。

## Android 真机

spike 是**普通 CLI**，不需要 APK / Tauri / WebView —— 可以直接 `adb push` 到
`/data/local/tmp` 跑。这样能在动整个 App 集成之前，先确认「QuickJS + pi-agent-core +
Rust 工具链」这一层在真机上不塌。

```bash
# 1. 交叉编译（产物在 spikes/quickjs-agent/target/aarch64-linux-android/release/）
bash spikes/quickjs-agent/tools/android-build.sh

# 2a. 真模型（key 从环境或 ~/.zshrc 读，走 env 文件传给设备，不进 argv）
bash spikes/quickjs-agent/tools/android-run.sh --prompt "Create src/x.js then stop."

# 2b. 或者不出网：宿主跑 mock（必须绑 0.0.0.0，设备经 LAN 连回来）
python3 spikes/quickjs-agent/tools/mock-deepseek.py 8899 0.0.0.0 &
MOCK=1 bash spikes/quickjs-agent/tools/android-run.sh --prompt "…"
```

`android-run.sh` 会**先跑一次自检**（`--net-check`，约 300ms），再执行正式那轮；
workspace/data 放在设备上的 `/data/local/tmp/pi-spike/`，所以第二轮加 `--resume`
就能在设备上验会话恢复。不带 `--yes/--deny` 时是本目录默认的**交互审批**：
`adb shell` 有 pty，可以直接在终端敲 `y`/`n`/`a`/`d`。

### `--net-check`：失败时先分层，别对着转圈的 agent 猜

真机上失败，第一件要回答的是**哪一层坏了**。所以有个只验分层、不起 agent 的模式：

```
$ ./quickjs-agent-spike --net-check
  dns     ok       13.5 ms  api.deepseek.com → 120.233.185.134, …
  tls     ok      125.6 ms  api.deepseek.com (HTTP 401) — 证书由编译进来的 webpki 根校验，未用系统信任库
  engine  ok      147.3 ms  QuickJS ok；bundle 356 KB eval 138 ms；堆 1.71 MB；__spike 6 个导出齐全
```

三层刻意分开：`dns` / `tls` 是网络，`engine` 完全不碰网络。所以 DNS 挂掉时
engine 那行照样 `ok` —— 一眼看出「引擎是好的，是网不通」。任一失败非零退出。

### 已验证的（静态）

| 项 | 结果 |
|---|---|
| 可执行形态 | `ELF 64-bit LSB **pie executable**, ARM aarch64, interpreter /system/bin/linker64` |
| 16KB 页对齐（Android 15+） | 4 个 `PT_LOAD` 全 `p_align = 0x4000`，用仓库的 `scripts/check-elf-align.py` 验过 |
| 动态依赖 | **只有 `libc.so` / `libdl.so` / `libm.so`** —— QuickJS、ring、rustls 全静态链进来 |
| 体积 | 9.49 MB（未 strip）/ **6.73 MB**（`llvm-strip` 后），对照 bun 路线的 87MB `.so` |

### TLS：这条路的根证书是编译进来的（对真机很关键）

`reqwest` 的 `rustls-tls` feature = `rustls-tls-webpki-roots`，也就是 **Mozilla 根证书库
静态编入二进制**（Cargo.lock 里 `webpki-roots 1.0.9`，且**没有** `rustls-native-certs`）。
实测二进制里 `system/etc/security/cacerts` 出现 **0 次**、`libssl/libcrypto` 符号 **0 个**
（`strings` 里那几处 openssl 字样是 ring 的 perlasm 汇编作者署名）。

这意味着 **D16 卡了 4 轮的那类问题在这条路上不存在**：不需要按 Android 的 hashed
目录格式拼 CA bundle、不需要 `set_ssl_cert_file/dir`、不受 `/apex/com.android.conscrypt`
布局变化影响。代价是根证书更新要跟着依赖走（换 `webpki-roots` 版本重编）。

### 交叉编译的三个坑（都封在 `android-build.sh` 里）

1. **任何 cargo 命令都要 NDK 的 CC/AR/RANLIB/LINKER** —— 同 `scripts/android-build.sh`
   的教训（Android 上连 `cargo check` 都会因为没有 CC 而失败）。
2. **rquickjs-sys 没有 android 的预生成绑定**（`src/bindings/` 里没有那一份），
   只能开 `bindgen` 现场生成 —— Cargo.toml 里按 target 打开。bindgen 要 libclang，
   而 **NDK 只带 `libClangdXPCLib`、不带 libclang** → 用 homebrew llvm 的
   （脚本自动探测 `LIBCLANG_PATH`）。
3. **bindgen 自己不传 `--target`**（读的是 rquickjs-sys 的 build.rs），不给就按宿主解析，
   报 `'stdio.h' file not found` —— 本机 PATH 里 NDK clang 排在 Apple clang 前面，
   正好把这个坑放大。要显式给 `--target=<triple><API> --sysroot=<NDK sysroot>`，
   且变量名是 **`BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android`**（下划线；bash 的
   `export` 也不接受带横线的名字）。

### 已在 arm64 Android 上**真跑过**（模拟器 API 32 / arm64-v8a）

不是只做静态检查 —— 下面这些是真在 Android 上执行出来的：

```
$ ./quickjs-agent-spike --net-check
  dns     ok       32.3 ms  api.deepseek.com → 120.232.219.129, …
  tls     ok      143.1 ms  api.deepseek.com (HTTP 401) — 证书由编译进来的 webpki 根校验
  engine  ok      264.4 ms  QuickJS ok；bundle 356 KB eval 250 ms；堆 1.71 MB
```

完整一轮（真 DeepSeek，8 次模型请求 / 6 次工具调用）：`src/hello.js` 被创建、
`notes.md` 被改写、**会话 JSONL 落在设备上**
（`/data/local/tmp/pi-spike/data/sessions/…jsonl`，13.9 KB）；第二轮 `--resume`
**恢复 20 条消息 5.1 ms**，且 agent 不调工具就答出上一轮创建的文件。

| 指标 | Android（模拟器） | macOS |
|---|---|---|
| guest 冷启动 | 84.6 ms | 26 – 60 ms |
| bundle eval（引擎自检里单列） | 250 ms | 138 ms |
| 会话恢复 20 条 | 5.1 ms | 1.8 ms（18 条） |
| QuickJS 堆 | 1.81 MB | 1.81 MB |
| DNS | 32.3 ms | 13.5 ms |
| TLS 握手 + HTTP | 143.1 ms | 125.6 ms |

结论：**Android 上每一层都成立**，包括最可疑的 DNS（`getaddrinfo` 走 Bionic，
不是 bun 在 iOS 上踩的 c-ares 那条路）与 TLS（根证书编译进来，不碰系统信任库）。

> 模拟器本身有个坑：本机 SDK 的 swiftshader 库签名坏了（`libGLESv2.dylib` /
> `libEGL.dylib` 报 code signature 错），`-gpu swiftshader_indirect` 起不来。
> 可用的组合是 **`-gpu host -feature -Vulkan`**：
> ```
> $ANDROID_HOME/emulator/emulator -avd Pixel_3a_API_32_arm64-v8a \
>     -no-window -no-audio -no-boot-anim -gpu host -feature -Vulkan -no-snapshot
> ```

### 真机（你的设备）上仍要看的一条

`/data/local/tmp` 可执行在 adb shell 下正常；但如果以后把这条路塞进 APK，则是另一套
问题：需要 INTERNET 权限、且不能从 data 分区 exec（得改成 JNI 加载 .so）。


## 实测数字

release / **真 DeepSeek**。Android 那列是在 arm64 模拟器上真跑出来的
（见「Android 真机」一节）：

| 指标 | macOS arm64 | Android arm64 |
|---|---|---|
| JS bundle（prelude + agent，含会话+todo） | **364,730 B**（App 的 2,950,176 B → **小 8.1×**） | 同一个二进制，同一份 bundle |
| guest 冷启动 | 26 – 60 ms | 84.6 ms |
| QuickJS 堆 | 1.81 MB | 1.81 MB |
| 会话恢复 | 1.8 ms（18 条） | 5.1 ms（20 条） |
| DNS | 13.5 ms | 32.3 ms |
| TLS 握手 + HTTP | 125.6 ms | 143.1 ms |
| 首增量（真网络） | 402 – 929 ms | 587 ms |
| 工具调用（本地 fs） | 0.0 – 1.5 ms | 0.1 – 0.9 ms |
| 审批等待期间的 tick 数 | **334 拍 / 1000 ms** | — |
| 整轮（4 次模型请求 + 3 次工具） | 4.4 s | — |
| 产物体积 | 7.92 MB（未 strip） | **9.49 MB / 6.73 MB strip 后**（App 是 87MB 的 .so） |

体积差从 9.3× 变成 8.1×，是因为这里**又多了会话与 todo 的能力**（316KB → 364KB）；
App 那 2.95MB 里仍有 8 家 provider + OAuth + 全部产品层。

## 三个能力点是怎么落的

### 审批：分档由 Rust 持有，JS 没有策略

分档表**照抄** `src-tauri/src/approval.rs`（`read/ls/grep` → auto、
`write/edit/mkdir/bash/git_commit` → ask、`rm/git_pull` → **always_ask 永不降级**）。

比 App 现有实现更进一步的一点：**工具执行的唯一入口 `host.callTool(callId, …)`
要求该 callId 先完成审批握手**，与档位无关。所以「JS 忘了问」或「JS 被改写后
故意不问」都执行不了 —— 与 D14「边界强制在 Rust 侧」同一思路。JS 侧因此不需要
任何策略代码，只是「问 → 等 → 执行」。

决策源可换：spike 是终端，App 里是 WebView 经 Tauri 命令。**协议一样**
（`approval_request` 事件出去、`approval_decision` 事件回来），换的只是谁回答。

非阻塞是真的：提示在独立线程读 stdin，guest 的 tick 循环照常跑。真机上这就是
「审批卡在等用户时 UI 不冻结」。

### 会话持久化：复用 App 的同一份实现

用上游 `JsonlSessionRepo` + `Session`，fs 后端全在 Rust
（`host.fs` → `pi-host-tools::sessions_fs`）。接法照搬 `pi-bundle/agent-main.js`
（设备验证过的那份）：净化 undefined → `appendMessage`；assistant 在 `message_end`
落盘、toolResult 在 `turn_end` 落盘；恢复时 `findEntries` 按 seq 升序回放。

产物与 App **逐字段一致**（pi-v4）：

```
.data/sessions/--Users-...-workspace--/2026-09-19T00-52-14-479Z_01a0b726-….jsonl
  {"kind":"header","version":4,"id":"01a0b726-…","createdAt":…,"cwd":"…"}
  {"kind":"entry","lane":"main","type":"message","id":"…","message":{…}}
```

即两条路线的会话文件可以互相打开。

### goal / todo 插件

- **goal**：目标由宿主持有（`goal.json`，将来换成 App 的 `goal.rs`），JS 只把它拼进
  systemPrompt 的 `# Current goal` —— 与 App 同一分工。
- **todo**：4 态状态机（`pending/in_progress/completed/deleted`）+ `blockedBy` 依赖
  校验（未知/墓碑/自阻塞/成环）+ 6 动作 + `todo_updated` 事件。状态**不写磁盘**，
  从会话消息的 `details` 快照回放重建 —— 上游同款哲学。

⚠️ todo 是**移植**不是共享：真正的产品形态应让两个 bundle import 同一份实现。
spike 阶段先把语义对齐（`TODO_TRANSITIONS` 与 `replayTodos` 逐行对照 App 那份）。

## 复用 Tauri 宿主的那两份实现

`crates/pi-host-tools` 现在是两个**不同的 jail 根**：

| 模块 | jail 根 | 内容 |
|---|---|---|
| `lib.rs` | workspace | agent 文件工具（read/write/edit/ls/mkdir/rm/grep）+ 写前备份/回滚 |
| `sessions_fs.rs` | sessions | pi `JsonlSessionRepo` 背后的 12 个 fs 方法（`/pi-sessions` 虚拟前缀） |

两份都是**脚本按行切片**从 `loopback.rs` 抽出（字符串未手抄），`loopback.rs` 保留
同名转发，签名与错误文案逐字不变：

- `src-tauri` 44 个测试全绿，含 `write_backup_and_revert_roundtrip`；
- crate 自带 4 个测试（越狱三类拒绝、会话 fs 往返、pi 错误形状、备份/回滚往返）；
- **零新增 lint 债务**：`cargo fmt --check` 14 处、`cargo clippy -D warnings` 27 个
  错误，与改动前的 HEAD 完全一致（stash 对照验证）。

## 踩过的坑（都是真跑出来的，不是推的）

1. **会话静默不落盘**：`repo.create` 一路 ENOENT。两个原因叠加，都靠「给 fs 通道加
   失败日志」才定位（第一版 JS 只看到 `FileError`，不知道是哪一步、哪个路径）：
   - 宿主漏了「建 sessions 根目录」这条职责（App 在 `lib.rs` 启动时建）；
   - 我照抄 `joinPath` 时**多拼了一次** `SESSIONS_ROOT`，叠出
     `/pi-sessions/pi-sessions/…`。App 那版返回 `'/' + joined`，parts 里已含根。
2. **deny 路径在指标里是隐形的**：被拒的调用提前 return，不记 span。已补
   `denied_calls` 记账，否则「拒绝生效了」这件事在报告里看不到。
3. **事件精简器丢了 `delta`**：JS 侧把 `message_update` 压成 `{kind}` 时漏了
   `delta`，表现是「模型答了但屏幕空白」。
4. **mock 自己在工具调用前发了 `[DONE]`**：宿主解析器读到 `[DONE]` 直接收工，
   工具调用整段丢失。mock 也要当被测代码写。
5. **二进制运行期还依赖 `dist/agent.js` 文件**：bundle 明明是 `include_str!` 编进去的，
   启动时却 `fs::metadata("spikes/quickjs-agent/dist/agent.js")` 只为打印体积 ——
   桌面看不出来，**Android 上直接 FAIL**（真机没有仓库相对路径）。改成从编译期常量取。
   是「上设备」这一步把它逼出来的。
6. **改了 bundle 必须重建 Rust 二进制**：bundle 是 `include_str!` 编进去的，只跑
   `js/build.sh` 不 `cargo build` 的话，跑的还是上一版 JS —— 我就这么"debug"了一轮：
   代码已经修好，看到却还是旧错误。`android-build.sh` 里也同理（它是先 build.sh 再 cargo）。
7. **「等审批时没阻塞」这条指标要设计观测窗口**：管道输入是瞬时回答，等待窗口只有
   微秒级，`ticks while waiting` 恒为 0，看不出任何东西。加 `--delay-approval`
   把窗口撑开才量得到（且它只在真有等待时打印——那一轮 agent 只调了只读工具，
   所以没有这行，不是 bug）。

## 已知问题 / 未验证

- **Android 已交叉编译通过、未上真机**（见「Android 真机」一节：静态项全过，
  真机跑法有脚本）。**iOS 未做**：rquickjs 无 JIT 本身合规，但没试过交叉编译。
- 桌面构建**不要开 `bindgen` feature**（Cargo.toml 里只在 android target 打开它）：
  本机 PATH 里 NDK 的 clang 排在 Apple clang 前面，bindgen 会拿 NDK 的 include
  路径去找 `stdio.h` 而失败（实测）。
- **`transformMessages` 没实现**：pi-ai 发请求前会做 provider 归一化（孤儿
  toolCall 修补、连续 toolResult 合并等）。本 spike 只做了三种角色 + 空 assistant
  丢弃，长时间多轮后可能撞到边界。
- **流式 toolCall 参数没有中途解析**：pi-ai 用 `partial-json` 让 UI 提前看到正在
  生成的参数；这里只在 `finish_reason` 之后整体解析。
- **无重试、无取消、无自动压缩**：`agent.abort()` 没接（prelude 里的
  AbortController 是空壳），上下文超窗不压缩。
- **goal 的 autoContinue 没做**：App 里 pi-goal 还有「目标未达成自动续跑（带上限）」，
  这里只有目标注入与持久化。
- **todo 是与 App 平行的移植**（见上），后续若两条路线并存应收敛成一份。
- `cargo fmt`/`clippy` 的存量问题与本 spike 无关，但 **CI 已连续多个提交全红**，
  且 `rust` job 卡在 `cargo fmt` 这第一步 —— 后面的 clippy 与 test 从未在 CI 上
  执行过。详见 `docs/PROGRESS.md` 2026-09-19 条目的「顺带发现 ①」。

## 结论

七条命题都成立。加上这一轮的能力（审批 / 会话 / goal / todo）之后，结论比第一轮更清楚：

**B 路线的 JS 侧确实可以很薄**（364KB、无 provider SDK、无 fs、无 HTTP、无策略），
但**每一层能力都需要在 Rust 侧重新长出来**。这一轮做的每一件事都在印证同一句话：

> 切 B 的真实增量是「用 Rust 重写 pi-ai 传输层 + 重建产品层」，不是「换个 JS 引擎」。

同时这一轮补上了第一轮缺的那半个答案，也就是一张「能复用 / 要重写」的清单：

| 层 | 结论 |
|---|---|
| 工具实现 | **能复用**（同一份 `pi-host-tools`，零改动） |
| 会话层 | **能复用**（同一份 `JsonlSessionRepo` + 同一份 Rust fs，格式互通） |
| 审批层 | **要重写**（协议可照搬，但策略持有者与决策源都要按新形态重建） |
| 插件层 | **要重写**（语义可照搬，实现要收敛成两份 bundle 共享的一份） |
| provider 传输 | **要重写**（pi-ai 的 8 家 + OAuth + caching 得用 Rust 复刻） |

如果哪天要评估是否切 B，这张表比体积数字更有用。

是否切换以及触发条件，见 `docs/POCKET-PI-NOTES.md` §4。
