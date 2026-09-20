# CC Switch 安全改造与瘦身实施方案 · 第三次全量重核审查意见

- 审查对象：`docs/plans/secrets-credential-manager-slimdown-plan-zh.md`（2026-09-13 版，基线 cc-switch 3.20.3 / `SCHEMA_VERSION = 18`）
- 审查基线：`HEAD = 4961510`，工作树对被审路径干净；tag `pre-slimdown-baseline` = `465bd54`
- 审查范围：§3 原则、§5 目标架构、§6 自动迁移、§7 删除面、§8 加固清单、§9 阶段验收、§10 测试策略、附录 A/B/C/D/E —— **从头到尾全部重跑**
- 方法：分区并行读码核对 + 高危结论逐行复核（本文所有 P0/P1 与"已核实"标注项均由审查人亲自读到代码行）
- 日期：2026-09-18

---

## 0. 总体结论

前两轮（2026-09-17 / 09-18）修复的缺口都还在，§7 删除面与 §8 的 S1–S12 基本收净，IPC 层未发现"密钥值过 WebView"的通路。本轮新查出：

| 级别 | 数量 | 性质 |
|---|---|---|
| P0 | 2 | 一条功能断链（同步凭据存不进去）+ 一条明文残留（迁移漏行） |
| P1 | 4 | 计划明文要求但完全没碰 |
| P2 | 8 | 实现与计划不一致，需要口径裁定 |
| P3 | 13 组 | 删除面残渣 / 死代码 / 死键 / 文档漂移 |
| 门禁 | 4 | 计划假设的测试门禁实际不存在或未接入 CI |

一句话：**架构与护栏是对的，缺口集中在"一次性迁移的边界情况""同步凭据写入的最后一公里""Phase 4/6 的验收证据"三处。**

---

## 1. P0-1 · WebDAV / S3 凭据根本存不进去（功能失效）

**计划要求**（§5.2.5 / L264）：前端传 `password?: string | null` 三态；后端只在有值时写凭据管理器。

**现状（已逐行核实）**

| 环节 | 证据 |
|---|---|
| 前端把密码塞进 `settings` 对象 | `src/components/settings/WebdavSyncSection.tsx:463` `password: passwordTouched ? form.password : ""`；S3 同构 `:677-678` |
| 前端 invoke 只发两个参数 | `src/lib/api/settings.ts:147-155`（`webdav_sync_save_settings` 发 `{settings, passwordTouched}`）、`:183-191`（`s3_sync_save_settings` 同） |
| 后端结构体已无该字段 | `src-tauri/src/settings.rs:131-132`（注释 `password moved to SecretStore`）、`:205-207`（S3 两键）；全仓未开 `serde(deny_unknown_fields)` → **多余字段被静默丢弃** |
| 命令形参恒为 None | `src-tauri/src/commands/webdav_sync.rs:160-166` `password: Option<String>` → `extract_webdav_password` 在生产路径**永不执行**；`src-tauri/src/commands/s3_sync.rs:153-170` 同 |

**后果**

1. 新用户填的 WebDAV 密码 / S3 AK+SK 永远进不了凭据管理器，同步功能不可用。
2. `webdav_test_connection`（`commands/webdav_sync.rs:92-105`）与 `s3_test_connection` 只从凭据管理器读 → **首次配置必然连不上**；其 `preserveEmptyPassword` 入参被 `let _ =` 忽略。
3. 现有测试 `commands/webdav_sync.rs:400-445` 直接调 helper、绕过命令签名，因此静态调用图上的断链没有任何测试能发现。
4. 附带：`passwordTouched=true` + 清空输入框**也删不掉**已存凭据 —— `src-tauri/src/secrets/sync_secrets.rs:17-19,41-51` 对空串直接 `return Ok(false)`，不 delete。

**要动**：`src/lib/api/settings.ts`、`WebdavSyncSection.tsx`、`src/types.ts:197-220`（从 TS 类型里去掉 `password`/`accessKeyId`/`secretAccessKey`，避免再被误塞进 `settings`）、`secrets/sync_secrets.rs`（空值→`delete`）、`commands/{webdav_sync.rs,s3_sync.rs}`，并补一条**经命令入参**的端到端测试。

