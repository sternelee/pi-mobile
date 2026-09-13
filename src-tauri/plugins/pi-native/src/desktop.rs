//! 桌面端：这些都是移动端系统能力，桌面没有对应实现。
//!
//! 保留一份 `PiNative` 是为了让 `lib.rs` 三端同构编译；调用一律返回明确的
//! unsupported 错误（而不是编译失败，也不是静默返回空值）。正常路径不会打到这里
//! —— 上层 `native::location()` 只在 Android 走本插件。

use serde::de::DeserializeOwned;
use serde_json::Value;
use tauri::{plugin::PluginApi, AppHandle, Runtime};

use crate::{CalendarArgs, Error, LocationArgs, PermissionKind};

pub fn init<R: Runtime, C: DeserializeOwned>(
    _app: &AppHandle<R>,
    _api: PluginApi<R, C>,
) -> crate::Result<PiNative<R>> {
    Ok(PiNative(std::marker::PhantomData))
}

/// `PhantomData<fn() -> R>` 而不是 `PhantomData<R>`：后者会把 `R` 的
/// auto-trait（Send/Sync）继承过来，而 `AppHandle::state::<T>()` 要求
/// `T: Send + Sync + 'static` —— 桌面端就会编译不过。`fn() -> R` 恒为
/// Send+Sync，与 `R` 无关。
pub struct PiNative<R: Runtime>(std::marker::PhantomData<fn() -> R>);

impl<R: Runtime> PiNative<R> {
    pub fn location(&self, _args: LocationArgs) -> crate::Result<Value> {
        Err(Error::Unsupported("location"))
    }

    pub fn calendar(&self, _args: CalendarArgs) -> crate::Result<Value> {
        Err(Error::Unsupported("calendar"))
    }

    pub fn request_permission(&self, _kind: PermissionKind) -> crate::Result<Value> {
        Err(Error::Unsupported("requestPermission"))
    }
}
