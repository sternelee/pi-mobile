//! oauth —— provider OAuth 登录的宿主侧辅助（方案：回调捕获在宿主）。
//!
//! 为什么回调捕获放宿主：pi-ai 的 login() 流程用 node:http 起本地回调
//! server（仅 CLI 可用），嵌入 JSC 运行时没有；而 Anthropic/OpenAI 的
//! client_id 只注册了 `http://localhost:<port>/callback` 回调，deep link
//! scheme 换不掉。宿主起一次性 HTTP server 捕获回调 → 经 resolver 注入
//! bundle（`__pi_oauth_callback(url)`），provider 侧 redirect_uri 原样保留。
//!
//! `pimobile://` deep link（Android intent-filter + tauri-plugin-deep-link）
//! 是第二通道：deep_link 插件收到 `pimobile://oauth/callback?...` 时同样
//! 注入 bundle——给未来允许自定义 scheme 的 provider 用。
//!
//! 生命周期：`listen(port, path)` 阻塞等待一次回调（10 分钟超时），返回前
//! 自动关停 server；重复 listen 同端口由 bind 失败自然拒绝。

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::Duration;

use base64::Engine;
use sha2::{Digest, Sha256};

/// 单次监听上限（浏览器授权一般几分钟内完成）。
const LISTEN_TIMEOUT: Duration = Duration::from_secs(600);

/// PKCE：verifier（base64url 48B 随机）+ challenge（S256）。
/// 在宿主生成——嵌入 JSC 的 crypto.subtle 可用性不确定，不赌。
pub fn pkce() -> Result<(String, String), String> {
    let mut bytes = [0u8; 48];
    rand::fill(&mut bytes);
    let verifier = b64url(&bytes);
    let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
    Ok((verifier, challenge))
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// 一次性回调捕获。两段式：`bind` 同步绑定（bundle 需在构建授权 URL 前拿到
/// 真实端口，OS 分配 port=0 场景），`wait` 在后台线程等回调并经 `sink` 回传
/// （成功/失败都给浏览器回一个可关闭的 HTML 页，返回前 server 关停）。
pub fn bind(port: u16) -> Result<(TcpListener, u16), String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("bind {port}: {e}"))?;
    let bound = listener
        .local_addr()
        .map_err(|e| format!("local_addr: {e}"))?
        .port();
    Ok((listener, bound))
}

/// 在已绑定的 listener 上等待一次匹配 path 的 GET，回传完整回调 URL。
/// 阻塞直至回调/超时——调用方自行 spawn。
pub fn wait(listener: TcpListener, path: &str, sink: impl FnOnce(String)) -> Result<(), String> {
    let deadline = std::time::Instant::now() + LISTEN_TIMEOUT;
    let result = (|| {
        while std::time::Instant::now() < deadline {
            listener
                .set_nonblocking(true)
                .map_err(|e| format!("nonblocking: {e}"))?;
            std::thread::sleep(Duration::from_millis(100));
            let (mut stream, _) = match listener.accept() {
                Ok(x) => x,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(e) => return Err(format!("accept: {e}")),
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut reader = BufReader::new(match stream.try_clone() {
                Ok(s) => s,
                Err(e) => return Err(format!("clone: {e}")),
            });
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            // 跳过剩余头（保持连接语义简单：响应即关）
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) if line == "\r\n" || line == "\n" || line.is_empty() => break,
                    Ok(_) => continue,
                    Err(_) => break,
                }
            }
            // "GET /callback?code=..&state=.. HTTP/1.1"
            let raw = request_line.split_whitespace().nth(1).unwrap_or("");
            let is_match = raw.starts_with(path)
                || raw.starts_with(&format!("/{path}"))
                || path.is_empty();
            if !is_match {
                respond(&mut stream, 404, "Not found");
                continue;
            }
            let port = listener
                .local_addr()
                .map(|a| a.port())
                .unwrap_or(0);
            let url = format!("http://127.0.0.1:{port}{raw}");
            let has_code = raw.contains("code=");
            let has_error = raw.contains("error=");
            if has_code || has_error {
                respond(
                    &mut stream,
                    if has_code { 200 } else { 400 },
                    if has_code {
                        "pi-mobile: authentication completed. You can close this page."
                    } else {
                        "pi-mobile: authentication did not complete."
                    },
                );
                sink(url);
                return Ok(());
            }
            respond(&mut stream, 400, "Missing code parameter");
        }
        Err("oauth callback listen timed out".into())
    })();
    drop(listener);
    result
}

/// 便捷封装：绑定 + 后台等待，返回绑定端口。
pub fn listen(port: u16, path: &str, sink: impl FnOnce(String) + Send + 'static) -> Result<u16, String> {
    let (listener, bound) = bind(port)?;
    let path = path.to_string();
    std::thread::spawn(move || {
        let _ = wait(listener, &path, sink);
    });
    Ok(bound)
}

fn respond(stream: &mut std::net::TcpStream, status: u16, text: &str) {
    let body = format!(
        "<html><body style='font-family:sans-serif;text-align:center;padding-top:3em'>{text}</body></html>"
    );
    let resp = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_shape_and_determinism() {
        let (v1, c1) = pkce().unwrap();
        let (v2, c2) = pkce().unwrap();
        // base64url 无 padding、长度合理、两次不重复
        assert!(!v1.contains('=') && !c1.contains('='));
        assert_eq!(v1.len(), 64); // 48B → 64 chars
        assert_ne!(v1, v2);
        assert_ne!(c1, c2);
        // challenge = S256(verifier)
        let expect = b64url(&Sha256::digest(v1.as_bytes()));
        assert_eq!(c1, expect);
    }

    #[test]
    fn listen_captures_callback_url() {
        // 起随机高位端口，线程里等 sink；主线程发真实 HTTP 请求
        let port = 49052;
        let sink_url = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink_url2 = sink_url.clone();
        let h = std::thread::spawn(move || {
            listen(port, "/callback", move |url| {
                *sink_url2.lock().unwrap() = Some(url);
            })
        });
        // bind 是同步的（listen 返回前已完成）
        std::thread::sleep(std::time::Duration::from_millis(200));
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(b"GET /callback?code=abc&state=xyz HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        // sink 在等待线程里触发——轮询等它（ listen 现在非阻塞返回）
        for _ in 0..50 {
            if sink_url.lock().unwrap().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let got = sink_url.lock().unwrap().take().unwrap();
        assert!(got.contains("/callback?code=abc&state=xyz"));
        h.join().unwrap().unwrap();
        // 捕获后 server 已关停：再连应失败
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
    }
}
