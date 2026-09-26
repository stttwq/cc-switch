//! 按「供应商 / 应用级组」为粒度的凭据保险箱抽象（施工方案 §4.1）。
//!
//! 与旧的按字段 `SecretStore` 不同，`SecretVault` 一次往返拿/写「整包」，
//! 这是 1Password 后端「一次操作最多一次 `op` 往返」原则（§3.1）的类型级护栏：
//! 把「取」和「用」分开，取只发生在流程入口一次。
//!
//! P1 阶段行为仍跑在凭据管理器上（见 `LegacyWindowsVault`），1Password 实现于 P3 接入。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use zeroize::Zeroizing;

use crate::app_config::AppType;
use crate::database::Database;
use crate::error::AppError;
use crate::secrets::store::SecretStore;
use crate::secrets::target::SecretTarget;
use crate::secrets::types::ProviderSecrets;

/// 一个凭据组：供应商级或应用级。
///
/// - `Provider`：某个 app 下某个供应商的全部秘密字段（api_key / base_url / extra_env）。
/// - `AppSync`：应用级同步秘密（WebDAV 密码、S3 两把、E2E 口令），共用一个条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretGroup {
    Provider { app: AppType, provider_id: String },
    AppSync,
}

impl SecretGroup {
    pub fn provider(app: AppType, provider_id: impl Into<String>) -> Self {
        Self::Provider {
            app,
            provider_id: provider_id.into(),
        }
    }

    /// 组的稳定标识（用于内存实现的键、日志脱敏后的定位；不含秘密）。
    pub fn key(&self) -> String {
        match self {
            Self::Provider { app, provider_id } => {
                format!("provider/{}/{}", app.as_str(), provider_id)
            }
            Self::AppSync => "app/sync".to_string(),
        }
    }
}

/// 字段名约定（§4.1）。
pub const FIELD_API_KEY: &str = "api_key";
pub const FIELD_BASE_URL: &str = "base_url";
/// extra_env 字段前缀：`env.<VAR>`。
pub const FIELD_ENV_PREFIX: &str = "env.";
/// 应用级字段前缀：`app.<field>`。
pub const FIELD_APP_PREFIX: &str = "app.";
/// AppSync 固定字段名（与 `APP_SYNC_FIELDS` 一一对应，同步子系统复用）。
pub const FIELD_APP_WEBDAV_PASSWORD: &str = "app.webdav_password";
pub const FIELD_APP_S3_ACCESS_KEY_ID: &str = "app.s3_access_key_id";
pub const FIELD_APP_S3_SECRET_ACCESS_KEY: &str = "app.s3_secret_access_key";
pub const FIELD_APP_E2E_PASSPHRASE: &str = "app.e2e_passphrase";

/// 整包秘密：字段名 → 值。字段名不是秘密，值以 `Zeroizing` 承载。
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretBundle {
    fields: BTreeMap<String, Zeroizing<String>>,
}

