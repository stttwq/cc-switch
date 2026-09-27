//! 1Password 后端设置命令（§8）：状态探测、账户/vault 列举、保存配置、测试取钥匙。
//!
//! 所有会调 `op` 的命令都是 async + `spawn_blocking`（`op` 是阻塞子进程，可能等解锁）。

use serde_json::{json, Value};
use tauri::{Emitter, State};

use crate::secrets;
use crate::store::AppState;

/// F1-8：「导入到 1Password 并剥离」——把启动剥离检测到的 live 文件明文钥匙
/// （`live_plaintext_pending`）收进 vault 后就地剥离。会触发 op（可能弹解锁），
/// 故 async + spawn_blocking。返回成功导入的数量。
#[tauri::command]
pub async fn import_live_plaintext_to_onepassword(
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::services::provider::import_live_plaintext_to_vault(&state)
    })
    .await
    .map_err(|e| format!("导入 live 明文任务失败: {e}"))?
    .map_err(|e| e.to_string())
}

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

/// 列出 vault（需解锁：会触发授权弹窗）。`account` 传当前下拉选中值（尚未保存），
/// 缺失时回落已保存设置。
#[tauri::command]
pub async fn onepassword_list_vaults(
    _state: State<'_, AppState>,
    account: Option<String>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let verify = crate::settings::onepassword_verify_signature();
        let path = secrets::locate_op(crate::settings::get_onepassword_op_path().as_deref())
            .ok_or_else(|| crate::error::AppError::from(secrets::VaultError::NotInstalled))?;
        if verify {
            secrets::verify_op_signature(&path).map_err(crate::error::AppError::from)?;
        }
        let account = account
            .filter(|s| !s.trim().is_empty())
            .or_else(crate::settings::get_onepassword_account)
            .ok_or_else(|| {
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
    // F1-7：固定前先校验签名（信任根不能是被篡改的二进制），失败则不写入任何设置。
    if op.op_path.is_none() {
        if let Some(path) = secrets::locate_op(None) {
            let verify = op.verify_signature.unwrap_or(true);
            if verify {
                secrets::verify_op_signature(&path).map_err(|e| e.to_string())?;
            }
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
        let vault =
            crate::secrets::onepassword_from_settings().map_err(crate::error::AppError::from)?;
        let report = crate::secrets::migrate_to_onepassword(&state, &vault)?;
        Ok::<Value, crate::error::AppError>(json!(report))
    })
    .await
    .map_err(|e| format!("迁移任务失败: {e}"))?
    .map_err(|e| e.to_string())
}

/// 轻量查询当前凭据后端标识（不调 op）。前端据此置灰/隐藏相关区块。
#[tauri::command]
pub async fn secret_backend_name(_state: State<'_, AppState>) -> Result<String, String> {
    Ok(crate::settings::get_secret_backend())
}

/// F1-2：端点回填待办数量（0 次 op，只查本地表）。
/// 大于 0 时前端在 1Password 区 / 主界面横幅提示「回填端点」。
#[tauri::command]
pub async fn secrets_endpoint_backfill_status(state: State<'_, AppState>) -> Result<Value, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let pending = state.db.list_endpoint_backfill_pending()?;
        Ok::<_, crate::error::AppError>(json!({ "pending": pending.len() }))
    })
    .await
    .map_err(|e| format!("端点回填状态任务失败: {e}"))?
    .map_err(|e| e.to_string())
}

/// F1-2：存量端点回填——把 vault 里托管的非敏感 base_url 逐个搬到端点表。
/// 每组 1 次 fetch（1P 模式会请求解锁，约 7 秒/个），完成后读取全部 0 次 op。
/// 逐个上报 `secrets-backfill-progress` 事件；单项失败不中断，记入 failed 清单。
#[tauri::command]
pub async fn secrets_backfill_endpoints(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    use crate::app_config::AppType;
    use std::str::FromStr;

    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let pending = state.db.list_endpoint_backfill_pending()?;
        let total = pending.len();
        let mut backfilled = 0usize;
        let mut failed: Vec<String> = Vec::new();
        for (app_str, id) in pending.iter() {
            let done = backfilled + failed.len();
            let _ = app_handle.emit(
                "secrets-backfill-progress",
                serde_json::json!({ "done": done, "total": total }),
            );
            let Ok(app) = AppType::from_str(app_str) else {
                failed.push(format!("{app_str}/{id}"));
                continue;
            };
            match crate::services::provider::resolve_base_url(&state, &app, id) {
                Ok(Some(_)) => backfilled += 1,
                Ok(None) => {
                    // vault 里没有该条目的 base_url（可能条目已删）：无从回填，记为失败
                    // 让用户感知；refs 行保持原样。
                    failed.push(format!("{app_str}/{id}"));
                }
                Err(e) => {
                    log::warn!("端点回填失败 {app_str}/{id}: {e}");
                    failed.push(format!("{app_str}/{id}"));
                }
            }
        }
        let _ = app_handle.emit(
            "secrets-backfill-progress",
            serde_json::json!({ "done": total, "total": total }),
        );
        Ok::<_, crate::error::AppError>(serde_json::json!({
            "total": total,
            "backfilled": backfilled,
            "failed": failed,
        }))
    })
    .await
    .map_err(|e| format!("端点回填任务失败: {e}"))?
    .map_err(|e| e.to_string())
}
