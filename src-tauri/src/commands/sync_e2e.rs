#![allow(non_snake_case)]

//! 端到端加密的口令管理、状态查询与远端重置命令（方案 2.4.2 / 2.4.6 / 附录 A）。
//!
//! 口令只写进 Windows 凭据管理器，永不上传。`reset_remote` 需重输口令确认，删远端
//! v3 并停用；`delete_legacy_remote` 删迁移后遗留的 v2 明文快照（不需口令）。

use serde_json::{json, Value};
use tauri::State;

use crate::database::Database;
use crate::error::AppError;
use crate::secrets::FIELD_APP_E2E_PASSPHRASE;
use crate::secrets::{fetch_sync_credentials, store_sync_passphrase};
use crate::settings;
use crate::store::AppState;

/// F3-1（P1-3）：判断「口令是否已设」改查 `secret_refs` 的 `_app/_sync` 字段清单
/// （0 次 op）。AppSync 每次写入都会在同一处维护该引用行（陷阱 10），字段清单里
/// 出现 `app.e2e_passphrase` 即视为已设。
///
/// 注意：2.3.0 及更早版本写入口令时未登记 `_app/_sync` 引用行，这类存量数据会
/// 被判为「未设」——重新保存一次口令即可补登记。
fn e2e_passphrase_set(db: &Database) -> Result<bool, AppError> {
    Ok(db
        .get_secret_ref_fields("_app", "_sync")?
        .is_some_and(|fields| fields.iter().any(|f| f == FIELD_APP_E2E_PASSPHRASE)))
}

/// 设置/清除同步口令。三态同 WebDAV 密码：空串删除条目，非空写入。
/// 变更后作废 KEK 缓存，下次同步用新口令重派生。
///
/// F3-1（P1-3）：写 vault 是阻塞调用（1P 模式下可能等解锁），包进 `spawn_blocking`。
#[tauri::command]
pub async fn sync_e2e_set_passphrase(
    state: State<'_, AppState>,
    passphrase: String,
) -> Result<Value, String> {
    let vault = state.vault.clone();
    let db = state.db.clone();
    let cleared = passphrase.is_empty();
    let stored = tauri::async_runtime::spawn_blocking(move || {
        store_sync_passphrase(&vault, &db, Some(&passphrase))
    })
    .await
    .map_err(|e| format!("同步口令任务执行失败: {e}"))?
    .map_err(|e| e.to_string())?;
    // 口令变了（或删了）→ 缓存的 KEK 立即失效。
    state.sync_kek.invalidate();
    Ok(json!({ "stored": stored, "cleared": cleared }))
}

/// 读取本机加密状态：两传输是否启用、是否已设口令、设备级序号。
///
/// F3-1（P1-3）：展开设置页「云同步」区不再触发任何 op / 解锁弹窗。
#[tauri::command]
pub async fn sync_e2e_get_status(state: State<'_, AppState>) -> Result<Value, String> {
    sync_e2e_status_core(&state.db)
}

/// `sync_e2e_get_status` 的可测核心（F0-3）：只依赖 DB，便于断言「读状态 0 次 op」。
pub(crate) fn sync_e2e_status_core(db: &Database) -> Result<Value, String> {
    let passphrase_set = e2e_passphrase_set(db).map_err(|e| e.to_string())?;
    let webdav = settings::get_webdav_sync_settings();
    let s3 = settings::get_s3_sync_settings();
    Ok(json!({
        "passphraseSet": passphrase_set,
        "webdav": {
            "e2eEnabled": webdav.as_ref().map(|s| s.e2e_enabled).unwrap_or(false),
            "allowInsecure": webdav.as_ref().map(|s| s.allow_insecure).unwrap_or(false),
            "lastAppliedSeq": webdav.as_ref().and_then(|s| s.status.last_applied_seq),
            "lastUploadedSeq": webdav.as_ref().and_then(|s| s.status.last_uploaded_seq),
        },
        "s3": {
            "e2eEnabled": s3.as_ref().map(|s| s.e2e_enabled).unwrap_or(false),
            "allowInsecure": s3.as_ref().map(|s| s.allow_insecure).unwrap_or(false),
            "lastAppliedSeq": s3.as_ref().and_then(|s| s.status.last_applied_seq),
            "lastUploadedSeq": s3.as_ref().and_then(|s| s.status.last_uploaded_seq),
        },
    }))
}

