#![allow(non_snake_case)]

//! 端到端加密的口令管理、状态查询与远端重置命令（方案 2.4.2 / 2.4.6 / 附录 A）。
//!
//! 口令只写进 Windows 凭据管理器，永不上传。`reset_remote` 需重输口令确认，删远端
//! v3 并停用；`delete_legacy_remote` 删迁移后遗留的 v2 明文快照（不需口令）。

use serde_json::{json, Value};
use tauri::State;

use crate::error::AppError;
use crate::secrets::sync_secrets::{extract_sync_passphrase, restore_sync_passphrase};
use crate::settings;
use crate::store::AppState;

/// 设置/清除同步口令。三态同 WebDAV 密码：空串删除条目，非空写入。
/// 变更后作废 KEK 缓存，下次同步用新口令重派生。
#[tauri::command]
pub async fn sync_e2e_set_passphrase(
    state: State<'_, AppState>,
    passphrase: String,
) -> Result<Value, String> {
    let stored = extract_sync_passphrase(&state.secrets, Some(&passphrase))
        .await
        .map_err(|e| e.to_string())?;
    // 口令变了（或删了）→ 缓存的 KEK 立即失效。
    state.sync_kek.invalidate();
    Ok(json!({ "stored": stored, "cleared": passphrase.is_empty() }))
}

/// 读取本机加密状态：两传输是否启用、是否已设口令、设备级序号。
#[tauri::command]
pub async fn sync_e2e_get_status(state: State<'_, AppState>) -> Result<Value, String> {
    let passphrase_set = restore_sync_passphrase(&state.secrets)
        .await
        .map_err(|e| e.to_string())?
        .is_some();
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
#[tauri::command]
pub async fn sync_e2e_set_enabled(
    state: State<'_, AppState>,
    transport: String,
    enabled: bool,
    allow_insecure: Option<bool>,
) -> Result<Value, String> {
    if enabled
        && restore_sync_passphrase(&state.secrets)
            .await
            .map_err(|e| e.to_string())?
            .is_none()
    {
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
#[tauri::command]
pub async fn sync_e2e_reset_remote(
    state: State<'_, AppState>,
    transport: String,
    confirm_passphrase: String,
) -> Result<Value, String> {
    let stored = restore_sync_passphrase(&state.secrets)
        .await
        .map_err(|e| e.to_string())?;
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
            crate::services::webdav_sync::reset_remote_e2e(&state.secrets, &settings)
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
            crate::services::s3_sync::reset_remote_e2e(&state.secrets, &settings)
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
#[tauri::command]
pub async fn sync_e2e_delete_legacy_remote(
    state: State<'_, AppState>,
    transport: String,
) -> Result<Value, String> {
    match transport.as_str() {
        "webdav" => {
            let settings = settings::get_webdav_sync_settings()
                .ok_or_else(|| "WebDAV 同步未配置".to_string())?;
            crate::services::webdav_sync::delete_legacy_remote(&state.secrets, &settings)
                .await
                .map_err(|e| e.to_string())?;
        }
        "s3" => {
            let settings =
                settings::get_s3_sync_settings().ok_or_else(|| "S3 同步未配置".to_string())?;
            crate::services::s3_sync::delete_legacy_remote(&state.secrets, &settings)
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
