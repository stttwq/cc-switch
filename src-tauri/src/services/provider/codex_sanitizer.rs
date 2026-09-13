//! Codex config.toml sanitization - inject environment variable references
//!
//! Implements Phase 4 safety requirements for Codex configuration:
//! - Remove experimental_bearer_token from config.toml
//! - Inject env_key reference pointing to CC_SWITCH_CODEX_API_KEY
//! - Ensure live config.toml never contains plaintext API keys

use crate::error::AppError;

/// Inject env_key reference into Codex config.toml
///
/// According to plan section 5.4 line 324:
/// - Codex config.toml should use `env_key = "CC_SWITCH_CODEX_API_KEY"` instead of plaintext
/// - Remove any existing experimental_bearer_token
/// - Inject env_key at the appropriate level (top-level or active provider)
pub fn sanitize_codex_config_for_live_write(toml_text: &str) -> Result<String, AppError> {
    use toml_edit::DocumentMut;

    let mut doc = toml_text
        .parse::<DocumentMut>()
        .map_err(|e| AppError::Config(format!("Failed to parse Codex config.toml: {}", e)))?;

    // Remove experimental_bearer_token if present (forbidden per plan 5.3.1)
    doc.remove("experimental_bearer_token");

    // Get the active model_provider to determine where to inject env_key
    let active_provider = doc
        .get("model_provider")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    match active_provider {
        Some(provider_id) => {
            // Inject env_key into the active provider section
            if let Some(providers_table) = doc
                .get_mut("model_providers")
                .and_then(|v| v.as_table_like_mut())
            {
                if let Some(provider_table) = providers_table
                    .get_mut(&provider_id)
                    .and_then(|v| v.as_table_like_mut())
                {
                    // Remove experimental_bearer_token from provider
                    provider_table.remove("experimental_bearer_token");

                    // Inject env_key
                    provider_table.insert(
                        "env_key",
                        toml_edit::value("CC_SWITCH_CODEX_API_KEY"),
                    );
                }
            }
        }
        None => {
            // No active provider - inject env_key at top level
            doc.insert(
                "env_key",
                toml_edit::Item::Value(toml_edit::Value::from("CC_SWITCH_CODEX_API_KEY")),
            );
        }
    }

    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inject_env_key_top_level() {
        let toml = r#"
experimental_bearer_token = "sk-test-12345"
some_other_field = "value"
"#;

        let result = sanitize_codex_config_for_live_write(toml).unwrap();

        // Should remove experimental_bearer_token and add env_key
        assert!(!result.contains("experimental_bearer_token"));
        assert!(result.contains(r#"env_key = "CC_SWITCH_CODEX_API_KEY""#));
        assert!(result.contains(r#"some_other_field = "value""#));
    }

    #[test]
    fn test_inject_env_key_into_active_provider() {
        let toml = r#"
model_provider = "anthropic"

[model_providers.anthropic]
experimental_bearer_token = "sk-test-12345"
base_url = "https://api.anthropic.com"
"#;

        let result = sanitize_codex_config_for_live_write(toml).unwrap();

        // Should remove experimental_bearer_token from provider and inject env_key
        assert!(!result.contains("experimental_bearer_token"));
        assert!(result.contains(r#"env_key = "CC_SWITCH_CODEX_API_KEY""#));
        assert!(result.contains(r#"base_url = "https://api.anthropic.com""#));
    }

    #[test]
    fn test_inject_env_key_preserves_other_fields() {
        let toml = r#"
model_provider = "openai"

[model_providers.openai]
base_url = "https://api.openai.com/v1"
timeout = 30

[model_providers.anthropic]
base_url = "https://api.anthropic.com"
"#;

        let result = sanitize_codex_config_for_live_write(toml).unwrap();

        // Should inject env_key into openai provider
        assert!(result.contains(r#"env_key = "CC_SWITCH_CODEX_API_KEY""#));
        assert!(result.contains(r#"timeout = 30"#));

        // Should preserve non-active provider as-is
        assert!(result.contains("[model_providers.anthropic]"));
    }

    #[test]
    fn test_inject_env_key_when_no_token_present() {
        let toml = r#"
model_provider = "anthropic"

[model_providers.anthropic]
base_url = "https://api.anthropic.com"
"#;

        let result = sanitize_codex_config_for_live_write(toml).unwrap();

        // Should inject env_key even when no token was present
        assert!(result.contains(r#"env_key = "CC_SWITCH_CODEX_API_KEY""#));
    }
}
