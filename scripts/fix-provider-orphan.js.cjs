const fs = require('fs');
const p = 'src-tauri/src/provider.rs';
let t = fs.readFileSync(p, 'utf8');
const orphan = '\n/// 用量数据\n#[derive(Debug, Clone, Serialize, Deserialize)]\n/// 用量查询结果（支持多套餐）\n#[derive(Debug, Clone, Serialize, Deserialize)]';
if (!t.includes(orphan)) { console.error('orphan not found'); process.exit(1); }
t = t.replace(orphan, '');
fs.writeFileSync(p, t);
console.log('provider.rs orphans removed');
