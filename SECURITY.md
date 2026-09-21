# Security Policy / 安全策略

## Supported Versions / 支持的版本

CC Switch 是本机上运行的 Windows 专版分支（基于上游 3.20.3）。仅当前 `2.0.x` 最新正式版收到安全更新；上游 3.x 与更早版本由上游仓库负责，不在本分支支持范围。

This is a Windows-only fork of the upstream project. Only the latest `2.0.x` stable release of this branch receives security updates; upstream 3.x and earlier are maintained (if at all) by the upstream repository and are out of scope here.

| Version / 版本 | Supported / 是否支持 |
|----------------|---------------------|
| Latest 2.0.x   | ✅ Yes / 是          |
| Other / 其它   | ❌ No / 否           |

## Threat Model / 威胁模型

CC Switch is a local desktop application. It manages configuration files for AI coding CLIs on the user's own machine. There is no project-operated cloud backend, no multi-user model, and no privilege separation from the user who runs it.

CC Switch 是一个本地桌面应用，用于管理本机上各 AI 编程 CLI 的配置文件。本项目不运营任何云端后端，没有多用户模型，也不与运行它的用户之间存在权限隔离。

Credentials (API keys and Base URLs) are stored only in **Windows Credential Manager**. Delivery to CLIs is via **user-level environment variables** (`HKCU\Environment`); live files must not contain secret values. Those environment variables are kept in **plaintext on disk** under `HKCU\Environment`, and only the **active** provider's copy is ever present — switching providers removes the previous values before writing the new ones. Codex and Pi currently still write the **active** Base URL into live config because those CLIs have no env-var indirection for it.

密钥与 Base URL 仅存放在 **Windows 凭据管理器**。向 CLI 投递的方式是**用户级环境变量**（`HKCU\Environment`）；live 文件不得含密钥值。这些环境变量的值在 `HKCU\Environment` 中**明文存储**，且任何时刻只保留**当前激活**供应商的那一份——切换供应商时会先删掉旧值再写入新值。Codex / Pi 因 CLI 没有环境变量间接引用，当前仍会把**当前激活**供应商的 Base URL 写入 live 配置。

**Frontend zero-secret by default / 前端默认零密钥.** Batch reads — the provider list, cards, tray — never carry a secret value. A credential value crosses IPC only when the user **explicitly clicks "显示" for a single field of a single provider**, via the dedicated `reveal_provider_secret` command; the frontend must not place that value in a TanStack Query cache, `localStorage`, a form initial-value snapshot, or any log, and the value is dropped when the dialog closes. As an intentional exception, `get_providers` does return each provider's **Base URL** for the card display — a Base URL is far less sensitive than a key and the user explicitly wants to see which endpoint is active. It reads Credential Manager for that one non-secret field only.

**前端默认零密钥。** 批量读取（列表、卡片、托盘）永不携带密钥值。密钥值只在用户**显式点击单个供应商单个字段的"显示"**时，经专用命令 `reveal_provider_secret` 按需回传一次；前端不得把它放进 TanStack Query 缓存、`localStorage`、表单初始值快照或任何日志，对话框关闭即丢弃。作为有意例外，`get_providers` 会为卡片显示回传每个供应商的 **Base URL**——Base URL 敏感度远低于密钥，且用户明确希望看到当前指向哪个端点；它仅为此一个非密钥字段读取凭据管理器。

**Credential persistence scope and portability / 凭据持久化范围与可移植性.** Entries are written through `keyring` with `CRED_PERSIST_ENTERPRISE`, so on a domain-joined machine they roam with the Windows user profile to every machine that user signs into; domain users who want machine-local persistence can change the entry's scope in the Credential Manager control panel. Secret values live outside the database and are **never** part of SQL export, WebDAV / S3 sync payloads or backups — after restoring a config on another machine every provider needs its key typed in again. That is by design, not a defect.

凭据条目经 `keyring` 以 `CRED_PERSIST_ENTERPRISE` 写入，因而在域环境下会随 Windows 用户配置文件漫游到该用户登录的每一台机器；希望只保留在本机的域环境用户，可在凭据管理器控制面板中改该条目的持久化范围。密钥存放在数据库之外，**不进入** SQL 导出、WebDAV / S3 同步载荷与备份——因此在另一台机器还原配置后，每个供应商都要重新输入密钥。这是设计，不是缺陷。

