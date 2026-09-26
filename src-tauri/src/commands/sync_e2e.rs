#![allow(non_snake_case)]

//! 端到端加密的口令管理、状态查询与远端重置命令（方案 2.4.2 / 2.4.6 / 附录 A）。
//!
//! 口令只写进 Windows 凭据管理器，永不上传。`reset_remote` 需重输口令确认，删远端
//! v3 并停用；`delete_legacy_remote` 删迁移后遗留的 v2 明文快照（不需口令）。

use serde_json::{json, Value};
use tauri::State;

use crate::error::AppError;
use crate::secrets::{fetch_sync_credentials, store_sync_passphrase};
use crate::settings;
use crate::store::AppState;

/// 设置/清除同步口令。三态同 WebDAV 密码：空串删除条目，非空写入。
/// 变更后作废 KEK 缓存，下次同步用新口令重派生。
#[tauri::command]
pub async fn sync_e2e_set_passphrase(
    state: State<'_, AppState>,
    passphrase: String,
) -> Result<Value, String> {
    let stored =
        store_sync_passphrase(&state.vault, Some(&passphrase)).map_err(|e| e.to_string())?;
    // 口令变了（或删了）→ 缓存的 KEK 立即失效。
    state.sync_kek.invalidate();
    Ok(json!({ "stored": stored, "cleared": passphrase.is_empty() }))
}

/// 读取本机加密状态：两传输是否启用、是否已设口令、设备级序号。
#[tauri::command]
pub async fn sync_e2e_get_status(state: State<'_, AppState>) -> Result<Value, String> {
    sync_e2e_status_core(&state.vault)
}

/// `sync_e2e_get_status` 的可测核心（F0-3）：只依赖 vault，便于用 `CountingVault`
/// 断言「读状态 0 次 op」。F3-1 将把口令判定改为查 `secret_refs`（彻底不 fetch）。
pub(crate) fn sync_e2e_status_core(
    vault: &std::sync::Arc<dyn crate::secrets::SecretVault>,
) -> Result<Value, String> {
    let passphrase_set = fetch_sync_credentials(vault)
        .map_err(|e| e.to_string())?
        .e2e_passphrase
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
        && fetch_sync_credentials(&state.vault)
            .map_err(|e| e.to_string())?
            .e2e_passphrase
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
    let creds = fetch_sync_credentials(&state.vault).map_err(|e| e.to_string())?;
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
#[tauri::command]
pub async fn sync_e2e_delete_legacy_remote(
    state: State<'_, AppState>,
    transport: String,
) -> Result<Value, String> {
    let creds = fetch_sync_credentials(&state.vault).map_err(|e| e.to_string())?;
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
    use crate::secrets::{store_s3_credentials, CountingVault, InMemoryVault, SecretVault};
    use std::sync::Arc;

    fn counting() -> Arc<CountingVault> {
        let inner: Arc<dyn SecretVault> = Arc::new(InMemoryVault::new());
        Arc::new(CountingVault::new(inner))
    }

    /// `sync_e2e_get_status` 只为判断「口令是否已设」就 fetch 整包 → 展开设置页
    /// 就可能触发解锁。F3-1 改查 secret_refs 后启用本断言。
    #[test]
    #[ignore = "F3-1 后启用"]
    fn sync_e2e_status_is_zero_fetch() {
        let counting = counting();
        super::sync_e2e_status_core(&(counting.clone() as Arc<dyn SecretVault>)).expect("status");
        assert_eq!(counting.fetch_count(), 0, "状态查询必须 0 次 fetch");
    }

    /// 保存一次 S3 设置（双字段）当前是 2×(fetch+put)；F1-1 的
    /// `update_app_sync` 改为一次 fetch → 一次 put 后启用。
    #[test]
    #[ignore = "F1-1 后启用"]
    fn store_s3_credentials_is_one_roundtrip() {
        let counting = counting();
        store_s3_credentials(
            &(counting.clone() as Arc<dyn SecretVault>),
            Some("AKIA123"),
            Some("secret456"),
        )
        .expect("store");
        assert_eq!(counting.fetch_count(), 1, "两个字段一次 fetch");
        assert_eq!(counting.put_count(), 1, "两个字段一次 put");
    }
}
