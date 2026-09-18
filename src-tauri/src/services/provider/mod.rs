//! Provider service module
//!
//! Handles provider CRUD operations, switching, and configuration management.

pub(crate) mod codex_sanitizer;
mod live;
mod live_sanitizer;
mod pi;
pub(crate) mod pi_sanitizer;

use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;

use crate::app_config::AppType;
use crate::error::AppError;
use crate::provider::Provider;
use crate::secrets::SecretExtractor;
use crate::services::mcp::McpService;
use crate::store::AppState;
use std::str::FromStr;
use zeroize::Zeroizing;

// Re-export sub-module functions for external access
pub use live::{
    import_default_config, read_live_settings, should_import_default_config_on_startup,
    sync_current_to_live, update_toml_common_config_snippet,
};

pub fn import_pi_providers_from_live(state: &AppState) -> Result<usize, AppError> {
    pi::import_from_live(state)
}

pub fn reapply_pi_live(state: &AppState) -> Result<usize, AppError> {
    pi::reapply_live(state)
}

pub fn cleanup_orphan_secrets(state: &AppState) -> Result<usize, AppError> {
    let mut targets = crate::secrets::load_known_targets(state.db.as_ref())?;
    let mut expected = Vec::new();
    for app in [AppType::Claude, AppType::Codex, AppType::Pi] {
        let providers = state.db.get_all_providers(app.as_str())?;
        for id in providers.keys() {
            expected.push(crate::secrets::provider_target_prefix(&app, id));
        }
    }
    let mut removed = 0;
    let mut kept = Vec::new();
    for target_str in targets.drain(..) {
        let still_needed = expected.iter().any(|prefix| target_str.starts_with(prefix));
        if still_needed {
            kept.push(target_str);
            continue;
        }
        if let Some(target) = parse_secret_target(&target_str) {
            if let Err(e) = futures::executor::block_on(state.secrets.delete(&target)) {
                log::warn!("清理孤儿凭据失败 {target_str}: {e}");
                kept.push(target_str);
                continue;
            }
        }
        removed += 1;
    }
    crate::secrets::save_known_targets(state.db.as_ref(), &kept)?;
    Ok(removed)
}

// Internal re-exports (pub(crate))
pub(crate) use live::sanitize_claude_settings_for_live;
pub(crate) use live::{
    normalize_provider_common_config_for_storage, provider_exists_in_live_config,
    strip_common_config_from_live_settings, sync_current_provider_for_app_to_live,
    write_live_with_common_config_for_state,
};

// Internal re-exports

/// 统一会话开关变更后，立即按新开关状态重写当前官方 Codex 供应商的
/// live 配置，使开关即时生效（无需等下一次切换）。
/// 当前供应商非官方（或不存在）时为 no-op：注入只作用于官方配置，
/// 第三方 live 配置不受开关影响。
pub fn reapply_current_codex_official_live(state: &AppState) -> Result<bool, AppError> {
    let current_id = ProviderService::current(state, AppType::Codex)?;
    if current_id.is_empty() {
        return Ok(false);
    }
    let providers = state.db.get_all_providers(AppType::Codex.as_str())?;
    let Some(provider) = providers.get(&current_id) else {
        return Ok(false);
    };
    if provider.category.as_deref() != Some("official")
        && !crate::codex_config::is_codex_official_provider(provider)
    {
        return Ok(false);
    }

    // 重写 live 会整体替换 config.toml（有意设计），[mcp_servers] 随之丢失，
    // 写完必须立刻从 DB 重新投影启用的 MCP。只投影 Codex 而非
    // sync_all_enabled：后者按 AppType::all() 顺序逐应用短路，排在 Codex
    // 前面的无关应用 live 损坏（如 ~/.claude.json 坏 JSON）会阻断 Codex
    // 的重投影，让刚被清掉的 [mcp_servers] 无人补回。
    // 投影失败降级为警告：走到这里 live 已按新开关状态落盘，开关事实上
    // 已生效；若把错误上抛，save_settings 会回滚开关设置，制造"设置=旧值、
    // live=新桶"的会话分裂——正是该回滚要防止的状态。MCP 投影可自愈
    // （下次切换 / 任一 MCP 启停操作都会重新投影）。
    write_live_with_common_config_for_state(state, &AppType::Codex, provider)?;
    if let Err(err) = McpService::sync_enabled_for_app(state, &AppType::Codex) {
        log::warn!("统一会话开关重写 live 后重投影 Codex MCP 失败（将在下次同步时自愈）: {err}");
    }
    Ok(true)
}

/// Provider business logic service
pub struct ProviderService;

/// Result of a provider switch operation, including any non-fatal warnings
#[derive(Debug, serde::Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SwitchResult {
    pub warnings: Vec<String>,
}

impl ProviderService {
    fn normalize_provider_if_claude(app_type: &AppType, provider: &mut Provider) {
        if matches!(app_type, AppType::Claude) {
            let mut v = provider.settings_config.clone();
            if normalize_claude_models_in_value(&mut v) {
                provider.settings_config = v;
            }
        }
    }

    /// Check whether a provider exists in live config, tolerating parse errors
    /// only for providers that are explicitly marked as DB-only.
    fn check_live_config_exists(
        app_type: &AppType,
        provider_id: &str,
        live_config_managed: Option<bool>,
    ) -> Result<bool, AppError> {
        if live_config_managed == Some(false) {
            Ok(provider_exists_in_live_config(app_type, provider_id).unwrap_or(false))
        } else {
            provider_exists_in_live_config(app_type, provider_id)
        }
    }

    fn provider_live_config_managed(provider: &Provider) -> Option<bool> {
        provider
            .meta
            .as_ref()
            .and_then(|meta| meta.live_config_managed)
    }

    fn set_provider_live_config_managed(provider: &mut Provider, managed: bool) {
        provider
            .meta
            .get_or_insert_with(Default::default)
            .live_config_managed = Some(managed);
    }

    /// List all providers for an app type
    pub fn list(
        state: &AppState,
        app_type: AppType,
    ) -> Result<IndexMap<String, Provider>, AppError> {
        if app_type == AppType::Pi {
            return pi::list(state);
        }
        state.db.get_all_providers(app_type.as_str())
    }

