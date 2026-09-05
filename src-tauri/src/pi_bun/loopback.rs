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
static WORKSPACE_DIR: OnceLock<String> = OnceLock::new();
static DATA_DIR: OnceLock<String> = OnceLock::new();
static SESSIONS_DIR: OnceLock<String> = OnceLock::new();
static EVENT_SINK: OnceLock<Box<dyn Fn(&str) + Send + Sync>> = OnceLock::new();

/// 会话目录的 JS 侧虚拟根（agent-main.js hostFs 同款常量）。
const SESSIONS_VIRTUAL_ROOT: &str = "/pi-sessions";

/// 配置路径（lib.rs 初始化时调用一次）。
pub fn configure(workspace_dir: &str, data_dir: &str) {
    WORKSPACE_DIR.set(workspace_dir.into()).ok();
    DATA_DIR.set(data_dir.into()).ok();
    SESSIONS_DIR.set(format!("{data_dir}/sessions")).ok();
}

/// 注册 JS→WebView 事件转发（agent_event → tauri emit）。
pub fn set_event_sink(f: impl Fn(&str) + Send + Sync + 'static) {
    EVENT_SINK.set(Box::new(f)).ok();
}

/// 路径越狱防护：限制在 workspace 内，拒绝绝对路径与 `..`。
fn jail_path(p: &str) -> Result<std::path::PathBuf, String> {
    let root = WORKSPACE_DIR
        .get()
        .ok_or("workspace not configured")?;
    if p.starts_with('/') || p.split('/').any(|seg| seg == "..") || p.contains('\\') {
        return Err(format!("path outside workspace: {p}"));
    }
    Ok(std::path::Path::new(root).join(p))
}

