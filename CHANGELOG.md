# Changelog

All notable changes to CC Switch will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.3.3] - 2026-09-29

### Security

- **MCP 导入本机审批**：外部导入（SQL / 同步 / 备份恢复）新增或内容变更的
  MCP 条目不再直接获得本机执行授权——启用位为真也只入库待审批，不写入可
  执行的 live 配置；确认框完整展示 command / 逐项 args / cwd / 目标地址，
  env 与 headers 键名可见、值默认掩码。审批绑定内容修订（规范化 JSON 全文），
  内容一变旧批准即失效；审批记录为本机数据，不随导出/同步传播，也不能被
  外部导入覆盖。本机表单保存与本机 live 导入的既有路径不受影响。
- **携凭据的模型获取请求禁止跨目标自动转发**：模型获取改用禁跟随重定向的
  专用客户端（代理选择与全局一致），端点 3xx 跳转一律返回「重定向被拒绝」，
  自定义 API key 头（`x-api-key`、`x-goog-api-key`、自定义头）不再可能被
  发往第二个 host/port；models URL override 跨源时拒绝携带当前 key 发送。
- **网络错误不再把秘密 URL 带回前端**：模型获取错误改为结构化
  `{code, retryable, status?}`，IPC 不再携带附完整 URL 的底层错误文本；
  错误响应体仅进本机 debug 日志（脱敏 + 截断）。前端按 code 映射文案，
  仍可区分认证失败、超时、不支持模型接口、重定向被拒绝。
- **导入归一化：只导入程序认可的数据**：SQL 导入与 `.db` 备份恢复在程序
  自建 schema 的干净库上进行——外部建表 SQL、约束与索引一律不进主库；
  结构审计拒绝未知表、列集合不符与表达式索引等对象，拒绝时不改动主库与本机
  安全状态。历史正常导出与二制备份恢复兼容不受影响，导入本身 0 次 `op`。
- **发布验证闭环**：校验清单与 minisign 签名一律对最终上传名生成；云端补位
  构建收敛权限、校验标签/版本/MSI ProductVersion 一致，且只产出草稿
  Release——不存在签名材料就不能出现无签名的正式发布。发布流程文档新增
  干净目录回验步骤。
- **`op` 排队预算与资源清理**：并发 `op` 有界（饱和返回可重试 busy），
  排队等待单独计时（不再冒充执行超时）；stdout/stderr 按操作类型限额读取，
  超限 kill 并回收；stdin 秘密副本用 `Zeroizing` 尽早清零；读写失败不再
  假装成功。
- **模型获取有限响应**：响应体按真实字节流式限额（成功 4 MiB / 错误正文
  64 KiB），无 Content-Length 与 chunked 同样受限；模型条目数与单条 ID
  长度受限；一次「获取模型」跨候选共享 20 秒总预算，超限明确报错而非
  伪装空列表。

### Changed

- **Claude 表单「获取模型」请求代际隔离**：endpoint / key / override 变化、
  切换供应商或卸载时作废在途请求，旧响应不再回灌 UI 或清掉新请求的
  loading（Codex 同款缺口一并补齐）。
- 同步导入采用意图式恢复阶段机：Skills 备份与恢复日志持久化在应用数据根
  （私有 ACL），commit marker 随主库替换原子生效；启动时按阶段幂等恢复或
  明确 needs_attention，不自动访问 1Password、不自动批准 MCP、不重放旧快照。
- 安全说明（SECURITY.md）与代码实现对账：区分严格 / 兼容投递与 1P 模式、
  secret_refs 受限传递策略、E2E 口令在 1P 后端的存放位置、条件写「尽力而为」
  的真实边界，并新增数据位置矩阵。

### Fixed

- **编辑已存供应商时「获取模型」不再要求重新输入密钥**：表单 API Key 留空
  （保留语义）时改由后端按供应商身份从凭据后端解析已存密钥后发起请求——
  明文密钥不过前端 IPC（2.3.2 起「显示」明文不回填表单后，编辑态获取模型
  恒提示「请先填写 API Key」）；凭据后端锁定时明确提示可重试或手动填写。
- **同步中断恢复**：Skills 已替换、DB 提交前后、后处理中途的进程强杀不再
  留下不一致状态——重启后自动恢复到同一确定状态或明确暂停等待人工处理；
  DB 已提交时绝不重新导入覆盖用户后续编辑。
- 基线 `2.3.2` 之后的安全加固全部详见
  `docs/plans/post-2.3.2-optimization-security-hardening-plan-2026-09-29-zh.md`
  施工记录（S0–S5）。

## [2.3.2] - 2026-09-29

### Security