    /// Get current provider ID
    ///
    /// 使用有效的当前供应商 ID（验证过存在性）。
    /// 优先从本地 settings 读取，验证后 fallback 到数据库的 is_current 字段。
    /// 这确保了云同步场景下多设备可以独立选择供应商，且返回的 ID 一定有效。
    ///
    /// 对于累加模式应用（OpenCode, OpenClaw），不存在"当前供应商"概念，直接返回空字符串。
    pub fn current(state: &AppState, app_type: AppType) -> Result<String, AppError> {
        // Additive mode apps have no "current" provider concept
        if app_type.is_additive_mode() {
            return Ok(String::new());
        }
        crate::settings::get_effective_current_provider(&state.db, &app_type)
            .map(|opt| opt.unwrap_or_default())
    }

    /// Add a new provider
    pub fn add(
        state: &AppState,
        app_type: AppType,
        provider: Provider,
        add_to_live: bool,
    ) -> Result<bool, AppError> {
        if app_type == AppType::Pi {
            return pi::add(state, provider, add_to_live);
        }

        let mut provider = provider;
        // Normalize Claude model keys
        Self::normalize_provider_if_claude(&app_type, &mut provider);
        Self::validate_provider_settings(&app_type, &provider)?;
        normalize_provider_common_config_for_storage(state.db.as_ref(), &app_type, &mut provider)?;
        if app_type.is_additive_mode() {
            Self::set_provider_live_config_managed(&mut provider, add_to_live);
        }
        strip_and_store_provider_secrets(state, &app_type, &mut provider)?;

        // Save to database
        state.db.save_provider(app_type.as_str(), &provider)?;

        // For other apps: Check if sync is needed (if this is current provider, or no current provider)
        let current = state.db.get_current_provider(app_type.as_str())?;
        if current.is_none() {
            // No current provider, set as current and sync.
            state
                .db
                .set_current_provider(app_type.as_str(), &provider.id)?;
            write_live_with_common_config_for_state(state, &app_type, &provider)?;
        }

        Ok(true)
    }

    /// Update a provider
    pub fn update(
        state: &AppState,
        app_type: AppType,
        original_id: Option<&str>,
        provider: Provider,
    ) -> Result<bool, AppError> {
        if app_type == AppType::Pi {
            return pi::update(state, original_id, provider);
        }

        let mut provider = provider;
        let original_id = original_id.unwrap_or(provider.id.as_str()).to_string();
        let provider_id_changed = original_id != provider.id;
        let existing_provider = state
            .db
            .get_provider_by_id(&original_id, app_type.as_str())?;
        // Normalize Claude model keys
        Self::normalize_provider_if_claude(&app_type, &mut provider);
        Self::validate_provider_settings(&app_type, &provider)?;
        normalize_provider_common_config_for_storage(state.db.as_ref(), &app_type, &mut provider)?;
        if matches!(app_type, AppType::Codex) && provider.category.as_deref() == Some("official") {
            crate::codex_config::strip_codex_unified_session_bucket_from_settings(
                &mut provider.settings_config,
            )?;
        }

        if provider_id_changed {
            if !app_type.is_additive_mode() {
                return Err(AppError::Message(
                    "Only additive-mode providers support changing provider key".to_string(),
                ));
            }

            let Some(existing_provider) = existing_provider else {
                return Err(AppError::Message(format!(
                    "Original provider '{}' does not exist in app '{}'",
                    original_id,
                    app_type.as_str()
                )));
            };

            let original_in_live = Self::check_live_config_exists(
                &app_type,
                &original_id,
                Self::provider_live_config_managed(&existing_provider),
            )?;
            if original_in_live {
                return Err(AppError::Message(
                    "Provider key cannot be changed after the provider has been added to the app config"
                        .to_string(),
                ));
            }

            let next_id_in_live = Self::check_live_config_exists(
                &app_type,
                &provider.id,
                Self::provider_live_config_managed(&existing_provider),
            )?;
            if state
                .db
                .get_provider_by_id(&provider.id, app_type.as_str())?
                .is_some()
                || next_id_in_live
            {
                return Err(AppError::Message(format!(
                    "Provider '{}' already exists in app '{}'",
                    provider.id,
                    app_type.as_str()
                )));
            }

            Self::set_provider_live_config_managed(&mut provider, false);
            strip_and_store_provider_secrets(state, &app_type, &mut provider)?;
            state.db.save_provider(app_type.as_str(), &provider)?;
            state.db.delete_provider(app_type.as_str(), &original_id)?;

            if crate::settings::get_current_provider(&app_type).as_deref() == Some(&original_id) {
                crate::settings::set_current_provider(&app_type, Some(provider.id.as_str()))?;
            }

            return Ok(true);
        }

        // For other apps: Check if this is current provider (use effective current, not just DB)
        let effective_current =
            crate::settings::get_effective_current_provider(&state.db, &app_type)?;
        let is_current = effective_current.as_deref() == Some(provider.id.as_str());

        strip_and_store_provider_secrets(state, &app_type, &mut provider)?;
        // Save to database
        state.db.save_provider(app_type.as_str(), &provider)?;

        if is_current {
            write_live_with_common_config_for_state(state, &app_type, &provider)?;
            if let Err(err) = McpService::sync_enabled_for_app(state, &app_type) {
                log::warn!("保存供应商后重投影 {app_type:?} MCP 失败（将在下次同步时自愈）: {err}");
            }
        }

        Ok(true)
    }

    /// Delete a provider
    ///
    /// 同时检查本地 settings 和数据库的当前供应商，防止删除任一端正在使用的供应商。
    /// 对于累加模式应用（OpenCode, OpenClaw），可以随时删除任意供应商，同时从 live 配置中移除。
    pub fn delete(state: &AppState, app_type: AppType, id: &str) -> Result<(), AppError> {
        if app_type == AppType::Pi {
            return pi::delete(state, id);
        }

        // For other apps: Check both local settings and database
        let local_current = crate::settings::get_current_provider(&app_type);
        let db_current = state.db.get_current_provider(app_type.as_str())?;

        if local_current.as_deref() == Some(id) || db_current.as_deref() == Some(id) {
            return Err(AppError::Message(
                "无法删除当前正在使用的供应商".to_string(),
            ));
        }

        delete_provider_secrets(state, &app_type, id);
        Self::release_provider_managed_env(state, &app_type, id);
        state.db.delete_provider(app_type.as_str(), id)
    }

