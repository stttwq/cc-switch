// one-off: remove two backup tests that validate the removed usage persistence stack
const fs = require('fs');
const p = 'src-tauri/src/database/backup.rs';
let t = fs.readFileSync(p, 'utf8');

function cutItem(t, name) {
  const re = new RegExp('(?:\\r?\\n|^)[ \\t]*(?:async fn|fn)[ \\t]+' + name + '\\b');
  const m = re.exec(t);
  if (!m) { console.error('NOT FOUND:', name); process.exit(1); }
  const brace = t.indexOf('{', m.index);
  const end = findMatchingBrace(t, brace) + 1;
  let endAll = end;
  if (t.slice(endAll, endAll + 2) === '\r\n') endAll += 2;
  else if (t[endAll] === '\n') endAll += 1;
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

t = cutItem(t, 'full_sql_backup_still_round_trips_session_cursors');
t = cutItem(t, 'periodic_maintenance_runs_even_when_auto_backup_disabled');
fs.writeFileSync(p, t);
console.log('done');
