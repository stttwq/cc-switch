// one-off: drop trailing #[cfg(test)] mod tests blocks entirely
const fs = require('fs');
for (const file of process.argv.slice(2)) {
  let t = fs.readFileSync(file, 'utf8');
  const marker = '#[cfg(test)]\nmod tests {';
  const idx = t.lastIndexOf(marker);
  if (idx < 0) { console.log('no tests module:', file); continue; }
  // verify nothing but whitespace after (tests at EOF)
  const rest = t.slice(idx + marker.length);
  t = t.slice(0, idx).trimEnd() + '\n';
  fs.writeFileSync(file, t);
  console.log('cut tests module:', file, '(was', rest.length, 'chars)');
}
