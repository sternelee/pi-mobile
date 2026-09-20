//! workspace —— 宿主路径登记 + workspace 越狱边界 + UI 侧的文件操作。
//!
//! 从 `pi_bun::loopback` 抽出来的（bun 运行时已删，见 docs/PROGRESS.md 第十五轮）：
//! 这些函数从来不属于 bun —— `lib.rs` 的 **UI 侧命令**（`workspace_tree` /
//! `workspace_read` / `workspace_revert` / `workspace_backup_info`）、`preview`、
//! `git` 都在用它们，只是当年跟着 loopback HTTP 宿主一起长在了那个模块里。
//!
//! ## 两套根目录来源（曾经在这上面踩过）
//!
//! | 读谁 | 谁在用 |
//! |---|---|
//! | `HostTools` 里的 root（qjs 的 `Host.workspace`） | agent 自己的文件工具 |
//! | 本模块的几个 `OnceLock` | lib.rs 的 UI 侧命令、preview、git |
//!
//! 两套**都必须喂**：只喂前者时症状是「agent 读写正常、UI 说你没配 workspace」。
//! 所以登记只有一个入口 [`configure_paths`]，由 `agent_init` 调用。
//!
//! 安全规则只保留一份：越狱判定在 [`jail_path`] / [`jail_path_in`]，实现都在
//! `pi_host_tools`（`preview::serve` 是纯函数，用 `jail_path_in` 显式传入 root，
//! 不碰全局 —— 避免与 `configure_paths` 的 OnceLock 互相干扰）。

use std::sync::OnceLock;

static WORKSPACE_DIR: OnceLock<String> = OnceLock::new();
static DATA_DIR: OnceLock<String> = OnceLock::new();
static SESSIONS_DIR: OnceLock<String> = OnceLock::new();

/// 建好标准子目录（workspace / sessions）并登记宿主路径 —— **`agent_init` 的必经一步**。
///
/// 唯一入口的意义：这些 `OnceLock` 是 UI 侧命令唯一的根目录来源，而 agent 工具走的是
/// `HostTools`（另一份）。两套来源并存时，漏登记一套的表现是「一半能用」，很难查。
pub(crate) fn configure_paths(data_dir: &str) -> Result<(), String> {
    let workspace = format!("{data_dir}/workspace");
    let sessions = format!("{data_dir}/sessions");
    for dir in [&workspace, &sessions] {
        std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {dir}: {e}"))?;
    }
    WORKSPACE_DIR.set(workspace).ok();
    DATA_DIR.set(data_dir.into()).ok();
    SESSIONS_DIR.set(sessions).ok();
    Ok(())
}

/// data 根目录（git 凭证按 `git:<host>` 存在这里，见 src/creds.rs）。
pub(crate) fn data_dir() -> Option<String> {
    DATA_DIR.get().cloned()
}

/// workspace 根目录（preview 的静态服务要用它做 jail 根）。
pub(crate) fn workspace_dir() -> Option<String> {
    WORKSPACE_DIR.get().cloned()
}

/// sessions 根目录（sessions 索引的默认位置）。
pub(crate) fn sessions_dir() -> Option<String> {
    SESSIONS_DIR.get().cloned()
}

/// 路径越狱防护：限制在 workspace 内，拒绝绝对路径与 `..`。
pub(crate) fn jail_path(p: &str) -> Result<std::path::PathBuf, String> {
    let root = WORKSPACE_DIR.get().ok_or("workspace not configured")?;
    jail_path_in(std::path::Path::new(root), p)
}

/// 同上，但根目录由调用方给出（`preview::serve` 用这个，不碰全局）。
pub(crate) fn jail_path_in(root: &std::path::Path, p: &str) -> Result<std::path::PathBuf, String> {
    pi_host_tools::jail_path_in(root, p)
}

/// 读 workspace 相对路径文件（approval 计算 diff 等宿主内部用途）。
pub(crate) fn read_workspace_rel(rel: &str) -> Option<String> {
    pi_host_tools::read_workspace_rel(std::path::Path::new(WORKSPACE_DIR.get()?), rel)
}

