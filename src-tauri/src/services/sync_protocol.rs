//! Transport-agnostic sync protocol layer.
//!
//! Shared by WebDAV, S3, and future transports. Artifact set: `db.sql` + `skills.zip`.

use std::collections::BTreeMap;
use std::fs;
use std::future::Future;
use std::process::Command;
use std::sync::OnceLock;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

use crate::error::AppError;
use crate::services::skill::{skill_state_read_guard, skill_state_write_guard};

// Re-export archive functions for use by transport layers.
pub(crate) use super::webdav_sync::archive::{
    backup_current_skills, restore_skills_from_backup, restore_skills_zip, zip_skills_ssot,
};

// ─── Protocol constants ──────────────────────────────────────

/// Wire-format identifier stored in remote manifests.
/// Retains historic "webdav" naming for backward compatibility with existing remotes.
pub(crate) const PROTOCOL_FORMAT: &str = "cc-switch-webdav-sync";
pub(crate) const PROTOCOL_VERSION: u32 = 2;
pub(crate) const DB_COMPAT_VERSION: u32 = 6;
pub(crate) const LEGACY_DB_COMPAT_VERSION: u32 = 5;
pub(crate) const REMOTE_DB_SQL: &str = "db.sql";
pub(crate) const REMOTE_SKILLS_ZIP: &str = "skills.zip";
pub(crate) const REMOTE_MANIFEST: &str = "manifest.json";
pub(crate) const MAX_DEVICE_NAME_LEN: usize = 64;
pub(crate) const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_SYNC_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;

// ─── Sync operation lock ────────────────────────────────────

/// Serialize every snapshot upload/download across all transports.
///
/// WebDAV and S3 used to own separate mutexes, which allowed two transports to
/// restore the database and Skills SSOT concurrently. Keep the lock in this
/// transport-agnostic layer so future transports automatically share it too.
pub(crate) fn sync_mutex() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

pub(crate) async fn run_with_sync_lock<T, Fut>(operation: Fut) -> Result<T, AppError>
where
    Fut: Future<Output = Result<T, AppError>>,
{
    let _guard = sync_mutex().lock().await;
    operation.await
}

/// Tables whose changes make the remote configuration snapshot stale.
///
/// Keep this transport-agnostic so WebDAV and S3 cannot silently drift apart.
/// Only tables that survive schema v19 belong here.
pub(crate) fn should_trigger_auto_sync_for_table(table: &str) -> bool {
    let normalized = table.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "providers"
            | "mcp_servers"
            | "prompts"
            | "skills"
            | "skill_repos"
            | "profiles"
            | "settings"
    )
}

// ─── Error helpers ───────────────────────────────────────────

pub(crate) fn localized(
    key: &'static str,
    zh: impl Into<String>,
    en: impl Into<String>,
) -> AppError {
    AppError::localized(key, zh, en)
}

pub(crate) fn io_context_localized(
    _key: &'static str,
    zh: impl Into<String>,
    en: impl Into<String>,
    source: std::io::Error,
) -> AppError {
    let zh_msg = zh.into();
    let en_msg = en.into();
    AppError::IoContext {
        context: format!("{zh_msg} ({en_msg})"),
        source,
    }
}

// ─── Types ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SyncManifest {
    pub format: String,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_compat_version: Option<u32>,
    pub device_name: String,
    pub created_at: String,
    pub artifacts: BTreeMap<String, ArtifactMeta>,
    pub snapshot_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ArtifactMeta {
    pub sha256: String,
    pub size: u64,
}

pub(crate) struct LocalSnapshot {
    pub db_sql: Vec<u8>,
    pub skills_zip: Vec<u8>,
    pub manifest_bytes: Vec<u8>,
    pub manifest_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteLayout {
    Current,
    Legacy,
    /// E2E v3 布局：`{root}/v3/{profile}/`，无 `db-v*` 子目录（方案 2.4.3）。
    E2e,
}

impl RemoteLayout {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Legacy => "legacy",
            Self::E2e => "e2e",
        }
    }
}

// ─── Snapshot building ───────────────────────────────────────

