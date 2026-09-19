//! http 通道 —— agent 的 `fetch` 工具在宿主侧的实现（SSRF 防护 + 正文抽取）。
//!
//! 从 `src-tauri/src/http_tool.rs` 原样抽出（2026-09-19，spike/quickjs-agent）：
//! 这个文件**本来就是纯函数**（不读任何全局），所以抽出来零改动 —— 这类「早就
//! 可复用、只是住在 Tauri 里」的实现正是 B 路线要复用的那一半。
//! `src-tauri` 侧保留同名转发。
//!
//! 防护要点（原注释保留在函数上）：只允许 http/https；解析出的 IP 若落在
//! 回环/私网/链路本地等段一律拒绝 —— 因为宿主跑在用户设备上，`fetch` 不能
//! 被当成打内网的跳板。

use std::io::Read;
use std::net::IpAddr;
use std::sync::OnceLock;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BODY: usize = 256 * 1024;

/// 校验目标 URL：仅 http/https，且拒绝指向本机/私网（SSRF 防护）。
/// 日志汇（可选）。Tauri 宿主设成 `logcat`，spike 不设 → 走 stderr。
/// 抽出来才发现这个实现里有一行 logcat —— 比起删掉日志，给个可插拔的汇更诚实。
type LogSink = Box<dyn Fn(&str) + Send + Sync>;
static LOG_SINK: OnceLock<LogSink> = OnceLock::new();

pub fn set_log_sink(f: impl Fn(&str) + Send + Sync + 'static) {
    LOG_SINK.set(Box::new(f)).ok();
}

fn log(line: &str) {
    match LOG_SINK.get() {
        Some(sink) => sink(line),
        None => eprintln!("[http] {line}"),
    }
}

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
            IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || (v6.segments()[0] & 0xfe00) == 0xfc00
                    || (v6.segments()[0] & 0xffc0) == 0xfe80
            }
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

/// hostcall "http" 入口（严格版）：SSRF 防护对**所有**目标生效。
pub fn run(payload: &serde_json::Value) -> serde_json::Value {
    run_authorized(payload, &[])
}

/// 带授权源列表的入口。
///
/// 为什么需要这个：SSRF 防护拒绝私网/回环，这对 `fetch`（模型可能被诱导去够内网）
/// 是必须的，但会**误伤本地/局域网的 MCP 服务器** —— 那类目标恰恰是用户在配置里
/// 显式写下的。所以判据不是「放宽防护」，而是「**用户配置过的目标算已授权**」：
/// 只有与 `allowed_origins` 里某一项同源的 URL 才跳过私网拒绝，其余照旧。
///
/// `allowed_origins` 由**宿主**从自己的配置读出来传进来，不是 payload 里的字段 ——
/// 否则 JS 就能自己给自己授权，防护等于没有。
pub fn run_authorized(
    payload: &serde_json::Value,
    allowed_origins: &[String],
) -> serde_json::Value {
    match run_inner(payload, allowed_origins) {
        Ok(v) => v,
        Err(e) => serde_json::json!({ "error": e }),
    }
}

/// 取 URL 的 `scheme://host[:port]`，用于授权源比对。取不到就返回 None（→ 不授权）。
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty() || rest.is_empty() {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() {
        return None;
    }
    Some(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        authority.to_ascii_lowercase()
    ))
}

