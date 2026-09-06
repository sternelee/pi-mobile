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

/// 读 workspace 相对路径文件（approval 计算 diff 等宿主内部用途）。
pub(crate) fn read_workspace_rel(rel: &str) -> Option<String> {
    if rel.starts_with('/') || rel.split('/').any(|s| s == "..") {
        return None;
    }
    let root = WORKSPACE_DIR.get()?;
    std::fs::read_to_string(std::path::Path::new(root).join(rel)).ok()
}

/// 覆盖写 workspace 文件：已有内容先备份（回滚闭环），best-effort。
fn write_with_backup(real: &std::path::Path, content: &str) -> Result<(), String> {
    if real.exists() {
        backup_existing(real);
    }
    std::fs::write(real, content).map_err(|e| format!("write: {e}"))
}

/// 精确文本替换（edit 工具与 approval diff 共用）。
pub(crate) fn apply_edit(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<String, String> {
    let matches = content.matches(old).count();
    if matches == 0 {
        return Err("oldText not found in file".into());
    }
    if matches > 1 && !replace_all {
        return Err(format!(
            "oldText occurs {matches} times — extend it for uniqueness or set replaceAll=true"
        ));
    }
    Ok(if replace_all {
        content.replace(old, new)
    } else {
        content.replacen(old, new, 1)
    })
}

// ── 写前备份与回滚（M3：「改文件 → 审批 → diff 可回滚」闭环）──────────

/// 备份文件名：`{millis}__{rel 中 / 换 __}`；回滚时按同 rel 后缀找最新。
fn backup_name(rel: &str, millis: u128) -> String {
    format!("{millis}__{}", rel.replace('/', "__"))
}

fn backup_dir() -> Option<std::path::PathBuf> {
    DATA_DIR.get().map(|d| std::path::Path::new(d).join("backups"))
}

/// 覆盖写入前保存旧内容（best-effort：备份失败不阻塞写入）。
fn backup_existing(real: &std::path::Path) {
    let (Some(dir), Some(ws)) = (backup_dir(), WORKSPACE_DIR.get()) else {
        return;
    };
    let Ok(rel) = real.strip_prefix(ws).map(|p| p.to_string_lossy().into_owned()) else {
        return;
    };
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::copy(real, dir.join(backup_name(&rel, millis)));
}

/// 文件树数据（workspace 递归展开，MVP：扁平列表 + 深度，UI 缩进渲染）。
/// 深度 ≤6、条目 ≤500，防大目录拖垮桥。
pub fn workspace_tree() -> Result<String, String> {
    let root = WORKSPACE_DIR.get().ok_or("workspace not configured")?;
    let mut out: Vec<serde_json::Value> = Vec::new();
    fn walk(
        dir: &std::path::Path,
        rel: &str,
        depth: usize,
        out: &mut Vec<serde_json::Value>,
    ) {
        const MAX_DEPTH: usize = 6;
        const MAX_ENTRIES: usize = 500;
        if depth > MAX_DEPTH || out.len() >= MAX_ENTRIES {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            if out.len() >= MAX_ENTRIES {
                return;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            let child_rel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            let Ok(meta) = e.metadata() else { continue };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let is_dir = meta.is_dir();
            out.push(serde_json::json!({
                "path": child_rel,
                "kind": if is_dir { "directory" } else { "file" },
                "size": meta.len(),
                "mtimeMs": mtime,
            }));
            if is_dir {
                walk(&e.path(), &child_rel, depth + 1, out);
            }
        }
    }
    let root_path = std::path::Path::new(root);
    walk(root_path, "", 1, &mut out);
    serde_json::to_string(&out).map_err(|e| format!("serialize: {e}"))
}

/// 只读预览：读 workspace 文件（上限 256KB，文件树 UI 用）。
pub fn workspace_read(rel: &str) -> Result<String, String> {
    const MAX_PREVIEW: u64 = 256 * 1024;
    let path = jail_path(rel)?;
    let meta = std::fs::metadata(&path).map_err(|e| format!("stat: {e}"))?;
    if meta.len() > MAX_PREVIEW {
        return Err(format!(
            "file too large for preview: {} bytes (limit {MAX_PREVIEW})",
            meta.len()
        ));
    }
    std::fs::read_to_string(&path).map_err(|e| format!("read: {e}"))
}

/// 指定 workspace 相对路径的最新备份时间戳（无备份 → None）。
pub fn latest_backup_millis(rel: &str) -> Option<u128> {    let dir = backup_dir()?;
    let suffix = format!("__{}", rel.replace('/', "__"));
    let mut latest: Option<u128> = None;
    for e in std::fs::read_dir(&dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix(&suffix) {
            if let Ok(millis) = stem.trim_end_matches('_').parse::<u128>() {
                if latest.is_none_or(|m| millis > m) {
                    latest = Some(millis);
                }
            }
        }
    }
    latest
}

/// 回滚：恢复指定 workspace 相对路径的最新一次备份（消费该备份，
/// 连续调用可逐级回退）。返回恢复的字节数。
pub fn revert_workspace_file(rel: &str) -> Result<u64, String> {
    let real = jail_path(rel)?;
    let dir = backup_dir().ok_or("backups not configured")?;
    let suffix = format!("__{}", rel.replace('/', "__"));
    let mut latest: Option<(u128, std::path::PathBuf)> = None;
    for e in std::fs::read_dir(&dir).map_err(|e| format!("backups: {e}"))?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix(&suffix) {
            if let Ok(millis) = stem.trim_end_matches('_').parse::<u128>() {
                if latest.as_ref().is_none_or(|(m, _)| millis > *m) {
                    latest = Some((millis, e.path()));
                }
            }
        }
    }
    let (_, src) = latest.ok_or_else(|| format!("no backup for {rel}"))?;
    let n = std::fs::copy(&src, &real).map_err(|e| format!("restore: {e}"))?;
    std::fs::remove_file(&src).map_err(|e| format!("consume backup: {e}"))?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 静态 OnceLock 全进程共享，备份/回滚场景合并为一个串行测试。
    #[test]
    fn write_backup_and_revert_roundtrip() {
        let dir = std::env::temp_dir().join(format!("pi-loopback-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ws = dir.join("workspace");
        std::fs::create_dir_all(&ws).unwrap();
        configure(ws.to_str().unwrap(), dir.to_str().unwrap());

        // 新建写入：无备份
        run_tool("write", &json!({ "path": "t.txt", "content": "v1" })).unwrap();
        assert!(!dir.join("backups").exists());

        // 覆盖写入：产生备份
        run_tool("write", &json!({ "path": "t.txt", "content": "v2" })).unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("t.txt")).unwrap(), "v2");

        // 回滚到 v1，备份被消费
        revert_workspace_file("t.txt").unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("t.txt")).unwrap(), "v1");
        assert!(revert_workspace_file("t.txt").is_err()); // 没有更多备份

        // 子目录路径的备份/回滚
        run_tool("write", &json!({ "path": "sub/a.md", "content": "s1" })).unwrap();
        run_tool("write", &json!({ "path": "sub/a.md", "content": "s2" })).unwrap();
        revert_workspace_file("sub/a.md").unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("sub/a.md")).unwrap(), "s1");

        // edit：唯一替换 + 备份生成
        run_tool(
            "edit",
            &json!({ "path": "t.txt", "oldText": "v1", "newText": "v3" }),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("t.txt")).unwrap(), "v3");
        assert!(latest_backup_millis("t.txt").is_some()); // edit 前的 v1 备份
        revert_workspace_file("t.txt").unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("t.txt")).unwrap(), "v1");

        // edit：oldText 未找到 / 多处出现须 replaceAll
        assert!(run_tool("edit", &json!({ "path": "t.txt", "oldText": "zzz", "newText": "x" })).is_err());
        run_tool("write", &json!({ "path": "dup.txt", "content": "aa" })).unwrap();
        assert!(run_tool("edit", &json!({ "path": "dup.txt", "oldText": "a", "newText": "b" })).is_err());
        run_tool(
            "edit",
            &json!({ "path": "dup.txt", "oldText": "a", "newText": "b", "replaceAll": true }),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("dup.txt")).unwrap(), "bb");

        // 文件树
        let tree = workspace_tree().unwrap();
        let v: serde_json::Value = serde_json::from_str(&tree).unwrap();
        let paths: Vec<&str> = v
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x["path"].as_str())
            .collect();
        assert!(paths.contains(&"t.txt"));
        assert!(paths.contains(&"sub/a.md"));
        assert!(paths.iter().any(|p| p.starts_with("sub"))); // 目录项也在树里

        // 只读预览
        assert_eq!(workspace_read("dup.txt").unwrap(), "bb");
        assert!(workspace_read("../../etc/passwd").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// workspace 内真实路径的展示名（相对 workspace 根，避免输出绝对路径噪音）。
fn display_rel(real: &std::path::Path) -> String {
    WORKSPACE_DIR
        .get()
        .and_then(|ws| real.strip_prefix(ws).ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| real.to_string_lossy().into_owned())
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
            write_with_backup(&path, content)?;
            Ok(format!("wrote {} bytes to {}", content.len(), display_rel(&path)))
        }
        "edit" => {
            let path = jail_path(args.get("path").and_then(|v| v.as_str()).ok_or("path?")?)?;
            let old = args.get("oldText").and_then(|v| v.as_str()).ok_or("oldText?")?;
            let new = args.get("newText").and_then(|v| v.as_str()).ok_or("newText?")?;
            let replace_all = args
                .get("replaceAll")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let content = std::fs::read_to_string(&path).map_err(|e| format!("read: {e}"))?;
            let updated = apply_edit(&content, old, new, replace_all)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
            }
            write_with_backup(&path, &updated)?;
            Ok(format!(
                "edited {} ({} → {} bytes)",
                display_rel(&path),
                content.len(),
                updated.len()
            ))
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
                                        display_rel(&p),
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

/// 凭证写入：pi-ai CredentialStore.modify 的宿主后端（空串即删除该 provider）。
fn creds_set(provider: &str, api_key: &str) -> Result<(), String> {
    let data_dir = DATA_DIR.get().ok_or("data dir not configured")?;
    if api_key.is_empty() {
        // 无独立删除 API：写空串等价于未配置（get 返回 None）
        return crate::creds::set(data_dir, provider, "");
    }
    crate::creds::set(data_dir, provider, api_key)
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
        "creds_set" => {
            let provider = payload.get("provider").and_then(|v| v.as_str()).unwrap_or("");
            let api_key = payload.get("apiKey").and_then(|v| v.as_str()).unwrap_or("");
            match creds_set(provider, api_key) {
                Ok(()) => serde_json::json!({ "ok": true }),
                Err(e) => serde_json::json!({ "error": e }),
            }
        }
        "fs" => fs_op(payload),
        "approval_request" => crate::approval::request(payload),
        "ask_user_register" => crate::ask_user::register(payload),
        "mcp_config" => match DATA_DIR.get() {
            Some(dir) => {
                let v = crate::mcp::list(dir).unwrap_or_else(|_| "[]".into());
                let servers =
                    serde_json::from_str::<serde_json::Value>(&v).unwrap_or(serde_json::json!([]));
                serde_json::json!({ "servers": servers })
            }
            None => serde_json::json!({ "servers": [] }),
        },
        "goal_get" => match DATA_DIR.get() {
            Some(dir) => serde_json::json!({ "objective": serde_json::from_str::<serde_json::Value>(&crate::goal::get(dir).unwrap_or_else(|_| "null".into())).unwrap_or(serde_json::Value::Null) }),
            None => serde_json::json!({ "objective": null }),
        },
        "skills_config" => match DATA_DIR.get() {
            Some(dir) => crate::skills::enabled_for_injection(dir),
            None => serde_json::json!({ "skills": [] }),
        },
        "agent_event" => {
            if let Some(sink) = EVENT_SINK.get() {
                sink(&payload.to_string());
            }
            let ev_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("?");
            // M4 保活：agent 运行期保持前台服务（best-effort，失败只进日志）
            match ev_type {
                "agent_start" => crate::keepalive::on_agent_start(),
                "agent_end" | "agent_error" => crate::keepalive::on_agent_end(),
                _ => {}
            }
            logcat(&format!("agent_event: {ev_type}"));
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
