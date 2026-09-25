//! S3 v2 sync protocol layer.
//!
//! Implements manifest-based synchronization on top of the S3 transport
//! primitives in [`super::s3`]. Artifact set: `db.sql` + `skills.zip`.

use std::collections::BTreeMap;

use chrono::Utc;
use serde_json::Value;

use crate::error::AppError;
use crate::secrets::SyncCredentials;
use crate::services::s3::{self, S3Credentials};
use crate::settings::{update_s3_sync_status, S3SyncSettings, WebDavSyncStatus};

pub(crate) use super::sync_protocol::run_with_sync_lock;
use super::sync_protocol::{
    apply_snapshot, build_local_snapshot, e2e_build_upload, e2e_describe_remote,
    e2e_downgrade_blocked_error, e2e_open_download, e2e_remote_changed_error, localized,
    persist_sync_success_best_effort, require_sync_passphrase, sha256_hex,
    validate_artifact_size_limit, validate_manifest_compat, verify_artifact, ArtifactMeta,
    E2eDownload, RemoteLayout, SyncManifest, DB_COMPAT_VERSION, MAX_MANIFEST_BYTES,
    MAX_SYNC_ARTIFACT_BYTES, PROTOCOL_VERSION, REMOTE_DB_SQL, REMOTE_MANIFEST, REMOTE_SKILLS_ZIP,
};

#[cfg(test)]
pub(crate) fn sync_mutex() -> &'static tokio::sync::Mutex<()> {
    super::sync_protocol::sync_mutex()
}

// ─── Public API ──────────────────────────────────────────────

/// Check S3 connectivity by issuing a HEAD request against the bucket.
pub async fn check_connection(
    secrets: &SyncCredentials,
    settings: &S3SyncSettings,
    credentials_override: Option<(&str, &str)>,
) -> Result<(), AppError> {
    settings.validate()?;
    let creds = creds_for(secrets, settings, credentials_override).await?;
    s3::test_connection(&creds).await
}

/// Upload local snapshot (db + skills) to remote S3.
pub async fn upload(
    db: &crate::database::Database,
    secrets: &SyncCredentials,
    settings: &mut S3SyncSettings,
    kek_cache: &crate::store::SyncKekCache,
) -> Result<Value, AppError> {
    settings.validate()?;
    let creds = creds_for(secrets, settings, None).await?;
    if settings.e2e_enabled {
        return upload_e2e(db, secrets, settings, &creds, kek_cache).await;
    }

    let snapshot = build_local_snapshot(db)?;

    // Upload order: artifacts first, manifest last (best-effort consistency)
    let db_key = s3_key(settings, REMOTE_DB_SQL);
    s3::put_object(&creds, &db_key, snapshot.db_sql, "application/sql").await?;

    let skills_key = s3_key(settings, REMOTE_SKILLS_ZIP);
    s3::put_object(&creds, &skills_key, snapshot.skills_zip, "application/zip").await?;

    let manifest_key = s3_key(settings, REMOTE_MANIFEST);
    s3::put_object(
        &creds,
        &manifest_key,
        snapshot.manifest_bytes,
        "application/json",
    )
    .await?;

    // Fetch etag (best-effort, don't fail the upload)
    let etag = match s3::head_object(&creds, &manifest_key).await {
        Ok(e) => e,
        Err(e) => {
            log::debug!("[S3] Failed to fetch ETag after upload: {e}");
            None
        }
    };

    let _persisted = persist_sync_success_best_effort(
        settings,
        snapshot.manifest_hash,
        etag,
        persist_sync_success,
    );
    Ok(serde_json::json!({ "status": "uploaded" }))
}

