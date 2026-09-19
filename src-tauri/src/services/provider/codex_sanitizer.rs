//! Codex config.toml sanitization - inject environment variable references
//!
//! Implements Phase 4 safety requirements for Codex configuration:
//! - Remove experimental_bearer_token from config.toml
//! - Inject env_key reference pointing to CC_SWITCH_CODEX_API_KEY
//! - Ensure live config.toml never contains plaintext API keys

use crate::error::AppError;

/// cc-switch 旧版本做"官方 Codex 路由接管"时写进 `config.toml` 的保留 provider id。
/// 本地代理已删除，这张表只剩 `base_url = http://127.0.0.1:<port>` 这种死端点。
const CC_SWITCH_CODEX_OFFICIAL_PROXY_PROVIDER_ID: &str = "cc-switch-official";

/// 清掉本地代理时代的官方路由残留（§6.4）。
///
/// 该 id 是 cc-switch 自己的保留 id（用户不可能主动使用），因此整表删除；
/// `model_provider` 指向它时一并清空，让 Codex 回落到内置 `openai` provider。
/// 不清理的话，老用户的 `config.toml` 每次切换都会被原样写回一个不存在的本地端点。
fn strip_codex_official_proxy_route(doc: &mut toml_edit::DocumentMut) {
    let has_stale_table = doc
        .get("model_providers")
        .and_then(|item| item.as_table_like())
        .and_then(|table| table.get(CC_SWITCH_CODEX_OFFICIAL_PROXY_PROVIDER_ID))
        .is_some();
    if has_stale_table {
        if let Some(model_providers) = doc
            .get_mut("model_providers")
            .and_then(|item| item.as_table_like_mut())
        {
            model_providers.remove(CC_SWITCH_CODEX_OFFICIAL_PROXY_PROVIDER_ID);
        }
    }
    if doc.get("model_provider").and_then(|item| item.as_str())
        == Some(CC_SWITCH_CODEX_OFFICIAL_PROXY_PROVIDER_ID)
    {
        doc.remove("model_provider");
    }
}

/// Inject env_key reference into Codex config.toml
///
/// According to plan section 5.4 line 324:
/// - Codex config.toml should use `env_key = "CC_SWITCH_CODEX_API_KEY"` instead of plaintext
/// - Remove any existing experimental_bearer_token
/// - Inject env_key at the appropriate level (top-level or active provider)
pub fn sanitize_codex_config_for_live_write(toml_text: &str) -> Result<String, AppError> {
    sanitize_codex_config_for_live_write_with_has_key(toml_text, true, None)
}

/// Preflight variant: the injection only happens when the store actually
/// holds the provider's key (the env var will be set). A keyless provider
/// must NOT gain an `env_key`, or the fail-closed fallback gate is bypassed.
pub fn sanitize_codex_config_for_live_write_preflight(
    toml_text: &str,
    has_store_key: bool,
) -> Result<String, AppError> {
    sanitize_codex_config_for_live_write_with_has_key(toml_text, has_store_key, None)
}

pub fn sanitize_codex_config_for_live_write_with_base_url(
    toml_text: &str,
    base_url: Option<&str>,
) -> Result<String, AppError> {
    sanitize_codex_config_for_live_write_with_has_key(toml_text, true, base_url)
}

