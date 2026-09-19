use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;

/// Environment variable conflict information returned to frontend
/// Phase 5 S11: Only the masked value (last 4 chars) crosses IPC; the full
/// value never leaves the backend, and deletion works by variable name.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvConflict {
    pub var_name: String,
    /// Masked value showing only last 4 characters (e.g., "***xyz")
    pub masked_value: String,
    pub source_type: String, // "system" | "file" | "claude_settings_local"
    pub source_path: String, // Registry path or file path
    /// §5.3.1 注：只读告警（例如 `~/.claude/settings.local.json`）。cc-switch 不代改
    /// 这类文件，前端只展示、不提供删除。
    #[serde(default)]
    pub read_only: bool,
}

/// Internal struct for processing - contains full value before masking
#[derive(Debug, Clone)]
struct EnvConflictInternal {
    var_name: String,
    var_value: String,
    source_type: String,
    source_path: String,
}

impl EnvConflictInternal {
    /// Convert to frontend-safe struct with masked value
    fn into_public(self) -> EnvConflict {
        let masked_value = mask_secret(&self.var_value);
        EnvConflict {
            var_name: self.var_name,
            masked_value,
            source_type: self.source_type,
            source_path: self.source_path,
            read_only: false,
        }
    }
}

/// Mask a secret value by showing only the last 4 characters
fn mask_secret(value: &str) -> String {
    if value.chars().count() <= 4 {
        "***".to_string()
    } else {
        format!("***{}", crate::secrets::last_chars(value, 4))
    }
}

#[cfg(target_os = "windows")]
use winreg::enums::*;
#[cfg(target_os = "windows")]
use winreg::RegKey;

/// Check environment variables for conflicts
pub fn check_env_conflicts(app: &str) -> Result<Vec<EnvConflict>, String> {
    let keywords = get_keywords_for_app(app);
    let mut conflicts = Vec::new();
    let managed = load_managed_names();

    // Check system environment variables
    conflicts.extend(check_system_env(&keywords, &managed)?);

    // Check shell configuration files (Unix only)
    #[cfg(not(target_os = "windows"))]
    conflicts.extend(check_shell_configs(&keywords)?);

    // §5.3.1 注：Claude 自己的 settings.local.json 会覆盖我们投递的进程环境变量。
    // cc-switch 不代改该文件，只给出只读告警。
    if app.eq_ignore_ascii_case("claude") {
        conflicts.extend(check_claude_settings_local(&keywords));
    }

    Ok(conflicts)
}

/// §5.3.1 注：扫描 `~/.claude/settings.local.json` 的 `env` 块。
///
/// 那里的 `env.ANTHROPIC_*` 优先级高于进程环境变量，会盖掉 cc-switch 通过
/// `HKCU\Environment` 投递的凭据。我们不改用户的这个文件，只如实报告。
fn check_claude_settings_local(keywords: &[EnvKeyword]) -> Vec<EnvConflict> {
    let path = crate::config::get_home_dir()
        .join(".claude")
        .join("settings.local.json");
    let Ok(text) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(env) = root.get("env").and_then(|value| value.as_object()) else {
        return Vec::new();
    };
    env.iter()
        .filter(|(name, _)| matches_env_keyword(name, keywords))
        .filter_map(|(name, value)| {
            Some(EnvConflict {
                var_name: name.clone(),
                masked_value: mask_secret(value.as_str()?),
                source_type: "claude_settings_local".to_string(),
                source_path: path.to_string_lossy().to_string(),
                read_only: true,
            })
        })
        .collect()
}

/// Delete the listed conflicting environment variables (no plaintext backup is
/// written anywhere — see施工計画 §5.3.3; originals are not retained).
pub fn delete_env_vars(
    sink: &dyn crate::env_delivery::EnvSink,
    conflicts: Vec<EnvConflict>,
) -> Result<usize, String> {
    let mut deleted = 0usize;
    for conflict in &conflicts {
        delete_single_env(conflict)?;
        deleted += 1;
    }
    if deleted > 0 {
        let _ = sink.broadcast();
    }
    Ok(deleted)
}

#[cfg(target_os = "windows")]
fn delete_single_env(conflict: &EnvConflict) -> Result<(), String> {
    match conflict.source_type.as_str() {
        "system" => {
            if conflict.source_path.contains("HKEY_CURRENT_USER") {
                let hkcu = RegKey::predef(HKEY_CURRENT_USER)
                    .open_subkey_with_flags("Environment", KEY_SET_VALUE)
                    .map_err(|e| format!("打开注册表失败: {e}"))?;

                hkcu.delete_value(&conflict.var_name)
                    .map_err(|e| format!("删除注册表项失败: {e}"))?;
                std::env::remove_var(&conflict.var_name);
            } else if conflict.source_path.contains("HKEY_LOCAL_MACHINE") {
                // D11: cc-switch 永不写 HKLM。系统级同名变量只读报告，
                // 用户级会覆盖它（PATH 除外），删除需用户自行在系统设置中操作。
                return Err("系统级 (HKLM) 环境变量不由 cc-switch 管理".to_string());
            }
            Ok(())
        }
        "file" => Err("Windows 系统不应该有文件类型的环境变量".to_string()),
        _ => Err(format!("未知的环境变量来源类型: {}", conflict.source_type)),
    }
}

