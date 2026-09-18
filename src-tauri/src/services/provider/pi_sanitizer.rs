//! Pi models.json sanitization: 字面量 apiKey / 敏感 header 改写为 $VAR。

use crate::error::AppError;
use crate::secrets::{
    is_literal_value, is_sensitive_config_key, pi_api_key_env_name, pi_header_env_name,
};
use serde_json::Value;

pub fn sanitize_pi_provider_for_live_write(
    provider_id: &str,
    config: &Value,
) -> Result<Value, AppError> {
    let mut sanitized = config.clone();
    let Some(obj) = sanitized.as_object_mut() else {
        return Ok(sanitized);
    };

    let current_key = obj
        .get("apiKey")
        .or_else(|| obj.get("api_key"))
        .and_then(Value::as_str);
    let should_inject_key =
        !current_key.is_some_and(|val| !val.is_empty() && !is_literal_value(val));
    if should_inject_key {
        obj.remove("api_key");
        obj.insert(
            "apiKey".to_string(),
            Value::String(format!("${}", pi_api_key_env_name(provider_id))),
        );
    }

    if let Some(headers) = obj.get_mut("headers").and_then(Value::as_object_mut) {
        let names: Vec<String> = headers
            .iter()
            .filter_map(|(k, v)| {
                let val = v.as_str()?;
                if is_sensitive_config_key(k) && is_literal_value(val) && !val.is_empty() {
                    Some(k.clone())
                } else {
                    None
                }
            })
            .collect();
        for name in names {
            let var = pi_header_env_name(provider_id, &name);
            headers.insert(name, Value::String(format!("${var}")));
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
        assert_eq!(sanitized["apiKey"], "$CC_SWITCH_PI_ANTHROPIC_API_KEY");
        assert_eq!(sanitized["model"], "claude-opus-5");
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
    fn test_sanitize_pi_rewrites_sensitive_headers() {
        let config = json!({
            "apiKey": "$CC_SWITCH_PI_AW_API_KEY",
            "headers": {
                "Authorization": "sk-header-plain",
                "X-Debug": "ok"
            }
        });
        let sanitized = sanitize_pi_provider_for_live_write("aw", &config).unwrap();
        assert_eq!(sanitized["apiKey"], "$CC_SWITCH_PI_AW_API_KEY");
        assert_eq!(
            sanitized["headers"]["Authorization"],
            "$CC_SWITCH_PI_AW_HEADER_AUTHORIZATION"
        );
        assert_eq!(sanitized["headers"]["X-Debug"], "ok");
        assert!(!sanitized.to_string().contains("sk-header-plain"));
    }

    #[test]
    fn test_sanitize_pi_injects_env_ref_when_no_api_key() {
        let config = json!({
            "model": "claude-opus-5",
            "baseUrl": "https://api.example.com"
        });
        let sanitized = sanitize_pi_provider_for_live_write("anthropic", &config).unwrap();
        assert_eq!(sanitized["apiKey"], "$CC_SWITCH_PI_ANTHROPIC_API_KEY");
        assert_eq!(sanitized["model"], "claude-opus-5");
    }
}
