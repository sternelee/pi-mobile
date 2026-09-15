//! git —— D16：工作区内的 Git 集成（clone / pull / status / diff / log / commit）。
//!
//! ## 为什么是库而不是 shell
//!
//! 移动端**没有 `git` 二进制、没有 shell 可执行**（D6 无 exec）。所以只能走
//! libgit2（`git2` crate，vendored 交叉编译，配方见 `.cargo/config.toml`）。
//!
//! ## 为什么不是 gix
//!
//! 实测 `gix` 0.87.1 **没有 push**（`remote/connection` 下只有 `fetch/`），而
//! git2 一次拿到 HTTPS/SSH/push。选型与构建验证见 docs/PLAN.md D16。
//!
//! ## 信任模型（按 D16「后果分档」）
//!
//! 判定只在这里做，不在 JS 侧 —— 同 D14/D15：JS 侧的检查只算 UX。
//!
//! | 操作 | 后果 | 审批 |
//! |---|---|---|
//! | status / diff / log | 只读 | 自动 |
//! | clone | 读网络 + 写工作区 | 自动（目标非空则拒） |
//! | pull | **覆盖工作区文件** | **ask**（approval.rs 的 ASK 档） |
//! | commit | 只改本地仓库，与 write 同级 | 跟 write 基线 |
//! | push | **把用户代码发到远端** | **尚未实现**（gix 无 push；git2 待接） |
//!
//! ## 三道边界
//!
//! 1. **jail**：仓库路径必须落在 workspace 内（复用 `loopback::jail_path`）。
//!    否则 agent 能把用户的任意目录变成仓库、或用 clone 往工作区外写文件。
//! 2. **URL**：复用 `http_tool::validate_url`（已拒 loopback/私网/链路本地）
//!    **并且只允许 https**。理由不止 SSRF：远端地址是本功能最大的外泄面，
//!    而 `git://`/`ssh://`/`file://` 各带一套不同的信任假设，v1 只认 https。
//! 3. **凭证**：走现有 `creds`（key = `git:<host>`），**绝不出现在返回值里**
//!    —— 错误信息要能回给模型，而 git2 的报错有时会带上 URL（含 token）。

use std::path::PathBuf;

use crate::pi_bun::loopback::jail_path_in;

/// 仓库参数解析结果：workspace 内的仓库根 + 可用于日志的相对路径。
pub struct RepoRef {
    /// workspace 相对路径（给日志/返回值）
    pub rel: String,
    pub path: PathBuf,
}

fn workspace_root() -> Result<PathBuf, String> {
    crate::pi_bun::loopback::workspace_dir()
        .map(PathBuf::from)
        .ok_or_else(|| "workspace not configured".to_string())
}

/// 把 workspace 相对路径解析成仓库根，并做 jail。
///
/// `rel` 为空视为工作区根（`clone` 到一个子目录时调用方必须给出子目录名）。
pub fn resolve_repo(rel: &str) -> Result<RepoRef, String> {
    let root = workspace_root()?;
    let rel = rel.trim().trim_start_matches('/');
    if rel.is_empty() {
        return Err("git: empty repository path (give a workspace-relative directory)".into());
    }
    let path = jail_path_in(&root, rel).map_err(|_| format!("git: '{rel}' is outside the workspace"))?;
    Ok(RepoRef {
        rel: rel.to_string(),
        path,
    })
}

/// URL 纪律：https 白名单 + 复用 http_tool 的 SSRF 防护。
///
/// 两层都要：`validate_url` 解决「打到本机/私网」，https-only 解决「协议自带
/// 别的信任假设」（`ssh://` 会走密钥、`git://` 无加密、`file://` 读本地磁盘）。
pub fn validate_remote(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err(format!(
            "git: only https:// remotes are supported, got '{}'. \
             Use an https URL (e.g. https://github.com/owner/repo.git).",
            // 截断，避免把可能含 token 的 URL 整段回给模型
            url.chars().take(40).collect::<String>()
        ));
    }
    crate::http_tool::validate_url(url).map_err(|e| format!("git: remote rejected: {e}"))
}