#[cfg(not(target_os = "windows"))]
fn delete_single_env(conflict: &EnvConflict) -> Result<(), String> {
    match conflict.source_type.as_str() {
        "file" => {
            // source_path 格式: "path:line"
            let file_path = conflict.source_path.split(':').next().unwrap_or("");
            if file_path.is_empty() {
                return Err("无效的文件路径格式".to_string());
            }

            let content = fs::read_to_string(file_path)
                .map_err(|e| format!("读取文件失败 {file_path}: {e}"))?;

            let new_content: Vec<String> = content
                .lines()
                .filter(|line| {
                    let trimmed = line.trim();
                    let export_line = trimmed.strip_prefix("export ").unwrap_or(trimmed);

                    if let Some(eq_pos) = export_line.find('=') {
                        let var_name = export_line[..eq_pos].trim();
                        var_name != conflict.var_name
                    } else {
                        true
                    }
                })
                .map(|s| s.to_string())
                .collect();

            fs::write(file_path, new_content.join("\n"))
                .map_err(|e| format!("写入文件失败 {file_path}: {e}"))?;

            std::env::remove_var(&conflict.var_name);
            Ok(())
        }
        "system" => {
            std::env::remove_var(&conflict.var_name);
            Ok(())
        }
        _ => Err(format!("未知的环境变量来源类型: {}", conflict.source_type)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnvKeyword {
    Prefix(&'static str),
}

/// Get relevant keywords for each app
fn get_keywords_for_app(app: &str) -> Vec<EnvKeyword> {
    match app.to_lowercase().as_str() {
        "claude" => vec![EnvKeyword::Prefix("ANTHROPIC")],
        "codex" => vec![EnvKeyword::Prefix("OPENAI")],
        _ => vec![],
    }
}

fn matches_env_keyword(name: &str, keywords: &[EnvKeyword]) -> bool {
    let upper_name = name.to_uppercase();
    keywords
        .iter()
        .any(|keyword| upper_name.starts_with(keyword.prefix()))
}

impl EnvKeyword {
    fn prefix(&self) -> &'static str {
        match self {
            EnvKeyword::Prefix(p) => p,
        }
    }
}

fn load_managed_names() -> HashSet<String> {
    crate::env_delivery::ManagedEnvVars::load_from_disk()
        .map(|m| m.entries.keys().cloned().collect())
        .unwrap_or_default()
}

/// Check system environment variables (Windows Registry or Unix env)
#[cfg(target_os = "windows")]
fn check_system_env(
    keywords: &[EnvKeyword],
    managed: &HashSet<String>,
) -> Result<Vec<EnvConflict>, String> {
    let mut conflicts = Vec::new();

    // Check HKEY_CURRENT_USER\Environment
    if let Ok(hkcu) = RegKey::predef(HKEY_CURRENT_USER).open_subkey("Environment") {
        for (name, value) in hkcu.enum_values().filter_map(Result::ok) {
            if managed.contains(&name) {
                continue;
            }
            if matches_env_keyword(&name, keywords) {
                conflicts.push(
                    EnvConflictInternal {
                        var_name: name.clone(),
                        var_value: value.to_string(),
                        source_type: "system".to_string(),
                        source_path: "HKEY_CURRENT_USER\\Environment".to_string(),
                    }
                    .into_public(),
                );
            }
        }
    }

    // Check HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Control\Session Manager\Environment
    if let Ok(hklm) = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey("SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment")
    {
        for (name, value) in hklm.enum_values().filter_map(Result::ok) {
            if matches_env_keyword(&name, keywords) {
                conflicts.push(EnvConflictInternal {
                    var_name: name.clone(),
                    var_value: value.to_string(),
                    source_type: "system".to_string(),
                    source_path: "HKEY_LOCAL_MACHINE\\SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment".to_string(),
                }.into_public());
            }
        }
    }

    Ok(conflicts)
}

#[cfg(not(target_os = "windows"))]
fn check_system_env(
    keywords: &[EnvKeyword],
    managed: &HashSet<String>,
) -> Result<Vec<EnvConflict>, String> {
    let mut conflicts = Vec::new();

    // Check current process environment
    for (key, value) in std::env::vars() {
        if managed.contains(&key) {
            continue;
        }
        if matches_env_keyword(&key, keywords) {
            conflicts.push(
                EnvConflictInternal {
                    var_name: key,
                    var_value: value,
                    source_type: "system".to_string(),
                    source_path: "Process Environment".to_string(),
                }
                .into_public(),
            );
        }
    }

    Ok(conflicts)
}

/// Check shell configuration files for environment variable exports (Unix only)
#[cfg(not(target_os = "windows"))]
fn check_shell_configs(keywords: &[EnvKeyword]) -> Result<Vec<EnvConflict>, String> {
    let mut conflicts = Vec::new();

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let config_files = vec![
        format!("{}/.bashrc", home),
        format!("{}/.bash_profile", home),
        format!("{}/.zshrc", home),
        format!("{}/.zprofile", home),
        format!("{}/.profile", home),
        "/etc/profile".to_string(),
        "/etc/bashrc".to_string(),
    ];

    for file_path in config_files {
        if let Ok(content) = fs::read_to_string(&file_path) {
            // Parse lines for export statements
            for (line_num, line) in content.lines().enumerate() {
                let trimmed = line.trim();

                // Match patterns like: export VAR=value or VAR=value
                if trimmed.starts_with("export ")
                    || (!trimmed.starts_with('#') && trimmed.contains('='))
                {
                    let export_line = trimmed.strip_prefix("export ").unwrap_or(trimmed);

                    if let Some(eq_pos) = export_line.find('=') {
                        let var_name = export_line[..eq_pos].trim();
                        let var_value = export_line[eq_pos + 1..].trim();

                        // Check if variable name contains any keyword
                        if matches_env_keyword(var_name, keywords) {
                            conflicts.push(
                                EnvConflictInternal {
                                    var_name: var_name.to_string(),
                                    var_value: var_value
                                        .trim_matches('"')
                                        .trim_matches('\'')
                                        .to_string(),
                                    source_type: "file".to_string(),
                                    source_path: format!("{}:{}", file_path, line_num + 1),
                                }
                                .into_public(),
                            );
                        }
                    }
                }
            }
        }
    }

    Ok(conflicts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_keywords() {
        assert_eq!(
            get_keywords_for_app("claude"),
            vec![EnvKeyword::Prefix("ANTHROPIC")]
        );
        assert_eq!(
            get_keywords_for_app("codex"),
            vec![EnvKeyword::Prefix("OPENAI")]
        );
        assert_eq!(get_keywords_for_app("gemini"), Vec::<EnvKeyword>::new());
        assert_eq!(get_keywords_for_app("grokbuild"), Vec::<EnvKeyword>::new());
        assert_eq!(get_keywords_for_app("unknown"), Vec::<EnvKeyword>::new());
    }

    #[test]
    fn broad_app_keywords_match_only_at_the_start() {
        let keywords = get_keywords_for_app("claude");

        assert!(matches_env_keyword("ANTHROPIC_API_KEY", &keywords));
        assert!(matches_env_keyword("anthropic_base_url", &keywords));
        assert!(!matches_env_keyword("MY_ANTHROPIC_API_KEY", &keywords));
        assert!(!matches_env_keyword("NOT_ANTHROPIC", &keywords));
    }

    #[test]
    #[serial_test::serial]
    fn claude_settings_local_env_is_reported_read_only() {
        // §5.3.1 注：~/.claude/settings.local.json 的 env.ANTHROPIC_* 会覆盖
        // 进程环境变量，必须只读上报（值只给末 4 位，且不可删除）。
        let home = std::env::temp_dir().join("cc-switch-env-checker-settings-local");
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(home.join(".claude")).expect("create .claude dir");
        fs::write(
            home.join(".claude").join("settings.local.json"),
            r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-local-secret-9999","OTHER":"x"}}"#,
        )
        .expect("write settings.local.json");
        std::env::set_var("CC_SWITCH_TEST_HOME", &home);

        let conflicts = check_claude_settings_local(&get_keywords_for_app("claude"));

        std::env::remove_var("CC_SWITCH_TEST_HOME");
        let _ = fs::remove_dir_all(&home);

        assert_eq!(conflicts.len(), 1, "只应命中 ANTHROPIC_* 前缀");
        assert_eq!(conflicts[0].var_name, "ANTHROPIC_AUTH_TOKEN");
        assert_eq!(conflicts[0].source_type, "claude_settings_local");
        assert!(conflicts[0].read_only);
        assert!(
            !conflicts[0].masked_value.contains("sk-local-secret"),
            "只读告警也不得回传明文"
        );
    }
}
