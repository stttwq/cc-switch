use crate::app_config::AppType;
use crate::services::env_checker::{
    check_env_conflicts as check_conflicts, delete_env_vars as delete_vars, EnvConflict,
};
use crate::services::ProviderService;
use crate::store::AppState;
use std::str::FromStr;
use tauri::State;

/// Check environment variable conflicts for a specific app
#[tauri::command]
pub fn check_env_conflicts(app: String) -> Result<Vec<EnvConflict>, String> {
    check_conflicts(&app)
}

/// Delete conflicting environment variables by name (originals are not retained)
#[tauri::command]
pub fn delete_env_vars(conflicts: Vec<EnvConflict>) -> Result<usize, String> {
    delete_vars(conflicts)
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

#[tauri::command]
pub fn env_delivery_adopt(
    state: State<'_, AppState>,
    app: String,
    provider_id: String,
    names: Vec<String>,
) -> Result<(), String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    ProviderService::adopt_env_vars(state.inner(), &app_type, &provider_id, &names)
        .map_err(|e| e.to_string())
}
