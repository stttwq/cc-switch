//! Live configuration operations
//!
//! Handles reading and writing live configuration files for Claude, Codex, and Pi.

use serde_json::Value;
use toml_edit::{DocumentMut, Item, TableLike};

use crate::app_config::AppType;
use crate::config::{get_claude_settings_path, read_json_file, write_json_file};
use crate::database::Database;
use crate::error::AppError;
use crate::provider::Provider;
use crate::services::mcp::McpService;
use crate::store::AppState;

use super::normalize_claude_models_in_value;

const KIMI_FOR_CODING_CONTEXT_TOKENS: &str = "262144";

fn is_kimi_for_coding_provider(provider: &Provider) -> bool {
    provider
        .settings_config
        .pointer("/env/ANTHROPIC_BASE_URL")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(|url| url.trim_end_matches('/'))
        == Some("https://api.kimi.com/coding")
}

/// Kimi For Coding serves a 256K window, but Claude Code caps unknown models at
/// 200K unless `CLAUDE_CODE_MAX_CONTEXT_TOKENS` is set — and that env is ignored
/// for `claude-`-prefixed ids, so these defaults only bite when the provider also
/// routes the endpoint's `kimi-for-coding` alias (the preset does). Keep the
/// defaults provider-owned so an old shared snippet cannot override them.
fn apply_kimi_for_coding_context_defaults(settings: &mut Value, provider: &Provider) {
    if !is_kimi_for_coding_provider(provider) {
        return;
    }

    let provider_env = provider
        .settings_config
        .get("env")
        .and_then(Value::as_object);
    let Some(env) = settings.get_mut("env").and_then(Value::as_object_mut) else {
        return;
    };

    for key in [
        "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
        "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
    ] {
        let value = provider_env
            .and_then(|provider_env| provider_env.get(key))
            .cloned()
            .unwrap_or_else(|| Value::String(KIMI_FOR_CODING_CONTEXT_TOKENS.to_string()));
        env.insert(key.to_string(), value);
    }
}

/// S5 / §5.3.1：写 live 前的门控是**失败关闭**的——净化流程一旦判定配置里
/// 仍有无法剥离的敏感材料，直接报错不写，绝不退回"尽量写干净"。
pub(crate) fn sanitize_claude_settings_for_live(settings: &Value) -> Result<Value, AppError> {
    super::live_sanitizer::sanitize_claude_settings_for_live_write(settings)
}

pub(crate) fn provider_exists_in_live_config(
    app_type: &AppType,
    provider_id: &str,
) -> Result<bool, AppError> {
    match app_type {
        AppType::Pi => crate::pi_config::pi_provider_exists(provider_id),
        _ => Ok(false),
    }
}

fn json_is_subset(target: &Value, source: &Value) -> bool {
    match source {
        Value::Object(source_map) => {
            let Some(target_map) = target.as_object() else {
                return false;
            };
            source_map.iter().all(|(key, source_value)| {
                target_map
                    .get(key)
                    .is_some_and(|target_value| json_is_subset(target_value, source_value))
            })
        }
        Value::Array(source_arr) => {
            let Some(target_arr) = target.as_array() else {
                return false;
            };
            json_array_contains_subset(target_arr, source_arr)
        }
        _ => target == source,
    }
}

fn json_array_contains_subset(target_arr: &[Value], source_arr: &[Value]) -> bool {
    let mut matched = vec![false; target_arr.len()];

    source_arr.iter().all(|source_item| {
        if let Some((index, _)) = target_arr.iter().enumerate().find(|(index, target_item)| {
            !matched[*index] && json_is_subset(target_item, source_item)
        }) {
            matched[index] = true;
            true
        } else {
            false
        }
    })
}

fn json_remove_array_items(target_arr: &mut Vec<Value>, source_arr: &[Value]) {
    for source_item in source_arr {
        if let Some(index) = target_arr
            .iter()
            .position(|target_item| json_is_subset(target_item, source_item))
        {
            target_arr.remove(index);
        }
    }
}

fn json_deep_merge(target: &mut Value, source: &Value) {
    match (target, source) {
        (Value::Object(target_map), Value::Object(source_map)) => {
            for (key, source_value) in source_map {
                match target_map.get_mut(key) {
                    Some(target_value) => json_deep_merge(target_value, source_value),
                    None => {
                        target_map.insert(key.clone(), source_value.clone());
                    }
                }
            }
        }
        (target_value, source_value) => {
            *target_value = source_value.clone();
        }
    }
}

