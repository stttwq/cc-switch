use crate::config::get_app_config_dir;
use crate::error::AppError;
use serde::Serialize;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaintextBackupInfo {
    pub path: String,
    pub kind: String,
}

pub fn cleanup_auto_deletable_plaintext() {
    let dir = get_app_config_dir();
    for name in [
        "config.json",
        "config.json.bak",
        "config.json.migrated",
        "codex_oauth_auth.json",
        "copilot_auth.json",
        "xai_oauth_auth.json",
    ] {
        let path = dir.join(name);
        if path.exists() {
            match fs::remove_file(&path) {
                Ok(()) => log::info!("已删除明文残留 {}", path.display()),
                Err(e) => log::warn!("删除 {} 失败: {e}", path.display()),
            }
        }
    }

    let backups = dir.join("backups");
    if backups.is_dir() {
        if let Ok(entries) = fs::read_dir(&backups) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("env-backup-") && name.ends_with(".json") {
                    let path = entry.path();
                    if let Err(e) = fs::remove_file(&path) {
                        log::warn!("删除 {} 失败: {e}", path.display());
                    }
                }
            }
        }
    }

    let temp = std::env::temp_dir();
    if let Ok(entries) = fs::read_dir(&temp) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // §6.3：旧终端启动器写的 `%TEMP%/claude_<id>_<pid>.json`，按文件名模式
            // 匹配 **且** 内容确实解析为含 `env` 的 JSON 对象才删，避免误伤同名文件。
            if !is_legacy_terminal_settings_name(&name) {
                continue;
            }
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let is_legacy = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.get("env").map(|e| e.is_object()))
                .unwrap_or(false);
            if is_legacy {
                if let Err(e) = fs::remove_file(&path) {
                    log::warn!("删除 {} 失败: {e}", path.display());
                }
            }
        }
    }
}

/// `claude_<id>_<pid>.json`：前缀后必须是两段非空、以 `_` 分隔的名称，最后 `.json`。
fn is_legacy_terminal_settings_name(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix("claude_")
        .and_then(|s| s.strip_suffix(".json"))
    else {
        return false;
    };
    let mut parts = rest.split('_');
    let Some(first) = parts.next() else {
        return false;
    };
    let Some(second) = parts.next() else {
        return false;
    };
    parts.next().is_none() && !first.is_empty() && !second.is_empty()
}

pub fn list_plaintext_db_backups() -> Result<Vec<PlaintextBackupInfo>, AppError> {
    let mut out = Vec::new();
    let backups = get_app_config_dir().join("backups");
    if !backups.is_dir() {
        return Ok(out);
    }
    let entries = fs::read_dir(&backups).map_err(|e| AppError::io(&backups, e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.ends_with(".db") {
            out.push(PlaintextBackupInfo {
                kind: if name.starts_with("pre-secrets-migration") {
                    "pre-secrets-migration".to_string()
                } else {
                    "db-backup".to_string()
                },
                path: path.to_string_lossy().to_string(),
            });
        }
    }
    Ok(out)
}

pub fn delete_plaintext_db_backups() -> Result<usize, AppError> {
    let listed = list_plaintext_db_backups()?;
    let mut n = 0usize;
    for item in listed {
        let path = PathBuf::from(&item.path);
        fs::remove_file(&path).map_err(|e| AppError::io(&path, e))?;
        n += 1;
    }
    Ok(n)
}
