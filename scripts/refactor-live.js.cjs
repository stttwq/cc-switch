// one-off: reapply Phase-1a prod edits to services/provider/live.rs (from HEAD)
const fs = require('fs');
const p = 'src-tauri/src/services/provider/live.rs';
let t = fs.readFileSync(p, 'utf8');

function must(re, rep, label) {
  if (!re.test(t)) { console.error('NOT MATCHED: ' + label); process.exit(1); }
  t = t.replace(re, rep);
  console.log('ok', label);
}

// 1. imports
must(/use crate::proxy::providers::codex_oauth_auth::CodexOAuthManager;\r?\n/, '', 'oauth import');

// 2. helper redirections
must(/\|\| !crate::proxy::providers::is_codex_official_provider\(provider\)/, '|| !crate::codex_config::is_codex_official_provider(provider)', 'official check 1');
must(/\|\| crate::proxy::providers::is_codex_official_provider\(provider\)/, '|| crate::codex_config::is_codex_official_provider(provider)', 'official check 2');
must(/crate::proxy::providers::resolve_codex_catalog_tool_profile\(provider\)/, 'crate::codex_config::resolve_codex_catalog_tool_profile(provider)', 'catalog profile');

// 3. rewrite the codex-oauth live plumbing block: from write_live_with_common_config_for_state
//    through end of codex_managed_oauth_live_auth
const blockStart = t.indexOf('pub(crate) fn write_live_with_common_config_for_state(');
if (blockStart < 0) { console.error('blockStart not found'); process.exit(1); }
const blockEndMarker = 'pub(crate) fn strip_common_config_from_live_settings(';
const blockEnd = t.indexOf(blockEndMarker, blockStart);
if (blockEnd < 0) { console.error('blockEnd not found'); process.exit(1); }
t = t.slice(0, blockStart) + `pub(crate) fn write_live_with_common_config_for_state(
    state: &AppState,
    app_type: &AppType,
    provider: &Provider,
) -> Result<(), AppError> {
    write_live_with_common_config(state.db.as_ref(), app_type, provider)
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
    crate::codex_config::preflight_codex_live_write(effective.category.as_deref(), auth, config_str)
}

pub(crate) fn write_live_with_common_config(
    db: &Database,
    app_type: &AppType,
    provider: &Provider,
) -> Result<(), AppError> {
    let effective_provider = build_effective_provider_for_live(db, app_type, provider)?;

    if matches!(app_type, AppType::ClaudeDesktop) {
        crate::claude_desktop_config::apply_provider(db, &effective_provider)?;
        log::info!(
            "Claude Desktop 3P profile '{}' written for provider '{}'",
            crate::claude_desktop_config::PROFILE_ID,
            effective_provider.id
        );
        return Ok(());
    }

    write_live_snapshot(app_type, &effective_provider)
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

` + t.slice(blockEnd);
console.log('ok live plumbing rewrite');

// 4. delete takeover ownership family: LiveSyncOutcome enum .. before sync_current_provider_for_app_respecting_takeover
const s1 = t.indexOf('#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub(crate) enum LiveSyncOutcome');
if (s1 < 0) { console.error('LiveSyncOutcome not found'); process.exit(1); }
const s2 = t.indexOf('fn sync_current_provider_for_app_respecting_takeover(', s1);
if (s2 < 0) { console.error('sync_current_provider_for_app_respecting_takeover not found'); process.exit(1); }
t = t.slice(0, s1) + t.slice(s2);
console.log('ok takeover family removal');

// 5. rename takeover-aware current sync
must(/fn sync_current_provider_for_app_respecting_takeover\(/, 'fn sync_current_provider_for_app(', 'rename sync_current');
must(/    sync_live_for_provider_respecting_takeover\(state, app_type, provider\)\.map\(\|_\| \(\)\)/,
     '    write_live_with_common_config_for_state(state, app_type, provider)', 'sync body');
must(/            \/\/ Switch mode: sync only current provider\. During proxy takeover,\r?\n            \/\/ update the restore backup instead of rewriting the taken-over\r?\n            \/\/ live file\.\r?\n            sync_current_provider_for_app_respecting_takeover\(state, &app_type\)/,
     '            // Switch mode: sync only current provider.\n            sync_current_provider_for_app(state, &app_type)', 'sync call site');

// 6. import_default_config: drop takeover guard
must(/    \/\/ 拒绝把"被代理接管的 Live"导入为供应商[\s\S]*?    \}\r?\n\r?\n    let settings_config = match app_type \{/, '    let settings_config = match app_type {', 'import takeover guard');

fs.writeFileSync(p, t);
console.log('live.rs stage done');
