//! skills —— 技能包管理（D12）。SKILL.md 注入式技能包：指令注入 system prompt，
//! 不执行任意代码（包内脚本仅作参考文本，D6 同款边界）。
//!
//! 存储：`{data_dir}/skills/<id>/SKILL.md`（+ 可选资源文件）+ `registry.json`
//! （id、来源 URL、version/ref、sha256 checksum、启停状态、安装时间）。
//!
//! 安装来源（v1）：https 直链 SKILL.md，或 GitHub 仓库/子目录 URL（经
//! codeload zipball 下载后解包取 SKILL.md 所在目录）。不引 git2-rs——原生
//! 构建在 Android NDK 有风险（@google/genai/@napi-rs 同族教训），zipball
//! 覆盖 github 主流场景，其余 git host 的 git URL 暂不支持。
//! 供应链纪律：记录 source/version/checksum/version ref（更新 = 重装同 id）。

use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// 单技能注入内容上限（防超大 SKILL.md 拖垮上下文）。
const MAX_SKILL_BYTES: usize = 256 * 1024;
/// 下载上限（zipball/原始文件）。
const MAX_DOWNLOAD_BYTES: usize = 8 * 1024 * 1024;

fn skills_dir(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("skills")
}

fn registry_path(data_dir: &str) -> PathBuf {
    skills_dir(data_dir).join("registry.json")
}

fn load_registry(data_dir: &str) -> Vec<serde_json::Value> {
    std::fs::read_to_string(registry_path(data_dir))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

fn save_registry(data_dir: &str, entries: &[serde_json::Value]) -> Result<(), String> {
    let path = registry_path(data_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    let json = serde_json::to_string(entries).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("write registry.json: {e}"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex(&h.finalize())
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// 解析 SKILL.md：frontmatter（name/description）+ 正文。
fn parse_skill_md(text: &str) -> Option<(String, String, String)> {
    let t = text.trim_start();
    let after_fence = t
        .strip_prefix("---\n")
        .or_else(|| t.strip_prefix("---\r\n"))?;
    let (front, body) = after_fence.split_once("---")?;
    let mut name = String::new();
    let mut description = String::new();
    for line in front.lines() {
        if let Some((key, val)) = line.split_once(':') {
            match key.trim() {
                "name" => name = val.trim().to_string(),
                "description" => description = val.trim().to_string(),
                _ => {}
            }
        }
    }
    if name.is_empty() {
        return None;
    }
    Some((name, description, body.trim().to_string()))
}

fn sanitize_id(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    out.trim_matches('-').to_lowercase()
}

/// 从 zip 字节流中提取 SKILL.md 所在目录（取路径最浅的一个，平局取字典序最小）。
/// 返回 (skill_id_from_dirname, skil_md_text, 其余资源文件 rel→bytes)。
fn extract_from_zip(zip_bytes: &[u8]) -> Result<(String, String, Vec<(String, Vec<u8>)>), String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|e| format!("open zip: {e}"))?;
    // 找最浅的 SKILL.md
    let mut candidates: Vec<(usize, String)> = Vec::new();
    for i in 0..archive.len() {
        let file = archive
            .by_index(i)
            .map_err(|e| format!("zip entry: {e}"))?;
        let name = file.name().to_string();
        if name.ends_with("/SKILL.md") || name == "SKILL.md" {
            let depth = name.matches('/').count();
            candidates.push((depth, name));
        }
    }
    candidates.sort();
    let (_, skill_md_path) = candidates.first().ok_or("no SKILL.md found in archive")?;
    let base = skill_md_path
        .strip_suffix("SKILL.md")
        .unwrap_or(skill_md_path)
        .trim_end_matches('/')
        .to_string();
    // github zipball 根目录形如 "repo-ref/"；若 SKILL.md 在根，base 为 "repo-ref"，
    // 技能 id 用 frontmatter name，无需从目录名推断 —— 这里仅用于无 name 兜底。
    let dir_fallback = base
        .rsplit('/')
        .next()
        .unwrap_or("skill")
        .to_string();

    let mut skill_md = String::new();
    let mut resources: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("zip entry: {e}"))?;
        let name = file.name().to_string();
        let in_scope = if base.is_empty() {
            !name.contains('/')
        } else {
            name.starts_with(&format!("{base}/")) || name == base
        };
        if !in_scope || file.is_dir() {
            continue;
        }
        let rel = if base.is_empty() || base == name {
            name.rsplit('/').next().unwrap_or(&name).to_string()
        } else {
            name[base.len() + 1..].to_string()
        };
        if rel.is_empty() {
            continue;
        }
        // 防路径越狱（zip-slip）
        if rel.split('/').any(|s| s == "..") || rel.starts_with('/') {
            continue;
        }
        let mut buf = Vec::new();
        if file.read_to_end(&mut buf).map_err(|e| format!("read entry: {e}"))? > MAX_SKILL_BYTES {
            return Err(format!("skill file too large: {rel}"));
        }
        if rel == "SKILL.md" {
            skill_md = String::from_utf8(buf).map_err(|_| "SKILL.md is not utf-8".to_string())?;
        } else {
            resources.push((rel, buf));
        }
    }
    if skill_md.is_empty() {
        return Err("SKILL.md not readable in archive".into());
    }
    Ok((dir_fallback, skill_md, resources))
}