- **1Password 条目归属核验**（SEC-01/02）：按 item ID 的读取、改名、删除与删除的
  标题兜底统一走安全定位器并核验 `cc-switch-group` 归属——被构造的导入引用指向
  同保险箱其它条目时，不再可能读出、改写、改名或归档他组条目；标题兜底不再凭裸
  显示名认领其它应用的同名条目。
- **孤儿清理改按真实引用判定**（SEC-03）：在用条目识别与显示标题解耦，改用本机
  `secret_refs` 的真实 item ID 集合；"无本地引用"不再自动等于孤儿，提交清理时
  后端重新核验在用状态，新标题在用条目不会被误归档。
- **SQL / 备份导入 schema 审查**（SEC-05）：外部 SQL 与二进制备份中的触发器、
  未经声明的 view 等可执行 schema 对象一律拒绝——不再可能借导入在暂存库回填
  本机引用/设备设置阶段篡改数据；正常历史导出不受影响。
- **`op` 调试输出脱敏**（SEC-04）：废弃 `CC_SWITCH_OP_DEBUG` 环境变量开关（存在
  即透出原始 args/stderr）；调试输出改编译期限定并脱敏（会话秘密、URL userinfo、
  疑似令牌），发布构建绝不打印。
- **依赖升级**：`smol-toml` 1.4.2 → 1.9.0（GHSA-7w5x-hrqm-74c2，high，前端解析
  Codex TOML 可达）；pnpm 生产依赖 high+ 审计升级为 CI 交付门禁，CI 覆盖
  `1password` 分支。

### Changed

- **编辑供应商按需访问 1Password**：仅修改模型、推理等级、备注、图标、排序等
  非凭据字段时全链路 0 次 `op` 调用（1Password 锁定状态也可正常保存）；名称、
  Base URL、API Key 的有效变化才进入凭据更新——API Key 改为显式 keep / set /
  clear 意图（空白输入框是保留、清除走独立按钮），多字段变化合并为一次条目
  更新（1 次 get + 至多 1 次 edit），未变字段全部保留。
- **保存失败分阶段报告**：1Password 已更新但本地保存失败、配置已保存但 live
  应用失败等部分成功场景如实报告阶段；「重试」只重做必要的本地保存/投影，
  不会用过期凭据意图覆盖远端新状态，也不会重复修改 1Password 条目。
- 端点缓存改在 1Password 写入成功之后维护：普通 → 敏感 URL 切换时删除旧缓存，
  显式清除端点同步清缓存，不再留下「新缓存、旧保险箱」的不一致状态。

### Fixed

- `op` 子进程超时覆盖盲区：截止时间提前到进程启动之前，stdout/stderr 读取先于
  stdin 写入并发进行——子进程不读输入、向 stdout 灌大输出时不再可能互等死锁；
  stdin 未写完且子进程「成功」退出时显式报错而非假装成功。
- 修复先写端点缓存再访问 vault 的写入顺序缺陷：1Password 失败不再留下新缓存、
  旧保险箱的不一致状态。

## [2.3.1] - 2026-09-28

### Fixed

- **1Password 条目标题改为「应用前缀/供应商名」**（方案 B 增补）：新标题形如
  `pi/OpenRouter`、`claude/My Provider`，用应用前缀区分不同应用的供应商，又不再
  回落成 `cc-switch/pi/<provider_id>` 的长串。
  - 修复根因：新建供应商写 vault 在入库之前，标题查库必失败、恒落旧格式长标题——
    现由保存流程把供应商显示名作为标题提示直传 `put_titled`，新建即得短标题。
  - 读取定位候选标题依次为新格式 → 过渡格式（裸显示名）→ 旧格式，存量条目全部可读。
  - 设置 → 凭据存储维护新增「对账条目标题」：一次性把存量条目改名为当前首选标题。
- 设置界面「CC Switch 配置目录」改显后端真实解析目录：此前前端自算 `~/.cc-switch`
  展示，与 MSI 安装版实际的「安装目录 data」不一致，造成"还在旧版本目录"的误解。

## [2.3.0] - 2026-09-26

### Added

