//! Sync-specific secret management for WebDAV and S3
//!
//! This module provides a simplified interface for extracting and restoring
//! WebDAV passwords and S3 credentials to/from SecretStore.

use std::sync::Arc;

use crate::error::AppError;
use crate::secrets::store::SecretStore;
use crate::secrets::target::SecretTarget;

/// 三态语义（§5.2.5）：`None` = 前端未触碰该字段，保持现值；`Some("")` = 用户清空了
/// 输入框，删除已存凭据；`Some(v)` = 写入 v。返回是否真的写入了值。
///
/// 空串必须删条目：否则"清空密码框再保存"永远删不掉凭据管理器里的旧密码。
/// `literal:` 前缀是历史"已迁移"占位，不覆盖真实凭据。
pub async fn extract_webdav_password(
    store: &Arc<dyn SecretStore>,
    password: Option<&str>,
) -> Result<bool, AppError> {
    let Some(password) = password else {
        return Ok(false);
    };
    if password.starts_with("literal:") {
        return Ok(false);
    }
    let target = SecretTarget::app("webdav", "password");
    if password.is_empty() {
        store.delete(&target).await?;
        return Ok(false);
    }
    store.store(&target, password).await?;
    Ok(true)
}

/// Restore WebDAV password from SecretStore
pub async fn restore_webdav_password(
    store: &Arc<dyn SecretStore>,
) -> Result<Option<zeroize::Zeroizing<String>>, AppError> {
    let target = SecretTarget::app("webdav", "password");
    store.retrieve(&target).await
}

/// 三态语义同 `extract_webdav_password`，两个字段各自独立：`None` = 不动，`Some("")` =
/// 删除该条，`Some(v)` = 写入。返回是否至少写入了一个值。
pub async fn extract_s3_credentials(
    store: &Arc<dyn SecretStore>,
    access_key_id: Option<&str>,
    secret_access_key: Option<&str>,
) -> Result<bool, AppError> {
    let mut stored = false;

    for (field, value) in [
        ("access_key_id", access_key_id),
        ("secret_access_key", secret_access_key),
    ] {
        let Some(value) = value else {
            continue;
        };
        if value.starts_with("literal:") {
            continue;
        }
        let target = SecretTarget::app("s3", field);
        if value.is_empty() {
            store.delete(&target).await?;
        } else {
            store.store(&target, value).await?;
            stored = true;
        }
    }

    Ok(stored)
}

/// Restore S3 credentials from SecretStore
pub async fn restore_s3_credentials(
    store: &Arc<dyn SecretStore>,
) -> Result<
    (
        Option<zeroize::Zeroizing<String>>,
        Option<zeroize::Zeroizing<String>>,
    ),
    AppError,
> {
    let access_key_id = store
        .retrieve(&SecretTarget::app("s3", "access_key_id"))
        .await?;
    let secret_access_key = store
        .retrieve(&SecretTarget::app("s3", "secret_access_key"))
        .await?;
    Ok((access_key_id, secret_access_key))
}