    /// §5.3.3 第 3 条：删除供应商 / Pi 移除时，先移除它托管的用户环境变量并更新
    /// `managed_env_vars`，避免孤儿变量残留在 `HKCU\Environment`。best-effort。
    pub(crate) fn release_provider_managed_env(state: &AppState, app_type: &AppType, id: &str) {
        use crate::env_delivery::ManagedEnvVars;
        match ManagedEnvVars::load(&state.db) {
            Ok(mut managed) => {
                let vars = managed.take_vars_for_provider(app_type.as_str(), id);
                if !vars.is_empty() {
                    let sink = crate::env_delivery::default_sink();
                    for name in &vars {
                        if let Err(e) = sink.remove(name) {
                            log::warn!("删除供应商时移除环境变量 {name} 失败: {e}");
                        }
                    }
                    if let Err(e) = managed.save(&state.db) {
                        log::warn!("删除供应商时更新 managed_env_vars 失败: {e}");
                    }
                    if let Err(e) = sink.broadcast() {
                        log::warn!("删除供应商后广播 WM_SETTINGCHANGE 失败: {e}");
                    }
                }
            }
            Err(e) => log::warn!("读取 managed_env_vars 以清理供应商失败: {e}"),
        }
    }

    /// Remove provider from live config only (for additive mode apps like Pi)
    ///
    /// Does NOT delete from database - provider remains in the list.
    /// This is used when user wants to "remove" a provider from active config
    /// but keep it available for future use.
    pub fn remove_from_live_config(
        state: &AppState,
        app_type: AppType,
        id: &str,
    ) -> Result<(), AppError> {
        if app_type == AppType::Pi {
            return pi::remove(state, id);
        }

        Err(AppError::Message(format!(
            "App {} does not support remove from live config",
            app_type.as_str()
        )))
    }

    /// Switch to a provider
    ///
    /// Switch flow:
    /// 1. Validate target provider exists
    /// 2. Check if proxy takeover mode is active AND proxy server is running
    /// 3. If takeover mode active: hot-switch proxy target and refresh proxy-safe Live labels
    /// 4. If normal mode:
    ///    a. **Backfill mechanism**: Backfill current live config to current provider
    ///    b. Update local settings current_provider_xxx (device-level)
    ///    c. Update database is_current (as default for new devices)
    ///    d. Write target provider config to live files
    ///    e. Sync MCP configuration
    pub fn switch(state: &AppState, app_type: AppType, id: &str) -> Result<SwitchResult, AppError> {
        if app_type == AppType::Pi {
            return pi::enable(state, id);
        }

        // Check if provider exists
        let providers = state.db.get_all_providers(app_type.as_str())?;
        providers
            .get(id)
            .ok_or_else(|| AppError::Message(format!("供应商 {id} 不存在")))?;

        // Provider switches mutate live config. Serialize them per app.
        let _switch_guard =
            futures::executor::block_on(state.switch_locks.lock_for_app(app_type.as_str()));

        Self::switch_normal(state, app_type, id, &providers)
    }

    /// Normal switch flow (non-proxy mode)
    fn switch_normal(
        state: &AppState,
        app_type: AppType,
        id: &str,
        providers: &indexmap::IndexMap<String, Provider>,
    ) -> Result<SwitchResult, AppError> {
        let provider = providers
            .get(id)
            .ok_or_else(|| AppError::Message(format!("供应商 {id} 不存在")))?;

        let mut result = SwitchResult::default();

        // Backfill: Backfill current live config to current provider
        // Use effective current provider (validated existence) to ensure backfill targets valid provider
        let current_id = crate::settings::get_effective_current_provider(&state.db, &app_type)?;
        if let Some(current_id) = current_id {
            if current_id != id {
                // Additive mode apps - all providers coexist in the same file,
                // no backfill needed (backfill is for exclusive mode apps like Claude/Codex/Gemini)
                if !app_type.is_additive_mode() {
                    // Only backfill when switching to a different provider
                    if let Ok(live_config) = live::read_live_settings_for_backfill(app_type.clone())
                    {
                        if let Some(mut current_provider) = providers.get(&current_id).cloned() {
                            // 切走前先把 live 里的可共享改动（含用户直接在应用内
                            // 装插件/加 hook/改偏好）同步进通用配置片段，再做剥离回填。
                            // 详见 sync_common_config_snippet_from_live 的文档。
                            Self::sync_common_config_snippet_from_live(
                                state,
                                &app_type,
                                &current_provider,
                                &live_config,
                                &mut result,
                            );

                            current_provider.settings_config =
                                strip_common_config_from_live_settings(
                                    state.db.as_ref(),
                                    &app_type,
                                    &current_provider,
                                    live_config,
                                );
                            if let Err(e) =
                                state.db.save_provider(app_type.as_str(), &current_provider)
                            {
                                log::warn!("Backfill failed: {e}");
                                result
                                    .warnings
                                    .push(format!("backfill_failed:{current_id}"));
                            }
                        }
                    }
                }
            }
        }

        {
            // Codex: validate the live projection before committing current —
            // the write-layer safety gates can refuse the switch, and a
            // refusal after current moved would let the next switch backfill
            // the old live config into the new provider's DB row.
            if matches!(app_type, AppType::Codex) {
                live::preflight_codex_live_write_for_state(state, provider)?;
            }

            Self::preflight_env_delivery(state, &app_type, provider)?;

            // Additive mode apps skip setting is_current (no such concept).
            if !app_type.is_additive_mode() {
                crate::settings::set_current_provider(&app_type, Some(id))?;
                state.db.set_current_provider(app_type.as_str(), id)?;
            }

            write_live_with_common_config_for_state(state, &app_type, provider)?;

            // Deliver credentials via environment variables
            Self::deliver_env_credentials(state, &app_type, provider, &mut result)?;
        }
        // Third-party dual of the block above: with preservation off, the
        // config-only write is expected to delete auth.json. A deletion
        // failure (read-only dir, ACL, file lock) must not fail the switch —
        // config and current are already committed — but the user has to see
        // that the official login is still on disk, so surface it as a
        // switch warning instead of only a log line.
        if matches!(app_type, AppType::Codex)
            && provider.category.as_deref() != Some("official")
            && !crate::codex_config::is_codex_official_provider(provider)
            && !crate::settings::preserve_codex_official_auth_on_switch()
            && crate::codex_config::get_codex_auth_path().exists()
        {
            log::warn!("Codex auth.json still present after a preservation-off third-party switch");
            result
                .warnings
                .push("codex_auth_cleanup_failed".to_string());
        }
        // 切换重写了目标应用的 live，只重投影该应用的 MCP（Codex 的
        // [mcp_servers] 与 live 同文件，整体替换后必须补回；其余应用的
        // MCP 文件独立于 live，投影是幂等维护）。不用全量 sync_all_enabled：
        // 无关应用的 live 损坏（如 ~/.claude.json 坏 JSON）不该阻断切换。
        // 走到这里 DB is_current 与 live 都已落盘，切换事实上已成功；
        // 投影失败上抛会让前端报"切换失败"制造分裂假象，故降级为警告
        // （MCP 投影可自愈：下次切换 / 任一 MCP 启停都会重新投影）。
        if let Err(err) = McpService::sync_enabled_for_app(state, &app_type) {
            log::warn!("切换供应商后重投影 {app_type:?} MCP 失败（将在下次同步时自愈）: {err}");
        }

        Ok(result)
    }