---

## 2. P0-2 · 迁移漏掉"空 secrets 行"，OAuth token 永久留在 SQLite 明文

**计划要求**（§6.2-4 / L369）：单个 SQLite 事务 `UPDATE providers SET settings_config = ?stripped WHERE id = ?` —— **全部行**。

**现状（已核实）**

- `src-tauri/src/secrets/migration.rs:131-133` 与 `:174-176` 两处 `if row.secrets.is_empty() { continue; }`。
- `src-tauri/src/secrets/extractor.rs:464-474`（`extract_codex`）确实已把 `auth.tokens`/`auth.last_refresh` 从 `stripped` 里删掉（符合 §5.2.3"不提取、不保留、直接丢弃"）。
- 但 `ProviderSecrets::is_empty()`（`src-tauri/src/secrets/types.rs:40-42`）只看 api_key / base_url / extra_env —— 只含 OAuth 登录态的行三者皆空 → **`stripped` 从不落库**。
- 对照：在线路径 `src-tauri/src/services/provider/mod.rs:1563-1569`（`strip_and_store_provider_secrets`）是**无条件**写 `stripped`。因此缺口只存在于一次性迁移与导入路径。

**后果**

1. 形如 `{"auth":{"tokens":{…refresh_token…}}}` 的 Codex 官方卡，其 OAuth token 继续明文留在 `providers.settings_config`。
2. `secrets_migration_pending` 在 `src-tauri/src/database/mod.rs:206` 被删除 → **永不再重试**。
3. SQL 导入与 DB 文件还原复用同一个 `run_migration`（`secrets/store.rs:26-50` → `secrets/migration.rs:65`）→ 同样漏。
4. 连锁：§6.4 的 live 重写会因 `codex_config.rs:3121-3124`（`write_full_auth = codex_auth_has_login_material(auth)`）把这份 tokens **再写回 `~/.codex/auth.json` 明文**，与 D3 / §5.3.1"不再写 auth.json"直接冲突。
5. 导出护栏（`secrets/scan.rs:42-53` 的正则集）对 JWT 形态的 token 未必命中。

**要动**：`secrets/migration.rs`（两处 `continue` 改为"仍进事务写 stripped，只是不写凭据、不进 `migrated_providers`"）；补测试：`migration.rs` 加"只有 `auth.tokens` 的行迁移后 DB 不含 tokens"，`src-tauri/tests/migration_v18_cold_start.rs` 夹具加一个 keyless-official 供应商。

---

## 3. P1 · 计划明文要求、完全没碰的四项

| # | 计划出处 | 要求 | 现状 |
|---|---|---|---|
| 1 | §5.3.1 注 / L281 | 冲突检测要扫描 `~/.claude/settings.local.json` 并给**只读告警**（那里的 `env.ANTHROPIC_*` 会覆盖我们投的进程环境变量） | 全仓（`src-tauri/src`、`src`）对 `settings.local` 零引用；`services/env_checker.rs` 只扫注册表 |
| 2 | §6-4 / L392 | 删除 `apply_codex_official_proxy_route` 写的官方代理路由表 | `cc-switch-official` / `OFFICIAL_PROXY_PROVIDER_ID` 在 `src-tauri/src`、`src-tauri/tests` 零命中 —— Phase 1a 只删了**写方**，没有读方/清理方。老用户 `config.toml`（及被回填进 DB 的那份）里的 `model_provider = "cc-switch-official"` + `base_url = http://127.0.0.1:<port>` 无人清，6.4 反而原样再写回盘 |
| 3 | §5.4 表"新增/编辑"② | 校验：Claude 必 api_key；Codex 第三方必 api_key+base_url；Pi 必 base_url | 后端 `validate_provider_settings`（`services/provider/mod.rs:1337-1394`）与 `pi_config/mod.rs:272-282` 只做形状校验；前端仅对 claude/codex 软校验（可跳过，`ProviderForm.tsx:612-653`），Pi 无校验。后果：Pi 无 base_url 的供应商可保存并启用，`models.json` 节点没有 `baseUrl` → CLI 直接不可用 |
| 4 | §6.3 + §9 Phase 4 验收 | 明文残留自动删除清单要有回归保护 | `secrets/cleanup.rs` 无 `mod tests`；夹具 `src-tauri/tests/fixtures/v18-home/` 里不放 `config.json`/`.bak`/`.migrated`/`backups/env-backup-*.json`/`codex_oauth_auth.json` → 冷启动测试也没机会验证删除；Phase 4"扫整个测试 home 为 0 命中"从未真正执行 |

