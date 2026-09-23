use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

use crate::app_config::AppType;
use crate::error::AppError;
use crate::services::skill::{SkillStorageLocation, SyncMethod};

/// 日志配置
///
/// 存储在 settings 表的 log_config 字段中（JSON 格式）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogConfig {
    /// 总开关：是否启用日志
    #[serde(default = "log_config_default_enabled")]
    pub enabled: bool,
    /// 日志级别: error, warn, info, debug, trace
    #[serde(default = "log_config_default_level")]
    pub level: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            level: "info".to_string(),
        }
    }
}

impl LogConfig {
    /// 将配置转换为 log::LevelFilter
    pub fn to_level_filter(&self) -> log::LevelFilter {
        if !self.enabled {
            return log::LevelFilter::Off;
        }
        match self.level.to_lowercase().as_str() {
            "error" => log::LevelFilter::Error,
            "warn" => log::LevelFilter::Warn,
            "info" => log::LevelFilter::Info,
            "debug" => log::LevelFilter::Debug,
            "trace" => log::LevelFilter::Trace,
            _ => log::LevelFilter::Info,
        }
    }
}

fn log_config_default_enabled() -> bool {
    true
}

fn log_config_default_level() -> String {
    "info".to_string()
}

fn default_true() -> bool {
    true
}

/// 主页面显示的应用配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisibleApps {
    #[serde(default = "default_true")]
    pub claude: bool,
    #[serde(default = "default_true")]
    pub codex: bool,
    #[serde(default = "default_true")]
    pub pi: bool,
}

impl Default for VisibleApps {
    fn default() -> Self {
        Self {
            claude: true,
            codex: true,
            pi: true,
        }
    }
}

impl VisibleApps {
    /// Check if the specified app is visible
    pub fn is_visible(&self, app: &AppType) -> bool {
        match app {
            AppType::Claude => self.claude,
            AppType::Codex => self.codex,
            AppType::Pi => self.pi,
        }
    }
}

/// WebDAV 同步状态（持久化同步进度信息）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WebDavSyncStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_remote_etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_local_manifest_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_remote_manifest_hash: Option<String>,
    /// E2E（方案 2.4.4）：本机已应用的远端快照序号，用于回滚检测。设备级、不随库同步。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_applied_seq: Option<u64>,
    /// E2E：本机最后一次成功上传的序号，用于计算 `max(本地, 远端) + 1`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_uploaded_seq: Option<u64>,
}

fn default_remote_root() -> String {
    "cc-switch-sync".to_string()
}
fn default_profile() -> String {
    "default".to_string()
}

/// WebDAV 同步设置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavSyncSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub auto_sync: bool,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub username: String,
    // password moved to SecretStore: SecretTarget::app("webdav", "password")
    #[serde(default = "default_remote_root")]
    pub remote_root: String,
    #[serde(default = "default_profile")]
    pub profile: String,
    /// E2E（方案 2.4.6 + B1）：是否对该远端启用端到端加密。默认关，
    /// 已有 v2 明文远端的用户须手动迁移；新远端的"默认开启"在启用流程里处理。
    #[serde(default)]
    pub e2e_enabled: bool,
    /// E2E（方案 2.4.5）：允许 http / 私网非加密传输的显式豁免，默认拒。
    #[serde(default)]
    pub allow_insecure: bool,
    #[serde(default)]
    pub status: WebDavSyncStatus,
}

impl Default for WebDavSyncSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_sync: false,
            base_url: String::new(),
            username: String::new(),
            remote_root: default_remote_root(),
            profile: default_profile(),
            e2e_enabled: false,
            allow_insecure: false,
            status: WebDavSyncStatus::default(),
        }
    }
}

impl WebDavSyncSettings {
    pub fn validate(&self) -> Result<(), crate::error::AppError> {
        if self.base_url.trim().is_empty() {
            return Err(crate::error::AppError::localized(
                "webdav.base_url.required",
                "WebDAV 地址不能为空",
                "WebDAV URL is required.",
            ));
        }
        if self.username.trim().is_empty() {
            return Err(crate::error::AppError::localized(
                "webdav.username.required",
                "WebDAV 用户名不能为空",
                "WebDAV username is required.",
            ));
        }
        // E2E-5（方案 2.4.5）：http 明文默认拒，除非本机/私网且已勾选允许不安全连接。
        crate::services::sync_protocol::ensure_transport_endpoint_secure(
            &self.base_url,
            self.allow_insecure,
        )?;
        Ok(())
    }

    pub fn normalize(&mut self) {
        self.base_url = self.base_url.trim().to_string();
        self.username = self.username.trim().to_string();
        self.remote_root = self.remote_root.trim().to_string();
        self.profile = self.profile.trim().to_string();
        if self.remote_root.is_empty() {
            self.remote_root = default_remote_root();
        }
        if self.profile.is_empty() {
            self.profile = default_profile();
        }
    }

