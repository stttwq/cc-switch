//! 1Password 后端设置命令（§8）：状态探测、账户/vault 列举、保存配置、测试取钥匙。
//!
//! 所有会调 `op` 的命令都是 async + `spawn_blocking`（`op` 是阻塞子进程，可能等解锁）。

use serde_json::{json, Value};
use tauri::State;

use crate::secrets;
use crate::store::AppState;

/// 状态探测（不需解锁）：是否安装、op 版本、路径、是否已登录、签名校验结果，
/// 外加当前后端与已配置的 account/vault。
#[tauri::command]
pub async fn onepassword_status(_state: State<'_, AppState>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let verify = crate::settings::onepassword_verify_signature();
        let configured = crate::settings::get_onepassword_op_path();
        let probe = secrets::onepassword_probe(configured.as_deref(), verify);
        json!({
            "installed": probe.installed,
            "opPath": probe.op_path,
            "version": probe.version,
            "signedIn": probe.signed_in,
            "signatureOk": probe.signature_ok,
            "backend": crate::settings::get_secret_backend(),
            "account": crate::settings::get_onepassword_account(),
            "vault": crate::settings::get_onepassword_vault(),
            "verifySignature": verify,
        })
    })
    .await
    .map_err(|e| format!("1Password 状态探测任务失败: {e}"))
}

/// 列出账户（不需解锁）。
#[tauri::command]
pub async fn onepassword_list_accounts(_state: State<'_, AppState>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let verify = crate::settings::onepassword_verify_signature();
        let path = secrets::locate_op(crate::settings::get_onepassword_op_path().as_deref())
            .ok_or_else(|| crate::error::AppError::from(secrets::VaultError::NotInstalled))?;
        if verify {
            secrets::verify_op_signature(&path).map_err(crate::error::AppError::from)?;
        }
        let accounts = secrets::list_accounts(&path).map_err(crate::error::AppError::from)?;
        Ok::<Value, crate::error::AppError>(json!(accounts))
    })
    .await
    .map_err(|e| format!("列出 1Password 账户任务失败: {e}"))?
    .map_err(|e| e.to_string())
}

/// 列出 vault（需解锁：会触发授权弹窗）。
#[tauri::command]
pub async fn onepassword_list_vaults(_state: State<'_, AppState>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let verify = crate::settings::onepassword_verify_signature();
        let path = secrets::locate_op(crate::settings::get_onepassword_op_path().as_deref())
            .ok_or_else(|| crate::error::AppError::from(secrets::VaultError::NotInstalled))?;
        if verify {
            secrets::verify_op_signature(&path).map_err(crate::error::AppError::from)?;
        }
        let account = crate::settings::get_onepassword_account().ok_or_else(|| {
            crate::error::AppError::from(secrets::VaultError::Other("请先选择账户".to_string()))
        })?;
        let vaults = secrets::list_vaults(&path, &account).map_err(crate::error::AppError::from)?;
        Ok::<Value, crate::error::AppError>(json!(vaults))
    })
    .await
    .map_err(|e| format!("列出 1Password vault 任务失败: {e}"))?
    .map_err(|e| e.to_string())
}

/// 保存 1Password 配置（account / vault / 是否校验签名）。同时把定位到的 op 绝对路径固定下来。
#[tauri::command]
pub async fn onepassword_save_config(
    _state: State<'_, AppState>,
    account: Option<String>,
    vault: Option<String>,
    #[allow(non_snake_case)] verifySignature: Option<bool>,
) -> Result<Value, String> {
    let mut settings = crate::settings::get_settings();
    let mut op = settings.onepassword.clone().unwrap_or_default();
    if let Some(a) = account {
        op.account = Some(a).filter(|s| !s.trim().is_empty());
    }
    if let Some(v) = vault {
        op.vault = Some(v).filter(|s| !s.trim().is_empty());
    }
    if let Some(vs) = verifySignature {
        op.verify_signature = Some(vs);
    }
    // 固定 op 绝对路径，之后每次直接用它（防搜索路径劫持）。
    if op.op_path.is_none() {
        if let Some(path) = secrets::locate_op(None) {
            op.op_path = Some(path.to_string_lossy().to_string());
        }
    }
    settings.onepassword = Some(op);
    crate::settings::update_settings(settings).map_err(|e| e.to_string())?;
    Ok(json!({ "success": true }))
}

/// 测试取钥匙：用当前配置构造 1Password 后端，触发一次需要解锁的调用（列 vault）。
/// 成功即证明「解锁 + 账户 + vault」链路可用；失败返回分类错误码供前端提示。
#[tauri::command]
pub async fn onepassword_test_fetch(_state: State<'_, AppState>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let verify = crate::settings::onepassword_verify_signature();
        let path = secrets::locate_op(crate::settings::get_onepassword_op_path().as_deref())
            .ok_or_else(|| crate::error::AppError::from(secrets::VaultError::NotInstalled))?;
        if verify {
            secrets::verify_op_signature(&path).map_err(crate::error::AppError::from)?;
        }
        let account = crate::settings::get_onepassword_account().ok_or_else(|| {
            crate::error::AppError::from(secrets::VaultError::Other("请先选择账户".to_string()))
        })?;
        // 列 vault 需要解锁，用作「测试取钥匙」的最小可用性验证。
        secrets::list_vaults(&path, &account).map_err(crate::error::AppError::from)?;
        Ok::<Value, crate::error::AppError>(json!({ "ok": true }))
    })
    .await
    .map_err(|e| format!("测试取钥匙任务失败: {e}"))?
    .map_err(|e| e.to_string())
}

/// 迁移向导（§7）：把凭据管理器里的 cc-switch 条目迁到 1Password，校验后删除并切后端。
/// 需先在设置里选好 account/vault。会触发解锁弹窗。
#[tauri::command]
pub async fn onepassword_migrate(state: State<'_, AppState>) -> Result<Value, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let vault = crate::secrets::onepassword_from_settings()
            .map_err(crate::error::AppError::from)?;
        let report = crate::secrets::migrate_to_onepassword(&state, &vault)?;
        Ok::<Value, crate::error::AppError>(json!(report))
    })
    .await
    .map_err(|e| format!("迁移任务失败: {e}"))?
    .map_err(|e| e.to_string())
}
