//! sessions —— 会话索引（D3/D7）：扫描 sessions 根目录，解析 pi-v4 header。
//!
//! 目录结构（pi JsonlSessionRepo 布局）：`<root>/<cwd-encoded>/<ts>_<id>.jsonl`。
//! UI 会话列表按 modifiedAt 倒序展示；切会话经 bundle 的 __pi_open_session。

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::UNIX_EPOCH;

/// 列出全部会话元数据（JSON 数组，modifiedAt 倒序）。
pub fn list(sessions_root: &str) -> Result<String, String> {
    let root = Path::new(sessions_root);
    let rd = std::fs::read_dir(root).map_err(|e| format!("sessions: {e}"))?;
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
            for line in reader.lines().map_while(Result::ok) {
                if line.contains("\"type\":\"message\"") {
                    entries += 1;
                }
            }
            out.push(serde_json::json!({
                "id": header.get("id").cloned().unwrap_or(serde_json::Value::Null),
                "createdAt": header.get("createdAt").cloned().unwrap_or(serde_json::Value::Null),
                "cwd": header.get("cwd").cloned().unwrap_or(serde_json::Value::Null),
                "modifiedAt": modified_at,
                "entries": entries,
                "size": meta.len(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
