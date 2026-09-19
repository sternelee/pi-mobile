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
| （默认） | 审批在终端交互：`y` / `n` / `a`(always) / `d`(deny-all)，write/edit 带 diff |
| `--model` / `--thinking` / `--workspace` / `--quiet` | 模型、思考档、工作区、静音 |

> mock 是**无状态**的：消息里没有 `role:"tool"` 就回一个 `read` 工具调用，有就回收尾
> 文本。所以同一句话可以反复跑，每次都会走完整的两轮 + 一次真实工具执行。

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

`android-run.sh` 会把 workspace/data 放到设备上的 `/data/local/tmp/pi-spike/`，
所以第二轮加 `--resume` 就能在设备上验会话恢复。不带 `--yes/--deny` 时是本目录默认的
**交互审批**：`adb shell` 有 pty，可以直接在终端敲 `y`/`n`/`a`/`d`。

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

### 真机上还没验的

静态检查过了，但**「能编译」不等于「能跑」**。上设备后重点看这几条（按可疑程度排）：

1. **DNS/出网**：rustls 走 `std::net` 的 `getaddrinfo` → Android Bionic 解析器读的是
   系统属性，理论上没问题；但**这正是 pi-mobile 在 iOS 上被 bun 的 c-ares 坑到的地方**
   （c-ares 读不到 `/etc/resolv.conf`）。这条是本 spike 在真机上最值得看的点。
2. **`/data/local/tmp` 可执行**：adb shell 里正常，但如果以后塞进 APK，则需要
   INTERNET 权限 + 不能从 data 分区 exec（那是另一套问题）。
3. 冷启动与堆占用是否与 macOS 同量级（QuickJS 无 JIT，Android 上 arm64 也是解释执行，
   预期接近）。

## 实测数字

macOS arm64 / release / **真 DeepSeek**：

| 指标 | 值 |
|---|---|
| JS bundle（prelude + agent，含会话+todo） | **364,730 B**；App 的 `pi-bundle/dist/agent.js` **2,950,176 B** → **小 8.1×** |
| guest 冷启动 | **26 – 60 ms**（Runtime + prelude + 364KB bundle eval + boot） |
| QuickJS 堆 | **1.81 MB** 在用 / 2.16 MB malloc（App 是 87MB 的 .so） |
| 会话恢复（18 条消息） | **1.8 ms** |
| 首增量（真网络） | 402 – 929 ms |
| 工具调用（本地 fs） | 0.0 – 1.5 ms |
| 审批等待期间的 tick 数 | **334 拍 / 1000 ms** |
| 整轮（4 次模型请求 + 3 次工具） | 4.4 s |

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
5. **「等审批时没阻塞」这条指标要设计观测窗口**：管道输入是瞬时回答，等待窗口只有
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
