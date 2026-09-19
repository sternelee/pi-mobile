//! skills 注入半 —— 把启用中的技能拼进 system prompt（D12 的运行时一侧）。
//!
//! 从 `src-tauri/src/skills.rs` 抽出（2026-09-19，spike/quickjs-agent）。
//! **只抽「注入」这一半**：注册表读取 + SKILL.md 解析 + 预算裁剪。安装器那一半
//! （git2 拉取 / zip 解包 / sha256 校验）留在 src-tauri —— 它绑定 git2 与 zip，
//! 换个宿主就得重写。这条切分线本身就说明了 D12 里哪些是可复用的。
//!
//! 语义与 App 完全一致：只看 registry 里 `enabled` 的条目；单技能 256KB 上限、
//! 总量 64KB 预算，超了截断 —— 这些数值照抄，改一处就该两处一起改。

use std::path::{Path, PathBuf};

/// 单技能正文上限。
const MAX_SKILL_BYTES: usize = 256 * 1024;

pub fn skills_dir(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("skills")
}

pub fn registry_path(data_dir: &str) -> PathBuf {
    skills_dir(data_dir).join("registry.json")
}

pub fn load_registry(data_dir: &str) -> Vec<serde_json::Value> {
    std::fs::read_to_string(registry_path(data_dir))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

pub fn save_registry(data_dir: &str, entries: &[serde_json::Value]) -> Result<(), String> {
    let path = registry_path(data_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    let json = serde_json::to_string(entries).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("write registry.json: {e}"))
}

/// 解析 SKILL.md：frontmatter（name/description/command）+ 正文。
/// `command: <slug>` 可选——声明后技能可作为自定义指令 `/slug` 调用
/// （pi TUI 语义：/commit-it 这类命令式技能）。
pub fn sanitize_id(s: &str) -> String {
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

pub fn parse_skill_md(text: &str) -> Option<(String, String, Option<String>, String)> {
    let t = text.trim_start();
    let after_fence = t
        .strip_prefix("---\n")
        .or_else(|| t.strip_prefix("---\r\n"))?;
    let (front, body) = after_fence.split_once("---")?;
    let mut name = String::new();
    let mut description = String::new();
    let mut command: Option<String> = None;
    for line in front.lines() {
        if let Some((key, val)) = line.split_once(':') {
            match key.trim() {
                "name" => name = val.trim().to_string(),
                "description" => description = val.trim().to_string(),
                "command" => {
                    let slug = sanitize_id(val.trim());
                    if !slug.is_empty() {
                        command = Some(slug);
                    }
                }
                _ => {}
            }
        }
    }
    if name.is_empty() {
        return None;
    }
    Some((name, description, command, body.trim().to_string()))
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
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let Some((name, description, command, body)) = parse_skill_md(&text) else {
            continue;
        };
        let mut clipped = body;
        if clipped.len() > MAX_SKILL_BYTES {
            clipped.truncate(MAX_SKILL_BYTES);
        }
        if total + clipped.len() > TOTAL_BUDGET {
            clipped.truncate(TOTAL_BUDGET.saturating_sub(total));
        }
        total += clipped.len();
        let mut item = serde_json::json!({
            "id": id,
            "name": name,
            "description": description,
            "content": clipped,
        });
        // 自定义指令形态（如 /commit-it）：声明了 command 的技能对 bundle
        // 可命令寻址——展开逻辑在 bundle 侧（/slug args → 按 skill 执行）。
        if let Some(cmd) = command {
            item["command"] = serde_json::json!(cmd);
        }
        out.push(item);
        if total >= TOTAL_BUDGET {
            break;
        }
    }
    serde_json::json!({ "skills": out })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手工造 fixture（不依赖安装器 —— 安装器那半在 src-tauri，不属于本 crate）。
    /// 这也让测试直接盯住「registry 启用位 + SKILL.md → 注入内容」这条判据。
    fn fixture(tag: &str, entries: &[(&str, &str, bool)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pi-skills-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(skills_dir(dir.to_str().unwrap())).unwrap();
        let mut registry = Vec::new();
        for (id, md, enabled) in entries {
            let dir_path = skills_dir(dir.to_str().unwrap()).join(id);
            std::fs::create_dir_all(&dir_path).unwrap();
            std::fs::write(dir_path.join("SKILL.md"), md).unwrap();
            registry.push(serde_json::json!({ "id": id, "enabled": enabled }));
        }
        save_registry(dir.to_str().unwrap(), &registry).unwrap();
        dir
    }

    #[test]
    fn command_frontmatter_is_exposed_and_absent_when_not_declared() {
        let dir = fixture(
            "cmd",
            &[
                (
                    "commit-pro",
                    "---\nname: commit-pro\ndescription: Write good commits\ncommand: commit-it\n---\nBody",
                    true,
                ),
                (
                    "commit-helper",
                    "---\nname: commit-helper\ndescription: Write good commit messages\n---\nBody",
                    true,
                ),
            ],
        );
        let injected = enabled_for_injection(dir.to_str().unwrap());
        let skills = injected["skills"].as_array().unwrap();
        let with_cmd = skills.iter().find(|s| s["id"] == "commit-pro").unwrap();
        assert_eq!(with_cmd["command"], "commit-it");
        let without = skills.iter().find(|s| s["id"] == "commit-helper").unwrap();
        assert!(without.get("command").is_none(), "{without}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn injection_skips_disabled_and_missing_dirs() {
        let dir = fixture(
            "inj",
            &[
                (
                    "commit-helper",
                    "---\nname: commit-helper\ndescription: Write good commit messages\n---\nBody",
                    true,
                ),
                (
                    "disabled-one",
                    "---\nname: disabled-one\ndescription: off\n---\noff body",
                    false,
                ),
                // 登记了但目录不存在：静默跳过，不 panic
                (
                    "ghost",
                    "---\nname: ghost\ndescription: gone\n---\nbody",
                    true,
                ),
            ],
        );
        let _ = std::fs::remove_dir_all(skills_dir(dir.to_str().unwrap()).join("ghost"));
        let injected = enabled_for_injection(dir.to_str().unwrap());
        let skills = injected["skills"].as_array().unwrap();
        assert_eq!(skills.len(), 1, "{injected}");
        assert_eq!(skills[0]["id"], "commit-helper");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_skill_md_requires_name_and_reports_what_it_parsed() {
        assert!(parse_skill_md("no frontmatter at all").is_none());
        assert!(parse_skill_md("---\ndescription: no name\n---\nbody").is_none());
        let (name, description, command, body) =
            parse_skill_md("---\nname: Skill Name\ndescription: what it does\n---\n\nbody text\n")
                .unwrap();
        assert_eq!(name, "Skill Name");
        assert_eq!(description, "what it does");
        assert!(command.is_none());
        assert_eq!(body, "body text");
        // command slug 会被 sanitize（非字母数字→连字符，去首尾连字符，小写）
        let (_, _, command, _) =
            parse_skill_md("---\nname: X\ncommand: Commit-It!\n---\nb").unwrap();
        assert_eq!(command.as_deref(), Some("commit-it"));
    }

    #[test]
    fn sanitize_id_folds_to_slug() {
        assert_eq!(sanitize_id("Hello World!"), "hello-world");
        assert_eq!(sanitize_id("--x--"), "x");
        assert_eq!(sanitize_id("a/b\\c"), "a-b-c");
    }
}
