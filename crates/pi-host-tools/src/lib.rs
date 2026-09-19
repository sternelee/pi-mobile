//! pi-host-tools —— agent 工具的本机实现（workspace 越狱保护 + 写前备份/回滚）。
//!
//! 从 `src-tauri/src/pi_bun/loopback.rs` 原样抽出（2026-09-19，spike/quickjs-agent）：
//! 每个函数都把根目录**显式传入**，不再读模块级 `OnceLock`，这样同一份实现既能被
//! Tauri 宿主用（`loopback.rs` 保留同名薄包装，行为不变），也能被桌面/移动端的
//! 其他宿主直接复用（见 `spikes/quickjs-agent`）。
//!
//! 抽出的动机来自 `docs/POCKET-PI-NOTES.md` 的结论：工具实现在「薄 JS + 厚原生」
//! 路线里是**已经沉没的成本**，应当可复用而不是重写。安全规则（越狱判定）只保留
//! 这一份 —— 见 [`jail_path_in`]。
//!
//! 模块划分（两个**不同的 jail 根**，别混）：
//!   · 本文件：workspace 根 —— agent 的文件工具（read/write/edit/…）；
//!   · [`sessions_fs`]：sessions 根 —— pi `JsonlSessionRepo` 背后的 fs 通道
//!     （带 `/pi-sessions` 虚拟前缀）。
//!
//! 另有 [`http`]：agent 的 `fetch` 工具（SSRF 防护 + HTML 正文抽取），本来就无状态。

pub mod http;
mod sessions_fs;

pub use sessions_fs::{fs_err, fs_op, SESSIONS_VIRTUAL_ROOT};

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// 同上，但根目录由调用方给出。
///
/// 拆出来是为了**不重复那条越狱规则**（安全规则只能有一份）：`preview::serve`
/// 是纯函数（不碰全局，否则会与 `configure` 的 OnceLock 互相干扰），它需要
/// 用自己的 root 做同一个判定。
///
/// 本 crate 的**公开入口**：越狱判定只有这一处，任何宿主都走它。
pub fn jail_path_in(root: &std::path::Path, p: &str) -> Result<std::path::PathBuf, String> {
    if p.starts_with('/') || p.split('/').any(|seg| seg == "..") || p.contains('\\') {
        return Err(format!("path outside workspace: {p}"));
    }
    Ok(root.join(p))
}