    /// Returns true if all credential fields are blank (no config to persist).
    fn is_empty(&self) -> bool {
        self.base_url.is_empty() && self.username.is_empty()
    }
}

/// S3 同步设置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct S3SyncSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub auto_sync: bool,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub bucket: String,
    // access_key_id and secret_access_key moved to SecretStore:
    // SecretTarget::app("s3", "access_key_id")
    // SecretTarget::app("s3", "secret_access_key")
    #[serde(default)]
    pub endpoint: String,
    #[serde(default = "default_remote_root")]
    pub remote_root: String,
    #[serde(default = "default_profile")]
    pub profile: String,
    /// E2E：见 `WebDavSyncSettings::e2e_enabled`（两个传输各自独立的设备级开关）。
    #[serde(default)]
    pub e2e_enabled: bool,
    /// E2E：见 `WebDavSyncSettings::allow_insecure`。
    #[serde(default)]
    pub allow_insecure: bool,
    #[serde(default)]
    pub status: WebDavSyncStatus,
}

impl Default for S3SyncSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_sync: false,
            region: String::new(),
            bucket: String::new(),
            endpoint: String::new(),
            remote_root: default_remote_root(),
            profile: default_profile(),
            e2e_enabled: false,
            allow_insecure: false,
            status: WebDavSyncStatus::default(),
        }
    }
}

impl S3SyncSettings {
    pub fn validate(&self) -> Result<(), crate::error::AppError> {
        if self.bucket.trim().is_empty() {
            return Err(crate::error::AppError::localized(
                "s3.bucket.required",
                "S3 存储桶不能为空",
                "S3 bucket is required.",
            ));
        }
        if self.region.trim().is_empty() {
            return Err(crate::error::AppError::localized(
                "s3.region.required",
                "S3 区域不能为空",
                "S3 region is required.",
            ));
        }
        // Note: access_key_id and secret_access_key validation removed
        // as they are now stored in SecretStore, not in this struct
        // E2E-5（方案 2.4.5）：自定义 endpoint 走 http 同样默认拒（空 endpoint = AWS 默认 https）。
        crate::services::sync_protocol::ensure_transport_endpoint_secure(
            &self.endpoint,
            self.allow_insecure,
        )?;
        Ok(())
    }

    pub fn normalize(&mut self) {
        self.region = self.region.trim().to_string();
        self.bucket = self.bucket.trim().to_string();
        self.endpoint = self.endpoint.trim().to_string();
        self.remote_root = self.remote_root.trim().to_string();
        self.profile = self.profile.trim().to_string();
        if self.remote_root.is_empty() {
            self.remote_root = default_remote_root();
        }
        if self.profile.is_empty() {
            self.profile = default_profile();
        }
    }

    /// Returns true if all credential fields are blank (no config to persist).
    fn is_empty(&self) -> bool {
        self.bucket.is_empty() && self.region.is_empty()
    }
}