/// 翻某传输的端到端加密开关（`transport` = "webdav" | "s3"）。`allow_insecure`
/// 为 `Some` 时一并改（管 http）。只翻开关——生成 v3 加密快照由用户手动点"上传"。
///
/// 启用前 fail-closed 校验：必须先设口令（否则加密无从谈起），并走一次
/// `validate()`（http 端点需先勾"允许不安全连接"）。
///
/// F3-1（P1-3）：口令判定查 `secret_refs`（0 次 op），见 [`e2e_passphrase_set`]。
#[tauri::command]
pub async fn sync_e2e_set_enabled(
    state: State<'_, AppState>,
    transport: String,
    enabled: bool,
    allow_insecure: Option<bool>,
) -> Result<Value, String> {
    if enabled && !e2e_passphrase_set(&state.db).map_err(|e| e.to_string())? {
        return Err(AppError::localized(
            "sync.e2e.passphrase_required",
            "启用端到端加密前请先设置同步口令",
            "Set a sync passphrase before enabling end-to-end encryption",
        )
        .to_string());
    }
    let changed = match transport.as_str() {
        "webdav" => {
            let mut s = settings::get_webdav_sync_settings()
                .ok_or_else(|| "WebDAV 同步未配置".to_string())?;
            s.e2e_enabled = enabled;
            if let Some(v) = allow_insecure {
                s.allow_insecure = v;
            }
            // 只在启用时校验（http 端点需先勾允许不安全）；关闭/取消勾选不该被门禁挡住。
            if enabled {
                s.validate().map_err(|e| e.to_string())?;
            }
            settings::set_webdav_sync_settings(Some(s.clone())).map_err(|e| e.to_string())?;
            (s.e2e_enabled, s.allow_insecure)
        }
        "s3" => {
            let mut s =
                settings::get_s3_sync_settings().ok_or_else(|| "S3 同步未配置".to_string())?;
            s.e2e_enabled = enabled;
            if let Some(v) = allow_insecure {
                s.allow_insecure = v;
            }
            if enabled {
                s.validate().map_err(|e| e.to_string())?;
            }
            settings::set_s3_sync_settings(Some(s.clone())).map_err(|e| e.to_string())?;
            (s.e2e_enabled, s.allow_insecure)
        }
        other => {
            return Err(AppError::localized(
                "sync.e2e.transport_unknown",
                format!("未知的同步传输类型: {other}"),
                format!("Unknown sync transport: {other}"),
            )
            .to_string())
        }
    };
    Ok(json!({ "e2eEnabled": changed.0, "allowInsecure": changed.1 }))
}

/// 停用端到端加密并清空远端 v3 快照（方案 2.4.6）。需重输口令确认（fail-closed：
/// 口令不符拒删）。删除成功后把 `e2e_enabled` 置假、清设备级序号。不提供降级上传。
///
/// F3-1（P1-3）：取口令是阻塞调用，包进 `spawn_blocking`。
#[tauri::command]
pub async fn sync_e2e_reset_remote(
    state: State<'_, AppState>,
    transport: String,
    confirm_passphrase: String,
) -> Result<Value, String> {
    let vault = state.vault.clone();
    let creds = tauri::async_runtime::spawn_blocking(move || fetch_sync_credentials(&vault))
        .await
        .map_err(|e| format!("读取同步口令任务失败: {e}"))?
        .map_err(|e| e.to_string())?;
    let stored = creds.e2e_passphrase.clone();
    let stored = stored.ok_or_else(|| {
        AppError::localized(
            "sync.e2e.passphrase_required",
            "尚未设置同步口令",
            "Sync passphrase is not set",
        )
        .to_string()
    })?;
    if stored.as_str() != confirm_passphrase {
        return Err(AppError::localized(
            "sync.e2e.reset_passphrase_mismatch",
            "口令不正确，已取消停用",
            "Passphrase mismatch; disable cancelled",
        )
        .to_string());
    }

    match transport.as_str() {
        "webdav" => {
            let settings = settings::get_webdav_sync_settings()
                .ok_or_else(|| "WebDAV 同步未配置".to_string())?;
            crate::services::webdav_sync::reset_remote_e2e(&creds, &settings)
                .await
                .map_err(|e| e.to_string())?;
            if let Some(mut s) = settings::get_webdav_sync_settings() {
                s.e2e_enabled = false;
                s.status.last_applied_seq = None;
                s.status.last_uploaded_seq = None;
                settings::set_webdav_sync_settings(Some(s)).map_err(|e| e.to_string())?;
            }
        }
        "s3" => {
            let settings =
                settings::get_s3_sync_settings().ok_or_else(|| "S3 同步未配置".to_string())?;
            crate::services::s3_sync::reset_remote_e2e(&creds, &settings)
                .await
                .map_err(|e| e.to_string())?;
            if let Some(mut s) = settings::get_s3_sync_settings() {
                s.e2e_enabled = false;
                s.status.last_applied_seq = None;
                s.status.last_uploaded_seq = None;
                settings::set_s3_sync_settings(Some(s)).map_err(|e| e.to_string())?;
            }
        }
        other => {
            return Err(AppError::localized(
                "sync.e2e.transport_unknown",
                format!("未知的同步传输类型: {other}"),
                format!("Unknown sync transport: {other}"),
            )
            .to_string())
        }
    }
    state.sync_kek.invalidate();
    Ok(json!({ "reset": true }))
}

