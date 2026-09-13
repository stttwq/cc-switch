use crate::app_config::AppType;
use crate::error::AppError;
use crate::secrets::rules::is_sensitive_config_key;
use crate::secrets::store::SecretStore;
use crate::secrets::target::SecretTarget;
use crate::secrets::types::ProviderSecrets;
use serde_json::Value;
use std::collections::HashMap;

/// Extract secrets from provider config JSON and store them in SecretStore
pub struct SecretExtractor<'a> {
    store: &'a dyn SecretStore,
    app: AppType,
}

impl<'a> SecretExtractor<'a> {
    pub fn new(store: &'a dyn SecretStore, app: AppType) -> Self {
        Self { store, app }
    }

    /// Extract secrets from a provider config object and return a sanitized version
    ///
    /// This function:
    /// 1. Scans the provider config for sensitive fields (api_key, auth, token, etc.)
    /// 2. Stores them in SecretStore with appropriate targets
    /// 3. Replaces sensitive values with `literal:***` markers
    /// 4. Returns the sanitized config
    pub async fn extract_provider_secrets(
        &self,
        provider_id: &str,
        config: &Value,
    ) -> Result<(Value, ProviderSecrets), AppError> {
        let mut sanitized = config.clone();
        let mut secrets = ProviderSecrets::new();

        if let Value::Object(map) = config {
            // Scan all top-level fields for sensitive keys
            for (key, value) in map {
                if let Value::String(val) = value {
                    if !val.is_empty() && !val.starts_with("literal:") && is_sensitive_config_key(key) {
                        // Determine the appropriate target based on the field name
                        let target = if key == "api_key" || key.to_lowercase().contains("apikey") {
                            SecretTarget::provider_api_key(self.app.clone(), provider_id.to_string())
                        } else if key == "base_url" || key == "baseUrl" {
                            // Only store base_url as secret if it contains credentials
                            if val.contains('@') || val.contains("token=") {
                                SecretTarget::provider_base_url(self.app.clone(), provider_id.to_string())
                            } else {
                                continue; // Skip non-credential base URLs
                            }
                        } else {
                            // Other sensitive fields go into extra_env
                            SecretTarget::provider_env(
                                self.app.clone(),
                                provider_id.to_string(),
                                key.clone(),
                            )
                        };

                        self.store.store(&target, val).await?;

                        // Record in secrets
                        if key == "api_key" || key.to_lowercase().contains("apikey") {
                            secrets = secrets.with_api_key(val.clone());
                        } else if key == "base_url" || key == "baseUrl" {
                            secrets = secrets.with_base_url(val.clone());
                        } else {
                            secrets = secrets.with_extra_env(key.clone(), val.clone());
                        }

                        // Replace with marker
                        if let Value::Object(obj) = &mut sanitized {
                            obj.insert(key.clone(), Value::String("literal:***".to_string()));
                        }
                    }
                }
            }

            // Extract environment variables with sensitive keys
            if let Some(Value::Object(env_map)) = map.get("env") {
                for (key, value) in env_map {
                    if let Value::String(val) = value {
                        if is_sensitive_config_key(key) && !val.starts_with("literal:") {
                            let target = SecretTarget::provider_env(
                                self.app.clone(),
                                provider_id.to_string(),
                                key.clone(),
                            );
                            self.store.store(&target, val).await?;
                            secrets = secrets.with_extra_env(key.clone(), val.clone());

                            // Replace with marker
                            if let Value::Object(obj) = &mut sanitized {
                                if let Some(Value::Object(env_obj)) = obj.get_mut("env") {
                                    env_obj.insert(
                                        key.clone(),
                                        Value::String("literal:***".to_string()),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok((sanitized, secrets))
    }

    /// Restore secrets from SecretStore into a config object
    ///
    /// This function:
    /// 1. Reads the sanitized config with `literal:***` markers
    /// 2. Retrieves actual secrets from SecretStore
    /// 3. Replaces markers with real values
    /// 4. Returns the hydrated config
    pub async fn restore_provider_secrets(
        &self,
        provider_id: &str,
        sanitized_config: &Value,
    ) -> Result<Value, AppError> {
        let mut restored = sanitized_config.clone();

        if let Value::Object(map) = sanitized_config {
            // Restore api_key
            if let Some(Value::String(marker)) = map.get("api_key") {
                if marker.starts_with("literal:") {
                    let target =
                        SecretTarget::provider_api_key(self.app.clone(), provider_id.to_string());
                    if let Some(api_key) = self.store.retrieve(&target).await? {
                        if let Value::Object(obj) = &mut restored {
                            obj.insert("api_key".to_string(), Value::String(api_key));
                        }
                    }
                }
            }

            // Restore base_url
            if let Some(Value::String(marker)) = map.get("base_url") {
                if marker.starts_with("literal:") {
                    let target =
                        SecretTarget::provider_base_url(self.app.clone(), provider_id.to_string());
                    if let Some(base_url) = self.store.retrieve(&target).await? {
                        if let Value::Object(obj) = &mut restored {
                            obj.insert("base_url".to_string(), Value::String(base_url));
                        }
                    }
                }
            }

            // Restore environment variables
            if let Some(Value::Object(env_map)) = map.get("env") {
                for (key, value) in env_map {
                    if let Value::String(marker) = value {
                        if marker.starts_with("literal:") {
                            let target = SecretTarget::provider_env(
                                self.app.clone(),
                                provider_id.to_string(),
                                key.clone(),
                            );
                            if let Some(val) = self.store.retrieve(&target).await? {
                                if let Value::Object(obj) = &mut restored {
                                    if let Some(Value::Object(env_obj)) = obj.get_mut("env") {
                                        env_obj.insert(key.clone(), Value::String(val));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(restored)
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
                sanitized.insert(key.clone(), "literal:***".to_string());
            }
        }

        Ok(sanitized)
    }

    /// Restore app-level secrets from SecretStore
    pub async fn restore_app_secrets(
        &self,
        sanitized_config: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>, AppError> {
        let mut restored = sanitized_config.clone();

        for (key, value) in sanitized_config {
            if value.starts_with("literal:") && is_sensitive_config_key(key) {
                let target = SecretTarget::app(self.app.as_str(), key.clone());
                if let Some(secret) = self.store.retrieve(&target).await? {
                    restored.insert(key.clone(), secret);
                }
            }
        }

        Ok(restored)
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
    pub async fn restore_s3_credentials(&self) -> Result<(Option<String>, Option<String>), AppError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::store::InMemorySecretStore;
    use serde_json::json;

    #[tokio::test]
    async fn test_extract_provider_secrets() {
        let store = InMemorySecretStore::new();
        let extractor = SecretExtractor::new(&store, AppType::Claude);

        let config = json!({
            "api_key": "sk-test-123",
            "base_url": "https://api.example.com",
            "model": "claude-3-opus"
        });

        let (sanitized, secrets) =
            extractor.extract_provider_secrets("anthropic", &config).await.unwrap();

        // Check sanitized config has marker
        assert_eq!(sanitized["api_key"], "literal:***");
        assert_eq!(sanitized["model"], "claude-3-opus");

        // Check secrets were extracted
        assert_eq!(secrets.api_key.as_ref().unwrap().as_str(), "sk-test-123");
    }

    #[tokio::test]
    async fn test_restore_provider_secrets() {
        let store = InMemorySecretStore::new();
        let extractor = SecretExtractor::new(&store, AppType::Claude);

        let config = json!({
            "api_key": "sk-test-456",
            "model": "claude-3-opus"
        });

        // First extract
        let (sanitized, _) = extractor.extract_provider_secrets("anthropic", &config).await.unwrap();
        assert_eq!(sanitized["api_key"], "literal:***");

        // Then restore
        let restored = extractor.restore_provider_secrets("anthropic", &sanitized).await.unwrap();
        assert_eq!(restored["api_key"], "sk-test-456");
        assert_eq!(restored["model"], "claude-3-opus");
    }

    #[tokio::test]
    async fn test_extract_app_secrets() {
        let store = InMemorySecretStore::new();
        let extractor = SecretExtractor::new(&store, AppType::Claude);

        let mut config = HashMap::new();
        config.insert("bearer_token".to_string(), "token-abc-123".to_string());
        config.insert("user_name".to_string(), "alice".to_string());

        let sanitized = extractor.extract_app_secrets(&config).await.unwrap();

        assert_eq!(sanitized.get("bearer_token").unwrap(), "literal:***");
        assert_eq!(sanitized.get("user_name").unwrap(), "alice");
    }

    #[tokio::test]
    async fn test_restore_app_secrets() {
        let store = InMemorySecretStore::new();
        let extractor = SecretExtractor::new(&store, AppType::Claude);

        let mut config = HashMap::new();
        config.insert("api_key".to_string(), "key-xyz-789".to_string());

        // Extract
        let sanitized = extractor.extract_app_secrets(&config).await.unwrap();
        assert_eq!(sanitized.get("api_key").unwrap(), "literal:***");

        // Restore
        let restored = extractor.restore_app_secrets(&sanitized).await.unwrap();
        assert_eq!(restored.get("api_key").unwrap(), "key-xyz-789");
    }

    #[tokio::test]
    async fn test_extract_env_secrets() {
        let store = InMemorySecretStore::new();
        let extractor = SecretExtractor::new(&store, AppType::Claude);

        let config = json!({
            "env": {
                "API_TOKEN": "secret-token",
                "LOG_LEVEL": "debug"
            }
        });

        let (sanitized, secrets) =
            extractor.extract_provider_secrets("custom", &config).await.unwrap();

        // Check sanitized
        assert_eq!(sanitized["env"]["API_TOKEN"], "literal:***");
        assert_eq!(sanitized["env"]["LOG_LEVEL"], "debug");

        // Check secrets
        assert!(secrets.extra_env.contains_key("API_TOKEN"));
        assert_eq!(secrets.extra_env.get("API_TOKEN").unwrap().as_str(), "secret-token");
    }
}
