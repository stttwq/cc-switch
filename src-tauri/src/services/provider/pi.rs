use super::{ProviderService, SwitchResult};
use crate::app_config::AppType;
use crate::error::AppError;
use crate::provider::Provider;
use crate::secrets::SecretExtractor;
use crate::store::AppState;
use indexmap::IndexMap;
use serde_json::Value;

const PI_APP: &str = "pi";

pub(super) fn list(state: &AppState) -> Result<IndexMap<String, Provider>, AppError> {
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(PI_APP));
    match crate::pi_config::read_pi_native_providers() {
        Ok(native) => {
            if let Err(error) = sync_native_locked(state, &native) {
                log::warn!("Failed to sync Pi providers from native config: {error}");
            }
        }
        Err(error) => {
            log::warn!("Failed to read Pi providers; showing saved catalog: {error}");
        }
    }
    state.db.get_all_providers(PI_APP)
}

pub(super) fn import_from_live(state: &AppState) -> Result<usize, AppError> {
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(PI_APP));
    let native = crate::pi_config::read_pi_native_providers()?;
    sync_native_locked(state, &native)
}

pub(super) fn reapply_live(state: &AppState) -> Result<usize, AppError> {
    let native = crate::pi_config::read_pi_native_providers()?;
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(PI_APP));
    sync_native_locked(state, &native)?;
    drop(_guard);
    let mut applied = 0;
    for id in native.keys() {
        enable(state, id)?;
        applied += 1;
    }
    Ok(applied)
}

pub(super) fn add(
    state: &AppState,
    mut provider: Provider,
    add_to_live: bool,
) -> Result<bool, AppError> {
    let app_type = AppType::Pi;
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(app_type.as_str()));
    strip_unsupported_pi_metadata(&mut provider);
    ProviderService::validate_provider_settings(&app_type, &provider)?;
    align_native_display_name(&mut provider);

    if state
        .db
        .get_provider_by_id(&provider.id, app_type.as_str())?
        .is_some()
    {
        return Err(AppError::InvalidInput(format!(
            "Pi provider '{}' already exists",
            provider.id
        )));
    }

    if !add_to_live && crate::pi_config::pi_provider_exists(&provider.id)? {
        return Err(AppError::InvalidInput(format!(
            "Pi provider key '{}' already exists in models.json",
            provider.id
        )));
    }

    let live_config = provider.settings_config.clone();
    strip_and_store_pi_secrets(state, &mut provider)?;

    let native_inserted = if add_to_live {
        crate::pi_config::insert_pi_provider(&provider.id, &live_config)?
    } else {
        false
    };

    if let Err(error) = state.db.save_provider(app_type.as_str(), &provider) {
        if native_inserted {
            if let Err(rollback) =
                crate::pi_config::remove_pi_provider_if_matches(&provider.id, &live_config)
            {
                return Err(AppError::Config(format!(
                    "failed to save Pi provider: {error}; native rollback failed: {rollback}"
                )));
            }
        }
        return Err(error);
    }
    Ok(true)
}

pub(super) fn update(
    state: &AppState,
    original_id: Option<&str>,
    mut provider: Provider,
) -> Result<bool, AppError> {
    let app_type = AppType::Pi;
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(app_type.as_str()));
    let original_id = original_id.unwrap_or(&provider.id).to_string();
    if original_id != provider.id {
        return Err(AppError::InvalidInput(
            "Pi provider keys cannot be renamed".to_string(),
        ));
    }

    state
        .db
        .get_provider_by_id(&original_id, app_type.as_str())?
        .ok_or_else(|| AppError::InvalidInput(format!("Pi provider '{original_id}' not found")))?;
    strip_unsupported_pi_metadata(&mut provider);
    ProviderService::validate_provider_settings(&app_type, &provider)?;

    let live_config = provider.settings_config.clone();
    strip_and_store_pi_secrets(state, &mut provider)?;

    let previous_native =
        crate::pi_config::replace_pi_provider_if_present(&original_id, &live_config)?;
    if let Err(error) = state.db.save_provider(app_type.as_str(), &provider) {
        if let Some(previous_native) = previous_native.as_ref() {
            if let Err(rollback) =
                crate::pi_config::replace_pi_provider(&original_id, &live_config, previous_native)
            {
                return Err(AppError::Config(format!(
                    "failed to save Pi provider: {error}; native rollback failed: {rollback}"
                )));
            }
        }
        return Err(error);
    }
    Ok(true)
}

pub(super) fn delete(state: &AppState, id: &str) -> Result<(), AppError> {
    let app_type = AppType::Pi;
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(app_type.as_str()));
    let Some(_) = state.db.get_provider_by_id(id, app_type.as_str())? else {
        return Ok(());
    };
    // Delete is intentionally keyed by provider ID. Once the user confirms
    // deleting the provider itself, supported field edits do not change that
    // intent; the latest native value is retained only for rollback.
    let removed = crate::pi_config::remove_pi_provider(id)?;
    super::delete_provider_secrets(state, &app_type, id);

    if let Err(error) = state.db.delete_provider(app_type.as_str(), id) {
        if let Some(removed) = removed.as_ref() {
            if let Err(rollback) = crate::pi_config::restore_pi_provider_if_missing(id, removed) {
                return Err(AppError::Config(format!(
                    "failed to delete Pi provider: {error}; native rollback failed: {rollback}"
                )));
            }
        }
        return Err(error);
    }
    Ok(())
}

