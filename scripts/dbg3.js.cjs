const fs = require('fs');
const p = 'src-tauri/src/services/provider/mod.rs';
let t = fs.readFileSync(p, 'utf8');
const i = t.indexOf('4) 代理接管中的 live 快照');
console.log('idx', i);
if (i >= 0) console.log(JSON.stringify(t.slice(i - 30, i + 120)));
