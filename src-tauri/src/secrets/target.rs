use crate::app_config::AppType;

/// Naming scheme for Credential Manager targets (Appendix B)
/// Format: cc-switch/v1/provider/<app>/<provider_id>/<field>
///         cc-switch/v1/app/<app>/<field>
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretTarget {
    /// cc-switch/v1/provider/<app>/<provider_id>/api_key
    ProviderApiKey { app: AppType, provider_id: String },
    /// cc-switch/v1/provider/<app>/<provider_id>/base_url
    ProviderBaseUrl { app: AppType, provider_id: String },
    /// cc-switch/v1/provider/<app>/<provider_id>/env/<VAR_NAME>
    ProviderEnv {
        app: AppType,
        provider_id: String,
        var_name: String,
    },
    /// cc-switch/v1/app/<app>/<field>
    AppSecret { app: String, field: String },
    /// cc-switch/v1/probe (startup self-check)
    Probe,
}

impl SecretTarget {
    pub fn provider_api_key(app: AppType, provider_id: impl Into<String>) -> Self {
        Self::ProviderApiKey {
            app,
            provider_id: provider_id.into(),
        }
    }

    pub fn provider_base_url(app: AppType, provider_id: impl Into<String>) -> Self {
        Self::ProviderBaseUrl {
            app,
            provider_id: provider_id.into(),
        }
    }

    pub fn provider_env(
        app: AppType,
        provider_id: impl Into<String>,
        var_name: impl Into<String>,
    ) -> Self {
        Self::ProviderEnv {
            app,
            provider_id: provider_id.into(),
            var_name: var_name.into(),
        }
    }

    pub fn app_secret(app: AppType, field: impl Into<String>) -> Self {
        Self::AppSecret {
            app: app.as_str().to_string(),
            field: field.into(),
        }
    }

    pub fn app(app: impl Into<String>, field: impl Into<String>) -> Self {
        Self::AppSecret {
            app: app.into(),
            field: field.into(),
        }
    }

    pub fn probe() -> Self {
        Self::Probe
    }

    /// Convert to credential manager target string
    pub fn to_target_string(&self) -> String {
        match self {
            Self::ProviderApiKey { app, provider_id } => {
                format!("cc-switch/v1/provider/{}/{}/api_key", app.as_str(), provider_id)
            }
            Self::ProviderBaseUrl { app, provider_id } => {
                format!("cc-switch/v1/provider/{}/{}/base_url", app.as_str(), provider_id)
            }
            Self::ProviderEnv {
                app,
                provider_id,
                var_name,
            } => {
                format!(
                    "cc-switch/v1/provider/{}/{}/env/{}",
                    app.as_str(),
                    provider_id,
                    var_name
                )
            }
            Self::AppSecret { app, field } => {
                format!("cc-switch/v1/app/{}/{}", app, field)
            }
            Self::Probe => "cc-switch/v1/probe".to_string(),
        }
    }

    /// Get the "user" metadata (username field in credential manager)
    pub fn to_user_metadata(&self) -> String {
        match self {
            Self::ProviderApiKey { app, provider_id }
            | Self::ProviderBaseUrl { app, provider_id }
            | Self::ProviderEnv { app, provider_id, .. } => {
                format!("{}/{}", app.as_str(), provider_id)
            }
            Self::AppSecret { app, .. } => app.clone(),
            Self::Probe => "probe".to_string(),
        }
    }

    /// Get service name (always "cc-switch")
    pub fn service() -> &'static str {
        "cc-switch"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_api_key_target() {
        let target = SecretTarget::provider_api_key(AppType::Claude, "provider-123");
        assert_eq!(
            target.to_target_string(),
            "cc-switch/v1/provider/claude/provider-123/api_key"
        );
        assert_eq!(target.to_user_metadata(), "claude/provider-123");
    }

    #[test]
    fn test_provider_base_url_target() {
        let target = SecretTarget::provider_base_url(AppType::Codex, "custom-openai");
        assert_eq!(
            target.to_target_string(),
            "cc-switch/v1/provider/codex/custom-openai/base_url"
        );
    }

    #[test]
    fn test_provider_env_target() {
        let target = SecretTarget::provider_env(AppType::Claude, "provider-1", "OPENROUTER_API_KEY");
        assert_eq!(
            target.to_target_string(),
            "cc-switch/v1/provider/claude/provider-1/env/OPENROUTER_API_KEY"
        );
    }

    #[test]
    fn test_app_secret_target() {
        let target = SecretTarget::app("webdav", "password");
        assert_eq!(target.to_target_string(), "cc-switch/v1/app/webdav/password");
        assert_eq!(target.to_user_metadata(), "webdav");
    }

    #[test]
    fn test_probe_target() {
        let target = SecretTarget::probe();
        assert_eq!(target.to_target_string(), "cc-switch/v1/probe");
    }
}
