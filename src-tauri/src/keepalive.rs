//! keepalive —— Android 前台服务保活 + 通知（Kotlin ForegroundService 的 JNI 桥）。
//!
//! agent 运行期（agent_start → agent_end/agent_error）保持前台服务：锁屏/
//! 切后台不再冻结多步任务（进程被 LMC 冻结是 loopback/VM 全链路停摆）。
//! 审批待决时把常驻通知切到高优先级通道，决策后回落工作态。
//!
//! 调用点：loopback 的 agent_event sink（agent_start/agent_end/agent_error）
//! 与 approval.rs（request/respond）。非 Android 平台全部 no-op。

#[cfg(target_os = "android")]
pub fn on_agent_start() {
    imp::start("agent working");
}

#[cfg(target_os = "android")]
pub fn on_agent_end() {
    imp::stop();
}

/// 审批待决：通知升高优先级（服务已在前台，仅换内容与通道）。
#[cfg(target_os = "android")]
pub fn on_approval_pending(tool: &str) {
    imp::notify(imp::CHANNEL_APPROVAL, "Approval required", tool);
}

/// 决策已回填：回落工作态通知（agent 仍在运行）。
#[cfg(target_os = "android")]
pub fn on_approval_resolved() {
    imp::notify(imp::CHANNEL_WORK, "pi mobile", "agent working");
}

// ── 非 Android 平台 no-op ──

#[cfg(not(target_os = "android"))]
pub fn on_agent_start() {}
#[cfg(not(target_os = "android"))]
pub fn on_agent_end() {}
#[cfg(not(target_os = "android"))]
pub fn on_approval_pending(_tool: &str) {}
#[cfg(not(target_os = "android"))]
pub fn on_approval_resolved() {}

#[cfg(target_os = "android")]
mod imp {
    use jni::objects::{JObject, JValue};
    use jni::{JNIEnv, JavaVM};

    // 与 ForegroundService.kt 常量保持一致
    pub const CHANNEL_WORK: &str = "pi_agent_work";
    pub const CHANNEL_APPROVAL: &str = "pi_agent_approval";

    /// 拿 VM + 当前 activity（tauri 的 WryActivity，即 Context），attach 本线程。
    fn with_activity(f: impl FnOnce(&mut JNIEnv, &JObject) -> Result<(), String>) -> Result<(), String> {
        let ctx = ndk_context::android_context();
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }
            .map_err(|e| format!("keepalive: jvm: {e}"))?;
        let mut env = vm
            .attach_current_thread()
            .map_err(|e| format!("keepalive: attach: {e}"))?;
        let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
        let r = f(&mut env, &activity);
        // 不 detach（AttachGuard drop 已处理）；错误只记日志不反传——保活是 best-effort
        if let Err(e) = r {
            crate::pi_bun::logcat(&format!("keepalive: {e}"));
        }
        Ok(())
    }

    fn jstr<'a>(env: &mut JNIEnv<'a>, s: &str) -> Result<jni::objects::JString<'a>, String> {
        env.new_string(s).map_err(|e| format!("new_string: {e}"))
    }

    pub fn start(label: &str) {
        let _ = with_activity(|env, activity| {
            let label = jstr(env, label)?;
            let cls = env
                .find_class("com/sternelee/pi_mobile/ForegroundService")
                .map_err(|e| format!("find class: {e}"))?;
            env.call_static_method(
                cls,
                "start",
                "(Landroid/content/Context;Ljava/lang/String;)V",
                &[JValue::Object(activity), JValue::Object(&label)],
            )
            .map_err(|e| format!("call start: {e}"))?;
            Ok(())
        });
    }

    pub fn notify(channel: &str, title: &str, text: &str) {
        let _ = with_activity(|env, activity| {
            let channel = jstr(env, channel)?;
            let title = jstr(env, title)?;
            let text = jstr(env, text)?;
            let cls = env
                .find_class("com/sternelee/pi_mobile/ForegroundService")
                .map_err(|e| format!("find class: {e}"))?;
            env.call_static_method(
                cls,
                "notify",
                "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
                &[
                    JValue::Object(activity),
                    JValue::Object(&channel),
                    JValue::Object(&title),
                    JValue::Object(&text),
                ],
            )
            .map_err(|e| format!("call notify: {e}"))?;
            Ok(())
        });
    }

    pub fn stop() {
        let _ = with_activity(|env, activity| {
            let cls = env
                .find_class("com/sternelee/pi_mobile/ForegroundService")
                .map_err(|e| format!("find class: {e}"))?;
            env.call_static_method(
                cls,
                "stop",
                "(Landroid/content/Context;)V",
                &[JValue::Object(activity)],
            )
            .map_err(|e| format!("call stop: {e}"))?;
            Ok(())
        });
    }
}
