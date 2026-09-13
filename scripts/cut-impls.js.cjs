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

// cut impl blocks (with preceding attrs/docs) whose name is listed
function cutImpls(t, names) {
  for (;;) {
    const re = /(?:\r?\n|^)((?:[ \t]*(?:#[^\n]*|\/\/[^\n]*)?\r?\n)*)[ \t]*impl[ \t]+([A-Za-z0-9_]+)/g;
    let cut = false;
    let m;
    while ((m = re.exec(t))) {
      if (!names.includes(m[2])) continue;
      const declStart = m.index + m[1].length;
      const brace = t.indexOf('{', declStart);
      const end = findMatchingBrace(t, brace) + 1;
      let endAll = end;
      if (t.slice(endAll, endAll + 2) === '\r\n') endAll += 2;
      else if (t[endAll] === '\n') endAll += 1;
      // extend attrs upward: walk back over comment/attr lines
      let start = m.index;
      for (;;) {
        const prev = t.lastIndexOf('\n', start - 2);
        if (prev < 0) break;
        const line = t.slice(prev + 1, start);
        if (/^\s*(#\[|\/\/)/.test(line) || line.trim() === '') start = prev + 1;
        else break;
      }
      console.log('cut impl', m[2]);
      t = t.slice(0, start) + t.slice(endAll);
      cut = true;
      break;
    }
    if (!cut) return t;
  }
}

const p = 'src-tauri/src/codex_config.rs';
let t = fs.readFileSync(p, 'utf8');
t = cutImpls(t, ['CodexLiveFileState', 'CodexModelCatalogFileSnapshot', 'CodexLiveStateSnapshot']);
fs.writeFileSync(p, t);
console.log('codex_config impls cleaned');
