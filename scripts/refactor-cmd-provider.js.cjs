// one-off: trim commands/provider.rs
const fs = require('fs');
const p = 'src-tauri/src/commands/provider.rs';
let t = fs.readFileSync(p, 'utf8');

function delRange(startMarker, endMarker) {
  const s = t.indexOf(startMarker);
  if (s < 0) { console.error('start not found: ' + startMarker); process.exit(1); }
  const e = t.indexOf(endMarker, s);
  if (e < 0) { console.error('end not found: ' + endMarker); process.exit(1); }
  t = t.slice(0, s) + t.slice(e);
}

// 1. queryProviderUsage .. testUsageScript block (ends right before read_live_provider_settings)
delRange('#[allow(non_snake_case)]\n#[tauri::command]\npub async fn queryProviderUsage', '#[tauri::command]\npub fn read_live_provider_settings');

// 2. test_api_endpoints command
delRange('#[tauri::command]\npub async fn test_api_endpoints', '#[tauri::command]\npub fn get_custom_endpoints');

// 3. imports
t = t.replace('use crate::commands::copilot::CopilotAuthState;\nuse crate::commands::xai_oauth::XaiOAuthState;\n', '');
t = t.replace('use crate::services::{\n    EndpointLatency, ProviderService, ProviderSortUpdate, SpeedtestService, SwitchResult,\n};', 'use crate::services::{ProviderService, ProviderSortUpdate, SwitchResult};');

// 4. template constants only used by usage queries
t = t.replace('// 常量定义\nconst TEMPLATE_TYPE_GITHUB_COPILOT: &str = "github_copilot";\nconst TEMPLATE_TYPE_TOKEN_PLAN: &str = "token_plan";\nconst TEMPLATE_TYPE_BALANCE: &str = "balance";\nconst TEMPLATE_TYPE_OFFICIAL_SUBSCRIPTION: &str = "official_subscription";\nconst COPILOT_UNIT_PREMIUM: &str = "requests";\n', '');

fs.writeFileSync(p, t);
console.log('provider.rs trimmed');
