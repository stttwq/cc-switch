use crate::app_config::AppType;
use crate::services::env_checker::{
    check_env_conflicts as check_conflicts, delete_env_vars as remove_vars, EnvConflict,
};
use crate::services::ProviderService;
use crate::store::AppState;
use std::str::FromStr;
use tauri::State;

/// §5.3.3：只读扫描该应用的用户级/系统级同名冲突变量（值仅回传末 4 位）。
#[tauri::command]
pub fn env_delivery_scan(app: String) -> Result<Vec<EnvConflict>, String> {
    check_conflicts(&app)
}

/// 移除用户选中的外来变量（原值不备份、不保留；系统级一律拒绝）。
#[tauri::command]
pub fn env_delivery_remove(
    state: State<'_, AppState>,
    conflicts: Vec<EnvConflict>,
) -> Result<usize, String> {
    remove_vars(state.env_sink.as_ref(), conflicts)
}

#[tauri::command]
pub fn env_delivery_conflicts(
    state: State<'_, AppState>,
    app: String,
    provider_id: String,
) -> Result<Vec<crate::env_delivery::EnvConflict>, String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    let provider = state
        .db
        .get_provider_by_id(&provider_id, app_type.as_str())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("供应商 {provider_id} 不存在"))?;
    match ProviderService::preflight_env_delivery(state.inner(), &app_type, &provider) {
        Ok(()) => Ok(Vec::new()),
        Err(e) => {
            let msg = e.to_string();
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&msg) {
                if value.get("code").and_then(|c| c.as_str()) == Some("ENV_CONFLICT") {
                    let conflicts = serde_json::from_value(
                        value
                            .get("conflicts")
                            .cloned()
                            .unwrap_or(serde_json::json!([])),
                    )
                    .unwrap_or_default();
                    return Ok(conflicts);
                }
            }
            Err(msg)
        }
    }
}

/// F3-2（P1-4）：接管会 `fetch_provider_secrets`（1P 模式下是阻塞子进程、可能弹解锁），
/// 必须 async + `spawn_blocking`。严格模式（含 1P 恒严格）下服务层直接拒绝（F3-3）。
#[tauri::command]
pub async fn env_delivery_adopt(
    state: State<'_, AppState>,
    app: String,
    provider_id: String,
    names: Vec<String>,
) -> Result<(), String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        ProviderService::adopt_env_vars(&state, &app_type, &provider_id, &names)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("接管环境变量任务执行失败: {e}"))?
}