**Rolling back to a pre-migration version / 回滚到迁移前的旧版本.** The v18→v19 upgrade keeps one dated backup `~/.cc-switch/backups/pre-secrets-migration-*.db` that still contains the old plaintext schema; it is intentionally NOT auto-deleted. Overwriting `cc-switch.db` with it lets an older build read it normally. Extra `cc-switch/`-prefixed entries left in Windows Credential Manager are then harmless and can be removed manually from the Credential Manager control panel.

v18→v19 升级会保留一份带时间戳的备份 `~/.cc-switch/backups/pre-secrets-migration-*.db`，其中仍是旧的明文 schema，且**刻意不自动删除**。用它覆盖 `cc-switch.db` 即可让旧版本正常读取；此时凭据管理器里多出的 `cc-switch/` 前缀条目无害，可在凭据管理器控制面板手动清理。

### The bundled renderer is inside the trust boundary / 打包的渲染进程属于信任边界之内

The bundled WebView renderer is treated as a trusted component. This is a **scoping decision, supported by the facts below rather than derived from them** — the facts are what make the decision checkable, and if any ceases to hold the decision must be revisited. Verified against v2.0.x:

打包的 WebView 渲染进程被视为可信组件。这是一项**范围划定决策，由下列事实支撑，而非从中必然推出**——这些事实的作用是让该决策可被核验；一旦任一条不再成立，该决策必须重新评估。已针对 v2.0.x 核实：

1. **No remote executable content is loaded.** `frontendDist` is bundled at build time (`src-tauri/tauri.conf.json`); the codebase contains no `<iframe>`, no `<webview>`, and no remote script or stylesheet URL. Avatars may load over `img-src https:`; Skills and sync go through the Rust backend, not the WebView.
   前端资源在构建期打包。头像可通过 `img-src https:` 加载；Skills 与同步走 Rust 后端，不经 WebView 直连。
2. **CSP restricts script execution to bundled assets** — `script-src 'self'` (`src-tauri/tauri.conf.json`).
   CSP 将脚本执行限制在打包资源内。
3. **No dynamic code evaluation.** There is no `eval()` or `new Function()` anywhere under `src/`.
   `src/` 下不存在 `eval()` 或 `new Function()`。
4. **The single HTML sink never receives user-controlled content.** `src/components/ProviderIcon.tsx` uses `dangerouslySetInnerHTML` exactly once; the user-supplied `icon` field is used **solely as a lookup key** into a build-time icon table, never as content.
   全库唯一的 `dangerouslySetInnerHTML` 位于 `src/components/ProviderIcon.tsx`；用户提供的 `icon` 字段**仅作为查表的键**用于构建期图标表，永不作为内容。

**What this excludes — and what it does not.** Out of scope: reports whose only path to the IPC surface is **direct invocation from DevTools or from a locally modified frontend**. Someone in that position controls the machine already.

**它排除什么、不排除什么。** 不在范围内的是：抵达 IPC 接口的**唯一途径**为**从 DevTools 或本地改造过的前端直接调用**的报告。处于该位置的人已经控制了这台机器。

**Still in scope:** any complete, demonstrable chain in which an *untrusted* source — a remote sync payload, remote data, or an XSS — reaches a high-privilege IPC command. The trust placed in the renderer covers the code we ship, not arbitrary values that flow through it.

**仍在范围内**：任何完整、可演示的利用链，其中**不可信来源**——远程同步载荷、远程数据或 XSS——抵达高权限 IPC 命令。对渲染进程的信任覆盖的是我们发布的代码，而非流经其中的任意值。

### Invalidation triggers / 声明失效条件

The scoping decision above is void the moment any of the following becomes true. Reports demonstrating any of these are **always in scope**, and we ask to be told about them:

一旦下列任一条件成立，上述范围划定立即作废。证明下列任一情形的报告**始终在范围内**，也欢迎报告：

- Remote **executable or navigable** content is loaded into the WebView — iframe, webview, remote script, remote stylesheet, or navigation to a remote origin
  远程**可执行或可导航**内容被载入 WebView——iframe、webview、远程脚本、远程样式表，或导航至远程源
- `script-src` is relaxed beyond `'self'`
  `script-src` 被放宽到 `'self'` 之外
- `eval()` or `new Function()` is introduced
  引入 `eval()` 或 `new Function()`
