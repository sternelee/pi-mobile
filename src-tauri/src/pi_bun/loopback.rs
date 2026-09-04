//! pi_bun/loopback.rs — JS→Rust hostcall 通道（预构建 skal ABI 阶段）。
//!
//! 嵌入式 bun 的原生 fetch（M1 已验证）POST 到 127.0.0.1:<port>/hostcall，
//! 本服务分发到宿主处理器并回 JSON。环回接口、无鉴权面（仅本进程可达——
//! Android 应用沙箱内 127.0.0.1 不跨进程）。
//! 后续切换自有 pi_entry.zig 后由 `__pi_hostcall` 函数指针取代。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

static PORT: OnceLock<u16> = OnceLock::new();

/// 启动（幂等）。返回 loopback 端口（随机，避免固定端口冲突）。
pub fn start() -> Result<u16, String> {
    if let Some(p) = PORT.get() {
        return Ok(*p);
    }
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("loopback bind: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("loopback addr: {e}"))?
        .port();
    PORT.set(port).ok();
    std::thread::Builder::new()
        .name("pi-loopback".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(s) => {
                        std::thread::spawn(move || handle_conn(s));
                    }
                    Err(e) => logcat(&format!("ERROR loopback accept: {e}")),
                }
            }
        })
        .map_err(|e| format!("loopback spawn: {e}"))?;
    Ok(port)
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// hostcall 分发：method → JSON 应答。M2 逐步扩充（creds_get 等）。
fn dispatch(method: &str, payload: &serde_json::Value) -> serde_json::Value {
    match method {
        "ping" => serde_json::json!({
            "pong": true,
            "echo": payload,
            "ts": now_ms(),
        }),
        "log" => {
            logcat(&format!(
                "js: {}",
                payload.get("msg").and_then(|v| v.as_str()).unwrap_or("")
            ));
            serde_json::json!({ "ok": true })
        }
        other => serde_json::json!({ "error": format!("unknown method: {other}") }),
    }
}

fn handle_conn(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));

    // 读头部（直到 \r\n\r\n）
    let mut buf = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(1) => {
                buf.push(byte[0]);
                if buf.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            _ => return, // 连接关闭/超时
        }
        if buf.len() > 16 * 1024 {
            return; // 头部异常大，放弃
        }
    }

    let head = String::from_utf8_lossy(&buf);
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    // Content-Length
    let mut content_length = 0usize;
    for line in lines {
        if let Some(v) = line
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
            .and_then(|v| v.trim().parse::<usize>().ok())
        {
            content_length = v;
        }
    }

    // 读 body
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        stream.read_exact(&mut body).ok();
    }

    let response = if method == "POST" && path == "/hostcall" {
        let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&body);
        match parsed {
            Ok(v) => {
                let m = v
                    .get("method")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string();
                let payload = v.get("payload").cloned().unwrap_or(serde_json::Value::Null);
                serde_json::to_string(&dispatch(&m, &payload))
                    .unwrap_or_else(|_| "{\"error\":\"serialize\"}".into())
            }
            Err(e) => format!("{{\"error\":\"bad json: {e}\"}}"),
        }
    } else {
        "{\"error\":\"not found\"}".into()
    };

    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response.len(),
        response
    );
    stream.write_all(resp.as_bytes()).ok();
    stream.flush().ok();
}

use super::logcat;
