use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Provider struct - full internal representation with all fields including settings_config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    #[serde(rename = "settingsConfig")]
    pub settings_config: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "websiteUrl")]
    pub website_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "createdAt")]
    pub created_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "sortIndex")]
    pub sort_index: Option<usize>,
    /// 备注信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// 供应商元数据（不写入 live 配置，仅存于 ~/.cc-switch/config.json）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<ProviderMeta>,
    /// 图标名称（如 "openai", "anthropic"）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// 图标颜色（Hex 格式，如 "#00A67E"）
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "iconColor")]
    pub icon_color: Option<String>,
}

/// Sanitized provider for frontend consumption - excludes settings_config to prevent secret leakage
/// Per Phase 5 S1 requirement: IPC commands must not return api_key/password/secret/token fields
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderForFrontend {
    pub id: String,
    pub name: String,
    /// Indicates whether this provider has configuration (true if settings_config is non-empty)
    #[serde(rename = "hasConfig")]
    pub has_config: bool,
    /// 已剥离密钥的配置（可含模型列表、非敏感 env）
    #[serde(rename = "settingsConfig")]
    pub settings_config: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "websiteUrl")]
    pub website_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "createdAt")]
    pub created_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "sortIndex")]
    pub sort_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<ProviderMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "iconColor")]
    pub icon_color: Option<String>,
    #[serde(rename = "secretStatus", skip_serializing_if = "Option::is_none")]
    pub secret_status: Option<SecretStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SecretHint {
    pub present: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SecretStatus {
    pub api_key: SecretHint,
    pub base_url: Option<String>,
    pub extra_env: Vec<String>,
}

impl Provider {
    /// Convert to frontend-safe representation by stripping settings_config
    pub fn to_frontend(&self) -> ProviderForFrontend {
        ProviderForFrontend {
            id: self.id.clone(),
            name: self.name.clone(),
            has_config: !self.settings_config.is_null()
                && self
                    .settings_config
                    .as_object()
                    .is_some_and(|obj| !obj.is_empty()),
            settings_config: self.settings_config.clone(),
            website_url: self.website_url.clone(),
            category: self.category.clone(),
            created_at: self.created_at,
            sort_index: self.sort_index,
            notes: self.notes.clone(),
            meta: self.meta.clone(),
            icon: self.icon.clone(),
            icon_color: self.icon_color.clone(),
            secret_status: None,
        }
    }

    /// Create a new Provider with the given ID and default values
    pub fn with_id(id: String) -> Self {
        Self {
            id,
            name: String::new(),
            settings_config: Value::Object(serde_json::Map::new()),
            website_url: None,
            category: None,
            created_at: None,
            sort_index: None,
            notes: None,
            meta: None,
            icon: None,
            icon_color: None,
        }
    }

    /// Test/helper constructor used by integration tests.
    pub fn from_parts(
        id: String,
        name: String,
        settings_config: Value,
        website_url: Option<String>,
    ) -> Self {
        let mut provider = Self::with_id(id);
        provider.name = name;
        provider.settings_config = settings_config;
        provider.website_url = website_url;
        provider
    }
}

/// Provider manager - holds providers and current selection for an app type
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderManager {
    pub providers: indexmap::IndexMap<String, Provider>,
    pub current: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderMeta {
    /// Whether this provider was auto-imported from live config on first launch
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported_from_live: Option<bool>,
    /// Original app type if this was migrated from another app
    #[serde(skip_serializing_if = "Option::is_none")]
    pub migrated_from: Option<String>,
    /// API format for Claude providers (anthropic/openai_chat/openai_responses)
    #[serde(rename = "apiFormat", skip_serializing_if = "Option::is_none")]
    pub api_format: Option<String>,
    /// Whether common config is enabled
    #[serde(
        rename = "commonConfigEnabled",
        skip_serializing_if = "Option::is_none"
    )]
    pub common_config_enabled: Option<bool>,
    /// Whether live config is managed by cc-switch
    #[serde(rename = "liveConfigManaged", skip_serializing_if = "Option::is_none")]
    pub live_config_managed: Option<bool>,
    /// Claude: ANTHROPIC_AUTH_TOKEN or ANTHROPIC_API_KEY
    #[serde(rename = "apiKeyField", skip_serializing_if = "Option::is_none")]
    pub api_key_field: Option<String>,
}