    /// Sync current provider to live configuration (re-export)
    pub fn sync_current_to_live(state: &AppState) -> Result<(), AppError> {
        sync_current_to_live(state)
    }

    pub fn sync_current_provider_for_app(
        state: &AppState,
        app_type: AppType,
    ) -> Result<(), AppError> {
        if app_type.is_additive_mode() {
            return sync_current_provider_for_app_to_live(state, &app_type);
        }

        let current_id =
            match crate::settings::get_effective_current_provider(&state.db, &app_type)? {
                Some(id) => id,
                None => return Ok(()),
            };

        let providers = state.db.get_all_providers(app_type.as_str())?;
        let Some(provider) = providers.get(&current_id) else {
            return Ok(());
        };

        write_live_with_common_config_for_state(state, &app_type, provider)?;

        McpService::sync_enabled_for_app(state, &app_type)
    }

    pub fn migrate_legacy_common_config_usage(
        state: &AppState,
        app_type: AppType,
        legacy_snippet: &str,
    ) -> Result<(), AppError> {
        if app_type.is_additive_mode() || legacy_snippet.trim().is_empty() {
            return Ok(());
        }

        let providers = state.db.get_all_providers(app_type.as_str())?;

        for provider in providers.values() {
            if provider
                .meta
                .as_ref()
                .and_then(|meta| meta.common_config_enabled)
                .is_some()
            {
                continue;
            }

            if !live::provider_uses_common_config(&app_type, provider, Some(legacy_snippet)) {
                continue;
            }

            let mut updated_provider = provider.clone();
            updated_provider
                .meta
                .get_or_insert_with(Default::default)
                .common_config_enabled = Some(true);

            match live::remove_common_config_from_settings(
                &app_type,
                &updated_provider.settings_config,
                legacy_snippet,
            ) {
                Ok(settings) => updated_provider.settings_config = settings,
                Err(err) => {
                    log::warn!(
                        "Failed to normalize legacy common config for {} provider '{}': {err}",
                        app_type.as_str(),
                        updated_provider.id
                    );
                }
            }

            state
                .db
                .save_provider(app_type.as_str(), &updated_provider)?;
        }

        Ok(())
    }

    pub(crate) fn deliver_env_credentials_pub(
        state: &AppState,
        app_type: &AppType,
        provider: &Provider,
        result: &mut SwitchResult,
    ) -> Result<(), AppError> {
        Self::deliver_env_credentials(state, app_type, provider, result)
    }

    pub(crate) fn preflight_env_delivery(
        state: &AppState,
        app_type: &AppType,
        provider: &Provider,
    ) -> Result<(), AppError> {
        let mut warnings = SwitchResult::default();
        let pending = Self::collect_pending_env(state, app_type, provider, &mut warnings)?;
        Self::reject_if_env_conflicts(state, app_type, &pending)
    }

    fn reject_if_env_conflicts(
        state: &AppState,
        app_type: &AppType,
        pending: &[(String, Zeroizing<String>)],
    ) -> Result<(), AppError> {
        use crate::env_delivery::ManagedEnvVars;
        let sink = crate::env_delivery::default_sink();
        let sink = sink.as_ref();
        let managed = ManagedEnvVars::load(&state.db)?;
        let mut conflicts = Vec::new();
        for (name, value) in pending {
            if let Some(conflict) =
                crate::env_delivery::check_conflict(sink, &managed, name, value.as_str())?
            {
                conflicts.push(conflict);
            }
        }
        if conflicts.is_empty() {
            return Ok(());
        }
        let payload = serde_json::json!({
            "code": "ENV_CONFLICT",
            "app": app_type.as_str(),
            "conflicts": conflicts,
        });
        Err(AppError::Message(payload.to_string()))
    }

    /// Deliver credentials via environment variables after switching provider
    fn deliver_env_credentials(
        state: &AppState,
        app_type: &AppType,
        provider: &Provider,
        result: &mut SwitchResult,
    ) -> Result<(), AppError> {
        use crate::env_delivery::ManagedEnvVars;

        let sink = crate::env_delivery::default_sink();
        let sink = sink.as_ref();

        let mut managed = ManagedEnvVars::load(&state.db)?;
        let old_vars = if matches!(app_type, AppType::Pi) {
            Vec::new()
        } else {
            managed.vars_for_app(app_type.as_str())
        };

        let pending = Self::collect_pending_env(state, app_type, provider, result)?;
        Self::reject_if_env_conflicts(state, app_type, &pending)?;

        for var_name in &old_vars {
            if let Err(e) = sink.remove(var_name) {
                log::warn!("Failed to remove env var {var_name}: {e}");
                result
                    .warnings
                    .push(format!("env_cleanup_failed:{var_name}"));
            }
            managed.unregister(var_name);
        }

        for (name, value) in pending {
            sink.set(&name, &value)?;
            managed.register(&name, app_type.as_str(), &provider.id);
        }

        managed.save(&state.db)?;
        if let Err(e) = sink.broadcast() {
            log::warn!("Failed to broadcast WM_SETTINGCHANGE: {e}");
        }

        Ok(())
    }

    fn collect_pending_env(
        state: &AppState,
        app_type: &AppType,
        provider: &Provider,
        result: &mut SwitchResult,
    ) -> Result<Vec<(String, Zeroizing<String>)>, AppError> {
        let mut warnings = Vec::new();
        let pending = Self::provider_env_pairs(state, app_type, provider, &mut warnings)?;
        result.warnings.extend(warnings);
        Ok(pending)
    }

