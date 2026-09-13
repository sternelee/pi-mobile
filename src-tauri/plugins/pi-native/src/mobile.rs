//! device 端（iOS / Android）的插件句柄。
//!
//! 与官方插件同款形态：`PluginHandle::run_mobile_plugin(name, payload)` 把
//! 调用投给原生侧同名命令，阻塞等回调。**它是阻塞的** —— 所以 `lib.rs` 的
//! `location` 命令声明为 `async`：Tauri 的同步命令在宿主线程执行，而原生侧
//! 要等 fix，占着主线程会拖住整个 UI。

use serde::de::DeserializeOwned;
use serde_json::Value;
use tauri::{
    plugin::{PluginApi, PluginHandle},
    AppHandle, Runtime,
};

use crate::{Error, LocationArgs};

/// Android 插件类的完全限定名（包名 + 类名）。
#[cfg(target_os = "android")]
const PLUGIN_IDENTIFIER: &str = "com.sternelee.pinative";

/// iOS 侧入口：由 build.rs 把 ios/ 的 Swift 链成静态库后，这个符号才存在。
#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_pi_native);

/// 初始化 Kotlin / Swift 插件类并取回句柄。
pub fn init<R: Runtime, C: DeserializeOwned>(
    _app: &AppHandle<R>,
    api: PluginApi<R, C>,
) -> crate::Result<PiNative<R>> {
    #[cfg(target_os = "android")]
    let handle = api.register_android_plugin(PLUGIN_IDENTIFIER, "PiNativePlugin")?;
    #[cfg(target_os = "ios")]
    let handle = api.register_ios_plugin(init_plugin_pi_native)?;
    Ok(PiNative(handle))
}

/// 原生插件的访问句柄。
pub struct PiNative<R: Runtime>(PluginHandle<R>);

impl<R: Runtime> PiNative<R> {
    /// 取当前位置。
    ///
    /// 超时 / 无可用定位源 / 权限缺失都由**原生侧**判定并以可读信息 reject
    /// —— 这是与官方 Android 实现最关键的差别（那边没有超时，会把调用方
    /// 拖到自己的 hostcall 超时才报一个没有信息量的错误）。
    pub fn location(&self, args: LocationArgs) -> crate::Result<Value> {
        let raw: Value = self
            .0
            .run_mobile_plugin("location", args)
            .map_err(Error::PluginInvoke)?;
        Ok(raw)
    }
}
