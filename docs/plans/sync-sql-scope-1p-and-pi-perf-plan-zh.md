# 施工方案：云同步 / SQL 导入导出的数据边界（1Password 模式）+ Pi 页面卡顿优化（2.3.x）

状态：S0–S7 施工完毕，§8.2 单机真机验收通过（2026-09-28）。**分支策略：`1password` 分支永不合并到 `main`，仅用户自用**——后续工作都直接发生在这个分支上，没有发版流程
基线：`1password` 分支 @ 8703c42（D3-B：base_url 随整包进 1Password；端点表降级为读取缓存）
上位文档：
- `docs/plans/1password-backend-impl-plan-zh.md`（原方案，D1–D10）
- `docs/plans/1password-fix-plan-zh.md`（修复方案，F0–F5、D11–D15）

本文件只写「同步 / SQL 导入导出应该带什么、不带什么，现有代码哪里不对、怎么改、为什么」，外加一个独立的 Pi 性能问题。与上位文档冲突时以本文件为准。行号以 8703c42 为准，施工前请先 `grep` 复核。

---

## 0. 一页纸结论

| 项 | 结论 |
|---|---|
| 用户的问题：1P 模式下 WebDAV / SQL 是否只同步模型设置和其他设置？ | **是。** 1Password 自己负责跨设备同步钥匙和端点（D3-B 之后 base_url 也在 1P 里），CCS 的云同步和 SQL 导出只应带「非秘密的共享配置」：供应商卡片、模型字段、MCP、Prompts、Skills、Profiles、通用配置片段等。**钥匙、端点和本机状态都不应出本机**，导入时也不应覆盖本机的这些数据 |
| 现状：api_key 是否已经不出本机？ | 基本是。提取器把 api_key、敏感 env、Codex bearer token 都从 `providers.settings_config` 剥掉了（`secrets/extractor.rs:419-590`），同步和 SQL 导出本来就不带钥匙，出口还有正则护栏（`secrets/scan.rs:46-50`） |
| 现状：哪些东西「不该出去却出去了」 | ① **base_url 仍以明文随同步和 SQL 出本机**：`provider_endpoints` 缓存表在同步导出和手动导出里都有（`backup.rs:89`、`:107`）；② **本机专属的 settings 行被整表同步覆盖**：`managed_env_vars`（本机 HKCU 投递登记）、`known_secret_targets`、迁移标记、`live_reapply_pending` 等（`backup.rs:44` 注释本身就承认 settings 表会被同步）；③ **手动 SQL 导入整库覆盖**，本机的 `secret_refs` 被文件里的引用替换（`backup.rs:147` 传的是 `&[]`），和同步路径「保留本机引用」的规则（`:92`）不一致 |
| 现状：哪些东西「该进来却没进来」 | 另一台设备新增的供应商下载到本机后，本机**没有它的 `secret_refs`**，列表显示「缺钥匙」、保存时报「必须填 API Key」，只能手动点「从 1Password 重建引用」（N+1 次 `op` 调用，每次 6~9 秒）。另外，端点缓存在另一台设备改了 base_url 之后**永远不会刷新**，缓存命中还会压过 1P 的真值（`services/provider/mod.rs:1464-1472`） |
| 最危险的隐患 | **标题兜底命中时不核对归属**（`secrets/onepassword.rs:399-407`）。方案 B 之后条目标题就是供应商显示名。Claude 和 Pi 里都有一个叫「OpenRouter」的供应商时，没有引用行的 Pi 供应商会读到 Claude 的条目，`repair_ref` 还会把这个错误的 item id 写进引用；之后编辑 Pi 供应商会**就地改写 Claude 的条目**。同步正好制造了大量「没有引用行」的供应商，所以这条必须在同步改造之前修 |
| Pi 卡顿根因（已核实） | 每点一次 Pi 页面，`models.json` 里的**每个供应商都触发一次 `op` 调用**（D3-B 让 `store_provider_bundle` 在 base_url 非空时总会 fetch）。同时 `get_pi_current_state` 作为**同步命令跑在主线程**，在等同一把 Pi 切换锁，于是整个窗口冻结。详见 §4 |
| 施工策略 | 7 个阶段：S0 测试护栏 → **S1 Pi 卡顿（用户痛点，先做）** → S2 标题兜底归属校验 → S3 快照导出策略 → S4 导入合并策略 → S5 同步冲突与回声 → S6 SQL 手动导入导出体验 → S7 护栏补强、S3（对象存储）一致性、文档与 i18n。每阶段独立可编译、测试全绿、单独提交 |

---

## 1. 现状核查：数据在哪、同步带不带、导入怎么处理

### 1.1 三个存储位置

| 位置 | 内容 | 是否出本机 |
|---|---|---|
| `~/.cc-switch/settings.json`（`settings.rs:388-541`，`AppSettings`） | WebDAV/S3 配置与同步状态（seq、etag）、`secret_backend`、1P 账户/保险箱、当前 Claude/Codex 供应商、严格投递、各类 pending 清单 | **不出**（设备本地，正确） |
| SQLite `cc-switch.db`（schema v21，`database/mod.rs:48`） | providers、mcp_servers、prompts、skills、skill_repos、profiles、settings（KV）、secret_refs、provider_endpoints | 同步导出只跳过 `secret_refs`（`backup.rs:89`）；手动导出全量（`:107`） |
| 凭据后端（1P 条目 / Windows 凭据管理器） | api_key、extra_env、base_url（D3-B）、AppSync（WebDAV 密码、S3 两把、E2E 口令） | 不经过 CCS 的同步；1P 自带跨设备同步 |

### 1.2 表级现状

| 表 / 键 | 性质 | 同步导出 | 同步导入 | 手动 SQL 导出 | 手动 SQL 导入 |
|---|---|---|---|---|---|
| providers（含模型字段） | 共享配置 | 带 | 覆盖 | 带 | 覆盖 |
| mcp_servers / prompts / skills / skill_repos / profiles | 共享配置 | 带 | 覆盖 | 带 | 覆盖 |
| settings：`common_config_*`、`global_proxy_url`、`log_config` 等 | 混合 | 带 | 覆盖 | 带 | 覆盖 |
| settings：`managed_env_vars`、`known_secret_targets`、`secrets_migration_*`、`live_reapply_pending`、`legacy_secret_recovery`、`skills_ssot_migration_*` | **设备本地** | 带 ✗ | **覆盖 ✗** | 带 | 覆盖 ✗ |
| provider_endpoints | 1P 模式下是 1P 真值的**本机缓存** | 带 ✗（1P 模式） | 覆盖 ✗（1P 模式） | 带 ✗ | 覆盖 ✗ |
| secret_refs | 引用（item id、字段名，不含值） | 跳过 | 保留本机 | 带 | **覆盖 ✗** |

✗ = 与「1P 模式只同步非秘密共享配置」的目标不符，或与同步路径规则不一致。

### 1.3 相关调用链（施工时要读的代码）

- 导出：`sync_protocol.rs:151-212` `build_local_snapshot` → `backup.rs:113-118` `export_sql_string_for_sync` → `snapshot_to_memory`（`:254-266`，**内存副本**）→ `dump_sql(conn, skip_tables)`（`:702`）→ `assert_no_secret_patterns`。
- 导入：`sync_protocol.rs:359-393` `apply_snapshot` → `backup.rs:157` `import_sql_string_for_sync` → `import_sql_string_inner_with_hook`（`:170-251`）：先在 `NamedTempFile` 暂存库里执行 SQL，然后迁移，再在主库锁内做安全备份 → `restore_tables(preserve)` → SQLite Backup 整库替换。
- 下载后处理：`commands/webdav_sync.rs:154-175`、`commands/s3_sync.rs:157-177` → `store.rs:100-127` `scrub_imported_plaintext` → `sync_support.rs:8-37` `run_post_import_sync`（`sync_current_to_live` 会**跳过 Pi**，见 `live.rs:1019-1021`）。
- 手动导入导出：`commands/import_export.rs:136-214`；`.db` 备份恢复：`:278-304`。
- 端点：写入 `mod.rs:2246-2252`（`store_provider_bundle`）、`mod.rs:2320-2325`（懒回填）、`pi.rs:284`；读取 `commands/provider.rs:54-61`（列表）、`mod.rs:1464-1472`（`fetch_provider_secrets` 覆盖 vault 值）、`live.rs:884-904`（Codex）。
- 引用：`dao/secret_refs.rs`；1P 定位 `onepassword.rs:236-272`（`ref_item_id` / `repair_ref`）、`:380-432`（`fetch_item_resolved`）；重建引用 `commands/onepassword.rs:268-336`。

---

## 2. 目标与总体施工原则

### 2.1 目标

1. **1P 模式下，云同步和 SQL 导出只带非秘密的共享配置**：供应商卡片与模型设置、MCP、Prompts、Skills、Profiles、通用配置片段等。base_url、钥匙、本机状态一律不出本机。
2. **导入不破坏本机**：本机的引用、端点缓存、设备本地 settings 行，不会被另一台设备或另一份文件覆盖。
3. **换设备 / 多设备可用**：另一台设备新增的供应商下载后，尽量 **0 次 `op`** 就能显示正确的钥匙状态；需要 `op` 的补齐动作由用户主动触发，并有进度显示。
4. **1Password 是钥匙和端点的唯一真源**（D3-B）：本机缓存只做加速，一旦与 1P 真值不一致，以 1P 为准，并顺手修正缓存。
5. **凭据管理器模式的行为不回归**：该模式下端点照旧随同步走（见 D-S1），只修设备本地 settings 行和冲突类问题。
6. Pi 页面点击响应 ≤ 300 ms，列表刷新 0 次 `op`、0 次无谓的数据库写入。