fn sanitize_codex_config_for_live_write_with_has_key(
    toml_text: &str,
    has_store_key: bool,
    base_url: Option<&str>,
) -> Result<String, AppError> {
    use toml_edit::DocumentMut;

    let mut doc = toml_text
        .parse::<DocumentMut>()
        .map_err(|e| AppError::Config(format!("Failed to parse Codex config.toml: {}", e)))?;

    // Remove experimental_bearer_token if present (forbidden per plan 5.3.1)
    doc.remove("experimental_bearer_token");

    // §6.4：本地代理删除前，官方卡的路由表指向 http://127.0.0.1:<port>，必须清掉。
    strip_codex_official_proxy_route(&mut doc);

    // §5.2.3 / S5：非激活表的 bearer token 同样禁止落 live，逐表清掉。
    let all_provider_ids: Vec<String> = doc
        .get("model_providers")
        .and_then(|v| v.as_table_like())
        .map(|table| table.iter().map(|(key, _)| key.to_string()).collect())
        .unwrap_or_default();
    for provider_id in &all_provider_ids {
        if let Some(provider_table) = doc
            .get_mut("model_providers")
            .and_then(|v| v.as_table_like_mut())
            .and_then(|providers_table| {
                providers_table
                    .get_mut(provider_id.as_str())
                    .and_then(|v| v.as_table_like_mut())
            })
        {
            provider_table.remove("experimental_bearer_token");
        }
    }

    // Get the active model_provider to determine where to inject env_key
    let active_provider = doc
        .get("model_provider")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Inject into the active provider's table when it exists; otherwise
    // (no table yet — e.g. a legacy `openai` reroute that the live-write
    // path normalizes afterwards) fall back to a top-level env_key so the
    // safety gates recognize the config as key-carrying.
    let injected_table = active_provider
        .as_deref()
        .and_then(|provider_id| {
            doc.get_mut("model_providers")
                .and_then(|v| v.as_table_like_mut())
                .and_then(|providers_table| {
                    providers_table
                        .get_mut(provider_id)
                        .and_then(|v| v.as_table_like_mut())
                })
        })
        .map(|provider_table| {
            // Remove experimental_bearer_token from provider
            provider_table.remove("experimental_bearer_token");
            if has_store_key {
                provider_table.insert("env_key", toml_edit::value("CC_SWITCH_CODEX_API_KEY"));
            }
            if let Some(url) = base_url {
                provider_table.insert("base_url", toml_edit::value(url));
            }
        })
        .is_some();

    if !injected_table && has_store_key {
        doc.insert(
            "env_key",
            toml_edit::Item::Value(toml_edit::Value::from("CC_SWITCH_CODEX_API_KEY")),
        );
    }

    let result = doc.to_string();
    // S5 写盘门控：仍残留 bearer token 即报错不写，绝不 fail-open。
    if result.contains("experimental_bearer_token") {
        return Err(AppError::Config(
            "安全检查失败：Codex 配置中仍包含 experimental_bearer_token".to_string(),
        ));
    }
    Ok(result)
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

    #[test]
    fn test_strips_legacy_official_proxy_route() {
        // §6.4 / P1-2：本地代理时代的官方路由表与 model_provider 都必须清掉，
        // 否则每次切换都会把 http://127.0.0.1:<port> 这个死端点写回盘。
        let toml = r#"
model_provider = "cc-switch-official"

[model_providers.cc-switch-official]
name = "OpenAI"
requires_openai_auth = true
supports_websockets = false
wire_api = "responses"
base_url = "http://127.0.0.1:57321"
"#;

        let result = sanitize_codex_config_for_live_write(toml).unwrap();

        assert!(!result.contains("cc-switch-official"));
        assert!(!result.contains("127.0.0.1"));
        assert!(!result.contains("model_provider"));
    }

    #[test]
    fn test_keeps_third_party_route_untouched() {
        // 只清 cc-switch 自己的保留 id，第三方路由不受影响。
        let toml = r#"
model_provider = "my-vendor"

[model_providers.my-vendor]
name = "My Vendor"
base_url = "https://api.my-vendor.example.com/v1"
wire_api = "responses"
"#;

        let result = sanitize_codex_config_for_live_write(toml).unwrap();

        assert!(result.contains(r#"model_provider = "my-vendor""#));
        assert!(result.contains("[model_providers.my-vendor]"));
        assert!(result.contains("https://api.my-vendor.example.com/v1"));
    }

    #[test]
    fn test_strips_bearer_token_from_inactive_provider() {
        // §5.2.3：非激活 [model_providers.*] 表的 bearer token 也绝不能落 live。
        let toml = r#"
model_provider = "active"

[model_providers.active]
base_url = "https://api.active.example.com/v1"

[model_providers.inactive]
base_url = "https://api.inactive.example.com/v1"
experimental_bearer_token = "sk-fixture-inactive-0009"
"#;

        let result = sanitize_codex_config_for_live_write(toml).unwrap();

        assert!(!result.contains("sk-fixture-inactive-0009"));
        assert!(!result.contains("experimental_bearer_token"));
        assert!(result.contains("[model_providers.inactive]"));
    }
}
