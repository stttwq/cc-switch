//! WebDAV v2 sync protocol layer with DB compatibility subdirectories.
//!
//! Implements manifest-based synchronization on top of the HTTP transport
//! primitives in [`super::webdav`]. Artifact set: `db.sql` + `skills.zip`.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;
use serde_json::Value;

use crate::error::AppError;
use crate::secrets::sync_secrets::restore_webdav_password;
use crate::secrets::SecretStore;
use crate::services::webdav::{
    auth_from_credentials, build_remote_url, ensure_remote_directories, get_bytes, head_etag,
    path_segments, put_bytes, put_bytes_with_precondition, test_connection, WebDavAuth,
};
use crate::settings::{update_webdav_sync_status, WebDavSyncSettings, WebDavSyncStatus};

pub(crate) use super::sync_protocol::run_with_sync_lock;
use super::sync_protocol::{
    apply_snapshot, build_local_snapshot, e2e_build_upload, e2e_describe_remote,
    e2e_downgrade_blocked_error, e2e_open_download, effective_db_compat_version, localized,
    persist_sync_success_best_effort, require_sync_passphrase, sha256_hex,
    validate_artifact_size_limit, validate_manifest_compat, verify_artifact, ArtifactMeta,
    E2eDownload, RemoteLayout, SyncManifest, DB_COMPAT_VERSION, MAX_MANIFEST_BYTES,
    MAX_SYNC_ARTIFACT_BYTES, PROTOCOL_VERSION, REMOTE_DB_SQL, REMOTE_MANIFEST, REMOTE_SKILLS_ZIP,
};

#[cfg(test)]
pub(crate) fn sync_mutex() -> &'static tokio::sync::Mutex<()> {
    super::sync_protocol::sync_mutex()
}

pub(crate) mod archive;

struct RemoteSnapshot {
    layout: RemoteLayout,
    manifest: SyncManifest,
    manifest_bytes: Vec<u8>,
    manifest_etag: Option<String>,
}
// ─── Public API ──────────────────────────────────────────────

/// Check WebDAV connectivity and ensure remote directory structure.
pub async fn check_connection(
    secrets: &Arc<dyn SecretStore>,
    settings: &WebDavSyncSettings,
    password_override: Option<&str>,
) -> Result<(), AppError> {
    settings.validate()?;
    let auth = auth_for(secrets, settings, password_override).await?;
    test_connection(&settings.base_url, &auth).await?;
    let dir_segs = remote_dir_segments(settings, RemoteLayout::Current);
    ensure_remote_directories(&settings.base_url, &dir_segs, &auth).await?;
    Ok(())
}