impl SecretBundle {
    pub fn new() -> Self {
        Self {
            fields: BTreeMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn insert(&mut self, name: impl Into<String>, value: Zeroizing<String>) {
        self.fields.insert(name.into(), value);
    }

    pub fn get(&self, name: &str) -> Option<&Zeroizing<String>> {
        self.fields.get(name)
    }

    /// 移除一个字段（用于 AppSync 三态写入里的“清空即删除”）。
    pub fn remove(&mut self, name: &str) -> Option<Zeroizing<String>> {
        self.fields.remove(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.fields.contains_key(name)
    }

    /// 字段名清单（稳定顺序），写入本地引用表用；不含值。
    pub fn field_names(&self) -> Vec<String> {
        self.fields.keys().cloned().collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Zeroizing<String>)> {
        self.fields.iter()
    }

    /// 从 `ProviderSecrets` 构造整包：api_key / base_url / `env.<VAR>`。
    pub fn from_provider_secrets(secrets: &ProviderSecrets) -> Self {
        let mut bundle = Self::new();
        if let Some(key) = &secrets.api_key {
            bundle.insert(FIELD_API_KEY, key.clone());
        }
        if let Some(url) = &secrets.base_url {
            bundle.insert(FIELD_BASE_URL, url.clone());
        }
        for (var, val) in &secrets.extra_env {
            bundle.insert(format!("{FIELD_ENV_PREFIX}{var}"), val.clone());
        }
        bundle
    }

    /// 解释成 `ProviderSecrets`（应用级 `app.*` 字段会被忽略）。
    pub fn to_provider_secrets(&self) -> ProviderSecrets {
        let mut secrets = ProviderSecrets::new();
        for (name, value) in &self.fields {
            if name == FIELD_API_KEY {
                secrets.api_key = Some(value.clone());
            } else if name == FIELD_BASE_URL {
                secrets.base_url = Some(value.clone());
            } else if let Some(var) = name.strip_prefix(FIELD_ENV_PREFIX) {
                secrets.extra_env.insert(var.to_string(), value.clone());
            }
            // 其它前缀（app.*）不属于供应商秘密，忽略。
        }
        secrets
    }
}

// §12.4 / §9.7：Debug 绝不能泄露值。
impl std::fmt::Debug for SecretBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretBundle")
            .field(
                "fields",
                &self
                    .fields
                    .keys()
                    .map(|k| format!("{k}=[REDACTED]"))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// 条目引用（不是秘密）：vault id + item id + 字段名清单。写入本地 `secret_refs`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultRef {
    pub vault_id: String,
    pub item_id: String,
    pub fields: Vec<String>,
}

/// 保险箱轻量状态（不取值、不强制解锁）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultStatus {
    /// 已安装且已配置账户（可能仍处于锁定态，取值时才会要求解锁）。
    Ready,
    /// 未安装（找不到 `op.exe` 等）。
    NotInstalled,
    /// 已安装但未登录 / 未配置账户。
    NotSignedIn,
    /// 无法判定（记录原因，不含秘密）。
    Unknown(String),
}

/// 保险箱错误分类（§4.1 / §5.2）。每个变体对应一个固定 code，前端按 code 显示提示与「重试」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultError {
    /// 后端未安装（op.exe 不存在等）。
    NotInstalled,
    /// 未登录 / 未配置账户。
    NotSignedIn,
    /// 已锁定，或用户取消了解锁授权。
    Locked,
    /// 网络错误（断网、DNS、连接失败）。
    Network,
    /// 调用超时（含等待解锁超时）。
    Timeout,
    /// 条目冲突（同标题条目已存在等）。
    ItemConflict,
    /// 其它错误（只带脱敏后的分类信息，绝不含值/原始 stdout）。
    Other(String),
}

impl VaultError {
    /// 固定错误码，前端据此显示本地化提示。
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotInstalled => "vault_not_installed",
            Self::NotSignedIn => "vault_not_signed_in",
            Self::Locked => "vault_locked",
            Self::Network => "vault_network",
            Self::Timeout => "vault_timeout",
            Self::ItemConflict => "vault_item_conflict",
            Self::Other(_) => "vault_other",
        }
    }

    fn messages(&self) -> (String, String) {
        match self {
            Self::NotInstalled => (
                "未检测到 1Password 命令行工具（op）".to_string(),
                "1Password CLI (op) not found".to_string(),
            ),
            Self::NotSignedIn => (
                "尚未登录 1Password，请先登录".to_string(),
                "Not signed in to 1Password".to_string(),
            ),
            Self::Locked => (
                "1Password 已锁定、未运行或授权被取消，请打开并解锁 1Password 后重试".to_string(),
                "1Password is locked, not running, or authorization was dismissed".to_string(),
            ),
            Self::Network => (
                "连接 1Password 失败，请检查网络".to_string(),
                "Failed to reach 1Password (network error)".to_string(),
            ),
            Self::Timeout => (
                "1Password 请求超时，请重试".to_string(),
                "1Password request timed out".to_string(),
            ),
            Self::ItemConflict => (
                "1Password 中已存在同名条目".to_string(),
                "A conflicting item already exists in 1Password".to_string(),
            ),
            Self::Other(detail) => (
                format!("1Password 调用失败：{detail}"),
                format!("1Password call failed: {detail}"),
            ),
        }
    }
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (zh, _) = self.messages();
        write!(f, "{zh}")
    }
}

