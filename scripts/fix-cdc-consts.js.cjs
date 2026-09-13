const fs = require('fs');
const p = 'src-tauri/src/claude_desktop_config.rs';
let t = fs.readFileSync(p, 'utf8');
if (!t.includes('const GATEWAY_TOKEN_SETTING_KEY: &str = "claude_desktop_gateway_token";const DEFAULT_CREATED_AT')) {
  console.log('already fixed');
  process.exit(0);
}
t = t.replace(
  'const GATEWAY_TOKEN_SETTING_KEY: &str = "claude_desktop_gateway_token";const DEFAULT_CREATED_AT: &str = "2024-01-01T00:00:00Z";const MIMO_TOOL_CALL_THINKING_PLACEHOLDER: &str = "tool call";',
  'const GATEWAY_TOKEN_SETTING_KEY: &str = "claude_desktop_gateway_token";'
);
fs.writeFileSync(p, t);
console.log('fixed');