/// 本机自动迁移状态。
///
/// 这里记录的是本机启动时执行过的一次性迁移；标记不随数据库同步。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LocalMigrations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_third_party_history_provider_bucket_v1:
        Option<CodexThirdPartyHistoryProviderBucketMigration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_provider_template_v1: Option<CodexProviderTemplateMigration>,
    /// 统一会话开关的官方历史迁移标记。开关关闭时会被清除，
    /// 这样重新开启能把"关闭期间"落入 openai 桶的官方会话补迁进来。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_official_history_unify_v1: Option<CodexOfficialHistoryUnifyMigration>,
    /// E2E-5 升级豁免：本版本起 http 端点默认拒绝。升级前就已配置 http 远端的存量用户
    /// 一次性预置 `allow_insecure=true`，避免同步被悄悄打断。置真后不再重复执行，
    /// 用户之后手动取消勾选也不会被再次豁免。
    #[serde(default)]
    pub http_insecure_grandfathered_v1: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexThirdPartyHistoryProviderBucketMigration {
    pub completed_at: String,
    pub target_provider_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_provider_ids: Vec<String>,
    #[serde(default)]
    pub migrated_jsonl_files: usize,
    #[serde(default)]
    pub migrated_state_rows: usize,
    #[serde(default)]
    pub scanned_history_files: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexProviderTemplateMigration {
    pub completed_at: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub migrated_provider_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexOfficialHistoryUnifyMigration {
    pub completed_at: String,
    pub target_provider_id: String,
    #[serde(default)]
    pub migrated_jsonl_files: usize,
    #[serde(default)]
    pub migrated_state_rows: usize,
    /// 迁移时的规范化 Codex 目录。标记只对同一目录生效：
    /// 切换 codex_config_dir 后旧标记不会挡住新目录的迁移。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_config_dir: Option<String>,
}

/// 应用设置结构
///
/// 存储设备级别设置，保存在本地 `~/.cc-switch/settings.json`，不随数据库同步。
/// 这确保了云同步场景下多设备可以独立运作。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    // ===== 设备级 UI 设置 =====
    #[serde(default = "default_show_in_tray")]
    pub show_in_tray: bool,
    #[serde(default = "default_minimize_to_tray_on_close")]
    pub minimize_to_tray_on_close: bool,
    #[serde(default)]
    pub use_app_window_controls: bool,
    /// 是否启用 Claude 插件联动
    #[serde(default)]
    pub enable_claude_plugin_integration: bool,
    /// 是否跳过 Claude Code 初次安装确认
    #[serde(default)]
    pub skip_claude_onboarding: bool,
    /// 是否开机自启
    #[serde(default)]
    pub launch_on_startup: bool,
    /// 静默启动（程序启动时不显示主窗口，仅托盘运行）
    #[serde(default)]
    pub silent_startup: bool,
    /// User has confirmed the common config first-run notice
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub common_config_confirmed: Option<bool>,
    /// User has confirmed the first-run welcome notice
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_run_notice_confirmed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,

    // ===== 主页面显示的应用 =====
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_apps: Option<VisibleApps>,

    // ===== 设备级目录覆盖 =====
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_config_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_config_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pi_config_dir: Option<String>,

    // ===== 当前供应商 ID（设备级）=====
    /// 当前 Claude 供应商 ID（本地存储，优先于数据库 is_current）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_provider_claude: Option<String>,
    /// 当前 Codex 供应商 ID（本地存储，优先于数据库 is_current）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_provider_codex: Option<String>,

    // ===== Skill 同步设置 =====
    /// Skill 同步方式：auto（默认，优先 symlink）、symlink、copy
    #[serde(default)]
    pub skill_sync_method: SyncMethod,
    /// Skill 存储位置：cc_switch（默认）或 unified（~/.agents/skills/）
    #[serde(default)]
    pub skill_storage_location: SkillStorageLocation,

    // ===== WebDAV 同步设置 =====
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webdav_sync: Option<WebDavSyncSettings>,

    // ===== S3 同步设置 =====
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3_sync: Option<S3SyncSettings>,

    // ===== 备份策略设置 =====
    /// Auto-backup interval in hours (default 24, 0 = disabled)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_interval_hours: Option<u32>,
    /// Maximum number of backup files to retain (default 10)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_retain_count: Option<u32>,

    // ===== 终端设置 =====
    /// 首选终端应用（可选，默认使用系统默认终端）
    /// - Windows: "cmd" | "powershell" | "wt" (Windows Terminal)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_terminal: Option<String>,
    /// 自定义终端可执行文件路径（preferred_terminal == "custom" 时生效）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_terminal_custom_path: Option<String>,
    /// 自定义终端启动参数模板，`{bat}` 占位符会被替换为启动批处理路径。
    /// 为空时默认 `-e cmd /K "{bat}"`（Pebrel/WezTerm/Alacritty 的 `-e` 均为可变参数，需逐参传递）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_terminal_custom_args: Option<String>,

    // ===== 环境变量投递（B5 严格模式，方案 2.4.7）=====
    /// 严格投递模式：开启后切换供应商不再把密钥写入 `HKCU\Environment`，只更新 live 文件；
    /// 密钥仅经 cc-switch「打开终端」注入到其自起的终端进程。从别处启动的 CLI 拿不到密钥
    /// （Codex `env_key` 缺失、Pi 变量未解析，均 fail-closed）。默认关。
    #[serde(default)]
    pub env_delivery_strict_mode: bool,

    /// 2.2 方案 P2：严格模式分级——"按应用"时列出的严格 app 集合（`["claude","codex","pi"]` 子集）。
    /// 叠加字段，不删旧的 `env_delivery_strict_mode`，老 settings.json 缺该字段照常反序列化（默认 None）。
    /// 不变量：`env_delivery_strict_mode == true`（全局）与 `strict_apps 非空` 不得同时成立，
    /// 由 `normalize_strict_mode()` 在所有写路径强制。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_delivery_strict_apps: Option<Vec<String>>,

    // ===== 本机自动迁移状态 =====
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_migrations: Option<LocalMigrations>,

    // ===== Codex session history unification (Phase 2A preserves existing fields) =====
    /// Run official Codex providers under the shared "custom" model_provider id
    /// so official sessions share one resume-history bucket with third-party
    /// providers. Opt-in: defaults to false.
    #[serde(default)]
    pub unify_codex_session_history: bool,
    /// User opted in (via the enable dialog checkbox) to migrate existing
    /// official sessions ("openai" bucket) into the shared bucket. Persisted so
    /// a failed migration retries at startup; cleared when the toggle turns off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unify_codex_migrate_existing: Option<bool>,
    /// Whether to show the project profile switcher on the main page header
    #[serde(default = "default_show_profile_switcher")]
    pub show_profile_switcher: bool,
}

fn default_show_in_tray() -> bool {
    true
}

fn default_minimize_to_tray_on_close() -> bool {
    true
}

fn default_show_profile_switcher() -> bool {
    true
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            show_in_tray: true,
            minimize_to_tray_on_close: true,
            use_app_window_controls: false,
            enable_claude_plugin_integration: false,
            skip_claude_onboarding: false,
            launch_on_startup: false,
            silent_startup: false,
            show_profile_switcher: true,
            unify_codex_session_history: false,
            unify_codex_migrate_existing: None,
            first_run_notice_confirmed: None,
            common_config_confirmed: None,
            language: None,
            visible_apps: None,
            claude_config_dir: None,
            codex_config_dir: None,
            pi_config_dir: None,
            current_provider_claude: None,
            current_provider_codex: None,
            skill_sync_method: SyncMethod::default(),
            skill_storage_location: SkillStorageLocation::default(),
            webdav_sync: None,
            s3_sync: None,
            backup_interval_hours: None,
            backup_retain_count: None,
            preferred_terminal: None,
            preferred_terminal_custom_path: None,
            preferred_terminal_custom_args: None,
            env_delivery_strict_mode: false,
            env_delivery_strict_apps: None,
            local_migrations: None,
        }
    }
}