pub(crate) fn build_local_snapshot(
    db: &crate::database::Database,
) -> Result<LocalSnapshot, AppError> {
    // Keep the DB's skill rows and the filesystem SSOT at one logical point in
    // time. Skill writers take the matching write guard around both mutations.
    let _skill_state_guard = skill_state_read_guard();

    // Export database to SQL string
    let sql_string = db.export_sql_string_for_sync()?;
    let db_sql = sql_string.into_bytes();

    // Pack skills into deterministic ZIP
    let tmp = tempdir().map_err(|e| {
        io_context_localized(
            "sync.snapshot_tmpdir_failed",
            "创建快照临时目录失败",
            "Failed to create temporary directory for snapshot",
            e,
        )
    })?;
    let skills_zip_path = tmp.path().join(REMOTE_SKILLS_ZIP);
    zip_skills_ssot(&skills_zip_path)?;
    let skills_zip = fs::read(&skills_zip_path).map_err(|e| AppError::io(&skills_zip_path, e))?;

    // Build artifact map and compute hashes
    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        REMOTE_DB_SQL.to_string(),
        ArtifactMeta {
            sha256: sha256_hex(&db_sql),
            size: db_sql.len() as u64,
        },
    );
    artifacts.insert(
        REMOTE_SKILLS_ZIP.to_string(),
        ArtifactMeta {
            sha256: sha256_hex(&skills_zip),
            size: skills_zip.len() as u64,
        },
    );

    let snapshot_id = compute_snapshot_id(&artifacts);
    let manifest = SyncManifest {
        format: PROTOCOL_FORMAT.to_string(),
        version: PROTOCOL_VERSION,
        db_compat_version: Some(DB_COMPAT_VERSION),
        device_name: detect_system_device_name().unwrap_or_else(|| "Unknown Device".to_string()),
        created_at: Utc::now().to_rfc3339(),
        artifacts,
        snapshot_id,
    };
    let manifest_bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|e| AppError::JsonSerialize { source: e })?;
    let manifest_hash = sha256_hex(&manifest_bytes);

    Ok(LocalSnapshot {
        db_sql,
        skills_zip,
        manifest_bytes,
        manifest_hash,
    })
}

// ─── Manifest handling ───────────────────────────────────────

/// Compute a deterministic snapshot identity from artifact hashes.
///
/// BTreeMap iteration order is sorted by key, ensuring stability.
pub(crate) fn compute_snapshot_id(artifacts: &BTreeMap<String, ArtifactMeta>) -> String {
    let parts: Vec<String> = artifacts
        .iter()
        .map(|(name, meta)| format!("{}:{}", name, meta.sha256))
        .collect();
    sha256_hex(parts.join("|").as_bytes())
}

pub(crate) fn effective_db_compat_version(
    manifest: &SyncManifest,
    layout: RemoteLayout,
) -> Option<u32> {
    manifest
        .db_compat_version
        .or_else(|| (layout == RemoteLayout::Legacy).then_some(LEGACY_DB_COMPAT_VERSION))
}

pub(crate) fn validate_manifest_compat(
    manifest: &SyncManifest,
    layout: RemoteLayout,
) -> Result<(), AppError> {
    if manifest.format != PROTOCOL_FORMAT {
        return Err(localized(
            "sync.manifest_format_incompatible",
            format!("远端 manifest 格式不兼容: {}", manifest.format),
            format!(
                "Remote manifest format is incompatible: {}",
                manifest.format
            ),
        ));
    }
    if manifest.version != PROTOCOL_VERSION {
        return Err(localized(
            "sync.manifest_version_incompatible",
            format!(
                "远端 manifest 协议版本不兼容: v{} (本地 v{PROTOCOL_VERSION})",
                manifest.version
            ),
            format!(
                "Remote manifest protocol version is incompatible: v{} (local v{PROTOCOL_VERSION})",
                manifest.version
            ),
        ));
    }
    let Some(db_compat_version) = effective_db_compat_version(manifest, layout) else {
        return Err(localized(
            "sync.manifest_db_version_missing",
            "远端 manifest 缺少数据库兼容版本",
            "Remote manifest is missing the database compatibility version.",
        ));
    };
    match layout {
        RemoteLayout::Current if db_compat_version != DB_COMPAT_VERSION => {
            return Err(localized(
                "sync.manifest_db_version_incompatible",
                format!(
                    "远端数据库快照版本不兼容: db-v{db_compat_version} (本地 db-v{DB_COMPAT_VERSION})"
                ),
                format!(
                    "Remote database snapshot version is incompatible: db-v{db_compat_version} (local db-v{DB_COMPAT_VERSION})"
                ),
            ));
        }
        RemoteLayout::Legacy if db_compat_version > DB_COMPAT_VERSION => {
            return Err(localized(
                "sync.manifest_db_version_incompatible",
                format!(
                    "远端数据库快照版本不兼容: db-v{db_compat_version} (本地最高支持 db-v{DB_COMPAT_VERSION})"
                ),
                format!(
                    "Remote database snapshot version is incompatible: db-v{db_compat_version} (local supports up to db-v{DB_COMPAT_VERSION})"
                ),
            ));
        }
        _ => {}
    }
    Ok(())
}

// ─── Artifact verification ───────────────────────────────────

pub(crate) fn validate_artifact_size_limit(artifact_name: &str, size: u64) -> Result<(), AppError> {
    if size > MAX_SYNC_ARTIFACT_BYTES {
        let max_mb = MAX_SYNC_ARTIFACT_BYTES / 1024 / 1024;
        return Err(localized(
            "sync.artifact_too_large",
            format!("artifact {artifact_name} 超过下载上限（{} MB）", max_mb),
            format!(
                "Artifact {artifact_name} exceeds download limit ({} MB)",
                max_mb
            ),
        ));
    }
    Ok(())
}