pub(super) fn remove(state: &AppState, id: &str) -> Result<(), AppError> {
    let app_type = AppType::Pi;
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(app_type.as_str()));
    let provider = state
        .db
        .get_provider_by_id(id, app_type.as_str())?
        .ok_or_else(|| AppError::InvalidInput(format!("Pi provider '{id}' not found")))?;
    let Some(removed) = crate::pi_config::remove_pi_provider(id)? else {
        return Ok(());
    };
    let mut synced = provider;
    merge_native_config(&mut synced, removed.clone());
    if let Err(error) = state.db.save_provider(app_type.as_str(), &synced) {
        if let Err(rollback) = crate::pi_config::restore_pi_provider_if_missing(id, &removed) {
            return Err(AppError::Config(format!(
                "failed to preserve Pi provider before removal: {error}; native rollback failed: {rollback}"
            )));
        }
        return Err(error);
    }
    Ok(())
}

pub(super) fn enable(state: &AppState, id: &str) -> Result<SwitchResult, AppError> {
    let app_type = AppType::Pi;
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(app_type.as_str()));
    let provider = state
        .db
        .get_provider_by_id(id, app_type.as_str())?
        .ok_or_else(|| AppError::InvalidInput(format!("Pi provider '{id}' not found")))?;

    if crate::pi_config::read_pi_native_provider(id)?.is_some() {
        let mut result = SwitchResult::default();
        ProviderService::deliver_env_credentials_pub(state, &app_type, &provider, &mut result)?;
        return Ok(result);
    }

    ProviderService::validate_provider_settings(&app_type, &provider)?;
    ProviderService::preflight_env_delivery(state, &app_type, &provider)?;
    crate::pi_config::insert_pi_provider(id, &provider.settings_config)?;
    let mut result = SwitchResult::default();
    ProviderService::deliver_env_credentials_pub(state, &app_type, &provider, &mut result)?;
    Ok(result)
}

fn sync_native_locked(
    state: &AppState,
    native: &IndexMap<String, Value>,
) -> Result<usize, AppError> {
    let saved = state.db.get_all_providers(PI_APP)?;
    let mut changed = 0;

    for (id, config) in native {
        let mut provider = saved.get(id).cloned().unwrap_or_else(|| {
            let name = native_provider_name(config).unwrap_or(id).to_string();
            let mut imported = Provider::with_id(id.clone());
            imported.name = name;
            imported.settings_config = config.clone();
            imported.category = Some("custom".to_string());
            imported.icon = Some("pi".to_string());
            imported
        });
        let is_new = !saved.contains_key(id);
        let previous_name = provider.name.clone();
        let previous_config = provider.settings_config.clone();
        merge_native_config(&mut provider, config.clone());
        let extractor =
            SecretExtractor::new(state.secrets.as_ref(), AppType::Pi).with_db(state.db.as_ref());
        let extracted =
            SecretExtractor::extract(&provider.id, &AppType::Pi, &provider.settings_config)?;
        let live_rewritten =
            crate::services::provider::pi_sanitizer::sanitize_pi_provider_for_live_write(
                &provider.id,
                config,
            )?;
        if live_rewritten != *config {
            match crate::pi_config::replace_pi_provider(id, config, &live_rewritten) {
                Ok(()) => {
                    if let Err(error) = futures::executor::block_on(
                        extractor.extract_provider_secrets(&provider.id, config),
                    ) {
                        log::warn!("Pi native extract after live rewrite failed for {id}: {error}");
                    }
                    provider.settings_config = extracted.stripped;
                }
                Err(error) => {
                    log::warn!(
                        "Failed to rewrite Pi models.json for '{id}', keeping original: {error}"
                    );
                    continue;
                }
            }
        } else {
            if let Err(error) = futures::executor::block_on(
                extractor.extract_provider_secrets(&provider.id, &provider.settings_config),
            ) {
                log::warn!("Pi native extract failed for {id}: {error}");
                continue;
            }
            provider.settings_config = extracted.stripped;
        }
        if !is_new && provider.name == previous_name && provider.settings_config == previous_config
        {
            continue;
        }

        state.db.save_provider(PI_APP, &provider)?;
        changed += 1;
    }

    Ok(changed)
}

fn merge_native_config(provider: &mut Provider, config: Value) {
    if let Some(name) = native_provider_name(&config) {
        provider.name = name.to_string();
    }
    provider.settings_config = config;
}

fn native_provider_name(config: &Value) -> Option<&str> {
    config
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
}

fn align_native_display_name(provider: &mut Provider) {
    let Some(config) = provider.settings_config.as_object_mut() else {
        return;
    };
    if config.contains_key("name") {
        config.insert("name".to_string(), Value::String(provider.name.clone()));
    }
}

fn strip_unsupported_pi_metadata(provider: &mut Provider) {
    // Pi doesn't support most metadata fields, so clear them
    provider.meta = None;
}

fn strip_and_store_pi_secrets(state: &AppState, provider: &mut Provider) -> Result<(), AppError> {
    let extractor =
        SecretExtractor::new(state.secrets.as_ref(), AppType::Pi).with_db(state.db.as_ref());
    let (stripped, _) = futures::executor::block_on(
        extractor.extract_provider_secrets(&provider.id, &provider.settings_config),
    )?;
    provider.settings_config = stripped;
    Ok(())
}