---

## 4. P2 · 实现与计划不一致，需要口径裁定

以下 8 条**都不在已确认的三条有意偏差之内**，需要逐条决定"改代码"还是"记为有意偏差"。

1. **Codex 第三方"无 key 即拒写"安全门被架空。** 门本身还在（`src-tauri/src/codex_config.rs:3189-3195`），但真实写盘用的 `sanitize_codex_config_for_live_write_with_base_url`（`services/provider/codex_sanitizer.rs:30-35`）把 `has_store_key` 硬编码 `true` → 无条件注入 `env_key` → `has_carried_auth` 恒真（`codex_config.rs:3143-3144`）→ 门永不触发。只有预检路径走真值（`services/provider/live.rs:571`）。另外 `ProviderService::update()`（`services/provider/mod.rs:357-358`）保存当前供应商时根本不跑预检。后果不是明文泄漏，而是无 key 的第三方卡被写成指向不存在变量的 `env_key`，UI 报"切换成功"但 Codex 不可用。
2. **缺 api_key 时 Codex/Pi 只 `warn` 不拒绝**（`services/provider/mod.rs:870-881,884-899`），与 §5.4-②"缺 api_key 直接拒绝"和 §5.5"同步还原后切换时拒绝"不一致。commit `0a3f830` 显示是有意降级，但未记入偏差清单。
3. **Pi 启用次序与计划相反**：`services/provider/pi.rs:219-226` 实际为 预检 → **先写 `models.json`** → 再投变量 + broadcast。计划要求 ②投变量 → ③写节点 → ④broadcast，且"③ 失败撤销 ②"。`sink.set` 失败（注册表权限、白名单拒绝、名字超限）时 live 已留下指向不存在变量的 `apiKey:"$CC_SWITCH_PI_…"`，无撤销路径。
4. **`get_providers` 每行 2 次 CredReadW + `block_on`**（`src-tauri/src/commands/provider.rs:15-55`）违反 §5.2.1 / L194"批量读取默认不触碰凭据管理器"。根因是 §5.2.1（不碰 CM）与 §5.2.2（`hint` = 末 4 位，必须读值）互斥，代码选了后者。需你定优先级：`present`/`extraEnv` 可从 `known_secret_targets` 得出（同文件 `:57-69` 已是范例），只有 `hint` 必须读值。
5. **`global_proxy_url` 是整键作废而非去 userinfo**：`database/schema.rs:1491-1502` 值含 `@` 即 `DELETE` + warn；计划 §5.2.4-4 是"保留但在 Rust 侧校验去掉 userinfo（D10）"。方向更严、无泄漏，但用户代理设置会莫名消失且无 UI 提示。
6. **§6.4 复用整条 `switch` 而非只跑 ⑤⑥**：`lib.rs:588` → `services/provider/mod.rs:574-577`（无条件 `set_current_provider`）、`:603`（MCP 重投影）、Codex 分支 `codex_config.rs:3266-3269`（可能删 `~/.codex/auth.json`）。`is_current` 值未变（写回同一 id），但迁移路径做了切换的全部副作用，其中"删 auth.json"与 §6.4"含 tokens 的文件一律不动"语义冲突。
7. **删除供应商三步次序**：实际 删凭据 → 删变量 → 删 DB（`services/provider/mod.rs:386-388`、`services/provider/pi.rs:149-162`），计划 §5.3.3 是 删变量 → 删凭据 → 删 DB。不影响安全性，仅事务次序。
8. **`migrate_v15_to_v16` 被改写**，违反 3.2-1"历史迁移原样保留"：基线（`git show efc4a76:src-tauri/src/database/schema.rs` L1559-1562）4 行转调 `session_usage_codex::reset_codex_usage_on_conn` → 现 `src-tauri/src/database/schema.rs:1274-1330` 内联 57 行，且不再重置 Codex 用量缓存。其余 17 个历史迁移函数逐字节未变（已核对）。

