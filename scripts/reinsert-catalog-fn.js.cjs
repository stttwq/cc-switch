// one-off: reinsert get_or_load_codex_model_catalog_template into codex_config.rs
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

const head = fs.readFileSync('src-tauri/cc_head_tmp.txt', 'utf8');
const cur = fs.readFileSync('src-tauri/src/codex_config.rs', 'utf8');

const name = 'get_or_load_codex_model_catalog_template';
const re = new RegExp('(?:\\r?\\n|^)[ \\t]*fn[ \\t]+' + name + '\\b');
const m = re.exec(head);
if (!m) { console.error('not in head'); process.exit(1); }
const brace = head.indexOf('{', m.index);
const end = findMatchingBrace(head, brace) + 1;
const item = head.slice(m.index, end) + '\n';

// insert into current before `fn load_codex_model_catalog_template`
const anchor = cur.indexOf('fn load_codex_model_catalog_template');
if (anchor < 0) { console.error('anchor not found'); process.exit(1); }
const out = cur.slice(0, anchor) + item + '\n' + cur.slice(anchor);
fs.writeFileSync('src/codex_config.rs', out);
console.log('reinserted', name);