impl std::error::Error for VaultError {}

impl From<VaultError> for AppError {
    fn from(err: VaultError) -> Self {
        let code = err.code();
        let (zh, en) = err.messages();
        AppError::localized(code, zh, en)
    }
}

/// 按组读写整包的保险箱接口（同步；1Password 实现本就是阻塞子进程）。
pub trait SecretVault: Send + Sync {
    /// 一次往返拿整包。条目不存在 => `Ok(None)`；锁定/断网等 => `Err`。
    fn fetch(&self, group: &SecretGroup) -> Result<Option<SecretBundle>, VaultError>;

    /// 整包覆盖写（新建或编辑），返回条目引用。
    fn put(&self, group: &SecretGroup, bundle: &SecretBundle) -> Result<VaultRef, VaultError>;

    /// 删除整个条目。
    fn delete(&self, group: &SecretGroup) -> Result<(), VaultError>;

    /// 轻量状态检测，不取值、不强制解锁。
    fn status(&self) -> VaultStatus;

    /// 后端名（诊断用）。
    fn backend_name(&self) -> &'static str;
}

/// 测试用内存实现。
pub struct InMemoryVault {
    storage: Mutex<BTreeMap<String, SecretBundle>>,
}

impl InMemoryVault {
    pub fn new() -> Self {
        Self {
            storage: Mutex::new(BTreeMap::new()),
        }
    }
}

impl Default for InMemoryVault {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretVault for InMemoryVault {
    fn fetch(&self, group: &SecretGroup) -> Result<Option<SecretBundle>, VaultError> {
        let store = self.storage.lock().unwrap();
        Ok(store.get(&group.key()).cloned())
    }

    fn put(&self, group: &SecretGroup, bundle: &SecretBundle) -> Result<VaultRef, VaultError> {
        let key = group.key();
        self.storage
            .lock()
            .unwrap()
            .insert(key.clone(), bundle.clone());
        Ok(VaultRef {
            vault_id: "in-memory".to_string(),
            item_id: key,
            fields: bundle.field_names(),
        })
    }

    fn delete(&self, group: &SecretGroup) -> Result<(), VaultError> {
        self.storage.lock().unwrap().remove(&group.key());
        Ok(())
    }

    fn status(&self) -> VaultStatus {
        VaultStatus::Ready
    }

    fn backend_name(&self) -> &'static str {
        "in-memory"
    }
}

/// 测试用计数包装器：锁住「每流程 op 往返次数」（§9.3 / 原则 1 的护栏）。
pub struct CountingVault {
    inner: Arc<dyn SecretVault>,
    fetch_count: AtomicUsize,
    put_count: AtomicUsize,
    delete_count: AtomicUsize,
}

impl CountingVault {
    pub fn new(inner: Arc<dyn SecretVault>) -> Self {
        Self {
            inner,
            fetch_count: AtomicUsize::new(0),
            put_count: AtomicUsize::new(0),
            delete_count: AtomicUsize::new(0),
        }
    }

    pub fn fetch_count(&self) -> usize {
        self.fetch_count.load(Ordering::SeqCst)
    }

    pub fn put_count(&self) -> usize {
        self.put_count.load(Ordering::SeqCst)
    }

    pub fn delete_count(&self) -> usize {
        self.delete_count.load(Ordering::SeqCst)
    }