/// Upload local snapshot (db + skills) to remote.
pub async fn upload(
    db: &crate::database::Database,
    secrets: &Arc<dyn SecretStore>,
    settings: &mut WebDavSyncSettings,
    kek_cache: &crate::store::SyncKekCache,
) -> Result<Value, AppError> {
    settings.validate()?;
    let auth = auth_for(secrets, settings, None).await?;
    if settings.e2e_enabled {
        return upload_e2e(db, secrets, settings, &auth, kek_cache).await;
    }
    let dir_segs = remote_dir_segments(settings, RemoteLayout::Current);
    ensure_remote_directories(&settings.base_url, &dir_segs, &auth).await?;

    let snapshot = build_local_snapshot(db)?;

    // Upload order: artifacts first, manifest last (best-effort consistency)
    let db_url = remote_file_url(settings, RemoteLayout::Current, REMOTE_DB_SQL)?;
    put_bytes(&db_url, &auth, snapshot.db_sql, "application/sql").await?;

    let skills_url = remote_file_url(settings, RemoteLayout::Current, REMOTE_SKILLS_ZIP)?;
    put_bytes(&skills_url, &auth, snapshot.skills_zip, "application/zip").await?;

    let manifest_url = remote_file_url(settings, RemoteLayout::Current, REMOTE_MANIFEST)?;
    put_bytes(
        &manifest_url,
        &auth,
        snapshot.manifest_bytes,
        "application/json",
    )
    .await?;

    // Fetch etag (best-effort, don't fail the upload)
    let etag = match head_etag(&manifest_url, &auth).await {
        Ok(e) => e,
        Err(e) => {
            log::debug!("[WebDAV] Failed to fetch ETag after upload: {e}");
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

/// E2E v3 上传：拉远端 manifest（算 seq + 沿用盐）→ 本机 seal → 传密文三件套。
async fn upload_e2e(
    db: &crate::database::Database,
    secrets: &Arc<dyn SecretStore>,
    settings: &mut WebDavSyncSettings,
    auth: &WebDavAuth,
    kek_cache: &crate::store::SyncKekCache,
) -> Result<Value, AppError> {
    use crate::services::sync_e2e::{DB_SQL_ENC, SKILLS_ZIP_ENC};
    let dir_segs = remote_dir_segments(settings, RemoteLayout::E2e);
    ensure_remote_directories(&settings.base_url, &dir_segs, auth).await?;

    let manifest_url = remote_file_url(settings, RemoteLayout::E2e, REMOTE_MANIFEST)?;
    let (remote_manifest, remote_etag) =
        match get_bytes(&manifest_url, auth, MAX_MANIFEST_BYTES).await? {
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

    let db_url = remote_file_url(settings, RemoteLayout::E2e, DB_SQL_ENC)?;
    put_bytes(&db_url, auth, upload.db_sql_enc, "application/octet-stream").await?;
    let skills_url = remote_file_url(settings, RemoteLayout::E2e, SKILLS_ZIP_ENC)?;
    put_bytes(
        &skills_url,
        auth,
        upload.skills_zip_enc,
        "application/octet-stream",
    )
    .await?;
    // manifest 是提交点：带条件写，远端被别的设备改过则 412（blob 先传无害）。
    put_bytes_with_precondition(
        &manifest_url,
        auth,
        upload.manifest_bytes,
        "application/json",
        remote_etag.as_deref(),
        remote_etag.is_none(),
    )
    .await?;

    let etag = match head_etag(&manifest_url, auth).await {
        Ok(e) => e,
        Err(e) => {
            log::debug!("[WebDAV] Failed to fetch ETag after upload: {e}");
            None
        }
    };
    let _persisted = persist_sync_success_best_effort(
        settings,
        upload.manifest_hash,
        etag,
        persist_sync_success,
    );
    // 记下本机已上传序号（跨设备并发/回滚检测用）。
    settings.status.last_uploaded_seq = Some(upload.seq);
    let _ = update_webdav_sync_status(settings.status.clone());
    Ok(serde_json::json!({ "status": "uploaded", "seq": upload.seq }))
}

/// Download remote snapshot and apply to local database + skills.
pub async fn download(
    db: &crate::database::Database,
    secrets: &Arc<dyn SecretStore>,
    settings: &mut WebDavSyncSettings,
    kek_cache: &crate::store::SyncKekCache,
    allow_rollback: bool,
) -> Result<Value, AppError> {
    settings.validate()?;
    let auth = auth_for(secrets, settings, None).await?;
    if settings.e2e_enabled {
        return download_e2e(db, secrets, settings, &auth, kek_cache, allow_rollback).await;
    }
    let snapshot = find_remote_snapshot(settings, &auth)
        .await?
        .ok_or_else(|| {
            localized(
                "webdav.sync.remote_empty",
                "远端没有可下载的同步数据",
                "No downloadable sync data found on the remote.",
            )
        })?;

    validate_manifest_compat(&snapshot.manifest, snapshot.layout)?;

    // Download and verify artifacts
    let db_sql = download_and_verify(
        settings,
        &auth,
        snapshot.layout,
        REMOTE_DB_SQL,
        &snapshot.manifest.artifacts,
    )
    .await?;
    let skills_zip = download_and_verify(
        settings,
        &auth,
        snapshot.layout,
        REMOTE_SKILLS_ZIP,
        &snapshot.manifest.artifacts,
    )
    .await?;

    // Apply snapshot
    apply_snapshot(db, &db_sql, &skills_zip)?;

    let manifest_hash = sha256_hex(&snapshot.manifest_bytes);
    let _persisted = persist_sync_success_best_effort(
        settings,
        manifest_hash,
        snapshot.manifest_etag,
        persist_sync_success,
    );
    Ok(serde_json::json!({
        "status": "downloaded",
        "sourceLayout": snapshot.layout.as_str(),
        "sourcePath": remote_dir_display(settings, snapshot.layout),
    }))
}

/// E2E v3 下载：只认 v3 布局；本机开启却只找到 v2 明文快照 → 拒绝降级。
async fn download_e2e(
    db: &crate::database::Database,
    secrets: &Arc<dyn SecretStore>,
    settings: &mut WebDavSyncSettings,
    auth: &WebDavAuth,
    kek_cache: &crate::store::SyncKekCache,
    allow_rollback: bool,
) -> Result<Value, AppError> {
    use crate::services::sync_e2e::{DB_SQL_ENC, SKILLS_ZIP_ENC};
    let manifest_url = remote_file_url(settings, RemoteLayout::E2e, REMOTE_MANIFEST)?;
    let Some((outer_bytes, manifest_etag)) =
        get_bytes(&manifest_url, auth, MAX_MANIFEST_BYTES).await?
    else {
        // v3 目录为空：若 v2 明文快照还在，本机开启加密时不能拿它降级还原。
        if find_remote_snapshot(settings, auth).await?.is_some() {
            return Err(e2e_downgrade_blocked_error());
        }
        return Err(localized(
            "webdav.sync.remote_empty",
            "远端没有可下载的加密快照",
            "No encrypted snapshot found on the remote.",
        ));
    };

    let db_sql_enc = get_enc_artifact(settings, auth, DB_SQL_ENC).await?;
    let skills_zip_enc = get_enc_artifact(settings, auth, SKILLS_ZIP_ENC).await?;

    let passphrase = require_sync_passphrase(secrets).await?;
    let opened = e2e_open_download(
        kek_cache,
        &passphrase,
        &outer_bytes,
        &db_sql_enc,
        &skills_zip_enc,
        settings.status.last_applied_seq.unwrap_or(0),
        allow_rollback,
    )?;
    let (db_sql, skills_zip, seq) = match opened {
        E2eDownload::RollbackConflict {
            remote_seq,
            last_applied,
        } => {
            // 远端比本机已应用的更旧（疑似回滚）：不落地，回结构化冲突给前端。
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
    let _persisted = persist_sync_success_best_effort(
        settings,
        manifest_hash,
        manifest_etag,
        persist_sync_success,
    );
    settings.status.last_applied_seq = Some(seq);
    settings.status.last_uploaded_seq =
        Some(settings.status.last_uploaded_seq.unwrap_or(0).max(seq));
    let _ = update_webdav_sync_status(settings.status.clone());
    Ok(serde_json::json!({
        "status": "downloaded",
        "sourceLayout": RemoteLayout::E2e.as_str(),
        "sourcePath": remote_dir_display(settings, RemoteLayout::E2e),
        "seq": seq,
    }))
}

async fn get_enc_artifact(
    settings: &WebDavSyncSettings,
    auth: &WebDavAuth,
    name: &str,
) -> Result<Vec<u8>, AppError> {
    let url = remote_file_url(settings, RemoteLayout::E2e, name)?;
    let (bytes, _) = get_bytes(&url, auth, MAX_SYNC_ARTIFACT_BYTES as usize)
        .await?
        .ok_or_else(|| {
            localized(
                "webdav.sync.remote_missing_artifact",
                format!("远端缺少加密文件: {name}"),
                format!("Remote encrypted file missing: {name}"),
            )
        })?;
    validate_artifact_size_limit(name, bytes.len() as u64)?;
    Ok(bytes)
}

/// Fetch remote manifest info without downloading artifacts.
pub async fn fetch_remote_info(
    secrets: &Arc<dyn SecretStore>,
    settings: &WebDavSyncSettings,
) -> Result<Option<Value>, AppError> {
    settings.validate()?;
    let auth = auth_for(secrets, settings, None).await?;
    if settings.e2e_enabled {
        return fetch_remote_info_e2e(secrets, settings, &auth).await;
    }
    let Some(snapshot) = find_remote_snapshot(settings, &auth).await? else {
        return Ok(None);
    };
    let compatible = validate_manifest_compat(&snapshot.manifest, snapshot.layout).is_ok();
    let db_compat_version = effective_db_compat_version(&snapshot.manifest, snapshot.layout);

    let payload = serde_json::json!({
        "deviceName": snapshot.manifest.device_name,
        "createdAt": snapshot.manifest.created_at,
        "snapshotId": snapshot.manifest.snapshot_id,
        "version": snapshot.manifest.version,
        "protocolVersion": snapshot.manifest.version,
        "dbCompatVersion": db_compat_version,
        "compatible": compatible,
        "artifacts": snapshot.manifest.artifacts.keys().collect::<Vec<_>>(),
        "layout": snapshot.layout.as_str(),
        "remotePath": remote_dir_display(settings, snapshot.layout),
    });

    Ok(Some(payload))
}

/// E2E 开关开时只认 v3：外层明文摘要判兼容，口令可用则顺带解出设备名/时间。
async fn fetch_remote_info_e2e(
    secrets: &Arc<dyn SecretStore>,
    settings: &WebDavSyncSettings,
    auth: &WebDavAuth,
) -> Result<Option<Value>, AppError> {
    use crate::services::sync_e2e::{DB_SQL_ENC, SKILLS_ZIP_ENC};
    let manifest_url = remote_file_url(settings, RemoteLayout::E2e, REMOTE_MANIFEST)?;
    let Some((outer_bytes, _)) = get_bytes(&manifest_url, auth, MAX_MANIFEST_BYTES).await? else {
        // 预览与下载保持一致：v3 缺失但 v2 明文还在时，直接给降级拒绝，而非"无数据"。
        if find_remote_snapshot(settings, auth).await?.is_some() {
            return Err(e2e_downgrade_blocked_error());
        }
        return Ok(None);
    };
    let info = e2e_describe_remote(secrets, &outer_bytes).await;
    let payload = serde_json::json!({
        "deviceName": info.device_name.unwrap_or_default(),
        "createdAt": info.created_at.unwrap_or_default(),
        "snapshotId": info.snapshot_id,
        "version": crate::services::sync_e2e::E2E_VERSION,
        "protocolVersion": crate::services::sync_e2e::E2E_VERSION,
        "dbCompatVersion": info.db_compat_version,
        "compatible": info.compatible,
        "encrypted": true,
        "seq": info.seq,
        "artifacts": [DB_SQL_ENC, SKILLS_ZIP_ENC],
        "layout": RemoteLayout::E2e.as_str(),
        "remotePath": remote_dir_display(settings, RemoteLayout::E2e),
    });
    Ok(Some(payload))
}

// ─── Sync status persistence ─────────────────────────────────

fn persist_sync_success(
    settings: &mut WebDavSyncSettings,
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
        ..settings.status.clone()
    };
    settings.status = status.clone();
    update_webdav_sync_status(status)
}

async fn find_remote_snapshot(
    settings: &WebDavSyncSettings,
    auth: &WebDavAuth,
) -> Result<Option<RemoteSnapshot>, AppError> {
    if let Some(snapshot) = fetch_remote_snapshot(settings, auth, RemoteLayout::Current).await? {
        return Ok(Some(snapshot));
    }
    fetch_remote_snapshot(settings, auth, RemoteLayout::Legacy).await
}

async fn fetch_remote_snapshot(
    settings: &WebDavSyncSettings,
    auth: &WebDavAuth,
    layout: RemoteLayout,
) -> Result<Option<RemoteSnapshot>, AppError> {
    let manifest_url = remote_file_url(settings, layout, REMOTE_MANIFEST)?;
    let Some((manifest_bytes, manifest_etag)) =
        get_bytes(&manifest_url, auth, MAX_MANIFEST_BYTES).await?
    else {
        return Ok(None);
    };

    let manifest: SyncManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|e| AppError::Json {
            path: REMOTE_MANIFEST.to_string(),
            source: e,
        })?;

    Ok(Some(RemoteSnapshot {
        layout,
        manifest,
        manifest_bytes,
        manifest_etag,
    }))
}
// ─── Download & verify ───────────────────────────────────────

async fn download_and_verify(
    settings: &WebDavSyncSettings,
    auth: &WebDavAuth,
    layout: RemoteLayout,
    artifact_name: &str,
    artifacts: &BTreeMap<String, ArtifactMeta>,
) -> Result<Vec<u8>, AppError> {
    let meta = artifacts.get(artifact_name).ok_or_else(|| {
        localized(
            "webdav.sync.manifest_missing_artifact",
            format!("manifest 中缺少 artifact: {artifact_name}"),
            format!("Manifest missing artifact: {artifact_name}"),
        )
    })?;
    validate_artifact_size_limit(artifact_name, meta.size)?;

    let url = remote_file_url(settings, layout, artifact_name)?;
    let (bytes, _) = get_bytes(&url, auth, MAX_SYNC_ARTIFACT_BYTES as usize)
        .await?
        .ok_or_else(|| {
            localized(
                "webdav.sync.remote_missing_artifact",
                format!("远端缺少 artifact 文件: {artifact_name}"),
                format!("Remote artifact file missing: {artifact_name}"),
            )
        })?;

    verify_artifact(&bytes, artifact_name, meta)?;
    Ok(bytes)
}

// ─── Remote path helpers ─────────────────────────────────────

fn remote_dir_segments(settings: &WebDavSyncSettings, layout: RemoteLayout) -> Vec<String> {
    let mut segs = Vec::new();
    segs.extend(path_segments(&settings.remote_root).map(str::to_string));
    match layout {
        RemoteLayout::E2e => {
            // v3 布局不带 db-v* 子目录（方案 2.4.3）；dbCompatVersion 在 manifest 里。
            segs.push(crate::services::sync_e2e::E2E_DIR.to_string());
        }
        other => {
            segs.push(format!("v{PROTOCOL_VERSION}"));
            if other == RemoteLayout::Current {
                segs.push(format!("db-v{DB_COMPAT_VERSION}"));
            }
        }
    }
    segs.extend(path_segments(&settings.profile).map(str::to_string));
    segs
}

fn remote_file_url(
    settings: &WebDavSyncSettings,
    layout: RemoteLayout,
    file_name: &str,
) -> Result<String, AppError> {
    let mut segs = remote_dir_segments(settings, layout);
    segs.extend(path_segments(file_name).map(str::to_string));
    build_remote_url(&settings.base_url, &segs)
}

fn remote_dir_display(settings: &WebDavSyncSettings, layout: RemoteLayout) -> String {
    let segs = remote_dir_segments(settings, layout);
    format!("/{}", segs.join("/"))
}

async fn auth_for(
    secrets: &Arc<dyn SecretStore>,
    settings: &WebDavSyncSettings,
    password_override: Option<&str>,
) -> Result<WebDavAuth, AppError> {
    // 表单里刚输入但尚未保存的密码优先：否则首次配置点"测试连接"必然失败。
    let password: zeroize::Zeroizing<String> = match password_override {
        Some(password) => zeroize::Zeroizing::new(password.to_string()),
        None => restore_webdav_password(secrets).await?.unwrap_or_default(),
    };
    Ok(auth_from_credentials(&settings.username, &password))
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_dir_segments_uses_current_layout() {
        let settings = WebDavSyncSettings {
            remote_root: "cc-switch-sync".to_string(),
            profile: "default".to_string(),
            ..WebDavSyncSettings::default()
        };
        let segs = remote_dir_segments(&settings, RemoteLayout::Current);
        assert_eq!(segs, vec!["cc-switch-sync", "v2", "db-v6", "default"]);
    }

    #[test]
    fn remote_dir_segments_uses_legacy_layout() {
        let settings = WebDavSyncSettings {
            remote_root: "cc-switch-sync".to_string(),
            profile: "default".to_string(),
            ..WebDavSyncSettings::default()
        };
        let segs = remote_dir_segments(&settings, RemoteLayout::Legacy);
        assert_eq!(segs, vec!["cc-switch-sync", "v2", "default"]);
    }
}
