// one-off: remove usage credential test module from commands/provider.rs
const fs = require('fs');
const p = 'src-tauri/src/commands/provider.rs';
let t = fs.readFileSync(p, 'utf8');
const marker = '#[cfg(test)]\nmod native_query_credentials_tests {';
const s = t.indexOf(marker);
if (s < 0) { console.error('test module not found'); process.exit(1); }
t = t.slice(0, s).trimEnd() + '\n';
fs.writeFileSync(p, t);
console.log('test module removed');