### 2.2 总体施工原则（每一步都要遵守）

1. **先写复现测试，再改代码。** 每个问题先用 `CountingVault` / `InMemoryVault` / `Database::memory()` 写出失败的测试，确认它失败，再修。计数断言写死数值（`fetch_count() == 0`），不能写 `<=`。
2. **策略集中，不散落。** 「哪些表 / 键算设备本地、导出时怎么裁剪、导入时怎么合并」只允许在一个地方定义（新增的 `database/snapshot_policy.rs`）。同步导出、手动导出、同步导入、手动导入都调用它，禁止在各个命令里各写一套 `if is_1p`。
3. **在副本上裁剪，不改主库。** 导出侧是在 `snapshot_to_memory` 得到的内存副本上 `DELETE`，导入侧是在暂存库 `temp_conn` 上合并，然后才整库替换。主库只在原有的「持锁整库替换」那一刻被改动，失败时主库不受影响（沿用现有的原子性）。
4. **按本机模式决定，不信任文件。** 导入时采纳什么、保留什么，由**导入方本机**的后端模式决定；文件里的元数据（§5.3 的 `cc-switch-meta` 行）只用于提示和统计，**不参与安全决策**。原因：导入文件和远端快照都不可信（`backup.rs:39-62` 的 authorizer 注释已写明这一点）。
5. **原则 1 次数约束继续有效**：列表、启动、同步下载后处理默认 0 次 `op`；需要 `op` 的动作必须由用户主动触发，走 `spawn_blocking`，并显示「正在向 1Password 请求，可能需要解锁…」。
6. **兼容旧数据。** 旧格式 SQL（全量导出、没有 meta 行）、旧远端快照（包含端点和设备本地行）都必须能导入，走同一套合并策略，不报错。
7. **不升 `DB_COMPAT_VERSION`、不改远端布局**（见 D-S5）。裁剪只是「少带几行」，schema 不变，旧版客户端也能读。
8. **每条改动都写「为什么」注释**，沿用仓库风格：中文注释，标注 `S<阶段>-<序号>` 或 `D-S<号>`，引用本文件的节号。

---

## 3. 问题清单

格式：位置 → 现象/后果 → 根因。

### 3.1 P0（钥匙串错 / 数据泄出 / 核心功能坏）

**P0-1 1P 标题兜底命中时不核对 `cc-switch-group` 归属**
- 位置：`secrets/onepassword.rs:399-407`（`fetch_item_resolved` 第 2 步：唯一命中就直接返回）。
- 后果：方案 B 之后标题 = 供应商显示名，跨 app 撞名很常见（Claude、Pi 都叫「OpenRouter」「DeepSeek」）。没有引用行的供应商（同步下载后新出现的、SQL 导入后引用失效的）按标题会唯一命中**别的 app 的条目** → 读到错误的钥匙；`repair_ref` 还会把错误的 item id 写进 `secret_refs` → 之后 `put` 走 id 直达，**改写另一个供应商的条目**。
- 根因：只在 `ItemConflict`（多条同名）时才核对归属（`:334-372`）；唯一命中时默认它就是自己的条目。

**P0-2 1P 模式下 base_url 仍以明文随同步和 SQL 出本机**
- 位置：`backup.rs:89`（同步只跳过 `secret_refs`）、`:107`（手动导出不跳过任何表）、`sync_protocol.rs:74-75`（端点表还会触发自动同步）、`schema.rs:154` 注释「随云同步」。
- 后果：D3-B 已经把 1P 定为端点真源，但 `provider_endpoints` 缓存仍然出现在 WebDAV 快照（v2 是明文）和 SQL 文件里，与用户的预期（1P 模式只同步模型和其他设置）相反。
- 根因：该表是 D3-A 设计的产物（「非敏感 base_url 存 DB 并随同步」），D3-B 把它降级成缓存，但同步和导出策略没有跟着改。

**P0-3 同步导入的端点缓存会压过 1P 真值，且永不刷新**
- 位置：`mod.rs:1464-1472`（`fetch_provider_secrets`：端点表命中就覆盖 vault 的 base_url）、`mod.rs:2300-2305`（`resolve_base_url`：缓存命中直接返回）、`commands/provider.rs:54-61`（列表只读缓存）。
- 后果：
  - 设备 A 改了 base_url 并写入 1P。设备 B 如果先通过 WebDAV 下载了 A 的旧快照（或者 B 本机的缓存本来就是旧值），B 开终端、切 Codex 用的都是**旧端点**，钥匙被发往旧主机。
  - 即使 B 刚从 1P fetch 到新值，也会被旧缓存覆盖掉。
- 根因：D3-A 时期端点表是真源，所以「缓存优先」是对的；D3-B 反转了真源，读取优先级没有跟着改。

**P0-4 手动 SQL 导入 / `.db` 恢复用文件里的 `secret_refs` 覆盖本机**
- 位置：`backup.rs:147`（`import_sql_string` 传 `preserve_tables=&[]`）、`:1001-1083`（`restore_from_backup`）。
- 后果：导入另一台设备（不同保险箱）或凭据管理器时代（`vault_id=""`）的 SQL 后，本机引用全部被替换。列表徽标和「必须填钥匙」校验都只看 `secret_refs`（`commands/provider.rs:45-52`、`mod.rs:1025-1037`），会出现「显示已配置，但 1P 里没有」或反过来的情况。叠加 P0-1，按标题兜底还可能串到别人的条目。
- 根因：D14（F4-5）只在同步路径落地了「引用是设备本地数据」，手动路径被漏掉。

**P0-5 设备本地的 settings 行被同步 / 导入覆盖**
- 位置：`backup.rs:44`（注释承认 settings 表不在跳过/保留名单）、`env_delivery/ownership.rs:50-63`（`managed_env_vars` 存在 DB settings 表里）。
- 后果：
  - `managed_env_vars` 是「本机向 `HKCU\Environment` 写过哪些变量」的登记表。下载后变成另一台设备的登记表 → 本机清理投递时，会**漏删自己写过的变量**（明文钥匙残留在注册表），或者把不属于自己的变量当成自己的。1P 模式强制严格投递，影响较小；凭据管理器模式影响直接。
  - `secrets_migration_pending`、`live_reapply_pending` 被远端值覆盖 → 本机下次启动**误触发迁移或整文件重写 live**（`database/mod.rs:182-195`、`lib.rs:655-663`）。
  - `global_proxy_url` 绕过了 DAO 的 `@` 校验（`dao/settings.rs:156-167`），直接被远端值写入。
- 根因：settings 表是「共享配置 + 本机状态」混放的 KV，同步策略只有表级粒度，没有行级粒度。

### 3.2 P1（体验严重 / 数据可能丢失 / 违反原则）

**P1-1 下载后新供应商没有引用，只能手动重建（N+1 次 `op`）**
- 位置：`backup.rs:86-92`；`commands/onepassword.rs:268-336`（重建引用：每个条目一次 `op item get`，而且只 upsert，从不删除过期行）。
- 后果：两台 1P 设备共用同一个保险箱时，item id 在两边完全相同（1P 自身会同步），本可以直接采纳；现在却要逐条重新拉取，10 个供应商大约 1 分钟以上。重建也不回填端点缓存，列表端点仍为空。

**P1-2 过期引用行 / 端点行不清理**
- 同步下载后，远端已删除的供应商在本机仍残留 `secret_refs`、`provider_endpoints` 行，成为孤儿。

**P1-3 v3 上传的防覆盖检查只防「上传窗口内」的竞态，防不住「本机状态过期」**
- 位置：`webdav_sync.rs:122-126`、`:148-156`：`If-Match` 用的是本次上传前几秒刚 GET 到的 etag；S3 在首次上传（远端无 etag）时完全不检查（`s3_sync.rs:133-138`）。
- 后果：设备 B 没下载 A 的新快照就上传，会**直接覆盖** A 的新配置（seq 还会被抬高，A 再下载时也不会触发回滚告警）。本方案让同步只带「模型和其他设置」之后，这类覆盖就等于丢掉用户的模型配置。
- 根因：原计划（`v2.1-…-plan-zh.md:203`）要求用「上次同步时的 etag」，落地时用成了「本次读到的 etag」。v2 布局完全没有冲突检测，`last_remote_etag` 存了但从不读取。

**P1-4 下载后处理的写入会回声触发自动上传**
- 位置：`commands/webdav_sync.rs:67-74`：抑制守卫只包住 `download.await`，`project(result)`（scrub 和 live 同步）在守卫之外；抑制守卫还是按传输各自一份（WebDAV 下载不抑制 S3 自动同步）。
- 后果：凭据管理器模式下，下载后 scrub 写了 providers/settings → 1 秒后自动把刚下载的数据再上传一遍（v3 还会抬高 seq）。1P 模式的自动同步整体跳过，不受影响。

**P1-5 回滚冲突时仍然执行后处理；scrub 失败时整个 live 刷新被跳过**
- 位置：`commands/webdav_sync.rs:162-173`、`commands/s3_sync.rs:165-176`（`.and_then` 链）；`import_export.rs:202`（手动导入时 scrub 失败 → 主库已被替换，却返回「导入失败」）。

