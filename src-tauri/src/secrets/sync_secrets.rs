//! 同步专用凭据管理（WebDAV / S3 / E2E 口令）——§6.9：改走 vault 的 AppSync 整包。
//!
//! 读侧：入口用 [`fetch_sync_credentials`] 一次性取整包（原则 1，一次操作一次往返），
//! 把 [`SyncCredentials`] 传进各同步函数；同步过程中不再逐字段回读。
//! 写侧：`store_*` 各自「fetch 整包 → 改目标字段 → put 整包」，只动自己那一格，
//! 保留 AppSync 里其它字段（webdav / s3 / e2e 互不干扰）。

use std::sync::Arc;

use zeroize::Zeroizing;

use crate::error::AppError;
use crate::secrets::vault::{
    SecretBundle, SecretGroup, SecretVault, FIELD_APP_E2E_PASSPHRASE, FIELD_APP_S3_ACCESS_KEY_ID,
    FIELD_APP_S3_SECRET_ACCESS_KEY, FIELD_APP_WEBDAV_PASSWORD,
};

/// 一次取回的应用级同步凭据整包。值以 `Zeroizing` 承载，用完即丢，不缓存。
#[derive(Clone, Default)]
pub struct SyncCredentials {
    pub webdav_password: Option<Zeroizing<String>>,
    pub s3_access_key_id: Option<Zeroizing<String>>,
    pub s3_secret_access_key: Option<Zeroizing<String>>,
    pub e2e_passphrase: Option<Zeroizing<String>>,
}

impl SyncCredentials {
    fn from_bundle(bundle: &SecretBundle) -> Self {
        Self {
            webdav_password: bundle.get(FIELD_APP_WEBDAV_PASSWORD).cloned(),
            s3_access_key_id: bundle.get(FIELD_APP_S3_ACCESS_KEY_ID).cloned(),
            s3_secret_access_key: bundle.get(FIELD_APP_S3_SECRET_ACCESS_KEY).cloned(),
            e2e_passphrase: bundle.get(FIELD_APP_E2E_PASSPHRASE).cloned(),
        }
    }
}

// 值不进 Debug。
impl std::fmt::Debug for SyncCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncCredentials")
            .field("webdav_password", &self.webdav_password.as_ref().map(|_| "[REDACTED]"))
            .field("s3_access_key_id", &self.s3_access_key_id.as_ref().map(|_| "[REDACTED]"))
            .field(
                "s3_secret_access_key",
                &self.s3_secret_access_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("e2e_passphrase", &self.e2e_passphrase.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

/// §6.9：一次往返取回应用级同步凭据整包。
pub fn fetch_sync_credentials(
    vault: &Arc<dyn SecretVault>,
) -> Result<SyncCredentials, AppError> {
    match vault.fetch(&SecretGroup::AppSync)? {
        Some(bundle) => Ok(SyncCredentials::from_bundle(&bundle)),
        None => Ok(SyncCredentials::default()),
    }
}

/// 在 AppSync 整包上「改一格再写回」，保留其它字段。`value` 为 `None` 表示删除该字段。
fn put_app_field(
    vault: &Arc<dyn SecretVault>,
    field: &str,
    value: Option<&str>,
) -> Result<(), AppError> {
    let mut bundle = vault.fetch(&SecretGroup::AppSync)?.unwrap_or_default();
    match value {
        Some(v) => bundle.insert(field, Zeroizing::new(v.to_string())),
        None => {
            bundle.remove(field);
        }
    }
    vault.put(&SecretGroup::AppSync, &bundle)?;
    Ok(())
}

/// 三态语义（§5.2.5）：`None` = 前端未触碰该字段，保持现值；`Some("")` = 用户清空了
/// 输入框，删除已存凭据；`Some(v)` = 写入 v。返回是否真的写入了值。
///
/// 空串必须删条目：否则"清空密码框再保存"永远删不掉里面的旧密码。
/// `literal:` 前缀是历史"已迁移"占位，不覆盖真实凭据。
pub fn store_webdav_password(
    vault: &Arc<dyn SecretVault>,
    password: Option<&str>,
) -> Result<bool, AppError> {
    let Some(password) = password else {
        return Ok(false);
    };
    if password.starts_with("literal:") {
        return Ok(false);
    }
    if password.is_empty() {
        put_app_field(vault, FIELD_APP_WEBDAV_PASSWORD, None)?;
        return Ok(false);
    }
    put_app_field(vault, FIELD_APP_WEBDAV_PASSWORD, Some(password))?;
    Ok(true)
}

/// 三态语义同 `store_webdav_password`，两个字段各自独立：`None` = 不动，`Some("")` =
/// 删除该条，`Some(v)` = 写入。返回是否至少写入了一个值。
pub fn store_s3_credentials(
    vault: &Arc<dyn SecretVault>,
    access_key_id: Option<&str>,
    secret_access_key: Option<&str>,
) -> Result<bool, AppError> {
    let mut stored = false;
    for (field, value) in [
        (FIELD_APP_S3_ACCESS_KEY_ID, access_key_id),
        (FIELD_APP_S3_SECRET_ACCESS_KEY, secret_access_key),
    ] {
        let Some(value) = value else {
            continue;
        };
        if value.starts_with("literal:") {
            continue;
        }
        if value.is_empty() {
            put_app_field(vault, field, None)?;
        } else {
            put_app_field(vault, field, Some(value))?;
            stored = true;
        }
    }
    Ok(stored)
}

// ─── 同步口令（端到端加密，方案 2.4.2） ──────────────────────

/// 同步口令与 WebDAV / S3 登录密码无关，永不上传。三态语义同 `store_webdav_password`：
/// `None`=不动，`Some("")`=删除，`Some(v)`=写入。返回是否真的写入了值。
pub fn store_sync_passphrase(
    vault: &Arc<dyn SecretVault>,
    passphrase: Option<&str>,
) -> Result<bool, AppError> {
    let Some(passphrase) = passphrase else {
        return Ok(false);
    };
    if passphrase.starts_with("literal:") {
        return Ok(false);
    }
    if passphrase.is_empty() {
        put_app_field(vault, FIELD_APP_E2E_PASSPHRASE, None)?;
        return Ok(false);
    }
    put_app_field(vault, FIELD_APP_E2E_PASSPHRASE, Some(passphrase))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::vault::{CountingVault, InMemoryVault, SecretVault};

    fn counting() -> (Arc<dyn SecretVault>, Arc<CountingVault>) {
        let inner: Arc<dyn SecretVault> = Arc::new(InMemoryVault::new());
        let counting = Arc::new(CountingVault::new(inner));
        let as_dyn: Arc<dyn SecretVault> = counting.clone();
        (as_dyn, counting)
    }

    #[test]
    fn fetch_sync_credentials_is_one_fetch() {
        let (vault, counting) = counting();
        fetch_sync_credentials(&vault).expect("fetch");
        assert_eq!(counting.fetch_count(), 1);
    }

    #[test]
    fn store_field_preserves_other_app_secrets() {
        let (vault, _c) = counting();
        store_webdav_password(&vault, Some("pw")).expect("webdav");
        store_sync_passphrase(&vault, Some("pass")).expect("e2e");
        // 删除 webdav（三态空串）不应连带删掉 e2e。
        store_webdav_password(&vault, Some("")).expect("clear webdav");

        let creds = fetch_sync_credentials(&vault).expect("fetch");
        assert!(creds.webdav_password.is_none(), "webdav 已删");
        assert_eq!(
            creds.e2e_passphrase.as_deref().map(|v| v.as_str()),
            Some("pass"),
            "e2e 口令不受 webdav 删除影响"
        );
    }
}
