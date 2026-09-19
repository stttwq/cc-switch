#!/usr/bin/env node
// Generates the website download manifest (manifest.json) from a directory of
// downloaded release assets. Consumed by ccswitch.io/download. The manifest
// schema is mirrored in cc-switch-website/src/lib/downloads.ts — keep both in
// sync when changing fields or classification rules.
//
// 注意：本脚本不在本仓库 CI 里调用（由网站侧手动/单独跑），因此不要因为
// "仓库内 0 引用" 就删掉它。
//
// Usage: node scripts/generate-download-manifest.mjs <assets-dir> <tag> <base-url> [output] [pub-date]

import { createHash } from 'node:crypto';
import { readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const [assetsDir, tag, baseUrl, output = 'manifest.json', pubDateArg] = process.argv.slice(2);

if (!assetsDir || !tag || !baseUrl) {
  console.error('Usage: node scripts/generate-download-manifest.mjs <assets-dir> <tag> <base-url> [output] [pub-date]');
  process.exit(1);
}

// Prefer the release's real publishedAt (passed by CI) over generation time.
const pubDate = pubDateArg ? new Date(pubDateArg) : new Date();
if (Number.isNaN(pubDate.getTime())) {
  console.error(`Invalid pub-date: ${pubDateArg}`);
  process.exit(1);
}

// 发布面已按 D1 收敛为 Windows x86_64 单一产物（自动更新移除后也不再产出 .sig /
// latest.json）。这里只保留 release.yml 实际会打出来的两种资产；等发布面恢复多平台时
// 再按"-Windows-arm64.msi 在 -Windows.msi 之前"的顺序补回来。
const RULES = [
  { suffix: '-Windows-Portable.zip', platform: 'windows', kind: 'portable', arch: 'x64' },
  { suffix: '-Windows.msi', platform: 'windows', kind: 'msi', arch: 'x64' },
];

const normalizedBase = baseUrl.replace(/\/+$/, '');
const files = [];

for (const name of readdirSync(assetsDir).sort()) {
  // Unmatched files are deliberately skipped — they are not user-facing downloads.
  const rule = RULES.find((entry) => name.endsWith(entry.suffix));
  if (!rule) continue;
  const path = join(assetsDir, name);
  files.push({
    platform: rule.platform,
    kind: rule.kind,
    arch: rule.arch,
    name,
    size: statSync(path).size,
    sha256: createHash('sha256').update(readFileSync(path)).digest('hex'),
    url: `${normalizedBase}/${tag}/${encodeURIComponent(name)}`,
  });
}

if (files.length === 0) {
  console.error(`No release assets matched in ${assetsDir}`);
  process.exit(1);
}

const manifest = {
  version: tag.replace(/^v/, ''),
  tag,
  pubDate: pubDate.toISOString(),
  files,
};

writeFileSync(output, `${JSON.stringify(manifest, null, 2)}\n`);
console.log(`Wrote ${output} with ${files.length} files for ${tag}`);