    pub fn reset(&self) {
        self.fetch_count.store(0, Ordering::SeqCst);
        self.put_count.store(0, Ordering::SeqCst);
        self.delete_count.store(0, Ordering::SeqCst);
    }
}

impl SecretVault for CountingVault {
    fn fetch(&self, group: &SecretGroup) -> Result<Option<SecretBundle>, VaultError> {
        self.fetch_count.fetch_add(1, Ordering::SeqCst);
        self.inner.fetch(group)
    }

    fn put(&self, group: &SecretGroup, bundle: &SecretBundle) -> Result<VaultRef, VaultError> {
        self.put_count.fetch_add(1, Ordering::SeqCst);
        self.inner.put(group, bundle)
    }

    fn delete(&self, group: &SecretGroup) -> Result<(), VaultError> {
        self.delete_count.fetch_add(1, Ordering::SeqCst);
        self.inner.delete(group)
    }

    fn status(&self) -> VaultStatus {
        self.inner.status()
    }

    fn backend_name(&self) -> &'static str {
        self.inner.backend_name()
    }
}

/// AppSync 组的字段映射：整包字段名 ↔ 凭据管理器 target（`app/<sub>/<field>`）。
/// 1Password 后端（P3）也复用同一套字段名，保证跨后端一致。
const APP_SYNC_FIELDS: &[(&str, &str, &str)] = &[
    (FIELD_APP_WEBDAV_PASSWORD, "webdav", "password"),
    (FIELD_APP_S3_ACCESS_KEY_ID, "s3", "access_key_id"),
    (FIELD_APP_S3_SECRET_ACCESS_KEY, "s3", "secret_access_key"),
    (FIELD_APP_E2E_PASSPHRASE, "sync", "passphrase"),
];

/// 把应用级 target 的 `(<app>, <field>)`（如 `("webdav","password")`）映射成 AppSync
/// 整包字段名（如 `app.webdav_password`）。迁移向导用。
pub fn app_sync_bundle_field(app: &str, field: &str) -> Option<&'static str> {
    APP_SYNC_FIELDS
        .iter()
        .find(|(_, a, f)| *a == app && *f == field)
        .map(|(label, _, _)| *label)
}

fn store_err(e: AppError) -> VaultError {
    VaultError::Other(e.to_string())
}

/// 把旧的按字段 `SecretStore`（凭据管理器）包成 `SecretVault`（§4.1 / §7）。
///
/// P1 阶段全程用它跑在凭据管理器上：调用方已改成按整包读写，行为不变。
/// 迁移完成后不再构造它，2.4 连同 `keyring` 依赖一并移除。
pub struct LegacyWindowsVault {
    store: Arc<dyn SecretStore>,
    db: Arc<Database>,
}

impl LegacyWindowsVault {
    pub fn new(store: Arc<dyn SecretStore>, db: Arc<Database>) -> Self {
        Self { store, db }
    }

    /// 读取某供应商已登记的 extra_env 变量名（keyring 无法枚举，靠 known_secret_targets）。
    fn known_env_vars(&self, app: &AppType, provider_id: &str) -> Result<Vec<String>, VaultError> {
        let prefix = format!(
            "cc-switch/v1/provider/{}/{}/env/",
            app.as_str(),
            provider_id
        );
        let targets = crate::secrets::load_known_targets(self.db.as_ref()).map_err(store_err)?;
        Ok(targets
            .into_iter()
            .filter_map(|t| t.strip_prefix(&prefix).map(str::to_string))
            .filter(|v| !v.is_empty())
            .collect())
    }

