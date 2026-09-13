// one-off: cut the big inline tests module in mod.rs (between marker and impl ProviderService)
const fs = require('fs');
const p = 'src-tauri/src/services/provider/mod.rs';
let t = fs.readFileSync(p, 'utf8');
const startMarker = '#[cfg(test)]\nmod tests {';
const endMarker = 'impl ProviderService {';
const s = t.indexOf(startMarker);
if (s < 0) { console.error('tests module not found'); process.exit(1); }
const e = t.indexOf(endMarker, s);
if (e < 0) { console.error('impl not found'); process.exit(1); }
const cutLen = e - s;
t = t.slice(0, s) + t.slice(e);
fs.writeFileSync(p, t);
console.log('cut', cutLen, 'chars of tests module');