/// Verify that downloaded artifact bytes match the expected size and SHA-256 hash.
pub(crate) fn verify_artifact(
    bytes: &[u8],
    artifact_name: &str,
    meta: &ArtifactMeta,
) -> Result<(), AppError> {
    // Quick size check before expensive hash
    if bytes.len() as u64 != meta.size {
        return Err(localized(
            "sync.artifact_size_mismatch",
            format!(
                "artifact {artifact_name} 大小不匹配 (expected: {}, got: {})",
                meta.size,
                bytes.len(),
            ),
            format!(
                "Artifact {artifact_name} size mismatch (expected: {}, got: {})",
                meta.size,
                bytes.len(),
            ),
        ));
    }

    let actual_hash = sha256_hex(bytes);
    if actual_hash != meta.sha256 {
        return Err(localized(
            "sync.artifact_hash_mismatch",
            format!(
                "artifact {artifact_name} SHA256 校验失败 (expected: {}..., got: {}...)",
                meta.sha256.get(..8).unwrap_or(&meta.sha256),
                actual_hash.get(..8).unwrap_or(&actual_hash),
            ),
            format!(
                "Artifact {artifact_name} SHA256 verification failed (expected: {}..., got: {}...)",
                meta.sha256.get(..8).unwrap_or(&meta.sha256),
                actual_hash.get(..8).unwrap_or(&actual_hash),
            ),
        ));
    }
    Ok(())
}

// ─── Snapshot application ────────────────────────────────────

pub(crate) fn apply_snapshot(
    db: &crate::database::Database,
    db_sql: &[u8],
    skills_zip: &[u8],
) -> Result<(), AppError> {
    let sql_str = std::str::from_utf8(db_sql).map_err(|e| {
        localized(
            "sync.sql_not_utf8",
            format!("SQL 非 UTF-8: {e}"),
            format!("SQL is not valid UTF-8: {e}"),
        )
    })?;
    // Exclude installs, uninstalls, updates, and local projection while Skills
    // are backed up/replaced and the corresponding database snapshot is applied.
    let _skill_state_guard = skill_state_write_guard();
    let skills_backup = backup_current_skills()?;

    // Replace skills first, then import database; roll back skills on DB failure.
    restore_skills_zip(skills_zip)?;

    if let Err(db_err) = db.import_sql_string_for_sync(sql_str) {
        if let Err(rollback_err) = restore_skills_from_backup(&skills_backup) {
            return Err(localized(
                "sync.db_import_and_rollback_failed",
                format!("导入数据库失败: {db_err}; 同时回滚 Skills 失败: {rollback_err}"),
                format!(
                    "Database import failed: {db_err}; skills rollback also failed: {rollback_err}"
                ),
            ));
        }
        return Err(db_err);
    }

    Ok(())
}

// ─── End-to-end encryption orchestration (E2E-3) ─────────────

// 传输层不感知加密：upload/download 各自把远端 v3 三件套（manifest.json +
// `db.sql.enc` + `skills.zip.enc`）搬到这里的 seal/open 编排上。KEK 由 `cache`
// 按盐派生并缓存（方案 2.4.2），口令只在本机、永不上传。

/// 上传前算好的 v3 载荷。
pub(crate) struct E2eUpload {
    pub manifest_bytes: Vec<u8>,
    pub manifest_hash: String,
    pub db_sql_enc: Vec<u8>,
    pub skills_zip_enc: Vec<u8>,
    pub seq: u64,
}

/// 本机 E2E 已开启却读到 v2 明文布局——拒绝降级（方案 2.4.3）。
pub(crate) fn e2e_downgrade_blocked_error() -> AppError {
    localized(
        "sync.e2e.v2_rejected",
        "远端是未加密的旧版（v2）快照，而本机已启用同步加密；请先在另一台设备或本页用「迁移远端」把它升级为加密快照",
        "The remote holds an unencrypted legacy (v2) snapshot while this device has sync encryption enabled; migrate the remote to encrypted first",
    )
}

/// 上传时发现远端已被其他设备改动（WebDAV 412 / S3 尽力而为 HEAD 比较）——方案 2.4.4。
pub(crate) fn e2e_remote_changed_error() -> AppError {
    localized(
        "sync.e2e.remote_changed",
        "远端已被其他设备更新，请先下载最新快照再上传",
        "The remote was updated by another device; download the latest snapshot before uploading",
    )
}

