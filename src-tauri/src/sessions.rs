//! sessions —— 会话索引（D3/D7）：扫描 sessions 根目录，解析 pi-v4 header。
//!
//! 目录结构（pi JsonlSessionRepo 布局）：`<root>/<cwd-encoded>/<ts>_<id>.jsonl`。
//! UI 会话列表按 modifiedAt 倒序展示；切会话经 bundle 的 __pi_open_session。

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::UNIX_EPOCH;

/// 从 message entry 提取纯文本（pi-ai content：字符串或 [{type:"text",text}] 数组）。
fn message_text(content: &serde_json::Value) -> Option<String> {
    match content {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(items) => {
            let joined = items
                .iter()
                .filter(|c| c.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("");
            if joined.is_empty() {
                None
            } else {
                Some(joined)
            }
        }
        _ => None,
    }
}

/// 标题截断（UTF-8 字符边界安全；含省略号）。
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// 列出全部会话元数据（JSON 数组，modifiedAt 倒序）。
/// lastMessage：最后一条 user/assistant 消息文本（截断 120 字符）——会话列表标题。
/// 根目录不存在时返回空数组（iOS 首启：agent 运行时门控下无人建目录）。
pub fn list(sessions_root: &str) -> Result<String, String> {
    let root = Path::new(sessions_root);
    let rd = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok("[]".into());
        }
        Err(e) => return Err(format!("sessions: {e}")),
    };
    let mut out = Vec::new();
    for dir in rd.flatten() {
        if !dir.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(dir.path()) else {
            continue;
        };
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".jsonl") {
                continue;
            }
            let Ok(meta) = f.metadata() else { continue };
            let modified_at = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let Ok(file) = std::fs::File::open(f.path()) else {
                continue;
            };
            let mut reader = BufReader::new(file);
            let mut header_line = String::new();
            if reader.read_line(&mut header_line).unwrap_or(0) == 0 {
                continue;
            }
            let Ok(header) = serde_json::from_str::<serde_json::Value>(&header_line) else {
                continue;
            };
            if header.get("kind").and_then(|k| k.as_str()) != Some("header") {
                continue;
            }
            let mut entries = 0u32;
            let mut last_message = serde_json::Value::Null;
            for line in reader.lines().map_while(Result::ok) {
                if !line.contains("\"type\":\"message\"") {
                    continue;
                }
                entries += 1;
                // 扁平 entry（codec encodeMutation）：{"kind":"entry",...,"type":"message",
                // "message":{"role":...,"content":...}}。取最后一条 user/assistant 文本。
                let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                let msg = match entry.get("message") {
                    Some(m) => m,
                    None => continue,
                };
                let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
                if role != "user" && role != "assistant" {
                    continue;
                }
                if let Some(text) = msg.get("content").and_then(message_text) {
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    last_message = serde_json::json!(truncate_chars(trimmed, 120));
                }
            }
            out.push(serde_json::json!({
                "id": header.get("id").cloned().unwrap_or(serde_json::Value::Null),
                "createdAt": header.get("createdAt").cloned().unwrap_or(serde_json::Value::Null),
                "cwd": header.get("cwd").cloned().unwrap_or(serde_json::Value::Null),
                "modifiedAt": modified_at,
                "entries": entries,
                "size": meta.len(),
                "lastMessage": last_message,
            }));
        }
    }
    out.sort_by(|a, b| {
        b["modifiedAt"]
            .as_u64()
            .unwrap_or(0)
            .cmp(&a["modifiedAt"].as_u64().unwrap_or(0))
    });
    serde_json::to_string(&out).map_err(|e| format!("serialize: {e}"))
}