fn json_deep_remove(target: &mut Value, source: &Value) {
    let (Some(target_map), Some(source_map)) = (target.as_object_mut(), source.as_object()) else {
        return;
    };

    for (key, source_value) in source_map {
        let mut remove_key = false;

        if let Some(target_value) = target_map.get_mut(key) {
            if source_value.is_object() && target_value.is_object() {
                json_deep_remove(target_value, source_value);
                remove_key = target_value.as_object().is_some_and(|obj| obj.is_empty());
            } else if let (Some(target_arr), Some(source_arr)) =
                (target_value.as_array_mut(), source_value.as_array())
            {
                json_remove_array_items(target_arr, source_arr);
                remove_key = target_arr.is_empty();
            } else if json_is_subset(target_value, source_value) {
                remove_key = true;
            }
        }

        if remove_key {
            target_map.remove(key);
        }
    }
}

fn toml_value_is_subset(target: &toml_edit::Value, source: &toml_edit::Value) -> bool {
    match (target, source) {
        (toml_edit::Value::String(target), toml_edit::Value::String(source)) => {
            target.value() == source.value()
        }
        (toml_edit::Value::Integer(target), toml_edit::Value::Integer(source)) => {
            target.value() == source.value()
        }
        (toml_edit::Value::Float(target), toml_edit::Value::Float(source)) => {
            target.value() == source.value()
        }
        (toml_edit::Value::Boolean(target), toml_edit::Value::Boolean(source)) => {
            target.value() == source.value()
        }
        (toml_edit::Value::Datetime(target), toml_edit::Value::Datetime(source)) => {
            target.value() == source.value()
        }
        (toml_edit::Value::Array(target), toml_edit::Value::Array(source)) => {
            toml_array_contains_subset(target, source)
        }
        (toml_edit::Value::InlineTable(target), toml_edit::Value::InlineTable(source)) => {
            source.iter().all(|(key, source_item)| {
                target
                    .get(key)
                    .is_some_and(|target_item| toml_value_is_subset(target_item, source_item))
            })
        }
        _ => false,
    }
}

fn toml_array_contains_subset(target: &toml_edit::Array, source: &toml_edit::Array) -> bool {
    let mut matched = vec![false; target.len()];
    let target_items: Vec<&toml_edit::Value> = target.iter().collect();

    source.iter().all(|source_item| {
        if let Some((index, _)) = target_items
            .iter()
            .enumerate()
            .find(|(index, target_item)| {
                !matched[*index] && toml_value_is_subset(target_item, source_item)
            })
        {
            matched[index] = true;
            true
        } else {
            false
        }
    })
}

fn toml_remove_array_items(target: &mut toml_edit::Array, source: &toml_edit::Array) {
    for source_item in source.iter() {
        let index = {
            let target_items: Vec<&toml_edit::Value> = target.iter().collect();
            target_items
                .iter()
                .enumerate()
                .find(|(_, target_item)| toml_value_is_subset(target_item, source_item))
                .map(|(index, _)| index)
        };

        if let Some(index) = index {
            target.remove(index);
        }
    }
}

fn toml_item_is_subset(target: &Item, source: &Item) -> bool {
    if let Some(source_table) = source.as_table_like() {
        let Some(target_table) = target.as_table_like() else {
            return false;
        };
        return source_table.iter().all(|(key, source_item)| {
            target_table
                .get(key)
                .is_some_and(|target_item| toml_item_is_subset(target_item, source_item))
        });
    }

    match (target.as_value(), source.as_value()) {
        (Some(target_value), Some(source_value)) => {
            toml_value_is_subset(target_value, source_value)
        }
        _ => false,
    }
}

fn merge_toml_item(target: &mut Item, source: &Item) {
    if let Some(source_table) = source.as_table_like() {
        if let Some(target_table) = target.as_table_like_mut() {
            merge_toml_table_like(target_table, source_table);
            return;
        }
    }

    *target = source.clone();
}

fn merge_toml_table_like(target: &mut dyn TableLike, source: &dyn TableLike) {
    for (key, source_item) in source.iter() {
        match target.get_mut(key) {
            Some(target_item) => merge_toml_item(target_item, source_item),
            None => {
                target.insert(key, source_item.clone());
            }
        }
    }
}

fn remove_toml_item(target: &mut Item, source: &Item) {
    if let Some(source_table) = source.as_table_like() {
        if let Some(target_table) = target.as_table_like_mut() {
            remove_toml_table_like(target_table, source_table);
            if target_table.is_empty() {
                *target = Item::None;
            }
            return;
        }
    }

    if let Some(source_value) = source.as_value() {
        let mut remove_item = false;

        if let Some(target_value) = target.as_value_mut() {
            match (target_value, source_value) {
                (toml_edit::Value::Array(target_arr), toml_edit::Value::Array(source_arr)) => {
                    toml_remove_array_items(target_arr, source_arr);
                    remove_item = target_arr.is_empty();
                }
                (target_value, source_value)
                    if toml_value_is_subset(target_value, source_value) =>
                {
                    remove_item = true;
                }
                _ => {}
            }
        }

        if remove_item {
            *target = Item::None;
        }
    }
}