### 其它同类小项（一并裁定）

- `EnvSink::set` 白名单比附录 C 宽：任何命中 `is_sensitive_config_key` 的名字（`MY_KEY`、`AWS_TOKEN`）都能写 `HKCU`（`src-tauri/src/env_delivery/sink.rs:85-94`，测试 `:294` 明确断言放行）。计划意图是"仅限 Claude `extra_env` 命中的键"。
- `known_secret_targets` 未在"每次 `SecretStore.set` 成功后"登记：应用级条目走 `secrets/sync_secrets.rs:13-54`，既不登记 target 也不 `note_session_secret`（孤儿清理靠附录 B 固定名探测兜住，功能没漏，但不变量不完整）。
- `env_checker.rs` 只读扫描的关键词只覆盖 `ANTHROPIC*`/`OPENAI*`（`services/env_checker.rs:159-165`），`CC_SWITCH_CODEX_API_KEY`/`CC_SWITCH_PI_*` 被外来工具占用时只有切换才发现。
- Pi 新增/编辑在 DB 写失败时不回收刚写的凭据（`services/provider/pi.rs:84-95,125-136`；Claude/Codex 已回收）。
- 终端启动器把 `Zeroizing` 降级为裸 `String`（`commands/misc.rs:2946-2952`）；`services/s3.rs:22-30` + `s3_sync.rs:255-264` 的 `access_key_id/secret_access_key` 是 `String`（与 `secrets/store.rs:77-78` 自述矛盾）→ S3 原则的收口点。
- 迁移错误文本值外泄面：`secrets/migration.rs:98-103` 把底层 `AppError` 原文拼进"只含 provider id 与字段名"的报错；`secrets/extractor.rs:496-499` 的 `Invalid Codex config.toml: {e}` 可能带 TOML 原文片段。

---

## 5. P3 · 删除面残渣（均已核实的具体项）

### 依赖与配置

| # | 项 | 证据 |
|---|---|---|
| 1 | `rustls` 直接依赖与 `install_default()` 都未删（计划 Cargo 删除清单里唯一未落地项） | `src-tauri/Cargo.toml:44`；`src-tauri/src/lib.rs:328`（全仓唯一使用点）。reqwest 已是 `rustls-tls` 且无 `native-tls`，删后可跑一次真实 HTTPS 验证 |
| 2 | `custom_user_agent` 参数链未随字段删（前端已 0 传参，是无生产者但可被外部数据写入的 UA 覆盖通路） | `commands/model_fetch.rs:18,23,35` → `services/model_fetch.rs:63,144,179-180` |
| 3 | `ProviderMeta.usage_script` 字段仍在结构体里（计划 §5.2.1 要删；v19 只清了 DB，入侧仍可原样写回） | `src-tauri/src/provider.rs:174-176`；唯一消费者 `src-tauri/tests/fixture_v18.rs:148` |
| 4 | 新库 DDL 仍为已删应用建 8 个死列，DAO 硬写 `false`（同版本 `schema.rs:1471` 已示范 `DROP COLUMN`，技术可行） | `database/schema.rs:53-55,81-84`；`dao/mcp.rs:12,111`；`dao/skills.rs:25-26,108,197` |

### 死代码 / 带雷代码

| # | 项 | 证据 |
|---|---|---|
| 5 | 两个未注册、无前端调用的命令（约 90 行，含本机 8 端口探测） | `commands/global_proxy.rs:164-185 get_upstream_proxy_status`、`:187-252 scan_local_proxies`；`lib.rs:1245-1248` 只注册 3 个 |
| 6 | `UPDATE provider_health` 指向 v19 已 DROP、新库从不创建的表（当前零调用方，一旦被复用整个事务必回滚） | `database/dao/providers.rs:299-303`（在 `replace_provider_id:225` 内，全仓无调用者） |
| 7 | 孤儿 API 且后者为空实现，还会产生附录 B 之外的 target 形态 | `secrets/extractor.rs:88 extract_app_secrets` / `:106 restore_app_secrets`（`:110` 原样返回） |
| 8 | S5 的 Pi 护栏是"构造时保证"而非"写入前断言"，且委托目标不存在 | `services/provider/pi_sanitizer.rs:9-51` 只做重写无断言；`services/provider/live_sanitizer.rs:126-128` 的 `AppType::Pi` 分支为空并注释"交给 pi_config 校验"，而 `pi_config/mod.rs:272-282` 只校验形状 |
| 9 | 新增 1 处 `#[allow(dead_code)]`（3.2-3"删除就是删除"） | `src-tauri/src/env_delivery/sink.rs:113` |