    /// 写完供应商字段后，把 known_secret_targets 里该供应商的登记项整体替换成 `desired`。
    fn rewrite_known_targets(
        &self,
        app: &AppType,
        provider_id: &str,
        desired: &ProviderSecrets,
    ) -> Result<(), VaultError> {
        let prefix = crate::secrets::provider_target_prefix(app, provider_id);
        let mut targets = crate::secrets::load_known_targets(self.db.as_ref()).map_err(store_err)?;
        // 先移除该供应商此前的全部登记项，再按 desired 重建（整包语义：删掉的字段要消失）。
        targets.retain(|t| !t.starts_with(&prefix));
        if desired.api_key.is_some() {
            targets.push(SecretTarget::provider_api_key(app.clone(), provider_id).to_target_string());
        }
        if desired.base_url.is_some() {
            targets
                .push(SecretTarget::provider_base_url(app.clone(), provider_id).to_target_string());
        }
        for var in desired.extra_env.keys() {
            targets.push(
                SecretTarget::provider_env(app.clone(), provider_id, var).to_target_string(),
            );
        }
        crate::secrets::save_known_targets(self.db.as_ref(), &targets).map_err(store_err)
    }
}

impl SecretVault for LegacyWindowsVault {
    fn fetch(&self, group: &SecretGroup) -> Result<Option<SecretBundle>, VaultError> {
        let mut bundle = SecretBundle::new();
        match group {
            SecretGroup::Provider { app, provider_id } => {
                if let Some(key) = futures::executor::block_on(
                    self.store
                        .get(&SecretTarget::provider_api_key(app.clone(), provider_id.clone())),
                )
                .map_err(store_err)?
                {
                    bundle.insert(FIELD_API_KEY, key);
                }
                if let Some(url) = futures::executor::block_on(
                    self.store
                        .get(&SecretTarget::provider_base_url(app.clone(), provider_id.clone())),
                )
                .map_err(store_err)?
                {
                    bundle.insert(FIELD_BASE_URL, url);
                }
                for var in self.known_env_vars(app, provider_id)? {
                    let target =
                        SecretTarget::provider_env(app.clone(), provider_id.clone(), var.clone());
                    if let Some(value) =
                        futures::executor::block_on(self.store.get(&target)).map_err(store_err)?
                    {
                        bundle.insert(format!("{FIELD_ENV_PREFIX}{var}"), value);
                    }
                }
            }
            SecretGroup::AppSync => {
                for (field, app, sub) in APP_SYNC_FIELDS {
                    let target = SecretTarget::app(*app, *sub);
                    if let Some(value) =
                        futures::executor::block_on(self.store.get(&target)).map_err(store_err)?
                    {
                        bundle.insert(*field, value);
                    }
                }
            }
        }
        if bundle.is_empty() {
            Ok(None)
        } else {
            Ok(Some(bundle))
        }
    }

    fn put(&self, group: &SecretGroup, bundle: &SecretBundle) -> Result<VaultRef, VaultError> {
        match group {
            SecretGroup::Provider { app, provider_id } => {
                let desired = bundle.to_provider_secrets();
                let api_key_target =
                    SecretTarget::provider_api_key(app.clone(), provider_id.clone());
                match &desired.api_key {
                    Some(k) => futures::executor::block_on(self.store.set(&api_key_target, k.clone()))
                        .map_err(store_err)?,
                    None => futures::executor::block_on(self.store.delete(&api_key_target))
                        .map_err(store_err)?,
                }
                let base_url_target =
                    SecretTarget::provider_base_url(app.clone(), provider_id.clone());
                match &desired.base_url {
                    Some(u) => {
                        futures::executor::block_on(self.store.set(&base_url_target, u.clone()))
                            .map_err(store_err)?
                    }
                    None => futures::executor::block_on(self.store.delete(&base_url_target))
                        .map_err(store_err)?,
                }
                // 整包语义：先删掉已登记但本次不再出现的 env 变量，再写入本次的。
                for var in self.known_env_vars(app, provider_id)? {
                    if !desired.extra_env.contains_key(&var) {
                        let target =
                            SecretTarget::provider_env(app.clone(), provider_id.clone(), var);
                        futures::executor::block_on(self.store.delete(&target))
                            .map_err(store_err)?;
                    }
                }
                for (var, value) in &desired.extra_env {
                    let target =
                        SecretTarget::provider_env(app.clone(), provider_id.clone(), var.clone());
                    futures::executor::block_on(self.store.set(&target, value.clone()))
                        .map_err(store_err)?;
                }
                self.rewrite_known_targets(app, provider_id, &desired)?;
            }
            SecretGroup::AppSync => {
                for (field, app, sub) in APP_SYNC_FIELDS {
                    let target = SecretTarget::app(*app, *sub);
                    match bundle.get(field) {
                        Some(value) => {
                            futures::executor::block_on(self.store.set(&target, value.clone()))
                                .map_err(store_err)?
                        }
                        None => futures::executor::block_on(self.store.delete(&target))
                            .map_err(store_err)?,
                    }
                }
            }
        }
        Ok(VaultRef {
            vault_id: String::new(),
            item_id: group.key(),
            fields: bundle.field_names(),
        })
    }

