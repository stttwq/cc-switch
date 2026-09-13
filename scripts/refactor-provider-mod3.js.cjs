const fs = require('fs');
const p = 'src-tauri/src/services/provider/mod.rs';
let t = fs.readFileSync(p, 'utf8');

// cut gemini scrub backup section by markers
const startMarker = '        // 4) 代理接管中的 live 快照里也可能有一份副本';
const endMarker = '        // 5) `~/.gemini/.env`';
const s = t.indexOf(startMarker);
const e = t.indexOf(endMarker, s);
if (s < 0 || e < 0) { console.error('markers not found', s, e); process.exit(1); }
t = t.slice(0, s) + '        // 4) Live 文件本体已不含共享凭据（无代理接管机制），无需额外清理。\n\n' + t.slice(e);

function must(re, rep, label) {
  if (!re.test(t)) { console.error('NOT MATCHED: ' + label); process.exit(1); }
  t = t.replace(re, rep);
  console.log('ok', label);
}

must(/    \/\/\/ Query provider usage \(re-export\)\r?\n    pub async fn query_usage\([\s\S]*?\n    \}\r?\n\r?\n    \/\/\/ Test usage script \(re-export\)\r?\n    #\[allow\(clippy::too_many_arguments\)\]\r?\n    pub async fn test_usage_script\([\s\S]*?\n    \}\r?\n\r?\n/, '', 'usage re-exports');

must(/        \/\/ Validate and clean UsageScript configuration \(common for all app types\)\r?\n        if let Some\(meta\) = &provider\.meta \{[\s\S]*?        \}\r?\n\r?\n        Ok\(\(\)\)/,
     '        Ok(())', 'validate tail');

fs.writeFileSync(p, t);
console.log('stage 3 done');
