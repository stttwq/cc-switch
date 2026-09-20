# 发布流程

正式发布一律在本地构建，再用 GitHub CLI 上传到 GitHub Release。云端 `.github/workflows/release.yml` 已改为只能手动触发，只在本地发不出去时补位。

产物只有一个：中文向导的 Windows MSI（x86_64）。本项目不产出也不分发绿色版/便携版。

## 版本号规则

- 补丁位递增：`2.0.0` → `2.0.1` → `2.0.2` …
- 标签 = `v<版本号>-<提交短哈希>`，例如 `v2.0.1-ab12cd3`。
- 三处版本字段必须同时改，且只写纯数字版本号（短哈希不进版本号，否则 MSI 的 ProductVersion 非法）：
  - `package.json`
  - `src-tauri/tauri.conf.json`
  - `src-tauri/Cargo.toml`
- `CHANGELOG.md` 补一条 `## [x.y.z] - 日期`。

## 一次性准备

```powershell
winget install --id GitHub.cli
gh auth login    # 选 GitHub.com → HTTPS → 浏览器授权
gh auth status   # 确认已登录 stttwq
```

## 每次发布

### 1. 门禁全绿

```bash
npx tsc --noEmit
npx prettier --check "src/**/*.{js,jsx,ts,tsx,css,json}"
npx vitest run
cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --tests
```

### 2. 改版本号并提交推送

```bash
git status --short          # 确认只有预期的文件
git add -p                  # 自行审阅后暂存
git commit                  # 用 /gencom 生成提交信息
git push origin main
```

### 3. 本地构建安装包

```bash
pnpm tauri build
```

产物在 `src-tauri/target/release/bundle/msi/CC Switch_<版本号>_x64_zh-CN.msi`。装到自己机器上跑一遍，确认「关于」页版本号正确、供应商列表正常、能切换，再往下发。

### 4. 打标签并推送

```bash
SHORT=$(git rev-parse --short=7 HEAD)
TAG="v$(node -p "require('./package.json').version")-$SHORT"
git tag -a "$TAG" -m "CC Switch $TAG"
git push origin "$TAG"      # 只推标签，不会触发云端发布
```

### 5. 建 Release 并上传产物

上传前先按历史命名复制一份（放在构建目录里，`src-tauri/target/` 已被 git 忽略）：

```bash
MSI="src-tauri/target/release/bundle/msi/CC Switch_$(node -p "require('./package.json').version")_x64_zh-CN.msi"
UP="src-tauri/target/release/CC-Switch-$TAG-Windows.msi"
cp "$MSI" "$UP"

gh release create "$TAG" "$UP" \
  --title "CC Switch $TAG" \
  --latest \
  --notes "## CC Switch $TAG

Claude Code、Codex 与 Pi 的供应商切换工具（Windows 专版）。

### 下载

- **Windows (x86_64)**: \`CC-Switch-$TAG-Windows.msi\`"
```

预发布版本改加 `--prerelease`，正式版用 `--latest`。GitHub 上显示的产物名取的是文件本身的名字（不是上传参数），所以必须先 `cp` 成 `CC-Switch-<标签>-Windows.msi` 再传。

### 6. 核对

```bash
gh release view "$TAG"      # 产物名、大小、是否 latest
```

### 7. 换包 / 补传（Release 已存在时）

别重复 `gh release create`，用 `--clobber` 覆盖同名产物，然后从公开地址回下载比对哈希，确认线上和本地是同一个文件：

```bash
gh release upload "$TAG" "$UP" --clobber
curl -sL "https://github.com/stttwq/cc-switch/releases/download/$TAG/CC-Switch-$TAG-Windows.msi" | sha256sum
sha256sum "$UP"             # 两行哈希一致即通过
```

## 云端补位（仅在本地发布不可用时）

Actions → Release → Run workflow → 填标签名。工作流会校验标签格式（只接受 `v数字.数字.数字` 及其 `-短哈希` / `-alpha` / `-beta` / `-rc.N` 形式），按标签检出对应提交后构建，产物命名与上面一致。标签以 `-alpha` / `-beta` / `-rc` 结尾时才会标记为预发布。
