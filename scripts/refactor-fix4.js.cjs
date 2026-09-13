const fs = require('fs');

// pi.rs: switch locks
{
  const p = 'src-tauri/src/services/provider/pi.rs';
  let t = fs.readFileSync(p, 'utf8');
  const n = (t.match(/state\.proxy_service\.lock_switch_for_app/g) || []).length;
  t = t.replace(/state\.proxy_service\.lock_switch_for_app/g, 'state.switch_locks.lock_for_app');
  fs.writeFileSync(p, t);
  console.log('pi.rs locks fixed:', n);
}

// mod.rs: third-party official check
{
  const p = 'src-tauri/src/services/provider/mod.rs';
  let t = fs.readFileSync(p, 'utf8');
  if (!t.includes('!crate::proxy::providers::is_codex_official_provider(provider)')) {
    console.error('mod.rs third-party check not found'); process.exit(1);
  }
  t = t.replace('!crate::proxy::providers::is_codex_official_provider(provider)',
                '!crate::codex_config::is_codex_official_provider(provider)');
  fs.writeFileSync(p, t);
  console.log('mod.rs third-party check fixed');
}