/// 工具实现（M2 子集：read/write/ls/grep；D6：不提供 exec）。
fn run_tool(name: &str, args: &serde_json::Value) -> Result<String, String> {
    match name {
        "read" => {
            let path = jail_path(args.get("path").and_then(|v| v.as_str()).ok_or("path?")?)?;
            let meta = std::fs::metadata(&path).map_err(|e| format!("stat: {e}"))?;
            if meta.len() > 512 * 1024 {
                return Err(format!("file too large: {} bytes", meta.len()));
            }
            std::fs::read_to_string(&path).map_err(|e| format!("read: {e}"))
        }
        "write" => {
            let path = jail_path(args.get("path").and_then(|v| v.as_str()).ok_or("path?")?)?;
            let content = args.get("content").and_then(|v| v.as_str()).ok_or("content?")?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
            }
            std::fs::write(&path, content).map_err(|e| format!("write: {e}"))?;
            Ok(format!("wrote {} bytes to {}", content.len(), path.display()))
        }
        "ls" => {
            let path = jail_path(args.get("path").and_then(|v| v.as_str()).unwrap_or("."))?;
            let mut out = Vec::new();
            for e in std::fs::read_dir(&path).map_err(|e| format!("ls: {e}"))? {
                let e = e.map_err(|e| format!("entry: {e}"))?;
                let ft = e.file_type().map_err(|e| format!("type: {e}"))?;
                out.push(format!(
                    "{} {}",
                    if ft.is_dir() { "d" } else { "-" },
                    e.file_name().to_string_lossy()
                ));
            }
            Ok(if out.is_empty() { "(empty)".to_string() } else { out.join("\n") })
        }
        "grep" => {
            let pattern = args.get("pattern").and_then(|v| v.as_str()).ok_or("pattern?")?;
            let re = regex::Regex::new(pattern).map_err(|e| format!("regex: {e}"))?;
            let base = jail_path(args.get("path").and_then(|v| v.as_str()).unwrap_or("."))?;
            let mut hits = Vec::new();
            fn walk(
                dir: &std::path::Path,
                re: &regex::Regex,
                hits: &mut Vec<String>,
                depth: usize,
            ) {
                if depth > 8 || hits.len() >= 200 {
                    return;
                }
                let Ok(rd) = std::fs::read_dir(dir) else { return };
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        walk(&p, re, hits, depth + 1);
                    } else if p.extension().is_some_and(|x| {
                        matches!(x.to_str(), Some("js" | "ts" | "rs" | "md" | "json" | "toml" | "txt" | "html" | "css"))
                    }) {
                        if let Ok(s) = std::fs::read_to_string(&p) {
                            for (i, line) in s.lines().enumerate() {
                                if re.is_match(line) {
                                    hits.push(format!(
                                        "{}:{}: {}",
                                        p.display(),
                                        i + 1,
                                        line.chars().take(200).collect::<String>()
                                    ));
                                    if hits.len() >= 200 {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            walk(&base, &re, &mut hits, 0);
            Ok(if hits.is_empty() { "(no matches)".into() } else { hits.join("\n") })
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

/// 凭证：keyring（D4；Android 为沙箱文件态），经 creds 模块。
fn creds_get(provider: &str) -> Result<String, String> {
    let data_dir = DATA_DIR.get().ok_or("data dir not configured")?;
    Ok(crate::creds::get(data_dir, provider).unwrap_or_default())
}

// ── 会话 JSONL 的 fs hostcall（pi 原生 JsonlSessionRepo 的 FileSystem 后端）──
//
// JS 侧路径在 /pi-sessions 虚拟命名空间内；此处剥离前缀并 jail 到
// {data_dir}/sessions。返回值镜像 pi 的 Result 形状
// （{ok:true,value} / {ok:false,error:{code,message}}），JS 零转换透传。

fn fs_ok(value: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "ok": true, "value": value })
}

fn fs_err(code: &str, msg: String) -> serde_json::Value {
    serde_json::json!({ "ok": false, "error": { "code": code, "message": msg } })
}

fn fs_io_err(e: std::io::Error) -> serde_json::Value {
    use std::io::ErrorKind::*;
    let code = match e.kind() {
        NotFound => "not_found",
        PermissionDenied => "permission_denied",
        AlreadyExists => "invalid",
        _ => "unknown",
    };
    fs_err(code, e.to_string())
}

/// 虚拟路径 → 沙箱真实路径。rel 为空表示根目录本身。
fn fs_jail(rel: &str) -> Result<std::path::PathBuf, serde_json::Value> {
    if rel.split('/').any(|s| s == "..") || rel.contains('\\') || rel.starts_with('/') {
        return Err(fs_err(
            "permission_denied",
            format!("path outside sessions root: {rel}"),
        ));
    }
    let root = SESSIONS_DIR
        .get()
        .ok_or_else(|| fs_err("unknown", "sessions dir not configured".into()))?;
    Ok(std::path::Path::new(root).join(rel))
}

/// FileInfo（路径回填虚拟命名空间 —— repo 后续调用消费的是这里的 path）。
fn fs_file_info(rel: &str, path: &std::path::Path, meta: std::fs::Metadata) -> serde_json::Value {
    let kind = if meta.is_symlink() {
        "symlink"
    } else if meta.is_dir() {
        "directory"
    } else {
        "file"
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let virtual_path = if rel.is_empty() {
        SESSIONS_VIRTUAL_ROOT.to_string()
    } else {
        format!("{SESSIONS_VIRTUAL_ROOT}/{rel}")
    };
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    serde_json::json!({
        "name": name,
        "path": virtual_path,
        "kind": kind,
        "size": meta.len(),
        "mtimeMs": mtime_ms,
    })
}

fn fs_op(payload: &serde_json::Value) -> serde_json::Value {
    let op = payload.get("op").and_then(|v| v.as_str()).unwrap_or("");
    let path_arg =
        |p: &serde_json::Value| -> Result<String, serde_json::Value> {
            p.get("path")
                .and_then(|v| v.as_str())
                .map(strip_virtual_root)
                .ok_or_else(|| fs_err("invalid", "path?".into()))
        };
    match op {
        "readTextFile" => {
            let rel = match path_arg(payload) {
                Ok(r) => r,
                Err(e) => return e,
            };
            match fs_jail(&rel).and_then(|p| {
                std::fs::read_to_string(&p).map_err(fs_io_err)
            }) {
                Ok(s) => fs_ok(serde_json::json!(s)),
                Err(e) => e,
            }
        }
        "readTextLines" => {
            let rel = match path_arg(payload) {
                Ok(r) => r,
                Err(e) => return e,
            };
            let max = payload
                .get("maxLines")
                .and_then(|v| v.as_u64())
                .unwrap_or(u64::MAX) as usize;
            match fs_jail(&rel).and_then(|p| std::fs::read_to_string(&p).map_err(fs_io_err)) {
                Ok(s) => fs_ok(serde_json::json!(
                    s.lines().take(max).collect::<Vec<_>>()
                )),
                Err(e) => e,
            }
        }
        "writeFile" => {
            let rel = match path_arg(payload) {
                Ok(r) => r,
                Err(e) => return e,
            };
            let Some(content) = payload.get("content").and_then(|v| v.as_str()) else {
                return fs_err("invalid", "content? (string)".into());
            };
            match fs_jail(&rel)
                .and_then(|p| std::fs::write(&p, content).map_err(fs_io_err))
            {
                Ok(()) => fs_ok(serde_json::json!(null)),
                Err(e) => e,
            }
        }
        "appendFile" => {
            let rel = match path_arg(payload) {
                Ok(r) => r,
                Err(e) => return e,
            };
            let Some(content) = payload.get("content").and_then(|v| v.as_str()) else {
                return fs_err("invalid", "content? (string)".into());
            };
            use std::io::Write;
            match fs_jail(&rel).and_then(|p| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&p)
                    .and_then(|mut f| f.write_all(content.as_bytes()))
                    .map_err(fs_io_err)
            }) {
                Ok(()) => fs_ok(serde_json::json!(null)),
                Err(e) => e,
            }
        }
        "renameFile" => {
            let rel = match path_arg(payload) {
                Ok(r) => r,
                Err(e) => return e,
            };
            let Some(dest) = payload.get("to").and_then(|v| v.as_str()) else {
                return fs_err("invalid", "to?".into());
            };
            let dest = strip_virtual_root(dest);
            match fs_jail(&rel).and_then(|p| {
                fs_jail(&dest)
                    .and_then(|d| std::fs::rename(&p, &d).map_err(fs_io_err))
            }) {
                Ok(()) => fs_ok(serde_json::json!(null)),
                Err(e) => e,
            }
        }
        "fileInfo" => {
            let rel = match path_arg(payload) {
                Ok(r) => r,
                Err(e) => return e,
            };
            match fs_jail(&rel).and_then(|p| {
                std::fs::symlink_metadata(&p)
                    .map(|m| fs_file_info(&rel, &p, m))
                    .map_err(fs_io_err)
            }) {
                Ok(v) => fs_ok(v),
                Err(e) => e,
            }
        }
        "listDir" => {
            let rel = match path_arg(payload) {
                Ok(r) => r,
                Err(e) => return e,
            };
            match fs_jail(&rel).and_then(|p| std::fs::read_dir(&p).map_err(fs_io_err)) {
                Ok(rd) => {
                    let mut items = Vec::new();
                    for e in rd.flatten() {
                        let child_rel = if rel.is_empty() {
                            e.file_name().to_string_lossy().into_owned()
                        } else {
                            format!("{rel}/{}", e.file_name().to_string_lossy())
                        };
                        if let Ok(m) = e.metadata() {
                            items.push(fs_file_info(&child_rel, &e.path(), m));
                        }
                    }
                    items.sort_by(|a, b| {
                        a["name"]
                            .as_str()
                            .unwrap_or("")
                            .cmp(b["name"].as_str().unwrap_or(""))
                    });
                    fs_ok(serde_json::json!(items))
                }
                Err(e) => e,
            }
        }
        "exists" => {
            let rel = match path_arg(payload) {
                Ok(r) => r,
                Err(e) => return e,
            };
            match fs_jail(&rel) {
                Ok(p) => fs_ok(serde_json::json!(p.exists())),
                Err(e) => e,
            }
        }
        "createDir" => {
            let rel = match path_arg(payload) {
                Ok(r) => r,
                Err(e) => return e,
            };
            let recursive = payload
                .get("recursive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            match fs_jail(&rel).and_then(|p| {
                if recursive {
                    std::fs::create_dir_all(&p)
                } else {
                    std::fs::create_dir(&p)
                }
                .map_err(fs_io_err)
            }) {
                Ok(()) => fs_ok(serde_json::json!(null)),
                Err(e) => e,
            }
        }
        "remove" => {
            let rel = match path_arg(payload) {
                Ok(r) => r,
                Err(e) => return e,
            };
            let recursive = payload
                .get("recursive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            match fs_jail(&rel).and_then(|p| {
                if p.is_dir() && !p.is_symlink() {
                    if recursive {
                        std::fs::remove_dir_all(&p)
                    } else {
                        std::fs::remove_dir(&p)
                    }
                } else {
                    std::fs::remove_file(&p)
                }
                .map_err(fs_io_err)
            }) {
                Ok(()) => fs_ok(serde_json::json!(null)),
                Err(e) => e,
            }
        }
        _ => fs_err("invalid", format!("unknown fs op: {op}")),
    }
}

/// "/pi-sessions/x/y" → "x/y"；根本身 → ""。
fn strip_virtual_root(p: &str) -> String {
    if p == SESSIONS_VIRTUAL_ROOT {
        String::new()
    } else if let Some(rest) = p.strip_prefix(&format!("{SESSIONS_VIRTUAL_ROOT}/")) {
        rest.to_string()
    } else {
        p.to_string()
    }
}


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
        "tool" => {
            let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = payload.get("args").cloned().unwrap_or(serde_json::json!({}));
            logcat(&format!("hostcall tool: {name} args={}", args));
            match run_tool(name, &args) {
                Ok(text) => {
                    logcat(&format!("hostcall tool: {name} ok ({} bytes)", text.len()));
                    serde_json::json!({ "text": text })
                }
                Err(e) => {
                    logcat(&format!("hostcall tool: {name} err: {e}"));
                    serde_json::json!({ "error": e })
                }
            }
        }
        "creds_get" => {
            let provider = payload.get("provider").and_then(|v| v.as_str()).unwrap_or("");
            match creds_get(provider) {
                Ok(k) if !k.is_empty() => serde_json::json!({ "apiKey": k }),
                _ => serde_json::json!({ "error": format!("no credential for provider '{provider}' — set it in the app") }),
            }
        }
        "fs" => fs_op(payload),
        "agent_event" => {
            if let Some(sink) = EVENT_SINK.get() {
                sink(&payload.to_string());
            }
            logcat(&format!(
                "agent_event: {}",
                payload.get("type").and_then(|v| v.as_str()).unwrap_or("?")
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
