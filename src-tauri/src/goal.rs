//! goal —— 持久目标（pi-goal 移动原生化）。存 `{data_dir}/goal.json`：
//! `{"objective":"..."}`。bundle boot 时经 `goal_get` hostcall 读出并注入
//! systemPrompt（"Current goal" 节）；设置/清除经 Tauri 命令 + bundle
//! `__pi_goal_apply` 热生效。

use std::path::Path;

fn config_path(data_dir: &str) -> std::path::PathBuf {
    Path::new(data_dir).join("goal.json")
}

fn load(data_dir: &str) -> serde_json::Value {
    std::fs::read_to_string(config_path(data_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({ "objective": null }))
}

fn save(data_dir: &str, v: &serde_json::Value) -> Result<(), String> {
    let json = serde_json::to_string(v).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(config_path(data_dir), json).map_err(|e| format!("write goal.json: {e}"))
}

pub fn get(data_dir: &str) -> Result<String, String> {
    let v = load(data_dir);
    serde_json::to_string(&v["objective"]).map_err(|e| format!("serialize: {e}"))
}

pub fn set(data_dir: &str, objective: &str) -> Result<(), String> {
    if objective.trim().is_empty() {
        return Err("objective must not be empty".into());
    }
    save(data_dir, &serde_json::json!({ "objective": objective.trim() }))
}

pub fn clear(data_dir: &str) -> Result<(), String> {
    save(data_dir, &serde_json::json!({ "objective": null }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn goal_set_get_clear_roundtrip() {
        let dir = std::env::temp_dir().join(format!("pi-goal-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir_s = dir.to_str().unwrap();

        assert_eq!(super::get(dir_s).unwrap(), "null");
        assert!(super::set(dir_s, "  ").is_err()); // 空目标拒绝
        super::set(dir_s, "ship pi-mobile m4").unwrap();
        assert_eq!(super::get(dir_s).unwrap(), "\"ship pi-mobile m4\"");
        super::clear(dir_s).unwrap();
        assert_eq!(super::get(dir_s).unwrap(), "null");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