    /// 单一真源地计算「某供应商应投递的用户环境变量 (name → value)」。
    /// 切换投递与内置「打开终端」共用，保证非当前供应商也能拿到自己的密钥（§5.3.4）。
    pub(crate) fn provider_env_pairs(
        state: &AppState,
        app_type: &AppType,
        provider: &Provider,
        warnings: &mut Vec<String>,
    ) -> Result<Vec<(String, Zeroizing<String>)>, AppError> {
        use crate::secrets::SecretTarget;
        let mut pending: Vec<(String, Zeroizing<String>)> = Vec::new();
        match app_type {
            AppType::Claude => {
                let key_target =
                    SecretTarget::provider_api_key(app_type.clone(), provider.id.clone());
                let key = futures::executor::block_on(state.secrets.get(&key_target))
                    .ok()
                    .flatten()
                    .ok_or_else(|| AppError::Message("请先补全密钥".to_string()))?;
                let field = provider
                    .meta
                    .as_ref()
                    .and_then(|m| m.api_key_field.as_deref())
                    .unwrap_or("ANTHROPIC_AUTH_TOKEN");
                pending.push((field.to_string(), key));
                if let Ok(Some(url)) = futures::executor::block_on(state.secrets.get(
                    &SecretTarget::provider_base_url(app_type.clone(), provider.id.clone()),
                )) {
                    pending.push(("ANTHROPIC_BASE_URL".to_string(), url));
                }
                pending.extend(load_extra_env_pending(state, app_type, &provider.id));
            }
            AppType::Codex => {
                let key = futures::executor::block_on(state.secrets.get(
                    &SecretTarget::provider_api_key(app_type.clone(), provider.id.clone()),
                ))
                .ok()
                .flatten();
                let official = provider.category.as_deref() == Some("official")
                    || crate::codex_config::is_codex_official_provider(provider);
                let name = if official {
                    "OPENAI_API_KEY"
                } else {
                    "CC_SWITCH_CODEX_API_KEY"
                };
                match key {
                    Some(key) => pending.push((name.to_string(), key)),
                    None => {
                        // 无密钥供应商（header 认证 / preserved login）合法：
                        // 活性安全由 live 写入门控保证，这里只降级为告警。
                        log::warn!(
                            "Codex provider {} has no api_key in SecretStore; \
                             relying on config-carried auth",
                            provider.id
                        );
                        warnings.push(format!("codex_missing_api_key:{}", provider.id));
                    }
                }
            }
            AppType::Pi => {
                match futures::executor::block_on(state.secrets.get(
                    &SecretTarget::provider_api_key(app_type.clone(), provider.id.clone()),
                )) {
                    Ok(Some(api_key)) => {
                        pending.push((crate::secrets::pi_api_key_env_name(&provider.id), api_key));
                    }
                    Ok(None) => {
                        log::warn!("Pi provider {} has no api_key in SecretStore", provider.id);
                        warnings.push(format!("pi_missing_api_key:{}", provider.id));
                    }
                    Err(e) => {
                        log::warn!("Pi retrieve api_key failed for {}: {}", provider.id, e);
                        warnings.push(format!("pi_retrieve_failed:{}", provider.id));
                    }
                }
                if let Some(headers) = provider
                    .settings_config
                    .get("headers")
                    .and_then(Value::as_object)
                {
                    for (header_name, header_value) in headers {
                        if !crate::secrets::is_sensitive_config_key(header_name) {
                            continue;
                        }
                        let Some(val) = header_value.as_str() else {
                            continue;
                        };
                        if crate::secrets::is_literal_value(val) {
                            continue;
                        }
                        match futures::executor::block_on(state.secrets.get(
                            &SecretTarget::provider_env(
                                app_type.clone(),
                                provider.id.clone(),
                                header_name,
                            ),
                        )) {
                            Ok(Some(secret)) => {
                                pending.push((
                                    crate::secrets::pi_header_env_name(&provider.id, header_name),
                                    secret,
                                ));
                            }
                            Ok(None) => {
                                warnings.push(format!(
                                    "pi_missing_header:{}:{header_name}",
                                    provider.id
                                ));
                            }
                            Err(e) => {
                                log::warn!(
                                    "Pi retrieve header {header_name} failed for {}: {e}",
                                    provider.id
                                );
                            }
                        }
                    }
                }
            }
        }
        Ok(pending)
    }

    pub fn adopt_env_vars(
        state: &AppState,
        app_type: &AppType,
        provider_id: &str,
        names: &[String],
    ) -> Result<(), AppError> {
        use crate::env_delivery::ManagedEnvVars;
        let provider = state
            .db
            .get_provider_by_id(provider_id, app_type.as_str())?
            .ok_or_else(|| AppError::Message(format!("供应商 {provider_id} 不存在")))?;
        let sink = crate::env_delivery::default_sink();
        let sink = sink.as_ref();
        let mut managed = ManagedEnvVars::load(&state.db)?;

        // 接管 = 用我方凭据覆盖外来变量并登记所有权；值与切换投递同源。
        let mut warnings = Vec::new();
        let pending = Self::provider_env_pairs(state, app_type, &provider, &mut warnings)?;
        let mut wrote = false;
        for name in names {
            managed.register(name, app_type.as_str(), provider_id);
            if let Some((_, value)) = pending.iter().find(|(k, _)| k == name) {
                sink.set(name, value)?;
                wrote = true;
            }
        }
        managed.save(&state.db)?;
        if wrote {
            if let Err(e) = sink.broadcast() {
                log::warn!("接管环境变量后广播失败: {e}");
            }
        }
        Ok(())
    }

    pub fn migrate_legacy_common_config_usage_if_needed(
        state: &AppState,
        app_type: AppType,
    ) -> Result<(), AppError> {
        if app_type.is_additive_mode() {
            return Ok(());
        }

        let Some(snippet) = state.db.get_config_snippet(app_type.as_str())? else {
            return Ok(());
        };

        if snippet.trim().is_empty() {
            return Ok(());
        }

        Self::migrate_legacy_common_config_usage(state, app_type, &snippet)
    }