- 新增可选的 **1Password 凭据后端**：运行时通过本机 1Password CLI（`op`）按需取钥匙，
  本地数据库/内存/凭据管理器/注册表都不再留任何钥匙明文。
  - 设置 → 高级 → 1Password：显示状态（安装/登录/op 版本/路径/签名校验）、选择账户与
    vault、测试取钥匙、一键「迁移到 1Password」。
  - 迁移向导把凭据管理器里的 `cc-switch/*` 条目读出 → 写入 1Password → 回读校验 →
    删除凭据管理器条目并清理注册表投递；先写后删、可中断可重跑。
  - `op.exe` 默认校验 Authenticode 签名（主体须为 AgileBits），失败拒用。
  - 1Password 模式下强制严格投递（钥匙绝不写 `HKCU\Environment`）；后台自动同步跳过
    （避免周期性解锁弹窗），手动同步照常；便携包导出隐藏（1Password 自带跨设备同步）。
  - **base_url 随整包进 1Password**（D3-B）：非敏感 URL 以可见字段、带凭据的 URL 以
    隐藏字段存入条目，1Password 成为钥匙与端点的持久真源；本地端点表降级为读取缓存
    （命中 0 次 op），数据库重置 / 换设备后「从 1Password 重建引用」可整体恢复。
  - **条目标题 = 供应商显示名**：改名后下次保存自动同步；`provider_id` 移入条目内的
    `cc-switch-group` 字段，「重建引用」按该字段识别归属，同名校验不再误绑。

### Changed

- 凭据读写重构为「按供应商/应用整包」一次往返（`SecretVault`），列表加载与切换在
  1Password 模式下不再逐字段触发解锁。
- 新增本地 `secret_refs` 引用表（只存字段名，不存值）：列表徽标、缺钥匙校验、删除定位
  全查该表，零后端往返。
- 凭据后端读取失败（锁定/断网/取消授权）一律明确报错，绝不静默降级为「没有钥匙」。

### Fixed

- **1Password 模式接入审查修复（2.3.x 加固）。** 对 `main...1password` 全量 diff 的审查发现 8 个 P0 问题，全部修复：
  - `base_url` 不再进出保险箱：非敏感 URL 存本地端点表（随云同步、零 op 读取），带凭据的 URL 仍存 vault；Codex 写 live、列表卡片、Pi 投递统一走 `resolve_base_url`，杜绝「从已清空的凭据管理器读 base_url」。
  - `OnePasswordVault::put` 改为原子的「读取 → 就地编辑」（`op item edit` 整份 JSON 走 stdin），不再「先归档删除、再新建」；item id 保持稳定，归档里不再堆积旧钥匙副本。
  - 编辑供应商时数据库写失败不再删除该供应商已有钥匙（仅新增路径保留归档回滚）。
  - Pi `models.json` 里的明文 key 在 1Password 模式不再被静默丢弃：保留原文件、记入待导入清单，用户确认后一键导入。
  - 堵住明文回流 Windows 凭据管理器的路径：SQL 导入 / 备份恢复 / 云同步下载 / 便携包导入在 1Password 模式改走 vault，写入失败保留明文并提示重试；便携包导出由后端直接拒绝。
  - 迁移提交后旧后端立即失效（返回 `vault_restart_required`），前端迁移成功后强制重启，杜绝钥匙写回凭据管理器。
  - 启动剥离改为「就地、定点、有备份才剥」：不再整文件重写 live（用户在 CCS 外的改动不再被吞），`auth.json` 只在有备份时删 `OPENAI_API_KEY`。
  - `op.exe` 签名校验只认签名者证书（O=Agilebits 完整匹配），不再遍历证书包子串匹配；校验失败绝不执行任何 op 子进程。
  - 其余健壮性加固：stderr 分类先剔除回显、状态探测不抢全局锁、后端判定 fail-closed、`secret_refs` 不随云同步（提供「从 1Password 重建引用」）、注册表清理保留登记等。
  - 读写按 `secret_refs` 的 item_id 直达（标题只作兜底），归档条目一律视为不存在；迁移用本地标记判定「已迁移」，每组上报进度。
  - 可能触发 op 的 Tauri 命令全部改 async + `spawn_blocking`；`ccs env` 退出码分级（6 = vault 锁定，7 = 网络/超时）。
