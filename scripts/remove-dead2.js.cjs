// one-off: dead-code cleanup round 2 (attrs-safe item cutter)
const fs = require('fs');

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
      i++; continue;
    }
    if (c === '/' && t[i + 1] === '/') { const j = t.indexOf('\n', i); i = j < 0 ? t.length : j; continue; }
    if (c === '/' && t[i + 1] === '*') { const j = t.indexOf('*/', i); i = j < 0 ? t.length : j + 2; continue; }
    if (c === '{') depth++;
    else if (c === '}') { depth--; if (depth === 0) return i; }
    i++;
  }
  return -1;
}

function cutItem(t, name) {
  const re = new RegExp(
    '(?:\\r?\\n|^)[ \\t]*(?:pub(?:\\(crate\\))?[ \\t]+|pub\\([^)]*\\)[ \\t]+)?(?:pub[ \\t]+)?(?:const|static|struct|enum|fn|async fn)[ \\t]+' + name + '\\b'
  );
  const m = re.exec(t);
  if (!m) { console.error('NOT FOUND:', name); return t; }
  const brace = t.indexOf('{', m.index);
  const semi = t.indexOf(';', m.index);
  let end;
  if (brace >= 0 && (semi < 0 || brace < semi)) end = findMatchingBrace(t, brace) + 1;
  else end = semi + 1;
  let endAll = end;
  if (t.slice(endAll, endAll + 2) === '\r\n') endAll += 2;
  else if (t[endAll] === '\n') endAll += 1;
  // extend start upward over doc comments and attributes
  let start = m.index;
  for (;;) {
    const prev = t.lastIndexOf('\n', start - 2);
    if (prev < 0) break;
    const line = t.slice(prev + 1, start);
    if (/^\s*(#\[|\/\/)/.test(line) || line.trim() === '') start = prev + 1;
    else break;
  }
  console.log('cut', name);
  return t.slice(0, start) + t.slice(endAll);
}

function processFile(file, names) {
  let t = fs.readFileSync(file, 'utf8');
  for (const n of names) t = cutItem(t, n);
  fs.writeFileSync(file, t);
  console.log('done', file);
}

processFile('src-tauri/src/claude_desktop_config.rs', [
  'CLAUDE_DESKTOP_PROXY_PREFIX', 'DEFAULT_CREATED_AT', 'MIMO_REDACTED_THINKING_PLACEHOLDER',
  'MIMO_TOOL_CALL_THINKING_PLACEHOLDER', 'LEGACY_OPUS_ROUTE_ID', 'model_list_response',
  'map_proxy_request_model', 'strip_one_m_suffix_for_route_lookup', 'legacy_raw_route_upstream_model',
  'is_compatible_opus_route_alias', 'claude_role_keyword', 'should_normalize_mimo_anthropic_thinking_history',
  'provider_uses_anthropic_messages_format', 'provider_has_mimo_endpoint', 'is_mimo_identifier',
  'normalize_mimo_anthropic_thinking_history', 'proxy_origin_from_parts',
]);

processFile('src-tauri/src/services/sql_helpers.rs', [
  'CACHE_INCLUSIVE_APP_TYPES', 'is_cache_inclusive_app', 'INPUT_TOKEN_SEMANTICS_LEGACY',
  'INPUT_TOKEN_SEMANTICS_TOTAL', 'INPUT_TOKEN_SEMANTICS_FRESH', 'fresh_input_sql',
]);

processFile('src-tauri/src/session_manager/providers/pi.rs', [
  'session_files', 'session_files_from_resolution',
]);

processFile('src-tauri/src/codex_config.rs', [
  'codex_managed_oauth_live_auth_marker_exists', 'test_codex_id_token',
  'get_or_load_codex_model_catalog_template',
]);

console.log('all done');