    fn delete(&self, group: &SecretGroup) -> Result<(), VaultError> {
        match group {
            SecretGroup::Provider { app, provider_id } => {
                futures::executor::block_on(
                    self.store
                        .delete(&SecretTarget::provider_api_key(app.clone(), provider_id.clone())),
                )
                .map_err(store_err)?;
                futures::executor::block_on(
                    self.store
                        .delete(&SecretTarget::provider_base_url(app.clone(), provider_id.clone())),
                )
                .map_err(store_err)?;
                for var in self.known_env_vars(app, provider_id)? {
                    let target =
                        SecretTarget::provider_env(app.clone(), provider_id.clone(), var);
                    futures::executor::block_on(self.store.delete(&target)).map_err(store_err)?;
                }
                // 从 known_secret_targets 抹掉该供应商的全部登记项。
                let empty = ProviderSecrets::new();
                self.rewrite_known_targets(app, provider_id, &empty)?;
            }
            SecretGroup::AppSync => {
                for (_, app, sub) in APP_SYNC_FIELDS {
                    let target = SecretTarget::app(*app, *sub);
                    futures::executor::block_on(self.store.delete(&target)).map_err(store_err)?;
                }
            }
        }
        Ok(())
    }

    fn status(&self) -> VaultStatus {
        // 凭据管理器在 Windows 上始终可用；真正的状态分类是 1Password 后端（P3）的事。
        VaultStatus::Ready
    }

    fn backend_name(&self) -> &'static str {
        "windows-credential-manager"
    }
}

/// 后端不可用时的占位实现（§6.7）：构造 OnePasswordVault 失败（op 未装/签名不信任）时
/// 用它代替，让 App 仍能正常打开、浏览；任何取钥匙操作明确失败（绝不静默降级）。
pub struct UnavailableVault {
    error: VaultError,
}

impl UnavailableVault {
    pub fn new(error: VaultError) -> Self {
        Self { error }
    }
}

impl SecretVault for UnavailableVault {
    fn fetch(&self, _group: &SecretGroup) -> Result<Option<SecretBundle>, VaultError> {
        Err(self.error.clone())
    }

    fn put(&self, _group: &SecretGroup, _bundle: &SecretBundle) -> Result<VaultRef, VaultError> {
        Err(self.error.clone())
    }

    fn delete(&self, _group: &SecretGroup) -> Result<(), VaultError> {
        Err(self.error.clone())
    }

    fn status(&self) -> VaultStatus {
        match &self.error {
            VaultError::NotInstalled => VaultStatus::NotInstalled,
            VaultError::NotSignedIn => VaultStatus::NotSignedIn,
            other => VaultStatus::Unknown(other.code().to_string()),
        }
    }