/// 删除指定会话（按 header id 匹配 JSONL 并移除；父目录空了顺手清掉）。
/// id 只允许非空且不含路径分隔符——它是 list() 回给 UI 的会话 id，
/// 校验防止把删除当任意路径删除原语用。
pub fn delete(sessions_root: &str, id: &str) -> Result<(), String> {
    if id.is_empty() || id.contains('/') || id.contains('\\') || id.contains("..") {
        return Err("sessions: invalid session id".into());
    }
    let root = Path::new(sessions_root);
    let rd = std::fs::read_dir(root).map_err(|e| format!("sessions: {e}"))?;
    for dir in rd.flatten() {
        if !dir.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(dir.path()) else {
            continue;
        };
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".jsonl") {
                continue;
            }
            let Ok(file) = std::fs::File::open(f.path()) else {
                continue;
            };
            let mut header_line = String::new();
            if std::io::BufRead::read_line(&mut std::io::BufReader::new(file), &mut header_line)
                .unwrap_or(0)
                == 0
            {
                continue;
            }
            let Ok(header) = serde_json::from_str::<serde_json::Value>(&header_line) else {
                continue;
            };
            if header.get("kind").and_then(|k| k.as_str()) != Some("header") {
                continue;
            }
            if header.get("id").and_then(|v| v.as_str()) != Some(id) {
                continue;
            }
            std::fs::remove_file(f.path()).map_err(|e| format!("delete {name}: {e}"))?;
            // 父目录空则移除（保持 workspace 编码目录整洁）
            if let Ok(mut left) = std::fs::read_dir(dir.path()) {
                if left.next().is_none() {
                    let _ = std::fs::remove_dir(dir.path());
                }
            }
            return Ok(());
        }
    }
    Err(format!("sessions: no such session: {id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_root_returns_empty() {
        let dir = std::env::temp_dir().join(format!("pi-sessions-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(list(dir.to_str().unwrap()).unwrap(), "[]");
    }

    #[test]
    fn lists_sessions_sorted_by_modified_desc() {
        let dir = std::env::temp_dir().join(format!("pi-sessions-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sub = dir.join("--data-workspace--");
        std::fs::create_dir_all(&sub).unwrap();

        let write_session = |name: &str, id: &str, content: &str, age_millis: u64| {
            let path = sub.join(name);
            std::fs::write(&path, content).unwrap();
            let past = std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64
                - age_millis as i64;
            let t = std::time::UNIX_EPOCH + std::time::Duration::from_millis(past as u64);
            filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(t)).unwrap();
        };

        let mk = |id: &str| {
            format!(
                "{{\"kind\":\"header\",\"version\":4,\"id\":\"{id}\",\"createdAt\":1,\"cwd\":\"/w\"}}\n{{\"kind\":\"entry\",\"type\":\"message\"}}\n"
            )
        };
        write_session("100_old.jsonl", "id-old", &mk("id-old"), 60_000);
        write_session("200_new.jsonl", "id-new", &mk("id-new"), 0);

        let list = list(dir.to_str().unwrap()).unwrap();
        let v: Vec<serde_json::Value> = serde_json::from_str(&list).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0]["id"], "id-new"); // modified 倒序
        assert_eq!(v[0]["entries"], 1);
        assert_eq!(v[1]["cwd"], "/w");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn last_message_extracted_as_title() {
        let dir = std::env::temp_dir().join(format!("pi-sessions-title-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sub = dir.join("--data-workspace--");
        std::fs::create_dir_all(&sub).unwrap();

        let long: String = "很".repeat(200);
        let content = concat!(
            "{\"kind\":\"header\",\"version\":4,\"id\":\"s-title\",\"createdAt\":1,\"cwd\":\"/w\"}\n",
            "{\"kind\":\"entry\",\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"first question\"}]}}\n",
            "{\"kind\":\"entry\",\"type\":\"message\",\"message\":{\"role\":\"toolResult\",\"content\":[{\"type\":\"text\",\"text\":\"tool output\"}]}}\n",
            "{\"kind\":\"entry\",\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"content\":\"final answer\"}}\n",
        );
        std::fs::write(sub.join("300_t.jsonl"), content).unwrap();

        let content2 = format!(
            "{{\"kind\":\"header\",\"version\":4,\"id\":\"s-long\",\"createdAt\":1,\"cwd\":\"/w\"}}\n{{\"kind\":\"entry\",\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"{long}\"}}]}}}}\n"
        );
        std::fs::write(sub.join("400_l.jsonl"), content2).unwrap();

        let v: Vec<serde_json::Value> = serde_json::from_str(&list(dir.to_str().unwrap()).unwrap())
            .unwrap();
        // modifiedAt 相同排序不稳定——按 id 找
        let title = v.iter().find(|s| s["id"] == "s-title").unwrap();
        assert_eq!(title["entries"], 3);
        assert_eq!(title["lastMessage"], "final answer"); // 最后一条 user/assistant（跳过 toolResult）
        let long = v.iter().find(|s| s["id"] == "s-long").unwrap();
        let lm = long["lastMessage"].as_str().unwrap();
        assert_eq!(lm.chars().count(), 121); // 120 字符 + 省略号
        assert!(lm.ends_with('…'));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_removes_file_and_empty_dir() {
        let dir = std::env::temp_dir().join(format!("pi-sessions-del-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sub = dir.join("--data-workspace--");
        std::fs::create_dir_all(&sub).unwrap();
        let mk = |id: &str| {
            format!(
                "{{\"kind\":\"header\",\"version\":4,\"id\":\"{id}\",\"createdAt\":1,\"cwd\":\"/w\"}}\n{{\"kind\":\"entry\",\"type\":\"message\"}}\n"
            )
        };
        std::fs::write(sub.join("500_a.jsonl"), mk("del-me")).unwrap();
        std::fs::write(sub.join("600_b.jsonl"), mk("keep-me")).unwrap();

        // 路径注入拒绝
        assert!(delete(dir.to_str().unwrap(), "../x").is_err());
        assert!(delete(dir.to_str().unwrap(), "a/b").is_err());
        // 不存在的 id
        assert!(delete(dir.to_str().unwrap(), "nope").is_err());
        // 命中删除（目录非空 → 保留）
        delete(dir.to_str().unwrap(), "del-me").unwrap();
        assert!(sub.join("500_a.jsonl").exists() == false);
        assert!(sub.join("600_b.jsonl").exists());
        // 最后一个删掉 → 空父目录一并清理
        delete(dir.to_str().unwrap(), "keep-me").unwrap();
        assert!(!sub.exists());
        assert!(delete(dir.to_str().unwrap(), "keep-me").is_err()); // 幂等误用报错

        let _ = std::fs::remove_dir_all(&dir);
    }
}