    /// 切走某供应商前，把它 live 配置里的可共享部分重新提取并**整体替换**到
    /// 通用配置片段，使在 live 应用里直接做的改动不会因切换而丢失。
    ///
    /// 采用"整体重提取 + 替换"而非"只合并新增"，是为了同时覆盖三种情况：
    /// - **新增**：用户直接在应用里装了插件、加了 hook、改了 env/主题/权限等共享
    ///   偏好，被捕获进通用配置，切到别的供应商也带得过去；
    /// - **删除**：被删掉的键不在新提取结果里，于是从片段里消失、下次切换不会被
    ///   重新注入——否则会出现"插件怎么删也删不掉"的反直觉 bug；
    /// - **密钥安全**：提取器已剥掉 auth / model / endpoint，密钥永不进共享片段。
    ///
    /// 之所以"整体替换"是安全的：每次写 live 都会把当前片段合并进去，所以切走时
    /// 读到的 live 一定是"片段 + 本地改动"的超集，重提取只会丢掉用户真正删掉的键，
    /// 不会误删其它供应商共享的内容。
    ///
    /// **作用域**：Claude + Codex。Codex 提取器（`extract_codex_common_config`）
    /// 已剥离全部供应商专属与 cc-switch 注入内容：`model` / `model_provider` /
    /// 顶层 `base_url` / 整张 `model_providers` 表（含端点与统一会话桶）、
    /// `mcp_servers`（SSOT 在 DB 表）、顶层 `experimental_bearer_token`
    /// fallback、`model_catalog_json`、`web_search = "disabled"` 哨兵——密钥与
    /// 注入产物不会进共享片段。Gemini 暂未纳入，如需支持应单独验证后再加。
    ///
    /// 仅对**显式勾选"写入通用配置"**（`meta.common_config_enabled == Some(true)`）的
    /// 供应商生效；用户**显式清空**过片段（`_cleared`）时跳过，避免把用户主动清掉的
    /// 配置又塞回来。所有失败均为非致命，只记 warning，绝不阻断切换。
    fn sync_common_config_snippet_from_live(
        state: &AppState,
        app_type: &AppType,
        provider: &Provider,
        live_config: &Value,
        result: &mut SwitchResult,
    ) {
        // 作用域限定 Claude + Codex（见函数文档）。
        if !matches!(app_type, AppType::Claude | AppType::Codex) {
            return;
        }

        let opted_in = provider
            .meta
            .as_ref()
            .and_then(|meta| meta.common_config_enabled)
            == Some(true);
        if !opted_in {
            return;
        }

        match state.db.is_config_snippet_cleared(app_type.as_str()) {
            Ok(true) => return, // 用户显式清空过通用配置，尊重其选择，不再自动塞回
            Ok(false) => {}
            Err(err) => {
                log::warn!(
                    "Failed to read common config cleared flag for {}: {err}",
                    app_type.as_str()
                );
                return;
            }
        }

        let new_snippet = match Self::extract_common_config_snippet_from_settings(
            app_type.clone(),
            live_config,
        ) {
            Ok(snippet) => snippet,
            Err(err) => {
                log::warn!(
                    "Failed to extract common config from live for {} provider '{}': {err}",
                    app_type.as_str(),
                    provider.id
                );
                return;
            }
        };

        // 未变化则跳过，避免无谓写库（不切 live 配置时这是常态路径）。
        let current = state
            .db
            .get_config_snippet(app_type.as_str())
            .ok()
            .flatten();
        if current.as_deref() == Some(new_snippet.as_str()) {
            return;
        }

        if let Err(err) = state
            .db
            .set_config_snippet(app_type.as_str(), Some(new_snippet))
        {
            log::warn!(
                "Failed to persist synced common config for {} provider '{}': {err}",
                app_type.as_str(),
                provider.id
            );
            result
                .warnings
                .push(format!("common_config_sync_failed:{}", provider.id));
        }
    }

    /// Extract common config snippet from current provider
    ///
    /// Extracts the current provider's configuration and removes provider-specific fields
    /// (API keys, model settings, endpoints) to create a reusable common config snippet.
    pub fn extract_common_config_snippet(
        state: &AppState,
        app_type: AppType,
    ) -> Result<String, AppError> {
        // Get current provider
        let current_id = Self::current(state, app_type.clone())?;
        if current_id.is_empty() {
            return Err(AppError::Message("No current provider".to_string()));
        }

        let providers = state.db.get_all_providers(app_type.as_str())?;
        let provider = providers
            .get(&current_id)
            .ok_or_else(|| AppError::Message(format!("Provider {current_id} not found")))?;

        match app_type {
            AppType::Claude => Self::extract_claude_common_config(&provider.settings_config),
            AppType::Codex => Self::extract_codex_common_config(&provider.settings_config),
            AppType::Pi => Ok(String::new()),
        }
    }

    /// Extract common config snippet from a config value (e.g. editor content).
    pub fn extract_common_config_snippet_from_settings(
        app_type: AppType,
        settings_config: &Value,
    ) -> Result<String, AppError> {
        match app_type {
            AppType::Claude => Self::extract_claude_common_config(settings_config),
            AppType::Codex => Self::extract_codex_common_config(settings_config),
            AppType::Pi => Ok(String::new()),
        }
    }

    /// Extract common config for Claude (JSON format)
    fn extract_claude_common_config(settings: &Value) -> Result<String, AppError> {
        let mut config = settings.clone();

        // 供应商专属的**非机密**字段（模型 + 端点），不应共享。凭据/机密不在此列举，
        // 改由 `is_sensitive_config_key`（模式匹配）统一剥离，新供应商的 `*_API_KEY`
        // 等无需再手工补名单即可被覆盖。
        const ENV_PROVIDER_SPECIFIC_EXCLUDES: &[&str] = &[
            "ANTHROPIC_MODEL",
            "ANTHROPIC_REASONING_MODEL", // legacy: 已废弃，但旧配置可能残留
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
            // Fable 是 v3.16.3 新增的第四档模型映射，与 haiku/sonnet/opus 同属供应商专属，
            // 不得进入通用配置片段，否则会污染其它供应商（issue #4272）。
            "ANTHROPIC_DEFAULT_FABLE_MODEL",
            "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME",
            "CLAUDE_CODE_SUBAGENT_MODEL",
            // Context limits follow the actual upstream model. Sharing these
            // across providers can cap GPT/Kimi to the wrong window and make
            // Claude Code compact too early or miss the upstream limit.
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
            "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
            "ANTHROPIC_BASE_URL",
        ];

        const TOP_LEVEL_EXCLUDES: &[&str] = &[
            "apiBaseUrl",
            // Legacy model fields
            "primaryModel",
            "smallFastModel",
        ];

        // Remove env fields: provider-specific (models/endpoint) + 任何凭据键。
        if let Some(env) = config.get_mut("env").and_then(|v| v.as_object_mut()) {
            let sensitive: Vec<String> = env
                .keys()
                .filter(|k| crate::secrets::is_sensitive_config_key(k))
                .cloned()
                .collect();
            for key in ENV_PROVIDER_SPECIFIC_EXCLUDES {
                env.remove(*key);
            }
            for key in &sensitive {
                env.remove(key);
            }
            // If env is empty after removal, remove the env object itself
            if env.is_empty() {
                config.as_object_mut().map(|obj| obj.remove("env"));
            }
        }

        // Remove top-level fields: legacy model fields + 任何凭据键
        // （例如非标准的顶层 apiKey / api_key / *_TOKEN）。
        if let Some(obj) = config.as_object_mut() {
            let sensitive: Vec<String> = obj
                .keys()
                .filter(|k| crate::secrets::is_sensitive_config_key(k))
                .cloned()
                .collect();
            for key in TOP_LEVEL_EXCLUDES {
                obj.remove(*key);
            }
            for key in &sensitive {
                obj.remove(key);
            }
        }

        // Check if result is empty
        if config.as_object().is_none_or(|obj| obj.is_empty()) {
            return Ok("{}".to_string());
        }

        serde_json::to_string_pretty(&config)
            .map_err(|e| AppError::Message(format!("Serialization failed: {e}")))
    }