    fn backend_name(&self) -> &'static str {
        "1password-unavailable"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_debug_redacts_values() {
        let mut bundle = SecretBundle::new();
        bundle.insert(FIELD_API_KEY, Zeroizing::new("sk-secret-123".to_string()));
        bundle.insert("env.FOO", Zeroizing::new("bar-value".to_string()));

        let out = format!("{bundle:?}");
        assert!(!out.contains("sk-secret-123"));
        assert!(!out.contains("bar-value"));
        assert!(out.contains("[REDACTED]"));
        assert!(out.contains("api_key=[REDACTED]"));
        assert!(out.contains("env.FOO=[REDACTED]"));
    }

    #[test]
    fn provider_secrets_roundtrip() {
        let secrets = ProviderSecrets::new()
            .with_api_key("sk-abc")
            .with_base_url("https://api.example.com")
            .with_extra_env("OPENROUTER_KEY", "or-xyz");

        let bundle = SecretBundle::from_provider_secrets(&secrets);
        assert_eq!(bundle.len(), 3);
        assert!(bundle.contains(FIELD_API_KEY));
        assert!(bundle.contains(FIELD_BASE_URL));
        assert!(bundle.contains("env.OPENROUTER_KEY"));

        let back = bundle.to_provider_secrets();
        assert_eq!(back, secrets);
    }

    #[test]
    fn to_provider_secrets_ignores_app_fields() {
        let mut bundle = SecretBundle::new();
        bundle.insert(FIELD_API_KEY, Zeroizing::new("k".to_string()));
        bundle.insert("app.webdav_password", Zeroizing::new("pw".to_string()));

        let secrets = bundle.to_provider_secrets();
        assert_eq!(secrets.api_key.as_deref().map(String::as_str), Some("k"));
        assert!(secrets.base_url.is_none());
        assert!(secrets.extra_env.is_empty());
    }

    #[test]
    fn vault_error_codes_are_stable() {
        assert_eq!(VaultError::NotInstalled.code(), "vault_not_installed");
        assert_eq!(VaultError::NotSignedIn.code(), "vault_not_signed_in");
        assert_eq!(VaultError::Locked.code(), "vault_locked");
        assert_eq!(VaultError::Network.code(), "vault_network");
        assert_eq!(VaultError::Timeout.code(), "vault_timeout");
        assert_eq!(VaultError::ItemConflict.code(), "vault_item_conflict");
        assert_eq!(VaultError::Other("x".to_string()).code(), "vault_other");
    }

    #[test]
    fn vault_error_display_and_apperror_hide_details_shape() {
        // Display/AppError 只含分类信息，不含任何字段值。
        let err = VaultError::Locked;
        let msg = err.to_string();
        assert!(msg.contains("锁定") || msg.contains("解锁"));
        let app: AppError = err.into();
        match app {
            AppError::Localized { key, .. } => assert_eq!(key, "vault_locked"),
            other => panic!("expected Localized, got {other:?}"),
        }
    }

    #[test]
    fn in_memory_vault_roundtrip() {
        let vault = InMemoryVault::new();
        let group = SecretGroup::provider(AppType::Claude, "p1");
        assert!(vault.fetch(&group).unwrap().is_none());

        let mut bundle = SecretBundle::new();
        bundle.insert(FIELD_API_KEY, Zeroizing::new("sk-1".to_string()));
        let vref = vault.put(&group, &bundle).unwrap();
        assert_eq!(vref.fields, vec![FIELD_API_KEY.to_string()]);

        let got = vault.fetch(&group).unwrap().unwrap();
        assert_eq!(
            got.get(FIELD_API_KEY).map(|v| v.to_string()),
            Some("sk-1".to_string())
        );

        vault.delete(&group).unwrap();
        assert!(vault.fetch(&group).unwrap().is_none());
    }

