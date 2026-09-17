//! Pi models.json sanitization - ensure API keys are replaced with environment variable references
//!
//! Implements Phase 4 safety requirements from secrets-credential-manager-slimdown-plan:
//! - Pi: replace plaintext API keys with $CC_SWITCH_PI_<ID>_API_KEY references
//! - Validate that no secret patterns exist in final models.json

use crate::error::AppError;
use serde_json::Value;

/// Sanitize Pi provider config for live write: replace API keys with environment variable references
///
/// According to plan section 5.4:
/// - Pi models.json should use `$CC_SWITCH_PI_<ID>_API_KEY` instead of plaintext API keys
/// - Remove any existing apiKey field
/// - Inject environment variable reference at the provider level
pub fn sanitize_pi_provider_for_live_write(
    provider_id: &str,
    config: &Value,
) -> Result<Value, AppError> {
    let mut sanitized = config.clone();

    if let Value::Object(obj) = &mut sanitized {
        // Remove plaintext API key if present
        if obj.contains_key("apiKey") || obj.contains_key("api_key") {
            obj.remove("apiKey");
            obj.remove("api_key");

            // Inject environment variable reference
            let var_name = format!(
                "CC_SWITCH_PI_{}_API_KEY",
                crate::secrets::normalize_env_key_segment(provider_id)
            );
            let env_ref = format!("${}", var_name);
            obj.insert("apiKey".to_string(), Value::String(env_ref));
        }
    }

    Ok(sanitized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_sanitize_pi_replaces_api_key_with_env_ref() {
        let config = json!({
            "apiKey": "sk-pi-test123",
            "model": "claude-opus-5",
            "baseUrl": "https://api.example.com"
        });

        let sanitized = sanitize_pi_provider_for_live_write("anthropic", &config).unwrap();

        // API key replaced with environment variable reference
        assert_eq!(sanitized["apiKey"], "$CC_SWITCH_PI_ANTHROPIC_API_KEY");

        // Other fields preserved
        assert_eq!(sanitized["model"], "claude-opus-5");
        assert_eq!(sanitized["baseUrl"], "https://api.example.com");

        // Plaintext key not present
        assert!(!sanitized.to_string().contains("sk-pi-test123"));
    }

    #[test]
    fn test_sanitize_pi_handles_snake_case_api_key() {
        let config = json!({
            "api_key": "sk-pi-test456",
            "model": "claude-opus-5"
        });

        let sanitized = sanitize_pi_provider_for_live_write("openai", &config).unwrap();

        assert_eq!(sanitized["apiKey"], "$CC_SWITCH_PI_OPENAI_API_KEY");
        assert!(!sanitized.to_string().contains("sk-pi-test456"));
    }

    #[test]
    fn test_sanitize_pi_no_op_when_no_api_key() {
        let config = json!({
            "model": "claude-opus-5",
            "baseUrl": "https://api.example.com"
        });

        let sanitized = sanitize_pi_provider_for_live_write("anthropic", &config).unwrap();

        // Config unchanged when no API key present
        assert_eq!(sanitized, config);
    }
}
