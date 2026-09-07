//! keepalive —— Android 前台服务保活 + 通知（目前为 no-op，见下）。
//!
//! 设计意图：agent 运行期（agent_start → agent_end/agent_error）经 JNI 桥
//! 保持 Kotlin ForegroundService 前台通知，锁屏/切后台不冻结多步任务；审批
//! 待决时通知切高优先级通道，决策后回落。
//!
//! ⚠️ 现状（两次真机事故后回退为 no-op）：
//! 1. ndk_context 版：tauri v2 依赖树无人调用 initialize_android_context，
//!    android_context() 必 panic 并杀死宿主线程——approval_request 的
//!    loopback 连接线程被杀（"socket closed unexpectedly"）、approval_respond
//!    task 被杀（决策丢失）。保活本身也从未真正生效。
//! 2. wry dispatch 版：把 JNI 闭包派发进 wry main pipe 线程，闭包内任何
//!    panic 都会带崩事件循环（应用闪退）。
//!
//! 教训：保活是旁路增强，绝不能让它影响主链路。恢复实现时的硬性要求：
//! JNI 调用与宿主线程隔离（独立 JNI 线程或 tauri 插件通知通道）、闭包内
//! catch_unwind、失败只记日志。

#[cfg(target_os = "android")]
pub fn on_agent_start() {
    crate::pi_bun::logcat("keepalive: no-op (disabled after JNI crashes)");
}

#[cfg(target_os = "android")]
pub fn on_agent_end() {}

/// 审批待决：通知升高优先级（no-op）。
#[cfg(target_os = "android")]
pub fn on_approval_pending(_tool: &str) {}

/// 决策已回填：回落工作态通知（no-op）。
#[cfg(target_os = "android")]
pub fn on_approval_resolved() {}

// ── 非 Android 平台 no-op ──

#[cfg(not(target_os = "android"))]
pub fn on_agent_start() {}
#[cfg(not(target_os = "android"))]
pub fn on_agent_end() {}
#[cfg(not(target_os = "android"))]
pub fn on_approval_pending(_tool: &str) {}
#[cfg(not(target_os = "android"))]
pub fn on_approval_resolved() {}
