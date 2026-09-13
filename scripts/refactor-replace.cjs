// one-off textual replacement helper for the slimdown refactor
const fs = require('fs');
const [,, file, pattern, replacement, flags] = process.argv;
let text = fs.readFileSync(file, 'utf8');
const re = new RegExp(pattern, flags || 'g');
const before = text;
text = text.replace(re, replacement);
if (text !== before) {
  fs.writeFileSync(file, text);
  console.log('CHANGED', file);
} else {
  console.log('NOCHANGE', file);
}
