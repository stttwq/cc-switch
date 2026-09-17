use crate::app_config::AppType;
use crate::codex_config::{
    extract_codex_base_url, extract_codex_experimental_bearer_token,
    remove_codex_experimental_bearer_token_if,
};
use crate::error::AppError;
use crate::secrets::rules::{
    is_literal_value, is_sensitive_config_key, normalize_env_key_segment, unescape_literal,
};
use crate::secrets::store::SecretStore;
use crate::secrets::target::SecretTarget;
use crate::secrets::types::{Extracted, ProviderSecrets};
use serde_json::{json, Value};
use std::collections::HashMap;
use toml_edit::DocumentMut;

/// Extract secrets from provider config JSON and store them in SecretStore
pub struct SecretExtractor<'a> {
    store: &'a dyn SecretStore,
    app: AppType,
}

impl<'a> SecretExtractor<'a> {
    pub fn new(store: &'a dyn SecretStore, app: AppType) -> Self {
        Self { store, app }
    }

    /// 纯提取：剥密钥与 Base URL，不写凭据管理器。
    pub fn extract(provider_id: &str, app: &AppType, raw: &Value) -> Result<Extracted, AppError> {
        Self::extract_with_meta(provider_id, app, raw, None)
    }

    pub fn extract_with_meta(
        provider_id: &str,
        app: &AppType,
        raw: &Value,
        api_key_field: Option<&str>,
    ) -> Result<Extracted, AppError> {
        match app {
            AppType::Claude => extract_claude(raw, api_key_field),
            AppType::Codex => extract_codex(raw),
            AppType::Pi => extract_pi(provider_id, raw),
        }
    }

    /// Extract secrets from a provider config, persist them, return stripped JSON.
    pub async fn extract_provider_secrets(
        &self,
        provider_id: &str,
        config: &Value,
    ) -> Result<(Value, ProviderSecrets), AppError> {
        self.extract_provider_secrets_with_field(provider_id, config, None)
            .await
    }

    pub async fn extract_provider_secrets_with_field(
        &self,
        provider_id: &str,
        config: &Value,
        api_key_field: Option<&str>,
    ) -> Result<(Value, ProviderSecrets), AppError> {
        let extracted = Self::extract_with_meta(provider_id, &self.app, config, api_key_field)?;
        persist_secrets(self.store, &self.app, provider_id, &extracted.secrets).await?;
        Ok((extracted.stripped, extracted.secrets))
    }

    /// Restore secrets from SecretStore into a config object
    pub async fn restore_provider_secrets(
        &self,
        provider_id: &str,
        sanitized_config: &Value,
    ) -> Result<Value, AppError> {
        let secrets = load_secrets(self.store, &self.app, provider_id).await?;
        Ok(hydrate(&self.app, sanitized_config, &secrets))
    }