impl AppSettings {
    fn settings_path() -> Option<PathBuf> {
        // settings.json 保留用于旧版本迁移和无数据库场景
        Some(
            crate::config::get_home_dir()
                .join(".cc-switch")
                .join("settings.json"),
        )
    }

    fn normalize_paths(&mut self) {
        self.claude_config_dir = self
            .claude_config_dir
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        self.codex_config_dir = self
            .codex_config_dir
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        self.pi_config_dir = self
            .pi_config_dir
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        self.language = self
            .language
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| matches!(*s, "en" | "zh" | "zh-TW" | "ja"))
            .map(|s| s.to_string());

        if let Some(sync) = &mut self.webdav_sync {
            sync.normalize();
            if sync.is_empty() {
                self.webdav_sync = None;
            }
        }

        if let Some(s3) = &mut self.s3_sync {
            s3.normalize();
            if s3.is_empty() {
                self.s3_sync = None;
            }
        }

        // 三态互斥归一化随路径归一化一起在**全路径必经点**执行。
        self.normalize_strict_mode();
    }

    /// 强制严格模式两字段的不变量：`env_delivery_strict_mode == true`（全局）与
    /// `env_delivery_strict_apps 非空` 不得同时成立。由 `normalize_paths()` 在所有读写
    /// 路径（load/update/mutate/save）统一调用，杜绝脏状态与前端三态回显歧义。
    fn normalize_strict_mode(&mut self) {
        if self.env_delivery_strict_mode {
            // 全局严格：清空"按应用"列表。
            self.env_delivery_strict_apps = None;
            return;
        }
        // 非全局：仅保留合法 app 名，去重排序；空则归 None。
        let cleaned: Vec<String> = self
            .env_delivery_strict_apps
            .take()
            .map(|apps| {
                let mut set: Vec<String> = apps
                    .into_iter()
                    .map(|a| a.trim().to_lowercase())
                    .filter(|a| matches!(a.as_str(), "claude" | "codex" | "pi"))
                    .collect();
                set.sort();
                set.dedup();
                set
            })
            .unwrap_or_default();
        self.env_delivery_strict_apps = if cleaned.is_empty() {
            None
        } else {
            Some(cleaned)
        };
    }

    /// 某 app 是否严格（纯函数，便于单测）。全局开→全严格；否则查按应用列表。
    fn is_strict_for(&self, app: &AppType) -> bool {
        if self.env_delivery_strict_mode {
            return true;
        }
        self.env_delivery_strict_apps
            .as_deref()
            .is_some_and(|apps| apps.iter().any(|a| a == app.as_str()))
    }

    fn load_from_file() -> Self {
        let Some(path) = Self::settings_path() else {
            return Self::default();
        };
        if let Ok(content) = fs::read_to_string(&path) {
            match serde_json::from_str::<AppSettings>(&content) {
                Ok(mut settings) => {
                    settings.normalize_paths();
                    settings
                }
                Err(err) => {
                    log::warn!(
                        "解析设置文件失败，将使用默认设置。路径: {}, 错误: {}",
                        path.display(),
                        err
                    );
                    Self::default()
                }
            }
        } else {
            Self::default()
        }
    }
}

fn save_settings_file(settings: &AppSettings) -> Result<(), AppError> {
    let mut normalized = settings.clone();
    normalized.normalize_paths();
    let Some(path) = AppSettings::settings_path() else {
        return Err(AppError::Config("无法获取用户主目录".to_string()));
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
    }

    let json = serde_json::to_string_pretty(&normalized)
        .map_err(|e| AppError::JsonSerialize { source: e })?;
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| AppError::io(&path, e))?;
        file.write_all(json.as_bytes())
            .map_err(|e| AppError::io(&path, e))?;
    }

    #[cfg(not(unix))]
    {
        fs::write(&path, json).map_err(|e| AppError::io(&path, e))?;
    }

    Ok(())
}