### 前端死 UI 与失效断言

| # | 项 | 证据 |
|---|---|---|
| 10 | "检测连通"是永久灰化、点不动的死按钮，整条管道未删 | `src/components/providers/ProviderActions.tsx:206-221`、`ProviderCard.tsx:31,33,86,88,239-248`、`ProviderList.tsx:232`（硬编码 `isTesting={false}`）；`lib/api/connectivity-check.ts` 已删 |
| 11 | 自动同步表清单仍含两张已删表，测试还断言其有效 | `services/sync_protocol.rs:69,76`（`provider_endpoints`/`proxy_config`）+ `:499,506`；`services/s3_auto_sync.rs:201-202`、`webdav_auto_sync.rs:205-206` |
| 12 | 测试里给已删模块打桩（"API 面已收敛"没有测试兜底） | `tests/integration/App.test.tsx:151`（UpdateBadge）、`tests/hooks/useProviderActions.test.tsx:60-61,76-83`（openclawApi）、`tests/components/EditProviderDialog.test.tsx:17,28-30`（openclawApi/managedAuth）、`ProviderList.test.tsx:62-77`（UsageFooter/useStreamCheck）、`SettingsDialog.test.tsx:23-27`（useProxyStatus）、`UnifiedMcpPanel.test.tsx:72-81`（`apps` fixture 含 4 个已删应用，靠 `as McpServer` 绕类型检查） |

### i18n 死键（四份同步，每份一份）

| 组 | 键数 | 位置（en.json） | 判定 |
|---|---|---|---|
| `console` 整命名空间（含计划点名的 `updateFailed`/`checkUpdateFailed`） | 17 | `:983-984` 等 | 零引用，整组可删 |
| `providerAdvanced` 整节（`costMultiplier*`/`pricingModelSource*`） | 11 | `:1160-1172` | D4 定价家族连带死键 |
| `settings.globalProxy.pricing*` | 14 | `:703-716` | 同上 |
| `app`（`app.title`/`app.description`，后者仍写"Gemini CLI"） | 2 | `:4` | 零引用 |
| `settings.advanced.connectivityCheck.*` | 2 | `:249-252` | 描述的能力已不存在 |
| `codexConfig.upstreamModelName{,Hint}` | 2 | `:1211-1212` | hint 仍宣称"路由会把 Responses 转成 Chat Completions" |
| `sessionManager.subtitle`、`firstRunNotice.bodyDefault` | 2 | `:889`、`:50` | **仍会渲染**，且列出 8 个应用 / 声称支持 Gemini CLI |

四份 locale 键集经扁平化对比为零差异（但见 §6-1，那只是巧合，不是被测出来的）。

### 仓库根 / CI / 发布 / 文案