/// 精确文本替换（edit 工具与 approval diff 共用）。
pub fn apply_edit(
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

fn backup_dir(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("backups")
}

/// 覆盖写入前保存旧内容（best-effort：备份失败不阻塞写入）。
fn backup_existing(
    workspace: &std::path::Path,
    data_dir: &std::path::Path,
    real: &std::path::Path,
) {
    let dir = backup_dir(data_dir);
    let ws = workspace;
    let Ok(rel) = real
        .strip_prefix(ws)
        .map(|p| p.to_string_lossy().into_owned())
    else {
        return;
    };
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::copy(real, dir.join(backup_name(&rel, millis)));
}

/// 覆盖写 workspace 文件：已有内容先备份（回滚闭环），best-effort。
fn write_with_backup(
    workspace: &std::path::Path,
    data_dir: &std::path::Path,
    real: &std::path::Path,
    content: &str,
) -> Result<(), String> {
    if real.exists() {
        backup_existing(workspace, data_dir, real);
    }
    std::fs::write(real, content).map_err(|e| format!("write: {e}"))
}

/// 读 workspace 相对路径文件（approval 计算 diff 等宿主内部用途）。
pub fn read_workspace_rel(root: &std::path::Path, rel: &str) -> Option<String> {
    if rel.starts_with('/') || rel.split('/').any(|s| s == "..") {
        return None;
    }
    std::fs::read_to_string(root.join(rel)).ok()
}

/// 文件树数据（workspace 递归展开，MVP：扁平列表 + 深度，UI 缩进渲染）。
/// 深度 ≤6、条目 ≤500，防大目录拖垮桥。
pub fn workspace_tree(workspace: &std::path::Path) -> Result<String, String> {
    let root = workspace;
    let mut out: Vec<serde_json::Value> = Vec::new();
    fn walk(dir: &std::path::Path, rel: &str, depth: usize, out: &mut Vec<serde_json::Value>) {
        const MAX_DEPTH: usize = 6;
        const MAX_ENTRIES: usize = 500;
        if depth > MAX_DEPTH || out.len() >= MAX_ENTRIES {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            if out.len() >= MAX_ENTRIES {
                return;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            let child_rel = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
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
    walk(root, "", 1, &mut out);
    serde_json::to_string(&out).map_err(|e| format!("serialize: {e}"))
}

/// 只读预览：读 workspace 文件（上限 256KB，文件树 UI 用）。
pub fn workspace_read(workspace: &std::path::Path, rel: &str) -> Result<String, String> {
    const MAX_PREVIEW: u64 = 256 * 1024;
    let path = jail_path_in(workspace, rel)?;
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
pub fn latest_backup_millis(data_dir: &std::path::Path, rel: &str) -> Option<u128> {
    let dir = backup_dir(data_dir);
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
pub fn revert_workspace_file(
    workspace: &std::path::Path,
    data_dir: &std::path::Path,
    rel: &str,
) -> Result<u64, String> {
    let real = jail_path_in(workspace, rel)?;
    let dir = backup_dir(data_dir);
    let suffix = format!("__{}", rel.replace('/', "__"));
    let mut latest: Option<(u128, std::path::PathBuf)> = None;
    for e in std::fs::read_dir(&dir)
        .map_err(|e| format!("backups: {e}"))?
        .flatten()
    {
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

/// workspace 内真实路径的展示名（相对 workspace 根，避免输出绝对路径噪音）。
fn display_rel_in(workspace: &std::path::Path, real: &std::path::Path) -> String {
    real.strip_prefix(workspace)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| real.to_string_lossy().into_owned())
}

/// 工具实现（M2 子集：read/write/ls/grep；D6：不提供 exec）。
fn run_tool(
    workspace: &std::path::Path,
    data_dir: &std::path::Path,
    name: &str,
    args: &serde_json::Value,
) -> Result<String, String> {
    match name {
        "read" => {
            let path = jail_path_in(
                workspace,
                args.get("path").and_then(|v| v.as_str()).ok_or("path?")?,
            )?;
            let meta = std::fs::metadata(&path).map_err(|e| format!("stat: {e}"))?;
            if meta.len() > 512 * 1024 {
                return Err(format!("file too large: {} bytes", meta.len()));
            }
            std::fs::read_to_string(&path).map_err(|e| format!("read: {e}"))
        }
        "write" => {
            let path = jail_path_in(
                workspace,
                args.get("path").and_then(|v| v.as_str()).ok_or("path?")?,
            )?;
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or("content?")?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
            }
            write_with_backup(workspace, data_dir, &path, content)?;
            Ok(format!(
                "wrote {} bytes to {}",
                content.len(),
                display_rel_in(workspace, &path)
            ))
        }
        "edit" => {
            let path = jail_path_in(
                workspace,
                args.get("path").and_then(|v| v.as_str()).ok_or("path?")?,
            )?;
            let old = args
                .get("oldText")
                .and_then(|v| v.as_str())
                .ok_or("oldText?")?;
            let new = args
                .get("newText")
                .and_then(|v| v.as_str())
                .ok_or("newText?")?;
            let replace_all = args
                .get("replaceAll")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let content = std::fs::read_to_string(&path).map_err(|e| format!("read: {e}"))?;
            let updated = apply_edit(&content, old, new, replace_all)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
            }
            write_with_backup(workspace, data_dir, &path, &updated)?;
            Ok(format!(
                "edited {} ({} → {} bytes)",
                display_rel_in(workspace, &path),
                content.len(),
                updated.len()
            ))
        }
        "ls" => {
            let path = jail_path_in(
                workspace,
                args.get("path").and_then(|v| v.as_str()).unwrap_or("."),
            )?;
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
            Ok(if out.is_empty() {
                "(empty)".to_string()
            } else {
                out.join("\n")
            })
        }
        "mkdir" => {
            let path = jail_path_in(
                workspace,
                args.get("path").and_then(|v| v.as_str()).ok_or("path?")?,
            )?;
            if path.is_file() {
                return Err(format!(
                    "not a directory: {}",
                    display_rel_in(workspace, &path)
                ));
            }
            std::fs::create_dir_all(&path).map_err(|e| format!("mkdir: {e}"))?;
            Ok(format!(
                "created directory {}",
                display_rel_in(workspace, &path)
            ))
        }
        // D17：删除文件/目录。**默认不递归** —— 删目录要显式传 recursive，
        // 否则只能删空目录。这样「误删整棵树」需要一个明确的动作，而不是默认后果。
        "rm" => {
            let path = jail_path_in(
                workspace,
                args.get("path").and_then(|v| v.as_str()).ok_or("path?")?,
            )?;
            let recursive = args
                .get("recursive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !path.exists() {
                return Err(format!(
                    "no such path: {} (use ls to see what is there)",
                    display_rel_in(workspace, &path)
                ));
            }
            let rel = display_rel_in(workspace, &path);
            if path.is_dir() && !path.is_symlink() {
                if recursive {
                    std::fs::remove_dir_all(&path).map_err(|e| format!("rm -r: {e}"))?;
                } else {
                    std::fs::remove_dir(&path).map_err(|e| {
                        format!(
                            "rm: directory not empty ({e}). Pass recursive: true to remove it                              and everything inside — that cannot be undone."
                        )
                    })?;
                }
            } else {
                std::fs::remove_file(&path).map_err(|e| format!("rm: {e}"))?;
            }
            Ok(format!("removed {rel}"))
        }
        "grep" => {
            let pattern = args
                .get("pattern")
                .and_then(|v| v.as_str())
                .ok_or("pattern?")?;
            let re = regex::Regex::new(pattern).map_err(|e| format!("regex: {e}"))?;
            let base = jail_path_in(
                workspace,
                args.get("path").and_then(|v| v.as_str()).unwrap_or("."),
            )?;
            let mut hits = Vec::new();
            fn walk(
                workspace: &std::path::Path,
                dir: &std::path::Path,
                re: &regex::Regex,
                hits: &mut Vec<String>,
                depth: usize,
            ) {
                if depth > 8 || hits.len() >= 200 {
                    return;
                }
                let Ok(rd) = std::fs::read_dir(dir) else {
                    return;
                };
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        walk(workspace, &p, re, hits, depth + 1);
                    } else if p.extension().is_some_and(|x| {
                        matches!(
                            x.to_str(),
                            Some(
                                "js" | "ts"
                                    | "rs"
                                    | "md"
                                    | "json"
                                    | "toml"
                                    | "txt"
                                    | "html"
                                    | "css"
                            )
                        )
                    }) {
                        if let Ok(s) = std::fs::read_to_string(&p) {
                            for (i, line) in s.lines().enumerate() {
                                if re.is_match(line) {
                                    hits.push(format!(
                                        "{}:{}: {}",
                                        display_rel_in(workspace, &p),
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
            walk(workspace, &base, &re, &mut hits, 0);
            Ok(if hits.is_empty() {
                "(no matches)".into()
            } else {
                hits.join("\n")
            })
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

/// 一个 workspace（+ 备份根）上的工具集。
///
/// 薄封装：真正的实现是上面的自由函数，根目录显式传入。宿主（Tauri `loopback.rs`、
/// spike 的 QuickJS 宿主）各自持有自己的实例，避免全局 `OnceLock` 让这份代码
/// 只能被一个进程配置一次。
#[derive(Clone, Debug)]
pub struct HostTools {
    workspace: std::path::PathBuf,
    data_dir: std::path::PathBuf,
}

impl HostTools {
    pub fn new(
        workspace: impl Into<std::path::PathBuf>,
        data_dir: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self {
            workspace: workspace.into(),
            data_dir: data_dir.into(),
        }
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// 越狱保护：限制在 workspace 内（拒绝绝对路径、`..`、反斜杠）。
    pub fn jail(&self, p: &str) -> Result<std::path::PathBuf, String> {
        jail_path_in(&self.workspace, p)
    }

    /// 工具实现（read/write/edit/ls/mkdir/rm/grep；D6：不提供 exec）。
    pub fn run_tool(&self, name: &str, args: &serde_json::Value) -> Result<String, String> {
        run_tool(&self.workspace, &self.data_dir, name, args)
    }

    /// 覆盖写：已有内容先备份（回滚闭环）。
    pub fn write_with_backup(&self, real: &Path, content: &str) -> Result<(), String> {
        write_with_backup(&self.workspace, &self.data_dir, real, content)
    }

    pub fn read_workspace_rel(&self, rel: &str) -> Option<String> {
        read_workspace_rel(&self.workspace, rel)
    }

    pub fn workspace_tree(&self) -> Result<String, String> {
        workspace_tree(&self.workspace)
    }

    pub fn workspace_read(&self, rel: &str) -> Result<String, String> {
        workspace_read(&self.workspace, rel)
    }

    pub fn latest_backup_millis(&self, rel: &str) -> Option<u128> {
        latest_backup_millis(&self.data_dir, rel)
    }

    pub fn revert_workspace_file(&self, rel: &str) -> Result<u64, String> {
        revert_workspace_file(&self.workspace, &self.data_dir, rel)
    }

    /// workspace 内真实路径的展示名（相对根，避免输出绝对路径噪音）。
    pub fn display_rel(&self, real: &Path) -> String {
        display_rel_in(&self.workspace, real)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 独立 host：workspace 与 data 同根（与 loopback 的测试布局一致）。
    fn host(dir: &Path) -> HostTools {
        HostTools::new(dir.join("workspace"), dir)
    }

    /// 越狱判定是安全规则，只此一份 —— 单独钉住它的三类拒绝。
    #[test]
    fn jail_rejects_absolute_parent_and_backslash() {
        let root = Path::new("/tmp/ws");
        for bad in ["/etc/passwd", "../escape", "a/../../b", r"a\b"] {
            assert!(jail_path_in(root, bad).is_err(), "should reject {bad:?}");
        }
        for ok in ["a.txt", "sub/a.md", "./x"] {
            assert!(jail_path_in(root, ok).is_ok(), "should accept {ok:?}");
        }
        // 空串 = 根目录本身（不是越狱）。注意：`rm` 传空路径 + recursive 会指向
        // workspace 根 —— 这是既有行为（抽取前后一致），已记录待评估。
        assert_eq!(jail_path_in(root, "").unwrap(), root);
    }

    /// 移植自 loopback.rs 的串行往返测试（原测试用全局 OnceLock，这里改用显式根）。
    #[test]
    fn write_backup_and_revert_roundtrip() {
        let dir = std::env::temp_dir().join(format!("pi-host-tools-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ws = dir.join("workspace");
        std::fs::create_dir_all(&ws).unwrap();
        let t = host(&dir);

        // 新建写入：无备份
        t.run_tool("write", &json!({ "path": "t.txt", "content": "v1" }))
            .unwrap();
        assert!(!dir.join("backups").exists());

        // 覆盖写入：产生备份
        t.run_tool("write", &json!({ "path": "t.txt", "content": "v2" }))
            .unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("t.txt")).unwrap(), "v2");

        // 回滚到 v1，备份被消费
        t.revert_workspace_file("t.txt").unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("t.txt")).unwrap(), "v1");
        assert!(t.revert_workspace_file("t.txt").is_err());

        // 子目录路径的备份/回滚
        t.run_tool("write", &json!({ "path": "sub/a.md", "content": "s1" }))
            .unwrap();
        t.run_tool("write", &json!({ "path": "sub/a.md", "content": "s2" }))
            .unwrap();
        t.revert_workspace_file("sub/a.md").unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("sub/a.md")).unwrap(), "s1");

        // edit：唯一替换 + 备份生成
        t.run_tool(
            "edit",
            &json!({ "path": "t.txt", "oldText": "v1", "newText": "v3" }),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("t.txt")).unwrap(), "v3");
        assert!(t.latest_backup_millis("t.txt").is_some());
        t.revert_workspace_file("t.txt").unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("t.txt")).unwrap(), "v1");

        // edit：oldText 未找到 / 多处出现须 replaceAll
        assert!(t
            .run_tool(
                "edit",
                &json!({ "path": "t.txt", "oldText": "zzz", "newText": "x" })
            )
            .is_err());
        t.run_tool("write", &json!({ "path": "dup.txt", "content": "aa" }))
            .unwrap();
        assert!(t
            .run_tool(
                "edit",
                &json!({ "path": "dup.txt", "oldText": "a", "newText": "b" })
            )
            .is_err());
        t.run_tool(
            "edit",
            &json!({ "path": "dup.txt", "oldText": "a", "newText": "b", "replaceAll": true }),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("dup.txt")).unwrap(), "bb");

        // mkdir：嵌套创建；幂等；目标是文件则拒绝；越狱拒绝
        t.run_tool("mkdir", &json!({ "path": "src/views" }))
            .unwrap();
        assert!(ws.join("src/views").is_dir());
        t.run_tool("mkdir", &json!({ "path": "src/views" }))
            .unwrap();
        t.run_tool("write", &json!({ "path": "f.txt", "content": "x" }))
            .unwrap();
        assert!(t.run_tool("mkdir", &json!({ "path": "f.txt" })).is_err());
        assert!(t
            .run_tool("mkdir", &json!({ "path": "../escape" }))
            .is_err());

        // ls / read / grep / rm（D17 默认不递归）
        let ls = t.run_tool("ls", &json!({ "path": "." })).unwrap();
        assert!(ls.contains("t.txt"), "{ls}");
        assert_eq!(
            t.run_tool("read", &json!({ "path": "dup.txt" })).unwrap(),
            "bb"
        );
        let g = t
            .run_tool("grep", &json!({ "pattern": "v1", "path": "." }))
            .unwrap();
        assert!(g.contains("t.txt"), "{g}");
        assert!(t.run_tool("rm", &json!({ "path": "src" })).is_err()); // 非空目录须 recursive
        t.run_tool("rm", &json!({ "path": "src", "recursive": true }))
            .unwrap();
        assert!(!ws.join("src").exists());

        // 文件树
        let tree = t.workspace_tree().unwrap();
        let v: serde_json::Value = serde_json::from_str(&tree).unwrap();
        let paths: Vec<&str> = v
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x["path"].as_str())
            .collect();
        assert!(paths.contains(&"t.txt"));
        assert!(paths.contains(&"sub/a.md"));

        // 只读预览 + 越狱拒绝
        assert_eq!(t.workspace_read("dup.txt").unwrap(), "bb");
        assert!(t.workspace_read("../../etc/passwd").is_err());

        // display_rel 输出相对路径，不泄漏绝对路径
        assert_eq!(t.display_rel(&ws.join("t.txt")), "t.txt");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