static SETTINGS_STORE: OnceLock<RwLock<AppSettings>> = OnceLock::new();

fn settings_store() -> &'static RwLock<AppSettings> {
    SETTINGS_STORE.get_or_init(|| RwLock::new(AppSettings::load_from_file()))
}

pub(crate) fn resolve_override_path(raw: &str) -> PathBuf {
    let join_home = |home: PathBuf, suffix: &str| {
        suffix
            .split(['/', '\\'])
            .filter(|component| !component.is_empty())
            .fold(home, |path, component| path.join(component))
    };

    if raw == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    } else if let Some(stripped) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return join_home(home, stripped);
        }
    } else if let Some(stripped) = raw.strip_prefix("~\\") {
        if let Some(home) = dirs::home_dir() {
            return join_home(home, stripped);
        }
    }

    PathBuf::from(raw)
}

pub fn get_settings() -> AppSettings {
    settings_store()
        .read()
        .unwrap_or_else(|e| {
            log::warn!("设置锁已毒化，使用恢复值: {e}");
            e.into_inner()
        })
        .clone()
}

pub fn get_settings_for_frontend() -> AppSettings {
    // All secrets now stored in SecretStore, no need to sanitize
    get_settings()
}

pub fn update_settings(mut new_settings: AppSettings) -> Result<(), AppError> {
    new_settings.normalize_paths();
    save_settings_file(&new_settings)?;

    let mut guard = settings_store().write().unwrap_or_else(|e| {
        log::warn!("设置锁已毒化，使用恢复值: {e}");
        e.into_inner()
    });
    *guard = new_settings;
    Ok(())
}

fn mutate_settings<F>(mutator: F) -> Result<(), AppError>
where
    F: FnOnce(&mut AppSettings),
{
    let mut guard = settings_store().write().unwrap_or_else(|e| {
        log::warn!("设置锁已毒化，使用恢复值: {e}");
        e.into_inner()
    });
    let mut next = guard.clone();
    mutator(&mut next);
    next.normalize_paths();
    save_settings_file(&next)?;
    *guard = next;
    Ok(())
}

/// 严格投递模式是否**以任何形态**开启（全局，或存在至少一个按应用严格）。
/// 供诊断包/托盘判断"当前是否存在严格语义"，不可再直接读裸 bool。
pub fn env_delivery_strict_mode_enabled() -> bool {
    let s = get_settings();
    s.env_delivery_strict_mode || s.env_delivery_strict_apps.is_some_and(|a| !a.is_empty())
}

/// 2.2 方案 P2：某 app 的有效严格性。全局开→所有 app 严格（兼容老语义）；
/// 否则看该 app 是否在"按应用"列表里。投递/预检/清理按此逐 app 决策。
pub fn strict_for(app: &AppType) -> bool {
    get_settings().is_strict_for(app)
}

/// 当前"按应用"严格列表（已归一化）；全局严格时返回全部 app（供诊断展示）。
pub fn strict_apps_for_display() -> Vec<String> {
    let s = get_settings();
    if s.env_delivery_strict_mode {
        return ["claude", "codex", "pi"].into_iter().map(String::from).collect();
    }
    s.env_delivery_strict_apps.unwrap_or_default()
}

/// 是否处于"全局严格"档（区别于"按应用"）。
pub fn is_global_strict_mode() -> bool {
    get_settings().env_delivery_strict_mode
}

/// 置严格投递模式总开关。true=全局严格；false=完全关闭（同时清空"按应用"列表，
/// 避免残留导致回显歧义）。与按应用列表的互斥由归一化在写路径强制。
pub fn set_env_delivery_strict_mode(enabled: bool) -> Result<(), AppError> {
    mutate_settings(|settings| {
        settings.env_delivery_strict_mode = enabled;
        if !enabled {
            settings.env_delivery_strict_apps = None;
        }
    })
}

/// 置"按应用"严格集合（P2 分级）。写该列表时强制全局 bool=false，由归一化维持互斥。
pub fn set_env_delivery_strict_apps(apps: Vec<String>) -> Result<(), AppError> {
    mutate_settings(|settings| {
        settings.env_delivery_strict_mode = false;
        settings.env_delivery_strict_apps = Some(apps);
    })
}

pub fn is_codex_third_party_history_provider_bucket_migrated() -> bool {
    get_settings()
        .local_migrations
        .as_ref()
        .and_then(|migrations| {
            migrations
                .codex_third_party_history_provider_bucket_v1
                .as_ref()
        })
        .is_some_and(|m| m.scanned_history_files)
}

pub fn mark_codex_third_party_history_provider_bucket_migrated(
    migration: CodexThirdPartyHistoryProviderBucketMigration,
) -> Result<(), AppError> {
    mutate_settings(|settings| {
        let migrations = settings
            .local_migrations
            .get_or_insert_with(Default::default);
        migrations.codex_third_party_history_provider_bucket_v1 = Some(migration);
    })
}

