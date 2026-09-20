//! 会话 fs 通道 —— pi 的 `JsonlSessionRepo` 背后那 12 个 fs 方法的宿主实现。
//!
//! 从 `src-tauri` 的会话 fs 通道原样抽出（2026-09-19；那张皮现在在
//! `src-tauri/src/workspace.rs` 与 qjs 的 `host.fs`，bun 时代的 loopback 已删），
//! 与 `lib.rs` 的 workspace 工具是**两个不同的 jail 根**：
//!   · 这里：jail 到 sessions 根（`{data}/sessions`），路径带 `/pi-sessions` 虚拟前缀；
//!   · `lib.rs`：jail 到 workspace 根。
//! 抽出的理由同工具：QuickJS 那条路线要跑**同一个** `JsonlSessionRepo`，
//! 这样两条路线的会话文件格式一致（pi-v4 JSONL），可互相打开。
//!
//! 应答形状是 pi 的 `Result`：`{ok:true,value}` / `{ok:false,error:{code,message}}`。

use std::time::UNIX_EPOCH;

/// 会话目录的 JS 侧虚拟根（`pi-bundle/agent-qjs.js` 里的同款常量）。
pub const SESSIONS_VIRTUAL_ROOT: &str = "/pi-sessions";

fn fs_ok(value: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "ok": true, "value": value })
}

/// pi 的 Result 错误形状 —— 也供宿主在通道未配置时复刻同样的应答。
pub fn fs_err(code: &str, msg: String) -> serde_json::Value {
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
fn fs_jail(root: &std::path::Path, rel: &str) -> Result<std::path::PathBuf, serde_json::Value> {
    if rel.split('/').any(|s| s == "..") || rel.contains('\\') || rel.starts_with('/') {
        return Err(fs_err(
            "permission_denied",
            format!("path outside sessions root: {rel}"),
        ));
    }
    Ok(root.join(rel))
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

/// 会话 fs 通道：`{ op, ... }` → pi 的 Result 形状 `{ok,value} / {ok,error}`。
///
/// `sessions_root` 是这条通道的 jail 根（App 里是 `{data}/sessions`）。
pub fn fs_op(sessions_root: &std::path::Path, payload: &serde_json::Value) -> serde_json::Value {
    let op = payload.get("op").and_then(|v| v.as_str()).unwrap_or("");
    let path_arg = |p: &serde_json::Value| -> Result<String, serde_json::Value> {
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
            match fs_jail(sessions_root, &rel)
                .and_then(|p| std::fs::read_to_string(&p).map_err(fs_io_err))
            {
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
            match fs_jail(sessions_root, &rel)
                .and_then(|p| std::fs::read_to_string(&p).map_err(fs_io_err))
            {
                Ok(s) => fs_ok(serde_json::json!(s.lines().take(max).collect::<Vec<_>>())),
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
            match fs_jail(sessions_root, &rel)
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
            match fs_jail(sessions_root, &rel).and_then(|p| {
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
            match fs_jail(sessions_root, &rel).and_then(|p| {
                fs_jail(sessions_root, &dest)
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
            match fs_jail(sessions_root, &rel).and_then(|p| {
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
            match fs_jail(sessions_root, &rel)
                .and_then(|p| std::fs::read_dir(&p).map_err(fs_io_err))
            {
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
            match fs_jail(sessions_root, &rel) {
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
            match fs_jail(sessions_root, &rel).and_then(|p| {
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
            match fs_jail(sessions_root, &rel).and_then(|p| {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup(tag: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("pi-sessions-fs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn write_read_append_and_file_info_roundtrip() {
        let root = setup("roundtrip");
        let p = json!({ "path": "/pi-sessions/a/b.jsonl" });

        let created = fs_op(
            &root,
            &json!({ "op": "createDir", "path": "/pi-sessions/a" }),
        );
        assert_eq!(created["ok"], true, "{created}");
        let written = fs_op(
            &root,
            &json!({ "op": "writeFile", "path": p["path"], "content": "line1\n" }),
        );
        assert_eq!(written["ok"], true, "{written}");
        let appended = fs_op(
            &root,
            &json!({ "op": "appendFile", "path": p["path"], "content": "line2\n" }),
        );
        assert_eq!(appended["ok"], true, "{appended}");

        let read = fs_op(&root, &json!({ "op": "readTextFile", "path": p["path"] }));
        assert_eq!(read["value"], "line1\nline2\n", "{read}");

        let info = fs_op(&root, &json!({ "op": "fileInfo", "path": p["path"] }));
        assert_eq!(info["value"]["kind"], "file", "{info}");
        // 路径回填虚拟命名空间 —— repo 后续调用消费的是这个值
        assert_eq!(info["value"]["path"], "/pi-sessions/a/b.jsonl", "{info}");

        let lines = fs_op(&root, &json!({ "op": "readTextLines", "path": p["path"] }));
        assert_eq!(lines["value"].as_array().unwrap().len(), 2, "{lines}");

        let listed = fs_op(&root, &json!({ "op": "listDir", "path": "/pi-sessions/a" }));
        assert_eq!(listed["ok"], true, "{listed}");

        // 字段名是 `to`（pi 的 FileSystem.renameFile(path, to) 口径）
        let moved = fs_op(
            &root,
            &json!({ "op": "renameFile", "path": p["path"], "to": "/pi-sessions/a/c.jsonl" }),
        );
        assert_eq!(moved["ok"], true, "{moved}");
        assert!(root.join("a/c.jsonl").exists());

        let removed = fs_op(
            &root,
            &json!({ "op": "remove", "path": "/pi-sessions/a/c.jsonl" }),
        );
        assert_eq!(removed["ok"], true, "{removed}");
        assert!(!root.join("a/c.jsonl").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 这个 jail 是安全边界：越狱必须被拒（与 workspace jail 是两套根、两套规则）。
    #[test]
    fn jail_rejects_escapes_and_reports_pi_error_shape() {
        let root = setup("jail");
        for bad in [
            "/pi-sessions/../../etc/passwd",
            "a/../../b",
            "..",
            "/etc/passwd",
        ] {
            let out = fs_op(&root, &json!({ "op": "readTextFile", "path": bad }));
            assert_eq!(out["ok"], false, "{bad} should be rejected: {out}");
            assert_eq!(out["error"]["code"], "permission_denied", "{out}");
        }
        let missing = fs_op(
            &root,
            &json!({ "op": "readTextFile", "path": "/pi-sessions/nope.txt" }),
        );
        assert_eq!(missing["error"]["code"], "not_found", "{missing}");
        let unknown = fs_op(&root, &json!({ "op": "nope" }));
        assert_eq!(unknown["error"]["code"], "invalid", "{unknown}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
