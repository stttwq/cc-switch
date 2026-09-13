// one-off: reinsert generic get_or_load fn from HEAD (lines 2237-2247, 1-based)
const { execSync } = require('child_process');
const fs = require('fs');
const head = execSync('git show "HEAD:src-tauri/src/codex_config.rs"', { encoding: 'utf8', cwd: 'src-tauri' });
const lines = head.split('\n');
const startIdx = lines.findIndex((l) => l.includes('fn get_or_load_codex_model_catalog_template'));
if (startIdx < 0) { console.error('not found'); process.exit(1); }
// walk back over doc comment lines
let s = startIdx;
while (s > 0 && /^\s*(\/\/|\/\/\/|#\[)/.test(lines[s - 1])) s--;
// find end: closing brace at column 0
let e = startIdx;
while (e < lines.length && lines[e] !== '}') e++;
const item = lines.slice(s, e + 1).join('\n') + '\n';
console.log('--- extracted ---');
console.log(item);

const p = 'src-tauri/src/codex_config.rs';
let cur = fs.readFileSync(p, 'utf8');
if (cur.includes('fn get_or_load_codex_model_catalog_template')) { console.log('already present'); process.exit(0); }
const anchor = cur.indexOf('#[cfg(not(test))]\nfn load_codex_model_catalog_template');
if (anchor < 0) { console.error('anchor not found'); process.exit(1); }
cur = cur.slice(0, anchor) + item + '\n' + cur.slice(anchor);
fs.writeFileSync(p, cur);
console.log('inserted');
