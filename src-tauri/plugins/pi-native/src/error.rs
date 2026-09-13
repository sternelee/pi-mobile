//! 插件错误类型。移动端形态照抄官方插件
//! （tauri-plugin-geolocation/src/error.rs）：只有一种「原生调用失败」，
//! `PluginInvokeError` 的 Display 已带原生侧 reject 的可读信息，透传即可。

use serde::{ser::Serializer, Serialize};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[cfg(mobile)]
    #[error(transparent)]
    PluginInvoke(#[from] tauri::plugin::mobile::PluginInvokeError),

    /// 桌面端：这些是移动端系统能力，没有对应实现。
    /// 显式报错而不是静默返回空值 —— 静默失败最难排。
    #[cfg(desktop)]
    #[error("pi-native: '{0}' is not available on desktop")]
    Unsupported(&'static str),
}

impl Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.to_string().as_ref())
    }
}