pub fn is_codex_provider_template_migrated() -> bool {
    get_settings()
        .local_migrations
        .as_ref()
        .and_then(|migrations| migrations.codex_provider_template_v1.as_ref())
        .is_some()
}

pub fn mark_codex_provider_template_migrated(
    migration: CodexProviderTemplateMigration,
) -> Result<(), AppError> {
    mutate_settings(|settings| {
        let migrations = settings
            .local_migrations
            .get_or_insert_with(Default::default);
        migrations.codex_provider_template_v1 = Some(migration);
    })
}

/// 统一会话迁移标记是否覆盖指定目录。标记里没记目录（不应出现的旧格式）
/// 视为不匹配——重跑迁移是幂等的，宁可重迁也不漏迁。
pub fn is_codex_official_history_unify_migrated_for_dir(codex_dir: &str) -> bool {
    get_settings()
        .local_migrations
        .as_ref()
        .and_then(|migrations| migrations.codex_official_history_unify_v1.as_ref())
        .is_some_and(|migration| migration.codex_config_dir.as_deref() == Some(codex_dir))
}

/// 条件写入迁移完成标记：仅当此刻开关仍开启且迁移意愿仍在时才写。
/// 检查与写入在 settings 写锁内原子完成，与关闭开关路径
/// （`update_settings` / 清标记）串行，消除"迁移线程复查开关后、写标记前
/// 用户恰好关闭开关"的竞态窗口。返回是否实际写入。
pub fn mark_codex_official_history_unify_migrated_if_enabled(
    migration: CodexOfficialHistoryUnifyMigration,
) -> Result<bool, AppError> {
    let mut written = false;
    mutate_settings(|settings| {
        if settings.unify_codex_session_history
            && settings.unify_codex_migrate_existing.unwrap_or(false)
        {
            settings
                .local_migrations
                .get_or_insert_with(Default::default)
                .codex_official_history_unify_v1 = Some(migration);
            written = true;
        }
    })?;
    Ok(written)
}

pub fn clear_codex_official_history_unify_migration() -> Result<(), AppError> {
    mutate_settings(|settings| {
        if let Some(migrations) = settings.local_migrations.as_mut() {
            migrations.codex_official_history_unify_v1 = None;
        }
    })
}

pub fn unify_codex_migrate_existing_requested() -> bool {
    get_settings().unify_codex_migrate_existing.unwrap_or(false)
}

pub fn clear_codex_unify_migrate_existing() -> Result<(), AppError> {
    mutate_settings(|settings| {
        settings.unify_codex_migrate_existing = None;
    })
}

/// 从文件重新加载设置到内存缓存
/// 用于导入配置等场景，确保内存缓存与文件同步
pub fn reload_settings() -> Result<(), AppError> {
    let fresh_settings = AppSettings::load_from_file();
    let mut guard = settings_store().write().unwrap_or_else(|e| {
        log::warn!("设置锁已毒化，使用恢复值: {e}");
        e.into_inner()
    });
    *guard = fresh_settings;
    Ok(())
}

pub fn get_claude_override_dir() -> Option<PathBuf> {
    let settings = settings_store().read().ok()?;
    settings
        .claude_config_dir
        .as_ref()
        .map(|p| resolve_override_path(p))
}

pub fn get_codex_override_dir() -> Option<PathBuf> {
    let settings = settings_store().read().ok()?;
    settings
        .codex_config_dir
        .as_ref()
        .map(|p| resolve_override_path(p))
}

pub fn get_pi_override_dir() -> Option<PathBuf> {
    let settings = settings_store().read().ok()?;
    settings
        .pi_config_dir
        .as_ref()
        .map(|path| resolve_override_path(path))
}

pub fn unify_codex_session_history() -> bool {
    settings_store()
        .read()
        .unwrap_or_else(|e| {
            log::warn!("设置锁已毒化，使用恢复值: {e}");
            e.into_inner()
        })
        .unify_codex_session_history
}

// ===== 当前供应商管理函数 =====

/// 获取指定应用类型的当前供应商 ID（从本地 settings 读取）
///
/// 这是设备级别的设置，不随数据库同步。
/// 如果本地没有设置，调用者应该 fallback 到数据库的 `is_current` 字段。
pub fn get_current_provider(app_type: &AppType) -> Option<String> {
    let settings = settings_store().read().ok()?;
    match app_type {
        AppType::Claude => settings.current_provider_claude.clone(),
        AppType::Codex => settings.current_provider_codex.clone(),
        AppType::Pi => None,
    }
}

/// 设置指定应用类型的当前供应商 ID（保存到本地 settings）
///
/// 这是设备级别的设置，不随数据库同步。
/// 传入 `None` 会清除当前供应商设置。
pub fn set_current_provider(app_type: &AppType, id: Option<&str>) -> Result<(), AppError> {
    let id_owned = id.map(|s| s.to_string());
    mutate_settings(|settings| match app_type {
        AppType::Claude => settings.current_provider_claude = id_owned.clone(),
        AppType::Codex => settings.current_provider_codex = id_owned.clone(),
        AppType::Pi => {}
    })
}