- **多设备与同步数据边界（本节所有改动要求所有设备同时升级 2.3.0+；旧客户端下载新快照会丢自己的设备本地项）。**
  - 云同步与手动 SQL 只搬运**配置**：按数据分级裁剪——设备本地项（代理、日志、迁移状态等）不再随同步扩散；1P 模式下端点缓存与 B 级键不导出，凭据管理器模式端点照旧随同步；1P 引用只带真实迁移行，且仅在导入方为同一 vault 时采纳（其余按「未关联」提示，从 1Password 一键关联）。
  - 上传前的「本机状态过期」检查：远端有本机尚未下载的更新时拒绝覆盖（`remote_ahead`），可选「先下载（推荐）」或「强制覆盖」；S3 / WebDAV、v2 / v3 行为一致，S3 补齐 legacy 布局回退、有效 `dbCompatVersion`、`sourceLayout` 与加密降级检查。
  - 回声抑制全局化：下载/导入触发的后处理写入不再被自动同步当成用户改动上传（跨传输亦然）；回滚冲突时跳过后处理。
  - Pi：列表查询节流、`models.json` 指纹短路（未变化不重同步）、下载后「已启用且有变化」的模型设置写回 `models.json`；Pi 外部修改 baseUrl 在 1P 模式推迟到下次主动 op 动作时写回。
  - 手动 SQL 导入改为「预览 → 确认」两步：先显示来源设备 / 导出时间 / 导出方后端 / 引用条数，确认后才执行；导入前自动备份；存在未导入完的明文钥匙时拦截导入并提供「重试导入」。
  - SQL 导出体验：私有权限写入、默认文件名 `cc-switch-config-{日期}.sql`、按凭据模式区分导出说明；数据库备份不含钥匙的说明与 `pre-secrets-migration` 备份清理豁免。
  - 1P 模式「有未上传改动」提示（自动同步停用时）；端点对账诊断增补 vault id 有效性校验（settings 的 vault 值损坏可被检出）；端点回填遇带凭据 URL 时记入本机清单并从待办排除。

## [2.2.9] - 2026-09-25

### Added

- **Encrypted credential bundle for moving credentials between machines.** Credentials live only in Windows Credential Manager and are never part of the WebDAV/S3 payload, so uninstalling deletes them for good — "uninstall + reinstall + sync" used to lose every API key and base URL permanently. Settings → Advanced → Credential manager maintenance now offers **Export credentials** / **Import credentials**: every `cc-switch/*` entry is sealed into one file with a passphrase (20-character minimum) using the same Argon2id + XChaCha20-Poly1305 construction as end-to-end sync, in a format of its own so the two payloads cannot be mistaken for each other. On import the bundle wins over local values and entries that exist only locally are kept. Neither plaintext credentials nor the passphrase ever appear in the file.

### Fixed