**P1-6 下载后 Pi 的模型设置不会生效，还会被本机 `models.json` 悄悄改回去**
- 位置：`live.rs:1019-1021`（后处理跳过 Pi）、`pi.rs:251-351`（每次列表都以 `models.json` 为准回写 DB）。
- 后果：设备 A 改了 Pi 供应商的模型列表并上传，设备 B 下载后 DB 里是新值；但 B 下次打开 Pi 页面时，`sync_native_locked` 用 B 本机 `models.json` 里的旧值把 DB 改回去。**Pi 的模型设置事实上无法同步**。
- 根因：Pi 原生契约是「`models.json` 为真源」（`docs/pi-native-contract-zh.md:41`），但下载属于「用户明确要求远端覆盖本机」，契约没有覆盖这个场景。

**P1-7 1P 模式下导入清理（scrub）在每次下载后无条件改写每一行并 `VACUUM`**
- 位置：`mod.rs:2338-2416`。即使没有任何明文，也会逐行 UPDATE 并执行 `VACUUM`，下载后处理明显变慢，还会产生大量 update_hook 事件。

**P1-8 存在待导入明文时，后续 SQL 导入 / 恢复会被安全备份的扫描拦住，而且没有重试入口**
- 位置：`backup.rs:231` → `:540-542`（安全备份要先过扫描）；`settings.rs:1273-1278`（`get_secrets_import_pending` 被标成仅测试用的死代码）；`PlaintextPendingBanner.tsx:94`（导入暂留没有重试按钮）。

### 3.3 P2（一致性 / 体验 / 护栏）

- **P2-1 扫描护栏漏掉常见密钥格式**：`sk-proj-…`、`sk-or-v1-…`（第二条正则不允许中划线）、`AIza…`、`ghp_…`、`github_pat_…`、`glpat-…` 都不在规则里（`scan.rs:46-50`）；下载侧从不扫描；文本扫描与连接扫描的排除项不一致（`scan.rs:72`）。
- **P2-2 手动 SQL 导出文件用普通权限写**（`backup.rs:128` `atomic_write`），便携包用的是 `atomic_write_private`。
- **P2-3 SQL 导入没有确认框，也不显示后端返回的 warning**（`ImportExportSection.tsx`、`useImportExport.ts:71-73`）；live 同步前后端各跑一遍（后端 `run_post_import_sync`，前端又调一次 `syncCurrentProvidersLiveSafe`）；手册（`1.5-settings.md:144-148`）描述了一个 UI 里不存在的确认步骤。
- **P2-4 S3 与 WebDAV 行为不一致**：S3 远端信息缺少降级检查（`s3_sync.rs:350-353`）；没有 Legacy 布局回退（`:179-196`）；返回原始 `dbCompatVersion`（`:389`）；下载结果缺 `sourceLayout`（`:222`、`:296`）；S3 的测试被注释掉（`s3_sync.rs:571`）。
- **P2-5 端点回填待办永不清零**：带凭据的 URL 永远不写缓存，所以 `list_endpoint_backfill_pending`（`dao/providers.rs:391-413`）会一直把它算作待回填。
- **P2-6 1P 模式下自动同步被跳过，但 UI 不提示「有未上传的改动」**，用户容易忘记手动上传。
- **P2-7 文档与实现不一致**：`1.5-settings.md:202` 说 `pre-secrets-migration` 备份不会被自动清理，但 `cleanup_db_backups`（`backup.rs:598-641`）没有这个前缀豁免。

---

## 4. Pi 页面卡顿（S1，独立于同步改造，先做）

### 4.1 根因（已在代码中逐行核实）

1. **每次进入 Pi 页面都做全量原生同步，而且每个供应商都 fetch 一次 1P。**
   - 前端 `queryClient.ts:7-8`：`staleTime: 0` + `refetchOnWindowFocus: true` → 切到 Pi 页、窗口重新获得焦点、任何对 `["providers","pi"]` 的失效操作，都会调 `get_providers("pi")`。
   - 后端 `commands/provider.rs:17-28` → `pi.rs:12-25` `list`：**拿 Pi 切换锁** → `sync_native_locked`（`:251-351`）→ 对 `models.json` 里的每个供应商调 `persist_pi_sync_secrets`（`:315`、`:329`）→ `store_provider_bundle(merge_existing=true)`（`mod.rs:2235`）。
   - D3-B 之后 `SecretBundle::from_provider_secrets`（`vault.rs:146-148`）把 base_url 也放进整包。Pi 的 `models.json` 里一定有明文 `baseUrl`（Pi CLI 不支持 baseUrl 的环境变量引用，`pi.rs:205-216`），所以整包永远不为空 → 必然 `vault.fetch`（`mod.rs:2262-2263`）→ 比较后相同，跳过 put（`:2272-2276`）。
   - 结果：**N 个 Pi 供应商 × 每次 6~9 秒的 `op`**。`pi.rs:400-402` 的注释（「拆掉非敏感 baseUrl 后整包为空、直接返回」）在 D3-B 之后已经不成立。
2. **整个窗口冻结，不只是列表在转圈。**
   - `commands/pi.rs:7` `get_pi_current_state` 是**同步** Tauri 命令（Tauri 2 的同步命令跑在主线程）。
   - 它在 `services/pi_state.rs:21` 里 `block_on(lock_for_app("pi"))`，等的正是第 1 步长时间持有的那把锁 → 主线程被阻塞，整个 UI 卡死。
   - `App.tsx:224` 和 `ProviderList.tsx:78` 在进入 Pi 页时都会发起这个查询。
3. **附带问题：每次点击都会写库，并可能触发一次云同步上传。**
   - `pi.rs:280-286` 每次都 `upsert_provider_endpoint`；`dao/providers.rs:343-349` 的 `ON CONFLICT DO UPDATE` 即使值没变也会更新 `updated_at` → update_hook 触发 → `provider_endpoints` 属于自动同步触发表（`sync_protocol.rs:75`）→ 凭据管理器模式下开着自动同步时，点一次 Pi 就上传一次。
4. 凭据管理器模式同样受影响：每个供应商每次列表都读一次凭据管理器（`CredReadW` × 字段数），虽然快，但完全是无用功。

### 4.2 改法

**S1-1 Pi 原生同步路径改成 0 次 vault 往返（核心）**
- 位置：`pi.rs:251-351` `sync_native_locked`、`:404-410` `persist_pi_sync_secrets`。
- 做法：
  1. 在 sync 路径把「真正的钥匙」和「非敏感 base_url」分开判断：定义 `has_real_secret = api_key.is_some() || !extra_env.is_empty() || base_url 是带凭据的 URL`。
  2. `has_real_secret == false` 时（这是绝大多数情况：live 里只有 `$CC_SWITCH_PI_…` 引用和明文非敏感 baseUrl）**完全不调用 `store_provider_bundle`**，只处理端点：
     - 与缓存比较（`get_provider_endpoint`）：相同 → 什么都不做。
     - 不同（用户在 CCS 之外改了 `models.json` 的 baseUrl）：
       - 凭据管理器模式：照旧 `store_provider_bundle`（本地调用，快），保持 vault 与缓存一致。
       - 1P 模式：更新缓存，并把 `pi/<id>` 记进新的本机 pending 清单 `settings.json: endpoint_vault_pending`（与 `pi_plaintext_pending` 同样的全量重建语义），**不调 op**。之后在用户主动触发的 op 动作里顺带写回 1P（S1-2）。
  3. `has_real_secret == true`：沿用现有逻辑（1P 模式走 `plaintext_pending`；凭据管理器模式立即收编）。
  4. 更新 `pi.rs:398-403` 的注释，写明 D3-B 之后为什么要在这里分流。
- 为什么：原则 1（列表 0 次 op）是 1P 接入的硬约束，F1-4 的回归测试只覆盖了「有明文钥匙」的情况，漏掉了「只有非敏感 baseUrl」这个最常见的情况。base_url 写回 1P 本来就可以推迟：缓存已经是新值，本机使用不受影响，只是其他设备要等用户下次触发 op 时才能拿到。

**S1-2 `endpoint_vault_pending` 的消化**
- 在以下已经会调 op 的用户动作里，顺带把 pending 的端点 merge 进 1P（同一次 fetch + put，不额外增加 op 次数）：编辑保存该供应商、`import_pi_plaintext_to_vault`、「从 1Password 重建引用」。
- `PlaintextPendingBanner` 增加一类「N 个端点改动待写入 1Password」，带「立即写入」按钮（async + spawn_blocking + 进度）。
- 为什么：避免 D3-B 的「1P 为真源」出现长期分歧；同时不把 op 放进列表路径。

**S1-3 端点 upsert 在值不变时不写**
- 位置：`dao/providers.rs:335-353`。
- 做法：`ON CONFLICT(app, provider_id) DO UPDATE SET … WHERE provider_endpoints.base_url IS NOT excluded.base_url`。SQLite 的 upsert 支持 `DO UPDATE … WHERE`；条件为假时不更新行，也不触发 update_hook。**施工时写测试确认 hook 不触发**（在测试里注册一个计数 hook）。
- 同理检查 `sync_native_locked` 末尾的 `save_provider`（`pi.rs:335-341` 已经有「无变化跳过」的判断，保留）。
- 为什么：消除「点一次 Pi 就触发一次自动上传」，也减少数据库写锁竞争。

**S1-4 `models.json` 指纹短路**
- 做法：在 `AppState`（或 `pi_config` 模块内的进程级 `Mutex<Option<Fingerprint>>`）记录上一次成功同步时 `models.json` 的 `(len, mtime, sha256)`。`list` 时如果指纹相同，且期间没有发生「会改 Pi DB 行的事件」，就跳过 `sync_native_locked`，直接读 DB。
- 以下事件使指纹失效：Pi 的 add/update/delete/enable/remove、`import_pi_plaintext_to_vault`、任何 SQL 导入 / `.db` 恢复 / 云同步下载（S4 的导入入口统一调用 `pi::invalidate_native_fingerprint()`）、后端切换。
- sha256 在 mtime 相同时才需要算（先比 len+mtime，不同就直接视为变化）；`models.json` 很小，算哈希也只要微秒级。
- 为什么：原生契约要求「每次进入列表都同步外部修改」（`pi-native-contract-zh.md:41`）。文件没变时同步本来就是空操作，短路不改变语义，却能省掉提取、清洗、比较等全部 CPU 和 IO 开销。

