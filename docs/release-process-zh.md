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
git push origin HEAD        # 推送当前分支，不默认推 main
```

### 3. 本地构建安装包

```bash
pnpm tauri build
```

产物在 `src-tauri/target/release/bundle/msi/CC Switch_<版本号>_x64_zh-CN.msi`。装到自己机器上跑一遍，确认「关于」页版本号正确、供应商列表正常、能切换，再往下发。

### 3.5 生成校验清单与 minisign 签名

安装包未做 Authenticode 签名，个人 fork 也不买证书；改用 minisign 让用户验证"这个包确实是维护者发的、且没被篡改"。密钥对**已生成**（一次性），私钥只留在发布机 `D:\GZ\Xlh\minisign.ccswitch.key`、**绝不入库**，公钥已提交为仓库根 [`minisign.pub`](../minisign.pub) 并写进 README 与 SECURITY.md。若日后轮换密钥：

```bash
"D:\GZ\Xlh\minisign-win64\x86_64\minisign.exe" -G \
  -p D:\GZ\Xlh\minisign.ccswitch.pub -s D:\GZ\Xlh\minisign.ccswitch.key
# 然后把新的 minisign.ccswitch.pub 覆盖仓库根 minisign.pub 并公告
```

每次发布对 MSI 生成校验清单并签名（Git Bash）。**关键顺序（DEL-A）**：先把 MSI 复制成最终上传名，再对最终名生成清单并签名——否则 `SHA256SUMS` 里记的是构建原名 `CC Switch_*_x64_zh-CN.msi`，用户下载到的是改名后的 `CC-Switch-<标签>-Windows.msi`，`sha256sum -c` 必然失败。

```bash
MINISIGN="/d/GZ/Xlh/minisign-win64/x86_64/minisign.exe"
KEY="/d/GZ/Xlh/minisign.ccswitch.key"
TAG="v$(node -p "require('./package.json').version")-$(git rev-parse --short=7 HEAD)"
MSI_DIR="src-tauri/target/release/bundle/msi"
STAGING="src-tauri/target/release/release-staging"

# 1. 新建独立 staging 目录，只放本次产物（禁止对历史 build 目录通配符取第一份）
rm -rf "$STAGING" && mkdir -p "$STAGING"

# 2. 先复制成最终上传名
MSI="$MSI_DIR/CC Switch_$(node -p "require('./package.json').version")_x64_zh-CN.msi"
test -f "$MSI" || { echo "未找到预期 MSI: $MSI" >&2; exit 1; }
cp "$MSI" "$STAGING/CC-Switch-$TAG-Windows.msi"

# 3. 对最终名生成校验清单并签名
( cd "$STAGING" && \
  sha256sum CC-Switch-$TAG-Windows.msi > SHA256SUMS && \
  "$MINISIGN" -S -m SHA256SUMS -s "$KEY" -x SHA256SUMS.minisig -H )   # 提示输一次 passcode

# 4. 干净目录回验：新建空目录只放这四个文件，模拟用户下载后的验证
#    （防止本机残留的原名 MSI 掩盖文件名错位）
VERIFY_DIR="$STAGING/verify-clean"
mkdir -p "$VERIFY_DIR"
cp "$STAGING/CC-Switch-$TAG-Windows.msi" "$STAGING/SHA256SUMS" \
   "$STAGING/SHA256SUMS.minisig" ../../minisign.pub "$VERIFY_DIR/"
( cd "$VERIFY_DIR" && \
  "$MINISIGN" -Vm SHA256SUMS -p minisign.pub -x SHA256SUMS.minisig && \
  sha256sum -c SHA256SUMS )
```

`rm -rf "$STAGING"` 只针对本次发布专用的 `release-staging` 目录，不得指向其他路径。

用户侧验证（公钥取本仓库根 `minisign.pub`），同样应在一个只含下载文件的目录里执行：

```bash
minisign -Vm SHA256SUMS -p minisign.pub -x SHA256SUMS.minisig
sha256sum -c SHA256SUMS
```

### 4. 打标签并推送

```bash
SHORT=$(git rev-parse --short=7 HEAD)
TAG="v$(node -p "require('./package.json').version")-$SHORT"
git tag -a "$TAG" -m "CC Switch $TAG"
git push origin "$TAG"      # 只推标签，不会触发云端发布
```

### 5. 建 Release 并上传产物

直接从 staging 目录上传（DEL-A：清单与签名都对应最终上传名）：

```bash
STAGING="src-tauri/target/release/release-staging"
TAG="v$(node -p "require('./package.json').version")-$(git rev-parse --short=7 HEAD)"

gh release create "$TAG" \
  "$STAGING/CC-Switch-$TAG-Windows.msi" \
  "$STAGING/SHA256SUMS" "$STAGING/SHA256SUMS.minisig" \
  --title "CC Switch $TAG" \
  --latest \
  --notes "## CC Switch $TAG

Claude Code、Codex 与 Pi 的供应商切换工具（Windows 专版）。

### 下载

- **Windows (x86_64)**: \`CC-Switch-$TAG-Windows.msi\`
- **校验**：\`SHA256SUMS\` 与 minisign 签名 \`SHA256SUMS.minisig\`（公钥见仓库根 \`minisign.pub\`）。安装包未做代码签名，首次运行 SmartScreen 会提示未知发布者，属正常。"
```

预发布版本改加 `--prerelease`，正式版用 `--latest`。GitHub 上显示的产物名取的是文件本身的名字（不是上传参数），所以必须先 `cp` 成 `CC-Switch-<标签>-Windows.msi` 再传。

### 6. 核对

```bash
gh release view "$TAG"      # 产物名、大小、是否 latest
```

### 7. 换包 / 补传（Release 已存在时）

别重复 `gh release create`，用 `--clobber` 覆盖同名产物，然后从公开地址回下载比对哈希，确认线上和本地是同一个文件：

```bash
STAGING="src-tauri/target/release/release-staging"
gh release upload "$TAG" \
  "$STAGING/CC-Switch-$TAG-Windows.msi" \
  "$STAGING/SHA256SUMS" "$STAGING/SHA256SUMS.minisig" --clobber
curl -sL "https://github.com/stttwq/cc-switch/releases/download/$TAG/CC-Switch-$TAG-Windows.msi" | sha256sum
sha256sum "$STAGING/CC-Switch-$TAG-Windows.msi"   # 两行哈希一致即通过
```

同版本换包后必须重新生成校验清单并重新签名，再 `--clobber` 上传。

## 云端补位（仅在本地发布不可用时）

Actions → Release → Run workflow → 填标签名。工作流会校验标签格式（只接受 `v数字.数字.数字` 及其 `-短哈希` / `-alpha` / `-beta` / `-rc.N` 形式），校验标签版本与 `package.json` 及 MSI ProductVersion 一致，按标签检出对应提交后构建，只接受唯一精确命名的 MSI，并对最终资产名生成 `SHA256SUMS`。

云端**只产出草稿 Release**（DEL-A）：私钥只留发布机，不上传 GitHub Secret。维护者需要：

1. 从草稿 Release 下载 `SHA256SUMS`，在发布机离线执行 minisign 签名生成 `SHA256SUMS.minisig`；
2. 上传 `SHA256SUMS.minisig` 到草稿，核对附件齐全（MSI + SHA256SUMS + 签名）后手动点击 Publish。

不允许把只有 MSI（或缺清单/签名）的附件集直接发布为正式 Release。
