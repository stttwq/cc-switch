// one-off: remove usage-script plumbing from provider modules
const fs = require('fs');

function edit(path, fns) {
  let t = fs.readFileSync(path, 'utf8');
  for (const [re, rep, label] of fns) {
    if (!re.test(t)) { console.error('NOT MATCHED in ' + path + ': ' + label); process.exit(1); }
    t = t.replace(re, rep);
  }
  fs.writeFileSync(path, t);
  console.log('ok', path);
}

edit('src-tauri/src/services/provider/pi.rs', [
  [/pub\(super\) fn update_usage_script\([\s\S]*?\n\}\r?\n\r?\n/, '', 'update_usage_script fn'],
  [/use crate::provider::\{Provider, ProviderMeta, UsageScript\};/, 'use crate::provider::{Provider, ProviderMeta};', 'pi import'],
]);

edit('src-tauri/src/services/provider/mod.rs', [
  [/    pub\(crate\) fn update_pi_usage_script\([\s\S]*?\n    \}\r?\n\r?\n/, '', 'update_pi_usage_script fn'],
]);