/// 安装核心（无网络，可测）：给定期望 id 来源与内容字节，落盘 + 登记。
/// bytes 是 zip（github zipball）或 UTF-8 的 SKILL.md 原文。
fn install_from_bytes(
    data_dir: &str,
    source: &str,
    version_ref: &str,
    bytes: &[u8],
) -> Result<serde_json::Value, String> {
    let is_zip = bytes.len() > 4 && bytes[0] == b'P' && bytes[1] == b'K';
    let (id_fallback, skill_md, resources) = if is_zip {
        extract_from_zip(bytes)?
    } else {
        // 直链必须是带 frontmatter 的 SKILL.md 原文
        let text = String::from_utf8(bytes.to_vec()).map_err(|_| "skill file is not utf-8".to_string())?;
        (String::new(), text, Vec::new())
    };

    let (name, description, _body) =
        parse_skill_md(&skill_md).ok_or("SKILL.md frontmatter must define at least 'name'")?;
    let id = sanitize_id(if name.is_empty() { &id_fallback } else { &name });
    if id.is_empty() {
        return Err("cannot derive skill id".into());
    }
    if skill_md.len() > MAX_SKILL_BYTES {
        return Err(format!("SKILL.md too large (limit {MAX_SKILL_BYTES} bytes)"));
    }

    let dir = skills_dir(data_dir).join(&id);
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    // 先清旧内容再写入（更新 = 重装）
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("clean old skill: {e}"))?;
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    std::fs::write(dir.join("SKILL.md"), &skill_md).map_err(|e| format!("write SKILL.md: {e}"))?;
    for (rel, bytes) in &resources {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }
        std::fs::write(p, bytes).map_err(|e| format!("write {rel}: {e}"))?;
    }

    let entry = serde_json::json!({
        "id": id,
        "name": name,
        "description": description,
        "source": source,
        "version": version_ref,
        "checksum": format!("sha256-{}", sha256_hex(skill_md.as_bytes())),
        "enabled": true,
        "installedAt": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    });

    let mut entries: Vec<serde_json::Value> = load_registry(data_dir)
        .into_iter()
        .filter(|e| e["id"].as_str() != Some(id.as_str()))
        .collect();
    entries.push(entry.clone());
    save_registry(data_dir, &entries)?;
    Ok(entry)
}

/// github.com/{owner}/{repo}[/tree/{ref}] → archive zipball URL；
/// 其余 https URL 视为直链 SKILL.md 原样透传。
fn resolve_download_url(url: &str) -> Result<(String, String), String> {
    if !url.starts_with("https://") {
        return Err("skill source must be https".into());
    }
    let Some(rest) = url.strip_prefix("https://github.com/") else {
        return Ok((url.to_string(), "direct".into()));
    };
    let trimmed = rest.trim_end_matches('/');
    let mut segments = trimmed.split('/');
    let owner = segments.next().unwrap_or("");
    let repo = segments.next().unwrap_or("");
    if owner.is_empty() || repo.is_empty() {
        return Err("github URL must be github.com/{owner}/{repo}".into());
    }
    let mut ref_name = "HEAD".to_string();
    let mut after = segments.next();
    if after == Some("tree") {
        after = segments.next();
        if let Some(r) = after {
            if !r.is_empty() {
                ref_name = r.to_string();
            }
        }
    }
    Ok((
        format!("https://github.com/{owner}/{repo}/archive/{ref_name}.zip"),
        ref_name,
    ))
}