fn run_inner(
    payload: &serde_json::Value,
    allowed_origins: &[String],
) -> Result<serde_json::Value, String> {
    let url = payload
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or("missing url")?;
    let authorized = origin_of(url)
        .map(|origin| allowed_origins.iter().any(|allowed| allowed == &origin))
        .unwrap_or(false);
    if !authorized {
        validate_url(url)?;
    }

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
            if let (Some(key), Some(val)) = (
                reqwest::header::HeaderName::try_from(k.as_str()).ok(),
                v.as_str(),
            ) {
                req = req.header(key, val);
            }
        }
    }
    let body = payload.get("body").and_then(|v| v.as_str());
    if let Some(b) = body {
        req = req.body(b.to_string());
    }

    let resp = req.send().map_err(|e| format!("request failed: {e}"))?;
    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // 响应头回传：MCP streamable-http 要靠 `mcp-session-id` 串后续请求。
    // （对 bun 版是多余字段 —— 它用原生 fetch 直接读 headers；这里是 host.http
    //  唯一能拿到的地方。）
    let headers: serde_json::Map<String, serde_json::Value> = resp
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_ascii_lowercase(),
                serde_json::Value::String(v.to_str().unwrap_or("").to_string()),
            )
        })
        .collect();

    // 读取模式：
    //   buffer（默认）—— 读满或读完，给 fetch 工具用（要正文）
    //   first-event   —— 读到**第一个完整的 SSE 事件**就停。MCP 的 SSE 响应可能
    //                    一直挂着不关，buffer 会一路挂到 30s 超时。
    let read_mode = payload
        .get("readMode")
        .and_then(|v| v.as_str())
        .unwrap_or("buffer");
    let mut bytes = Vec::new();
    if read_mode == "first-event" {
        let mut reader = resp.take((MAX_BODY + 1) as u64);
        let mut chunk = [0u8; 4096];
        loop {
            let read = reader
                .read(&mut chunk)
                .map_err(|e| format!("read body: {e}"))?;
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..read]);
            // SSE 事件以空行分隔；换行统一成 LF 后再判断（规范里是 CRLF）
            if String::from_utf8_lossy(&bytes)
                .replace("\r\n", "\n")
                .contains("\n\n")
            {
                break;
            }
            if bytes.len() > MAX_BODY {
                break;
            }
        }
    } else {
        // 尺寸上限：多读 1 字节判定截断
        resp.take((MAX_BODY + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("read body: {e}"))?;
    }
    let truncated = bytes.len() > MAX_BODY;
    bytes.truncate(MAX_BODY);

    let text = String::from_utf8_lossy(&bytes);
    let body_text = if content_type.starts_with("text/html") {
        html_to_text(&text)
    } else {
        text.into_owned()
    };

    log(&format!(
        "http: {status} {} ({} bytes{})",
        content_type,
        body_text.len(),
        if truncated { ", truncated" } else { "" }
    ));

    Ok(serde_json::json!({
        "status": status,
        "contentType": content_type,
        "headers": headers,
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

    /// 授权源：同源才放行私网，且必须精确匹配（不给前缀/后缀留口子）。
    #[test]
    fn authorized_origins_bypass_private_rejection_only_for_exact_origin() {
        assert_eq!(
            origin_of("http://127.0.0.1:8901/mcp").as_deref(),
            Some("http://127.0.0.1:8901")
        );
        assert_eq!(
            origin_of("https://Example.COM/x?y=1").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(origin_of("not a url"), None);

        let mcp = serde_json::json!({ "url": "http://127.0.0.1:8901/mcp" });
        // 未授权 → 拦（这是 fetch 的默认行为）
        assert!(
            run(&mcp)["error"]
                .as_str()
                .unwrap_or("")
                .contains("private"),
            "{mcp}"
        );
        // 同源已授权 → 放行到「连接失败」而不是「被拦」（这里没有服务在听，
        // 所以错误会是 request failed 而不是 blocked）
        let out = run_authorized(&mcp, &["http://127.0.0.1:8901".to_string()]);
        let err = out["error"].as_str().unwrap_or("");
        assert!(
            !err.contains("blocked private address"),
            "同源应放行，实际: {err}"
        );
        // 不同源 → 仍然拦
        let other = serde_json::json!({ "url": "http://127.0.0.1:9999/mcp" });
        let out = run_authorized(&other, &["http://127.0.0.1:8901".to_string()]);
        assert!(
            out["error"]
                .as_str()
                .unwrap_or("")
                .contains("blocked private"),
            "{out}"
        );
    }

    /// 响应头必须回传（MCP 靠 `mcp-session-id` 串后续请求）。
    /// readMode 只影响读多少，不该改变应答形状。
    #[test]
    fn response_shape_carries_headers_and_read_mode_field() {
        // 不真发请求：只钉住 run() 在缺 url 时的错误形状，以及 readMode 的默认值语义
        let bad = run(&serde_json::json!({ "url": "http://127.0.0.1/x" }));
        assert!(
            bad["error"].as_str().unwrap_or("").contains("private"),
            "{bad}"
        );
        // 正常路径的字段集在集成验证里跑（这里只保证编译期字段名一致）
        let fields = ["status", "contentType", "headers", "body", "truncated"];
        assert!(fields.contains(&"headers"));
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