- Any user-controlled string reaches an HTML sink as content
  任何用户可控字符串作为内容进入 HTML sink
- The IPC surface is exposed to a non-bundled origin
  IPC 接口暴露给非打包来源

## Scope / 范围

### In scope / 在范围内

Inputs that genuinely cross a trust boundary:

真正跨越信任边界的输入：

- Remote sync payloads restored from WebDAV / S3 / 从 WebDAV、S3 还原的同步数据
- Imported files: SQL import/export, provider and MCP config import / 导入文件：SQL 导入导出、供应商与 MCP 配置导入
- Remote data rendered by the renderer (avatars) / 渲染进程展示的远程数据（头像）
- Live config files on disk that a third party can write / 磁盘上可被第三方写入的 live 配置文件
- Any path by which credentials (API keys, tokens) reach logs, telemetry, or shared config snippets / 凭据（API Key、令牌）进入日志、遥测或共享配置片段的任何路径
- The build, release and signing pipeline / 构建、发布与签名链路

### Out of scope / 不在范围内

- Findings whose only path to the IPC surface is direct invocation from DevTools or a locally modified frontend — see Threat Model
  抵达 IPC 接口的唯一途径为从 DevTools 或本地改造过的前端直接调用的问题——见威胁模型
- **Ordinary file operations the user directs.** Reading or writing a file whose path *and* content the user chose through the local UI, with no untrusted input participating.
  **用户主动指示的常规文件操作。** 读写路径**与**内容均由用户经本地界面选定、且无不可信输入参与的文件。
  → Not excluded: cases where a sync payload or other untrusted source controls the path or the content. Having the same filesystem permissions as the user does not make it the user's decision — that is a confused-deputy attack and is **in scope**.
  → 不属豁免：路径或内容由同步载荷等不可信来源控制的情形。攻击者与用户拥有相同的文件系统权限，并不等于该操作出自用户的决定——那是 confused deputy 攻击，**在范围内**。
- **User-authored integrations executing by design.** MCP servers and terminal launch run commands because that is their purpose. Where the user typed the command themselves and enabled it themselves, execution is the feature, not the bug.
  **用户亲手编写的集成按设计执行命令。** MCP server、终端启动执行命令是其本职。命令由用户自己输入、并由用户自己启用时，执行本身是功能而非缺陷。
  → Not excluded: the same integrations when they **arrive through import**. There the required security property is *informed consent*, and the following are **in scope**: the command, arguments, environment or script body being hidden, truncated or misrepresented in the confirmation UI; and any integration carrying executable content being enabled without an explicit user decision.
  → 不属豁免：同样的集成**经导入抵达**时。此时所要求的安全属性是**知情同意**，下列情形**在范围内**：确认界面隐藏、截断或错误展示命令、参数、环境变量或脚本正文；以及任何携带可执行内容的集成在缺少用户明确决定的情况下被启用。
- Denial of service against the user's own local instance
  针对用户自己本地实例的拒绝服务
- Automated scanner output without a demonstrated exploitation path on a currently supported release
  未在受支持版本上给出可行利用路径的自动化扫描结果
- Findings against unsupported versions — see Supported Versions
  针对不受支持版本的问题——见支持的版本

## IPC Path Parameters / IPC 路径参数来源

Commands that accept a filesystem path are enumerated below with where the value is allowed to originate. A path parameter must come from one of the trusted sources listed; anything flowing from a sync payload or other untrusted source into these parameters is a **confused deputy** and in scope.

接受文件系统路径的命令列于此表，并标注其允许的来源。同步载荷或其它不可信来源流入这些路径参数即属 **confused deputy**，在范围内。

| Command / 命令 | Path parameter / 路径参数 | Allowed source / 允许来源 |
|---|---|---|
| `get_session_messages` | `sourcePath` | 后端枚举的会话目录（限制在供应商根目录内，`canonicalize` + `starts_with` 校验） |
| `delete_session` | `sourcePath` | 同上 |
| `reveal_provider_secret` | `app` + `providerId` + `field` | 前端来自 `get_providers`；后端先校验供应商存在于 DB，`field` 仅 `api_key`/`base_url` |
| `open_external` | `url` | 后端用 `url::Url::parse` 校验，仅放行 `http`/`https`；调用方传完整 URL |
| 配置导入/导出 | 目标文件路径 | 由系统文件对话框在 Rust 侧产生，路径不经前端往返 |