/// 精确文本替换（edit 工具与 approval diff 共用）。
pub(crate) fn apply_edit(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<String, String> {
    pi_host_tools::apply_edit(content, old, new, replace_all)
}

/// 工具集实例（workspace 与备份根都取自全局配置）。构造很轻（两个 PathBuf）。
pub(crate) fn tools() -> Result<pi_host_tools::HostTools, String> {
    let ws = WORKSPACE_DIR.get().ok_or("workspace not configured")?;
    let data = DATA_DIR.get().ok_or("data dir not configured")?;
    Ok(pi_host_tools::HostTools::new(ws, data))
}

/// 文件树数据（workspace 递归展开，MVP：扁平列表 + 深度，UI 缩进渲染）。
/// 深度 ≤6、条目 ≤500，防大目录拖垮桥。
pub fn workspace_tree() -> Result<String, String> {
    tools()?.workspace_tree()
}

/// 只读预览：读 workspace 文件（上限 256KB，文件树 UI 用）。
pub fn workspace_read(rel: &str) -> Result<String, String> {
    tools()?.workspace_read(rel)
}

/// 指定 workspace 相对路径的最新备份时间戳（无备份 → None）。
pub fn latest_backup_millis(rel: &str) -> Option<u128> {
    tools().ok()?.latest_backup_millis(rel)
}

/// 回滚：恢复指定 workspace 相对路径的最新一次备份（消费该备份，
/// 连续调用可逐级回退）。返回恢复的字节数。
pub fn revert_workspace_file(rel: &str) -> Result<u64, String> {
    tools()?.revert_workspace_file(rel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 工具调用转发（原来 loopback 里的 `run_tool` 是模块内私有包装）
    fn run(name: &str, args: &serde_json::Value) -> Result<String, String> {
        tools()?.run_tool(name, args)
    }

    /// 静态 OnceLock 全进程共享，备份/回滚场景合并为一个串行测试。
    #[test]
    fn write_backup_and_revert_roundtrip() {
        let dir = std::env::temp_dir().join(format!("pi-workspace-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ws = dir.join("workspace");
        std::fs::create_dir_all(&ws).unwrap();
        // 注意传的是 **data_dir**（workspace = {data_dir}/workspace 由它推出）
        configure_paths(&dir.to_string_lossy()).unwrap();

        // 新建写入：无备份
        run("write", &json!({ "path": "t.txt", "content": "v1" })).unwrap();
        assert!(!dir.join("backups").exists());

        // 覆盖写入：产生备份
        run("write", &json!({ "path": "t.txt", "content": "v2" })).unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("t.txt")).unwrap(), "v2");

        // 回滚到 v1，备份被消费
        revert_workspace_file("t.txt").unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("t.txt")).unwrap(), "v1");
        assert!(revert_workspace_file("t.txt").is_err()); // 没有更多备份

        // 子目录路径的备份/回滚
        run("write", &json!({ "path": "sub/a.md", "content": "s1" })).unwrap();
        run("write", &json!({ "path": "sub/a.md", "content": "s2" })).unwrap();
        revert_workspace_file("sub/a.md").unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("sub/a.md")).unwrap(), "s1");

        // edit：唯一替换 + 备份生成
        run(
            "edit",
            &json!({ "path": "t.txt", "oldText": "v1", "newText": "v3" }),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("t.txt")).unwrap(), "v3");
        assert!(latest_backup_millis("t.txt").is_some()); // edit 前的 v1 备份
        revert_workspace_file("t.txt").unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("t.txt")).unwrap(), "v1");

        // edit：oldText 未找到 / 多处出现须 replaceAll
        assert!(run(
            "edit",
            &json!({ "path": "t.txt", "oldText": "zzz", "newText": "x" })
        )
        .is_err());
        run("write", &json!({ "path": "dup.txt", "content": "aa" })).unwrap();
        assert!(run(
            "edit",
            &json!({ "path": "dup.txt", "oldText": "a", "newText": "b" })
        )
        .is_err());
        run(
            "edit",
            &json!({ "path": "dup.txt", "oldText": "a", "newText": "b", "replaceAll": true }),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("dup.txt")).unwrap(), "bb");

        // mkdir：嵌套目录创建；已存在幂等；目标是文件则拒绝；越狱拒绝
        run("mkdir", &json!({ "path": "src/views" })).unwrap();
        assert!(ws.join("src/views").is_dir());
        run("mkdir", &json!({ "path": "src/views" })).unwrap();
        run("write", &json!({ "path": "f.txt", "content": "x" })).unwrap();
        assert!(run("mkdir", &json!({ "path": "f.txt" })).is_err());
        assert!(run("mkdir", &json!({ "path": "../escape" })).is_err());

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