fn remove_toml_table_like(target: &mut dyn TableLike, source: &dyn TableLike) {
    let keys: Vec<String> = source.iter().map(|(key, _)| key.to_string()).collect();

    for key in keys {
        let mut remove_key = false;
        if let (Some(target_item), Some(source_item)) = (target.get_mut(&key), source.get(&key)) {
            remove_toml_item(target_item, source_item);
            remove_key = target_item.is_none()
                || target_item
                    .as_table_like()
                    .is_some_and(|table_like| table_like.is_empty());
        }

        if remove_key {
            target.remove(&key);
        }
    }
}

/// 前端表单勾选/取消"使用通用配置"时，对编辑器里的 config.toml 文本做
/// 结构化合并/剥离。必须在后端用 toml_edit 做：前端 smol-toml 只能
/// parse → merge → 整文档重序列化，注释全丢、键序重排，还会生成多余的
/// 空父表头（如 `[model_providers]`）。
pub fn update_toml_common_config_snippet(
    config_toml: &str,
    snippet_toml: &str,
    enabled: bool,
) -> Result<String, AppError> {
    let trimmed = snippet_toml.trim();
    if trimmed.is_empty() {
        return Ok(config_toml.to_string());
    }

    let mut target_doc = if config_toml.trim().is_empty() {
        DocumentMut::new()
    } else {
        config_toml
            .parse::<DocumentMut>()
            .map_err(|e| AppError::Message(format!("Invalid Codex config.toml: {e}")))?
    };
    let source_doc = trimmed
        .parse::<DocumentMut>()
        .map_err(|e| AppError::Message(format!("Invalid Codex common config snippet: {e}")))?;

    if enabled {
        merge_toml_table_like(target_doc.as_table_mut(), source_doc.as_table());
    } else {
        remove_toml_table_like(target_doc.as_table_mut(), source_doc.as_table());
    }

    Ok(target_doc.to_string())
}

fn settings_contain_common_config(app_type: &AppType, settings: &Value, snippet: &str) -> bool {
    let trimmed = snippet.trim();
    if trimmed.is_empty() {
        return false;
    }

    match app_type {
        AppType::Claude => match serde_json::from_str::<Value>(trimmed) {
            Ok(source) if source.is_object() => json_is_subset(settings, &source),
            _ => false,
        },
        AppType::Codex => {
            let config_toml = settings.get("config").and_then(Value::as_str).unwrap_or("");
            if config_toml.trim().is_empty() {
                return false;
            }

            let target_doc = match config_toml.parse::<DocumentMut>() {
                Ok(doc) => doc,
                Err(_) => return false,
            };
            let source_doc = match trimmed.parse::<DocumentMut>() {
                Ok(doc) => doc,
                Err(_) => return false,
            };

            toml_item_is_subset(target_doc.as_item(), source_doc.as_item())
        }
        AppType::Pi => false,
    }
}

pub(crate) fn provider_uses_common_config(
    app_type: &AppType,
    provider: &Provider,
    snippet: Option<&str>,
) -> bool {
    match provider
        .meta
        .as_ref()
        .and_then(|meta| meta.common_config_enabled)
    {
        Some(explicit) => explicit && snippet.is_some_and(|value| !value.trim().is_empty()),
        None => snippet.is_some_and(|value| {
            settings_contain_common_config(app_type, &provider.settings_config, value)
        }),
    }
}

pub(crate) fn remove_common_config_from_settings(
    app_type: &AppType,
    settings: &Value,
    snippet: &str,
) -> Result<Value, AppError> {
    let trimmed = snippet.trim();
    if trimmed.is_empty() {
        return Ok(settings.clone());
    }

    match app_type {
        AppType::Claude => {
            let source = serde_json::from_str::<Value>(trimmed)
                .map_err(|e| AppError::Message(format!("Invalid Claude common config: {e}")))?;
            let mut result = settings.clone();
            json_deep_remove(&mut result, &source);
            Ok(result)
        }
        AppType::Codex => {
            let mut result = settings.clone();
            let config_toml = settings.get("config").and_then(Value::as_str).unwrap_or("");
            let mut target_doc = if config_toml.trim().is_empty() {
                DocumentMut::new()
            } else {
                config_toml.parse::<DocumentMut>().map_err(|e| {
                    AppError::Message(format!(
                        "Invalid Codex config.toml while removing common config: {e}"
                    ))
                })?
            };
            let source_doc = trimmed.parse::<DocumentMut>().map_err(|e| {
                AppError::Message(format!("Invalid Codex common config snippet: {e}"))
            })?;

            remove_toml_table_like(target_doc.as_table_mut(), source_doc.as_table());
            if let Some(obj) = result.as_object_mut() {
                obj.insert("config".to_string(), Value::String(target_doc.to_string()));
            }
            Ok(result)
        }
        AppType::Pi => Ok(settings.clone()),
    }
}