## Sync Encryption / 同步加密（端到端）

When end-to-end sync encryption is enabled, the WebDAV / S3 server — and anyone who reads it (CDN, bucket operator, a compromised host) — sees only ciphertext. The payload is encrypted on this device and decrypted only on another device that knows the passphrase.

启用端到端同步加密后，WebDAV / S3 服务器及其的一切读取方（CDN、存储桶运维、被入侵的主机）只能看到密文。载荷在本机加密，只在持有口令的另一台设备上解密。

- **Construction / 构造**: passphrase → **Argon2id** (m=64 MiB, t=3, p=1, 16-byte random salt) → KEK; a fresh random 32-byte DEK per upload encrypts `db.sql.enc` and `skills.zip.enc` with **XChaCha20-Poly1305**; the DEK is wrapped by the KEK and stored in the manifest. Each artifact's AAD binds `format‖version‖snapshotId‖artifactName‖seq`, and the device name / timestamps live inside an encrypted inner manifest — so a blob cannot be swapped between snapshots and the plaintext outer `manifest.json` carries no user data. 口令经 Argon2id 派生 KEK；每次上传随机生成 DEK 以 XChaCha20-Poly1305 加密两个 blob，DEK 由 KEK 封装。AAD 绑定快照与序号，设备名/时间在加密的内层 manifest 内，明文外层不含任何用户数据。
- **The passphrase never leaves the device / 口令永不出本机**: it is stored only in the Windows Credential Manager (`cc-switch/v1/app/sync/passphrase`) and is **never uploaded**. It is what makes zero trust against the server hold. 口令只存于 Windows 凭据管理器，**绝不上上传**。
- **Forgetting the passphrase is unrecoverable / 忘记口令不可恢复**: there is no backdoor and no server-side reset. Losing the passphrase means the encrypted remote snapshot cannot be decrypted by anyone, including the maintainer. 无后门、无服务端重置。忘记口令意味着远端加密快照对任何人（含维护者）都不可解密。
- **API keys are NOT part of the encrypted payload / API Key 不随加密载荷同步**: consistent with the no-key-on-the-server model, keys stay in each device's Credential Manager; a restored device shows every provider as "needs key". 密钥仍各机各存，还原后的设备每个供应商都提示"需要密钥"。
- **Integrity, tamper and rollback / 完整性、防篡改与防回滚**: a wrong passphrase and a tampered byte produce the *same* error (indistinguishable by design). Monotonic `seq` makes a server that replays an older snapshot detectable and blocked by default. 口令错与改一字节产生同一错误（刻意不区分）；`seq` 单调递增使服务器重放旧快照可被检测并默认拦截。
- **Transport / 传输**: `http://` endpoints are rejected by default; plaintext http is allowed only for `localhost` / loopback / RFC1918 private hosts **and** an explicit "allow insecure connection" opt-in. HTTPS is always required for public endpoints. 公网 http 一律拒绝；仅本机/回环/RFC1918 私网且显式勾选"允许不安全连接"才放行 http。
- **Conditional write is best effort / 条件写尽力而为**: WebDAV uses `If-Match`/`If-None-Match` (412 → conflict). Some servers ignore these headers, degrading to an unconditional PUT — data is never lost, but concurrent-overwrite protection is not guaranteed on such servers. S3 falls back to a pre-upload HEAD ETag comparison, which is not atomic. WebDAV 用 If-Match/If-None-Match（412→冲突）；部分服务器忽略该头则退化为无条件 PUT（不丢数据，但并发保护不保证）。S3 降级为上传前 HEAD 比较，非原子。
- **Local DB is intentionally NOT encrypted / 本地数据库刻意不加密**: the SQLite file already contains no keys; encrypting it would require the Credential Manager to be available on every launch for marginal benefit. BitLocker + DPAPI cover the device-loss case. 库内已无密钥；加密它收益极低且会让凭据管理器不可用时应用瘫痪。设备丢失场景由 BitLocker + DPAPI 覆盖。

## Distribution and Signature / 分发与签名

Releases ship an unsigned Windows `.msi`. Because there is no Authenticode certificate (an OV/EV cert or Azure Trusted Signing is not economical for a personal fork), Windows SmartScreen will show an "unknown publisher" warning on first run. To let users verify origin and integrity anyway, each release publishes `SHA256SUMS` alongside a **minisign** signature (`.minisig`); the maintainer's public key is pinned in this file's repository and in the README. Users should download the MSI, verify the SHA-256 and the minisign signature, and only then run it. The **build, release and signing pipeline** is in the scope above.

