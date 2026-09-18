use std::collections::BTreeMap;
use zeroize::Zeroizing;

/// Provider secrets extracted from settings_config
/// Custom Debug implementation to prevent logging sensitive data
#[derive(Clone, PartialEq, Eq)]
pub struct ProviderSecrets {
    pub api_key: Option<Zeroizing<String>>,
    pub base_url: Option<Zeroizing<String>>,
    /// Map of environment variable name to value (Claude extra_env, Pi sensitive headers).
    /// BTreeMap so credential writes and orphan-cleanup targets are in a stable order.
    pub extra_env: BTreeMap<String, Zeroizing<String>>,
}

impl ProviderSecrets {
    pub fn new() -> Self {
        Self {
            api_key: None,
            base_url: None,
            extra_env: BTreeMap::new(),
        }
    }

    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(Zeroizing::new(key.into()));
        self
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(Zeroizing::new(url.into()));
        self
    }

    pub fn with_extra_env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_env
            .insert(name.into(), Zeroizing::new(value.into()));
        self
    }

    pub fn is_empty(&self) -> bool {
        self.api_key.is_none() && self.base_url.is_none() && self.extra_env.is_empty()
    }
}

impl Default for ProviderSecrets {
    fn default() -> Self {
        Self::new()
    }
}

// Custom Debug implementation (principle 3.1-8: logs must not contain secrets)
impl std::fmt::Debug for ProviderSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderSecrets")
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("base_url", &self.base_url.as_ref().map(|_| "[REDACTED]"))
            .field(
                "extra_env",
                &self
                    .extra_env
                    .keys()
                    .map(|k| format!("{}=[REDACTED]", k))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// Result of secret extraction
#[derive(Debug)]
pub struct Extracted {
    /// The settings_config with secrets stripped out
    pub stripped: serde_json::Value,
    /// The extracted secrets
    pub secrets: ProviderSecrets,
    /// §5.2.3 / §6.2-6：Codex 官方卡里被丢弃的 OAuth 登录态（值不保留，只记事实）。
    pub dropped_codex_oauth_tokens: bool,
    /// §5.2.3 / §6.2-6：不阻断迁移的告警（如 Pi 模型级 baseUrl），只含字段名不含值。
    pub warnings: Vec<String>,
}

impl Extracted {
    pub fn new(stripped: serde_json::Value, secrets: ProviderSecrets) -> Self {
        Self {
            stripped,
            secrets,
            dropped_codex_oauth_tokens: false,
            warnings: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_secrets_debug_redacts() {
        let secrets = ProviderSecrets::new()
            .with_api_key("sk-secret-key-12345")
            .with_base_url("https://api.example.com")
            .with_extra_env("CUSTOM_KEY", "custom-value");

        let debug_output = format!("{:?}", secrets);
        assert!(!debug_output.contains("sk-secret"));
        assert!(!debug_output.contains("api.example.com"));
        assert!(!debug_output.contains("custom-value"));
        assert!(debug_output.contains("[REDACTED]"));
        assert!(debug_output.contains("CUSTOM_KEY=[REDACTED]"));
    }

    #[test]
    fn test_provider_secrets_is_empty() {
        let empty = ProviderSecrets::new();
        assert!(empty.is_empty());

        let with_key = ProviderSecrets::new().with_api_key("test");
        assert!(!with_key.is_empty());

        let with_url = ProviderSecrets::new().with_base_url("http://test");
        assert!(!with_url.is_empty());

        let with_env = ProviderSecrets::new().with_extra_env("KEY", "value");
        assert!(!with_env.is_empty());
    }
}