async fn upload_e2e(
    db: &crate::database::Database,
    secrets: &SyncCredentials,
    settings: &mut S3SyncSettings,
    creds: &S3Credentials,
    kek_cache: &crate::store::SyncKekCache,
) -> Result<Value, AppError> {
    use crate::services::sync_e2e::{DB_SQL_ENC, SKILLS_ZIP_ENC};
    let manifest_key = s3_key_e2e(settings, REMOTE_MANIFEST);
    let (remote_manifest, remote_etag) =
        match s3::get_object(creds, &manifest_key, MAX_MANIFEST_BYTES).await? {
            Some((bytes, etag)) => (Some(bytes), etag),
            None => (None, None),
        };

    let passphrase = require_sync_passphrase(secrets).await?;
    let upload = e2e_build_upload(
        db,
        kek_cache,
        &passphrase,
        remote_manifest.as_deref(),
        settings.status.last_uploaded_seq.unwrap_or(0),
    )?;

    s3::put_object(
        creds,
        &s3_key_e2e(settings, DB_SQL_ENC),
        upload.db_sql_enc,
        "application/octet-stream",
    )
    .await?;
    s3::put_object(
        creds,
        &s3_key_e2e(settings, SKILLS_ZIP_ENC),
        upload.skills_zip_enc,
        "application/octet-stream",
    )
    .await?;
    // B6：多数 S3 端点不支持条件写，降级为"上传前 HEAD 比较 ETag"（尽力而为，
    // 非原子，窗口内仍可能被抢写；WebDAV 用真正的 If-Match）。ETag 变了则拒。
    if let Some(expected) = remote_etag.as_deref() {
        let current = s3::head_object(creds, &manifest_key).await.ok().flatten();
        if current.as_deref() != Some(expected) {
            return Err(e2e_remote_changed_error());
        }
    }
    s3::put_object(
        creds,
        &manifest_key,
        upload.manifest_bytes,
        "application/json",
    )
    .await?;

    let etag = match s3::head_object(creds, &manifest_key).await {
        Ok(e) => e,
        Err(e) => {
            log::debug!("[S3] Failed to fetch ETag after upload: {e}");
            None
        }
    };
    let _persisted = persist_sync_success_best_effort(
        settings,
        upload.manifest_hash,
        etag,
        persist_sync_success,
    );
    settings.status.last_uploaded_seq = Some(upload.seq);
    let _ = update_s3_sync_status(settings.status.clone());
    Ok(serde_json::json!({ "status": "uploaded", "seq": upload.seq }))
}

/// Download remote snapshot and apply to local database + skills.
pub async fn download(
    db: &crate::database::Database,
    secrets: &SyncCredentials,
    settings: &mut S3SyncSettings,
    kek_cache: &crate::store::SyncKekCache,
    allow_rollback: bool,
) -> Result<Value, AppError> {
    settings.validate()?;
    let creds = creds_for(secrets, settings, None).await?;
    if settings.e2e_enabled {
        return download_e2e(db, secrets, settings, &creds, kek_cache, allow_rollback).await;
    }

    let manifest_key = s3_key(settings, REMOTE_MANIFEST);
    let (manifest_bytes, etag) = s3::get_object(&creds, &manifest_key, MAX_MANIFEST_BYTES)
        .await?
        .ok_or_else(|| {
            localized(
                "s3.sync.remote_empty",
                "远端没有可下载的同步数据",
                "No downloadable sync data found on the remote.",
            )
        })?;

    let manifest: SyncManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|e| AppError::Json {
            path: REMOTE_MANIFEST.to_string(),
            source: e,
        })?;

    validate_manifest_compat(&manifest, RemoteLayout::Current)?;

    // Download and verify artifacts
    let db_sql = download_and_verify(
        secrets,
        settings,
        &creds,
        REMOTE_DB_SQL,
        &manifest.artifacts,
    )
    .await?;
    let skills_zip = download_and_verify(
        secrets,
        settings,
        &creds,
        REMOTE_SKILLS_ZIP,
        &manifest.artifacts,
    )
    .await?;

    // Apply snapshot
    apply_snapshot(db, &db_sql, &skills_zip)?;

    let manifest_hash = sha256_hex(&manifest_bytes);
    let _persisted =
        persist_sync_success_best_effort(settings, manifest_hash, etag, persist_sync_success);
    Ok(serde_json::json!({ "status": "downloaded" }))
}