| # | 项 | 证据 |
|---|---|---|
| 13 | 三个删除作业遗留文件，**两个已提交进 git** | `session-manager.md`(9.4KB)、`sql_helpers_head.rs`(16.9KB) 均 `git ls-files` 命中（`d94388f` 引入）；`nul`(141B) 内容是 `dir: cannot access 'srccomponentsuniversal'`——删 universal/deeplink 目录时的误重定向产物，Windows 保留名会让后续 `git clean`/复制异常（需 `del \\.\D:\LS\DM\ccs\nul`） |
| 14 | `release.yml` 未按 D1 收敛为"只保留 Windows 构建 job" | matrix 只有 `windows-2022`（`:22-23`），但 `:42-70` 装 Linux 依赖、`:125-127` Linux 构建、`:196-228` Prepare Linux Assets、`:286-287` 发布正文承诺 Linux 产物；ARM64 分支（`:37-40,95-107,116,133,138`）依赖**从未定义的 `matrix.arch`** → 永不进入，正文却列 `-Windows-arm64.msi` |
| 15 | `.github` 元数据指向已删对象 | `.github/labeler.yml:56,68-74`（4+1 条 glob 永不命中）；`.github/ISSUE_TEMPLATE/{bug_report.yml:52-54,feature_request.yml:31-33,question.yml:31-33}` 下拉仍列 Gemini CLI / OpenCode / OpenClaw，且没有 Pi |
| 16 | `scripts/generate-download-manifest.mjs` 计划点名要改的注释未改，且脚本已无调用方 | `:48-49` 仍解释 `.sig`/`latest.json`；`:30-41` 的 macOS/Linux 分类在 D1 下永不命中；全仓 grep 该脚本名 → 除自身与计划文档外 0 引用。另有 `pnpm-lock.yaml`/`package.json` 与 `Cargo.toml:4` 的 `description` 仍写"All-in-One Assistant for Claude Code, Codex & Gemini CLI" |
| 17 | 注释/文档字符串仍在叙述已删能力 | Rust 约 12 处：`codex_config.rs:176`（失效的 `crate::proxy::…` intra-doc link）、`:2050,2993-2995`（引用已不存在的 `update_live_backup_from_provider`）、`:2985-2997`（inject 文档错位粘到 `strip_…` 上）、`services/switch_lock.rs:43-51`、`services/provider/mod.rs:436-445,467`、`services/profile.rs:9`、`lib.rs:707`、`dao/providers.rs:451`、`commands/skill.rs:4`、`app_config.rs:535`、`commands/misc.rs:2972`、`services/provider/mod.rs:209,370`（"累加模式应用 OpenCode/OpenClaw"，现在只有 Pi）。前端约 25 处，其中用户可见 fallback 文案三处直接宣称需要代理：`CodexFormFields.tsx:706,755`、`shared/EndpointField.tsx:47-48`；另 `config/codexProviderPresets.ts:1334,2648` 等 |
| 18 | `docs/user-manual` 整站与删除面矛盾（Phase 6 未完成，本轮面积最大的一块） | `docs/user-manual/{en,ja,zh}/1-getting-started/1.1,1.2,1.3,1.4,1.5`、`2-providers/*`、`3-extensions/*`、`5-faq/*` 仍描述 7 个应用；`en/1/1.2-installation.md:196-202` 仍写"内置自动更新 / 在设置>关于手动检查更新"；`docs/user-manual/assets/claude-desktop-*.png` 15 张已删功能截图。另 `docs/guides/codex-official-auth-preservation-guide-{zh,ja,en}.md`、`codex-desktop-custom-model-visibility-*`、`codex-unified-session-history-guide-en.md:221` 仍把"路由接管"当必需步骤，含 9 条指向已删 `user-manual/*/4-proxy/*` 的死链 |
| 19 | `docs/` 之外：CHANGELOG 无本次 breaking 条目 | `CHANGELOG.md` 顶部仍是 `## [3.20.3] - 2026-09-11`，本轮瘦身（移除自动更新 + 5 个应用 + 明文存储）无 Unreleased/breaking 段 |

---

## 6. 门禁与测试的四个事实（其中两条纠正计划假设）