发布产物是未做代码签名的 Windows `.msi`。个人 fork 承担不起 OV/EV 证书或 Azure Trusted Signing 的费用，因此首次运行 Windows SmartScreen 会给出"未知发布者"警告。为让用户仍能验证来源与完整性，每个 Release 随包发布 `SHA256SUMS` 与 **minisign** 签名（`.minisig`），维护者公钥固定在本仓库与 README。用户应下载 MSI、校验 SHA-256 与 minisign 签名后再运行。**构建、发布与签名链路**始终在上文范围内。

维护者公钥（本仓库根 [`minisign.pub`](minisign.pub)）：

```
untrusted comment: minisign public key 0877A52104C63DB6
RWS2PcYEIaV3CIrxcer+LZzEqHAVEC9r38Y4HIO9ZxCd0mRCTU5tLCQG
```

下载后按序验证（先验签名确认清单来源，再验哈希确认安装包未被替换）：

```bash
minisign -Vm SHA256SUMS -p minisign.pub -x SHA256SUMS.minisig
sha256sum -c SHA256SUMS
```

## Reporting a Vulnerability / 报告漏洞

**Please do NOT report security vulnerabilities through public GitHub issues.**

**请不要通过公开的 GitHub Issue 报告安全漏洞。**

Instead, please report them through [GitHub Security Advisories](https://github.com/stttwq/cc-switch/security/advisories/new).

请通过 [GitHub 安全公告](https://github.com/stttwq/cc-switch/security/advisories/new) 进行报告。

When reporting, please include:

报告时请包含以下信息：

- A description of the vulnerability / 漏洞描述
- **The untrusted source of the input, and the full data path from that source to the affected code** / **输入的不可信来源，以及从该来源到相关代码的完整数据路径**
- Steps to reproduce against a currently supported release / 在受支持版本上的复现步骤
- Potential impact / 潜在影响
- Affected versions / 受影响版本

The data path matters more than the sink. We assess severity by **who controls the input**, not by which API the value eventually reaches — the same function call can be critical or harmless depending entirely on where its argument came from.

数据路径比 sink 更重要。我们按**谁能控制输入**来评估严重度，而非按该值最终抵达哪个 API——同一处调用是严重还是无害，完全取决于其参数的来源。

## Response Timeline / 响应时间

- **Acknowledgment / 确认**: within 48 hours / 48 小时内
- **Initial assessment / 初步评估**: within 7 days / 7 天内
- **Fix for critical issues / 关键问题修复**: within 14 days / 14 天内

## Disclosure Policy / 披露政策

We follow a coordinated disclosure process:

我们遵循协调披露流程：

1. The reporter submits the vulnerability privately. / 报告者私下提交漏洞。
2. We confirm and work on a fix. / 我们确认并修复漏洞。
3. A patch release is published. / 发布修复版本。
4. The vulnerability is publicly disclosed. / 公开披露漏洞详情。

Reporters will be credited in the release notes unless they prefer to remain anonymous.

除非报告者希望匿名，否则将在发布说明中致谢。

### CVE identifiers / CVE 编号

For eligible vulnerabilities, the maintainer may request a CVE ID through a GitHub Security Advisory once a fix is available. Being in scope for this policy and meeting GitHub's CVE eligibility criteria are separate questions, and the second is decided by GitHub as CNA, not by this project.

对于符合条件的漏洞，维护者可在修复就绪后通过 GitHub 安全公告申请 CVE 编号。「属于本策略范围」与「符合 GitHub 的 CVE 分配条件」是两个不同的问题，后者由作为 CNA 的 GitHub 判定，而非本项目。

Severity is scored with CVSS (v3.1 or v4.0), and the vector will reflect any required user interaction or prior local access.

严重度采用 CVSS（v3.1 或 v4.0）评分，向量将如实反映所需的用户交互或前置本地访问条件。

## Security Updates / 安全更新

Security fixes are released as patch versions and announced via [GitHub Releases](https://github.com/stttwq/cc-switch/releases). We recommend always updating to the latest version.

安全修复通过补丁版本发布，并通过 [GitHub Releases](https://github.com/stttwq/cc-switch/releases) 通知。建议始终更新到最新版本。
