const fs = require('fs');
const p = 'src-tauri/src/tray.rs';
let t = fs.readFileSync(p, 'utf8');
const re = /\/\/\/ [^\n]*pub const TRAY_ID: &str = "cc-switch";/;
if (!re.test(t)) { console.error('not found'); process.exit(1); }
t = t.replace(re, 'pub const TRAY_ID: &str = "cc-switch";');
fs.writeFileSync(p, t);
console.log('TRAY_ID restored');
