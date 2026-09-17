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
            if name.starts_with("claude_") && name.ends_with(".json") {
                let path = entry.path();
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                if text.contains("\"env\"") {
                    let _ = fs::remove_file(&path);
                }
            }
        }
    }
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
