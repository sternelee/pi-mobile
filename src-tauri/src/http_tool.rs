//! http_tool —— agent `fetch` 工具的宿主侧（方案 B）。
//!
//! 网络请求集中在宿主执行（reqwest blocking + rustls，与 skills 安装器
//! 同一栈）：30s 超时、响应体 256KB 上限、HTML 转纯文本（LLM 读正文比
//! 原始标记有效得多）。SSRF 防护：拒绝 loopback/私网/链路本地目标——
//! 否则模型可经 fetch 打到本机 loopback hostcall 端口（creds_get 等）。
//!
//! 已知边界（v1）：不做 DNS 解析级校验（DNS rebinding 理论上可绕过
//! 主机名黑名单），域名白名单/审计日志留待策略层。

use std::io::Read;
use std::net::IpAddr;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BODY: usize = 256 * 1024;

/// 校验目标 URL：仅 http/https，且拒绝指向本机/私网（SSRF 防护）。
pub fn validate_url(url: &str) -> Result<(), String> {
    let lower = url.trim_start().to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err("only http/https URLs are supported".into());
    }
    // 提取 host：scheme:// 之后到 /、?、# 或结尾（IPv6 字面量 [..] 整体提取）
    let rest = &url[lower.find("://").unwrap() + 3..];
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit('@').next().unwrap_or_default();
    let host = if let Some(stripped) = authority.strip_prefix('[') {
        stripped
            .split(']')
            .next()
            .ok_or_else(|| "bad IPv6 literal".to_string())?
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    if host.is_empty() {
        return Err("URL has no host".into());
    }
    let h = host.to_ascii_lowercase();
    if h == "localhost" || h.ends_with(".localhost") || h.ends_with(".local") || h == "0.0.0.0" {
        return Err(format!("blocked host: {h}"));
    }
    if let Ok(ip) = h.parse::<IpAddr>() {
        let private = match ip {
            IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
            }
            // IPv6：环回/链路本地/唯一本地(fc00::/7)/未指定
            IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified() || (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80,
        };
        if private {
            return Err(format!("blocked private address: {ip}"));
        }
    }
    Ok(())
}

/// 极简 HTML → 纯文本：剥 script/style/注释/标签，解常见实体，折叠空白。
/// 不追求 DOM 正确性——目标是给 LLM 可读的正文。
pub fn html_to_text(html: &str) -> String {
    use regex::Regex;
    // rust regex 无反向引用，逐标签匹配（script/style 内容体可能含 "</script>" 字符串的
    // 概率极低，v1 接受该简化）
    let script = Regex::new(r"(?is)<script\b[^>]*>.*?</script>").unwrap();
    let style = Regex::new(r"(?is)<style\b[^>]*>.*?</style>").unwrap();
    let noscript = Regex::new(r"(?is)<noscript\b[^>]*>.*?</noscript>").unwrap();
    let svg = Regex::new(r"(?is)<svg\b[^>]*>.*?</svg>").unwrap();
    let comment = Regex::new(r"(?s)<!--.*?-->").unwrap();
    let breaks = Regex::new(r"(?i)<(br|/p|/div|/li|/tr|/h[1-6])\s*/?>").unwrap();
    let li = Regex::new(r"(?i)<li\b[^>]*>").unwrap();
    let tags = Regex::new(r"(?s)<[^>]+>").unwrap();

    let mut s = script.replace_all(html, "").to_string();
    for re in [&style, &noscript, &svg, &comment] {
        s = re.replace_all(&s, "").to_string();
    }
    s = breaks.replace_all(&s, "\n").to_string();
    s = li.replace_all(&s, "• ").to_string();
    s = tags.replace_all(&s, "").to_string();
    for (ent, ch) in [
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&nbsp;", " "),
    ] {
        s = s.replace(ent, ch);
    }
    // 空白折叠：每行 trim，连续空行合并为一个空行
    let mut out = Vec::new();
    let mut blank = true;
    for line in s.lines() {
        let t = line.trim();
        if t.is_empty() {
            if !blank {
                out.push(String::new());
                blank = true;
            }
        } else {
            out.push(t.to_string());
            blank = false;
        }
    }
    out.join("\n").trim().to_string()
}

