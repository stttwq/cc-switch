const fs = require('fs');

// 1. codex_config: restore CC_SWITCH_CODEX_MODEL_CATALOG_FILENAME on its own line
{
  const p = 'src-tauri/src/codex_config.rs';
  let t = fs.readFileSync(p, 'utf8');
  const re = /for takeover\.pub const CC_SWITCH_CODEX_MODEL_CATALOG_FILENAME/;
  if (re.test(t)) {
    t = t.replace(re, 'for takeover.\npub const CC_SWITCH_CODEX_MODEL_CATALOG_FILENAME');
    fs.writeFileSync(p, t);
    console.log('codex const split');
  } else {
    console.log('codex const already ok');
  }
}