/// E2E v3 下载：只认 v3 key；本机开启却只有 v2 明文快照 → 拒绝降级。
async fn download_e2e(
    db: &crate::database::Database,
    secrets: &SyncCredentials,
    settings: &mut S3SyncSettings,
    creds: &S3Credentials,
    kek_cache: &crate::store::SyncKekCache,
    allow_rollback: bool,
) -> Result<Value, AppError> {
    use crate::services::sync_e2e::{DB_SQL_ENC, SKILLS_ZIP_ENC};
    let manifest_key = s3_key_e2e(settings, REMOTE_MANIFEST);
    let Some((outer_bytes, etag)) =
        s3::get_object(creds, &manifest_key, MAX_MANIFEST_BYTES).await?
    else {
        let v2_exists = s3::get_object(
            creds,
            &s3_key(settings, REMOTE_MANIFEST),
            MAX_MANIFEST_BYTES,
        )
        .await?
        .is_some();
        if v2_exists {
            return Err(e2e_downgrade_blocked_error());
        }
        return Err(localized(
            "s3.sync.remote_empty",
            "远端没有可下载的加密快照",
            "No encrypted snapshot found on the remote.",
        ));
    };

    let db_sql_enc = get_enc_object(creds, &s3_key_e2e(settings, DB_SQL_ENC), DB_SQL_ENC).await?;
    let skills_zip_enc =
        get_enc_object(creds, &s3_key_e2e(settings, SKILLS_ZIP_ENC), SKILLS_ZIP_ENC).await?;

    let passphrase = require_sync_passphrase(secrets).await?;
    let (db_sql, skills_zip, seq) = match e2e_open_download(
        kek_cache,
        &passphrase,
        &outer_bytes,
        &db_sql_enc,
        &skills_zip_enc,
        settings.status.last_applied_seq.unwrap_or(0),
        allow_rollback,
    )? {
        E2eDownload::RollbackConflict {
            remote_seq,
            last_applied,
        } => {
            return Ok(serde_json::json!({
                "status": "rollbackConflict",
                "remoteSeq": remote_seq,
                "lastApplied": last_applied,
            }));
        }
        E2eDownload::Applied {
            db_sql,
            skills_zip,
            seq,
        } => (db_sql, skills_zip, seq),
    };

    apply_snapshot(db, &db_sql, &skills_zip)?;

    let manifest_hash = sha256_hex(&outer_bytes);
    let _persisted =
        persist_sync_success_best_effort(settings, manifest_hash, etag, persist_sync_success);
    settings.status.last_applied_seq = Some(seq);
    settings.status.last_uploaded_seq =
        Some(settings.status.last_uploaded_seq.unwrap_or(0).max(seq));
    let _ = update_s3_sync_status(settings.status.clone());
    Ok(serde_json::json!({ "status": "downloaded", "seq": seq }))
}

async fn get_enc_object(creds: &S3Credentials, key: &str, name: &str) -> Result<Vec<u8>, AppError> {
    let (bytes, _) = s3::get_object(creds, key, MAX_SYNC_ARTIFACT_BYTES as usize)
        .await?
        .ok_or_else(|| {
            localized(
                "s3.sync.remote_missing_artifact",
                format!("远端缺少加密文件: {name}"),
                format!("Remote encrypted file missing: {name}"),
            )
        })?;
    validate_artifact_size_limit(name, bytes.len() as u64)?;
    Ok(bytes)
}

/// 停用端到端加密：删除远端 v3 三件套（口令确认由命令层做）。
pub async fn reset_remote_e2e(
    secrets: &SyncCredentials,
    settings: &S3SyncSettings,
) -> Result<(), AppError> {
    settings.validate()?;
    let creds = creds_for(secrets, settings, None).await?;
    use crate::services::sync_e2e::{DB_SQL_ENC, SKILLS_ZIP_ENC};
    for name in [DB_SQL_ENC, SKILLS_ZIP_ENC, REMOTE_MANIFEST] {
        s3::delete_object(&creds, &s3_key_e2e(settings, name)).await?;
    }
    Ok(())
}

/// 迁移后清理：删除远端旧版 v2 明文快照，不需要口令。
pub async fn delete_legacy_remote(
    secrets: &SyncCredentials,
    settings: &S3SyncSettings,
) -> Result<(), AppError> {
    settings.validate()?;
    let creds = creds_for(secrets, settings, None).await?;
    for name in [REMOTE_DB_SQL, REMOTE_SKILLS_ZIP, REMOTE_MANIFEST] {
        s3::delete_object(&creds, &s3_key(settings, name)).await?;
    }
    Ok(())
}

