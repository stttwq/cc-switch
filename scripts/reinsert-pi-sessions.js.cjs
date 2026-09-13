// one-off: restore session_files_from_resolution into session_manager/providers/pi.rs from HEAD
const { execSync } = require('child_process');
const fs = require('fs');
const head = execSync('git show "HEAD:src-tauri/src/session_manager/providers/pi.rs"', { encoding: 'utf8' });
const lines = head.split('\n');
const startIdx = lines.findIndex((l) => l.includes('fn session_files_from_resolution'));
if (startIdx < 0) { console.error('not found'); process.exit(1); }
let s = startIdx;
while (s > 0 && /^\s*(\/\/|\/\/\/|#\[)/.test(lines[s - 1])) s--;
let e = startIdx;
while (e < lines.length && lines[e] !== '}') e++;
const item = lines.slice(s, e + 1).join('\n') + '\n';
console.log(item);
const p = 'src-tauri/src/session_manager/providers/pi.rs';
let cur = fs.readFileSync(p, 'utf8');
if (cur.includes('fn session_files_from_resolution')) { console.log('already present'); process.exit(0); }
const anchor = cur.indexOf('#[cfg(test)]');
if (anchor < 0) { console.error('no tests anchor'); process.exit(1); }
cur = cur.slice(0, anchor) + item + '\n' + cur.slice(anchor);
fs.writeFileSync(p, cur);
console.log('inserted');