**S1-5 `get_pi_current_state` / `get_pi_session_discovery` 改为 async，并且不再等待切换锁**
- 位置：`commands/pi.rs:6-14`、`services/pi_state.rs:20-35`。
- 做法：
  - 两个命令都改成 `async fn` + `spawn_blocking`（与 `get_providers` 相同的模式）。
  - `PiStateService::current` **去掉** `lock_for_app` —— 它只读 `models.json` 和 Pi 全局 `settings.json`，而这两个文件都是原子写入（`pi-native-contract-zh.md:43`），读到的一定是某个完整版本。前端在 mutation 之后本来就会失效重取（`invalidatePiProviderCaches`）。
  - 如果施工时发现确实需要与写操作互斥，就改用 `try_lock`：拿不到锁时读当前文件（不阻塞），**绝不在主线程 `block_on`**。
- 同时全局排查：`grep -n "block_on(state.switch_locks" src-tauri/src`，所有调用方如果处在**同步 Tauri 命令**里，一并改为 async + spawn_blocking。
- 为什么：主线程 `block_on` 一把可能被长时间持有的锁，是「整个窗口冻结」的直接原因。即使 S1-1 修好了，只要以后锁内又出现慢操作，冻结还会重演，所以要在结构上消除。

**S1-6 前端查询节流**
- 位置：`src/lib/query/queries.ts:33-60`、`src/lib/query/pi.ts:26-32`。
- 做法：`useProvidersQuery` 与 `usePiCurrentState` 设置 `staleTime: 5_000`（切页与聚焦的突发请求在 5 秒内去重）；保留 `refetchOnWindowFocus`，确保「在外部改了 `models.json`，切回窗口即可看到」的契约行为。所有 mutation 已经会 `invalidateQueries`，不受 staleTime 影响。
- 为什么：后端修好之后，这一步是锦上添花；但它能把「一次聚焦 = 2~3 次后端全量同步」降到 1 次。

### 4.3 验收

- 单测（`pi.rs` 的 `plaintext_pending_tests` 模块旁新增 `list_perf_tests`）：
  - 1P 模式（`CountingVault`），`models.json` 有 5 个供应商（都是 `$VAR` 引用 + 非敏感 baseUrl），连续 `list` 3 次：`fetch_count()==0 && put_count()==0`。
  - 第 2、3 次 `list`：update_hook 计数 == 0（指纹短路 + 无变化不写）。
  - 在外部修改其中一个的 baseUrl 后 `list`：缓存更新，`endpoint_vault_pending == ["pi/<id>"]`，vault 仍然 0 次往返。
  - 凭据管理器模式同样的场景：base_url 变化时写入 vault（1 次 put），没变时 0 次。
  - `PiStateService::current` 在另一线程持有 Pi 锁时，**不阻塞**并能返回结果。
- 真机：1P 模式、5 个 Pi 供应商，点击 Pi 页签到列表渲染完成 ≤ 300 ms；列表加载期间窗口可以拖动、可以切换其他页签；日志里没有 `op item get`。

---

## 5. 同步与 SQL 导入导出的数据边界（S2–S6）

### 5.1 数据分级（本方案的核心定义）

新增 `src-tauri/src/database/snapshot_policy.rs`，把下列分级写成常量加函数，作为**唯一来源**：

| 级别 | 内容 | 导出（同步 + SQL） | 导入（同步 + SQL） |
|---|---|---|---|
| **A 共享配置** | providers、mcp_servers、prompts、skills、skill_repos、profiles；settings 中除 B 级以外的键 | 带 | 远端 / 文件覆盖本机 |
| **B 设备本地** | settings 键：`managed_env_vars`、`known_secret_targets`、`secrets_migration_pending`、`secrets_migration_report`、`secrets_migration_confirmed`、`live_reapply_pending`、`legacy_secret_recovery`、`skills_ssot_migration_pending`、`skills_ssot_migration_snapshot`、`official_providers_seeded`、`global_proxy_url`、`global_proxy_url_invalidated`、`log_config`、`current_profile_id_*`（前缀）。最终名单见 D-S2 | **不带**（在副本上 DELETE） | **保留本机值**：丢弃文件里的这些行，把本机行原样拷回；本机没有的就保持没有 |
| **C 凭据缓存** | provider_endpoints | 按**导出方**模式：1P → 不带；凭据管理器 → 带（D-S1） | 按**导入方**模式：1P → 忽略文件内容，保留本机缓存（再清理孤儿）；凭据管理器 → 合并（文件覆盖同键，本机独有的保留） |
| **D 引用** | secret_refs | 只带 `vault_id != ''` 的行（1P 引用；凭据管理器的占位引用对其他设备没有意义）（D-S3） | 保留本机行；本机没有、且满足「本机是 1P 模式 + 行的 `vault_id` 等于本机配置的保险箱 + 供应商存在于导入后的 providers」的，**采纳**文件里的行；最后删除「供应商已不存在」的本机孤儿行 |

说明：

- 「模型设置」全都在 `providers.settings_config` 里（Claude 的 `env.ANTHROPIC_MODEL*`，Codex 的 `config` TOML 里的 `model` / `model_provider` / `[model_providers.*]`，Pi 的 `models[]`），属于 A 级，本来就在同步。本方案**不需要新增「只同步模型」的开关**，只需要把 B/C/D 级处理对。
- 已知边界（写进手册，不在本期处理）：Codex 非当前 `[model_providers.*]` 表里的 `base_url`（`extractor.rs:509-530` 只剥当前表）、Pi `models[].baseUrl`（`extractor.rs:569-580` 保留并告警）、`common_config_*` 片段里用户自己写的 URL，仍属于 A 级配置文本，会随同步走。
- 为什么 C 级按模式区分，而不是一律设为本机：凭据管理器模式没有「自带同步」的真源，端点随同步走是 D3-A 给这类用户的便利，保留它符合「不回归」原则（§2.1-5）；1P 模式下 1P 就是真源，缓存出本机既多余又会造成 P0-3。

### 5.2 S2：标题兜底核对归属（P0-1，必须先于 S3/S4）

- 位置：`secrets/onepassword.rs:399-407`。
- 做法：标题唯一命中后，**用已经拿到的 `raw`**（不增加 op 调用）判断归属：
  1. 读取 `parse_item_group_field(&raw)`：有值且 `== group_field_value(group)` → 命中；有值但不相等 → 视为 `NotFound`，继续尝试下一个候选标题；
  2. 没有 group 字段（旧格式条目）：只有当命中的标题是**旧格式结构化标题**（`parse_group_from_title(title) == Some(group)`）时才接受；显示名标题 + 没有 group 字段 → 视为 `NotFound`。
  3. 被拒绝时 `log::warn!` 记录「标题撞名但归属不符（仅结构定位，不含值）」。
- 同样的核对应用到 `put` 的标题兜底路径（如果 put 复用了 `fetch_item_resolved`，就自动覆盖；否则单独补上）。
- 测试：
  - 两个 app 的同名供应商，Pi 没有引用行 → Pi fetch 返回 `None`，不读 Claude 的条目，`secret_refs` 不被写入错误的 id；
  - 旧格式条目（没有 group 字段、标题为 `cc-switch/pi/x`）→ 仍能命中；
  - 显示名命中但 group 不符 → 继续尝试旧格式标题，命中正确条目。
- 为什么：S4 会让「没有引用行」的供应商大量出现（同步下来的新供应商），标题兜底会被频繁使用。不先堵住这里，同步改造会直接放大串钥匙风险。

### 5.3 S3：导出策略（P0-2、P0-5 导出侧）

**S3-1 新增 `SnapshotPolicy` 并在内存副本上裁剪**
- 位置：新增 `database/snapshot_policy.rs`；修改 `backup.rs:105-118`（两个 `export_sql_string*`）。
- 做法：
  ```text
  pub(crate) enum ExportPurpose { Sync, ConfigFile }   // 云同步 / 手动 SQL 导出
  pub(crate) struct ExportMeta { backend: SecretBackendKind, endpoints_included: bool, refs_included: usize, device: String }
  pub(crate) fn prune_for_export(snapshot: &Connection, purpose, local_backend) -> Result<ExportMeta, AppError>
  ```
  在 `snapshot_to_memory()` 得到的内存连接上执行：
  1. `DELETE FROM settings WHERE key IN (B 级键) OR key LIKE 'current_profile_id_%'`；
  2. 本机为 1P 模式：`DELETE FROM provider_endpoints`；
  3. `DELETE FROM secret_refs WHERE vault_id = ''`；
  4. 然后调用现有的 `dump_sql(&snapshot, &[])`（**同步导出不再依赖 `SYNC_SKIP_TABLES`**，删掉这个常量，或者只作为兼容别名留到下个版本）。
- 施工前核实：内存快照连接**没有注册** update_hook（`register_db_change_hook` 只在主库 `init` 里调用）。在副本上 DELETE 不会触发自动同步。写一个测试确认这一点。
- 为什么：在副本上裁剪，不需要改 `dump_sql` 的生成逻辑（它有大量格式和兼容测试，`backup.rs:1695-2180`），风险最小；裁剪规则集中在一个函数里，同步和手动导出共用。

