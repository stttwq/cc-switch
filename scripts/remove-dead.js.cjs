// one-off: delete dead items by name across rust sources (Phase-1a leftovers)
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

// cut a top-level item (with preceding attrs/comments) starting at declIndex
function cutItem(t, declIndex) {
  // walk back over attribute/comment lines and blank lines
  let start = declIndex;
  for (;;) {
    const lineStart = t.lastIndexOf('\n', start - 2) + 1;
    const line = t.slice(lineStart, start).trimEnd();
    if (/^\s*(#\[|\/\/|\/\/\/|$)/.test(t.slice(lineStart, start)) && lineStart > 0) {
      start = lineStart;
      // if line is code (not attr/comment/blank), stop
      if (!/^\s*(#\[|\/\/|\/\/\/)/.test(line) && line.trim() !== '') { start = lineStart + line.length + 1; break; }
    } else break;
  }
  // simpler: scan line-by-line backwards while attr/comment/blank
  return { start };
}

function removeItem(t, name, label) {
  const re = new RegExp(
    '(?:\\r?\\n|^)((?:[ \\t]*(?:#[^\\n]*|//[^\\n]*)?\\r?\\n)*)([ \\t]*(?:pub(?:\\(crate\\))?[ \\t]+|pub\\([^)]*\\)[ \\t]+)?(?:pub[ \\t]+)?(?:const|static|struct|enum|fn|async fn)[ \\t]+' +
    name.replace(/[-]/g, '\\-') +
    '\\b)'
  );
  const m = re.exec(t);
  if (!m) { console.error('NOT FOUND: ' + label + ' (' + name + ')'); return t; }
  const attrsStart = m.index + m[1].length;
  const declStart = attrsStart;
  const lineEnd = t.indexOf('\n', declStart);
  const brace = t.indexOf('{', declStart);
  const semi = t.indexOf(';', declStart);
  let end;
  if (brace >= 0 && (semi < 0 || brace < semi)) {
    end = findMatchingBrace(t, brace) + 1;
  } else {
    end = semi + 1;
  }
  if (end <= declStart) { console.error('BAD END: ' + label); return t; }
  // also swallow trailing blank line
  let endAll = end;
  if (t.slice(endAll, endAll + 2) === '\r\n') endAll += 2;
  else if (t[endAll] === '\n') endAll += 1;
  console.log('cut', label);
  return t.slice(0, declStart) + t.slice(endAll);
}

function removeImplBlock(t, name) {
  const re = new RegExp('(?:\\r?\\n|^)[ \\t]*impl[ \\t]+' + name + '[ \\t]*\\{');
  const m = re.exec(t);
  if (!m) return t;
  const brace = t.indexOf('{', m.index);
  const end = findMatchingBrace(t, brace) + 1;
  let endAll = end;
  if (t.slice(endAll, endAll + 2) === '\r\n') endAll += 2;
  else if (t[endAll] === '\n') endAll += 1;
  console.log('cut impl', name);
  return t.slice(0, m.index) + '\n' + t.slice(endAll);
}

function processFile(file, names, impls) {
  let t = fs.readFileSync(file, 'utf8');
  for (const n of impls || []) t = removeImplBlock(t, n);
  for (const n of names) t = removeItem(t, n, n);
  fs.writeFileSync(file, t);
  console.log('done', file);
}

processFile('src-tauri/src/codex_config.rs',
  [
    'CC_SWITCH_CODEX_OFFICIAL_PROXY_PROVIDER_ID', 'CODEX_PROXY_AUTH_PLACEHOLDER',
    'CodexManagedLiveRefresh', 'CodexLiveFileState', 'CodexModelCatalogFileSnapshot', 'CodexLiveStateSnapshot',
    'codex_managed_oauth_auth_value', 'migrate_legacy_codex_managed_oauth_live_auth_marker',
    'prepare_codex_live_auth_for_managed_account_removal', 'codex_auth_matches_recorded_managed_oauth',
    'codex_live_auth_matches_managed_request', 'clear_codex_managed_oauth_live_auth_marker_for_account',
    'clear_codex_live_auth_for_managed_account', 'ensure_codex_live_auth_unchanged_for_managed_account',
    'clear_codex_live_auth_for_managed_account_if_unchanged', 'codex_live_auth_is_managed_chatgpt_login',
    'read_codex_live_auth_refresh_for_account', 'read_codex_live_auth_refresh_for_managed_account',
    'sync_codex_managed_oauth_live_auth_after_refresh', 'CodexResolvedAuthMode', 'codex_auth_resolved_mode',
    'codex_auth_has_openai_account_material', 'CodexAuthStoreMode', 'codex_config_auth_store_mode',
    'codex_auth_has_credential_login_material', 'codex_live_auth_is_stale_third_party_residue',
    'clear_stale_codex_live_auth_after_official_switch', 'read_codex_model_catalog_text',
    'prepare_codex_live_config_text_with_optional_catalog', 'neutralize_codex_official_auth_fallback_for_proxy_oauth',
    'remove_codex_proxy_placeholders_from_providers', 'apply_codex_official_proxy_route',
    'codex_config_has_official_proxy_route', 'remove_codex_official_proxy_route',
    'apply_codex_unified_session_bucket_to_settings', 'update_codex_toml_field', 'remove_codex_toml_base_url_if',
  ]);

processFile('src-tauri/src/grok_config.rs',
  ['extract_inline_api_key', 'update_selected_model_string', 'apply_proxy_takeover', 'update_api_key', 'has_proxy_placeholder', 'base_url_matches']);

processFile('src-tauri/src/opencode_config.rs',
  ['get_opencode_db_path', 'get_opencode_data_dir']);

processFile('src-tauri/src/provider.rs',
  ['UsageData', 'UsageResult']);

processFile('src-tauri/src/services/provider/mod.rs',
  ['normalize_usage_script_credential_overrides', 'should_clear_usage_api_key_override', 'should_clear_usage_base_url_override', 'normalize_usage_base_url_for_compare']);

processFile('src-tauri/src/tray.rs',
  ['AUTO_SUFFIX']);

processFile('src-tauri/src/lib.rs',
  ['redact_url_origin_for_log']);

console.log('all done');