/// Fetch remote manifest info without downloading artifacts.
pub async fn fetch_remote_info(
    secrets: &SyncCredentials,
    settings: &S3SyncSettings,
) -> Result<Option<Value>, AppError> {
    settings.validate()?;
    let creds = creds_for(secrets, settings, None).await?;
    if settings.e2e_enabled {
        use crate::services::sync_e2e::{DB_SQL_ENC, E2E_VERSION, SKILLS_ZIP_ENC};
        let manifest_key = s3_key_e2e(settings, REMOTE_MANIFEST);
        let Some((bytes, _)) = s3::get_object(&creds, &manifest_key, MAX_MANIFEST_BYTES).await?
        else {
            return Ok(None);
        };
        let info = e2e_describe_remote(secrets, &bytes).await;
        return Ok(Some(serde_json::json!({
            "deviceName": info.device_name.unwrap_or_default(),
            "createdAt": info.created_at.unwrap_or_default(),
            "snapshotId": info.snapshot_id,
            "version": E2E_VERSION,
            "protocolVersion": E2E_VERSION,
            "dbCompatVersion": info.db_compat_version,
            "compatible": info.compatible,
            "encrypted": true,
            "seq": info.seq,
            "artifacts": [DB_SQL_ENC, SKILLS_ZIP_ENC],
            "layout": RemoteLayout::E2e.as_str(),
            "remotePath": s3_key_e2e(settings, ""),
        })));
    }
    let manifest_key = s3_key(settings, REMOTE_MANIFEST);

    let Some((bytes, _)) = s3::get_object(&creds, &manifest_key, MAX_MANIFEST_BYTES).await? else {
        return Ok(None);
    };

    let manifest: SyncManifest = serde_json::from_slice(&bytes).map_err(|e| AppError::Json {
        path: REMOTE_MANIFEST.to_string(),
        source: e,
    })?;

    let compatible = validate_manifest_compat(&manifest, RemoteLayout::Current).is_ok();

    let payload = serde_json::json!({
        "deviceName": manifest.device_name,
        "createdAt": manifest.created_at,
        "snapshotId": manifest.snapshot_id,
        "version": manifest.version,
        "protocolVersion": manifest.version,
        "dbCompatVersion": manifest.db_compat_version,
        "compatible": compatible,
        "artifacts": manifest.artifacts.keys().collect::<Vec<_>>(),
        "layout": RemoteLayout::Current.as_str(),
        "remotePath": s3_dir_display(settings),
    });

    Ok(Some(payload))
}

// ─── Sync status persistence ─────────────────────────────────

fn persist_sync_success(
    settings: &mut S3SyncSettings,
    manifest_hash: String,
    etag: Option<String>,
) -> Result<(), AppError> {
    let status = WebDavSyncStatus {
        last_sync_at: Some(Utc::now().timestamp()),
        last_error: None,
        last_error_source: None,
        last_local_manifest_hash: Some(manifest_hash.clone()),
        last_remote_manifest_hash: Some(manifest_hash),
        last_remote_etag: etag,
        // E2E 序号是设备级持久状态，成功同步不应清零它，从现有 status 续接。
        ..settings.status.clone()
    };
    settings.status = status.clone();
    update_s3_sync_status(status)
}

// ─── Download & verify ───────────────────────────────────────

async fn download_and_verify(
    _secrets: &SyncCredentials,
    settings: &S3SyncSettings,
    creds: &S3Credentials,
    artifact_name: &str,
    artifacts: &BTreeMap<String, ArtifactMeta>,
) -> Result<Vec<u8>, AppError> {
    let meta = artifacts.get(artifact_name).ok_or_else(|| {
        localized(
            "s3.sync.manifest_missing_artifact",
            format!("manifest 中缺少 artifact: {artifact_name}"),
            format!("Manifest missing artifact: {artifact_name}"),
        )
    })?;
    validate_artifact_size_limit(artifact_name, meta.size)?;

    let key = s3_key(settings, artifact_name);
    let (bytes, _) = s3::get_object(creds, &key, MAX_SYNC_ARTIFACT_BYTES as usize)
        .await?
        .ok_or_else(|| {
            localized(
                "s3.sync.remote_missing_artifact",
                format!("远端缺少 artifact 文件: {artifact_name}"),
                format!("Remote artifact file missing: {artifact_name}"),
            )
        })?;

    verify_artifact(&bytes, artifact_name, meta)?;
    Ok(bytes)
}