**S3-2 导出文件头追加 meta 行**
- 位置：`backup.rs:709-711`（头部三行注释之后）。
- 做法：追加一行 `-- cc-switch-meta: {"v":1,"purpose":"sync|config","secretBackend":"onepassword|credential_manager","endpoints":false,"refs":3,"device":"<normalize_device_name>","exportedAt":"<rfc3339>"}`。JSON 单行、不含任何秘密；设备名复用 `sync_protocol.rs:686` 的 `normalize_device_name`（不能含换行，否则会破坏注释行）。
- 导入侧新增 `fn parse_export_meta(sql: &str) -> Option<ExportMeta>`：只扫描前 10 行，解析失败返回 `None`（旧文件没有这一行）。
- **meta 只用于 UI 提示和统计**（§2.2-4），不参与任何保留或采纳的决策。
- 为什么：导入确认框需要告诉用户「这份文件来自哪台设备、是不是 1P 模式导出的、有没有带端点」；放在 SQL 注释里，不需要改同步协议和 manifest，旧客户端会直接忽略。

**S3-3 自动同步触发表去掉 `provider_endpoints`（1P 模式下）**
- 位置：`sync_protocol.rs:63-77`。
- 做法：保留 `provider_endpoints` 在列表里（凭据管理器模式仍需要），但 1P 模式下自动同步本来就整体跳过（`webdav_auto_sync.rs:113`），所以不需要改。只需更新注释：「D-S1：1P 模式不导出端点；该表仍触发是为了凭据管理器模式」。同步更新测试 `sync_protocol.rs:781-812` 的注释。

**S3-4 导出护栏补强（P2-1）**
- 位置：`secrets/scan.rs:42-53`。
- 做法：
  - `sk-[A-Za-z0-9]{16,}` 改为 `sk-[A-Za-z0-9_-]{20,}`；新增 `AIza[0-9A-Za-z_-]{35}`、`gh[pousr]_[A-Za-z0-9]{36,}`、`github_pat_[A-Za-z0-9_]{22,}`、`glpat-[A-Za-z0-9_-]{20,}`。
  - 每条新规则都要跑一遍现有测试和 fixture，确认不会误伤（例如 `sk-` 开头的模型名 `sk-…`）。如果误伤，把长度门槛提高到 32。
  - 1P 模式导出后新增**结构断言**：dump 文本里不得出现 `INSERT INTO "provider_endpoints"`（防止以后有人改回来）。
  - `extra_env` 的值也调用 `note_session_secret`（`mod.rs:2258-2260` 目前只登记了 api_key）。
- 为什么：D3-B 之后 SQL 导出和同步是钥匙泄出本机的唯一旁路，出口护栏要覆盖主流厂商的钥匙格式。

### 5.4 S4：导入合并策略（P0-3、P0-4、P0-5 导入侧、P1-1、P1-2、P1-7）

**S4-1 统一的导入合并入口**
- 位置：`backup.rs:170-251`：把 `preserve_tables: &[&str]` 参数替换为 `policy: ImportPolicy`（新增在 `snapshot_policy.rs`）。
- 做法：在现有的「主库锁内、整库替换之前」那一段（`:229-244`，`restore_tables` 所在位置）调用：
  ```text
  pub(crate) fn merge_for_import(main: &Connection, staging: &Connection, local_backend, local_vault: Option<&str>) -> Result<ImportReport, AppError>
  ```
  在一个事务里对 `staging` 依次执行：
  1. **B 级 settings**：`DELETE FROM staging.settings WHERE key ∈ B`，再把 `main` 中这些键的行插入 staging。
  2. **C 级端点**：
     - 本机为 1P：`DELETE FROM staging.provider_endpoints`，把 `main` 的全部行拷入，再删除 staging.providers 中已不存在的 `(app, provider_id)`；
     - 本机为凭据管理器：先把 `main` 中「staging 没有同键行」的记录补进 staging（远端优先、本机独有保留），再删孤儿。
  3. **D 级引用**：先把 staging 里的 secret_refs 读到内存（远端候选），`DELETE FROM staging.secret_refs`，拷入 `main` 的全部行；然后对每个远端候选，满足以下全部条件才插入：本机为 1P、`vault_id == local_vault`、本机没有同键行、供应商存在于 staging.providers。最后删除 staging 中「供应商已不存在」的引用行。
  4. `ImportReport { adopted_refs, unlinked_providers: Vec<(app,id)>, pruned_refs, pruned_endpoints, meta: Option<ExportMeta> }`。`unlinked_providers` = 导入后存在、但本机既没有引用、也没有采纳到引用的 1P 模式供应商（官方供应商和不需要钥匙的类别除外，复用 `mod.rs:1932-1948` 的判定）。
- 同步导入（`import_sql_string_for_sync`）、手动 SQL 导入（`import_sql_string`）、`.db` 恢复（`restore_from_backup`）**三条路径都走这个入口**。
  - `.db` 恢复是本机自己的备份：B 级键保留本机当前值（备份时的 `live_reapply_pending` 等标记不应复活），C/D 级按同样的规则处理。施工时在 `restore_from_backup_with_hook`（`:1001`）的暂存库阶段调用。
- `local_vault` 取 `settings.onepassword.vault`（与 `OnePasswordVault::ref_item_id` 比较时用的是 `self.vault`，见 `onepassword.rs:243`，必须用同一个值；施工时核实 `self.vault` 的来源）。
- 为什么：
  - 同一个保险箱的 item id 在两台设备上一模一样（1P 自己同步），直接采纳可以把「下载后逐条重建」的 N+1 次 op 降到 **0 次**；
  - 即使采纳的 id 已经过期（条目被删或被归档），`fetch_item_resolved` 的 id 直达失败后会走标题兜底（S2 已经加上归属核对），能自我修复；
  - 文件不可信：只有 vault 匹配时才采纳，不匹配的一律丢弃，本机行永远优先。

**S4-2 `fetch_provider_secrets` / `resolve_base_url` 在 1P 模式下以 vault 为准**
- 位置：`mod.rs:1464-1472`、`mod.rs:2295-2327`。
- 做法：
  - `fetch_provider_secrets`：1P 模式下如果 vault 整包里有 `base_url`，就**用 vault 的值**；如果与缓存不同，更新缓存（非敏感 URL）或删除缓存（敏感 URL），并记日志（只记「端点缓存已按 1Password 更新」，不记 URL）。vault 没有 base_url 时才回落到缓存。凭据管理器模式维持现状。
  - `resolve_base_url`：维持「缓存命中 = 0 次 op」（列表、Codex 切换的性能依赖这一点），但缓存只可能由本机写入（S4-1 保证导入不会带进外来的缓存），加上本条让每次 fetch 都顺带校正缓存，分歧窗口被收窄到「另一台设备改了 URL → 本机下一次 fetch 之前」。
  - 列表 `commands/provider.rs:54-61` 保持只读缓存（0 次 op）。
- 施工前风险确认：D3-A 时期（F1-2 → 8703c42），1P 条目里可能留有**拆分之前的旧 base_url 副本**（`mod.rs:2293` 的注释「vault 里的旧副本留给回填 / 后续 put 清理」）。如果改成「vault 优先」，会让这些旧副本复活。
  - 缓解：2.3.0 尚未发布（D15），D3-A 时期只存在于开发机上。施工者在开发机上先跑一次只读诊断（S4-5 的「端点对账」），确认没有分歧；有分歧的，先用「以缓存为准写回 1P」修一次。
  - 这一步写进验收清单（§8.2），不能跳过。
- 为什么：D3-B 把 1P 定为真源；在多设备场景下，「缓存永远优先」等于让本机的旧值永久压过 1P 的新值（P0-3）。

**S4-3 导入后处理：未关联供应商的提示与一键关联**
- 后端：同步下载、SQL 导入、`.db` 恢复的返回值都增加 `unlinkedProviders: number` 和 `adoptedRefs: number`（来自 `ImportReport`）。1P 模式下把 `unlinked_providers` 写进本机 `settings.json: onepassword_unlinked`（全量重建语义）。
- 前端：`PlaintextPendingBanner`（或新建 `UnlinkedProvidersBanner`）显示「N 个供应商来自其他设备，尚未关联 1Password 条目」，按钮「从 1Password 关联」→ 调用**只针对这 N 个**的重建（见 S4-4），带进度。
- 列表徽标：`load_secret_status`（`commands/provider.rs:45-79`）对 `onepassword_unlinked` 里的供应商返回新的状态 `linked: false`，前端显示「未关联」而不是「缺钥匙」；「必须填 API Key」校验（`mod.rs:1932-1948`）对未关联的供应商给出专门的错误码 `vault_unlinked`，提示先关联，而不是要求用户重新输入钥匙。
- 为什么：「缺钥匙」会误导用户重新输入钥匙，导致 1P 里出现重复条目；「未关联」准确说明了状态，并给出不需要重新输入的修复路径。

**S4-4 「从 1Password 重建引用」增强**
- 位置：`commands/onepassword.rs:268-336`。
- 做法：
  1. 增加参数 `only: Option<Vec<(app,id)>>`：给定时只处理这些供应商（用 `list_tagged_items` 一次拿到全部条目，按 `cc-switch-group` / 旧标题过滤后，只对目标条目执行 `read_item_meta`）。
  2. `read_item_meta` 顺带解析**非 CONCEALED 的 `base_url` 字段值**（D3-B 下非敏感 URL 是可见的 STRING 字段，不带 `--reveal` 也能读到），回填 `provider_endpoints`，不增加 op 调用。改名为 `read_item_meta_and_endpoint`，返回值加一个 `Option<String>`；解析失败或是 CONCEALED 时返回 `None`。
  3. 全量重建时（`only == None`）删除「本机 secret_refs 中 vault 匹配、但在 `list_tagged_items` 结果里找不到对应 item」的行（当前只 upsert、从不删除，见 P1-1）。
  4. 成功后从 `onepassword_unlinked` 里移除对应项。
