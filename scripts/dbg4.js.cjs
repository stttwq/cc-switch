const fs = require('fs');
const p = 'src-tauri/src/provider.rs';
let t = fs.readFileSync(p, 'utf8');
const i = t.indexOf('认证绑定来源');
console.log(JSON.stringify(t.slice(i - 260, i + 30)));
