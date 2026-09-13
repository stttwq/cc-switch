// one-off: mod.rs stage 2 — switch/switch_normal/managed-codex helpers/usage re-exports
const fs = require('fs');
const p = 'src-tauri/src/services/provider/mod.rs';
let t = fs.readFileSync(p, 'utf8');

function must(re, rep, label) {
  if (!re.test(t)) { console.error('NOT MATCHED: ' + label); process.exit(1); }
  t = t.replace(re, rep);
  console.log('ok', label);
}

// duplicate import
must(/pub\(crate\) use live::sanitize_claude_settings_for_live;\r?\npub\(crate\) use live::sanitize_claude_settings_for_live;\r?\n/,
     'pub(crate) use live::sanitize_claude_settings_for_live;\n', 'dedupe import');

// switch_normal codex official cleanup condition (do first, pristine text)
must(/ && target_managed_codex_account_id\.is_none\(\)(?=\r?\n)/, '', 'target_managed condition');

// reapply codex official check
must(/        && !crate::proxy::providers::is_codex_official_provider\(provider\)/,
     '        && !crate::codex_config::is_codex_official_provider(provider)', 'reapply check');
must(/\|\| crate::proxy::providers::is_codex_official_provider\(provider\)/g,
     '|| crate::codex_config::is_codex_official_provider(provider)', 'other official checks');

// delete managed-codex helper block: fn managed_codex_oauth_account_id .. clear_outgoing_managed_codex_live_auth end
const a = t.indexOf('    fn managed_codex_oauth_account_id(provider: &Provider) -> Option<String> {');
const b = t.indexOf('    fn normalize_provider_if_claude(app_type: &AppType, provider: &mut Provider) {');
if (a < 0 || b < 0 || b <= a) { console.error('helper block bounds not found', a, b); process.exit(1); }
t = t.slice(0, a) + t.slice(b);
console.log('ok managed-codex helpers block');

// switch(): replace takeover/hot-switch logic with lock + switch_normal
const sw = t.indexOf('    pub fn switch(state: &AppState, app_type: AppType, id: &str) -> Result<SwitchResult, AppError> {');
const sn = t.indexOf('    /// Normal switch flow (non-proxy mode)', sw);
if (sw < 0 || sn < 0) { console.error('switch bounds not found', sw, sn); process.exit(1); }
t = t.slice(0, sw) + `    pub fn switch(state: &AppState, app_type: AppType, id: &str) -> Result<SwitchResult, AppError> {
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

` + t.slice(sn);
console.log('ok switch rewrite');

// switch_normal: drop managed codex transaction
must(/        let current_managed_codex_account_id = current_id[\s\S]*?        let mut backfill_completed = false;/,
     '        let mut backfill_completed = false;', 'switch_normal current managed');
must(/        let target_managed_codex_account_id = Self::managed_codex_oauth_account_id\(provider\);[\s\S]*?        \} else \{\r?\n            \/\/ Codex: validate the live projection before committing current —[\s\S]*?            \}\r?\n        \}/,
`        {
            // Codex: validate the live projection before committing current —
            // the write-layer safety gates can refuse the switch, and a
            // refusal after current moved would let the next switch backfill
            // the old live config into the new provider's DB row.
            if matches!(app_type, AppType::Codex) {
                live::preflight_codex_live_write_for_state(state, provider)?;
            }

            // Additive mode apps skip setting is_current (no such concept).
            if !app_type.is_additive_mode() {
                crate::settings::set_current_provider(&app_type, Some(id))?;
                state.db.set_current_provider(app_type.as_str(), id)?;
            }

            // Sync to live (write_gemini_live handles security flag internally for Gemini).
            write_live_with_common_config_for_state(state, &app_type, provider)?;
        }`, 'switch_normal managed transaction');

fs.writeFileSync(p, t);
console.log('stage 2 done');