- 可选优化（需要真机验证后再决定，写进 D-S7）：`op item list --format json | op item get - --format json` 支持从 stdin 批量读取条目。如果真机上批量读取明显快于逐条读取，就把逐条 `read_item_meta` 改成一次批量调用。**没有真机数据不要改。**
- 为什么：把「关联」变成用户主动触发、精准、一次拿到引用和端点的动作。

**S4-5 「端点对账」诊断（只读）**
- 新增命令 `onepassword_endpoint_audit`（async + spawn_blocking + 进度）：对每个有引用的供应商 fetch 一次，比较 vault 与缓存的 base_url，返回分歧清单（只返回 app/id 和「一致 / 不一致 / 1P 缺失 / 缓存缺失」，**不返回 URL 本身**）。UI 放在 1Password 设置区的「诊断」折叠项里，提供「以 1Password 为准」「以本机缓存为准写回 1Password」两个按钮。
- 为什么：S4-2 的施工前风险确认需要它；日后用户怀疑端点不对时，也有一个自查入口。

**S4-6 scrub 只处理确实有变化的行（P1-7）**
- 位置：`mod.rs:2338-2416`。
- 做法：只有当 `extracted.stripped != provider.settings_config` 时，才放进 `stripped_rows`；只有当至少一行被剥离时才 `VACUUM`。
- 测试：导入一个完全干净的快照 → 0 次 UPDATE（用 update_hook 计数断言），不执行 VACUUM。

**S4-7 下载后 Pi 模型设置生效（P1-6，决策 D-S6 默认 A）**
- 位置：`sync_support.rs:8-37` `run_post_import_sync`；新增 `pi::apply_imported_configs_to_native(state, before: &IndexMap<String, Provider>)`。
- 做法：
  1. 导入之前，在命令层先读一份本机 Pi DB 行（`get_all_providers("pi")`，0 次 op）作为 `before`；
  2. 导入之后，对「`models.json` 里存在（已启用）并且 DB 行在导入前后发生变化」的每个 Pi 供应商：用 `hydrate_pi_base_url_for_live`（端点缓存；1P 模式下缓存缺失就**跳过该供应商**，记入 warning，不调 op）→ `pi_sanitizer::sanitize_pi_provider_for_live_write` → `pi_config::replace_pi_provider(id, 当前原生节点, 新节点)`（revision 比较，冲突就跳过并告警）；
  3. 全程持有 Pi 切换锁；完成后使 `models.json` 指纹失效（S1-4）。
- 为什么：下载是用户明确要求「用远端覆盖本机」，Pi 的模型列表如果不写进 `models.json`，下一次列表就会被原生同步改回去，用户看到的就是「同步了但没生效」。只处理「已启用且有变化」的供应商，未启用的供应商保持只存在于 DB，符合原生契约「启停即增删节点」。

### 5.5 S5：同步冲突与回声（P1-3、P1-4、P1-5、P2-6）

**S5-1 上传前的「本机状态过期」检查**
- 位置：`webdav_sync.rs:110-175`、`s3_sync.rs:93-163`（v3），以及两者的 v2 上传。
- 做法：
  - v3：拿到远端外层 manifest 之后，如果 `remote.seq > max(status.last_applied_seq, status.last_uploaded_seq)`，就返回新的错误码 `sync.remote_ahead`（「远端有本机尚未下载的更新」），不上传。
  - v2：上传前 GET 远端 manifest，计算其哈希；如果远端存在，且哈希既不等于 `status.last_remote_manifest_hash`（本机上次上传或下载时记录的值；字段不存在就新增到 `settings.rs:95-117` 的 status 里），也不等于本机将要上传的 manifest 的哈希，就返回 `sync.remote_ahead`。
  - 两个上传命令都增加参数 `force: Option<bool>`；前端收到 `remote_ahead` 时弹框：「先下载（推荐）」/「强制覆盖远端」/「取消」。
  - 自动上传遇到 `remote_ahead` 时不强制，只记录状态并通知 UI。
  - S3 首次上传（远端无 etag）也要执行这项检查（远端 manifest 不存在，才视为首次）。
- 为什么：同步只带配置之后，快照里就是用户的模型和各项设置，被旧设备覆盖就等于丢数据；「最后写入者胜」对单用户多设备场景并不安全。

**S5-2 回声抑制覆盖后处理，并且跨传输生效**
- 位置：`commands/webdav_sync.rs:67-74`、`commands/s3_sync.rs` 对应位置；`webdav_auto_sync.rs:19-42`、`s3_auto_sync.rs`。
- 做法：把两个 `AUTO_SYNC_SUPPRESS_DEPTH` 合并成 `sync_protocol.rs` 里的一个全局抑制计数（两个 auto_sync 模块都读这一个）；命令层把 `AutoSyncSuppressionGuard` 的作用域扩大到 `project(result).await` 结束。手动 SQL 导入和 `.db` 恢复也包上同一个守卫。
- 同时删除或更新断言「后处理期间不抑制」的测试（`commands/webdav_sync.rs:311-314`），并在注释里写明这是有意改变的行为及原因。
- 为什么：下载后的写入不是「用户改了配置」，不应该触发上传；跨传输不抑制会让 WebDAV 下载的数据被 S3 自动上传一遍，反之亦然。

**S5-3 回滚冲突跳过后处理；后处理各步独立执行**
- 位置：`commands/webdav_sync.rs:162-173`、`commands/s3_sync.rs:165-176`、`import_export.rs:202`、`:290`。
- 做法：
  - 下载结果是 `rollbackConflict` 时直接返回，不执行 scrub 和 live 同步；
  - scrub 与 `run_post_import_sync` 改为各自执行、汇总 warning（把 `.and_then` 改成两次独立调用，结果合并）；
  - 手动导入时 scrub 失败**不再让整个命令返回失败**（主库已经被替换，返回失败会误导用户），而是作为 warning 返回，并给出 `secrets_import_pending` 的数量。

**S5-4 1P 模式下的「未上传改动」提示（P2-6）**
- 做法：update_hook 触发、且本机处于 1P 模式（自动同步被跳过）时，在内存里置一个 `dirty_since_last_upload` 标记（进程级 `AtomicBool`，上传成功后清零），并通过事件 `sync-dirty-changed` 通知前端；`WebdavSyncSection` / S3 区显示「有未上传的改动（1Password 模式下自动同步已暂停，请手动上传）」。
- 为什么：D4 的取舍（1P 模式下不做后台自动同步）是对的，但用户需要知道「现在需要手动上传」。

### 5.6 S6：手动 SQL 导入导出体验（P0-4 前端侧、P1-8、P2-2、P2-3、P2-7）

**S6-1 导出**
- `backup.rs:128` 改为 `atomic_write_private`（P2-2）。
- 默认文件名改为 `cc-switch-config-<yyyyMMdd>.sql`（体现这是「配置导出」，不是本机完整备份）。
- `ImportExportSection.tsx` 导出按钮旁加说明：
  - 1P 模式：「导出供应商、模型、MCP、提示词、Skills 等配置；钥匙与端点在 1Password 中，不包含在文件里。本机完整备份请使用下方数据库备份。」
  - 凭据管理器模式：「钥匙不包含在文件里；换设备请配合『凭据便携包』一起使用。」（这正是手册缺少的「SQL + 便携包」配对说明，见 S7-3）

**S6-2 导入**
- 新增命令 `preview_sql_import_via_dialog`：选择文件 → 只读取文件头，解析 meta（S3-2）并做头部校验，返回 `{ path_token, meta, sizeBytes }`。`path_token` 是后端内存里的一次性 token，映射到用户选中的路径，**路径不回传前端**（沿用 S-2「路径不经前端往返」的约定，`import_export.rs:131-134`），10 分钟后过期。
- 前端弹确认框：显示来源设备、导出时间、导出方后端、是否含端点、引用条数，以及「将覆盖本机的供应商 / MCP / 提示词 / Skills 等配置；本机钥匙、1Password 关联、本机状态不受影响；导入前会自动备份当前数据库」。确认后调用 `import_config_confirmed(path_token)`。
  - 没有 meta 的旧文件显示「旧版导出文件（可能来自其他设备），将按本机规则合并」。
- 导入结果显示后端返回的 `warning`、`adoptedRefs`、`unlinkedProviders`；删除前端重复调用的 `syncCurrentProvidersLiveSafe()`（`useImportExport.ts:73`），因为后端的 `run_post_import_sync` 已经做了（P2-3）。
- `ConfigTransferResult` 类型（`lib/api/settings.ts:11-16`）补上 `warning`、`adoptedRefs`、`unlinkedProviders`。

**S6-3 待导入明文拦截导入的问题（P1-8）**
- 导入和恢复开始前，如果 `secrets_import_pending` 非空，直接返回错误码 `import.plaintext_pending`：「有 N 个供应商的明文钥匙尚未导入 1Password，请先在横幅中处理」。不要等到安全备份的扫描失败才报一个看不懂的错。
- 新增命令 `retry_secrets_import_pending`（async + spawn_blocking）：对清单里的供应商重跑 `scrub_imported_plaintext_via_vault` 的单行逻辑；`settings.rs:1273-1278` 的 `get_secrets_import_pending` 去掉 `dead_code` 标记，改为正式接口。
- `PlaintextPendingBanner.tsx:94`：「导入暂留」也显示重试按钮；导入 / 恢复 / 下载成功之后前端主动刷新横幅（目前只在挂载时读取一次，见 `:30-40`）。