// ─── S3 key helpers ──────────────────────────────────────────

/// Build the S3 object key for a given artifact.
///
/// Format: `{remote_root}/v{PROTOCOL_VERSION}/db-v{DB_COMPAT_VERSION}/{profile}/{artifact}`
/// Example: `cc-switch-sync/v2/db-v6/default/manifest.json`
fn s3_key(settings: &S3SyncSettings, artifact: &str) -> String {
    format!(
        "{}/v{}/db-v{}/{}/{}",
        settings.remote_root, PROTOCOL_VERSION, DB_COMPAT_VERSION, settings.profile, artifact
    )
}

/// E2E v3 key：`{remote_root}/v3/{profile}/{artifact}`（不带 db-v* 子目录）。
fn s3_key_e2e(settings: &S3SyncSettings, artifact: &str) -> String {
    format!(
        "{}/{}/{}/{}",
        settings.remote_root,
        crate::services::sync_e2e::E2E_DIR,
        settings.profile,
        artifact
    )
}

fn s3_dir_display(settings: &S3SyncSettings) -> String {
    format!(
        "{}/v{}/db-v{}/{}",
        settings.remote_root, PROTOCOL_VERSION, DB_COMPAT_VERSION, settings.profile
    )
}

async fn creds_for(
    secrets: &SyncCredentials,
    settings: &S3SyncSettings,
    credentials_override: Option<(&str, &str)>,
) -> Result<S3Credentials, AppError> {
    // 表单里刚输入但尚未保存的密钥优先：否则首次配置点"测试连接"必然失败。
    let (access_key_id, secret_access_key) = match credentials_override {
        Some((access_key_id, secret_access_key)) => {
            (access_key_id.to_string(), secret_access_key.to_string())
        }
        None => {
            let (access_key_id, secret_access_key) =
                (secrets.s3_access_key_id.clone(), secrets.s3_secret_access_key.clone());
            (
                access_key_id.map(|key| key.to_string()).unwrap_or_default(),
                secret_access_key
                    .map(|key| key.to_string())
                    .unwrap_or_default(),
            )
        }
    };
    Ok(S3Credentials {
        access_key_id,
        secret_access_key,
        region: settings.region.clone(),
        bucket: settings.bucket.clone(),
        endpoint: settings.endpoint.clone(),
    })
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_settings() -> S3SyncSettings {
        S3SyncSettings {
            remote_root: "cc-switch-sync".to_string(),
            profile: "default".to_string(),
            ..S3SyncSettings::default()
        }
    }

    #[test]
    fn s3_key_uses_v2_and_correct_format() {
        let settings = test_settings();
        let key = s3_key(&settings, "manifest.json");
        assert_eq!(key, "cc-switch-sync/v2/db-v6/default/manifest.json");
    }

    #[test]
    fn s3_key_with_custom_profile() {
        let settings = S3SyncSettings {
            remote_root: "my-root".to_string(),
            profile: "work".to_string(),
            ..S3SyncSettings::default()
        };
        assert_eq!(s3_key(&settings, "db.sql"), "my-root/v2/db-v6/work/db.sql");
    }

    #[test]
    fn s3_key_matches_expected_pattern() {
        let settings = test_settings();
        let key = s3_key(&settings, "skills.zip");
        // Should follow {remote_root}/v{version}/db-v{db}/{profile}/{artifact}
        let parts: Vec<&str> = key.splitn(5, '/').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0], "cc-switch-sync");
        assert_eq!(parts[1], "v2");
        assert_eq!(parts[2], "db-v6");
        assert_eq!(parts[3], "default");
        assert_eq!(parts[4], "skills.zip");
    }

    #[test]
    fn sync_mutex_is_singleton() {
        let m1 = sync_mutex();
        let m2 = sync_mutex();
        assert!(
            std::ptr::eq(m1, m2),
            "sync_mutex must return the same instance"
        );
    }

    // Test removed - credential mapping now handled by SecretStore in Phase 2B
}
