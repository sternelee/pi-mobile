//! preview —— D15：agent 自写 html/js/css 的预览服务。
//!
//! ## 为什么需要一个真实的 HTTP 源
//!
//! 多文件项目靠**相对路径**互相引用（html → css/js），所以不能把内容内联成
//! `srcdoc`：那会打断相对路径，还要手工处理转义。必须要有一个真源。
//!
//! ## 为什么是**独立端口**
//!
//! 预览跑的是 **agent（LLM）写出来的 JS**，所以「它与谁能同源」是承载性的：
//!
//! * 与 `/hostcall`（`pi_bun::loopback`）**不同源** → 纵深防御。
//! * 真正的防线仍是 `script::REQUIRE_HOST_TOKEN`：预览页拿不到 host token，
//!   即便它自己去打 `/hostcall` 也会被拒。这条已在真机上验证（diagnostic 静默）。
//!
//! 两者是**两层**，不是重复：换端口挡住的是「意外可达」，token 挡住的是
//! 「故意可达」。
//!
//! ## 只读且只服务 workspace
//!
//! 复用 `pi_bun::loopback::jail_path`（拒绝对路径与 `..`），并且**只允许 GET/HEAD**
//! —— 预览页永远不该有能力写任何东西。若不 jail，它就能读到 `creds.json` /
//! `sessions/`，那等于把凭证交给一段 LLM 写的脚本。
//!
//! ## 已知且有意接受的风险（D15）
//!
//! 用户选择「允许脚本 + 允许联网」：预览页可以把数据发到任意外网。这是本模块
//! **无法**防的（我们刻意不拦网络）。能外泄的只有页面自己生成的、或先经审批写进
//! workspace 的东西；预览页够不到 app 的 DOM、拿不到 token、调不了任何工具。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::OnceLock;

static PORT: OnceLock<u16> = OnceLock::new();

/// 启动（幂等）。返回预览端口。
///
/// 懒启动：只在 UI 真的要开预览时才 bind，不给 app 启动路径加东西。
pub fn start() -> Result<u16, String> {
    if let Some(p) = PORT.get() {
        return Ok(*p);
    }
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("preview bind: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("preview addr: {e}"))?
        .port();
    PORT.set(port).ok();
    std::thread::Builder::new()
        .name("pi-preview".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(s) => {
                        std::thread::spawn(move || handle_conn(s));
                    }
                    Err(e) => log(&format!("ERROR preview accept: {e}")),
                }
            }
        })
        .map_err(|e| format!("preview spawn: {e}"))?;
    log(&format!("preview up: port={port}"));
    Ok(port)
}

pub fn port() -> Option<u16> {
    PORT.get().copied()
}

fn log(msg: &str) {
    crate::pi_bun::logcat(&format!("[preview] {msg}"));
}

/// 扩展名 → Content-Type。**必须正确**：浏览器对 `text/html` 才会渲染，
/// 否则（如 application/octet-stream）会变成下载。
fn mime_for(path: &str) -> &'static str {
    let ext = path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "txt" | "md" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// workspace 里的 html 入口候选（给 UI 的选择列表）。
///
/// 有界：限深度与条数。**不做全量遍历**——workspace 里可能有 node_modules 之类
/// （本仓库在 fs/jail 上吃过无界遍历的亏）。
pub fn targets() -> serde_json::Value {
    let root = match crate::pi_bun::loopback::workspace_dir() {
        Some(r) => r,
        None => return serde_json::json!([]),
    };
    let mut out: Vec<String> = Vec::new();
    collect_html(std::path::Path::new(&root), "", 0, &mut out);
    out.sort();
    serde_json::json!(out)
}

const MAX_DEPTH: usize = 4;
const MAX_TARGETS: usize = 50;
const SKIP_DIRS: &[&str] = &["node_modules", ".git", "target", "dist", ".venv"];