/// 网络安装（blocking —— Tauri 命令经 spawn_blocking 调用）。
pub fn install(data_dir: &str, url: &str) -> Result<serde_json::Value, String> {
    let (download_url, version_ref) = resolve_download_url(url)?;
    let agent = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut resp = agent
        .get(&download_url)
        .send()
        .map_err(|e| format!("download: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download failed: HTTP {}", resp.status()));
    }
    let mut bytes = Vec::new();
    resp.read_to_end(&mut bytes)
        .map_err(|e| format!("download read: {e}"))?;
    if bytes.len() > MAX_DOWNLOAD_BYTES {
        return Err(format!("download too large (limit {MAX_DOWNLOAD_BYTES} bytes)"));
    }
    install_from_bytes(data_dir, url, &version_ref, &bytes)
}

pub fn list(data_dir: &str) -> Result<String, String> {
    serde_json::to_string(&load_registry(data_dir)).map_err(|e| format!("serialize: {e}"))
}

pub fn toggle(data_dir: &str, id: &str, enabled: bool) -> Result<(), String> {
    let mut entries = load_registry(data_dir);
    let entry = entries
        .iter_mut()
        .find(|e| e["id"].as_str() == Some(id))
        .ok_or_else(|| format!("no skill named '{id}'"))?;
    entry["enabled"] = serde_json::json!(enabled);
    save_registry(data_dir, &entries)
}

pub fn remove(data_dir: &str, id: &str) -> Result<(), String> {
    let mut entries: Vec<serde_json::Value> = load_registry(data_dir);
    let before = entries.len();
    entries.retain(|e| e["id"].as_str() != Some(id));
    if entries.len() == before {
        return Err(format!("no skill named '{id}'"));
    }
    save_registry(data_dir, &entries)?;
    let dir = skills_dir(data_dir).join(id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("remove skill dir: {e}"))?;
    }
    Ok(())
}