    /// Extract common config for Codex (TOML format)
    fn extract_codex_common_config(settings: &Value) -> Result<String, AppError> {
        // Codex config is stored as { "auth": {...}, "config": "toml string" }
        let config_toml = settings
            .get("config")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if config_toml.is_empty() {
            return Ok(String::new());
        }

        let mut doc = config_toml
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| AppError::Message(format!("TOML parse error: {e}")))?;

        // Remove provider-specific fields.
        let root = doc.as_table_mut();
        root.remove("model");
        root.remove("model_provider");
        // Legacy/alt formats might use a top-level base_url.
        root.remove("base_url");
        // wire_api 与 base_url 同属供应商路由语义：无 model_provider 时
        // update_codex_toml_field / 前端 setCodexWireApi 都会把它落在顶层，
        // 进了片段会改写其它供应商的协议选择（chat vs responses）。
        root.remove("wire_api");

        // Remove entire model_providers table (provider-specific configuration)
        root.remove("model_providers");

        // MCP 服务器归 DB mcp_servers 表所有：进了共享片段会绕过按应用的
        // 启用状态被合并进所有勾选通用配置的供应商，且在通用配置编辑框里
        // 显示为一份"重复"的 MCP 配置。
        root.remove("mcp_servers");
        // 历史错误格式 [mcp.servers] 一并剥离（与 strip_codex_mcp_servers_from_settings
        // 一致）：sync_all_enabled 只管理 [mcp_servers.*]，legacy 形态一旦进了
        // 片段就会被合并进所有供应商，且没有任何同步路径能清掉这个孤儿。
        if let Some(mcp_tbl) = root
            .get_mut("mcp")
            .and_then(|item| item.as_table_like_mut())
        {
            mcp_tbl.remove("servers");
            if mcp_tbl.is_empty() {
                root.remove("mcp");
            }
        }

        // cc-switch 写 live 时注入的产物一律不进共享片段：
        // - experimental_bearer_token 正常写在 [model_providers.<id>] 内（上面
        //   整表已剥），但无活跃路由 / 内建保留 id / 路由表缺失三种 fallback
        //   会落在顶层——不剥等于把 API 密钥写进共享片段。
        root.remove("experimental_bearer_token");
        // - model_catalog_json 指向按供应商生成的 catalog 投影文件（DB 为 SSOT）。
        root.remove("model_catalog_json");
        // - web_search 只剥 cc-switch 注入的 "disabled" 哨兵；用户手设的其它值
        //   属于可共享偏好，保留。
        if root
            .get(crate::codex_config::CODEX_WEB_SEARCH_FIELD)
            .and_then(|item| item.as_str())
            == Some(crate::codex_config::CODEX_WEB_SEARCH_DISABLED)
        {
            root.remove(crate::codex_config::CODEX_WEB_SEARCH_FIELD);
        }

        // Clean up multiple empty lines (keep at most one blank line).
        let mut cleaned = String::new();
        let mut blank_run = 0usize;
        for line in doc.to_string().lines() {
            if line.trim().is_empty() {
                blank_run += 1;
                if blank_run <= 1 {
                    cleaned.push('\n');
                }
                continue;
            }
            blank_run = 0;
            cleaned.push_str(line);
            cleaned.push('\n');
        }

        Ok(cleaned.trim().to_string())
    }

    /// Import default configuration from live files (re-export)
    ///
    /// Returns `Ok(true)` if imported, `Ok(false)` if skipped.
    pub fn import_default_config(state: &AppState, app_type: AppType) -> Result<bool, AppError> {
        import_default_config(state, app_type)
    }

    pub fn should_import_default_config_on_startup(
        state: &AppState,
        app_type: &AppType,
    ) -> Result<bool, AppError> {
        should_import_default_config_on_startup(state, app_type)
    }

    /// Read current live settings (re-export)
    pub fn read_live_settings(app_type: AppType) -> Result<Value, AppError> {
        read_live_settings(app_type)
    }

    /// Update provider sort order
    pub fn update_sort_order(
        state: &AppState,
        app_type: AppType,
        updates: Vec<ProviderSortUpdate>,
    ) -> Result<bool, AppError> {
        let mut providers = state.db.get_all_providers(app_type.as_str())?;

        for update in updates {
            if let Some(provider) = providers.get_mut(&update.id) {
                provider.sort_index = Some(update.sort_index);
                state.db.save_provider(app_type.as_str(), provider)?;
            }
        }

        Ok(true)
    }

    fn validate_provider_settings(app_type: &AppType, provider: &Provider) -> Result<(), AppError> {
        match app_type {
            AppType::Claude => {
                if !provider.settings_config.is_object() {
                    return Err(AppError::localized(
                        "provider.claude.settings.not_object",
                        "Claude 配置必须是 JSON 对象",
                        "Claude configuration must be a JSON object",
                    ));
                }
            }
            AppType::Codex => {
                let settings = provider.settings_config.as_object().ok_or_else(|| {
                    AppError::localized(
                        "provider.codex.settings.not_object",
                        "Codex 配置必须是 JSON 对象",
                        "Codex configuration must be a JSON object",
                    )
                })?;

                let auth = settings.get("auth").ok_or_else(|| {
                    AppError::localized(
                        "provider.codex.auth.missing",
                        format!("供应商 {} 缺少 auth 配置", provider.id),
                        format!("Provider {} is missing auth configuration", provider.id),
                    )
                })?;
                if !auth.is_object() {
                    return Err(AppError::localized(
                        "provider.codex.auth.not_object",
                        format!("供应商 {} 的 auth 配置必须是 JSON 对象", provider.id),
                        format!(
                            "Provider {} auth configuration must be a JSON object",
                            provider.id
                        ),
                    ));
                }

                if let Some(config_value) = settings.get("config") {
                    if !(config_value.is_string() || config_value.is_null()) {
                        return Err(AppError::localized(
                            "provider.codex.config.invalid_type",
                            "Codex config 字段必须是字符串",
                            "Codex config field must be a string",
                        ));
                    }
                    if let Some(cfg_text) = config_value.as_str() {
                        crate::codex_config::validate_config_toml(cfg_text)?;
                    }
                }
            }
            AppType::Pi => {
                crate::pi_config::validate_provider_node(&provider.id, &provider.settings_config)?;
            }
        }

        Ok(())
    }
}

