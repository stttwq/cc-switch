const fs = require('fs');
const t = fs.readFileSync('src-tauri/src/services/provider/mod.rs', 'utf8').split(/\r?\n/);
let depth = 0; const bad = [];
const marks = [];
for (let i = 0; i < t.length; i++) {
  const line = t[i];
  if (/^\s*(pub )?(mod tests|impl |fn |pub fn|pub\(crate\) fn )/.test(line)) marks.push([i + 1, depth, line.trim().slice(0, 60)]);
  for (const ch of line) { if (ch === '{') depth++; else if (ch === '}') depth--; }
  if (depth < 0) { bad.push(i + 1); depth = 0; }
}
console.log('total lines', t.length, 'final depth', depth);
console.log('negative at', bad);
for (const m of marks) console.log(m[0], 'd' + m[1], m[2]);