fn apply_common_config_to_settings(
    app_type: &AppType,
    settings: &Value,
    snippet: &str,
) -> Result<Value, AppError> {
    let trimmed = snippet.trim();
    if trimmed.is_empty() {
        return Ok(settings.clone());
    }

    match app_type {
        AppType::Claude => {
            let source = serde_json::from_str::<Value>(trimmed)
                .map_err(|e| AppError::Message(format!("Invalid Claude common config: {e}")))?;
            let mut result = settings.clone();
            json_deep_merge(&mut result, &source);
            Ok(result)
        }
        AppType::Codex => {
            let mut result = settings.clone();
            let config_toml = settings.get("config").and_then(Value::as_str).unwrap_or("");
            let mut target_doc = if config_toml.trim().is_empty() {
                DocumentMut::new()
            } else {
                config_toml.parse::<DocumentMut>().map_err(|e| {
                    AppError::Message(format!(
                        "Invalid Codex config.toml while applying common config: {e}"
                    ))
                })?
            };
            let source_doc = trimmed.parse::<DocumentMut>().map_err(|e| {
                AppError::Message(format!("Invalid Codex common config snippet: {e}"))
            })?;

            merge_toml_table_like(target_doc.as_table_mut(), source_doc.as_table());
            if let Some(obj) = result.as_object_mut() {
                obj.insert("config".to_string(), Value::String(target_doc.to_string()));
            }
            Ok(result)
        }
        AppType::Pi => Ok(settings.clone()),
    }
}

pub(crate) fn build_effective_settings_with_common_config(
    db: &Database,
    app_type: &AppType,
    provider: &Provider,
) -> Result<Value, AppError> {
    let snippet = db.get_config_snippet(app_type.as_str())?;
    let mut effective_settings = provider.settings_config.clone();

    if provider_uses_common_config(app_type, provider, snippet.as_deref()) {
        if let Some(snippet_text) = snippet.as_deref() {
            match apply_common_config_to_settings(app_type, &effective_settings, snippet_text) {
                Ok(settings) => effective_settings = settings,
                Err(err) => {
                    log::warn!(
                        "Failed to apply common config for {} provider '{}': {err}",
                        app_type.as_str(),
                        provider.id
                    );
                }
            }
        }
    }

    if matches!(app_type, AppType::Claude) {
        apply_kimi_for_coding_context_defaults(&mut effective_settings, provider);
    }

    Ok(effective_settings)
}

pub(crate) fn write_live_with_common_config_for_state(
    state: &AppState,
    app_type: &AppType,
    provider: &Provider,
) -> Result<(), AppError> {
    write_live_with_common_config(state, app_type, provider)
}

/// Validate the target provider's Codex live projection without writing.
pub(crate) fn preflight_codex_live_write_for_state(
    state: &AppState,
    provider: &Provider,
) -> Result<(), AppError> {
    let effective =
        build_effective_provider_for_live(state.db.as_ref(), &AppType::Codex, provider)?;
    let obj = effective
        .settings_config
        .as_object()
        .ok_or_else(|| AppError::Config("Codex 供应商配置必须是 JSON 对象".to_string()))?;
    let auth = obj
        .get("auth")
        .ok_or_else(|| AppError::Config("Codex 供应商配置缺少 'auth' 字段".to_string()))?;
    let config_str = obj.get("config").and_then(|v| v.as_str());
    // Env-var delivery: the sanitizer injects `env_key` (the store holds the
    // literal key). Preflight must judge the same text the write path
    // produces, or a key-in-store legacy shape is mis-refused here. The
    // injection is gated on the store actually holding the key, so a
    // keyless provider keeps the fail-closed fallback gate.
    let has_store_key = futures::executor::block_on(state.secrets.retrieve(
        &crate::secrets::SecretTarget::provider_api_key(AppType::Codex, provider.id.clone()),
    ))
    .ok()
    .flatten()
    .is_some();
    let config_for_preflight = config_str
        .map(|text| {
            crate::services::provider::codex_sanitizer::sanitize_codex_config_for_live_write_preflight(
                text,
                has_store_key,
            )
        })
        .transpose()?
        .or(config_str.map(str::to_string));
    crate::codex_config::preflight_codex_live_write(
        effective.category.as_deref(),
        auth,
        config_for_preflight.as_deref(),
    )
}

pub(crate) fn write_live_with_common_config(
    state: &AppState,
    app_type: &AppType,
    provider: &Provider,
) -> Result<(), AppError> {
    let effective_provider =
        build_effective_provider_for_live(state.db.as_ref(), app_type, provider)?;
    write_live_snapshot(state, app_type, &effective_provider)
}

pub(crate) fn build_effective_provider_for_live(
    db: &Database,
    app_type: &AppType,
    provider: &Provider,
) -> Result<Provider, AppError> {
    let mut effective_provider = provider.clone();
    effective_provider.settings_config =
        build_effective_settings_with_common_config(db, app_type, provider)?;
    Ok(effective_provider)
}