/// 获取有效的当前供应商 ID（验证存在性）
///
/// 逻辑：
/// 1. 从本地 settings 读取当前供应商 ID
/// 2. 验证该 ID 在数据库中存在
/// 3. 如果不存在则清理本地 settings，fallback 到数据库的 is_current
///
/// 这确保了返回的 ID 一定是有效的（在数据库中存在）。
/// 多设备云同步场景下，配置导入后本地 ID 可能失效，此函数会自动修复。
pub fn get_effective_current_provider(
    db: &crate::database::Database,
    app_type: &AppType,
) -> Result<Option<String>, AppError> {
    // 1. 从本地 settings 读取
    if let Some(local_id) = get_current_provider(app_type) {
        // 2. 验证该 ID 在数据库中存在
        let providers = db.get_all_providers(app_type.as_str())?;
        if providers.contains_key(&local_id) {
            // 存在，直接返回
            return Ok(Some(local_id));
        }

        // 3. 不存在，清理本地 settings
        log::warn!(
            "本地 settings 中的供应商 {} ({}) 在数据库中不存在，将清理并 fallback 到数据库",
            local_id,
            app_type.as_str()
        );
        let _ = set_current_provider(app_type, None);
    }

    // Fallback 到数据库的 is_current
    db.get_current_provider(app_type.as_str())
}

// ===== Skill 同步方式管理函数 =====

/// 获取 Skill 同步方式配置
pub fn get_skill_sync_method() -> SyncMethod {
    settings_store()
        .read()
        .unwrap_or_else(|e| {
            log::warn!("设置锁已毒化，使用恢复值: {e}");
            e.into_inner()
        })
        .skill_sync_method
}

// ===== Skill 存储位置管理函数 =====

/// 获取 Skill 存储位置配置
pub fn get_skill_storage_location() -> SkillStorageLocation {
    settings_store()
        .read()
        .unwrap_or_else(|e| {
            log::warn!("设置锁已毒化，使用恢复值: {e}");
            e.into_inner()
        })
        .skill_storage_location
}

/// 设置 Skill 存储位置
pub fn set_skill_storage_location(location: SkillStorageLocation) -> Result<(), AppError> {
    mutate_settings(|s| {
        s.skill_storage_location = location;
    })
}

// ===== 备份策略管理函数 =====

/// Get the effective auto-backup interval in hours (default 24)
pub fn effective_backup_interval_hours() -> u32 {
    settings_store()
        .read()
        .unwrap_or_else(|e| {
            log::warn!("设置锁已毒化，使用恢复值: {e}");
            e.into_inner()
        })
        .backup_interval_hours
        .unwrap_or(24)
}

/// Get the effective backup retain count (default 10, minimum 1)
pub fn effective_backup_retain_count() -> usize {
    settings_store()
        .read()
        .unwrap_or_else(|e| {
            log::warn!("设置锁已毒化，使用恢复值: {e}");
            e.into_inner()
        })
        .backup_retain_count
        .map(|n| (n as usize).max(1))
        .unwrap_or(10)
}

// ===== 终端设置管理函数 =====

/// 获取首选终端应用
pub fn get_preferred_terminal() -> Option<String> {
    settings_store()
        .read()
        .unwrap_or_else(|e| {
            log::warn!("设置锁已毒化，使用恢复值: {e}");
            e.into_inner()
        })
        .preferred_terminal
        .clone()
}

/// 获取自定义终端配置：(可执行路径, 参数模板)。
/// 仅当 preferred_terminal == "custom" 且路径非空时返回。
#[cfg(target_os = "windows")]
pub fn get_custom_terminal_config() -> Option<(String, String)> {
    let settings = settings_store().read().unwrap_or_else(|e| {
        log::warn!("设置锁已毒化，使用恢复值: {e}");
        e.into_inner()
    });
    if settings.preferred_terminal.as_deref() != Some("custom") {
        return None;
    }
    let path = settings.preferred_terminal_custom_path.clone()?;
    let path = path.trim().to_string();
    if path.is_empty() {
        return None;
    }
    let args = settings
        .preferred_terminal_custom_args
        .clone()
        .filter(|a| !a.trim().is_empty())
        .unwrap_or_else(|| "-e cmd /K \"{bat}\"".to_string());
    Some((path, args))
}

// ===== WebDAV 同步设置管理函数 =====

/// 获取 WebDAV 同步设置
pub fn get_webdav_sync_settings() -> Option<WebDavSyncSettings> {
    settings_store().read().ok()?.webdav_sync.clone()
}

/// 保存 WebDAV 同步设置
/// Phase 2B: This will need to extract password to SecretStore
pub fn set_webdav_sync_settings(settings: Option<WebDavSyncSettings>) -> Result<(), AppError> {
    mutate_settings(|current| {
        current.webdav_sync = settings;
    })
}