/// 由本机明文快照 + 远端（可选）现有 manifest 生成 v3 密文载荷。
///
/// `remote_manifest_bytes` = 远端已存在的 v3 外层 manifest 字节（首次上传传 `None`）。
/// `seq = max(last_uploaded_seq, 远端 seq) + 1`；盐沿用远端（保持口令稳定），无远端才新生成。
pub(crate) fn e2e_build_upload(
    db: &crate::database::Database,
    cache: &crate::store::SyncKekCache,
    passphrase: &str,
    remote_manifest_bytes: Option<&[u8]>,
    last_uploaded_seq: u64,
) -> Result<E2eUpload, AppError> {
    let snapshot = build_local_snapshot(db)?;

    let remote_outer = remote_manifest_bytes.and_then(|bytes| parse_e2e_outer_manifest(bytes).ok());
    let remote_seq = remote_outer.as_ref().map(|o| o.seq);
    let seq = crate::services::sync_e2e::next_seq(last_uploaded_seq, remote_seq);
    let kdf = remote_outer
        .map(|o| o.kdf)
        .unwrap_or_else(crate::services::sync_e2e::new_kdf_params);

    let kek = cache.get_or_derive(passphrase, &kdf)?;
    let inputs = crate::services::sync_e2e::SealInputs {
        db_sql: &snapshot.db_sql,
        skills_zip: &snapshot.skills_zip,
        device_name: detect_system_device_name().unwrap_or_else(|| "Unknown Device".to_string()),
        created_at: Utc::now().to_rfc3339(),
        seq,
        db_compat_version: DB_COMPAT_VERSION,
        kdf: Some(kdf),
    };
    let sealed = crate::services::sync_e2e::seal_with_kek(&inputs, &kek)?;
    Ok(E2eUpload {
        manifest_bytes: sealed.manifest_bytes,
        manifest_hash: sealed.manifest_hash,
        db_sql_enc: sealed.db_sql_enc,
        skills_zip_enc: sealed.skills_zip_enc,
        seq,
    })
}

/// 下载后按固定顺序解开 v3 载荷并做序号回滚检测，返回明文 db.sql/skills.zip 与新序号。
/// 任何一步失败都在 `apply_snapshot` 之前返回 `Err`，本地库不动（方案 2.4.5）。
pub(crate) fn e2e_open_download(
    cache: &crate::store::SyncKekCache,
    passphrase: &str,
    outer_bytes: &[u8],
    db_sql_enc: &[u8],
    skills_zip_enc: &[u8],
    last_applied_seq: u64,
    allow_rollback: bool,
) -> Result<E2eDownload, AppError> {
    use crate::services::sync_e2e as e2e;
    let outer = parse_e2e_outer_manifest(outer_bytes)?;
    // 回滚检测在解密之前：`seq` 在外层明文 manifest 里，无需口令即可比对。
    if let Some((remote_seq, last_applied)) =
        e2e::detect_seq_regression(outer.seq, last_applied_seq, allow_rollback)
    {
        return Ok(E2eDownload::RollbackConflict {
            remote_seq,
            last_applied,
        });
    }
    let kek = cache.get_or_derive(passphrase, &outer.kdf)?;
    let opened = e2e::open_manifest_with_kek(outer, &kek)?;
    let db_sql = e2e::open_artifact(&opened, e2e::DB_SQL_ENC, REMOTE_DB_SQL, db_sql_enc)?;
    let skills_zip = e2e::open_artifact(
        &opened,
        e2e::SKILLS_ZIP_ENC,
        REMOTE_SKILLS_ZIP,
        skills_zip_enc,
    )?;
    Ok(E2eDownload::Applied {
        db_sql,
        skills_zip,
        seq: opened.outer.seq,
    })
}

/// `e2e_open_download` 的结果：正常可应用，或检测到远端回滚需用户显式放行。
pub(crate) enum E2eDownload {
    Applied {
        db_sql: Vec<u8>,
        skills_zip: Vec<u8>,
        seq: u64,
    },
    RollbackConflict {
        remote_seq: u64,
        last_applied: u64,
    },
}

/// 复用 sync_e2e 的解析，但把错误透传出来供 `Option` 场景使用。
fn parse_e2e_outer_manifest(
    bytes: &[u8],
) -> Result<crate::services::sync_e2e::OuterManifest, AppError> {
    crate::services::sync_e2e::parse_outer_manifest(bytes)
}

/// 供 fetch_remote_info 用的 v3 摘要（方案 2.4.3）。
pub(crate) struct E2eRemoteInfo {
    pub device_name: Option<String>,
    pub created_at: Option<String>,
    pub snapshot_id: String,
    pub db_compat_version: u32,
    pub seq: u64,
    pub compatible: bool,
}