pub(crate) fn strip_common_config_from_live_settings(
    db: &Database,
    app_type: &AppType,
    provider: &Provider,
    live_settings: Value,
) -> Value {
    let snippet = match db.get_config_snippet(app_type.as_str()) {
        Ok(snippet) => snippet,
        Err(err) => {
            log::warn!(
                "Failed to load common config for {} while backfilling '{}': {err}",
                app_type.as_str(),
                provider.id
            );
            return restore_live_settings_for_provider_backfill(app_type, provider, live_settings);
        }
    };

    let backfill_settings = if provider_uses_common_config(app_type, provider, snippet.as_deref()) {
        match snippet.as_deref() {
            Some(snippet_text) => {
                match remove_common_config_from_settings(app_type, &live_settings, snippet_text) {
                    Ok(settings) => settings,
                    Err(err) => {
                        log::warn!(
                            "Failed to strip common config for {} provider '{}': {err}",
                            app_type.as_str(),
                            provider.id
                        );
                        live_settings
                    }
                }
            }
            None => live_settings,
        }
    } else {
        live_settings
    };

    restore_live_settings_for_provider_backfill(app_type, provider, backfill_settings)
}

fn strip_injected_kimi_for_coding_context_defaults(settings: &mut Value, provider: &Provider) {
    if !is_kimi_for_coding_provider(provider) {
        return;
    }
    let provider_env = provider
        .settings_config
        .get("env")
        .and_then(Value::as_object);
    let Some(env) = settings.get_mut("env").and_then(Value::as_object_mut) else {
        return;
    };
    for key in [
        "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
        "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
    ] {
        if provider_env.is_some_and(|provider_env| provider_env.contains_key(key)) {
            continue;
        }
        if env.get(key).and_then(Value::as_str) == Some(KIMI_FOR_CODING_CONTEXT_TOKENS) {
            env.remove(key);
        }
    }
}

fn restore_live_settings_for_provider_backfill(
    app_type: &AppType,
    provider: &Provider,
    live_settings: Value,
) -> Value {
    if matches!(app_type, AppType::Claude) {
        let mut settings = live_settings;
        strip_injected_kimi_for_coding_context_defaults(&mut settings, provider);
        return settings;
    }
    if !matches!(app_type, AppType::Codex) {
        return live_settings;
    }

    let mut settings = live_settings;
    let restore_provider_token =
        crate::codex_config::should_restore_codex_provider_token_for_backfill(
            provider.category.as_deref(),
            &provider.settings_config,
        );
    if let Err(err) = crate::codex_config::restore_codex_settings_for_backfill(
        &mut settings,
        &provider.settings_config,
        restore_provider_token,
    ) {
        log::warn!(
            "Failed to restore Codex settings while backfilling '{}': {err}",
            provider.id
        );
    }

    // MCP 服务器归 DB mcp_servers 表所有，live 里的 [mcp_servers] 是同步投影；
    // 回填时剥掉，否则已删除的服务器会随供应商快照复活（逐条 reconcile 清不掉孤儿）。
    if let Err(err) = crate::codex_config::strip_codex_mcp_servers_from_settings(&mut settings) {
        log::warn!(
            "Failed to strip mcp_servers while backfilling '{}': {err}",
            provider.id
        );
    }

    // 统一会话开关注入的共享 `custom` 路由只属于 live 配置；切换回填时
    // 必须剥掉，否则官方供应商的存储配置被污染，关闭开关后无法还原。
    if provider.category.as_deref() == Some("official")
        || crate::codex_config::is_codex_official_provider(provider)
    {
        if let Err(err) =
            crate::codex_config::strip_codex_unified_session_bucket_from_settings(&mut settings)
        {
            log::warn!(
                "Failed to strip unified session bucket while backfilling '{}': {err}",
                provider.id
            );
        }
    }

    // `modelCatalog` is a cc-switch–private field whose SSOT is the DB. Live's
    // `config.toml` only carries a lossy projection (`model_catalog_json` →
    // generated catalog file) that proxy takeover/restore cycles and Codex.app
    // config rewrites can drop, so `read_live_settings` may reconstruct it as
    // absent. Never let a switch-away backfill from Live erase the stored
    // mapping: prefer the DB provider's `modelCatalog`, falling back to whatever
    // Live reconstructed only when the DB has none.
    if let Some(stored_catalog) = provider.settings_config.get("modelCatalog") {
        if let Some(obj) = settings.as_object_mut() {
            obj.insert("modelCatalog".to_string(), stored_catalog.clone());
        }
    }

    settings
}

pub(crate) fn normalize_provider_common_config_for_storage(
    db: &Database,
    app_type: &AppType,
    provider: &mut Provider,
) -> Result<(), AppError> {
    let uses_common_config = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.common_config_enabled)
        .unwrap_or(false);

    if !uses_common_config {
        return Ok(());
    }

    let Some(snippet) = db.get_config_snippet(app_type.as_str())? else {
        return Ok(());
    };

    if snippet.trim().is_empty() {
        return Ok(());
    }

    match remove_common_config_from_settings(app_type, &provider.settings_config, &snippet) {
        Ok(settings) => provider.settings_config = settings,
        Err(err) => {
            log::warn!(
                "Failed to normalize common config before saving {} provider '{}': {err}",
                app_type.as_str(),
                provider.id
            );
        }
    }

    Ok(())
}