fn collect_html(dir: &std::path::Path, prefix: &str, depth: usize, out: &mut Vec<String>) {
    if depth > MAX_DEPTH || out.len() >= MAX_TARGETS {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        if out.len() >= MAX_TARGETS {
            return;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        match e.file_type() {
            Ok(t) if t.is_dir() => collect_html(&e.path(), &rel, depth + 1, out),
            Ok(t) if t.is_file() => {
                let lower = name.to_ascii_lowercase();
                if lower.ends_with(".html") || lower.ends_with(".htm") {
                    out.push(rel);
                }
            }
            _ => {}
        }
    }
}

fn respond(stream: &mut TcpStream, status: &str, mime: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {mime}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
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
                if buf.len() > 16 * 1024 {
                    return;
                }
            }
            _ => return,
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let request_line = head.lines().next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw_path = parts.next().unwrap_or("/");

    // HEAD 用于探活；POST/PUT/DELETE 等一律拒 —— 预览页不该有能力写任何东西。
    if method != "GET" && method != "HEAD" {
        respond(&mut stream, "405 Method Not Allowed", "text/plain", b"read-only");
        return;
    }

    // 查串与锚点不属于文件名；顺带做个 URL 解码（%20 等）。
    let path = raw_path.split(['?', '#']).next().unwrap_or("");
    let decoded = percent_decode(path);
    let rel = decoded.trim_start_matches('/');
    if rel.is_empty() {
        respond(
            &mut stream,
            "200 OK",
            "text/plain; charset=utf-8",
            b"pi-mobile preview: GET /<workspace-relative-path>\n",
        );
        return;
    }
    // 目录请求 → 补 index.html（相对路径引用才不会 404）。
    let rel = if rel.ends_with('/') {
        format!("{rel}index.html")
    } else {
        rel.to_string()
    };

    let full = match crate::pi_bun::loopback::jail_path(&rel) {
        Ok(p) => p,
        Err(e) => {
            log(&format!("deny {rel}: {e}"));
            respond(&mut stream, "403 Forbidden", "text/plain", b"outside workspace");
            return;
        }
    };
    match std::fs::read(&full) {
        Ok(bytes) => {
            let mime = mime_for(&rel);
            if method == "HEAD" {
                respond(&mut stream, "200 OK", mime, b"");
            } else {
                respond(&mut stream, "200 OK", mime, &bytes);
            }
        }
        Err(e) => {
            let msg = format!("cannot read {rel}: {e}");
            respond(&mut stream, "404 Not Found", "text/plain; charset=utf-8", msg.as_bytes());
        }
    }
}

/// 最小 %XX 解码。只做这一件事，不引入 URL 依赖。
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_covers_the_web_trio() {
        // 这三个错了会直接表现为「浏览器不渲染」或「CSS/JS 被拒」。
        assert!(mime_for("a/index.html").starts_with("text/html"));
        assert!(mime_for("x/y.js").starts_with("text/javascript"));
        assert!(mime_for("s.css").starts_with("text/css"));
        assert_eq!(mime_for("noext"), "application/octet-stream");
    }

    #[test]
    fn percent_decode_handles_spaces_and_utf8() {
        assert_eq!(percent_decode("/a%20b/c.html"), "/a b/c.html");
        assert_eq!(percent_decode("/%E4%B8%AD.html"), "/中.html");
        // 不完整/非法的 % 序列按字面保留，不能 panic
        assert_eq!(percent_decode("/a%zz"), "/a%zz");
        assert_eq!(percent_decode("/a%2"), "/a%2");
    }

    #[test]
    fn targets_is_bounded_and_skips_noise() {
        // 无界遍历在这个仓库是踩过的坑，所以只验「不 panic + 形状对」。
        let v = targets();
        assert!(v.is_array());
        assert!(v.as_array().unwrap().len() <= MAX_TARGETS);
    }

    /// 真的起服务、真的走 TCP 发请求 —— 只测 mime 函数不算验过这条链路。
    ///
    /// 重点是两件安全性质：
    ///   1. **只读**：POST 必须 405（预览页永不该有能力写东西）
    ///   2. **jail**：`../creds.json` 这类路径必须 403（否则等于把凭证交给一段
    ///      LLM 写的脚本）
    #[test]
    fn serves_over_real_http_and_refuses_escape_and_writes() {
        let dir = std::env::temp_dir().join(format!("pi-preview-test-{}", std::process::id()));
        let ws = dir.join("workspace");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(ws.join("app")).unwrap();
        std::fs::write(ws.join("app/index.html"), b"<h1>hi</h1>").unwrap();
        std::fs::write(ws.join("app/site.css"), b"h1{color:red}").unwrap();
        // 想让逃逸成立的诱饵：就放在 workspace 旁边
        std::fs::write(dir.join("creds.json"), b"SECRET").unwrap();
        crate::pi_bun::loopback::configure(ws.to_str().unwrap(), dir.to_str().unwrap());

        let port = start().expect("preview start");
        let get = |target: &str| -> String {
            use std::io::{Read, Write};
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(format!("GET {target} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes())
                .unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            out
        };

        let ok = get("/app/index.html");
        assert!(ok.starts_with("HTTP/1.1 200 OK"), "{ok}");
        assert!(ok.contains("text/html"), "Content-Type 错了浏览器就不渲染: {ok}");
        assert!(ok.contains("<h1>hi</h1>"), "{ok}");

        // 相对引用：css 能被取到（这正是不用 srcdoc 的原因）
        let css = get("/app/site.css");
        assert!(css.contains("text/css"), "{css}");

        // 目录请求补 index.html
        assert!(get("/app/").starts_with("HTTP/1.1 200 OK"));

        // 逃逸必须被拒，且**不能**泄露内容
        let esc = get("/../creds.json");
        assert!(esc.starts_with("HTTP/1.1 403"), "{esc}");
        assert!(!esc.contains("SECRET"), "逃逸把 workspace 外的文件泄了: {esc}");

        // 只读
        {
            use std::io::{Read, Write};
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(b"POST /app/index.html HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            assert!(out.starts_with("HTTP/1.1 405"), "{out}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