    /// Extract app-level secrets (e.g., WebDAV password, S3 credentials)
    pub async fn extract_app_secrets(
        &self,
        config: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>, AppError> {
        let mut sanitized = config.clone();

        for (key, value) in config {
            if is_sensitive_config_key(key) && !value.starts_with("literal:") {
                let target = SecretTarget::app(self.app.as_str(), key.clone());
                self.store.store(&target, value).await?;
                sanitized.remove(key);
            }
        }

        Ok(sanitized)
    }

    /// Restore app-level secrets from SecretStore
    pub async fn restore_app_secrets(
        &self,
        sanitized_config: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>, AppError> {
        Ok(sanitized_config.clone())
    }

    /// Extract WebDAV password from settings
    pub async fn extract_webdav_password(&self, password: &str) -> Result<(), AppError> {
        if password.is_empty() || password.starts_with("literal:") {
            return Ok(());
        }
        let target = SecretTarget::app("webdav", "password");
        self.store.store(&target, password).await
    }

    /// Restore WebDAV password from SecretStore
    pub async fn restore_webdav_password(&self) -> Result<Option<String>, AppError> {
        let target = SecretTarget::app("webdav", "password");
        self.store.retrieve(&target).await
    }

    /// Extract S3 credentials from settings
    pub async fn extract_s3_credentials(
        &self,
        access_key_id: &str,
        secret_access_key: &str,
    ) -> Result<(), AppError> {
        if !access_key_id.is_empty() && !access_key_id.starts_with("literal:") {
            let target = SecretTarget::app("s3", "access_key_id");
            self.store.store(&target, access_key_id).await?;
        }
        if !secret_access_key.is_empty() && !secret_access_key.starts_with("literal:") {
            let target = SecretTarget::app("s3", "secret_access_key");
            self.store.store(&target, secret_access_key).await?;
        }
        Ok(())
    }

    /// Restore S3 credentials from SecretStore
    pub async fn restore_s3_credentials(
        &self,
    ) -> Result<(Option<String>, Option<String>), AppError> {
        let access_key_id = self
            .store
            .retrieve(&SecretTarget::app("s3", "access_key_id"))
            .await?;
        let secret_access_key = self
            .store
            .retrieve(&SecretTarget::app("s3", "secret_access_key"))
            .await?;
        Ok((access_key_id, secret_access_key))
    }
}

async fn persist_secrets(
    store: &dyn SecretStore,
    app: &AppType,
    provider_id: &str,
    secrets: &ProviderSecrets,
) -> Result<(), AppError> {
    if let Some(key) = secrets.api_key.as_ref() {
        store
            .store(
                &SecretTarget::provider_api_key(app.clone(), provider_id),
                key.as_str(),
            )
            .await?;
    }
    if let Some(url) = secrets.base_url.as_ref() {
        store
            .store(
                &SecretTarget::provider_base_url(app.clone(), provider_id),
                url.as_str(),
            )
            .await?;
    }
    for (name, value) in &secrets.extra_env {
        store
            .store(
                &SecretTarget::provider_env(app.clone(), provider_id, name),
                value.as_str(),
            )
            .await?;
    }
    Ok(())
}

async fn load_secrets(
    store: &dyn SecretStore,
    app: &AppType,
    provider_id: &str,
) -> Result<ProviderSecrets, AppError> {
    let mut secrets = ProviderSecrets::new();
    if let Some(key) = store
        .retrieve(&SecretTarget::provider_api_key(app.clone(), provider_id))
        .await?
    {
        secrets = secrets.with_api_key(key);
    }
    if let Some(url) = store
        .retrieve(&SecretTarget::provider_base_url(app.clone(), provider_id))
        .await?
    {
        secrets = secrets.with_base_url(url);
    }
    Ok(secrets)
}

pub fn hydrate(app: &AppType, stripped: &Value, secrets: &ProviderSecrets) -> Value {
    let mut restored = stripped.clone();
    match app {
        AppType::Claude => {
            let env = restored
                .as_object_mut()
                .map(|obj| obj.entry("env").or_insert_with(|| json!({})))
                .and_then(Value::as_object_mut);
            if let Some(env) = env {
                if let Some(key) = secrets.api_key.as_ref() {
                    env.insert(
                        "ANTHROPIC_AUTH_TOKEN".to_string(),
                        Value::String(key.to_string()),
                    );
                }
                if let Some(url) = secrets.base_url.as_ref() {
                    env.insert(
                        "ANTHROPIC_BASE_URL".to_string(),
                        Value::String(url.to_string()),
                    );
                }
                for (name, value) in &secrets.extra_env {
                    env.insert(name.clone(), Value::String(value.to_string()));
                }
            }
        }
        AppType::Codex => {
            if let Some(obj) = restored.as_object_mut() {
                if let Some(key) = secrets.api_key.as_ref() {
                    let mut auth = obj
                        .get("auth")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    auth.insert(
                        "OPENAI_API_KEY".to_string(),
                        Value::String(key.to_string()),
                    );
                    obj.insert("auth".to_string(), Value::Object(auth));
                }
            }
        }
        AppType::Pi => {
            if let Some(obj) = restored.as_object_mut() {
                if let Some(key) = secrets.api_key.as_ref() {
                    obj.insert("apiKey".to_string(), Value::String(key.to_string()));
                }
                if let Some(url) = secrets.base_url.as_ref() {
                    obj.insert("baseUrl".to_string(), Value::String(url.to_string()));
                }
            }
        }
    }
    restored
}

fn take_string_field(obj: &mut serde_json::Map<String, Value>, key: &str) -> Option<String> {
    match obj.remove(key) {
        Some(Value::String(s)) if !s.is_empty() && s != "literal:***" => Some(s),
        _ => None,
    }
}

fn extract_claude(raw: &Value, api_key_field: Option<&str>) -> Result<Extracted, AppError> {
    let mut stripped = raw.clone();
    let mut secrets = ProviderSecrets::new();
    let Some(root) = stripped.as_object_mut() else {
        return Ok(Extracted { stripped, secrets });
    };
    let Some(env) = root.get_mut("env").and_then(Value::as_object_mut) else {
        return Ok(Extracted { stripped, secrets });
    };

    let auth_token = take_string_field(env, "ANTHROPIC_AUTH_TOKEN");
    let api_key = take_string_field(env, "ANTHROPIC_API_KEY");
    let prefer_api_key = api_key_field
        .map(|f| f.eq_ignore_ascii_case("ANTHROPIC_API_KEY"))
        .unwrap_or(false);
    let chosen = if prefer_api_key {
        api_key.or(auth_token)
    } else {
        auth_token.or(api_key)
    };
    if let Some(token) = chosen {
        secrets = secrets.with_api_key(token);
    }
    if let Some(url) = take_string_field(env, "ANTHROPIC_BASE_URL") {
        secrets = secrets.with_base_url(url);
    }

    let extra_keys: Vec<String> = env
        .iter()
        .filter(|(k, v)| v.as_str().is_some() && is_sensitive_config_key(k))
        .map(|(k, _)| k.clone())
        .collect();
    for key in extra_keys {
        if let Some(val) = take_string_field(env, &key) {
            secrets = secrets.with_extra_env(key, val);
        }
    }

    Ok(Extracted { stripped, secrets })
}

fn extract_codex(raw: &Value) -> Result<Extracted, AppError> {
    let mut stripped = raw.clone();
    let mut secrets = ProviderSecrets::new();
    let Some(root) = stripped.as_object_mut() else {
        return Ok(Extracted { stripped, secrets });
    };

    if let Some(auth) = root.get_mut("auth").and_then(Value::as_object_mut) {
        if let Some(key) = take_string_field(auth, "OPENAI_API_KEY") {
            secrets = secrets.with_api_key(key);
        }
        auth.remove("tokens");
        auth.remove("last_refresh");
    }

    if let Some(config_text) = root.get("config").and_then(Value::as_str) {
        let mut config_text = config_text.to_string();
        if secrets.api_key.is_none() {
            if let Some(token) = extract_codex_experimental_bearer_token(&config_text) {
                secrets = secrets.with_api_key(token);
            }
        }
        if let Some(url) = extract_codex_base_url(&config_text) {
            secrets = secrets.with_base_url(url);
            config_text = strip_active_codex_base_url(&config_text)?;
        }
        config_text = remove_codex_experimental_bearer_token_if(&config_text, |_| true)?;
        root.insert("config".to_string(), Value::String(config_text));
    }

    Ok(Extracted { stripped, secrets })
}

fn strip_active_codex_base_url(config_text: &str) -> Result<String, AppError> {
    let mut doc = config_text
        .parse::<DocumentMut>()
        .map_err(|e| AppError::Message(format!("Invalid Codex config.toml: {e}")))?;
    let provider_id = doc
        .get("model_provider")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    if let Some(id) = provider_id {
        if let Some(table) = doc
            .get_mut("model_providers")
            .and_then(|item| item.as_table_like_mut())
            .and_then(|table| table.get_mut(id.as_str()))
            .and_then(|item| item.as_table_like_mut())
        {
            table.remove("base_url");
        }
    }
    Ok(doc.to_string())
}

fn extract_pi(provider_id: &str, raw: &Value) -> Result<Extracted, AppError> {
    let mut stripped = raw.clone();
    let mut secrets = ProviderSecrets::new();
    let Some(root) = stripped.as_object_mut() else {
        return Ok(Extracted { stripped, secrets });
    };

    match root.get("apiKey").and_then(Value::as_str) {
        Some(val) if is_literal_value(val) && !val.is_empty() => {
            secrets = secrets.with_api_key(unescape_literal(val));
            root.remove("apiKey");
        }
        _ => {}
    }
    if let Some(url) = take_string_field(root, "baseUrl") {
        secrets = secrets.with_base_url(url);
    }

    if let Some(headers) = root.get_mut("headers").and_then(Value::as_object_mut) {
        let sensitive: Vec<(String, String)> = headers
            .iter()
            .filter_map(|(k, v)| {
                let val = v.as_str()?;
                if is_sensitive_config_key(k) && is_literal_value(val) && !val.is_empty() {
                    Some((k.clone(), unescape_literal(val)))
                } else {
                    None
                }
            })
            .collect();
        let key_seg = normalize_env_key_segment(provider_id);
        for (name, value) in sensitive {
            let var = format!(
                "CC_SWITCH_PI_{}_HEADER_{}",
                key_seg,
                normalize_env_key_segment(&name)
            );
            headers.insert(name.clone(), Value::String(format!("${var}")));
            secrets = secrets.with_extra_env(name, value);
        }
    }

    Ok(Extracted { stripped, secrets })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::store::InMemorySecretStore;
    use serde_json::json;

    #[test]
    fn extract_claude_strips_auth_and_base_url() {
        let raw = json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": "sk-ant-fixture",
                "ANTHROPIC_BASE_URL": "https://fixture-claude.example.com/v1",
                "ANTHROPIC_MODEL": "claude-opus-4",
                "CLAUDE_CODE_MAX_OUTPUT_TOKENS": "64000"
            }
        });
        let extracted = SecretExtractor::extract("p1", &AppType::Claude, &raw).unwrap();
        assert_eq!(
            extracted.secrets.api_key.as_ref().unwrap().as_str(),
            "sk-ant-fixture"
        );
        assert_eq!(
            extracted.secrets.base_url.as_ref().unwrap().as_str(),
            "https://fixture-claude.example.com/v1"
        );
        let env = extracted.stripped["env"].as_object().unwrap();
        assert!(!env.contains_key("ANTHROPIC_AUTH_TOKEN"));
        assert!(!env.contains_key("ANTHROPIC_BASE_URL"));
        assert_eq!(env["ANTHROPIC_MODEL"], "claude-opus-4");
        assert_eq!(env["CLAUDE_CODE_MAX_OUTPUT_TOKENS"], "64000");
    }

    #[test]
    fn extract_codex_strips_auth_bearer_and_base_url() {
        let raw = json!({
            "auth": { "OPENAI_API_KEY": "sk-fixture-codex-official-0001" },
            "config": "model_provider = \"custom\"\n\n[model_providers.custom]\nbase_url = \"https://api.example.com/v1\"\nexperimental_bearer_token = \"sk-fixture-codex-3rd-0002\"\n"
        });
        let extracted = SecretExtractor::extract("p1", &AppType::Codex, &raw).unwrap();
        assert_eq!(
            extracted.secrets.api_key.as_ref().unwrap().as_str(),
            "sk-fixture-codex-official-0001"
        );
        assert_eq!(
            extracted.secrets.base_url.as_ref().unwrap().as_str(),
            "https://api.example.com/v1"
        );
        let config = extracted.stripped["config"].as_str().unwrap();
        assert!(!config.contains("experimental_bearer_token"));
        assert!(!config.contains("https://api.example.com/v1"));
        assert!(extracted.stripped["auth"].get("OPENAI_API_KEY").is_none());
    }

    #[test]
    fn extract_pi_strips_literal_key_and_sensitive_header() {
        let raw = json!({
            "apiKey": "sk-fixture-pi",
            "baseUrl": "https://fixture-pi.example.com",
            "headers": {
                "Authorization": "sk-fixture-pi-header-0008",
                "X-Debug": "ok"
            }
        });
        let extracted = SecretExtractor::extract("pi-one", &AppType::Pi, &raw).unwrap();
        assert_eq!(
            extracted.secrets.api_key.as_ref().unwrap().as_str(),
            "sk-fixture-pi"
        );
        assert_eq!(
            extracted.secrets.base_url.as_ref().unwrap().as_str(),
            "https://fixture-pi.example.com"
        );
        assert!(extracted.stripped.get("apiKey").is_none());
        assert!(extracted.stripped.get("baseUrl").is_none());
        assert_eq!(
            extracted.stripped["headers"]["Authorization"],
            "$CC_SWITCH_PI_PI_ONE_HEADER_AUTHORIZATION"
        );
        assert_eq!(extracted.stripped["headers"]["X-Debug"], "ok");
    }

    #[test]
    fn extract_claude_prefers_api_key_field() {
        let raw = json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": "sk-auth",
                "ANTHROPIC_API_KEY": "sk-api"
            }
        });
        let extracted = SecretExtractor::extract_with_meta(
            "p1",
            &AppType::Claude,
            &raw,
            Some("ANTHROPIC_API_KEY"),
        )
        .unwrap();
        assert_eq!(extracted.secrets.api_key.as_ref().unwrap().as_str(), "sk-api");
        let env = extracted.stripped["env"].as_object().unwrap();
        assert!(!env.contains_key("ANTHROPIC_AUTH_TOKEN"));
        assert!(!env.contains_key("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn extract_pi_keeps_dollar_var() {
        let raw = json!({ "apiKey": "$MY_KEY", "baseUrl": "https://x" });
        let extracted = SecretExtractor::extract("p1", &AppType::Pi, &raw).unwrap();
        assert!(extracted.secrets.api_key.is_none());
        assert_eq!(extracted.stripped["apiKey"], "$MY_KEY");
    }

    #[tokio::test]
    async fn test_extract_provider_secrets() {
        let store = InMemorySecretStore::new();
        let extractor = SecretExtractor::new(&store, AppType::Claude);
        let config = json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": "sk-test-123",
                "ANTHROPIC_MODEL": "claude-3-opus"
            }
        });
        let (sanitized, secrets) = extractor
            .extract_provider_secrets("anthropic", &config)
            .await
            .unwrap();
        assert!(sanitized["env"].get("ANTHROPIC_AUTH_TOKEN").is_none());
        assert_eq!(sanitized["env"]["ANTHROPIC_MODEL"], "claude-3-opus");
        assert_eq!(secrets.api_key.as_ref().unwrap().as_str(), "sk-test-123");
    }

    #[tokio::test]
    async fn test_restore_provider_secrets() {
        let store = InMemorySecretStore::new();
        let extractor = SecretExtractor::new(&store, AppType::Claude);
        let config = json!({
            "env": { "ANTHROPIC_AUTH_TOKEN": "sk-test-456", "ANTHROPIC_MODEL": "claude-3-opus" }
        });
        let (sanitized, _) = extractor
            .extract_provider_secrets("anthropic", &config)
            .await
            .unwrap();
        let restored = extractor
            .restore_provider_secrets("anthropic", &sanitized)
            .await
            .unwrap();
        assert_eq!(restored["env"]["ANTHROPIC_AUTH_TOKEN"], "sk-test-456");
        assert_eq!(restored["env"]["ANTHROPIC_MODEL"], "claude-3-opus");
    }

    #[tokio::test]
    async fn test_extract_app_secrets() {
        let store = InMemorySecretStore::new();
        let extractor = SecretExtractor::new(&store, AppType::Claude);
        let mut config = HashMap::new();
        config.insert("MY_SECRET".to_string(), "token-abc-123".to_string());
        config.insert("user_name".to_string(), "alice".to_string());
        let sanitized = extractor.extract_app_secrets(&config).await.unwrap();
        assert!(!sanitized.contains_key("MY_SECRET"));
        assert_eq!(sanitized.get("user_name").unwrap(), "alice");
    }

    #[tokio::test]
    async fn test_extract_env_secrets() {
        let store = InMemorySecretStore::new();
        let extractor = SecretExtractor::new(&store, AppType::Claude);
        let config = json!({
            "env": {
                "OPENROUTER_API_KEY": "secret-token",
                "LOG_LEVEL": "debug"
            }
        });
        let (sanitized, secrets) = extractor
            .extract_provider_secrets("custom", &config)
            .await
            .unwrap();
        assert!(sanitized["env"].get("OPENROUTER_API_KEY").is_none());
        assert_eq!(sanitized["env"]["LOG_LEVEL"], "debug");
        assert_eq!(
            secrets.extra_env.get("OPENROUTER_API_KEY").unwrap().as_str(),
            "secret-token"
        );
    }
}