    #[test]
    fn counting_vault_counts_calls() {
        let inner = Arc::new(InMemoryVault::new());
        let counting = CountingVault::new(inner);
        let group = SecretGroup::AppSync;

        let mut bundle = SecretBundle::new();
        bundle.insert("app.e2e_passphrase", Zeroizing::new("pw".to_string()));
        counting.put(&group, &bundle).unwrap();
        counting.fetch(&group).unwrap();
        counting.fetch(&group).unwrap();
        counting.delete(&group).unwrap();

        assert_eq!(counting.put_count(), 1);
        assert_eq!(counting.fetch_count(), 2);
        assert_eq!(counting.delete_count(), 1);
    }

    fn legacy_vault() -> LegacyWindowsVault {
        use crate::secrets::store::InMemorySecretStore;
        let db = Arc::new(crate::database::Database::memory().expect("memory db"));
        let store: Arc<dyn crate::secrets::store::SecretStore> =
            Arc::new(InMemorySecretStore::new());
        LegacyWindowsVault::new(store, db)
    }

    #[test]
    fn legacy_provider_roundtrip_and_env_deletion() {
        let vault = legacy_vault();
        let group = SecretGroup::provider(AppType::Claude, "p1");
        assert!(vault.fetch(&group).unwrap().is_none());

        // 写入 api_key + base_url + 两个 extra_env。
        let mut b1 = SecretBundle::new();
        b1.insert(FIELD_API_KEY, Zeroizing::new("sk-1".to_string()));
        b1.insert(FIELD_BASE_URL, Zeroizing::new("https://x".to_string()));
        b1.insert("env.FOO", Zeroizing::new("foo".to_string()));
        b1.insert("env.BAR", Zeroizing::new("bar".to_string()));
        vault.put(&group, &b1).unwrap();

        let got = vault.fetch(&group).unwrap().unwrap();
        assert_eq!(got.get(FIELD_API_KEY).map(|v| v.to_string()), Some("sk-1".into()));
        assert_eq!(got.get("env.FOO").map(|v| v.to_string()), Some("foo".into()));
        assert_eq!(got.get("env.BAR").map(|v| v.to_string()), Some("bar".into()));

        // 重写：去掉 BAR、去掉 base_url，整包语义应让它们消失。
        let mut b2 = SecretBundle::new();
        b2.insert(FIELD_API_KEY, Zeroizing::new("sk-2".to_string()));
        b2.insert("env.FOO", Zeroizing::new("foo2".to_string()));
        vault.put(&group, &b2).unwrap();

        let got2 = vault.fetch(&group).unwrap().unwrap();
        assert_eq!(got2.get(FIELD_API_KEY).map(|v| v.to_string()), Some("sk-2".into()));
        assert!(got2.get(FIELD_BASE_URL).is_none(), "base_url 应被整包覆盖删除");
        assert_eq!(got2.get("env.FOO").map(|v| v.to_string()), Some("foo2".into()));
        assert!(got2.get("env.BAR").is_none(), "BAR 应被整包覆盖删除");

        // 删除整组。
        vault.delete(&group).unwrap();
        assert!(vault.fetch(&group).unwrap().is_none());
    }

    #[test]
    fn legacy_app_sync_roundtrip() {
        let vault = legacy_vault();
        let group = SecretGroup::AppSync;

        let mut b = SecretBundle::new();
        b.insert("app.webdav_password", Zeroizing::new("pw".to_string()));
        b.insert("app.e2e_passphrase", Zeroizing::new("pass".to_string()));
        vault.put(&group, &b).unwrap();

        let got = vault.fetch(&group).unwrap().unwrap();
        assert_eq!(
            got.get("app.webdav_password").map(|v| v.to_string()),
            Some("pw".into())
        );
        assert_eq!(
            got.get("app.e2e_passphrase").map(|v| v.to_string()),
            Some("pass".into())
        );
        assert!(got.get("app.s3_access_key_id").is_none());

        vault.delete(&group).unwrap();
        assert!(vault.fetch(&group).unwrap().is_none());
    }
}