- **Full uninstall now clears the data CC Switch writes outside its own directories, so a reinstall no longer resurrects old providers.** Uninstall previously removed only the install-directory `data` and the legacy `%USERPROFILE%\.cc-switch`; it left behind `~/.pi/agent/models.json` (Pi's native config, which startup re-imports as live providers), every `cc-switch/*` Windows Credential Manager entry, and the `CC_SWITCH_*` variables in `HKCU\Environment`. The new `ccs --cleanup-user-data` step runs on full uninstall and removes exactly what CC Switch manages: Pi `models.json` nodes are removed only when their `apiKey` references `$CC_SWITCH_PI_*`, so providers you configured yourself are preserved.
- Uninstall also clears `env_key = "CC_SWITCH_CODEX_API_KEY"` from `~/.codex/config.toml`. The env var it points at is removed by the same uninstall step, so leaving the reference behind would break Codex until CC Switch was reinstalled. Because dropping the `env_key` removes the provider's credential short-circuit, `requires_openai_auth` is set to `false` at the same time — otherwise Codex would fall back to the official OAuth login in `auth.json` and send those credentials to a third-party endpoint. A user-authored `env_key` is left alone.
- The provider imported from live config on a fresh install is no longer displayed as the literal `default`. Its id stays `default` (credential targets, backfill protection, and history sync are keyed on it), but the card now shows a readable name via the i18n layer.
- Windows Credential Manager entries can now be enumerated (`SecretStore::list_targets` previously returned an empty list on the Windows backend). The encrypted credential bundle and the uninstall cleanup both depend on it.

## [2.2.8] - 2026-09-25

### Fixed

- **Windows MSI upgrades no longer fail with error 1926 ("failed to set file security").** The installer is `perUser`/`limited` (non-elevated), and Windows Installer writes rollback backups (`Config.Msi\*.rbf`) on the drive where the app is installed. On a non-system drive, MSI resets that drive's `Config.Msi` ACL to SYSTEM + Administrators only, so the non-elevated install process cannot set the backup file's security descriptor and reports 1926 — surfacing as a permission prompt on every upgrade. Rollback is now disabled in the package (`DisableRollback`), so no `.rbf` files are created and the error is gone.
- MSI bundling no longer fails with `CNDL0107`/`LGHT0094`: the uninstall cleanup custom action referenced an undeclared `SystemFolder` directory.
- The three uninstall-cleanup custom actions now use `Execute="immediate"` instead of `deferred`. Deferred actions generate rollback scripts in `Config.Msi`, which is the same non-elevated path that produced 1926. These actions were already `Return="ignore"` (no rollback semantics), so nothing is lost.

## [2.2.7] - 2026-09-25

### Changed

- Windows MSI installs now store CC Switch data under the install directory. Existing user-profile data is copied on first launch and retained until full uninstall; upgrades preserve data, while full uninstall removes install-local data and the legacy default user-profile data directories. Custom data directories are left untouched.

## [2.2.6] - 2026-09-25

### Fixed

- Prevented asynchronously loaded custom terminal paths from being replaced by an empty input draft after upgrading.

## [2.2.5] - 2026-09-24

### Fixed

- Pi terminals now receive credentials for every enabled provider, allowing provider switching within a session.
- Settings saves preserve terminal preferences omitted by stale or older forms; custom terminal launch failures now report an error instead of silently opening cmd.

## [2.2.4] - 2026-09-24

### Fixed

- **Right-click "Open here" now works with non-ASCII (e.g. Chinese) folder paths (Windows).** The launcher batch file is written as UTF-8, but `cmd.exe` parses it in the console code page (GBK/936 on Chinese Windows), so a `cd /d "<Chinese path>"` line was misread and the terminal failed to switch into the clicked folder (surfacing as errors such as `'/d' is not recognized` / "path not found"). The target directory is now passed through an environment variable (`%CC_SWITCH_CWD%`, delivered to the child process as UTF-16), leaving the batch file pure ASCII so any path resolves correctly.
- **Pi right-click launch resolves a provider even without a native default.** Pi runs in additive mode with no cc-switch "current provider", so the context-menu launcher relied solely on Pi's native `defaultProvider` in `settings.json` and failed with "no default provider" when it was unset. Launching a Pi provider from the GUI ("Run Pi" / "Open Terminal") now writes it back to Pi's `defaultProvider` (preserving the file's other fields, idempotent), so the right-click menu and Pi itself both follow your most recent choice; when exactly one Pi provider exists it is used automatically.

### Changed

- **Strict credential delivery now defaults to global-strict.** Fresh installs (and configs missing the field) no longer write provider secrets into `HKCU\Environment`; credentials are injected only into terminals launched from cc-switch or activated via `ccs env`. Existing settings that already recorded the switch are left untouched.

## [2.2.3] - 2026-09-24

### Fixed

- **Explorer right-click menu self-heals on startup (Windows).** After an app update, an already-registered context menu could keep pointing at a stale command line (old launcher path / old command format), leaving the menu broken until re-toggled. Startup now silently rewrites any expired command line (and refreshes the icon) for existing registry entries only — unregistered users and fresh installs are untouched. Failure is logged and never blocks launch.

## [2.2.2] - 2026-09-24

### Added

- **Explorer right-click "Open terminal here" (Windows).** An opt-in Settings toggle registers a cascading folder context menu (Claude / Codex / Pi) under the current user's registry — no admin required. Clicking an entry runs that CLI in the clicked folder with the current provider's credentials injected (same boundary as "Open Terminal": env-only, never `HKCU\Environment` or a file), skipping the open-main-window-then-pick-folder flow. A dedicated windowless launcher binary (`ccs-open.exe`, built with `windows_subsystem = "windows"`) avoids the console-window flash, and the entries run the CLI directly instead of just opening a shell. The MSI removes the menu keys on a true uninstall (not on version upgrades) via a conditioned custom action.

## [2.2.1] - Unreleased

### Added

- **Custom terminal for "Open Terminal" (Windows).** The preferred-terminal dropdown gains a "Custom terminal" option backed by two new settings — an executable path and an argument template. The template's `{bat}` placeholder expands to the launcher batch script; quoting is honored when splitting (`-e cmd /K "{bat}"` passes four arguments, which Pebrel/WezTerm/Alacritty-style variadic `-e` requires). Leaving the template empty defaults to `-e cmd /K "{bat}"`. Launch failure falls back to cmd, and credentials still enter the child process only via the environment.

## [2.2.0] - Unreleased

Strict-mode ergonomics: activate credentials in your own shell, a tiered strict-delivery switch, and a fixed "Open Terminal" that lands Codex/Pi in a real shell.

### Added

- **`ccs env <app>` shell shim (P1).** A new console sub-binary (`ccs.exe`, built from a dedicated `[[bin]]`, no tauri/single-instance) lets you activate the current provider's credentials inside _your own_ PowerShell / cmd / Git Bash: `ccs env claude | iex`, `eval "$(ccs env claude --shell bash)"`. Keys go only into that shell process (same security boundary as the terminal injection — never `HKCU\Environment`, never a file). `--clear` emits only unsets and synchronously deregisters from `managed_env_vars` so re-activation isn't falsely blocked. Credentials reuse `provider_env_pairs`; the shim refuses to migrate a schema that is newer/older than itself and resolves the custom config-dir override from `app_paths.json`. A "Copy activation command" button in Settings emits a one-time absolute-path snippet (does not touch PATH). Stable exit codes: 0 ok, 2 usage, 3 missing key, 4 DB version, 5 store/config unavailable.
- **Tiered strict-delivery mode (P2).** The global bool becomes three states — Off / Per app / Global — via an additive `env_delivery_strict_apps` list (old `env_delivery_strict_mode` still honored). Delivery/preflight now decide per app (`strict_for`), enabling a mode only reclaims the variables of apps that just turned strict, and the tray hint / diagnostics / provider-card badge now aggregate instead of reading the raw bool. The mutual-exclusion invariant is enforced at the single always-on save path.
- **"Run X" entry + real interactive shell (P3).** The CLI name is now chosen by app (shared `cli_command_for`), so Codex/Pi launch the correct CLI. "Open Terminal" lands an environment-loaded interactive shell without auto-running a CLI; a new "Run X" action starts the app's CLI directly. The terminal button is no longer Claude-only.

### Changed

- Codex/Pi provider cards now show the "Open Terminal" / "Run X" actions (previously only Claude).

### Notes

- Shipping `ccs.exe` inside the MSI is a packaging step that must be verified against a real per-user installer build (see the plan's open point ①).

## [2.1.0] - 2026-09-21

End-to-end encrypted cloud sync, a strict credential-delivery mode, one-click diagnostics, and a large Windows-only repo/security hardening pass (on-demand key reveal, IPC input tightening).

### Added

- **Reveal a provider's API key on demand.** Editing a provider now shows an eye button that reads a single field's key from Credential Manager once (`reveal_provider_secret`), unmasks it in place, and re-masks on blur or after 60 s. Batch reads (list/cards/tray) still never carry a key — the frontend is zero-secret by default, not zero-secret ever.
- **Base URL is back-filled and visible.** The Base URL edit box now defaults to the current value per app (Claude `env`, Codex TOML `base_url`, Pi top-level), and provider cards show the active endpoint's host so you can tell which endpoint you are on without opening the editor.
- **End-to-end encrypted sync (E2E).** Opt-in per transport (WebDAV / S3). A user passphrase (Argon2id → KEK) seals a per-snapshot data key (XChaCha20-Poly1305) that encrypts `db.sql` and `skills.zip`; the AAD binds `snapshotId + seq`, the inner manifest (device name / time / plaintext hash) is itself encrypted, and a monotonic `seq` blocks rollback. The server only ever sees ciphertext and an opaque `snapshot_id`. The passphrase is stored in Credential Manager and **never uploaded**; losing it means the remote is unrecoverable. Keys are not part of the synced payload, so a restored device shows "key required". v3 lives in a separate `{root}/v3/{profile}` layout — 2.0 clients ignore it, and enabling E2E refuses to downgrade to v2 plaintext.
- **Conditional writes for concurrency.** WebDAV manifest uploads use `If-Match`/`If-None-Match` (412 → "remote changed"); S3 does a best-effort HEAD compare. A rollback conflict returns a structured payload so the UI can offer an explicit "apply anyway".
- **Strict credential-delivery mode (B5).** A global switch (off by default) that stops writing secrets into `HKCU\Environment` on provider switch — keys are injected only into terminals launched from cc-switch (`Command::env`). Enabling it immediately reclaims any already-delivered keys. Off-machine CLIs then fail closed (Codex missing `env_key`, Pi unresolved vars); provider cards show a "strict delivery" badge.
- **One-click diagnostics.** The About page copies a redacted bundle — version, DB schema, migration markers, key-target count, strict-mode state, `crash.log` presence, and up to the last 50 log lines run through known-secret redaction with all `http(s)` hosts/paths masked — with no keys, provider names, base URLs, or WebDAV/S3 endpoints.

### Changed

- `get_providers` is now async and no longer reads Credential Manager per provider on the main thread.
- Credential principle updated from "frontend zero-secret" to "frontend zero-secret by default + explicit on-demand reveal"; documented in SECURITY.md.

### Fixed / Security

- **IPC input surface tightened (S-1/S-2/S-4):** `get_session_messages`/`delete_session` confine `sourcePath` to the provider root; config import/export go through a native dialog so the path never round-trips through the renderer; `open_external` parses the URL and allows only `http`/`https`.
- **HTTP stack (S-8):** reqwest no longer pulls `native-tls`/`schannel`; it uses rustls against the **OS certificate store** (`rustls-tls-native-roots`), which also fixes a real-machine `UnknownIssuer` failure downloading skills behind a TLS-intercepting proxy. A single reqwest version now appears in the shipped target.
- Bumped transitive deps (rustls, rustls-webpki, h2, anyhow, uds_windows) to clear five RustSec advisories.
- **Sync transport security.** `http` remote endpoints are refused by default — only loopback / private-network URLs pass, and only when "allow insecure" is checked; existing http remotes configured before the upgrade get a one-time grandfather so they aren't silently cut off. Wrong passphrase and tampered ciphertext are deliberately indistinguishable (fail closed, no oracle).
- **Provider-switch delivery is now transactional.** The old value of each environment variable is snapshotted (`Zeroizing`) before the delete-then-write sequence; if writing any new variable fails, the switch rolls back — previously written vars are removed and the old values restored — instead of leaving a half-applied "old deleted, new not written" state.
- **DB robustness (T-4/T-5).** `busy_timeout = 5000` and a startup `PRAGMA quick_check` with an offline "restore from latest backup" path (WAL intentionally not enabled — it corrupts on cloud/NAS/WSL2-UNC sync dirs); a `pre-sync-restore-<ts>.db` file backup is taken before applying a synced snapshot.
- **Tighter local ACL (S-10).** On first run, `~/.cc-switch` is locked down to the current user + SYSTEM + Administrators (idempotent `icacls`).

### Removed / Cleanup

- **Windows-only code (C5):** deleted `linux_fix`, the macOS-only session-terminal subsystem and `launch_session_terminal`, non-Windows `cfg` branches in `misc.rs`/`lib.rs`/`tray.rs`/`lightweight.rs`/`auto_launch.rs`, the `webkit2gtk`/`libc`/`objc2` dependencies, and the iOS/Android/macOS icons and `Info.plist`.
- **Repo identity:** README, CHANGELOG, CONTRIBUTING/SUPPORT/CODE_OF_CONDUCT/CODEOWNERS, issue templates and docs now point at this fork instead of upstream `farion1231`/`ccswitch.io`; upstream 3.x history moved to `docs/changelog-upstream-3.x.md`; 92 upstream release notes and the en/ja manuals removed; in-app copy no longer describes removed apps (Gemini/Claude Desktop) as managed.

### Build / Release

- All GitHub Actions pinned to commit SHAs; added a weekly `cargo-deny` + `gitleaks` + `pnpm audit` workflow and a blocking `cargo deny check advisories` gate on the main CI backend job.
- Releases ship `SHA256SUMS` + a **minisign** signature (public key committed as `minisign.pub`); SECURITY.md rewritten for 2.x.

## [2.0.1] - 2026-09-20

Fixes from the first real-machine upgrade rehearsal (3.20.3 → 2.0.0), recorded in `docs/plans/secrets-slimdown-acceptance-zh.md`.

### Fixed

- **Editing the provider you are currently using now takes effect immediately.** Credentials were delivered to `HKCU\Environment` only when switching providers, so saving a new key or base URL on the active card left Claude Code, Codex and Pi reading the previous key. The save path re-delivers now — before anything is written to the database, so a failed delivery no longer leaves a card that says "save failed" while actually being half-saved.
- **Cloud sync is no longer refused by the export guard.** Base URLs were registered among "secrets seen this session", so any payload mentioning the same domain as a provider's website was rejected with 「导出护栏拒绝」. A URL that embeds `user:password@` is still treated as a secret.
- **Re-upgrading after a rollback no longer deadlocks.** With the pre-migration database restored, the environment variables left by the previous install were judged foreign and refused, so the live rewrite failed on every launch and the dialog blamed a running CLI. The rewrite step now adopts variables that follow our own naming before it delivers.
- **Outstanding live rewrites are visible after the migration dialog is dismissed.** That one-time dialog was the only place reporting them. Settings → Advanced → Credential Manager maintenance now lists the pending rewrites with their reasons and offers a retry that runs immediately instead of waiting for the next launch.

## [2.0.0] - 2026-09-19

**This is a breaking release.** It removes the local router, six of the nine supported apps, the auto-updater and every form of plaintext credential storage. Existing installations migrate automatically on first launch, but the shape of the product changes: cc-switch no longer proxies requests, no longer stores keys on disk, and no longer builds for macOS or Linux.

### Removed

- **Local proxy and everything built on it**: `src-tauri/src/proxy/**` with request forwarding, failover queues, the usage dashboard, stream checking, model pricing and the routing takeover. No listening port is opened any more and no request is forwarded.
- **Seven applications**: Gemini, OpenCode (and OMO), OpenClaw (and Workspace), Hermes, GrokBuild (and xAI OAuth), Claude Desktop and Copilot are no longer supported. Only Claude Code, Codex and Pi remain. Providers, MCP servers and skills belonging to the removed apps are dropped from the database by the v19 migration.
- **Auto-update**: `tauri-plugin-updater` and the release-chain signing and `latest.json` artifacts are gone; the app no longer checks for or installs updates. `latest.json` / `.sig` assets are no longer published.
- **macOS and Linux builds**: releases ship a single Windows x86_64 MSI. The macOS/Linux build jobs, their packaging steps and the corresponding release notes are removed.
- **The "Gemini Native" upstream format for Claude providers**: protocol conversion left with the local router, so the option could only ever produce a card that does not work. The preset and the selector are gone; the v19 migration normalizes existing cards to OpenAI Chat Completions and the migration report asks you to verify the endpoint yourself.
- **The Codex advanced knobs that only fed format conversion**: prompt-cache routing mode, the Chat-Completions reasoning capability map (thinking / effort switches) and the Claude-emulation and `max_output_tokens` overrides are removed, along with their 16 locale strings. Nothing in the app read them any more, so flipping them silently did nothing; the model catalog and model-mapping settings are untouched.

### Changed

- **Credentials now live in Windows Credential Manager.** API keys, base URLs, Pi request headers, and the WebDAV / S3 sync credentials are stored there and nowhere else. `providers.settings_config`, `settings.json` and the live CLI config files no longer contain values.
- **Environment variables are the only delivery path.** Claude Code, Codex and Pi receive their credentials as user-level environment variables (`HKCU\Environment`); live config files contain only the _names_ of those variables. The helper-command delivery modes (`apiKeyHelper`, Pi's `"!command"`) are not implemented by design.
- **Database schema v19.** Ten tables tied to the removed features are dropped, `providers.in_failover_queue` is dropped as a column, and `meta.usage_script` is stripped on the Rust side.
- **Existing installations migrate automatically and idempotently on first launch.** Provider credentials are extracted, written to Credential Manager, and the stored configs are rewritten with the secrets removed. No interaction is required; a report lists what was migrated and offers a retry for any live-file rewrite that failed.
- **Codex official cards no longer carry a ChatGPT login.** OAuth tokens found in stored configs are discarded rather than migrated — run `codex login` in the Codex CLI instead. Codex third-party cards that would silently fall back to `auth.json` are refused at switch time.
- **Listing providers never reads Credential Manager for keys.** The last-4-character hint is gone: showing it meant one credential read per provider on every list refresh. Cards and forms now say only whether a key is configured, which is derived from the credential registry instead.

### Fixed

- **WebDAV and S3 credentials are actually persisted.** The settings dialog used to place the password inside the settings object while the backend struct no longer had that field, so the value was dropped by serde and the command's `password` argument was always `None`; new sync configurations could never authenticate. Credentials are now sent as a dedicated three-state argument (absent = unchanged, empty = delete, value = store), and "Test connection" can use a password that has not been saved yet.
- **Keyless Codex official cards no longer leave OAuth tokens in SQLite.** The migration skipped rows with nothing to migrate, so a card holding only `auth.tokens` kept its refresh token in `providers.settings_config` forever — and the pending flag was already cleared, so it was never retried.
- **The database file no longer retains plaintext in free pages.** Overwritten `settings_config` values stayed readable in SQLite free pages after the migration; the migration now enables `secure_delete` before rewriting and runs `VACUUM` afterwards.
- **Sync credentials can be cleared.** Emptying the password or S3 key field now deletes the stored entry instead of being ignored.
- **The legacy official-proxy route is cleaned up.** Older versions pointed Codex at `http://127.0.0.1:<port>`; that dead table and its `model_provider` selector are now stripped on every live write instead of being written back.
- **`~/.claude/settings.local.json` conflicts are reported.** Its `env.ANTHROPIC_*` entries override the process environment, so the conflict scan lists them as read-only warnings. cc-switch does not modify that file.
- **Missing required credentials are rejected before saving.** Claude providers need an API key and Pi providers need a base URL (official and Bedrock/cloud-provider cards, which authenticate elsewhere, are exempt).

### Security

- Plaintext residue from older versions is deleted automatically at startup (`config.json`, its `.bak`/`.migrated` siblings, `codex_oauth_auth.json`, `backups/env-backup-*.json`, and legacy `%TEMP%\claude_*.json` launchers). Database backups are never auto-deleted.
- Exports, backups and sync payloads are scanned for credential patterns and refused if a plaintext secret is found; imported/restored data is scrubbed through the same extraction path.
- Known secret values are redacted from logs and from every text that leaves the backend; only a last-4 hint crosses IPC.
- `~/.codex/auth.json` is deleted only when it holds nothing but an API key that has already been migrated to Credential Manager.

## 更早版本

3.x 及更早版本为 fork 之前的上游历史，完整记录见 [`docs/changelog-upstream-3.x.md`](docs/changelog-upstream-3.x.md)。