/// 仅更新 WebDAV 同步状态，避免覆写 credentials/root/profile 等字段
pub fn update_webdav_sync_status(status: WebDavSyncStatus) -> Result<(), AppError> {
    mutate_settings(|current| {
        if let Some(sync) = current.webdav_sync.as_mut() {
            sync.status = status;
        }
    })
}

// ===== S3 同步设置管理函数 =====

pub fn get_s3_sync_settings() -> Option<S3SyncSettings> {
    settings_store().read().ok()?.s3_sync.clone()
}

/// Phase 2B: This will need to extract access_key_id and secret_access_key to SecretStore
pub fn set_s3_sync_settings(settings: Option<S3SyncSettings>) -> Result<(), AppError> {
    mutate_settings(|current| {
        current.s3_sync = settings;
    })
}

pub fn update_s3_sync_status(status: WebDavSyncStatus) -> Result<(), AppError> {
    mutate_settings(|current| {
        if let Some(s3) = current.s3_sync.as_mut() {
            s3.status = status;
        }
    })
}

/// E2E-5 升级豁免（一次性）：本版本起 http 端点默认拒。为避免悄悄打断存量用户，
/// 升级后首次启动时，把**升级前就已配置**的 http WebDAV/S3 远端的 `allow_insecure`
/// 预置为 `true`。之后置真不再执行（用户手动取消勾选也不会被重新豁免）。
/// 返回是否真的改动了某条（供调用方记日志）。
pub fn grandfather_existing_insecure_http() -> Result<bool, AppError> {
    if get_settings()
        .local_migrations
        .as_ref()
        .map(|m| m.http_insecure_grandfathered_v1)
        .unwrap_or(false)
    {
        return Ok(false);
    }
    let mut changed = false;
    mutate_settings(|current| {
        if let Some(w) = current.webdav_sync.as_mut() {
            if w.base_url
                .trim_start()
                .to_lowercase()
                .starts_with("http://")
                && !w.allow_insecure
            {
                w.allow_insecure = true;
                changed = true;
            }
        }
        if let Some(s) = current.s3_sync.as_mut() {
            if s.endpoint
                .trim_start()
                .to_lowercase()
                .starts_with("http://")
                && !s.allow_insecure
            {
                s.allow_insecure = true;
                changed = true;
            }
        }
        current
            .local_migrations
            .get_or_insert_with(Default::default)
            .http_insecure_grandfathered_v1 = true;
    })?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_paths_expand_windows_style_tilde_separators() {
        let home = dirs::home_dir().expect("home directory");
        assert_eq!(
            resolve_override_path(r"~\pi\agent"),
            home.join("pi").join("agent")
        );
    }

    // ===== P2 严格模式分级：归一化不变量与逐 app 判定 =====

    #[test]
    fn normalize_global_strict_clears_app_list() {
        let mut s = AppSettings {
            env_delivery_strict_mode: true,
            env_delivery_strict_apps: Some(vec!["claude".into()]),
            ..AppSettings::default()
        };
        s.normalize_strict_mode();
        assert!(
            s.env_delivery_strict_apps.is_none(),
            "全局严格须清空按应用列表"
        );
    }

    #[test]
    fn normalize_per_app_dedupes_lowercases_and_drops_invalid() {
        let mut s = AppSettings {
            env_delivery_strict_mode: false,
            env_delivery_strict_apps: Some(vec![
                " Claude ".into(),
                "claude".into(),
                "gemini".into(),
                "codex".into(),
            ]),
            ..AppSettings::default()
        };
        s.normalize_strict_mode();
        assert_eq!(
            s.env_delivery_strict_apps,
            Some(vec!["claude".to_string(), "codex".to_string()]),
            "去空格/小写/去重/剔除非法 app 名"
        );
    }

    #[test]
    fn normalize_empty_per_app_becomes_none() {
        let mut s = AppSettings {
            env_delivery_strict_mode: false,
            env_delivery_strict_apps: Some(vec![]),
            ..AppSettings::default()
        };
        s.normalize_strict_mode();
        assert_eq!(s.env_delivery_strict_apps, None, "空列表归 None（关档）");
    }

    #[test]
    fn is_strict_for_global_true_covers_every_app() {
        let s = AppSettings {
            env_delivery_strict_mode: true,
            env_delivery_strict_apps: None,
            ..AppSettings::default()
        };
        for app in AppType::all() {
            assert!(s.is_strict_for(&app), "全局严格：{app:?} 应严格");
        }
    }

    #[test]
    fn is_strict_for_per_app_only_hits_selected() {
        let s = AppSettings {
            env_delivery_strict_mode: false,
            env_delivery_strict_apps: Some(vec!["claude".into()]),
            ..AppSettings::default()
        };
        assert!(s.is_strict_for(&AppType::Claude));
        assert!(!s.is_strict_for(&AppType::Codex));
        assert!(!s.is_strict_for(&AppType::Pi));
    }
}
