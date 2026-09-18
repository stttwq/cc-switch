//! Live configuration sanitization - ensure secrets never appear in live files
//!
//! Implements Phase 4 safety requirements from secrets-credential-manager-slimdown-plan:
//! - Claude: strip all sensitive env keys before writing to settings.json
//! - Codex: ensure no experimental_bearer_token appears in config.toml
//! - Validate that no secret patterns exist in final live configs

use crate::app_config::AppType;
use crate::error::AppError;
use crate::secrets::is_sensitive_config_key;
use serde_json::Value;

/// Sanitize Claude settings for live file: remove all sensitive env keys
///
/// According to plan section 5.3.1:
/// - Live file MUST NOT contain: env.ANTHROPIC_AUTH_TOKEN, env.ANTHROPIC_API_KEY,
///   env.ANTHROPIC_BASE_URL, or any other sensitive env keys
/// - Live file MAY contain: model names, CLAUDE_CODE_MAX_CONTEXT_TOKENS, other non-sensitive config
pub fn sanitize_claude_settings_for_live_write(settings: &Value) -> Result<Value, AppError> {
    let mut v = settings.clone();

    if let Some(obj) = v.as_object_mut() {
        // Remove internal-only fields
        obj.remove("api_format");
        obj.remove("apiFormat");
        obj.remove("openrouter_compat_mode");
        obj.remove("openrouterCompatMode");

        // Strip sensitive keys from env
        if let Some(env) = obj.get_mut("env").and_then(Value::as_object_mut) {
            let sensitive_keys: Vec<String> = env
                .keys()
                .filter(|key| is_claude_env_secret(key))
                .cloned()
                .collect();

            for key in sensitive_keys {
                env.remove(&key);
            }
        }
    }

    // Final safety check: ensure no secret patterns remain
    assert_no_secret_keys(&v, &AppType::Claude)?;

    Ok(v)
}

/// Check if a Claude env key should be treated as secret and excluded from live files
///
/// According to plan 5.3.1, these MUST NOT appear in settings.json:
/// - ANTHROPIC_API_KEY
/// - ANTHROPIC_AUTH_TOKEN
/// - ANTHROPIC_BASE_URL (credential-bearing base URL)
/// - Any other key matching sensitive patterns
fn is_claude_env_secret(key: &str) -> bool {
    // Explicit blacklist for Claude env keys per plan 5.3.1
    const CLAUDE_SECRET_ENV_KEYS: &[&str] = &[
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
    ];

    if CLAUDE_SECRET_ENV_KEYS
        .iter()
        .any(|k| key.eq_ignore_ascii_case(k))
    {
        return true;
    }

    // Also check general sensitive patterns (but with whitelist)
    is_sensitive_key_for_live(key)
}

/// Assert that no secret keys exist in the configuration
///
/// This is the final safety gate before writing live files.
/// According to plan section 5.3.1: "写入前再过一次 assert_no_secret_keys，命中即报错不写"
pub fn assert_no_secret_keys(config: &Value, app_type: &AppType) -> Result<(), AppError> {
    match app_type {
        AppType::Claude => {
            if let Some(obj) = config.as_object() {
                // Check top-level keys
                for key in obj.keys() {
                    if is_sensitive_key_for_live(key) {
                        return Err(AppError::Config(format!(
                            "安全检查失败：live 配置中仍包含敏感字段 '{}'",
                            key
                        )));
                    }
                }

                // Check env keys
                if let Some(env) = obj.get("env").and_then(Value::as_object) {
                    for key in env.keys() {
                        if is_sensitive_key_for_live(key) {
                            return Err(AppError::Config(format!(
                                "安全检查失败：live 配置的 env 中仍包含敏感字段 '{}'",
                                key
                            )));
                        }
                    }
                }
            }
        }
        AppType::Codex => {
            if let Some(obj) = config.as_object() {
                // Check for experimental_bearer_token (forbidden in plan 5.3.1)
                if obj.contains_key("experimental_bearer_token") {
                    return Err(AppError::Config(
                        "安全检查失败：Codex 配置中包含 experimental_bearer_token".to_string(),
                    ));
                }
            }
        }
        AppType::Pi => {
            // Pi validation happens in pi_config module
        }
    }

    Ok(())
}

/// Check if a key is sensitive for live file validation (stricter than is_sensitive_config_key)
///
/// This uses a more conservative approach with known-safe whitelist for common false positives
fn is_sensitive_key_for_live(key: &str) -> bool {
    // Whitelist of keys that contain sensitive patterns but are safe for live files
    const SAFE_KEYS: &[&str] = &[
        "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
        "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
        "max_tokens",
        "max_context_tokens",
    ];

    if SAFE_KEYS.iter().any(|safe| key.eq_ignore_ascii_case(safe)) {
        return false;
    }

    if is_sensitive_config_key(key) {
        return true;
    }
    let key_lower = key.to_lowercase();
    if key_lower.contains("max_output") || key_lower.ends_with("tokens") {
        return false;
    }
    if key_lower.contains("helper") {
        return false;
    }
    key_lower.ends_with("_key")
        || key_lower.ends_with("_token")
        || key_lower.ends_with("token")
        || key_lower.ends_with("_secret")
        || key_lower.ends_with("_password")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_sanitize_claude_removes_sensitive_env() {
        let settings = json!({
            "model": "claude-opus-5",
            "env": {
                "ANTHROPIC_API_KEY": "sk-ant-secret",
                "ANTHROPIC_BASE_URL": "https://api.anthropic.com",
                "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "200000",
                "MY_SECRET_TOKEN": "secret123"
            }
        });

        let sanitized = sanitize_claude_settings_for_live_write(&settings).unwrap();

        // Non-sensitive fields remain
        assert_eq!(sanitized["model"], "claude-opus-5");
        assert_eq!(sanitized["env"]["CLAUDE_CODE_MAX_CONTEXT_TOKENS"], "200000");

        // Sensitive fields removed
        assert!(sanitized["env"].get("ANTHROPIC_API_KEY").is_none());
        assert!(sanitized["env"].get("ANTHROPIC_BASE_URL").is_none());
        assert!(sanitized["env"].get("MY_SECRET_TOKEN").is_none());
    }

    #[test]
    fn test_assert_no_secret_keys_claude() {
        let clean = json!({
            "model": "claude-opus-5",
            "env": {
                "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "200000"
            }
        });
        assert!(assert_no_secret_keys(&clean, &AppType::Claude).is_ok());

        let dirty_env = json!({
            "model": "claude-opus-5",
            "env": {
                "ANTHROPIC_API_KEY": "sk-ant-secret"
            }
        });
        assert!(assert_no_secret_keys(&dirty_env, &AppType::Claude).is_err());

        let dirty_top = json!({
            "model": "claude-opus-5",
            "api_key": "sk-ant-secret"
        });
        assert!(assert_no_secret_keys(&dirty_top, &AppType::Claude).is_err());
    }

    #[test]
    fn test_assert_no_secret_keys_codex() {
        let clean = json!({
            "auth": {"type": "oauth"},
            "config": "some config"
        });
        assert!(assert_no_secret_keys(&clean, &AppType::Codex).is_ok());

        let dirty = json!({
            "auth": {"type": "oauth"},
            "experimental_bearer_token": "secret"
        });
        assert!(assert_no_secret_keys(&dirty, &AppType::Codex).is_err());
    }
}
