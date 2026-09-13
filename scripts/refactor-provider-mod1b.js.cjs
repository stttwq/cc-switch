const fs = require('fs');
const p = 'src-tauri/src/services/provider/mod.rs';
let t = fs.readFileSync(p, 'utf8');

function must(re, rep, label) {
  if (!re.test(t)) { console.error('NOT MATCHED: ' + label); process.exit(1); }
  t = t.replace(re, rep);
  console.log('ok', label);
}

must(/        \/\/ Serialize the read\/decide\/commit window for every Codex update\.[\s\S]*?        \};\r?\n        let existing_provider = state/, '        let existing_provider = state', 'update switch lock');

fs.writeFileSync(p, t);
console.log('stage 1b done');
