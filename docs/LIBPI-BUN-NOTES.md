# libpi-bun 工程笔记（M1 输入，源自 skal 源码研究）

> 2026-09-04 调研记录：直接读取 [skal](https://github.com/skal-multiplatform/skal) 的 `patches/skal_entry.zig`（72KB 入口实现）与 `scripts/skal-link.sh` 总结出的可复刻工艺。M1 执行时以本笔记为起点。

## 1. 入口形态（skal_entry.zig → 我们的 pi_entry.zig）

- **语言与依赖**：zig，`@import("bun")` + `bun.jsc` 直接使用 bun 内部模块——因此必须在 bun fork 仓内构建，不能作为独立 crate。
- **线程模型**：宿主线程（UI/main）与 **skal worker 线程**（owns VM）分离；宿主经 `enqueueTaskConcurrent` 投递任务，`ResetEvent` 唤醒 worker 的 `tickPossiblyForever` 循环。→ 我们的 Rust 宿主同样只投递、不直接碰 VM。
- **JSC C API 最小子集**：`JSObjectMakeFunctionWithCallback`（注册原生全局函数 = hostcall 的 JS 侧入口）、`JSObjectMakeArrayBufferWithBytesNoCopy`（零拷贝 ArrayBuffer）、`JSStringCreateWithUTF8CString` / `JSValueProtect` 等——全部是 libJavaScriptCore.a 的稳定公开 ABI。
- **零拷贝桥（v2 参考）**：单块 6 MiB 共享内存 = `[Header 64B][Op ring 4MiB][JS string heap 768KiB][Reply heap 256KiB][Event ring ~1MiB]`；每方向一个原子 seq 计数器同步；JS 侧 `Uint8Array` no-copy 视图，宿主侧裸指针。**三方镜像常量必须一致**（zig / JS / 宿主语言）。

## 2. 构建与链接（skal-link.sh → 我们的 build-libpi-bun.sh）

- 构建命令（bun fork 仓内）：`bun run build:libskal`（macOS host / iOS 模拟器）与 `bun run build:libskal:android`（Android arm64）；产物在 `vendor/bun/build/release/` 与 `vendor/bun/build/android/`。
- **Android**：lib 拷入 app 工程的 jniLibs（与我们 `gen/android/app/src/main/jniLibs/arm64-v8a/` 对应，Rust .so 已在用同一路径）。
- **iOS**：模拟器用预构建 JSC；**真机需从源码构建 WebKit JSC**（skal 的 `build-jsc-ios.sh` + `link-skal-ios.sh`）+ 自备签名。→ 我们的 M5 iOS 真机项要预留这条从源码构建链路。
- **符号陈旧守卫（必抄的实践）**：链接时从 C ABI 头文件（`skal.h`）解析期望导出的符号集，用 `nm` 对照实际 .so/.dylib；缺失只**警告不失败**。原因：缺导出是**静默**的——宿主侧按 nullable lookup 查符号并降级，陈旧二进制照样能跑但每个 RPC 都退化。skal 曾因此浪费两轮完整 benchmark。我们的 `pi_bun.h` + `build-libpi-bun.sh` 必须内置同款守卫。

## 3. 决策记录要点（ENGINE_CHOICE.md）

- iOS 无 JIT 硬约束的解法：JSC interpreter + bytecode cache（`.cjs.jsc`），RN 同款先例，App Store 合规。
- 候选引擎淘汰理由：QuickJS-NG（无 fetch/URL/Streams，需自建 Web API 数年）、Hermes（RN 耦合、ESM 不全）、V8（强制 JIT 被 iOS 禁）、Boa（慢 + spec 不全）。
- bun+JSC 静态链 ≈ 87 MB（Android arm64）。
- **.jsc 字节码缓存与 JSC 版本强耦合**：bundle 与运行时必须同版本构建（我们的 `build-pi-bundle.sh` 要与 `build-libpi-bun.sh` 锁同一 bun 版本）。

## 4. 我们的差异点（相对 skal）

| 维度 | skal | pi-mobile |
|------|------|-----------|
| 宿主 | Flutter/Dart（FFI） | Tauri/Rust（FFI），另有 WebView UI 层 |
| JS 负载 | Solid 渲染 ops（高频小消息 → 零拷贝环是刚需） | agent 事件流（delta 批量 16ms 合并，JSON 通道 v1 够用） |
| worker 线程 | 必须（UI 帧率隔离） | 同样必须（Rust 消息泵不阻塞 Tauri 主线程） |
| 入口 JS | 渲染器 | `pi-bundle/entry.ts`（pi-coding-agent headless） |
| bridge 内容 | 渲染 op ring + store | `agent:delta/tool/done` 事件 + hostcall（凭证/审批） |