/// 解析 v3 外层 manifest：兼容判定只依赖明文；口令可用时顺带解出设备名/时间做预览，
/// 解不出（未设/错口令）也照常返回摘要——下载预览不该被口令挡住。
pub(crate) async fn e2e_describe_remote(
    secrets: &std::sync::Arc<dyn crate::secrets::SecretStore>,
    outer_bytes: &[u8],
) -> E2eRemoteInfo {
    let outer = match parse_e2e_outer_manifest(outer_bytes) {
        Ok(o) => o,
        Err(_) => {
            return E2eRemoteInfo {
                device_name: None,
                created_at: None,
                snapshot_id: String::new(),
                db_compat_version: 0,
                seq: 0,
                compatible: false,
            }
        }
    };
    let compatible = outer.db_compat_version == DB_COMPAT_VERSION;
    let (device_name, created_at) =
        match crate::secrets::sync_secrets::restore_sync_passphrase(secrets).await {
            Ok(Some(passphrase)) => {
                match crate::services::sync_e2e::derive_kek(&passphrase, &outer.kdf) {
                    Ok(kek) => {
                        match crate::services::sync_e2e::open_manifest_with_kek(outer.clone(), &kek)
                        {
                            Ok(opened) => (
                                Some(opened.inner.device_name),
                                Some(opened.inner.created_at),
                            ),
                            Err(_) => (None, None),
                        }
                    }
                    Err(_) => (None, None),
                }
            }
            _ => (None, None),
        };
    E2eRemoteInfo {
        device_name,
        created_at,
        snapshot_id: outer.snapshot_id,
        db_compat_version: outer.db_compat_version,
        seq: outer.seq,
        compatible,
    }
}

/// 读取本机同步口令；未设置时给出可操作错误（口令永不上上传，方案 2.4.2）。
/// 两个传输层共用。
pub(crate) async fn require_sync_passphrase(
    secrets: &std::sync::Arc<dyn crate::secrets::SecretStore>,
) -> Result<zeroize::Zeroizing<String>, AppError> {
    crate::secrets::sync_secrets::restore_sync_passphrase(secrets)
        .await?
        .ok_or_else(|| {
            localized(
                "sync.e2e.passphrase_required",
                "尚未设置同步口令，请在设置的同步加密区设置口令",
                "Sync passphrase is not set; configure it in the sync encryption settings",
            )
        })
}

// ─── Transport security (E2E-5) ──────────────────────────────

/// 判断 URL 主机是否是本机/私网（localhost、回环 IP、RFC1918、IPv6 回环/唯一本地）。
/// 空串或无法解析按"非私网"处理（交给上层据此拒绝 http）。
fn host_is_local_or_private(url_str: &str) -> bool {
    let Ok(u) = url::Url::parse(url_str.trim()) else {
        return false;
    };
    let Some(host) = u.host_str() else {
        return false;
    };
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => match ip {
            std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
            std::net::IpAddr::V6(v6) => v6.is_loopback() || v6_unique_local(v6),
        },
        // 域名（如 `nas.local`）无法在本地判定是否私网：按非私网处理。
        Err(_) => false,
    }
}

fn v6_unique_local(v6: std::net::Ipv6Addr) -> bool {
    // fc00::/7 唯一本地地址
    (v6.segments()[0] & 0xfe00) == 0xfc00
}

/// `http://` 端点默认拒绝，除非主机是本机/私网**且**用户勾选"允许不安全连接"
/// （方案 2.4.5）。`https://` 始终放行；空端点（如 S3 走 AWS 默认 https）放行。
pub(crate) fn ensure_transport_endpoint_secure(
    url_str: &str,
    allow_insecure: bool,
) -> Result<(), AppError> {
    let trimmed = url_str.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let Ok(u) = url::Url::parse(trimmed) else {
        return Ok(()); // 更基础的格式校验由各传输的 parse 负责
    };
    if u.scheme() != "http" {
        return Ok(());
    }
    let private = host_is_local_or_private(trimmed);
    if private && allow_insecure {
        return Ok(());
    }
    let (key, zh, en) = if private {
        (
            "sync.insecure.local_needs_consent",
            "本机/私网的 http 明文地址需先在设置里勾选「允许不安全连接」",
            "Plaintext http to a local/private host requires enabling 'Allow insecure connection'",
        )
    } else {
        (
            "sync.insecure.public_http_forbidden",
            "明文密码会经 http 过线，公网地址必须使用 https",
            "Passwords would cross the wire in plaintext over http; public endpoints must use https",
        )
    };
    Err(localized(key, zh, en))
}

// ─── Utilities ───────────────────────────────────────────────

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub(crate) fn detect_system_device_name() -> Option<String> {
    let env_name = ["CC_SWITCH_DEVICE_NAME", "COMPUTERNAME", "HOSTNAME"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .find_map(|value| normalize_device_name(&value));

    if env_name.is_some() {
        return env_name;
    }

    let output = Command::new("hostname").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let hostname = String::from_utf8(output.stdout).ok()?;
    normalize_device_name(&hostname)
}

pub(crate) fn normalize_device_name(raw: &str) -> Option<String> {
    let compact = raw
        .chars()
        .fold(String::with_capacity(raw.len()), |mut acc, ch| {
            if ch.is_whitespace() {
                acc.push(' ');
            } else if !ch.is_control() {
                acc.push(ch);
            }
            acc
        });
    let normalized = compact.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return None;
    }

    let limited = trimmed
        .chars()
        .take(MAX_DEVICE_NAME_LEN)
        .collect::<String>();
    if limited.is_empty() {
        None
    } else {
        Some(limited)
    }
}

