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
    /// 是否加入故障转移队列
    #[serde(default)]
    #[serde(rename = "inFailoverQueue")]
    pub in_failover_queue: bool,
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
    #[serde(default)]
    #[serde(rename = "inFailoverQueue")]
    pub in_failover_queue: bool,
}

impl Provider {
    /// Convert to frontend-safe representation by stripping settings_config
    pub fn to_frontend(&self) -> ProviderForFrontend {
        ProviderForFrontend {
            id: self.id.clone(),
            name: self.name.clone(),
            has_config: !self.settings_config.is_null()
                && self.settings_config.as_object().map_or(false, |obj| !obj.is_empty()),
            website_url: self.website_url.clone(),
            category: self.category.clone(),
            created_at: self.created_at,
            sort_index: self.sort_index,
            notes: self.notes.clone(),
            meta: self.meta.clone(),
            icon: self.icon.clone(),
            icon_color: self.icon_color.clone(),
            in_failover_queue: self.in_failover_queue,
        }
    }

    pub fn provider_type(&self) -> Option<&str> {
        self.settings_config
            .get("providerType")
            .and_then(|v| v.as_str())
    }

    pub fn is_codex_oauth(&self) -> bool {
        self.provider_type() == Some("codex_oauth")
    }

    pub fn is_xai_oauth(&self) -> bool {
        self.provider_type() == Some("xai_oauth")
    }

    /// Create a new Provider with the given ID and default values
    pub fn with_id(id: String) -> Self {
        Self {
            id,
            name: String::new(),
            settings_config: serde_json::Value::Object(serde_json::Map::new()),
            website_url: None,
            category: None,
            created_at: None,
            sort_index: None,
            notes: None,
            meta: None,
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        }
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
    /// Provider type identifier (e.g., "codex_oauth", "xai_oauth")
    #[serde(rename = "providerType", skip_serializing_if = "Option::is_none")]
    pub provider_type: Option<String>,
    /// Whether common config is enabled
    #[serde(rename = "commonConfigEnabled", skip_serializing_if = "Option::is_none")]
    pub common_config_enabled: Option<bool>,
    /// Whether live config is managed by cc-switch
    #[serde(rename = "liveConfigManaged", skip_serializing_if = "Option::is_none")]
    pub live_config_managed: Option<bool>,
}

impl ProviderMeta {
    /// Get managed account ID for a specific auth type (e.g., "codex_oauth", "github_copilot")
    pub fn managed_account_id_for(&self, _auth_type: &str) -> Option<String> {
        // This was removed in earlier refactoring - return None for now
        // The field no longer exists in ProviderMeta
        None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSortUpdate {
    pub id: String,
    #[serde(rename = "sortIndex")]
    pub sort_index: usize,
}
