const fs = require('fs');
const p = 'src-tauri/src/services/provider/mod.rs';
let t = fs.readFileSync(p, 'utf8');
const old = `        let outcome = live::sync_live_for_provider_respecting_takeover(state, &app_type, provider)?;
        if outcome == LiveSyncOutcome::BackupOnly {
            return Ok(());
        }

        McpService::sync_enabled_for_app(state, &app_type)`;
const rep = `        write_live_with_common_config_for_state(state, &app_type, provider)?;

        McpService::sync_enabled_for_app(state, &app_type)`;
if (!t.includes(old)) { console.error('block not found'); process.exit(1); }
t = t.replace(old, rep);
fs.writeFileSync(p, t);
console.log('sync_current fixed');