/// Normalize Claude model keys in a JSON value
///
/// Reads old key (ANTHROPIC_SMALL_FAST_MODEL), writes new keys (DEFAULT_*), and deletes old key.
pub(crate) fn normalize_claude_models_in_value(settings: &mut Value) -> bool {
    let mut changed = false;
    let env = match settings.get_mut("env").and_then(|v| v.as_object_mut()) {
        Some(obj) => obj,
        None => return changed,
    };

    let model = env
        .get("ANTHROPIC_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let small_fast = env
        .get("ANTHROPIC_SMALL_FAST_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let current_haiku = env
        .get("ANTHROPIC_DEFAULT_HAIKU_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let current_sonnet = env
        .get("ANTHROPIC_DEFAULT_SONNET_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let current_opus = env
        .get("ANTHROPIC_DEFAULT_OPUS_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let target_haiku = current_haiku
        .or_else(|| small_fast.clone())
        .or_else(|| model.clone());
    let target_sonnet = current_sonnet
        .or_else(|| model.clone())
        .or_else(|| small_fast.clone());
    let target_opus = current_opus
        .or_else(|| model.clone())
        .or_else(|| small_fast.clone());

    if env.get("ANTHROPIC_DEFAULT_HAIKU_MODEL").is_none() {
        if let Some(v) = target_haiku {
            env.insert(
                "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
                Value::String(v),
            );
            changed = true;
        }
    }
    if env.get("ANTHROPIC_DEFAULT_SONNET_MODEL").is_none() {
        if let Some(v) = target_sonnet {
            env.insert(
                "ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(),
                Value::String(v),
            );
            changed = true;
        }
    }
    if env.get("ANTHROPIC_DEFAULT_OPUS_MODEL").is_none() {
        if let Some(v) = target_opus {
            env.insert("ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(), Value::String(v));
            changed = true;
        }
    }

    if env.remove("ANTHROPIC_SMALL_FAST_MODEL").is_some() {
        changed = true;
    }

    changed
}

pub(super) fn delete_provider_secrets(state: &AppState, app_type: &AppType, id: &str) {
    let prefix = crate::secrets::provider_target_prefix(app_type, id);
    let mut targets = crate::secrets::load_known_targets(state.db.as_ref()).unwrap_or_default();
    let related: Vec<String> = targets
        .iter()
        .filter(|t| t.starts_with(&prefix))
        .cloned()
        .collect();
    let fallback = [
        crate::secrets::SecretTarget::provider_api_key(app_type.clone(), id).to_target_string(),
        crate::secrets::SecretTarget::provider_base_url(app_type.clone(), id).to_target_string(),
    ];
    let mut to_delete = related;
    for t in fallback {
        if !to_delete.iter().any(|x| x == &t) {
            to_delete.push(t);
        }
    }
    for target_str in &to_delete {
        if let Some(target) = parse_secret_target(target_str) {
            if let Err(e) = futures::executor::block_on(state.secrets.delete(&target)) {
                log::warn!("删除凭据失败 {target_str}: {e}");
            }
        }
    }
    targets.retain(|t| !t.starts_with(&prefix));
    if let Err(e) = crate::secrets::save_known_targets(state.db.as_ref(), &targets) {
        log::warn!("更新 known_secret_targets 失败: {e}");
    }
}

fn parse_secret_target(target: &str) -> Option<crate::secrets::SecretTarget> {
    let rest = target.strip_prefix("cc-switch/v1/provider/")?;
    let mut parts = rest.splitn(3, '/');
    let app = AppType::from_str(parts.next()?).ok()?;
    let provider_id = parts.next()?;
    let field = parts.next()?;
    match field {
        "api_key" => Some(crate::secrets::SecretTarget::provider_api_key(
            app,
            provider_id,
        )),
        "base_url" => Some(crate::secrets::SecretTarget::provider_base_url(
            app,
            provider_id,
        )),
        other => other
            .strip_prefix("env/")
            .map(|var| crate::secrets::SecretTarget::provider_env(app, provider_id, var)),
    }
}

fn load_extra_env_pending(
    state: &AppState,
    app_type: &AppType,
    provider_id: &str,
) -> Vec<(String, Zeroizing<String>)> {
    let prefix = format!(
        "cc-switch/v1/provider/{}/{}/env/",
        app_type.as_str(),
        provider_id
    );
    let Ok(targets) = crate::secrets::load_known_targets(state.db.as_ref()) else {
        return Vec::new();
    };
    let mut pending = Vec::new();
    for target_str in targets {
        let Some(var) = target_str.strip_prefix(&prefix) else {
            continue;
        };
        if var.is_empty() {
            continue;
        }
        let target = crate::secrets::SecretTarget::provider_env(app_type.clone(), provider_id, var);
        if let Ok(Some(value)) = futures::executor::block_on(state.secrets.get(&target)) {
            pending.push((var.to_string(), value));
        }
    }
    pending
}

fn strip_and_store_provider_secrets(
    state: &AppState,
    app_type: &AppType,
    provider: &mut Provider,
) -> Result<(), AppError> {
    let extractor =
        SecretExtractor::new(state.secrets.as_ref(), app_type.clone()).with_db(state.db.as_ref());
    let field = provider
        .meta
        .as_ref()
        .and_then(|m| m.api_key_field.as_deref());
    let (stripped, _) =
        futures::executor::block_on(extractor.extract_provider_secrets_with_field(
            &provider.id,
            &provider.settings_config,
            field,
        ))?;
    provider.settings_config = stripped;
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderSortUpdate {
    pub id: String,
    #[serde(rename = "sortIndex")]
    pub sort_index: usize,
}
