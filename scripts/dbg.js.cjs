const fs = require('fs');
let t = fs.readFileSync('src-tauri/src/services/provider/mod.rs', 'utf8');
const i = t.indexOf('target_managed_codex_account_id.is_none');
console.log('found at', i);
if (i >= 0) console.log(JSON.stringify(t.slice(i - 80, i + 40)));