/// Write live configuration snapshot for a provider
pub(crate) fn write_live_snapshot(
    state: &AppState,
    app_type: &AppType,
    provider: &Provider,
) -> Result<(), AppError> {
    match app_type {
        AppType::Claude => {
            let path = get_claude_settings_path();
            let settings = sanitize_claude_settings_for_live(&provider.settings_config)?;
            write_json_file(&path, &settings)?;
        }
        AppType::Codex => {
            let obj = provider
                .settings_config
                .as_object()
                .ok_or_else(|| AppError::Config("Codex 供应商配置必须是 JSON 对象".to_string()))?;
            let auth = obj
                .get("auth")
                .ok_or_else(|| AppError::Config("Codex 供应商配置缺少 'auth' 字段".to_string()))?;
            let config_str = obj.get("config").and_then(|v| v.as_str());

            // Native (direct) Responses and Anthropic providers must suppress Codex's
            // freeform apply_patch custom tool via the generated catalog; chat/proxy
            // providers keep the default tool set. Uses the same Anthropic detection as
            // the proxy router (apiFormat meta/settings + TOML wire_api).
            let profile = crate::codex_config::resolve_codex_catalog_tool_profile(provider);

            let mut live_auth = auth.clone();
            if let Some(obj) = live_auth.as_object_mut() {
                obj.remove("OPENAI_API_KEY");
            }
            let base_url = futures::executor::block_on(state.secrets.retrieve(
                &crate::secrets::SecretTarget::provider_base_url(
                    AppType::Codex,
                    provider.id.clone(),
                ),
            ))
            .ok()
            .flatten();
            let sanitized_config = if let Some(config_text) = config_str {
                let sanitized =
                    super::codex_sanitizer::sanitize_codex_config_for_live_write_with_base_url(
                        config_text,
                        base_url.as_ref().map(|url| url.as_str()),
                    )?;
                Some(sanitized)
            } else {
                None
            };

            crate::codex_config::write_codex_provider_live_with_catalog(
                &provider.settings_config,
                provider.category.as_deref(),
                &live_auth,
                sanitized_config.as_deref().or(config_str),
                profile,
            )?;
        }
        AppType::Pi => {
            return Err(AppError::InvalidInput(
                "Pi providers use the Pi provider service".to_string(),
            ));
        }
    }
    Ok(())
}

/// Sync all providers to live configuration (for additive mode apps)
///
/// Writes all providers from the database to the live configuration file.
/// Used for Pi and other additive mode applications.
fn sync_all_providers_to_live(state: &AppState, app_type: &AppType) -> Result<(), AppError> {
    let providers = state.db.get_all_providers(app_type.as_str())?;
    let mut synced_count = 0usize;

    for provider in providers.values() {
        if provider
            .meta
            .as_ref()
            .and_then(|meta| meta.live_config_managed)
            == Some(false)
        {
            continue;
        }

        if let Err(e) = write_live_with_common_config_for_state(state, app_type, provider) {
            log::warn!(
                "Failed to sync {:?} provider '{}' to live: {e}",
                app_type,
                provider.id
            );
            continue;
        }
        synced_count += 1;
    }

    log::info!("Synced {synced_count} {app_type:?} providers to live config");
    Ok(())
}

pub(crate) fn sync_current_provider_for_app_to_live(
    state: &AppState,
    app_type: &AppType,
) -> Result<(), AppError> {
    if app_type.is_additive_mode() {
        sync_all_providers_to_live(state, app_type)?;
    } else {
        let current_id = match crate::settings::get_effective_current_provider(&state.db, app_type)?
        {
            Some(id) => id,
            None => return Ok(()),
        };

        let providers = state.db.get_all_providers(app_type.as_str())?;
        if let Some(provider) = providers.get(&current_id) {
            write_live_with_common_config_for_state(state, app_type, provider)?;
        }
    }

    // 本函数语义是"把这个应用同步到 live"，MCP 重投影也只针对该应用；
    // 全量 sync_all_enabled 会把无关应用的 live 损坏牵连进来。投影失败
    // 上抛（不降级）：这里没有已变更的 DB 状态需要保护，调用方重试即可。
    McpService::sync_enabled_for_app(state, app_type)?;

    Ok(())
}

fn sync_current_provider_for_app(state: &AppState, app_type: &AppType) -> Result<(), AppError> {
    let current_id = match crate::settings::get_effective_current_provider(&state.db, app_type)? {
        Some(id) => id,
        None => return Ok(()),
    };

    let providers = state.db.get_all_providers(app_type.as_str())?;
    let Some(provider) = providers.get(&current_id) else {
        return Ok(());
    };

    write_live_with_common_config_for_state(state, app_type, provider)
}

