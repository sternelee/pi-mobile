//! creds —— provider 凭证存取（D4）。
//!
//! 桌面 / iOS：keyring crate（macOS Keychain / Windows Credential Manager /
//! Linux keyutils；iOS 走 apple-native，M5 上机）。Android：keyring v3 无
//! Keystore 后端，暂存 App 沙箱私有文件（0600）；迁 Android Keystore 需经
//! tauri 插件走 JNI，落 M3（PLAN 风险表已记录）。
//!
//! 安全边界：JS 侧永不落盘明文 —— API key 仅经 loopback `creds_get`
//! hostcall 注入嵌入式运行时内存（agent-main.js getApiKey）。

#[cfg(not(target_os = "android"))]
mod imp {
    const SERVICE: &str = "pi-mobile";
    use std::path::Path;

    fn entry(provider: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, provider).map_err(|e| format!("keyring entry: {e}"))
    }

    pub fn get(_data_dir: &str, provider: &str) -> Option<String> {
        let e = entry(provider).ok()?;
        match e.get_password() {
            Ok(k) if !k.is_empty() => Some(k),
            _ => None,
        }
    }

    pub fn set(data_dir: &str, provider: &str, api_key: &str) -> Result<(), String> {
        entry(provider)?
            .set_password(api_key)
            .map_err(|e| format!("keyring set: {e}"))?;
        // M2 早期文件态凭证迁移：入库后从 creds.json 摘除该 provider
        remove_legacy(data_dir, provider);
        Ok(())
    }

    fn remove_legacy(data_dir: &str, provider: &str) {
        let path = Path::new(data_dir).join("creds.json");
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return;
        };
        let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return;
        };
        let Some(obj) = v.as_object_mut() else {
            return;
        };
        if obj.remove(provider).is_some() {
            if obj.is_empty() {
                let _ = std::fs::remove_file(&path);
            } else {
                let _ = std::fs::write(&path, v.to_string());
            }
        }
    }
}

#[cfg(target_os = "android")]
mod imp {
    use std::path::Path;

    fn creds_path(data_dir: &str) -> std::path::PathBuf {
        Path::new(data_dir).join("creds.json")
    }

    pub fn get(data_dir: &str, provider: &str) -> Option<String> {
        let raw = std::fs::read_to_string(creds_path(data_dir)).ok()?;
        let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
        v.get(provider)?
            .as_str()
            .filter(|s| !s.is_empty())
            .map(String::from)
    }

    pub fn set(data_dir: &str, provider: &str, api_key: &str) -> Result<(), String> {
        let path = creds_path(data_dir);
        let mut v: serde_json::Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::json!({}));
        v[provider] = serde_json::json!(api_key);
        std::fs::write(&path, v.to_string()).map_err(|e| format!("write creds: {e}"))?;
        // App 沙箱私有目录仍收紧到 owner-only（过渡态，M3 迁 Keystore 加密）
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

pub fn get(data_dir: &str, provider: &str) -> Option<String> {
    imp::get(data_dir, provider)
}

pub fn set(data_dir: &str, provider: &str, api_key: &str) -> Result<(), String> {
    imp::set(data_dir, provider, api_key)
}

/// OAuth 凭证（pi-ai Credential JSON，含 refresh/access token）——与 api key
/// 同库隔离存储（条目名 `{provider}#oauth`），值是 JSON 字符串。
pub fn get_json(data_dir: &str, provider: &str) -> Option<String> {
    imp::get(data_dir, &format!("{provider}#oauth"))
}

pub fn set_json(data_dir: &str, provider: &str, json: &str) -> Result<(), String> {
    serde_json::from_str::<serde_json::Value>(json).map_err(|e| format!("invalid credential json: {e}"))?;
    imp::set(data_dir, &format!("{provider}#oauth"), json)
}

pub fn delete_json(data_dir: &str, provider: &str) -> Result<(), String> {
    imp::set(data_dir, &format!("{provider}#oauth"), "")
}