// ─── Sync status persistence ─────────────────────────────────

pub(crate) fn persist_sync_success_best_effort<S, F>(
    settings: &mut S,
    manifest_hash: String,
    etag: Option<String>,
    persist_fn: F,
) -> bool
where
    F: FnOnce(&mut S, String, Option<String>) -> Result<(), AppError>,
{
    match persist_fn(settings, manifest_hash, etag) {
        Ok(()) => true,
        Err(err) => {
            log::warn!("[Sync] Persist sync status failed, keep operation success: {err}");
            false
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn webdav_and_s3_operations_share_one_sync_mutex() {
        let webdav_lock = crate::services::webdav_sync::sync_mutex();
        let s3_lock = crate::services::s3_sync::sync_mutex();
        assert!(
            std::ptr::eq(webdav_lock, s3_lock),
            "every transport must expose the same global sync lock"
        );

        let guard = webdav_lock.lock().await;
        assert!(s3_lock.try_lock().is_err());
        drop(guard);
        assert!(s3_lock.try_lock().is_ok());
    }

    #[test]
    fn endpoint_security_gates_http_by_host_and_consent() {
        // https 恒放行
        assert!(ensure_transport_endpoint_secure("https://dav.example.com/dav", false).is_ok());
        // 空 endpoint（S3 走 AWS 默认 https）放行
        assert!(ensure_transport_endpoint_secure("", false).is_ok());
        // 公网 http：即使勾选也拒（明文密码过线）
        assert!(ensure_transport_endpoint_secure("http://dav.example.com/dav", true).is_err());
        // 私网/本机 http：未勾选拒，勾选放行
        assert!(ensure_transport_endpoint_secure("http://192.168.1.10/dav", false).is_err());
        assert!(ensure_transport_endpoint_secure("http://192.168.1.10/dav", true).is_ok());
        assert!(ensure_transport_endpoint_secure("http://127.0.0.1:8080/dav", true).is_ok());
        assert!(ensure_transport_endpoint_secure("http://localhost:8080/dav", false).is_err());
        assert!(ensure_transport_endpoint_secure("http://localhost:8080/dav", true).is_ok());
        // 域名形式的内网（nas.local）无法本地判定 → 视作公网 http，拒
        assert!(ensure_transport_endpoint_secure("http://nas.local/dav", true).is_err());
    }

    fn artifact(sha256: &str, size: u64) -> ArtifactMeta {
        ArtifactMeta {
            sha256: sha256.to_string(),
            size,
        }
    }

    #[test]
    fn auto_sync_table_filter_covers_shared_configuration() {
        for table in [
            "providers",
            "mcp_servers",
            "prompts",
            "skills",
            "skill_repos",
            "profiles",
            "settings",
        ] {
            assert!(
                should_trigger_auto_sync_for_table(table),
                "{table} should trigger an automatic snapshot upload"
            );
        }

        assert!(should_trigger_auto_sync_for_table("  PROFILES  "));
        for table in [
            "proxy_request_logs",
            "provider_health",
            "session_log_sync",
            "model_pricing",
            // schema v19 已 DROP（§7.1 / D9），出现在清单里只会误导后来人
            "provider_endpoints",
            "proxy_config",
        ] {
            assert!(
                !should_trigger_auto_sync_for_table(table),
                "{table} should not trigger automatic snapshot upload"
            );
        }
    }

    #[test]
    fn snapshot_id_is_stable() {
        let mut artifacts = BTreeMap::new();
        artifacts.insert("db.sql".to_string(), artifact("abc123", 100));
        artifacts.insert("skills.zip".to_string(), artifact("def456", 200));

        let id1 = compute_snapshot_id(&artifacts);
        let id2 = compute_snapshot_id(&artifacts);
        assert_eq!(id1, id2);
    }

    #[test]
    fn snapshot_id_changes_with_artifacts() {
        let mut a1 = BTreeMap::new();
        a1.insert("db.sql".to_string(), artifact("hash-a", 1));

        let mut a2 = BTreeMap::new();
        a2.insert("db.sql".to_string(), artifact("hash-b", 1));

        assert_ne!(compute_snapshot_id(&a1), compute_snapshot_id(&a2));
    }

    #[test]
    fn sha256_hex_is_correct() {
        let hash = sha256_hex(b"hello");
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn persist_best_effort_returns_true_on_success() {
        let mut dummy = ();
        let ok = persist_sync_success_best_effort(
            &mut dummy,
            "hash".to_string(),
            Some("etag".to_string()),
            |_settings, _hash, _etag| Ok(()),
        );
        assert!(ok);
    }

    #[test]
    fn persist_best_effort_returns_false_on_error() {
        let mut dummy = ();
        let ok = persist_sync_success_best_effort(
            &mut dummy,
            "hash".to_string(),
            None,
            |_settings, _hash, _etag| Err(AppError::Config("boom".to_string())),
        );
        assert!(!ok);
    }

    fn manifest_with(format: &str, version: u32, db_compat_version: Option<u32>) -> SyncManifest {
        let mut artifacts = BTreeMap::new();
        artifacts.insert("db.sql".to_string(), artifact("abc", 1));
        artifacts.insert("skills.zip".to_string(), artifact("def", 2));
        SyncManifest {
            format: format.to_string(),
            version,
            db_compat_version,
            device_name: "My MacBook".to_string(),
            created_at: "2026-02-12T00:00:00Z".to_string(),
            artifacts,
            snapshot_id: "snap-1".to_string(),
        }
    }

    #[test]
    fn validate_manifest_compat_accepts_supported_manifest() {
        let manifest = manifest_with(PROTOCOL_FORMAT, PROTOCOL_VERSION, Some(DB_COMPAT_VERSION));
        assert!(validate_manifest_compat(&manifest, RemoteLayout::Current).is_ok());
    }

    #[test]
    fn validate_manifest_compat_rejects_wrong_format() {
        let manifest = manifest_with("other-format", PROTOCOL_VERSION, Some(DB_COMPAT_VERSION));
        assert!(validate_manifest_compat(&manifest, RemoteLayout::Current).is_err());
    }

    #[test]
    fn validate_manifest_compat_rejects_wrong_version() {
        let manifest = manifest_with(
            PROTOCOL_FORMAT,
            PROTOCOL_VERSION + 1,
            Some(DB_COMPAT_VERSION),
        );
        assert!(validate_manifest_compat(&manifest, RemoteLayout::Current).is_err());
    }

    #[test]
    fn validate_manifest_compat_accepts_legacy_manifest_without_db_compat() {
        let manifest = manifest_with(PROTOCOL_FORMAT, PROTOCOL_VERSION, None);
        assert!(validate_manifest_compat(&manifest, RemoteLayout::Legacy).is_ok());
    }

    #[test]
    fn validate_manifest_compat_rejects_current_manifest_with_wrong_db_compat() {
        let manifest = manifest_with(
            PROTOCOL_FORMAT,
            PROTOCOL_VERSION,
            Some(LEGACY_DB_COMPAT_VERSION),
        );
        assert!(validate_manifest_compat(&manifest, RemoteLayout::Current).is_err());
    }

    #[test]
    fn validate_manifest_compat_rejects_legacy_manifest_from_newer_db_generation() {
        let manifest = manifest_with(
            PROTOCOL_FORMAT,
            PROTOCOL_VERSION,
            Some(DB_COMPAT_VERSION + 1),
        );
        assert!(validate_manifest_compat(&manifest, RemoteLayout::Legacy).is_err());
    }

    #[test]
    fn effective_db_compat_version_defaults_legacy_layout_to_v5() {
        let manifest = manifest_with(PROTOCOL_FORMAT, PROTOCOL_VERSION, None);
        assert_eq!(
            effective_db_compat_version(&manifest, RemoteLayout::Legacy),
            Some(LEGACY_DB_COMPAT_VERSION)
        );
        assert_eq!(
            effective_db_compat_version(&manifest, RemoteLayout::Current),
            None
        );
    }

    #[test]
    fn normalize_device_name_returns_none_for_blank_input() {
        assert_eq!(normalize_device_name("   \n\t  "), None);
    }

    #[test]
    fn normalize_device_name_collapses_whitespace_and_drops_control_chars() {
        assert_eq!(
            normalize_device_name("  Mac\tBook \n Pro\u{0007} "),
            Some("Mac Book Pro".to_string())
        );
    }

    #[test]
    fn normalize_device_name_truncates_to_max_len() {
        let long = "a".repeat(80);
        assert_eq!(normalize_device_name(&long).map(|s| s.len()), Some(64));
    }

    #[test]
    fn manifest_serialization_uses_device_name_only() {
        let manifest = manifest_with(PROTOCOL_FORMAT, PROTOCOL_VERSION, Some(DB_COMPAT_VERSION));
        let value = serde_json::to_value(&manifest).expect("serialize manifest");
        assert!(
            value.get("deviceName").is_some(),
            "manifest should contain deviceName"
        );
        assert_eq!(
            value.get("dbCompatVersion").and_then(|v| v.as_u64()),
            Some(DB_COMPAT_VERSION as u64)
        );
        assert!(
            value.get("deviceId").is_none(),
            "manifest should not contain deviceId"
        );
    }

    #[test]
    fn validate_artifact_size_limit_rejects_oversized_artifacts() {
        let err = validate_artifact_size_limit("skills.zip", MAX_SYNC_ARTIFACT_BYTES + 1)
            .expect_err("artifact larger than limit should be rejected");
        assert!(
            err.to_string().contains("too large") || err.to_string().contains("超过"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_artifact_size_limit_accepts_limit_boundary() {
        assert!(validate_artifact_size_limit("skills.zip", MAX_SYNC_ARTIFACT_BYTES).is_ok());
    }

    #[test]
    fn verify_artifact_rejects_size_mismatch() {
        let meta = artifact("abc123", 100);
        let bytes = vec![0u8; 50];
        let err = verify_artifact(&bytes, "test.bin", &meta)
            .expect_err("size mismatch should be rejected");
        assert!(
            err.to_string().contains("mismatch") || err.to_string().contains("不匹配"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn verify_artifact_rejects_hash_mismatch() {
        let meta = ArtifactMeta {
            sha256: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            size: 5,
        };
        let bytes = b"hello";
        let err = verify_artifact(bytes, "test.bin", &meta)
            .expect_err("hash mismatch should be rejected");
        assert!(
            err.to_string().contains("verification failed") || err.to_string().contains("校验失败"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn verify_artifact_accepts_matching_data() {
        let data = b"hello";
        let meta = ArtifactMeta {
            sha256: sha256_hex(data),
            size: data.len() as u64,
        };
        assert!(verify_artifact(data, "test.bin", &meta).is_ok());
    }

    #[test]
    fn e2e_open_download_roundtrip_and_seq_regression() {
        use crate::services::sync_e2e as e2e;
        use crate::store::SyncKekCache;

        let db_sql = b"BEGIN;INSERT INTO providers VALUES('a');COMMIT;";
        let skills = b"PK\x03\x04zip";
        let sealed = e2e::seal(
            &e2e::SealInputs {
                db_sql,
                skills_zip: skills,
                device_name: "DEV".to_string(),
                created_at: "2026-09-21T00:00:00Z".to_string(),
                seq: 5,
                db_compat_version: DB_COMPAT_VERSION,
                kdf: Some(e2e::new_kdf_params()),
            },
            "pw",
        )
        .expect("seal");

        let cache = SyncKekCache::default();
        let opened = e2e_open_download(
            &cache,
            "pw",
            &sealed.manifest_bytes,
            &sealed.db_sql_enc,
            &sealed.skills_zip_enc,
            0,
            false,
        )
        .expect("open download");
        let (out_db, out_skills, seq) = match opened {
            E2eDownload::Applied {
                db_sql,
                skills_zip,
                seq,
            } => (db_sql, skills_zip, seq),
            E2eDownload::RollbackConflict { .. } => {
                panic!("fresh device must not see rollback conflict")
            }
        };
        assert_eq!(out_db, db_sql);
        assert_eq!(out_skills, skills);
        assert_eq!(seq, 5);

        // 本机已应用 seq=9 > 远端 5 → 返回回滚冲突（默认拒绝），库不动。
        let conflict = e2e_open_download(
            &cache,
            "pw",
            &sealed.manifest_bytes,
            &sealed.db_sql_enc,
            &sealed.skills_zip_enc,
            9,
            false,
        )
        .expect("regression is a conflict, not an error");
        match conflict {
            E2eDownload::RollbackConflict {
                remote_seq,
                last_applied,
            } => {
                assert_eq!((remote_seq, last_applied), (5, 9));
            }
            E2eDownload::Applied { .. } => panic!("seq regression must be blocked"),
        }
        // 用户显式接受回滚 → 放行；且第二次命中 KEK 缓存（不重跑 Argon2）
        let allowed = e2e_open_download(
            &cache,
            "pw",
            &sealed.manifest_bytes,
            &sealed.db_sql_enc,
            &sealed.skills_zip_enc,
            9,
            true,
        )
        .expect("allow_rollback passes");
        assert!(matches!(allowed, E2eDownload::Applied { .. }));
    }

    #[test]
    fn e2e_open_download_rejects_wrong_passphrase() {
        use crate::services::sync_e2e as e2e;
        use crate::store::SyncKekCache;
        let sealed = e2e::seal(
            &e2e::SealInputs {
                db_sql: b"db",
                skills_zip: b"zip",
                device_name: "D".to_string(),
                created_at: "2026-09-21T00:00:00Z".to_string(),
                seq: 1,
                db_compat_version: DB_COMPAT_VERSION,
                kdf: Some(e2e::new_kdf_params()),
            },
            "right-pw",
        )
        .expect("seal");
        let cache = SyncKekCache::default();
        assert!(e2e_open_download(
            &cache,
            "wrong-pw",
            &sealed.manifest_bytes,
            &sealed.db_sql_enc,
            &sealed.skills_zip_enc,
            0,
            false,
        )
        .is_err());
    }
}
