const fs = require('fs');
const p = 'src-tauri/src/codex_config.rs';
let t = fs.readFileSync(p, 'utf8');
// find orphaned #[derive] lines (a derive immediately followed by `pub fn`/`impl`/`fn`)
const re = /\n#\[derive\([^\n]*\)\]\n(?=pub fn |fn |impl )/g;
let n = 0;
t = t.replace(re, (m) => { n++; return '\n'; });
fs.writeFileSync(p, t);
console.log('orphan derives removed:', n);
