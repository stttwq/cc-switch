use indexmap::IndexMap;
use tauri::{Emitter, Manager, State};

use crate::app_config::AppType;
use crate::error::AppError;
use crate::provider::Provider;
use crate::services::{ProviderService, ProviderSortUpdate, SwitchResult};
use crate::store::AppState;
use std::str::FromStr;

/// 获取所有供应商
#[tauri::command]
pub fn get_providers(
    state: State<'_, AppState>,
    app: String,
) -> Result<IndexMap<String, Provider>, String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    ProviderService::list(state.inner(), app_type).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_current_provider(state: State<'_, AppState>, app: String) -> Result<String, String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    ProviderService::current(state.inner(), app_type).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn add_provider(
    app_handle: tauri::AppHandle,
    app: String,
    provider: Provider,
    #[allow(non_snake_case)] addToLive: Option<bool>,
) -> Result<bool, String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    let add_to_live = addToLive.unwrap_or(true);
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle
            .try_state::<AppState>()
            .ok_or_else(|| "应用状态不可用".to_string())?;
        ProviderService::add(state.inner(), app_type, provider, add_to_live)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("供应商添加任务执行失败: {e}"))?
}

#[tauri::command]
pub async fn update_provider(
    app_handle: tauri::AppHandle,
    app: String,
    provider: Provider,
    #[allow(non_snake_case)] originalId: Option<String>,
) -> Result<bool, String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle
            .try_state::<AppState>()
            .ok_or_else(|| "应用状态不可用".to_string())?;
        ProviderService::update(state.inner(), app_type, originalId.as_deref(), provider)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("供应商更新任务执行失败: {e}"))?
}

#[tauri::command]
pub fn delete_provider(
    state: State<'_, AppState>,
    app: String,
    id: String,
) -> Result<bool, String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    ProviderService::delete(state.inner(), app_type, &id)
        .map(|_| true)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn remove_provider_from_live_config(
    state: tauri::State<'_, AppState>,
    app: String,
    id: String,
) -> Result<bool, String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    ProviderService::remove_from_live_config(state.inner(), app_type, &id)
        .map(|_| true)
        .map_err(|e| e.to_string())
}

fn switch_provider_internal(
    state: &AppState,
    app_type: AppType,
    id: &str,
) -> Result<SwitchResult, AppError> {
    ProviderService::switch(state, app_type, id)
}

#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub fn switch_provider_test_hook(
    state: &AppState,
    app_type: AppType,
    id: &str,
) -> Result<SwitchResult, AppError> {
    switch_provider_internal(state, app_type, id)
}

#[tauri::command]
pub async fn switch_provider(
    app_handle: tauri::AppHandle,
    app: String,
    id: String,
) -> Result<SwitchResult, String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle
            .try_state::<AppState>()
            .ok_or_else(|| "应用状态不可用".to_string())?;
        switch_provider_internal(state.inner(), app_type, &id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("供应商切换任务执行失败: {e}"))?
}

fn import_default_config_internal(state: &AppState, app_type: AppType) -> Result<bool, AppError> {
    let imported = ProviderService::import_default_config(state, app_type.clone())?;

    if imported {
        // Extract common config snippet (mirrors old startup logic in lib.rs)
        if state
            .db
            .should_auto_extract_config_snippet(app_type.as_str())?
        {
            match ProviderService::extract_common_config_snippet(state, app_type.clone()) {
                Ok(snippet) if !snippet.is_empty() && snippet != "{}" => {
                    let _ = state
                        .db
                        .set_config_snippet(app_type.as_str(), Some(snippet));
                    let _ = state
                        .db
                        .set_config_snippet_cleared(app_type.as_str(), false);
                }
                _ => {}
            }
        }

        ProviderService::migrate_legacy_common_config_usage_if_needed(state, app_type.clone())?;
    }

    Ok(imported)
}