**S6-4 备份区的 1P 提示与文档修正**
- `BackupListSection.tsx`：1P 模式下说明「数据库备份不含钥匙；恢复后本机的 1Password 关联会保留」。
- 手册修正 P2-7：二选一——给 `cleanup_db_backups` 加上 `pre-secrets-migration` 前缀豁免（推荐，因为这类备份是迁移回退点），或者修改手册的说法。见 D-S8。

### 5.7 S7：收尾

> **✅ 已于 2026-09-28 完成**（001e699 / ba57bc1 / 文档提交）。S0–S7 全部施工完毕，剩余 §8.2 真机验收第 2–7 项由用户执行。
> 落地差异：S7-4 的键名以实际实现为准（`settings.importPreview.*`、`settings.webdavSync.remoteAhead.*`、`settings.s3Sync.dirtySinceUpload`、`onepassword.unlinkedPending`、`onepassword.endpointAudit*`、`onepassword.endpointVaultPending` 等），四语键集一致性校验通过（1666 键零差异）；§8.2 增补的 vault id 有效性校验随 S7-2 一并落地（对账前 `op vault list` 预检）。

- **S7-1 S3 一致性（P2-4）**：补降级检查、Legacy 回退、有效 `dbCompatVersion`、`sourceLayout`；恢复被注释掉的 S3 测试（改用本地 mock，不依赖网络）。可以单独成一个提交，优先级最低。
- **S7-2 端点回填待办（P2-5）**：`secrets_backfill_endpoints` 遇到带凭据的 URL 时，把 `app/id` 记入 `settings.json: endpoint_backfill_sensitive`，`list_endpoint_backfill_pending` 排除这些项。
- **S7-3 文档**：
  - `docs/user-manual/zh/4-credentials/4.1-1password.md` 新增「多设备：1Password 同步钥匙和端点，CCS 云同步 / SQL 只同步配置」一节，写清数据分级表（§5.1 的精简版）、「未关联」的含义与处理、端点对账；
  - `1.5-settings.md` 修正导入流程描述，并补上「凭据管理器模式换设备 = SQL + 便携包」的配对说明；
  - `SECURITY.md` 的 1P 专节补一句：「1P 模式下 base_url 不随 CCS 同步 / 导出离开本机」；
  - `CHANGELOG.md` 记在 2.3.0 节下（D15：没有 v2.3.0 tag，并入 2.3.0），**特别写明：所有设备需要同时升级**（见 D-S5）。
- **S7-4 四语 i18n**（zh / zh-TW / en / ja）：新增 `sync.remoteAhead*`、`sync.dirty1P`、`import.preview*`、`import.plaintextPending`、`onepassword.unlinked*`、`onepassword.endpointAudit*`、`pi.endpointVaultPending*` 等键，跑键集一致性校验。

---

## 6. 施工顺序与提交粒度

| 阶段 | 内容 | 依赖 | 提交建议 | 阶段出口 |
|---|---|---|---|---|
| S0 | 测试护栏：update_hook 计数测试工具、`ImportPolicy` / `ExportPurpose` 的空实现、各 P0 的失败复现测试（标 `#[ignore = "S<n> 待修"]`，修复时去掉） | — | 1 个 | `cargo test` 全绿（被 ignore 的除外） |
| **S1** | Pi 卡顿：S1-1 → S1-3 → S1-5 → S1-4 → S1-6 → S1-2 | S0 | 每项 1 个 | §4.3 单测通过；**用户真机验收 Pi 页响应** |
| S2 | 标题兜底归属核对 | S0 | 1 个 | P0-1 复现测试转绿 |
| S3 | 导出策略 S3-1 → S3-2 → S3-4（S3-3 只改注释） | S2 | 每项 1 个 | 1P 导出不含端点 / B 级键 / 占位引用 |
| S4 | 导入合并 S4-1 → S4-6 → S4-2（先跑 S4-5 诊断）→ S4-4 → S4-3 → S4-7 | S3 | 每项 1 个 | 双设备模拟测试（§8.1）通过；**用户真机验收 1P 双设备** |
| S5 | S5-2 → S5-3 → S5-1 → S5-4 | S4 | 每项 1 个 | 回声测试、`remote_ahead` 测试通过 |
| S6 | S6-1 → S6-3 → S6-2 → S6-4 | S4、S5-3 | 每项 1 个 | 前端 vitest + 手动验收 |
| S7 | S7-1～S7-4 | 全部 | 按主题 | 文档、i18n 键集校验 |

每个提交：`cargo fmt`、`cargo clippy -- -D warnings`、`cargo test`、`pnpm test`、`pnpm typecheck` 全部通过；提交信息沿用仓库的 emoji + 中文风格，并注明阶段号。

---

## 7. 决策表（带默认值；用户未表态时按默认施工）

| 编号 | 问题 | 默认 | 备选 | 理由 |
|---|---|---|---|---|
| D-S1 | 端点缓存的同步策略 | **A：按模式区分**——1P 模式不导出，导入时忽略；凭据管理器模式照旧随同步 | B：两种模式都当作本机数据 | 凭据管理器模式没有自带同步的真源，保留它才不回归；1P 模式下 1P 就是真源 |
| D-S2 | B 级设备本地键名单是否包含 `global_proxy_url`、`log_config`、`current_profile_id_*` | **包含**（代理是网络环境相关的，日志级别是本机偏好，当前 Profile 与本机当前供应商配套） | 只包含与凭据和迁移相关的键 | 这三项跨设备同步的收益小；覆盖之后本机行为会莫名其妙地改变 |
| D-S3 | 是否在同步 / SQL 里带 1P 引用并在导入时采纳 | **带，且只在 vault 相同时采纳**（修改 D14） | 维持 D14（引用完全本机），下载后提示用户重建 | 同一保险箱的 item id 在两台设备上相同，采纳可以把 N+1 次 op 降到 0；不匹配的丢弃，id 过期也能自愈 |
| D-S4 | 手动 SQL 导出是否保留一个「完整导出」选项 | **不保留**；本机完整快照交给 `.db` 备份 | 在导出时提供「完整 / 配置」二选一 | 语义单一：SQL = 可以跨设备的配置，`.db` = 本机完整快照；少一个容易选错的开关 |
| D-S5 | 是否升级 `DB_COMPAT_VERSION` | **不升**；CHANGELOG 要求所有设备同时升级 | 升到 7，强制旧客户端拒绝新快照 | schema 没变；升级会切换 v2 的远端目录（`db-v7`），造成迁移负担。已知代价：旧客户端下载新快照时，会丢掉自己的 B 级行（以前是被别的设备的值覆盖，同样是错的） |
| D-S6 | 下载后是否把 Pi 的配置写回 `models.json` | **A：只写回「已启用且有变化」的供应商**；1P 模式下端点缓存缺失就跳过并告警 | B：维持原生为准，在 UI 提示「Pi 配置未应用」 | 不写回的话，Pi 的模型设置事实上无法同步 |
| D-S7 | 重建引用是否改用 `op item get -` 批量读取 | **先真机测量，再决定**；没有数据不改 | 直接改 | op 的耗时构成（进程启动 / 鉴权 / 网络）没有测过 |
| D-S8 | `pre-secrets-migration` 备份是否豁免自动清理 | **豁免**（改代码） | 改手册 | 这是迁移的回退点，被轮换删除之后无法恢复 |
| D-S9 | Pi 外部修改 baseUrl 后，1P 模式下何时写回 1P | **推迟**到用户下一次主动触发 op 的动作时（`endpoint_vault_pending`） | 列表时立即写（会重新引入 op） | 原则 1：列表 0 次 op |

---

## 8. 验收清单

### 8.1 单测 / 集成测试（必须有）

1. **S2**：跨 app 同名供应商，没有引用行的一方 fetch 返回 `None`，`secret_refs` 不被写入；旧格式标题条目仍然命中；group 不符时继续尝试下一个候选标题。
2. **S3**：
   - 1P 模式导出（同步 + 配置两种用途）：dump 中没有 `provider_endpoints` 的 INSERT、没有 B 级键、没有 `vault_id=''` 的引用；有 meta 行，且可以被解析；
   - 凭据管理器模式导出：端点**仍然存在**（不回归）；
   - 在内存快照上 DELETE 时，主库 update_hook 计数为 0。
3. **S4（双设备模拟）**：两个 `Database::memory()` 分别代表设备 A 和 B，共用同一个 `InMemoryVault`（模拟同一个保险箱）：
   - A 新增供应商 X 并导出，B 导入：B 获得 X 的配置，采纳了 X 的引用（`adopted_refs == 1`），B 本机的 B 级键与端点不变，`CountingVault` 在导入与后处理期间 0 次往返；
   - vault 不同：不采纳，`unlinked_providers == [X]`；
   - A 删除供应商 Y 后导出，B 导入：B 的 Y 引用行和端点行被清理；
   - 旧格式全量 SQL（包含端点、B 级键、占位引用）导入 1P 设备：全部按规则处理，不报错；
   - `.db` 恢复：本机当前的 B 级键保留（备份里的 `live_reapply_pending=1` 不会复活）；
   - `fetch_provider_secrets` 在 1P 模式下，vault 有 URL 且与缓存不同时返回 vault 的值，并校正缓存；凭据管理器模式维持缓存优先；
   - 干净快照导入后 scrub 0 次 UPDATE、不执行 VACUUM；
   - Pi 写回：已启用且有变化 → `models.json` 被更新；未启用 → 不写；1P 模式缓存缺失 → 跳过并返回 warning，vault 0 次往返。