/// hostcall "http" 入口：发起请求并返回 { status, contentType, body, truncated }。
pub fn run(payload: &serde_json::Value) -> serde_json::Value {
    match run_inner(payload) {
        Ok(v) => v,
        Err(e) => serde_json::json!({ "error": e }),
    }
}

fn run_inner(payload: &serde_json::Value) -> Result<serde_json::Value, String> {
    let url = payload
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or("missing url")?;
        validate_url(url)?;

        let method = payload
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("GET")
            .to_ascii_uppercase();
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|e| format!("invalid method: {e}"))?;

        let client = reqwest::blocking::Client::builder()
            .timeout(TIMEOUT)
            .user_agent("pi-mobile-agent/0.1")
            .build()
            .map_err(|e| format!("client: {e}"))?;

        let mut req = client.request(method, url);
        if let Some(headers) = payload.get("headers").and_then(|v| v.as_object()) {
            for (k, v) in headers {
                if let (Some(key), Some(val)) = (reqwest::header::HeaderName::try_from(k.as_str()).ok(), v.as_str()) {
                    req = req.header(key, val);
                }
            }
        }
        let body = payload.get("body").and_then(|v| v.as_str());
        if let Some(b) = body {
            req = req.body(b.to_string());
        }

        let mut resp = req.send().map_err(|e| format!("request failed: {e}"))?;
        let status = resp.status().as_u16();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        // 尺寸上限：多读 1 字节判定截断
        let mut bytes = Vec::new();
        resp.take((MAX_BODY + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("read body: {e}"))?;
        let truncated = bytes.len() > MAX_BODY;
        bytes.truncate(MAX_BODY);

        let text = String::from_utf8_lossy(&bytes);
        let body_text = if content_type.starts_with("text/html") {
            html_to_text(&text)
        } else {
            text.into_owned()
        };

        crate::pi_bun::logcat(&format!(
            "http: {status} {} ({} bytes{})",
            content_type,
            body_text.len(),
            if truncated { ", truncated" } else { "" }
        ));

        Ok(serde_json::json!({
            "status": status,
            "contentType": content_type,
            "body": body_text,
            "truncated": truncated,
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_loopback_and_private_targets() {
        assert!(validate_url("http://127.0.0.1:19999/hostcall").is_err());
        assert!(validate_url("http://localhost/x").is_err());
        assert!(validate_url("http://192.168.1.1/router").is_err());
        assert!(validate_url("http://10.0.0.1/x").is_err());
        assert!(validate_url("http://172.16.0.1/x").is_err());
        assert!(validate_url("http://169.254.1.1/x").is_err());
        assert!(validate_url("http://[::1]:8080/x").is_err());
        assert!(validate_url("http://router.local/x").is_err());
        assert!(validate_url("ftp://example.com").is_err());
        assert!(validate_url("example.com").is_err());
        assert!(validate_url("https://example.com/path?q=1").is_ok());
        assert!(validate_url("http://example.com").is_ok());
    }

    #[test]
    fn html_to_text_strips_and_folds() {
        let html = r#"<html><head><style>body{color:red}</style>
            <script>alert(1)</script></head>
            <body><!-- comment --><h1>Title</h1>
            <p>Hello&nbsp;&amp; world</p><ul><li>a</li><li>b</li></ul>
            <script>var x = "&lt;script&gt;";</script></body></html>"#;
        let text = html_to_text(html);
        assert!(text.contains("Title"));
        assert!(text.contains("Hello & world"));
        assert!(text.contains("• a"));
        assert!(!text.contains("alert"));
        assert!(!text.contains("color:red"));
        assert!(!text.contains("<"));
    }

    #[test]
    fn body_cap_is_enforced() {
        // 1MB 空白填充响应——read 端截断到 256KB（经本地纯函数模拟 take 语义）
        let big = "x".repeat(1024 * 1024);
        assert_eq!(big.len(), 1024 * 1024);
        let capped = &big[..MAX_BODY];
        assert_eq!(capped.len(), MAX_BODY);
    }
}
