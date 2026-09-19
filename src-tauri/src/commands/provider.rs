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
    // 一次读盘、所有行共用：known_secret_targets 既是 extra_env 的唯一真源，
    // 也用来跳过确实没有凭据的行（§5.2.1）。
    let mut known_targets =
        crate::secrets::load_known_targets(state.db.as_ref()).unwrap_or_default();
    let mut repaired = false;
    let mut sanitized = IndexMap::new();
    for (id, provider) in providers {
        let mut front = provider.to_frontend();
        front.secret_status = Some(load_secret_status(
            state.inner(),
            &app_type,
            &id,
            &mut known_targets,
            &mut repaired,
        ));
        sanitized.insert(id, front);
    }
    if repaired {
        if let Err(e) = crate::secrets::save_known_targets(state.db.as_ref(), &known_targets) {
            log::warn!("补登记凭据条目失败: {e}");
        }
    }
    Ok(sanitized)
}

/// 该 target 是否真的有凭据。名册已登记就直接信名册；没登记则探测一次，
/// 确实存在就补进名册。
///
/// 补登记是给历史洞自愈：登记机制之前完成的迁移只把密钥写进了凭据管理器、
/// 没往 `known_secret_targets` 里记，于是卡片会误报「需要密钥」并拒绝切换，
/// 而凭据其实好端端在库里。
fn ensure_registered(
    state: &AppState,
    known_targets: &mut Vec<String>,
    repaired: &mut bool,
    target: &SecretTarget,
) -> bool {
    let name = target.to_target_string();
    if known_targets.iter().any(|t| t == &name) {
        return true;
    }
    let exists = matches!(
        futures::executor::block_on(state.secrets.get(target)),
        Ok(Some(_))
    );
    if exists {
        known_targets.push(name);
        *repaired = true;
    }
    exists
}

fn load_secret_status(
    state: &AppState,
    app_type: &AppType,
    provider_id: &str,
    known_targets: &mut Vec<String>,
    repaired: &mut bool,
) -> SecretStatus {
    let api_key_target = SecretTarget::provider_api_key(app_type.clone(), provider_id);
    let base_url_target = SecretTarget::provider_base_url(app_type.clone(), provider_id);

    let api_present = ensure_registered(state, known_targets, repaired, &api_key_target);
    // base_url 允许回显（§5.2.2，编辑表单与卡片要显示），确认存在后读一次值。
    let base = ensure_registered(state, known_targets, repaired, &base_url_target)
        .then(|| {
            futures::executor::block_on(state.secrets.retrieve(&base_url_target))
                .ok()
                .flatten()
        })
        .flatten();
    SecretStatus {
        api_key: SecretHint {
            present: api_present,
        },
        // baseUrl 允许回显（§5.2.2）；从 Zeroizing 里短生命周期取出后即刻丢弃。
        base_url: base.map(|url| url.to_string()),
        extra_env: extra_env_keys(known_targets, app_type, provider_id),
    }
}

fn extra_env_keys(known_targets: &[String], app_type: &AppType, provider_id: &str) -> Vec<String> {
    let prefix = format!(
        "cc-switch/v1/provider/{}/{}/env/",
        app_type.as_str(),
        provider_id
    );
    known_targets
        .iter()
        .filter_map(|t| t.strip_prefix(&prefix).map(str::to_string))
        .filter(|k| !k.is_empty())
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use std::sync::Arc;
    use zeroize::Zeroizing;

    fn state_with_store() -> AppState {
        AppState::new(
            Arc::new(Database::memory().expect("内存库")),
            Arc::new(crate::secrets::InMemorySecretStore::new()),
        )
    }

    /// 名册漏记但凭据确实在库里的行（登记机制之前完成的迁移）必须被探测出来并补登记，
    /// 否则界面会一直误报「需要密钥」。
    #[test]
    fn unregistered_credential_is_probed_and_repaired_into_the_registry() {
        let state = state_with_store();
        let target = SecretTarget::provider_api_key(AppType::Claude, "p1".to_string());
        futures::executor::block_on(
            state
                .secrets
                .set(&target, Zeroizing::new("sk-x".to_string())),
        )
        .expect("写入凭据");

        let mut known: Vec<String> = Vec::new();
        let mut repaired = false;
        let status = load_secret_status(&state, &AppType::Claude, "p1", &mut known, &mut repaired);
        assert!(status.api_key.present, "凭据存在就必须判为已配置");
        assert!(repaired, "首次探测到未登记凭据应标记为需要补登记");
        assert!(
            known.iter().any(|t| t == &target.to_target_string()),
            "实际名册: {known:?}"
        );

        // 补登记之后不再改动名册
        let mut repaired_again = false;
        let second = load_secret_status(
            &state,
            &AppType::Claude,
            "p1",
            &mut known,
            &mut repaired_again,
        );
        assert!(second.api_key.present);
        assert!(!repaired_again, "已登记的行不该再触发补登记");
    }

    #[test]
    fn row_without_any_credential_stays_unconfigured() {
        let state = state_with_store();
        let mut known: Vec<String> = Vec::new();
        let mut repaired = false;
        let status = load_secret_status(
            &state,
            &AppType::Claude,
            "missing",
            &mut known,
            &mut repaired,
        );
        assert!(!status.api_key.present);
        assert!(!repaired);
        assert!(
            known.is_empty(),
            "没有凭据的行不该往名册里写东西: {known:?}"
        );
    }
}