1. **计划 3.2-4 的前提不成立。** `tests/config/localeCoverage.test.ts` 只断言 Pi 命名空间键与含 `\bPi\b` 的文案覆盖（`:40-51,54-92`），**不存在**"强制四份 locale 键集合一致"的全量门禁。所以"删键必须四份一起删"目前没有任何机制保证——上面那批死键与四份零差异只是巧合。
2. **Phase 2 要求的 `--ignored` CI step 不存在。** `secrets_windows_roundtrip`（`src-tauri/src/secrets/store.rs:571-596`）与 `env_sink_windows_roundtrip`（`src-tauri/src/env_delivery/sink.rs:371-402`）本体都在、质量不错（`Drop` 清理、断言注册表无残留），但 `.github/workflows/ci.yml` 唯一的两处 `--ignored`（`:255,:263`）是 WSL2 的 `atomic_write` 用例 → 真实凭据管理器/注册表往返从未在 CI 跑过。
3. **CI secret-scan 覆盖不到迁移产物（纠正"完全空跑"的说法）。** 扫描目标 `$env:TEMP\cc-switch-test-home`（`ci.yml:159`）**确实**与 `src-tauri/tests/support.rs:10` 一致，常规集成测试产物在被扫范围内；但冷启动迁移测试用的是另一个目录 `temp_dir()/cc-switch-v18-cold-<pid>-<ns>`（`src-tauri/tests/migration_v18_cold_start.rs:96-99`）并在 `Drop` 里 `remove_dir_all`（`:120`）→ §6.2/6.3/6.4 的产物在扫描时刻已消失；且步骤先 `New-Item` 建空目录，目录不存在时也静默通过。夹具真身 `src-tauri/tests/fixtures/v18-home/`（本身含明文）未被扫。
4. **缺失的验收用例**：Phase 4"人为把 `models.json` 设为只读 → `live_reapply_pending` 保持 1"无测试（`secrets/migration.rs:338 record_live_reapply_failures` 零测试引用）；Phase 3"切换后环境变量已投递并登记所有权"无自动化断言（`EnvSink` 不在 `AppState` 上、`default_sink()` 每次返回**新**实例 `sink.rs:17-30`，导致同一次 `deliver_env_credentials` 里 `check_conflict` 读到另一个空对象 → 集成测试层的冲突/所有权逻辑形同不存在）；Phase 4 回滚仅有 `SECURITY.md:28-30` 的文字 + `database/tests.rs:205 schema_migration_rejects_future_version`；§9 L559"从 3.20.3 安装包真实机器升级测试"在仓库内**无任何完成记录**。
5. 另有两处次要：`secrets/store.rs:520-540`（未 `#[ignore]` 的单测读真实凭据管理器，只读不写，违反 3.2-5 的"真实后端只在一个明确标记的集成测试里跑"）；§10 要求的 extractor"规则表驱动"形式未采用（现为 11 个具名用例），`hydrate` 覆盖仅 2 例未按三应用展开。

---

## 7. 已确认有意偏差（本轮不再作为缺陷上报）

1. `read_live_settings` 保持 `{auth, config}` 形状但全量脱敏（计划 §5.2.2 字面要求只返回 `config`）——说明见 `services/provider/live.rs:975-980,1053`。
2. 写侧 `ProviderSecretsInput` 的 `undefined/null/string` 三态与"`null` = 删除"不做。同一缺陷的另一半体现在 P0-1 第 4 条（WebDAV/S3 空值不删条目），属 §5.2.5 而非 §5.2.2，仍建议收。
3. 附录 A 要删的 `check_env_conflicts`/`delete_env_vars` 并入命名空间 `env_delivery_scan`/`env_delivery_remove`，保留"移除选中外来变量"能力——已确认旧命令名确实不再暴露（`lib.rs` invoke_handler 内不在册）。

## 8. 本轮复核确认「已做且无缺口」的主要部分（避免重复劳动）