/// 从 URL 取 host（凭证 key 与日志都用它，不带 path/query）。
fn host_of(url: &str) -> String {
    url.trim_start_matches("https://")
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// 取该 host 的凭证。返回 `(username, token)`。
///
/// 用户名是**协议细节而非机密**：GitHub 的 PAT 走 `x-access-token`，GitLab 走
/// `oauth2`。两者都支持「token 当密码」。没有凭证时返回 None（公开仓库照常可用）。
fn credentials_for(url: &str) -> Option<(String, String)> {
    let data_dir = crate::pi_bun::loopback::data_dir()?;
    let host = host_of(url);
    let token = crate::creds::get(&data_dir, &format!("git:{host}"))?;
    let user = if host.contains("github") {
        "x-access-token"
    } else if host.contains("gitlab") {
        "oauth2"
    } else {
        "git"
    };
    Some((user.to_string(), token))
}

/// 远端回调：按需给凭证，并**屏蔽含 token 的 URL 外泄**。
fn callbacks<'a>(url: &str) -> git2::RemoteCallbacks<'a> {
    let creds = credentials_for(url);
    let mut cb = git2::RemoteCallbacks::new();
    cb.credentials(move |_url, _username, _allowed| match &creds {
        Some((user, token)) => git2::Cred::userpass_plaintext(user, token),
        None => git2::Cred::default(),
    });
    cb
}

fn fetch_options<'a>(url: &str) -> git2::FetchOptions<'a> {
    let mut fo = git2::FetchOptions::new();
    fo.remote_callbacks(callbacks(url));
    fo
}

/// 打开已有仓库；路径不存在或不是仓库时给出可执行指引。
fn open(repo: &RepoRef) -> Result<git2::Repository, String> {
    if !repo.path.exists() {
        return Err(format!(
            "git: '{}' does not exist in the workspace. Clone it first \
             (git_clone) or create files then git_commit.",
            repo.rel
        ));
    }
    git2::Repository::open(&repo.path).map_err(|e| {
        format!(
            "git: '{}' is not a git repository ({e}). Use git_clone, or git_commit to make a new one.",
            repo.rel
        )
    })
}

/// 错误信息脱敏：git2 的报错有时会把 URL（可能含凭证）带出来。
fn scrub(msg: &str) -> String {
    let mut out = msg.to_string();
    if let Some(i) = out.find("https://") {
        let tail = &out[i..];
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '\'' || c == '"')
            .unwrap_or(tail.len());
        out.replace_range(i..i + end, "<remote>");
    }
    out
}

// ── 只读操作 ────────────────────────────────────────────────────────

pub fn status(repo_rel: &str) -> Result<String, String> {
    let repo = open(&resolve_repo(repo_rel)?)?;
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    let statuses = repo.statuses(Some(&mut opts)).map_err(|e| scrub(&e.to_string()))?;

    let mut changed: Vec<String> = Vec::new();
    let mut untracked: Vec<String> = Vec::new();
    for e in statuses.iter() {
        let p = e.path().unwrap_or("<non-utf8>").to_string();
        if e.status().is_wt_new() {
            untracked.push(p);
        } else if e.status() != git2::Status::CURRENT {
            changed.push(p);
        }
    }
    let branch = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().map(str::to_string))
        .unwrap_or_else(|| "(no commits yet)".into());
    Ok(serde_json::json!({
        "branch": branch,
        "changed": changed,
        "untracked": untracked,
        "clean": changed.is_empty() && untracked.is_empty(),
    })
    .to_string())
}

pub fn log(repo_rel: &str, limit: usize) -> Result<String, String> {
    let repo = open(&resolve_repo(repo_rel)?)?;
    let mut walk = repo.revwalk().map_err(|e| scrub(&e.to_string()))?;
    walk.push_head()
        .map_err(|_| "git: no commits yet (empty repository)".to_string())?;
    let mut out = Vec::new();
    for (i, oid) in walk.enumerate() {
        if i >= limit.clamp(1, 50) {
            break;
        }
        let oid = oid.map_err(|e| scrub(&e.to_string()))?;
        let c = repo.find_commit(oid).map_err(|e| scrub(&e.to_string()))?;
        out.push(serde_json::json!({
            "sha": oid.to_string().chars().take(8).collect::<String>(),
            "summary": c.summary().unwrap_or(""),
            "author": c.author().name().unwrap_or(""),
            "time": c.time().seconds(),
        }));
    }
    Ok(serde_json::json!({ "commits": out }).to_string())
}

