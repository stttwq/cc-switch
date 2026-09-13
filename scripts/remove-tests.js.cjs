// one-off: remove #[test] fns whose bodies reference removed APIs (rust-aware brace scan)
// usage: node remove-tests.js.cjs <file> [file...]
const fs = require('fs');

const PATTERNS = [
  /proxy_service/, /codex_oauth_manager/, /save_live_backup/, /get_live_backup/,
  /get_proxy_config/, /update_proxy_config/, /get_proxy_flags/, /set_proxy_flags/,
  /managed_codex/, /codex_oauth/, /ProxyConfig/, /LiveBackup/, /UsageScript/,
  /usage_script/, /neutralize_codex/, /apply_codex_official_auth/, /update_usage_script/,
  /hot_switch/, /switch_proxy_provider/, /takeover/, /proxy_live_backup/,
  /remove_codex_oauth_account/, /logout_codex_oauth/, /speedtest/, /CopilotAuthState/,
  /XaiOAuthState/, /usage_cache/, /get_pricing_model_source/, /ProxyService/,
];

function findMatchingBrace(t, start) {
  let depth = 0, i = start, inStr = null;
  while (i < t.length) {
    const c = t[i];
    if (inStr === '"') {
      if (c === '\\') { i += 2; continue; }
      if (c === '"') inStr = null;
      i++; continue;
    }
    if (c === '"') { inStr = '"'; i++; continue; }
    if (c === "'") {
      if (t[i + 1] === '\\' || (t[i + 1] && t[i + 2] === "'")) { i += t[i + 1] === '\\' ? 4 : 3; continue; }
      i++; continue; // lifetime
    }
    if (c === '/' && t[i + 1] === '/') { const j = t.indexOf('\n', i); i = j < 0 ? t.length : j; continue; }
    if (c === '/' && t[i + 1] === '*') { const j = t.indexOf('*/', i); i = j < 0 ? t.length : j + 2; continue; }
    if (c === '{') depth++;
    else if (c === '}') { depth--; if (depth === 0) return i; }
    i++;
  }
  return -1;
}

for (const file of process.argv.slice(2)) {
  const t = fs.readFileSync(file, 'utf8');
  let removed = 0;
  let out = '';
  let copyPos = 0;   // everything before copyPos is already handled (kept or removed)
  let scanPos = 0;   // regex scan cursor
  for (;;) {
    const re = /(?:\r?\n(?:[ \t]*\/\/[^\n]*\r?\n)*[ \t]*(?:#\[[^\]]*\][ \t]*\r?\n)+)([ \t]*(?:pub(?:\(crate\))?[ \t]+)?(?:async[ \t]+)?fn[ \t]+[A-Za-z0-9_]+[ \t]*(?:<[^>]*>)?\()/g;
    re.lastIndex = scanPos;
    const m = re.exec(t);
    if (!m) break;
    const open = t.indexOf('{', m.index + m[0].length - 1);
    const close = findMatchingBrace(t, open);
    if (close < 0) { console.error('unbalanced in ' + file); process.exit(1); }
    let endAll = close + 1;
    if (t.slice(endAll, endAll + 2) === '\r\n') endAll += 2;
    else if (t[endAll] === '\n') endAll += 1;
    const item = t.slice(m.index, endAll);
    if (PATTERNS.some((p) => p.test(item))) {
      out += t.slice(copyPos, m.index);
      copyPos = endAll;
      scanPos = endAll;
      removed++;
    } else {
      scanPos = m.index + 1;
    }
  }
  if (removed > 0) {
    fs.writeFileSync(file, out + t.slice(copyPos));
  }
  console.log(file, 'removed', removed);
}