4. **S5**：
   - v3 `remote.seq > max(applied, uploaded)` → `remote_ahead`；`force=true` 时放行；
   - v2 远端 manifest 哈希与记录不符 → `remote_ahead`；
   - 下载 + 后处理期间，两个传输的自动同步都被抑制（计数为 0）；
   - `rollbackConflict` 时不执行 scrub 和 live 同步。
5. **S6**：`preview` 返回 meta、不返回路径；token 一次性并且会过期；存在 `secrets_import_pending` 时导入返回 `import.plaintext_pending`；`retry_secrets_import_pending` 成功后清单清空。
6. **S1**：见 §4.3。

### 8.2 真机验收（用户执行，施工模型提供步骤）

> **✅ 单机部分已于 2026-09-28 全部通过**（2.3.0 MSI：端点对账含 vault id 预检、Pi 响应、手动 SQL 导出内容核验与导入确认框、钥匙剥离）。双设备项（3～5）留待实际换机时验证；GitHub 秘密扫描告警确认为测试样本误报并已按 false positive 关闭。

1. **施工 S4-2 之前**：在开发机上跑「端点对账」，确认 1P 与缓存没有分歧；有分歧的先处理掉。
   - ✅ **已于 2026-09-27 完成**（op 2.39 CLI + SQLite 只读对账；首次对账误用了遗留数据目录，同日更正，见下）。
   - **重要背景**：本机存在两套数据目录——活跃目录 `<InstallDir>\data\`（安装版应用使用，注册表 `HKCU\Software\ccswitch\CC Switch\InstallDir` 定位）与遗留目录 `~/.cc-switch/`（2026-09-25 已迁移，留有 `.migrated-legacy-0` 标记；**dev 构建（exe 不在 InstallDir）会回落使用它**，见 `config.rs:263-294`）。
   - **活跃库对账结论（以此为准）**：
     - 活跃 `settings.json` 的 vault id **正确**（`pc77xj3pjxnmlupvohgsceyoly`）；
     - `secret_refs` 12 行中 **11 行与 1P 现存条目一一对应**、vault 一致、供应商存在；仅 1 行孤儿（`codex/default` → item `7nnoah…`，供应商与 1P 条目均已不存在）；
     - **端点对账：11 行缓存与 1P `base_url` 全部一致，不一致项为 0，无孤儿行** → 不存在 D3-A 遗留旧副本，S4-2 可施工；
     - 1P 侧 11 个条目全部对应活跃供应商，无孤儿条目；
     - 1P 中存在 3 个同名「南梁」跨 app 条目（claude/君的公益、codex/君的、pi/nl），P0-1 撞名场景在本机真实存在，S2 归属核对仍是硬前置。
   - **遗留目录问题（热补丁处理对象）**：`~/.cc-switch/settings.json` 的 vault 值已损坏（今天 18:46 被 dev 侧写入）、其 `cc-switch.db` 是陈旧快照（含已删除供应商 0u0/default/official/f1test、5 条指向已删除条目的失效引用、2 行孤儿端点缓存）。dev 构建回落使用该目录会造成双数据源分裂，建议归档。
   - ✅ **热补丁已于 2026-09-27 21:52 执行完毕**（纯数据修复，无代码变更）：
     - **H1**：遗留目录已改名归档为 `~/.cc-switch.migrated-20260927.bak`（不删除，可随时还原）。同时消除两个风险：dev 构建回落使用陈旧数据（施工期间 `cargo run` 会创建全新目录）；设置页「CC Switch 配置目录」override 误启用时指向坏数据（后端对不存在目录的 override 返回 `None`，自动失效，见 `app_store.rs:39-41`）。
     - **H2**：活跃库删除孤儿引用 `codex/default`（item `7nnoah…`，已用 `op item get` 确认 1P 中不存在）；执行前整库备份至 `data\backups\hotfix-pre-orphan-ref-cleanup-20260927-215006.db`。
     - **H3 复验全绿**：`secret_refs` 11 行全部有效（供应商存在、vault 一致、item 在 1P 存在）且与 1P 条目一一对应；`provider_endpoints` 11 行与 1P 真值全部一致、无孤儿行；应用重启正常。
     - **结论：S4-2 的施工前置条件全部满足。** S2 归属核对仍为硬前置（3 个同名「南梁」）；S4-5 诊断建议增补 vault id 有效性校验（本次 18:46 的 settings 损坏证明该故障真实会发生，现有诊断测不出）。
2. Pi 页签：1P 模式、≥5 个 Pi 供应商，点击响应 ≤ 300 ms，列表加载期间窗口不冻结，日志里没有 `op item get`。
3. 1P 双设备（同一个保险箱）：A 新增 Claude 供应商并改模型 → 手动上传 → B 手动下载：B 显示该供应商，钥匙状态为「已配置」（采纳引用），**全程不弹 1P 解锁**；B 打开终端时解锁一次，注入的钥匙和端点正确。
4. A 修改某个供应商的 base_url → B 下载后打开终端：使用的是新端点（S4-2 在 fetch 时校正缓存）。
5. B 在没有下载的情况下修改配置并上传 → 收到 `remote_ahead` 提示，选择「先下载」后正常。
6. 手动 SQL：1P 设备导出后用文本编辑器打开，确认没有 `provider_endpoints` 数据、没有 `managed_env_vars`；在另一台设备上导入时，确认框显示来源信息；导入后本机的 1P 关联仍在。
7. 凭据管理器模式回归：两台凭据管理器设备之间同步，端点照旧随同步生效；SQL + 便携包换设备流程完整可用。

---

## 9. 施工陷阱（务必逐条自查）

1. **不要在主库上做裁剪。** 导出只能在 `snapshot_to_memory()` 的副本上 DELETE；导入只能在暂存库上合并。在主库上 DELETE 会触发 update_hook → 自动上传一份残缺的快照。
2. **合并必须和整库替换在同一把主库锁、同一个时间点内完成**（`backup.rs:229-244` 的块内），否则暂存期间本机新写入的引用或端点会丢失（`:226-228` 的注释写明了为什么）。
3. **本机后端模式和保险箱要在持锁前读好**，不要在持有主库锁的块里调 `settings::get_settings()` 之外的任何可能调 op 的代码。合并阶段 **0 次 op**。
4. **文件里的 meta 不能参与安全决策。** 有人手工改 meta 写 `"secretBackend":"onepassword"`，也不能让导入方因此采纳引用或丢弃本机数据；采纳条件只看本机模式和 `vault_id` 是否匹配。
5. **S2 的归属核对不能增加 op 调用次数**：必须复用已经拿到的 `raw`。在 `CountingVault` / `OpRunner` 注入的测试里断言调用次数不变。
6. **`fetch_provider_secrets` 改为 vault 优先只适用于 1P 模式**；凭据管理器模式下凭据管理器里可能有 D3-A 之前的旧副本，维持缓存优先。
7. **敏感 URL（带凭据）绝不写入端点缓存**：S4-2 校正缓存、S4-4 从 1P 回填、S1-1 更新缓存，这三处都要调用 `is_credential_bearing_url` 过滤；敏感 URL 在 1P 里是 CONCEALED 字段，不带 `--reveal` 读不到，本来就不应该出现在回填里。
8. **`get_pi_current_state` 去掉锁之后**，前端 mutation 的失效逻辑必须同时失效 `piKeys.currentState` 和 `["providers","pi"]`（`invalidatePiProviderCaches` 已经这样做了，别改坏）。
9. **Pi 指纹短路要在所有「会改 Pi DB 行」的入口失效**，漏掉一个就会出现「DB 已被下载覆盖，但列表不再和原生对齐」。用一个 `pi::invalidate_native_fingerprint()` 函数，并在测试中覆盖每个入口。
10. **`remote_ahead` 检查不要误伤首次上传和「刚下载完马上上传」**：刚下载完，`last_applied_seq == remote.seq`，不能报错；首次上传（远端没有 manifest）不能报错。
11. **旧测试的调整**：`sync_skips_secret_refs_on_export_and_preserves_local_rows_on_import`（`backup.rs:2377`）会因为 D-S3 改变行为（开始导出 1P 引用）。要**改写测试并注明新规则**，不能直接删掉；`commands/webdav_sync.rs:311-314` 的「后处理不抑制」断言同理。
12. **测试隔离**：凡是动到 `settings.json` 的测试，都要设置 `CC_SWITCH_TEST_HOME`，并加 `#[serial]`（8703c42 已经在 `cfg(test)` 下对未设置时写 settings.json 加了保险丝，不要绕开它）。
13. **日志不写 URL、不写 item id 以外的条目内容**，沿用「仅结构定位，不含值」的写法。

---

## 10. 明确不做 / 诚实边界

- 不做「CCS 自己同步钥匙」：1P 模式下钥匙和端点只通过 1Password 跨设备；凭据管理器模式下通过便携包。
- 不做后台自动下载，也不做 1P 模式下的后台自动上传（D4 维持）。
- 不做字段级的三方合并：冲突时只提供「先下载 / 强制覆盖」，不做逐字段合并。
- 不处理 A 级配置文本里用户手写的 URL（Codex 非当前 provider 表、Pi `models[].baseUrl`、通用配置片段），只在手册里说明。
- 同一个 1P 账户但不同保险箱的两台设备之间，引用不采纳，需要用户手动「从 1Password 关联」；这是有意的安全边界。