- **§5.3.2 `EnvSink` 全表**：trait 四方法、`REG_SZ` 而非 `REG_EXPAND_SZ`、`KEY_SET_VALUE|KEY_QUERY_VALUE`、`std::env::set_var/remove_var`、`SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, …, SMTO_ABORTIFHUNG, 5000, null)`（`env_delivery/sink.rs:216-240`）、`Win32_UI_WindowsAndMessaging` 特性（`Cargo.toml:77-84`）、14 项禁止列表 + 前缀兜底、`set` 参数已是 `&Zeroizing<String>`、永不碰 HKLM。3.1-7 也达标：全仓新增 `unsafe` 仅此 1 处（`git diff pre-slimdown-baseline..HEAD` + `git blame` 已核）。
- **§5.3.4 终端启动器**：临时 `--settings` 文件路径已删（全仓 `--settings` 零命中），改 `Command::env()` 注入，不落文件不写值。
- **§5.5 + S4**：SQL 导出（`database/backup.rs:105`）、同步载荷（`:113`）、DB 文件备份（`:527` → `secrets/scan.rs:55-81` 扫 `providers.settings_config` + `settings` 表）三面挂护栏；`sk-ant-`/`xai-` 实现比计划更严；导入/还原后 `scrub_imported_plaintext`；跨机还原"需要重新输入密钥"徽标（`ProviderCard.tsx:191-202`，四语键齐）。护栏回归测试存在：`database/backup.rs:1309 export_rejects_plaintext_secret_pattern`。
- **S1 IPC 零密钥**：逐命令核过（含 `get_providers`、`read_live_provider_settings`、`get_settings`、`env_delivery_*`、`export_*`、webdav/s3、model_fetch、profiles、prompts、config snippets），未发现 api_key/密码/token 值外泄；唯二"值过 WebView"是计划授权的 `base_url` 全值与末 4 位 hint。
- **S2 日志**：`redact_known_secrets` 最小长度已 6 且 `CC_SWITCH_*` 值自动入脱敏集（`lib.rs:109,135-141`），计划要求的那个测试确实存在（`src-tauri/tests/log_no_secrets.rs:52`）。
- **S6/S7/S8/S9/S10/S11/S12**：CSP `connect-src 'self' ipc: http://ipc.localhost` + `img-src` 保留 `https:`（`tauri.conf.json:29`）；`capabilities/default.json` 10 项且逐项能对上使用点；SECURITY.md 三段如实；单实例回调 `|app,_args,_cwd|`（`lib.rs:247`）；`global_proxy_url` setter 拒 userinfo（`dao/settings.rs:158-166`）+ 前端无用户名密码框；`EnvConflict` 只带 `masked_value`；`atomic_write_private` 保留。
- **§7.1/7.2/7.3 主干**：四项"必须搬迁"（`http_client`、`switch_lock`、`LogConfig`、两个 Codex 函数 + `ImageInputCapability`）全部到位且**无误删**（`restart_app`、`tauri-plugin-process`、`json5` 保留而 `json-five` 删除、D10 三项、Skills/CLI 工具自更新面均完好）；Rust 整删清单 36 项、前端整删清单 28 项 + 5 目录、Cargo 依赖删除清单除 `rustls` 外全部落地（`Cargo.lock` 里 axum/hyper/rquickjs/updater/deep-link 均 0 条目）；`switch` 已收敛为"取锁 + `switch_normal`"；`PROXY_MANAGED` 只剩读方；`AppType` 只剩 claude/codex/pi；updater/deeplink/sync-r2/flatpak 发布面基本收净。
- **附录 B/C 命名**：target 与环境变量名严格符合，无临时发明的新名（唯一例外见 P3-7 的孤儿 API）；`EnvSink` 白名单以 `validate_env_name` 形式实现（宽度问题见 §4 其它小项）。
- **§5.2.4 schema v19**：`SCHEMA_VERSION = 19`、10 张表 DROP、非目标应用清理（含 `profiles.payload` Rust 侧过滤、`current_profile_id_*`）、`DROP COLUMN in_failover_queue`、settings 键作废集、`meta.usage_script` Rust 侧剥离、两个 pending 标志、且 v19 未碰 `settings_config`（符合"纯 SQL 可内存库单测"）——逐项已核。

---

## 9. 建议收尾顺序

1. **P0-2**（明文残留，一处 `continue` + 测试）→ **P0-1**（同步凭据写入最后一公里）→ **P1 的 2/3/4**。三者都是小改动、可分别提交。
2. **P1-1 / §6.4 官方路由表清理 / Pi 启用次序 / 无 key 是否拒切**：都需要先定 §4 的口径，定完一次性收，避免反复触碰同一批文件。
3. **P3 残渣**按"依赖与配置 → 死代码 → 前端死 UI → i18n 死键 → 注释 → 仓库根 → CI/发布/文档"分组清，每组跑一次 clippy + typecheck + 测试。
4. **§6 门禁**：补一个全量四份 locale 键集合一致测试（否则死键会再长回来）+ CI 加 `--ignored` step + secret-scan 指向真正的迁移产物目录。
5. **Phase 6 文档**（README 已完成，`docs/user-manual` 三语与 `docs/guides` 三个 guide 是剩下的主体）+ CHANGELOG breaking 条目 + §9 L559 的真机升级验收留痕。

> 本文只描述现状，未对仓库做任何修改；除 P3 表中标"仅提及"的死代码（按项目规则不顺手删）外，各条都给了可执行的收口点。
