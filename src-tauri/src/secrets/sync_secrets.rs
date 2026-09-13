//! Sync-specific secret management for WebDAV and S3
//!
//! This module provides a simplified interface for extracting and restoring
//! WebDAV passwords and S3 credentials to/from SecretStore.

use std::sync::Arc;

use crate::error::AppError;
use crate::secrets::store::SecretStore;
use crate::secrets::target::SecretTarget;

/// Extract and store WebDAV password, returning whether a password was stored
pub async fn extract_webdav_password(
    store: &Arc<dyn SecretStore>,
    password: &str,
) -> Result<bool, AppError> {
    if password.is_empty() || password.starts_with("literal:") {
        return Ok(false);
    }
    let target = SecretTarget::app("webdav", "password");
    store.store(&target, password).await?;
    Ok(true)
}

/// Restore WebDAV password from SecretStore
pub async fn restore_webdav_password(
    store: &Arc<dyn SecretStore>,
) -> Result<Option<String>, AppError> {
    let target = SecretTarget::app("webdav", "password");
    store.retrieve(&target).await
}

/// Extract and store S3 credentials, returning whether any credentials were stored
pub async fn extract_s3_credentials(
    store: &Arc<dyn SecretStore>,
    access_key_id: &str,
    secret_access_key: &str,
) -> Result<bool, AppError> {
    let mut stored = false;

    if !access_key_id.is_empty() && !access_key_id.starts_with("literal:") {
        let target = SecretTarget::app("s3", "access_key_id");
        store.store(&target, access_key_id).await?;
        stored = true;
    }

    if !secret_access_key.is_empty() && !secret_access_key.starts_with("literal:") {
        let target = SecretTarget::app("s3", "secret_access_key");
        store.store(&target, secret_access_key).await?;
        stored = true;
    }

    Ok(stored)
}

/// Restore S3 credentials from SecretStore
pub async fn restore_s3_credentials(
    store: &Arc<dyn SecretStore>,
) -> Result<(Option<String>, Option<String>), AppError> {
    let access_key_id = store
        .retrieve(&SecretTarget::app("s3", "access_key_id"))
        .await?;
    let secret_access_key = store
        .retrieve(&SecretTarget::app("s3", "secret_access_key"))
        .await?;
    Ok((access_key_id, secret_access_key))
}
