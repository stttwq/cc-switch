#!/usr/bin/env node
// 只校验仓库内 Markdown 的“相对链接与页内锚点”，不访问网络。
// 目的：本轮 P2 的痛点是仓库内互相指错（多语手册/上游链接收敛后留下的死链），
// 外链检查在 CI 里不稳定，这里刻意不做。
//
// 用法：node scripts/check-links.mjs [paths...]
// 默认扫描仓库根下所有被 git 跟踪的 .md（排除归档与 plans）。

import { execFileSync } from "node:child_process";
import { readFileSync, existsSync } from "node:fs";
import { dirname, resolve, relative } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

// 归档与施工方案文档里的历史链接不作要求，排除之。
const EXCLUDE = [
  "node_modules/",
  "dist/",
  "src-tauri/target/",
  "docs/plans/",
  "docs/changelog-upstream-3.x.md",
];

const IGNORED_LINK_PREFIXES = [
  "http://",
  "https://",
  "mailto:",
  "tel:",
  "data:",
  "javascript:",
  "about:",
  "file:",
];

// GitHub 标题锚点规则：去掉 Markdown 强调/代码/行内标记，小写，
// 保留 Unicode 字母数字与空格连字符，其它标点删除，空格转连字符。
// 末尾的 `{#custom-id}` 若存在则直接作为 id。
function slugify(headingText) {
  const custom = headingText.match(/\{#([^}]+)\}\s*$/);
  if (custom) return custom[1].toLowerCase();
  return headingText
    .replace(/`([^`]*)`/g, "$1") // 行内代码
    .replace(/\*\*([^*]*)\*\*/g, "$1")
    .replace(/\*([^*]*)\*/g, "$1")
    .replace(/__([^_])__/g, "$1")
    .replace(/_([^_])_/g, "$1")
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1") // 链接取文字
    .replace(/\{#[^}]+\}/g, "")
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{N} \-]/gu, "")
    .replace(/ /g, "-");
}

function collectFiles(explicitPaths) {
  if (explicitPaths && explicitPaths.length) return explicitPaths;
  const out = execFileSync("git", ["ls-files", "*.md"], {
    cwd: ROOT,
    encoding: "utf8",
  });
  return out
    .split(/\r?\n/)
    .filter(Boolean)
    .filter((p) => !EXCLUDE.some((e) => p.startsWith(e) || p.includes(e)));
}

function headingsOf(content) {
  const ids = new Set();
  for (const line of content.split(/\r?\n/)) {
    const m = line.match(/^\s{0,3}#{1,6}\s+(.+?)\s*$/);
    if (m) ids.add(slugify(m[1].trim()));
  }
  return ids;
}

// 抓取 ![alt](url) 与 [text](url)，忽略引用式链接与图片 data 之外的东西。
const LINK_RE = /(!?)\[([^\]]*)\]\(\s*([^)\s]+)(?:\s+"[^"]*")?\s*\)/g;

function main() {
  const files = collectFiles(process.argv.slice(2));
  const anchorCache = new Map();
  const problems = [];

  for (const fileRel of files) {
    const abs = resolve(ROOT, fileRel);
    let content;
    try {
      content = readFileSync(abs, "utf8");
    } catch {
      continue;
    }
    // 跳过代码围栏内的链接，避免把示例误判为死链。
    let inFence = false;
    const lines = content.split(/\r?\n/);
    lines.forEach((line, idx) => {
      if (/^\s{0,3}(```|~~~)/.test(line)) {
        inFence = !inFence;
        return;
      }
      if (inFence) return;
      let m;
      LINK_RE.lastIndex = 0;
      while ((m = LINK_RE.exec(line)) !== null) {
        const target = m[3];
        if (IGNORED_LINK_PREFIXES.some((p) => target.toLowerCase().startsWith(p)))
          continue;
        if (target.startsWith("#")) {
          // 页内锚点：与本文标题比对。
          const id = decodeURIComponent(target.slice(1)).toLowerCase();
          const ids = headingsOf(content);
          if (!ids.has(id)) {
            problems.push(`${fileRel}:${idx + 1} 缺少页内锚点目标 ${target}`);
          }
          continue;
        }
        const [pathPart, anchorPart] = target.split("#");
        if (!pathPart) continue; // 纯 #anchor 已在上面处理
        const resolvedRel = resolve(dirname(abs), pathPart);
        if (!existsSync(resolvedRel)) {
          problems.push(`${fileRel}:${idx + 1} 目标文件不存在 ${pathPart}`);
          continue;
        }
        if (anchorPart && resolvedRel.endsWith(".md")) {
          let ids = anchorCache.get(resolvedRel);
          if (!ids) {
            ids = headingsOf(readFileSync(resolvedRel, "utf8"));
            anchorCache.set(resolvedRel, ids);
          }
          const id = decodeURIComponent(anchorPart).toLowerCase();
          if (!ids.has(id)) {
            const shown = relative(ROOT, resolvedRel).replace(/\\/g, "/");
            problems.push(
              `${fileRel}:${idx + 1} ${shown} 中找不到锚点 #${anchorPart}`,
            );
          }
        }
      }
    });
  }

  if (problems.length) {
    console.error(`发现 ${problems.length} 处失效的仓库内链接：\n`);
    for (const p of problems) console.error("  - " + p);
    process.exit(1);
  }
  console.log(`OK: ${files.length} 个 Markdown 文件的仓库内相对链接与锚点均有效。`);
}

main();
