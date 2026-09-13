const fs = require('fs');
const p = 'src-tauri/src/services/provider/mod.rs';
let t = fs.readFileSync(p, 'utf8');
const lines = t.split('\n');
// print exact lines around error positions (1-based)
for (const n of [4828, 4929, 5581, 5766, 5927]) {
  console.log('--- around', n);
  console.log(lines.slice(n - 4, n + 3).map((l, i) => (n - 3 + i) + ': ' + l).join('\n'));
}
