use indexmap::IndexMap;
use tauri::{Manager, State};

use crate::app_config::AppType;
use crate::error::AppError;
use crate::provider::{Provider, ProviderForFrontend, SecretHint, SecretStatus};
use crate::secrets::SecretTarget;
use crate::services::{ProviderService, ProviderSortUpdate, SwitchResult};
use crate::store::AppState;
use std::str::FromStr;

/// 获取所有供应商（前端安全版本，不包含 settings_config）
/// Phase 5 S1: IPC 零密钥 - 防止 settings_config 中的敏感字段泄漏到前端
#[tauri::command]
pub fn get_providers(
    state: State<'_, AppState>,
    app: String,
) -> Result<IndexMap<String, ProviderForFrontend>, String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    let providers =
        ProviderService::list(state.inner(), app_type.clone()).map_err(|e| e.to_string())?;
    let mut sanitized = IndexMap::new();
    for (id, provider) in providers {
        let mut front = provider.to_frontend();
        front.secret_status = Some(load_secret_status(state.inner(), &app_type, &id));
        sanitized.insert(id, front);
    }
    Ok(sanitized)
}

fn load_secret_status(state: &AppState, app_type: &AppType, provider_id: &str) -> SecretStatus {
    // api_key 用 Zeroizing 承载，取完末四位即离开作用域被清零，不生成裸 String。
    let api = futures::executor::block_on(state.secrets.get(&SecretTarget::provider_api_key(
        app_type.clone(),
        provider_id,
    )))
    .ok()
    .flatten();
    let hint = api.as_deref().and_then(|key| hint_last4(key));
    // base_url 允许回显（编辑表单与卡片要显示），仍来自凭据管理器按需读取。
    let base = futures::executor::block_on(state.secrets.retrieve(
        &SecretTarget::provider_base_url(app_type.clone(), provider_id),
    ))
    .ok()
    .flatten();
    SecretStatus {
        api_key: SecretHint {
            present: api.is_some(),
            hint,
        },
        base_url: base,
        extra_env: extra_env_keys(state, app_type, provider_id),
    }
}

fn extra_env_keys(state: &AppState, app_type: &AppType, provider_id: &str) -> Vec<String> {
    let prefix = format!(
        "cc-switch/v1/provider/{}/{}/env/",
        app_type.as_str(),
        provider_id
    );
    crate::secrets::load_known_targets(state.db.as_ref())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|t| t.strip_prefix(&prefix).map(str::to_string))
        .filter(|k| !k.is_empty())
        .collect()
}

fn hint_last4(value: &str) -> Option<String> {
    if value.chars().count() < 8 {
        None
    } else {
        Some(crate::secrets::last_chars(value, 4).to_string())
    }
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
pub fn update_providers_sort_order(
    state: State<'_, AppState>,
    app: String,
    updates: Vec<ProviderSortUpdate>,
) -> Result<bool, String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    ProviderService::update_sort_order(state.inner(), app_type, updates).map_err(|e| e.to_string())
}