/// 迁移后清理：删除远端旧版 v2 明文快照（方案 2.4.6）。不需口令（删的是明文遗留）。
///
/// F3-1（P1-3）：取凭据是阻塞调用，包进 `spawn_blocking`。
#[tauri::command]
pub async fn sync_e2e_delete_legacy_remote(
    state: State<'_, AppState>,
    transport: String,
) -> Result<Value, String> {
    let vault = state.vault.clone();
    let creds = tauri::async_runtime::spawn_blocking(move || fetch_sync_credentials(&vault))
        .await
        .map_err(|e| format!("读取同步凭据任务失败: {e}"))?
        .map_err(|e| e.to_string())?;
    match transport.as_str() {
        "webdav" => {
            let settings = settings::get_webdav_sync_settings()
                .ok_or_else(|| "WebDAV 同步未配置".to_string())?;
            crate::services::webdav_sync::delete_legacy_remote(&creds, &settings)
                .await
                .map_err(|e| e.to_string())?;
        }
        "s3" => {
            let settings =
                settings::get_s3_sync_settings().ok_or_else(|| "S3 同步未配置".to_string())?;
            crate::services::s3_sync::delete_legacy_remote(&creds, &settings)
                .await
                .map_err(|e| e.to_string())?;
        }
        other => {
            return Err(AppError::localized(
                "sync.e2e.transport_unknown",
                format!("未知的同步传输类型: {other}"),
                format!("Unknown sync transport: {other}"),
            )
            .to_string())
        }
    }
    Ok(json!({ "deleted": true }))
}

#[cfg(test)]
mod op_count_tests {
    //! F0-3 次数断言（§9.3）：状态查询 0 次 op；保存 S3 设置 fetch=1、put=1。
    use super::sync_e2e_status_core;
    use crate::secrets::{
        store_s3_credentials, store_sync_passphrase, CountingVault, InMemoryVault, SecretVault,
    };
    use std::sync::Arc;

    fn counting() -> Arc<CountingVault> {
        let inner: Arc<dyn SecretVault> = Arc::new(InMemoryVault::new());
        Arc::new(CountingVault::new(inner))
    }

    /// F3-1：`sync_e2e_get_status` 只查 `secret_refs`（0 次 op），展开设置页
    /// 「云同步」区不再触发解锁。
    #[test]
    fn sync_e2e_status_is_zero_fetch() {
        let counting = counting();
        let db = Arc::new(crate::database::Database::memory().expect("db"));
        let stored = store_sync_passphrase(
            &(counting.clone() as Arc<dyn SecretVault>),
            &db,
            Some("pass"),
        )
        .expect("store");
        assert!(stored, "口令应写入");
        counting.reset();

        let status = sync_e2e_status_core(&db).expect("status");
        assert_eq!(
            status.get("passphraseSet").and_then(|v| v.as_bool()),
            Some(true),
            "refs 里登记了口令字段 → 已设"
        );
        assert_eq!(counting.fetch_count(), 0, "状态查询必须 0 次 fetch");

        // 未登记引用行 → 判为未设，同样 0 次 op。
        let empty_db = Arc::new(crate::database::Database::memory().expect("db"));
        let status = sync_e2e_status_core(&empty_db).expect("status");
        assert_eq!(
            status.get("passphraseSet").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(counting.fetch_count(), 0);
    }

    /// 保存一次 S3 设置（双字段）= 1×(fetch+put)：F1-1 的
    /// `update_app_sync` 一次 fetch → 一次 put。
    #[test]
    fn store_s3_credentials_is_one_roundtrip() {
        let counting = counting();
        store_s3_credentials(
            &(counting.clone() as Arc<dyn SecretVault>),
            &crate::database::Database::memory().expect("db"),
            Some("AKIA123"),
            Some("secret456"),
        )
        .expect("store");
        assert_eq!(counting.fetch_count(), 1, "两个字段一次 fetch");
        assert_eq!(counting.put_count(), 1, "两个字段一次 put");
    }
}
