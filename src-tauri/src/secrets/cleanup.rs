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

    cleanup_legacy_terminal_settings(&std::env::temp_dir());
}

/// 扫描一个临时目录，删除旧终端启动器留下的 `claude_<id>_<pid>.json`。
///
/// 单独成函数是为了可测：测试直接传入隔离目录，不去改全局 `TMP`/`TEMP`
/// （改全局环境变量会串到同进程并行的其他测试，让它们的 `tempdir()` 失效）。
fn cleanup_legacy_terminal_settings(temp: &std::path::Path) {
    let Ok(entries) = fs::read_dir(temp) else {
        return;
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// 隔离 home：只改 `CC_SWITCH_TEST_HOME`，**不碰** `TMP`/`TEMP`
    /// （改全局临时目录会串到同进程并行的其他测试）。
    struct TempHome {
        dir: PathBuf,
        prev: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl TempHome {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("cc-switch-cleanup-{tag}"));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(dir.join(".cc-switch").join("backups")).expect("create config dir");
            fs::create_dir_all(dir.join("tmp")).expect("create scratch dir");
            // `get_app_config_dir()` 在默认位置没有 cc-switch.db 时会回退到 `$HOME/.cc-switch`
            // （v3.10.3 兼容分支）。占位文件让解析结果稳定停在本测试的临时 home，
            // 否则测试会去删用户真实的备份目录。
            fs::write(dir.join(".cc-switch").join("cc-switch.db"), b"").expect("placeholder db");

            let vars = ["CC_SWITCH_TEST_HOME"];
            let prev: Vec<(&'static str, Option<std::ffi::OsString>)> =
                vars.iter().map(|k| (*k, std::env::var_os(k))).collect();
            std::env::set_var("CC_SWITCH_TEST_HOME", &dir);

            let resolved = get_app_config_dir();
            assert!(
                resolved.starts_with(&dir),
                "测试 home 未生效，解析到 {}；立即中止以免动到真实目录",
                resolved.display()
            );

            Self { dir, prev }
        }

        fn config_dir(&self) -> PathBuf {
            self.dir.join(".cc-switch")
        }

        fn scratch(&self) -> PathBuf {
            self.dir.join("tmp")
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            for (key, value) in self.prev.iter() {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    const AUTO_DELETABLE: &[&str] = &[
        "config.json",
        "config.json.bak",
        "config.json.migrated",
        "codex_oauth_auth.json",
        "copilot_auth.json",
        "xai_oauth_auth.json",
    ];

    #[test]
    #[serial]
    fn cleanup_removes_auto_deletable_plaintext_and_keeps_the_rest() {
        let home = TempHome::new("auto");
        let cfg = home.config_dir();
        for name in AUTO_DELETABLE {
            fs::write(cfg.join(name), "{}").expect("write plaintext residue");
        }
        fs::write(cfg.join("backups").join("env-backup-1.json"), "{}").unwrap();
        fs::write(cfg.join("backups").join("keep-me.json"), "{}").unwrap();
        // §6.3：DB 备份是唯一回滚路径，只能由用户显式删除，不得自动清掉。
        fs::write(cfg.join("backups").join("pre-secrets-migration-1.db"), "x").unwrap();
        fs::write(cfg.join("settings.json"), "{}").unwrap();

        cleanup_auto_deletable_plaintext();

        for name in AUTO_DELETABLE {
            assert!(!cfg.join(name).exists(), "{name} 应被自动删除");
        }
        assert!(!cfg.join("backups").join("env-backup-1.json").exists());
        assert!(
            cfg.join("backups").join("keep-me.json").exists(),
            "非 env-backup 前缀的备份不得被删"
        );
        assert!(
            cfg.join("backups")
                .join("pre-secrets-migration-1.db")
                .exists(),
            "DB 备份是唯一回滚路径，不得自动删除"
        );
        assert!(
            cfg.join("settings.json").exists(),
            "settings.json 不是明文残留"
        );
    }

    #[test]
    #[serial]
    fn cleanup_removes_legacy_terminal_settings_by_name_and_content() {
        let home = TempHome::new("temp");
        let scratch = home.scratch();
        fs::write(
            scratch.join("claude_abc_1234.json"),
            r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-old"}}"#,
        )
        .unwrap();
        // 同名模式但内容解析不出含 env 的对象 → 保留，避免误伤
        fs::write(scratch.join("claude_def_5678.json"), "not json").unwrap();
        // 段数不符 → 保留
        fs::write(scratch.join("claude_settings.json"), r#"{"env":{}}"#).unwrap();

        cleanup_legacy_terminal_settings(&scratch);

        assert!(!scratch.join("claude_abc_1234.json").exists());
        assert!(scratch.join("claude_def_5678.json").exists());
        assert!(scratch.join("claude_settings.json").exists());
    }

    #[test]
    #[serial]
    fn list_and_delete_plaintext_db_backups_covers_both_kinds() {
        let home = TempHome::new("db");
        let backups = home.config_dir().join("backups");
        fs::write(backups.join("cc-switch-2026.db"), "x").unwrap();
        fs::write(backups.join("pre-secrets-migration-2.db"), "x").unwrap();
        fs::write(backups.join("notes.txt"), "x").unwrap();

        let listed = list_plaintext_db_backups().expect("list");
        assert_eq!(listed.len(), 2);
        assert!(listed
            .iter()
            .any(|item| item.kind == "pre-secrets-migration"));
        assert_eq!(delete_plaintext_db_backups().expect("delete"), 2);
        assert!(backups.join("notes.txt").exists());
    }
}