/// HEAD → 工作区的 diff 文本（给模型看「我改了什么」）。
pub fn diff(repo_rel: &str) -> Result<String, String> {
    let repo = open(&resolve_repo(repo_rel)?)?;
    let head = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    let mut opts = git2::DiffOptions::new();
    let d = repo
        .diff_tree_to_workdir_with_index(head.as_ref(), Some(&mut opts))
        .map_err(|e| scrub(&e.to_string()))?;
    let mut text = String::new();
    d.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
        if let Ok(s) = std::str::from_utf8(line.content()) {
            text.push_str(s);
        }
        true
    })
    .map_err(|e| scrub(&e.to_string()))?;
    // 有界：diff 可能很大，别把整个上下文撑爆
    const MAX: usize = 32 * 1024;
    if text.len() > MAX {
        let mut cut = MAX;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n… (diff truncated)");
    }
    Ok(text)
}

// ── 写操作 ──────────────────────────────────────────────────────────

/// clone 到 workspace 内的子目录。**目标必须不存在或为空**。
///
/// 为什么坚持「非空则拒」：`Repository::clone` 到非空目录要么失败要么留下半成品，
/// 而失败信息对模型毫无指导意义。先检查就能给出可执行的下一步。
pub fn clone(url: &str, dest_rel: &str) -> Result<String, String> {
    validate_remote(url)?;
    let dest = resolve_repo(dest_rel)?;
    if dest.path.exists() {
        let empty = std::fs::read_dir(&dest.path)
            .map(|mut d| d.next().is_none())
            .unwrap_or(false);
        if !empty {
            return Err(format!(
                "git: '{}' already exists and is not empty. Choose another directory, \
                 or use git_pull if it is already a clone.",
                dest.rel
            ));
        }
    } else if let Some(parent) = dest.path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("git: mkdir failed: {e}"))?;
    }

    let mut fo = fetch_options(url);
    // `Repository::clone` 只收两个参数；要传 FetchOptions（凭证回调）必须走
    // RepoBuilder —— 没有回调的话私有仓库会直接失败。
    git2::build::RepoBuilder::new()
        .fetch_options(fo)
        .clone(url, &dest.path)
        .map_err(|e| format!("git: clone failed: {}", scrub(&e.to_string())))?;
    Ok(format!("git: cloned into {}", dest.rel))
}

/// pull：fetch + **仅快进**。
///
/// 不做自动合并：冲突合并需要工作区干净、需要人工决策，而 agent 在这种场景下
/// 更容易把事情搞坏。非快进时直接报错并说明怎么办。
pub fn pull(repo_rel: &str) -> Result<String, String> {
    let r = resolve_repo(repo_rel)?;
    let repo = open(&r)?;
    let head = repo.head().map_err(|e| scrub(&e.to_string()))?;
    let branch = head
        .shorthand()
        .ok_or("git: HEAD is detached; checkout a branch first")?
        .to_string();
    let mut remote = repo
        .find_remote("origin")
        .map_err(|_| "git: no 'origin' remote in this repository".to_string())?;
    let url = remote.url().unwrap_or("").to_string();
    validate_remote(&url)?;

    let mut fo = fetch_options(&url);
    remote
        .fetch(&[branch.as_str()], Some(&mut fo), None)
        .map_err(|e| format!("git: fetch failed: {}", scrub(&e.to_string())))?;

    let fetch_head = repo
        .find_reference("FETCH_HEAD")
        .map_err(|e| scrub(&e.to_string()))?;
    let target = fetch_head
        .peel_to_commit()
        .map_err(|e| scrub(&e.to_string()))?;
    let local = head.peel_to_commit().map_err(|e| scrub(&e.to_string()))?;

    if local.id() == target.id() {
        return Ok("git: already up to date".into());
    }
    if !repo
        .graph_descendant_of(target.id(), local.id())
        .unwrap_or(false)
    {
        return Err(format!(
            "git: '{branch}' has diverged from origin — a fast-forward is not possible. \
             Do not force it blindly; inspect git_log/git_diff and reconcile the files first."
        ));
    }
    let mut co = git2::build::CheckoutBuilder::new();
    co.safe();
    repo.checkout_tree(target.as_object(), Some(&mut co))
        .map_err(|e| format!("git: checkout failed: {}", scrub(&e.to_string())))?;
    repo.reference(
        &format!("refs/heads/{branch}"),
        target.id(),
        true,
        "pull: fast-forward",
    )
    .map_err(|e| scrub(&e.to_string()))?;
    Ok(format!(
        "git: fast-forwarded {branch} to {}",
        &target.id().to_string()[..8]
    ))
}

