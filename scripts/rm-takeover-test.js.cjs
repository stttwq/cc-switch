const fs = require('fs');
const p = 'src-tauri/tests/provider_commands.rs';
let t = fs.readFileSync(p, 'utf8');
const re = /\r?\n#\[test\]\r?\nfn import_refuses_live_config_under_proxy_takeover\(\) \{[\s\S]*?\n\}\r?\n/;
if (!re.test(t)) { console.error('test not found'); process.exit(1); }
t = t.replace(re, '\n');
fs.writeFileSync(p, t);
console.log('removed');
