// one-off: pi.rs Phase-1a edits (usage script plumbing removal)
const fs = require('fs');
const p = 'src-tauri/src/services/provider/pi.rs';
let t = fs.readFileSync(p, 'utf8');

function must(re, rep, label) {
  if (!re.test(t)) { console.error('NOT MATCHED: ' + label); process.exit(1); }
  t = t.replace(re, rep);
  console.log('ok', label);
}

must(/    ProviderService::normalize_usage_script_credential_overrides\(&app_type, &mut provider\);\r?\n/g, '', 'usage override calls');

// delete update_usage_script fn (with preceding blank line)
must(/\r?\npub\(super\) fn update_usage_script\([\s\S]*?\n\}\r?\n/, '', 'update_usage_script fn');

// import
must(/use crate::provider::\{Provider, ProviderMeta, UsageScript\};/, 'use crate::provider::{Provider, ProviderMeta};', 'pi import');

fs.writeFileSync(p, t);
console.log('pi.rs stage done');