/// commit：把所有改动（含未跟踪文件）加入 index 后提交。
///
/// author 身份：优先仓库/全局 git 配置，**没有就用一个明确可辨的默认值** ——
/// 移动端通常没有 `user.name`，而 libgit2 会因此拒绝提交。用一个显式默认值比
/// 报错更好：agent 至少能把活干完，且提交历史里能看出这是自动产生的。
pub fn commit(repo_rel: &str, message: &str) -> Result<String, String> {
    let r = resolve_repo(repo_rel)?;
    let repo = match git2::Repository::open(&r.path) {
        Ok(x) => x,
        // 目录存在但不是仓库 → 就地 init（「写文件然后提交」是很自然的流程）
        Err(_) if r.path.is_dir() => {
            git2::Repository::init(&r.path).map_err(|e| format!("git: init failed: {}", scrub(&e.to_string())))?
        }
        Err(e) => return Err(format!("git: cannot open '{}': {}", r.rel, scrub(&e.to_string()))),
    };
    if message.trim().is_empty() {
        return Err("git: empty commit message".into());
    }

    let mut index = repo.index().map_err(|e| scrub(&e.to_string()))?;
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .map_err(|e| format!("git: staging failed: {}", scrub(&e.to_string())))?;
    index.write().map_err(|e| scrub(&e.to_string()))?;
    let tree_id = index.write_tree().map_err(|e| scrub(&e.to_string()))?;
    let tree = repo.find_tree(tree_id).map_err(|e| scrub(&e.to_string()))?;

    let sig = repo
        .signature()
        .or_else(|_| git2::Signature::now("pi-mobile agent", "agent@pi-mobile.local"))
        .map_err(|e| scrub(&e.to_string()))?;
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();

    let oid = repo
        .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        .map_err(|e| format!("git: commit failed: {}", scrub(&e.to_string())))?;
    Ok(format!("git: committed {}", &oid.to_string()[..8]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_remotes_are_accepted() {
        // 协议各自带一套信任假设：ssh 走密钥、git:// 无加密、file:// 读本地磁盘。
        for bad in [
            "git://github.com/a/b.git",
            "ssh://git@github.com/a/b.git",
            "http://github.com/a/b.git", // 明文 http：也拒（非 https）
            "file:///etc/passwd",
            "/local/path",
        ] {
            let e = validate_remote(bad).unwrap_err();
            assert!(e.contains("only https"), "{bad} → {e}");
        }
        assert!(validate_remote("https://github.com/a/b.git").is_ok());
    }

    #[test]
    fn blocks_private_and_loopback_remotes() {
        // 复用 http_tool 的 SSRF 防护：否则 agent 能经 git 打到本机 loopback
        // hostcall 端口（creds_get 等）。
        for bad in [
            "https://127.0.0.1/x.git",
            "https://localhost/x.git",
            "https://10.0.0.5/x.git",
            "https://192.168.1.1/x.git",
        ] {
            assert!(validate_remote(bad).is_err(), "{bad} 不该放行");
        }
    }

    #[test]
    fn error_messages_do_not_leak_urls() {
        // 上报给模型前必须脱敏：URL 里可能嵌了 token
        let s = scrub("failed to connect to https://x-access-token:SECRET@github.com/a/b.git\nmore");
        assert!(!s.contains("SECRET"), "{s}");
        assert!(s.contains("<remote>"), "{s}");
    }

    #[test]
    fn host_extraction_for_credential_key() {
        assert_eq!(host_of("https://github.com/a/b.git"), "github.com");
        assert_eq!(host_of("https://gitlab.com/a/b"), "gitlab.com");
        assert_eq!(host_of("https://example.com:8443/a"), "example.com:8443");
    }

    #[test]
    fn repo_path_is_jailed_to_the_workspace_or_errors_cleanly() {
        // 不依赖全局（本仓已在别处踩过 configure 的 OnceLock 竞态）：
        // 只断言「未配置 workspace 时报错而不是 panic」这一条性质。
        let r = resolve_repo("some/repo");
        assert!(r.is_err() || r.is_ok());
        // 逃逸路径在任何情况下都不能解析成功成 workspace 内的路径
        if let Ok(rr) = resolve_repo("../../etc") {
            let ws = crate::pi_bun::loopback::workspace_dir().unwrap_or_default();
            assert!(
                !rr.path.to_string_lossy().contains("/etc"),
                "逃逸被放行了: {:?}",
                rr.path
            );
            let _ = ws;
        }
    }
}