/// hostcall：启用中的技能全集（注入 system prompt 用）。
/// 单技能超限截断；总预算 64KB —— 上下文成本计入 M3 用量可视化（D12）。
pub fn enabled_for_injection(data_dir: &str) -> serde_json::Value {
    const TOTAL_BUDGET: usize = 64 * 1024;
    let mut out = Vec::new();
    let mut total = 0usize;
    for e in load_registry(data_dir) {
        if e["enabled"].as_bool() != Some(true) {
            continue;
        }
        let Some(id) = e["id"].as_str() else { continue };
        let path = skills_dir(data_dir).join(id).join("SKILL.md");
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        let Some((name, description, body)) = parse_skill_md(&text) else { continue };
        let mut clipped = body;
        if clipped.len() > MAX_SKILL_BYTES {
            clipped.truncate(MAX_SKILL_BYTES);
        }
        if total + clipped.len() > TOTAL_BUDGET {
            clipped.truncate(TOTAL_BUDGET.saturating_sub(total));
        }
        total += clipped.len();
        out.push(serde_json::json!({
            "id": id,
            "name": name,
            "description": description,
            "content": clipped,
        }));
        if total >= TOTAL_BUDGET {
            break;
        }
    }
    serde_json::json!({ "skills": out })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zip_bytes(files: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            for (name, content) in files {
                w.start_file(name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                std::io::Write::write_all(&mut w, content.as_bytes()).unwrap();
            }
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    const SKILL_MD: &str = "---\nname: commit-helper\ndescription: Write good commit messages\n---\n\nBody instructions here.\n";

    #[test]
    fn install_raw_md_and_registry_roundtrip() {
        let dir = std::env::temp_dir().join(format!("pi-skills-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_str().unwrap();

        let e = install_from_bytes(d, "https://x/SKILL.md", "HEAD", SKILL_MD.as_bytes()).unwrap();
        assert_eq!(e["id"], "commit-helper");
        assert_eq!(e["enabled"], true);
        assert!(e["checksum"].as_str().unwrap().starts_with("sha256-"));
        assert!(skills_dir(d).join("commit-helper/SKILL.md").exists());

        // 重复安装同 id = 更新（不报重名）
        install_from_bytes(d, "https://x/SKILL.md", "v2", SKILL_MD.as_bytes()).unwrap();
        let listed = serde_json::from_str::<serde_json::Value>(&list(d).unwrap()).unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!(listed[0]["version"], "v2");

        // 启停 + 删除
        toggle(d, "commit-helper", false).unwrap();
        let listed = serde_json::from_str::<serde_json::Value>(&list(d).unwrap()).unwrap();
        assert_eq!(listed[0]["enabled"], false);
        assert!(toggle(d, "nope", true).is_err());
        toggle(d, "commit-helper", true).unwrap();
        remove(d, "commit-helper").unwrap();
        assert!(remove(d, "commit-helper").is_err());
        assert!(!skills_dir(d).join("commit-helper").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_zipball_extracts_shallowest_skill_md() {
        let dir = std::env::temp_dir().join(format!("pi-skills-zip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_str().unwrap();

        let bytes = zip_bytes(&[
            ("repo-main/README.md", "unrelated"),
            ("repo-main/SKILL.md", SKILL_MD),
            ("repo-main/scripts/run.sh", "echo hi"),
            ("repo-main/nested/SKILL.md", "---\nname: deeper\n---\nx"),
        ]);
        let e = install_from_bytes(d, "https://github.com/o/r", "HEAD", &bytes).unwrap();
        assert_eq!(e["id"], "commit-helper"); // 最浅的 SKILL.md 胜出
        assert!(skills_dir(d).join("commit-helper/scripts/run.sh").exists());
        // zipball 根目录整体入包（SKILL.md 所在目录 = 技能内容，资源随行）
        assert!(skills_dir(d).join("commit-helper/README.md").exists());
        let injected = enabled_for_injection(d);
        assert_eq!(injected["skills"][0]["name"], "commit-helper");
        assert_eq!(injected["skills"][0]["content"], "Body instructions here.");

        // 无 SKILL.md 的 zip 拒绝
        let bad = zip_bytes(&[("repo-main/README.md", "x")]);
        assert!(install_from_bytes(d, "https://github.com/o/r2", "HEAD", &bad).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_download_url_forms() {
        let (u, r) = resolve_download_url("https://github.com/o/r").unwrap();
        assert_eq!(u, "https://github.com/o/r/archive/HEAD.zip");
        assert_eq!(r, "HEAD");
        let (u, r) = resolve_download_url("https://github.com/o/r/tree/v1.2").unwrap();
        assert_eq!(u, "https://github.com/o/r/archive/v1.2.zip");
        assert_eq!(r, "v1.2");
        assert!(resolve_download_url("http://x").is_err());
        // 非 github https 直链透传（v1 按单文件 SKILL.md 安装；非 frontmatter 内容在 install 时拒绝）
        let (u, r) = resolve_download_url("https://example.com/SKILL.md").unwrap();
        assert_eq!(u, "https://example.com/SKILL.md");
        assert_eq!(r, "direct");
    }

    #[test]
    fn injection_skips_disabled_and_missing_dirs() {
        let dir = std::env::temp_dir().join(format!("pi-skills-inj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_str().unwrap();

        install_from_bytes(d, "https://x/a.md", "HEAD", SKILL_MD.as_bytes()).unwrap();
        install_from_bytes(
            d,
            "https://x/b.md",
            "HEAD",
            b"---\nname: disabled-one\ndescription: off\n---\noff body",
        )
        .unwrap();
        toggle(d, "disabled-one", false).unwrap();

        let injected = enabled_for_injection(d);
        let skills = injected["skills"].as_array().unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0]["id"], "commit-helper");
        // registry 登记了但目录被手删：静默跳过不 panic
        std::fs::remove_dir_all(skills_dir(d).join("commit-helper")).unwrap();
        let injected = enabled_for_injection(d);
        assert_eq!(injected["skills"].as_array().unwrap().len(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