/// Sync current provider to live configuration
///
/// 使用有效的当前供应商 ID（验证过存在性）。
/// 优先从本地 settings 读取，验证后 fallback 到数据库的 is_current 字段。
/// 这确保了配置导入后无效 ID 会自动 fallback 到数据库。
///
/// For additive mode apps (Pi), all providers are synced instead of just the current one.
pub fn sync_current_to_live(state: &AppState) -> Result<(), AppError> {
    let mut failures = Vec::new();

    // Sync providers based on mode
    for app_type in AppType::all() {
        if matches!(app_type, AppType::Pi) {
            continue;
        }
        let result = if app_type.is_additive_mode() {
            // Additive mode: sync ALL providers
            sync_all_providers_to_live(state, &app_type)
        } else {
            // Switch mode: sync only current provider.
            sync_current_provider_for_app(state, &app_type)
        };

        if let Err(error) = result {
            log::warn!("同步 Provider 到 {app_type:?} 失败: {error}");
            failures.push(format!("provider/{}: {error}", app_type.as_str()));
        }
    }

    // MCP sync is already best-effort per application. Preserve its aggregate
    // error while continuing with Skills.
    if let Err(error) = McpService::sync_all_enabled(state) {
        failures.push(format!("mcp: {error}"));
    }

    // Skill sync
    for app_type in AppType::all() {
        if let Err(e) = crate::services::skill::SkillService::sync_to_app(&state.db, &app_type) {
            log::warn!("同步 Skill 到 {app_type:?} 失败: {e}");
            failures.push(format!("skill/{}: {e}", app_type.as_str()));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(AppError::Message(format!(
            "部分 live 配置同步失败: {}",
            failures.join("; ")
        )))
    }
}

/// Read current live settings for an app type (sanitized for frontend)
/// Phase 5 S1: Strips sensitive fields before returning to IPC
pub fn read_live_settings(app_type: AppType) -> Result<Value, AppError> {
    // IPC / 前端可见路径：脱去全部凭据材料（原则 3.1-3 / §5.2.2-4）。
    read_live_settings_with_auth(&app_type, true)
}

/// 内部回填专用：需要 live `auth.json` 里用户自己的 `OPENAI_API_KEY` / 登录态
/// 原样读回，才能把它回填进被切走的供应商行；这条路径不经过 IPC，
/// 脱敏只在 `read_live_settings`（前端命令）里做。
pub(crate) fn read_live_settings_for_backfill(app_type: AppType) -> Result<Value, AppError> {
    read_live_settings_with_auth(&app_type, false)
}

fn read_live_settings_with_auth(app_type: &AppType, strip_auth: bool) -> Result<Value, AppError> {
    match app_type {
        AppType::Codex => {
            let mut result = crate::codex_config::read_codex_live_settings()?;

            // 前端 IPC 路径按敏感规则逐个剥（原则 3.1-3：IPC 零密钥），不能只靠
            // 固定三键白名单，否则用户自写的 `*_API_KEY` 仍会过 IPC。
            // 内部回填路径（strip_auth=false）保留原文，交给提取器剥（§5.4-③）。
            if let Some(obj) = result.as_object_mut() {
                if strip_auth {
                    if let Some(auth) = obj.get_mut("auth").and_then(|v| v.as_object_mut()) {
                        let sensitive: Vec<String> = auth
                            .keys()
                            .filter(|key| {
                                crate::secrets::is_sensitive_config_key(key)
                                    || matches!(
                                        key.as_str(),
                                        "experimental_bearer_token"
                                            | "bearer_token"
                                            | "api_key"
                                            | "tokens"
                                            | "last_refresh"
                                    )
                            })
                            .cloned()
                            .collect();
                        for key in sensitive {
                            auth.remove(&key);
                        }
                    }
                }

                // Sanitize config text - 净化失败绝不退回原文（否则等于把 token 原样送出）
                if let Some(config_text) = obj.get("config").and_then(|v| v.as_str()) {
                    let sanitized = sanitize_codex_config_text(config_text)?;
                    obj.insert("config".to_string(), Value::String(sanitized));
                }
            }

            // `modelCatalog` is a cc-switch private field that lives only in
            // the DB SSOT plus the `cc-switch-model-catalog.json` projection
            // file — it is never inlined into `auth.json` or `config.toml`.
            // Reverse-parse the projection so the edit form for the active
            // Codex provider doesn't see an empty mapping table.
            if let Ok(Some(model_catalog)) =
                crate::codex_config::read_codex_model_catalog_simplified_from_live()
            {
                if let Some(obj) = result.as_object_mut() {
                    obj.insert("modelCatalog".to_string(), model_catalog);
                }
            }
            Ok(result)
        }
        AppType::Claude => {
            let path = get_claude_settings_path();
            if !path.exists() {
                return Err(AppError::localized(
                    "claude.live.missing",
                    "Claude Code 配置文件不存在",
                    "Claude settings file is missing",
                ));
            }
            let mut settings: serde_json::Value = read_json_file(&path)?;

            // IPC 路径按敏感规则剥（§5.2.2）：固定三键之外，用户自写的
            // `OPENROUTER_API_KEY` 等敏感 env 同样不得回传 WebView。
            if strip_auth {
                if let Some(obj) = settings.as_object_mut() {
                    if let Some(env) = obj.get_mut("env").and_then(|v| v.as_object_mut()) {
                        let sensitive: Vec<String> = env
                            .keys()
                            .filter(|key| {
                                super::live_sanitizer::is_claude_env_secret(key)
                                    || matches!(
                                        key.as_str(),
                                        "ANTHROPIC_API_KEY"
                                            | "ANTHROPIC_AUTH_TOKEN"
                                            | "ANTHROPIC_BASE_URL"
                                    )
                            })
                            .cloned()
                            .collect();
                        for key in sensitive {
                            env.remove(&key);
                        }
                    }
                }
            }

            Ok(settings)
        }
        AppType::Pi => Err(AppError::InvalidInput(
            "Pi providers are read from Pi's native models file".to_string(),
        )),
    }
}

/// Sanitize Codex config.toml text by removing bearer tokens
fn sanitize_codex_config_text(config_text: &str) -> Result<String, AppError> {
    use crate::services::provider::codex_sanitizer::sanitize_codex_config_for_live_write;

    // Reuse Phase 4 sanitizer which already removes experimental_bearer_token
    // and injects env_key references
    sanitize_codex_config_for_live_write(config_text)
}

/// Import default configuration from live files
///
/// Returns `Ok(true)` if a provider was actually imported,
/// `Ok(false)` if skipped (providers already exist for this app).
pub fn import_default_config(state: &AppState, app_type: AppType) -> Result<bool, AppError> {
    // Additive mode apps (Pi) should use their dedicated
    // import_xxx_providers_from_live functions, not this generic default config import
    if app_type.is_additive_mode() {
        return Ok(false);
    }

    // 允许 "只有官方 seed 预设" 的情况下继续导入 live：
    // - 启动编排顺序是先 import 后 seed，新用户启动时 providers 为空，导入照常
    // - 老用户已有非 seed provider，跳过导入（正确）
    // - 用户手动点 ProviderEmptyState 的导入按钮时，与官方 seed 共存而不被阻塞
    if state.db.has_non_official_seed_provider(app_type.as_str())? {
        return Ok(false);
    }

    let settings_config = match app_type {
        AppType::Codex => crate::codex_config::read_codex_live_settings()?,
        AppType::Claude => {
            let settings_path = get_claude_settings_path();
            if !settings_path.exists() {
                return Err(AppError::localized(
                    "claude.live.missing",
                    "Claude Code 配置文件不存在",
                    "Claude settings file is missing",
                ));
            }
            let mut v = read_json_file::<Value>(&settings_path)?;
            let _ = normalize_claude_models_in_value(&mut v);
            v
        }
        AppType::Pi => {
            unreachable!("additive mode apps are handled by early return")
        }
    };

    let mut provider = Provider::with_id("default".to_string());
    provider.name = "default".to_string();
    provider.settings_config = settings_config;
    provider.category = Some(
        if matches!(app_type, AppType::Codex) {
            let config_text = provider
                .settings_config
                .get("config")
                .and_then(Value::as_str);
            let has_provider_key = crate::codex_config::extract_codex_api_key(
                provider.settings_config.get("auth"),
                config_text,
            )
            .is_some();
            let has_login_material = provider
                .settings_config
                .get("auth")
                .is_some_and(crate::codex_config::codex_auth_has_login_material);

            if has_login_material && !has_provider_key {
                "official"
            } else {
                "custom"
            }
        } else {
            "custom"
        }
        .to_string(),
    );

    // §3.1-5：从 live 文件回填也是入口之一，落 DB 前必须剥凭据。
    super::strip_and_store_provider_secrets(state, &app_type, &mut provider)?;

    state.db.save_provider(app_type.as_str(), &provider)?;
    state
        .db
        .set_current_provider(app_type.as_str(), &provider.id)?;
    crate::settings::set_current_provider(&app_type, Some(provider.id.as_str()))?;

    Ok(true) // 真正导入了
}

/// Decide whether startup should auto-import the current live config as `default`.
///
/// This is intentionally stricter than the manual import path:
/// if the app already has any provider row at all (including official seeds),
/// startup must skip auto-import to avoid recreating `default` on each launch.
pub fn should_import_default_config_on_startup(
    state: &AppState,
    app_type: &AppType,
) -> Result<bool, AppError> {
    if app_type.is_additive_mode() {
        return Ok(false);
    }

    Ok(!state.db.has_any_provider_for_app(app_type.as_str())?)
}
