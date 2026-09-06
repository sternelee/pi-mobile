//! mcp —— MCP 服务器配置管理（D11）。存 `{data_dir}/mcp.json`：
//! `{"servers":[{"name":"...","url":"https://..."}]}`。
//! 连接与工具发现由 bundle 内的最小 streamable-http 客户端完成（pi-mcp-adapter
//! 的移动原生化，见 agent-main.js）；此处只负责配置的增删查。

use std::path::Path;

fn config_path(data_dir: &str) -> std::path::PathBuf {
    Path::new(data_dir).join("mcp.json")
}

fn load(data_dir: &str) -> serde_json::Value {
    std::fs::read_to_string(config_path(data_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({ "servers": [] }))
}

fn save(data_dir: &str, v: &serde_json::Value) -> Result<(), String> {
    let json = serde_json::to_string(v).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(config_path(data_dir), json).map_err(|e| format!("write mcp.json: {e}"))
}

pub fn list(data_dir: &str) -> Result<String, String> {
    let v = load(data_dir);
    serde_json::to_string(&v["servers"]).map_err(|e| format!("serialize: {e}"))
}

pub fn add(data_dir: &str, name: &str, url: &str) -> Result<(), String> {
    if name.is_empty() || url.is_empty() {
        return Err("name and url are required".into());
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("url must be http(s)".into());
    }
    let mut v = load(data_dir);
    {
        let servers = v["servers"].as_array_mut().ok_or("corrupt mcp.json")?;
        if servers
            .iter()
            .any(|s| s["name"].as_str() == Some(name))
        {
            return Err(format!("server '{name}' already exists"));
        }
        servers.push(serde_json::json!({ "name": name, "url": url }));
    }
    save(data_dir, &v)
}

pub fn remove(data_dir: &str, name: &str) -> Result<(), String> {
    let mut v = load(data_dir);
    {
        let servers = v["servers"].as_array_mut().ok_or("corrupt mcp.json")?;
        let before = servers.len();
        servers.retain(|s| s["name"].as_str() != Some(name));
        if servers.len() == before {
            return Err(format!("no server named '{name}'"));
        }
    }
    save(data_dir, &v)
}

#[cfg(test)]
mod tests {
    #[test]
    fn add_list_remove_roundtrip() {
        let dir = std::env::temp_dir().join(format!("pi-mcp-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir_s = dir.to_str().unwrap();

        assert!(super::list(dir_s).unwrap().contains("[]"));
        super::add(dir_s, "demo", "https://mcp.example.com/mcp").unwrap();
        assert!(super::add(dir_s, "demo", "https://x").is_err()); // 重名
        assert!(super::add(dir_s, "bad", "ftp://x").is_err()); // 非 http(s)
        let list = super::list(dir_s).unwrap();
        assert!(list.contains("demo") && list.contains("mcp.example.com"));
        super::remove(dir_s, "demo").unwrap();
        assert!(super::remove(dir_s, "demo").is_err()); // 不存在
        assert!(super::list(dir_s).unwrap().contains("[]"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