#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub fn import_default_config_test_hook(
    state: &AppState,
    app_type: AppType,
) -> Result<bool, AppError> {
    import_default_config_internal(state, app_type)
}

#[tauri::command]
pub fn import_default_config(state: State<'_, AppState>, app: String) -> Result<bool, String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    import_default_config_internal(&state, app_type).map_err(Into::into)
}

#[tauri::command]
pub fn ensure_codex_official_provider(state: State<'_, AppState>) -> Result<bool, String> {
    state
        .db
        .ensure_official_seed_by_id(crate::database::CODEX_OFFICIAL_PROVIDER_ID, AppType::Codex)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn read_live_provider_settings(app: String) -> Result<serde_json::Value, String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    ProviderService::read_live_settings(app_type).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_custom_endpoints(
    state: State<'_, AppState>,
    app: String,
    #[allow(non_snake_case)] providerId: String,
) -> Result<Vec<crate::settings::CustomEndpoint>, String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    ProviderService::get_custom_endpoints(state.inner(), app_type, &providerId)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_custom_endpoint(
    state: State<'_, AppState>,
    app: String,
    #[allow(non_snake_case)] providerId: String,
    url: String,
) -> Result<(), String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    ProviderService::add_custom_endpoint(state.inner(), app_type, &providerId, url)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn remove_custom_endpoint(
    state: State<'_, AppState>,
    app: String,
    #[allow(non_snake_case)] providerId: String,
    url: String,
) -> Result<(), String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    ProviderService::remove_custom_endpoint(state.inner(), app_type, &providerId, url)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_endpoint_last_used(
    state: State<'_, AppState>,
    app: String,
    #[allow(non_snake_case)] providerId: String,
    url: String,
) -> Result<(), String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    ProviderService::update_endpoint_last_used(state.inner(), app_type, &providerId, url)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_providers_sort_order(
    state: State<'_, AppState>,
    app: String,
    updates: Vec<ProviderSortUpdate>,
) -> Result<bool, String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    ProviderService::update_sort_order(state.inner(), app_type, updates).map_err(|e| e.to_string())
}

use crate::provider::UniversalProvider;
use std::collections::HashMap;
use tauri::AppHandle;

#[derive(Clone, serde::Serialize)]
pub struct UniversalProviderSyncedEvent {
    pub action: String,
    pub id: String,
}

fn emit_universal_provider_synced(app: &AppHandle, action: &str, id: &str) {
    let _ = app.emit(
        "universal-provider-synced",
        UniversalProviderSyncedEvent {
            action: action.to_string(),
            id: id.to_string(),
        },
    );
}

#[tauri::command]
pub fn get_universal_providers(
    state: State<'_, AppState>,
) -> Result<HashMap<String, UniversalProvider>, String> {
    ProviderService::list_universal(state.inner()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_universal_provider(
    state: State<'_, AppState>,
    id: String,
) -> Result<Option<UniversalProvider>, String> {
    ProviderService::get_universal(state.inner(), &id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn upsert_universal_provider(
    app: AppHandle,
    state: State<'_, AppState>,
    provider: UniversalProvider,
) -> Result<bool, String> {
    let id = provider.id.clone();
    let result =
        ProviderService::upsert_universal(state.inner(), provider).map_err(|e| e.to_string())?;

    emit_universal_provider_synced(&app, "upsert", &id);

    Ok(result)
}

#[tauri::command]
pub fn delete_universal_provider(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<bool, String> {
    let result =
        ProviderService::delete_universal(state.inner(), &id).map_err(|e| e.to_string())?;

    emit_universal_provider_synced(&app, "delete", &id);

    Ok(result)
}

#[tauri::command]
pub fn sync_universal_provider(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<bool, String> {
    let result =
        ProviderService::sync_universal_to_apps(state.inner(), &id).map_err(|e| e.to_string())?;

    emit_universal_provider_synced(&app, "sync", &id);

    Ok(result)
}
