// one-off: reapply all Phase-1a prod edits to services/provider/mod.rs (from HEAD state)
const fs = require('fs');
const p = 'src-tauri/src/services/provider/mod.rs';
let t = fs.readFileSync(p, 'utf8');
let failed = false;

function must(re, rep, label) {
  if (!re.test(t)) { console.error('NOT MATCHED: ' + label); failed = true; return; }
  t = t.replace(re, rep);
  console.log('ok', label);
}

// 1. mod decls + imports
must(/^mod usage;\r?\n/m, '', 'mod usage');
must(/use crate::database::\{validate_cost_multiplier, validate_pricing_source\};\r?\n/, '', 'db imports');
must(/use crate::provider::\{Provider, UsageResult\};/, 'use crate::provider::Provider;', 'provider import');
must(/use crate::services::mcp::McpService;\r?\n/, 'use crate::services::mcp::McpService;\n', 'mcp import keep');
must(/use usage::validate_usage_script;\r?\n/, '', 'usage import');

// 2. re-export block
must(/pub\(crate\) use live::\{[\s\S]*?\};/, `pub(crate) use live::sanitize_claude_settings_for_live;
pub(crate) use live::{
    build_effective_settings_with_common_config, normalize_provider_common_config_for_storage,
    provider_exists_in_live_config, strip_common_config_from_live_settings,
    sync_current_provider_for_app_to_live, write_live_with_common_config_for_state,
};`, 're-exports');

// 3. remove takeover predicate fn
must(/\/\/\/ Codex official providers are safe to select during takeover[\s\S]*?\n\}\r?\n\r?\n/, '', 'takeover predicate fn');

// 4. reapply_current_codex_official_live: replace takeover-aware block
must(/    \/\/ 代理接管期间 live 归代理所有[\s\S]*?    let outcome =\r?\n        live::sync_live_for_provider_respecting_takeover\(state, &AppType::Codex, provider\)\?;\r?\n    if outcome == LiveSyncOutcome::BackupOnly \{\r?\n        return Ok\(true\);\r?\n    \}\r?\n/, '', 'reapply takeover block');
must(/    if let Err\(err\) = McpService::sync_enabled_for_app\(state, &AppType::Codex\) \{\r?\n        log::warn!\("统一会话开关重写 live 后重投影 Codex MCP 失败（将在下次同步时自愈）: \{err\}"\);\r?\n    \}\r?\n    Ok\(true\)/,
`    write_live_with_common_config_for_state(state, &AppType::Codex, provider)?;
    if let Err(err) = McpService::sync_enabled_for_app(state, &AppType::Codex) {
        log::warn!("统一会话开关重写 live 后重投影 Codex MCP 失败（将在下次同步时自愈）: {err}");
    }
    Ok(true)`, 'reapply write');

// 5. add(): remove managed codex path
must(/        Self::normalize_usage_script_credential_overrides\(&app_type, &mut provider\);\r?\n/, '', 'add usage normalize');
must(/        let is_managed_codex_add = matches!\(app_type, AppType::Codex\)[\s\S]*?        \/\/ Save to database/, '        // Save to database', 'add managed codex path');
must(/            \/\/ No current provider, set as current and sync\. Managed Codex adds\r?\n            \/\/ use the transactional path above because token resolution can fail\.\r?\n/, '            // No current provider, set as current and sync.\n', 'add comment');

// 6. update(): remove lock + managed codex path
must(/        \/\/ Serialize the read\/decide\/commit window for every Codex update\.[\s\S]*?        \}\r?\n        let existing_provider = state/, '        let existing_provider = state', 'update switch lock');
must(/        Self::normalize_usage_script_credential_overrides\(&app_type, &mut provider\);\r?\n/, '', 'update usage normalize');
must(/        let existing_managed_codex_account_id = existing_provider[\s\S]*?        drop\(codex_update_switch_guard\);\r?\n\r?\n        \/\/ Save to database/, '        // Save to database', 'update managed codex path');
must(/        if is_current \{\r?\n            let outcome =\r?\n                live::sync_live_for_provider_respecting_takeover\(state, &app_type, &provider\)\?;\r?\n            if outcome == LiveSyncOutcome::WroteLive \{\r?\n                \/\/ MCP is stored in the database and projected after a successful\r?\n                \/\/ live write\. Keep the failure best-effort so the provider save\r?\n                \/\/ itself is not reported as failed when MCP projection can retry\.\r?\n                if let Err\(err\) = McpService::sync_enabled_for_app\(state, &app_type\) \{\r?\n                    log::warn!\(\r?\n                        "保存供应商后重投影 \{app_type:\?\} MCP 失败（将在下次同步时自愈）: \{err\}"\r?\n                    \);\r?\n                \}\r?\n            \}\r?\n        \}/,
`        if is_current {
            write_live_with_common_config_for_state(state, &app_type, &provider)?;
            if let Err(err) = McpService::sync_enabled_for_app(state, &app_type) {
                log::warn!("保存供应商后重投影 {app_type:?} MCP 失败（将在下次同步时自愈）: {err}");
            }
        }`, 'update current path');

// 7. delete update_pi_usage_script re-export
must(/    pub\(crate\) fn update_pi_usage_script\([\s\S]*?\n    \}\r?\n\r?\n/, '', 'update_pi_usage_script');

fs.writeFileSync(p, t);
console.log(failed ? 'FAILED (partially written)' : 'stage 1 done');
