//! http_tool —— agent `fetch` 工具的宿主侧（方案 B）。
//!
//! 网络请求集中在宿主执行（reqwest blocking + rustls，与 skills 安装器
//! 同一栈）：30s 超时、响应体 256KB 上限、HTML 转纯文本（LLM 读正文比
//! 原始标记有效得多）。SSRF 防护：拒绝 loopback/私网/链路本地目标——
//! 否则模型可经 fetch 打到本机 loopback hostcall 端口（creds_get 等）。
//!
//! 已知边界（v1）：不做 DNS 解析级校验（DNS rebinding 理论上可绕过
//! 主机名黑名单），域名白名单/审计日志留待策略层。

// ⚠️ 实现已移到 `pi-host-tools::http`（跨宿主复用，见 docs/POCKET-PI-NOTES.md）。
// 本文件只保留同名转发，签名与行为不变；这个实现本来就是纯函数，所以零改动搬走。

pub use pi_host_tools::http::{html_to_text, validate_url};

/// hostcall "http" 入口：发起请求并返回 { status, contentType, body, truncated }。
pub fn run(payload: &serde_json::Value) -> serde_json::Value {
    pi_host_tools::http::run(payload)
}
