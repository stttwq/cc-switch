# 施工方案：1Password 后端接入审查修复（2.3.x）

状态：待施工
基线：`1password` 分支 @ 639ccbc（2.3.0：P0–P5 + 4 个实测修复提交）
上位文档：`docs/plans/1password-backend-impl-plan-zh.md`（下称「原方案」）。原方案的目标、诚实边界、原则与 D1–D10 决策继续有效；本文件只写「现有代码哪里不对、怎么改、为什么」。两者冲突时以本文件为准。

---

## 0. 一页纸结论

| 项 | 结论 |
|---|---|
| 总体评价 | 骨架方向正确且大部分已落地（`SecretVault` 整包、`secret_refs`、严格投递强制、`op` 调用规范、读失败向上传播）。但有 **8 个 P0 问题**会导致功能坏、钥匙丢失或安全门槛失效，合并/发布前必须修掉 |
| 最大单点 | **D3（base_url 放哪）没有真正落地**：代码仍把 base_url 当秘密写进 vault，但 Codex 写 live 与列表显示却从**迁移时已被清空的凭据管理器**读 → 1P 模式下 Codex 第三方供应商的 `config.toml` 丢 `base_url`（每次启动都会重写一次），钥匙会被发往默认 OpenAI 端点；列表端点全空 |
| 第二大单点 | `OnePasswordVault::put` 用「先归档删除、再新建」实现覆盖写，**非原子**。其依据（`op item edit` 只能用命令行赋值）是错的：本机 op 2.39.0 的 `op item edit --help` 明确支持 stdin 管道 JSON |
| 其它 P0 | 编辑供应商失败会删光其钥匙；Pi `models.json` 里的明文 key 在 1P 模式被静默丢弃；导入/同步/便携包导入在 1P 模式把明文写回凭据管理器；迁移后未重启窗口钥匙回流凭据管理器；op.exe 签名校验可被绕过；1P 模式每次启动整文件重写 live 吞掉用户改动 |
| 修复策略 | 6 个阶段：F0 测试护栏 → F1 止血（P0）→ F2 迁移与读路径 → F3 线程与命令边界 → F4 健壮性 → F5 文档与 UI。每阶段独立可编译、测试通过、可单独提交；**每个问题先写复现测试再改代码** |

---

## 1. 审查范围与已确认无误的部分

范围：`main...1password` 全部 48 个文件的 diff，及其调用链上未改动的代码（`lib.rs` 启动路径、`live.rs`、`pi.rs`、`cli/`、同步链路、`database/backup.rs`、`env_delivery/`）。对照原方案 §3 原则、§9.3 次数断言、§12 陷阱逐条核查。本机只做了只读实测（`op --version`、`op item edit --help`、op.exe Authenticode 签名），未调用任何需要解锁的命令，未改代码。

**已核实正确、施工时不要动的部分**：

- `provider_env_pairs` 已纯函数化，`fetch_provider_secrets` 是唯一取包入口，读失败向上传播（原方案 §1.4 吞错已修，且有回归测试）。
- 1P 模式强制严格投递：`strict_for` 恒真、`set_env_delivery_strict_mode(false)` / `set_env_delivery_strict_apps` 拒绝、`EnvDeliverySection` 置灰。
- `open_provider_terminal` / `run_provider_cli` 已 async + `spawn_blocking`；Pi 多供应商串行取包。
- `op` 调用规范：绝对路径、清 `OP_SERVICE_ACCOUNT_TOKEN`/`OP_CONNECT_*`、`create` 走 stdin、stdout 不入日志、stderr 只记分类、进程内串行锁、120 秒超时。
- 列表徽标 / extra_env 名单 / 缺钥匙校验查 `secret_refs`（0 次 op）。
- 1P 模式 KEK 不缓存（D6）；后台自动同步跳过（D4）。
- 迁移主干顺序：写入 → 回读校验 → 切后端 → 删凭据管理器 → 清注册表投递。

---

## 2. 问题清单

每条格式：位置 → 现象/后果 → 根因。行号以 639ccbc 为准。

### 2.1 P0（功能坏 / 钥匙丢失 / 安全门槛失效）

**P0-1 base_url 仍从凭据管理器读（D3 未落地）**
- 位置：`services/provider/live.rs:808-815`（Codex `write_live_snapshot`）、`commands/provider.rs:57-66`（列表 `load_secret_status`）。
- 后果：迁移会把 `cc-switch/v1/provider/*/*/base_url` 写进 1P 并从凭据管理器删除（`migration_1p.rs` 的 `inventory` 收了 `base_url`）。之后这两处读到 `None`：
  - Codex 第三方供应商写出的 `config.toml` 当前表**没有 `base_url`**；1P 模式**每次启动**的 `strip_current_live_plaintext` 都会走这条路径重写一次，切换也一样。Codex 会退回默认端点，带着第三方钥匙请求 OpenAI（钥匙发错主机）。
  - 列表卡片端点全空；且运行时仍在读 Windows 凭据管理器（违反原方案目标 1）。
- 根因：原方案 D3 默认 A（base_url 非秘密、存 DB）从未实现；base_url 仍被提取器当秘密剥离进 vault，而读取点没有全部改走 vault。Pi 的 `hydrate_pi_base_url_for_live` 在 577cde1 改成了 fetch（等于 D3-B），三个 app 行为不一致。

**P0-2 `OnePasswordVault::put` 非原子（先归档删除，再新建）**
- 位置：`secrets/onepassword.rs:530-578`。
- 后果：
  - 删除成功、新建失败（断网 / 锁定 / 第二次授权被取消 / 120 秒超时）→ 条目进了归档，CCS 视为「没有钥匙」。AppSync 条目一次装 4 把（WebDAV 密码、S3 两把、E2E 口令），任一字段写失败就**连带丢 4 把**；E2E 口令丢了意味着远端加密快照无法解密。
  - 每次覆盖写都在 1P 归档里**多留一份旧钥匙副本**；item id 每次都变。
  - 超时被杀但服务端已提交时会出现同标题重复条目 → 之后按标题读一律 `ItemConflict`，该供应商永久不可用（见 P1-2）。
- 根因：注释称「`op item edit` 靠赋值语句传值，会把密钥放命令行」。本机实测 op 2.39.0 的 `op item edit --help` 原文：「You can also edit an item using piped input: `cat updatedLogin.json | op item edit oldLogin`」，且「can't combine piped input and the `--template` flag」。前提不成立。

**P0-3 编辑供应商时 DB 写失败会删光该供应商全部钥匙**
- 位置：`services/provider/mod.rs:478-483`（`update` 路径）。
- 后果：`save_provider` 失败 → `delete_provider_secrets` 把整组条目归档，还删掉 `secret_refs`。这对「新增」是对的回滚（`:352-356`），对「编辑」就是删掉用户已有的钥匙。
- 根因：新增路径的回滚逻辑被原样复制到编辑路径。原方案 §6.6 明确写了编辑无法原子回滚、DB 失败只记孤儿。

**P0-4 Pi `models.json` 的明文 key 在 1P 模式被静默丢弃**
- 位置：`services/provider/pi.rs:273-303`（`sync_native_locked`）、`:355-367`（`persist_pi_sync_secrets`）。
- 后果：用户直接在 `models.json` 填了明文 `apiKey` / 敏感 header。`sync_native_locked`（每次列表 Pi、每次启动都会跑）把 `models.json` 改写成 `$CC_SWITCH_PI_…` 引用、把剥离后的配置存进 DB，但 1P 模式下 `persist_pi_sync_secrets` 直接 `return Ok(())`，**明文 key 不写进任何地方**。
- 根因：3caeed3 为了「启动不因重抽 baseUrl 触发 op」一刀切跳过写入；真正的问题是 base_url 每次都被重抽（P0-1 修好后 base_url 不再触发 vault 写入），不该连 api_key 一起丢。

**P0-5 1P 模式下仍有路径把明文写回凭据管理器**
- 位置：
  - `store.rs:96-119` `scrub_imported_plaintext` → `secrets/migration.rs` `CredentialMigrator::run_migration`：`:67` 对凭据管理器 `probe`（写/读/删探针），`:130-139` 把提取出的明文写进凭据管理器，`:141-157` 写「假」`secret_refs`（`vault_id=""`，声称有钥匙）。调用方：SQL 导入、备份恢复、WebDAV/S3 下载。
  - `commands/import_export.rs:88` 便携包导入写凭据管理器（原方案 D9 要求 1P 模式写入 1P）；`:48` 导出命令后端未拒绝（前端仅隐藏按钮）。
- 后果：违反目标 1（运行时不再读写凭据管理器）；导入的钥匙 1P 里没有，UI 却显示「已配置」，开终端报「请先补全密钥」。
- 根因：这几条路径直接用 `AppState.secrets`（凭据管理器），没有经过 vault 抽象，也没有任何守卫。

**P0-6 迁移后到重启之间，运行中的仍是凭据管理器后端**
- 位置：`components/settings/OnePasswordSection.tsx:142-147`（重启可选「否」）、`secrets/vault.rs` `LegacyWindowsVault`。
- 后果：迁移已删除凭据管理器条目，但运行中的 `AppState.vault` 仍是 `LegacyWindowsVault`。用户点「否」后继续用：开终端/切 Codex 读不到钥匙；**新增/编辑供应商会把钥匙写回凭据管理器**，重启后 1P 里没有 → 钥匙丢失且明文回流凭据管理器。
- 根因：运行时 vault 只在启动时按设置构造，迁移提交后没有让旧后端失效。

**P0-7 op.exe 签名校验可被绕过，且校验失败仍会执行 op.exe**
- 位置：`secrets/onepassword.rs:689-748`（主体检查）、`:340-364`（`probe`）、`:763-778`（`op_version`）、`commands/onepassword.rs:82-110`（`onepassword_save_config`）。
- 后果：
  - 主体检查遍历签名里**所有**证书、做「含 agilebits 或 1password」子串匹配。签名方可以在证书包里夹带任意证书：攻击者用自己合法的代码签名证书签一个假 op.exe，再夹带一张 CN 含 “1Password” 的证书，即可通过。
  - `probe()` 算出 `signature_ok=false` 后仍照样执行 `op --version` 与 `op account list`（打开设置页就会跑）；`onepassword_save_config` 固定 `op_path` 时不校验签名。
- 根因：没有定位「签名者证书」，而且「校验失败拒用」只在 `from_settings()` 里执行。
- 实测：本机 op 2.39.0 签名者 `CN=Agilebits, O=Agilebits, L=Toronto, S=Ontario, C=CA`，颁发者 `CN=Microsoft ID Verified CS EOC CA 03`（Azure Trusted Signing，短期证书轮换，**不能钉证书指纹**）。

**P0-8 1P 模式每次启动整文件重写 live，吞掉用户在 CCS 外的修改**
- 位置：`services/provider/mod.rs:116-147`（`strip_current_live_plaintext`）、`lib.rs:634-642`、`codex_config.rs:563-592`。
- 后果：
  - 每次启动都用 DB 内容整份重写 `~/.claude/settings.json` 与 Codex `config.toml`。正常切换前会先「回填」（`sync_common_config_snippet_from_live` + 剥离回写），启动路径没有 → 用户或 Claude Code / Codex 自己写进 live 的改动（插件、hooks、model、profiles、sandbox 等）**每次启动被覆盖**。
  - `auth.json` 的 `OPENAI_API_KEY` 不管 1P 里有没有这把钥匙都直接删（例如用户刚 `codex login --api-key` 换了新钥匙，就被删掉了）。
  - 叠加 P0-1，每次启动都把 Codex 的 `base_url` 写没。
- 根因：把一次性的迁移后重写（旧逻辑受 `live_reapply_pending` 控制）改成了「每次启动无条件重写」。

### 2.2 P1（违反原方案原则 / 性能与体验严重问题）

**P1-1 迁移流程**（`secrets/migration_1p.rs`）
- `:138-148` 用 `secret_refs 有行 && vault.fetch 有值` 判定「已迁移」。v20 回填后每个供应商都有 `secret_refs` 行 → 首次迁移每组先白白 fetch 一次。每组合计 fetch + put（删+建 2 次）+ 回读 fetch = 4 次 op，N 个供应商约 4N×7 秒（10 个约 5 分钟），前端只有一个转圈，没有进度。
- `:170` 注释说「vault_item_conflict 时 put 内部按标题复用覆盖」，`put` 里并没有这段逻辑；出现同名条目后迁移永久失败。
- `:201-207` 在迁移开始时 `get_settings()` 取快照、几分钟后 `update_settings(整份)` 写回，迁移期间的其它设置改动会被覆盖（应使用已有的 `mutate_settings`）。
- base_url 被迁进 1P（与 D3-A 相反，见 P0-1）。

**P1-2 读取只按标题，没用 `secret_refs.item_id`**（`onepassword.rs:521-528`）
- 原方案 §4.3 要求 item_id 直达、标题只作兜底。按标题读，一旦出现同名条目（P0-2 的超时场景、用户手工复制条目）→ `ItemConflict`，该供应商所有取钥匙操作永久失败，且无修复入口。

**P1-3 同步链路**
- 在 async 命令里直接调用阻塞的 `fetch_sync_credentials`（`commands/webdav_sync.rs:100,113,135,220`、`commands/s3_sync.rs:102,115,137,214`、`commands/sync_e2e.rs`），最长阻塞 tokio 工作线程 120 秒。
- `sync_e2e_get_status`（`commands/sync_e2e.rs:30-37`）只为判断「口令是否已设」就调 op → 展开设置页「云同步」区就会弹解锁、等 6~9 秒。
- AppSync 写入后不更新 `secret_refs`（只有迁移时写过一次），所以没法做 0 次 op 的状态查询。
- `store_s3_credentials` 逐字段 `fetch + put`，保存一次 S3 设置最多 6 次 op。
- 自动同步在 1P 模式被整体跳过，UI 没有任何说明（原方案 D4 要求说明）。

**P1-4 可能触发 op 的同步 Tauri 命令仍跑在 IPC 线程**：`delete_provider`（`commands/provider.rs:199-209`，会 `vault.delete`）、`import_default_config`（`:293-297`，可能 `put`）、`env_delivery_adopt`（`commands/env.rs:58-67`，会 fetch）、`run_live_reapply_now`（`commands/misc.rs:148-153`）、`secrets_cleanup_orphans`（`:163-166`）。界面会冻结 6~9 秒到 120 秒。

**P1-5 1P 模式仍存在写 `HKCU\Environment` 钥匙的路径**：`adopt_env_vars`（`services/provider/mod.rs:1246-1276`，`:1269` `sink.set`）不检查严格模式，命令可直接调用（违反原方案陷阱 §12.8）。

**P1-6 `run_live_reapply_now` 在 1P 模式走旧流程**：`reapply_live_after_migration`（`mod.rs:46-111`）→ `adopt_unregistered_managed_env` 对三个 app 的当前供应商各 fetch 一次（3 次 op，且在 IPC 线程），`:94-101` 还读凭据管理器做 `prune`。

**P1-7 `sync_current_providers_live` 另起一个 `AppState`**（`commands/import_export.rs:197-212`）：`AppState::new` 会把 vault 重置成 `LegacyWindowsVault`，还新建了 `SwitchLockManager` / `env_sink` / `sync_kek`，绕过运行时后端与切换互斥锁。

**P1-8 `ccs env` 退出码**（`cli/mod.rs:278-295`、`:477-488`）：vault 的锁定/断网/超时错误都被包成 `ccs_missing_key` → 退出码 3，与「缺钥匙」无法区分；原方案 §6.5 要求的 6（锁定/取消）、7（网络/超时）未实现。

**P1-9 启动路径仍可能取钥匙**：某 app 没有任何供应商时，启动会 `import_default_config`（`lib.rs:726-759` → `live.rs:1164`），若 live 文件里有明文钥匙就 `vault.put` → 启动即弹解锁（违反原方案 §6.7 / 陷阱 §12.7）。

**P1-10 删除与孤儿**：`delete_provider_secrets`（`mod.rs:1891-1902`）在 1P 删除失败（锁定）时仍删除 `secret_refs`，1P 里的条目变成 CCS 看不见的孤儿；`cleanup_orphan_secrets`（`mod.rs:149-208`）在 1P 模式仍去清凭据管理器与 `known_secret_targets`。

### 2.3 P2（健壮性 / 内存卫生）

- **P2-1 stderr 分类会被回显的用户字符串误导**（`onepassword.rs:472-518`）：op 的错误信息会回显条目标题与 vault 名；`has("locked")` 是子串匹配，Pi 供应商 id 为 `unlocked-proxy`、`blocked` 之类时，「条目不存在」被判成 `Locked` → 该供应商永远写不进去（`put` 的删除步骤先报错）。
- **P2-2 Zeroizing 不完整**：`build_template_json`（`:446-468`）把所有值复制进普通 `String` 并序列化成普通 `Vec<u8>`；`parse_item_bundle`（`:403-417`）把空字符串值当作「有值」→ 可能注入空钥匙。
- **P2-3 1P 模式可随意改 account/vault**：运行中的 vault 不跟随新设置（需重启）；换 vault 后已迁移条目全部孤立。
- **P2-4 状态探测抢全局锁**：`op account list` / `--version` 也拿 `OP_LOCK`，一次等解锁的调用会把设置页状态卡住最长 120 秒；`get_diagnostics_bundle` 在 async 中同步执行探测。
- **P2-5 后端判定 fail-open**：`settings::get_secret_backend()` 读取失败回落 `"windows"`，`strict_for`、KEK 缓存、自动同步都会随之放宽。
- **P2-6 `secret_refs` 随云同步整表覆盖**：`database/backup.rs:85-89` 的 `SYNC_SKIP_TABLES` / `SYNC_PRESERVE_TABLES` 为空，`secret_refs` 会被远端设备的引用整表覆盖，而其语义依赖本机后端与 vault。
- **P2-7 注册表清理不彻底**：`purge_all_env_delivery`（`mod.rs:1039-1058`）删除失败也清空登记 → 值还在、登记没了（D-4 孤儿态）；迁移收尾没有扫 `CC_SWITCH_*` 残留。
- **P2-8** `locate_op` 用相对名 `where.exe`（`onepassword.rs:68`），应使用 `%SystemRoot%\System32\where.exe`（低）。

### 2.4 P3（文档 / UI）

- **P3-1 CHANGELOG 损坏**：`## [2.2.9] - 2026-09-25` 标题被直接改成 `## [2.3.0]`，2.2.9 的「加密便携包」等条目并入了 2.3.0，2.2.9 的发布记录消失。
- **P3-2** 用户手册与 `SECURITY.md` 没有 1Password 章节；原方案 §2.2「诚实边界」与 P5 要求的手册更新未落地。
- **P3-3** 前端没有 `vault_*` 错误码处理与「正在向 1Password 请求…」加载态（显示明文失败只显示通用 `revealFailed`）；1P 模式下多处 UI 仍写「凭据管理器」（如 `zh.json:295,764,1787`）；自动同步停用无提示。

---

## 3. 总体施工原则

原方案 §3 的 10 条原则全部继续有效（一次操作最多一次 op 往返 / 不缓存 / 失败即失败 / 钥匙不进命令行 / op 绝对路径 / 不阻塞 UI / 启动不取钥匙 / 本地只存引用 / 靠 trait 注入测试 / 迁移先写后删）。本轮新增：

1. **先止血，后优化**。F1 只修会丢钥匙、写坏配置、破坏安全门槛的问题；性能与体验放到 F2 之后。每个 F1 项单独提交，便于回退。
2. **写操作宁可整体失败，不可半截成功**。任何「先删后写」都禁止——原方案陷阱 §12.9 同样适用于单个条目的覆盖写，不只是迁移。
3. **取不到、存不进时，绝不丢弃明文**。发现明文但无法安全写入 vault（锁定、断网、启动路径不允许调 op）时：保留原处明文，记录待办并在 UI 提示用户处理，而不是剥掉了事。
4. **1P 模式下凭据管理器只是迁移源，要有会失败的守卫**。运行时路径误用凭据管理器必须立刻报错（测试里直接失败），不能靠「代码审查时注意」。
5. **启动、列表、状态查询 = 0 次 op**。所有需要 op 的操作都是用户显式动作，并且在 `spawn_blocking` 里执行（async 命令里也不允许直接调阻塞的 op）。
6. **先写复现测试，再改代码**（仓库 AGENTS.md「目标驱动执行」）。F0 建立的测试替身（`OpRunner` 假实现、`CountingVault`、守卫版凭据存储）是本轮所有修复的验收手段。
7. **外科手术式修改**：只改本文件列出的点；保持现有代码风格；注释与提交信息用简体中文；自己改动产生的无用代码要删掉，原有死代码只提及不删。
8. **每阶段收尾检查**：`cargo test`、`cargo clippy`、`cargo fmt --check`、`pnpm typecheck` 全部通过后再提交；按仓库 AGENTS.md 流程先代码审查再提交。
9. **真机验收由用户在本机执行**（需要解锁 1Password）。施工者负责给出命令与预期结果，不得在测试中使用用户真实 vault 的数据。

---

## 4. 修复方案

### F0 测试护栏（先做，不改业务行为）

**F0-1 `OpRunner` 注入，让 `OnePasswordVault` 可单测**
- 做法：把 `exec_op` 抽象成 trait `OpRunner { fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Zeroizing<Vec<u8>>, RunErr> }`，生产实现就是现有 `exec_op`；`OnePasswordVault` 持有 `Arc<dyn OpRunner>`。测试用 `FakeOpRunner`：按脚本返回 stdout / stderr 分类结果，并记录每次调用的 args 与 stdin。
- 为什么：`put` 原子性、item_id 优先、冲突处理、「命令行不含任何值」这些关键性质目前只能真机验证，改动后没有回归保护。

**F0-2 守卫版凭据存储 `GuardedSecretStore`**
- 做法：包在 `AppState.secrets` 外面。`settings::is_onepassword_backend()` 为真时，所有方法返回 `Err`，并 `log::error!` 记录（只记方法名，不记 target 与值）；否则透传给内层存储。测试里构造 1P 模式的 `AppState` 时一律使用它。
- 为什么：落实原则 4；F1-5 修完之后，任何残留或新增的误用都会让测试直接失败。
- 注意：`LegacyWindowsVault` 持有的是**未包装**的原始存储（Windows 模式运行时需要）；迁移向导与卸载清理自行构造 `WindowsSecretStore` 或直接调用 Win32 函数，不经过 `AppState.secrets`。

**F0-3 补全次数断言（原方案 §9.3 中缺失的几条）**
- 切换 Codex（含「切走回填」）：fetch = 0、put = 0。
- 1P 模式启动路径（抽出可测函数：启动剥离 + 默认导入 + Pi 原生同步 + Pi 投递）：fetch = 0、put = 0。
- Pi 列表（`models.json` 含明文 key，1P 模式）：fetch = 0、put = 0。
- `sync_e2e_get_status`：fetch = 0；保存 S3 设置：fetch = 1、put = 1。
- 这些断言先写出来，F1–F3 完成后变绿；不许删、不许放宽。

### F1 止血（P0）

建议提交顺序：F1-6 → F1-5 → F1-3 → F1-1 → F1-2 → F1-4 → F1-8 → F1-7（先堵「钥匙继续流失」的口子，再做改动面大的 F1-2）。

**F1-6 迁移提交后旧后端立即失效，并强制重启**（修 P0-6）
- 做法：
  1. `LegacyWindowsVault::{fetch, put, delete}` 开头检查 `settings::is_onepassword_backend()`，为真时返回新错误 `VaultError::RestartRequired`（code `vault_restart_required`，文案「已切换到 1Password，请重启 CC Switch」）。迁移是先切 `secret_backend` 再删凭据管理器条目，所以守卫在删除之前就已生效。
  2. 前端迁移成功后去掉「否」分支：弹窗只有「立即重启」；重启失败时显示常驻横幅。
- 为什么：这是最小改动——运行中不再有任何读写能落到凭据管理器。热切换运行时 vault（`RwLock` 替换）作为后续优化，本轮不做。
- 验收：单测——设置切到 onepassword 后，`LegacyWindowsVault` 的三个方法均返回 `vault_restart_required`。

**F1-5 堵住明文回流凭据管理器的路径**（修 P0-5）
- `scrub_imported_plaintext`（`store.rs:96`）在 1P 模式改走 vault：
  1. 逐行 `SecretExtractor::extract_with_meta`（纯函数，不碰任何存储）；
  2. 对有秘密的行调用 `store_provider_bundle(merge_existing = true)`（base_url 按 F1-2 走端点表）；
  3. 写入成功才在 DB 里剥离该行；写入失败的行**保留明文不剥**，记入本机设置 `secrets_import_pending`（只记 app/provider id），UI 提示「解锁 1Password 后重试导入钥匙」。DB 暂时带明文时，导出护栏 `assert_no_secret_patterns` 会拒绝同步上传，这是期望的 fail-closed 行为；
  4. 1P 模式下**不调用**凭据管理器的 `probe`。
  Windows 模式保持现状（`CredentialMigrator`）。
- 便携包：1P 模式的 `secrets_import_via_dialog` 改为「解包 → 按 `(app, provider)` / AppSync 分组 → 每组 fetch 合并后 put 一次 → 更新 `secret_refs` 与端点表」；`secrets_export_via_dialog` 在 1P 模式由后端直接拒绝（不能只靠前端隐藏）。
- 最后启用 F0-2 的守卫。自查：`grep -rn "state.secrets\|app_state.secrets\|\.secrets\.as_ref()" src-tauri/src`，每个结果要么只在 Windows 模式分支里，要么是迁移源 / 卸载。
- 为什么：原方案目标 1 要求运行时不再读写凭据管理器；目前这几条路径不仅违反目标，还会让 UI 显示「已配置」而 1P 里其实没有钥匙。
- 验收：1P 模式 + 守卫存储的单测中，导入带明文的 SQL 后钥匙进入 `InMemoryVault`，守卫存储零调用；vault 写入失败时，该行 DB 保持原样并出现 pending 标记。

**F1-3 编辑失败不再删除钥匙**（修 P0-3）
- 做法：`mod.rs:478-483` 的 `update` 路径在 `save_provider` 失败时**不调用** `delete_provider_secrets`，直接返回错误。vault 里已经是新值、DB 还是旧行，两者暂时不一致但没有丢失，用户重新保存即可恢复一致。新增路径（`:352-356`）保持归档回滚不变。
- 为什么：编辑场景下该条目本来就存在，删除等于把用户已有的全部钥匙删掉。
- 验收：注入 `save_provider` 失败的单测——旧 api_key 与 extra_env 仍可 fetch 到，`secret_refs` 行仍在。

**F1-1 `put` 改为原子的「读取 → 就地编辑」**（修 P0-2）
- 新流程（`onepassword.rs` `put`）：
  1. `op item get <ref> --vault V --account A --reveal --format json`（`<ref>` 按 F2-2 规则：item_id 优先，标题兜底）。
  2. 不存在 → `op item create --vault V --account A --format json -`，模板 JSON 走 stdin（保持现状）。
  3. 已存在 → 在返回的 JSON（`serde_json::Value`）上修改：非托管字段原样保留（op 默认字段、用户自己加的字段）；托管字段（`is_managed_field`）按目标整包更新值、删除目标里没有的、追加新增的（类型 `CONCEALED`）；确保 `cc-switch-schema` 标记字段存在。然后 `op item edit <item_id> --vault V --account A --format json`，**整份 JSON 走 stdin**。
  4. 用 edit 返回的 JSON 校验「托管字段集合 == 目标集合」。如果实测发现 stdin JSON 对缺失字段是「合并」而不是「替换」，对多出来的 label 再执行一次 `op item edit <item_id> '<label>[delete]'`——参数里只有字段名、没有值，符合原方案 §12.3。
  5. 返回 `VaultRef`，item_id 保持不变。
- 删除现有「delete + create」代码及其错误注释。
- **施工前必须在本机实测**：stdin JSON 编辑是替换还是合并字段集合、`[delete]` 能否与管道输入同时使用、编辑后 item id 是否不变、归档里有没有新增条目。结论写进 `onepassword.rs` 顶部注释，并做成 `FakeOpRunner` 的测试样本。
- 内存卫生：序列化结果用 `Zeroizing<Vec<u8>>` 承载；修改用的 `serde_json::Value` 用完立即 drop。
- AppSync：`sync_secrets.rs` 的 `put_app_field` 改为 `update_app_sync(vault, |bundle| { … })`：一次 fetch → 在闭包里改任意多个字段 → 一次 put；`store_s3_credentials` 两个字段一次写完。
- 为什么：单次 edit 要么整体生效要么不生效；item id 稳定（F2-2 依赖它）；不在归档里堆积旧钥匙副本。写操作仍是 2 次 op，与现状相同。
- **真机修正（2026-09-27 验收发现）**：~~「1Password 自带条目历史，误写可在 1P 里回滚」~~ **不成立**——`op item edit` 的就地编辑在 1Password 客户端里不产生版本历史（实测 item 只有当前一版）。覆盖写旧 api_key 的值是不可恢复的。安全网只剩：get→edit 保证非托管字段不丢；因此「编辑供应商时表单不回传旧钥匙」的前端契约不能破坏。归档旧副本清理（delete+create 时代残留 46 条）与 1P 孤儿条目清理（如 cc-switch/codex/default）记入 F2 范围。
- 验收（`FakeOpRunner`）：已存在时调用序列为 `[get, edit]`，没有 delete / create；不存在时为 `[get(NotFound), create]`；edit 失败时返回 Err 且没有发出任何 delete；**所有调用的 args 里不出现 bundle 中任何值**（遍历断言）。

**F1-2 D3-A 落地：base_url 作为非秘密存本地端点表**（修 P0-1；决策见 §7 D3）
- 数据：新表 `provider_endpoints(app TEXT NOT NULL, provider_id TEXT NOT NULL, base_url TEXT NOT NULL, updated_at INTEGER NOT NULL, PRIMARY KEY(app, provider_id))`，`SCHEMA_VERSION` 20 → 21（同时要在 `create_tables` 与 `database/tests.rs` 的表名清单里补上）。该表随云同步（非秘密，换设备也需要）；删除供应商时一并删除对应行。
- 敏感 URL 判定 `is_credential_bearing_url(url)`：URL 带 userinfo（`scheme://user[:pass]@host`），或 query 参数名命中 `is_sensitive_config_key` → 视为秘密，**仍存 vault**（保持现状）。其余一律进端点表。
- 统一读取函数 `resolve_base_url(state, app, id) -> Result<Option<Zeroizing<String>>, AppError>`：
  1. 端点表命中 → 直接返回（0 次 op）；
  2. 否则若 `secret_refs` 字段里有 `base_url` → `fetch` 一次取出；若是非敏感 URL，顺手写入端点表（懒迁移，之后都是 0 次 op）；
  3. 都没有 → `None`。
- 写入：`store_provider_bundle` 把 `secrets.base_url` 拆出来——非敏感的写端点表，并从准备写入 vault 的整包里去掉 `base_url`（merge 时 fetch 回来的旧整包里的 `base_url` 也要删掉）；敏感的留在整包里。拆分后如果 vault 整包没有变化（例如只改了端点）→ **不 fetch 也不 put**。
- 读取点全部改造：
  - `live.rs:808` Codex 写 live → `resolve_base_url`。若解析结果为 `None` 而 `secret_refs` 表明应有 base_url，则**切换失败并提示**，绝不写出缺 `base_url` 的 `config.toml`；
  - `pi.rs:208` `hydrate_pi_base_url_for_live` → `resolve_base_url`；
  - `commands/provider.rs:57` `load_secret_status` → **只读端点表**，不触发懒迁移（列表必须 0 次 op）。端点表没有、但 `secret_refs` 有 `base_url` 时返回空值，前端显示「在 1Password 中（待回填）」；
  - `reveal_provider_secret_internal(field = "base_url")` → 端点表优先；
  - `fetch_provider_secrets` 返回前用端点表覆盖 `base_url`（Claude 终端注入 `ANTHROPIC_BASE_URL` 的行为不变）；
  - `validate_provider_settings` 的 base_url 存在性检查 → 端点表 **或** `secret_refs`。
- 存量数据：
  - 1P 模式已迁移的用户，base_url 目前在 1P 里。启动时（0 次 op）统计「`secret_refs` 有 `base_url` 但端点表没有」的数量；大于 0 时在设置页 1Password 区和主界面横幅提示「回填端点（N 个，约 N×7 秒，会请求解锁）」，用户点击后在 `spawn_blocking` 里逐个 fetch 并上报进度。回填完成前，Codex 切换 / Pi 启用走懒迁移分支（每个供应商首次 1 次 op，之后 0 次）。
  - Windows 模式同样走端点表（只保留一套代码路径）；懒迁移读的是凭据管理器，本地很快。
  - `migration_1p` 迁移时，非敏感 base_url 直接写端点表，不进 1P（见 F2-1）。
- 为什么：Codex / Pi 本来就把 URL 明文写进各自的配置文件，放进 1P 只会让每次切换多 7 秒并弹解锁，却挡不住文件泄露；列表可以正常显示端点；彻底消除「从已清空的凭据管理器读 base_url」这类错读。Codex 切走时的回填会从 `config.toml` 提取出 base_url，改造后写端点表，切换真正做到 0 次 op。
- 验收：
  - 1P 模式（`InMemoryVault` + 守卫存储）下 Codex `write_live` 输出的 `config.toml` 当前表含 `base_url`（来自端点表）；
  - Codex 切换（含回填）fetch = 0、put = 0；
  - 端点表为空且 `secret_refs` 有 `base_url` 时，切换只 fetch 1 次，之后再切换为 0 次；
  - 带 `user:pass@` 的 URL 仍存 vault，不进端点表。

**F1-4 Pi 原生同步不再丢明文**（修 P0-4）
- 做法（`pi.rs` `sync_native_locked`）：
  1. 提取结果中的 base_url 按 F1-2 写端点表（两种模式都是 0 次 op）。这一步做完，就不会再出现「每次启动因重抽 baseUrl 而触发 op」。
  2. 若提取到 api_key 或敏感 header 的**明文**：
     - Windows 模式：保持现状，写入凭据管理器（本地、快）；
     - 1P 模式：**不改写 `models.json`、不保存剥离后的配置**（DB 行保持原来已剥离的状态），把 provider id 记入本机设置 `pi_plaintext_pending`，UI 提示「检测到 Pi 配置里有明文钥匙，[导入到 1Password]」。
  3. 新增 async 命令（`spawn_blocking`）处理「导入到 1Password」：`store_provider_bundle(merge = true)` → 成功后再用 `sanitize_pi_provider_for_live_write` 改写 `models.json` → 保存剥离后的 DB 行 → 清除 pending 标记。
  4. 删除 `persist_pi_sync_secrets` 里 1P 模式 `return Ok(())` 的短路。
- 为什么：列表和启动必须 0 次 op（原则 5），但也不能用「丢掉」来换 0 次 op（原则 3）。明文本来就在用户自己的 `models.json` 里，推迟到用户确认后再处理不会让情况更糟。
- 验收：1P 模式下 `models.json` 含明文 `apiKey` → 同步后 `models.json` 字节不变、DB 行不变、pending 有记录、op 调用 0 次；执行导入命令后 vault 里有这把 key、`models.json` 变成引用、pending 清空。

**F1-8 启动剥离改为「就地、定点、有备份才剥」**（修 P0-8）
- 做法：重写 `strip_current_live_plaintext`，**删除**其中对 `write_live_with_common_config_for_state` 的调用（启动时不再整文件重写 live）。改为：
  - Claude `settings.json`：只删除 `env` 与顶层的敏感键（复用 `is_claude_env_secret`），其余内容原样保留；
  - Codex `auth.json`：只删除 `OPENAI_API_KEY`（存在 `tokens` 时不动，保持现状）；
  - Codex `config.toml`：用 `toml_edit` 只删除各表的 `experimental_bearer_token`；
  - **剥离前提**：当前供应商的 `secret_refs` 含 `api_key`（说明 1P 里有一份）。不满足时不剥，记入 `live_plaintext_pending`，UI 提示「检测到 live 文件含明文钥匙，[导入到 1Password 并剥离]」（用户动作，允许调用 op）；
  - 没检测到明文就不写文件（幂等；无明文时零写入）。
- 同时调整 `codex_config::strip_codex_apikey_plaintext_for_onepassword`：增加「refs 有 api_key」这个前提参数。
- 为什么：正常切换在重写 live 之前会先回填，启动路径没有回填，整文件重写就会吞掉用户在 CCS 外对 live 的改动；无条件删 `auth.json` 里的钥匙，可能删掉 1P 里没有的新钥匙。
- 验收：无明文的 live 文件启动后字节完全不变；有明文且 refs 有 api_key → 只少了敏感键，其它键和注释保留；有明文但 refs 没有 api_key → 文件不变、pending 有记录；整个启动路径 op 调用 0 次。

**F1-7 签名校验只认签名者证书，校验失败绝不执行**（修 P0-7）
- 做法：
  1. 在 `WinVerifyTrust`（`WTD_STATEACTION_VERIFY`）成功之后、`WTD_STATEACTION_CLOSE` 之前，用 `WTHelperProvDataFromStateData(wtd.hWVTStateData)` → `WTHelperGetProvSignerFromChain(prov, 0, FALSE, 0)` → `pasCertChain[0].pCert` 取得**签名者证书**（或用 `CryptMsgGetParam(CMSG_SIGNER_CERT_INFO_PARAM)` 找到签名者证书）。不再遍历证书包里的所有证书。
  2. 用 `CertGetNameStringW(CERT_NAME_ATTR_TYPE, szOID_ORGANIZATION_NAME)` 取签名者 O 字段，要求**完整等于** `Agilebits`（忽略大小写）；去掉 “1password” 子串匹配。「从名称字符串判断是否可信」抽成纯函数，便于单测。
  3. `probe()`：签名失败时不执行 `op --version` / `op account list`，返回 `installed = true, signature_ok = false`，其余字段为空。
  4. `onepassword_save_config` 固定 `op_path` 之前先校验签名，失败则不写入；`op_version` 改走 `exec_op`（带环境变量清理与超时）。
  5. 保持 `WTD_REVOKE_NONE`（不联网查吊销），在注释里写明这一取舍。
- 为什么：D8 把 op.exe 视为整个方案的信任根。当前实现既可以被夹带证书绕过，又会在校验失败后照样执行被拒的二进制。签名来自 Azure Trusted Signing，证书短期轮换，所以只能校验主体，不能钉指纹。
- 验收：纯函数单测（`O=Agilebits` 通过；`O=Evil, CN=1Password Fake` 不通过；空主体不通过）；已有的 ignored 真机测试 `real_op_signature_verifies` 仍然通过；把 op.exe 复制出来改一个字节后，校验失败且没有任何 op 子进程被拉起。

### F2 迁移与读路径（P1-1、P1-2、P2-3）

**F2-1 迁移流程**
- 「已迁移」改用本地标记判定：`secret_refs.vault_id` 等于目标 vault，且 `item_id` 是真实的 1P id（非空、不是 `provider/<app>/<id>` 这种旧占位形式）。首次迁移不再额外 fetch。
- 每组固定为：`put`（≤ 2 次 op）+ 回读校验 `fetch`（1 次）。非敏感 base_url 写端点表，不进 1P（与 F1-2 一致）。
- 进度：每完成一组 `emit("onepassword-migrate-progress", {done, total})`，前端显示「第 k/N 个，约 7 秒/个」。
- `ItemConflict`：列出同名条目（id、更新时间）交给用户处理，**不自动归档**；迁移停在当前组，不删除任何凭据管理器数据。
- 切后端改用 `mutate_settings`，避免覆盖迁移期间的其它设置改动。
- 1P 模式下后端拒绝再次执行迁移；另提供「清理凭据管理器残留」动作，用 `windows_enumerate_targets` + `windows_delete_credential` 删除剩余的 `cc-switch/*` 条目（迁移后 1P 是唯一真源；动作前让用户确认）。
- 验收：v20 回填过 refs 的库首次迁移 fetch 次数 = N（仅回读校验）；中断后重跑跳过已完成组；制造同名冲突时迁移停止，凭据管理器数据完好。

**F2-2 按 item_id 读写，标题只作兜底**
- 做法：`OnePasswordVault` 构造时注入 `Arc<Database>`（与 `LegacyWindowsVault` 一致）。`fetch` / `put` / `delete` 先查 `secret_refs`：`vault_id` 与当前配置一致、`item_id` 是真实 1P id → 用 id；返回 NotFound 时再按标题兜底一次，并把找到的 id 写回 `secret_refs`。
- 按标题命中多个（`ItemConflict`）→ `op item list --vault V --account A --tags cc-switch --format json`（**不带** `--reveal`）筛出同标题条目，取 `updated_at` 最新的一条读取，记告警，并在 UI 提示清理重复条目。
- 所有 `put` / `delete` 调用点（含 AppSync）统一走一个辅助函数，在同一处完成「写 vault + 维护 `secret_refs`」：AppSync 写入后同样 upsert `_app/_sync` 行的字段名清单。
- 为什么：item id 唯一，不会被同名条目误导（原方案 §4.3）；F3-1 的「0 次 op 状态查询」需要 AppSync 的引用始终是准的。
- 验收（`FakeOpRunner`）：有 ref 时 args 里是 item id 而不是标题；id NotFound → 标题兜底 1 次并回写 ref；标题冲突 → 选最新的一条并返回告警。

**F2-3 1P 模式锁定 account / vault**
- 做法：`backend == "onepassword"` 时前端把账户、vault 下拉框置灰并说明原因；`onepassword_save_config` 后端拒绝修改 account / vault（签名校验开关仍可改，关闭时给出警告）。
- 为什么：运行中的 vault 不会跟随设置变化；换 vault 会让所有已迁移条目变成孤儿。「整体搬到另一个 vault」不在本轮范围内。

### F3 线程与命令边界（P1-3 ～ P1-10）

**F3-1 同步链路**
- 所有 `fetch_sync_credentials` / `store_*` / `update_app_sync` 的调用都包进 `spawn_blocking`（`commands/webdav_sync.rs`、`commands/s3_sync.rs`、`commands/sync_e2e.rs`）。
- `sync_e2e_get_status`、`sync_e2e_set_enabled` 判断「口令是否已设」改查 `secret_refs` 的 `_app/_sync` 字段清单（0 次 op）。
- 1P 模式下，同步设置区的自动同步开关旁显示「1Password 模式下自动同步已停用，请手动同步」。
- 验收：F0-3 中的同步相关次数断言全部通过。

**F3-2 可能触发 op 的命令一律 async + `spawn_blocking`**
- 至少包括：`delete_provider`、`import_default_config`、`env_delivery_adopt`、`run_live_reapply_now`、`secrets_cleanup_orphans`，以及 F1-4 / F1-8 / F1-2 新增的「导入到 1Password」「回填端点」命令。
- 自查方法：遍历 `lib.rs` `invoke_handler` 注册的命令，凡调用链可达 `state.vault` 的必须是 async 且在 `spawn_blocking` 中执行。把审计结果以表格形式写进 PR 描述。

**F3-3 `adopt_env_vars` 在严格模式下拒绝**
- 做法：`strict_for(app)` 为真时直接返回本地化错误，不 fetch、不写注册表。
- 为什么：落实原方案陷阱 §12.8——1P 模式下不存在任何往 `HKCU\Environment` 写钥匙的路径。

**F3-4 `run_live_reapply_now` / `get_live_reapply_status` 在 1P 模式改走 F1-8**
- 1P 模式下不调用 `reapply_live_after_migration`、不执行 `adopt_unregistered_managed_env`、不读凭据管理器 `prune`；改为执行 F1-8 的就地剥离并返回 pending 清单。

**F3-5 `sync_current_providers_live` 复用运行时状态**
- `commands/import_export.rs:197-212` 改为 `state.inner().clone()`，删除 `AppState::new` 的调用。

**F3-6 `ccs env` / `ccs-open` 错误分级**
- 做法：`env_command_core` 不再把 fetch 错误包成 `ccs_missing_key`，而是保留 vault 错误分类：`vault_locked` → 退出码 6（新常量 `EXIT_VAULT_LOCKED`）；`vault_network` / `vault_timeout` → 7（`EXIT_VAULT_UNREACHABLE`）；`vault_not_installed` / `vault_not_signed_in` / `vault_other` / `vault_restart_required` → 5；缺钥匙仍为 3。同步更新 `classify_exit`、`cli/mod.rs` 顶部注释与用户手册里的退出码表。
- `ccs-open` 弹窗按分类显示可操作的文案（如「请打开并解锁 1Password 后重试」）。
- 为什么：脚本需要区分「锁了，稍后重试」和「根本没有配置钥匙」。

**F3-7 启动时不因默认导入而取钥匙**
- 1P 模式下，启动时的 `import_default_config` 若提取到秘密，就跳过这次自动导入并记一条提示；用户通过已有的「导入当前配置」按钮完成导入（该命令已按 F3-2 改为 async）。

**F3-8 删除供应商与孤儿清理**
- `vault.delete` 失败时，供应商照删，把组 key 记入本机设置 `onepassword_orphans`；此时不要把该条目当作「已清理」。
- `secrets_cleanup_orphans` 在 1P 模式：处理 `onepassword_orphans` 列表，再用一次 `op item list --tags cc-switch`（不带 `--reveal`）与 DB 对比，列出孤儿条目给用户确认后归档。1P 模式下该命令不再碰凭据管理器（凭据管理器残留由 F2-1 的单独按钮处理）。

### F4 健壮性（P2）

- **F4-1 stderr 分类**：匹配关键字之前，先把 stderr 中成对引号（`"…"`、`'…'`）里的内容替换成占位符（op 在这些位置回显条目名与 vault 名）；把 `has("locked")` 收窄为明确短语（如 `account is not unlocked`、`is locked`）；保持「not-found 必须同时满足退出码非零与关键字」。表驱动样本里加入「id 为 `unlocked-proxy` 的条目不存在」。
- **F4-2 内存卫生**：模板序列化结果用 `Zeroizing<Vec<u8>>`；`OpFieldTemplate` 用 `&str` 借用，避免多余的 `String` 副本；`parse_item_bundle` 把空字符串值视为「无此字段」。
- **F4-3 状态探测不抢全局锁**：`op account list` / `op --version` 不会弹授权，不获取 `OP_LOCK`；`get_diagnostics_bundle` 的探测放进 `spawn_blocking`。
- **F4-4 后端判定 fail-closed**：新增 `settings::backend_is_onepassword_or_unknown()`，读取失败按 1P 处理；`strict_for`、KEK 缓存、自动同步的判断改用它。展示类场景仍可用原函数。
- **F4-5 `secret_refs` 本机保留**：加入 `SYNC_SKIP_TABLES` 与 `SYNC_PRESERVE_TABLES`（导出不带，导入保留本机行）。新增维护动作「从 1Password 重建引用」：`op item list --vault V --tags cc-switch --format json` + 对每个条目执行不带 `--reveal` 的 `op item get`，只取字段 label，重建 `secret_refs`（用户动作，N+1 次 op，带进度）。手册写明：新设备同步后先执行一次重建。
- **F4-6 注册表清理**：`purge_all_env_delivery` 删除失败时保留该变量的登记（与 `ccs env --clear` 的「删成功才摘登记」一致）；迁移收尾再扫描 `HKCU\Environment` 中 `CC_SWITCH_` 前缀的变量并删除；其它敏感名（如 `ANTHROPIC_AUTH_TOKEN`）只在迁移报告里列出，不自动删（可能是用户自己设置的）。
- **F4-7** `where.exe` 改用 `%SystemRoot%\System32\where.exe` 绝对路径。

### F5 文档与 UI（P3）

- **F5-1 CHANGELOG**：恢复 `## [2.2.9] - 2026-09-25` 原标题与原条目；2.3.0 单独成节，只写 1Password 相关内容；本轮修复记为 2.3.1（或并入 2.3.0，见 §7 D15）。
- **F5-2 用户手册 + `SECURITY.md`**：新增 1Password 章节，内容包括：原方案 §2.2 的诚实边界（防得住 / 防不住）；base_url 的定位（D3-A，非秘密、会随同步）；`ccs env` 退出码表（含 6、7）；自动同步停用；换设备需先「重建引用」；迁移单向（D10）以及 1Password 不可用时的影响；卸载不会动 1Password 条目（D5）。
- **F5-3 前端错误码**：后端对 key 以 `vault_` 开头的 `Localized` 错误，统一输出 JSON 字符串 `{"code":"vault_locked","message":"…"}`（沿用现有 `ENV_CONFLICT` 的 JSON-in-string 约定，写一个辅助函数集中处理）；前端 `extractErrorMessage` 识别 `vault_*` 后映射到 i18n `vault.errors.<code>`，并提供「重试」。覆盖：打开终端、运行 CLI、显示明文、保存 / 删除供应商、同步、迁移、回填端点、导入明文。
- **F5-4 加载态**：所有可能触发 op 的按钮统一显示「正在向 1Password 请求，可能需要解锁…」。
- **F5-5 文案**：1P 模式下把仍写「凭据管理器」的文案按后端切换（`SecretStoreMaintenance`、E2E 口令警告 `zh.json:764`、便携包提示等）；新增的 pending 提示、端点回填、重建引用、需要重启等文案补齐四语（zh / zh-TW / en / ja）。

---

## 5. 测试方案

**单元测试（CI 可跑，不依赖真实 op）**
1. `FakeOpRunner`：`put` 调用序列（F1-1）；item_id 优先与标题兜底（F2-2）；冲突选最新（F2-2）；**任何调用的 args 不含 bundle 中的任何值**；stderr 分类表驱动（含回显陷阱样本，F4-1）。
2. `CountingVault` 次数断言（F0-3 全部，以及原方案 §9.3 已有的断言）。
3. 回归测试，每个 P0 至少一条：
   - P0-1：1P 模式下 Codex live 的 `config.toml` 含 `base_url`；列表 0 次 op 且能显示端点；
   - P0-2：见 F1-1 验收；
   - P0-3：编辑时 `save_provider` 失败，旧钥匙仍在；
   - P0-4：见 F1-4 验收；
   - P0-5：1P 模式 + `GuardedSecretStore`，导入 / 恢复 / 便携包导入都不触碰守卫存储；
   - P0-6：迁移后 `LegacyWindowsVault` 返回 `vault_restart_required`；
   - P0-7：签名者主体判定纯函数；
   - P0-8：启动剥离的三种情形（无明文零写入 / 定点剥离 / 无备份不剥）。
4. 迁移：已迁移判定不 fetch；中断重跑；`ItemConflict` 停止且不删源数据；非敏感 base_url 进端点表。
5. DB：v20 → v21 迁移建表；`provider_endpoints` 随供应商删除；`secret_refs` 同步导出跳过、导入保留。
6. `ccs env` 退出码映射（3 / 5 / 6 / 7）。

**真机测试（`#[ignore]`，用户在本机手动跑，使用专用测试 vault 与随机标题，`Drop` 守卫清理）**
- 扩展 `real_op_roundtrip`：create → get → edit（改值 + 删字段 + 加字段）→ get，断言字段集合精确一致、item id 不变、归档里没有新增条目。
- 锁定 1Password 后 fetch，断言 `vault_locked` 或 `vault_timeout`。

**手动验收清单（用户执行）**
1. 迁移后重启：Codex 第三方供应商的 `config.toml` 仍有 `base_url`；列表显示端点；切换 Claude / Codex 不弹解锁、秒回。
2. 在 `~/.claude/settings.json` 手工加一个 hook → 重启 CCS → hook 仍在。
3. 在 Pi `models.json` 手填明文 `apiKey` → 打开列表 → 文件不变、出现导入提示 → 导入后变成引用。
4. 断开网络 → 打开终端报「网络」错误；恢复后成功。锁定 1Password → 右键打开终端弹窗提示解锁，不会打开缺钥匙的终端；`ccs env claude` 退出码为 6。
5. 编辑 AppSync 相关设置时中途锁定 1Password → 旧的 WebDAV 密码与 E2E 口令都还在。
6. `cmdkey /list | findstr cc-switch` 无结果；`reg query HKCU\Environment` 中没有 CCS 投递的钥匙变量；1Password 归档中没有因为编辑而产生的重复条目。
7. 用 Process Explorer 观察 `op.exe` 的命令行参数，确认不含任何钥匙值。

---

## 6. 施工顺序与提交粒度

| 阶段 | 内容 | 验收 |
|---|---|---|
| F0 | `OpRunner` 注入、`GuardedSecretStore`、补全次数断言（此时部分断言预期失败，用 `#[ignore = "F1 后启用"]` 标记，F1 完成时逐条去掉） | 全量测试通过；新增测试替身有自测 |
| F1 | 按 F1-6 → F1-5 → F1-3 → F1-1 → F1-2 → F1-4 → F1-8 → F1-7 顺序，**每项一个提交** | 每项对应的回归测试变绿；F1 全部完成后**请用户做一轮真机验收**（§5 手动 1~3、5），通过后再继续 |
| F2 | 迁移、item_id 读写、account/vault 锁定 | 迁移相关测试；真机 `real_op_roundtrip` |
| F3 | 线程边界、`adopt` 拦截、reapply、`ccs env` 退出码、启动导入、孤儿 | 命令审计表；§5 手动 4、6、7 |
| F4 | 健壮性 | 单测 |
| F5 | 文档、i18n、前端错误码与加载态 | `pnpm typecheck`；四语键齐全 |

`SCHEMA_VERSION` 20 → 21 放在 F1-2。注意 CLI 的 `gate_db_version`：升级后必须先启动一次 GUI 完成迁移，`ccs env` / 右键菜单才能用，手册里要写明。

---

## 7. 待用户确认的决策（施工默认值已给出）

| # | 问题 | 默认（推荐） | 理由 |
|---|---|---|---|
| D3 | base_url 是否也放 1Password | **A：非敏感 URL 存本地端点表（随同步），带凭据的 URL 仍存 vault** | 延用原方案默认。若坚持 URL 也是秘密，改走附录 B（代价：Codex/Pi 每次切换 1 次 op，列表不显示端点） |
| D11 | 迁移后能否改 account / vault | **锁定** | 运行中 vault 不跟随；换 vault 会让条目全部孤立 |
| D12 | 删除供应商时 1Password 不可用 | **照删供应商，记孤儿，稍后清理** | 不阻塞正常操作，也不让 1P 条目变得不可见 |
| D13 | 启动时发现 live / `models.json` 明文，但 1P 里没有对应钥匙 | **保留明文 + 提示用户导入** | 启动不能调 op，也不能丢钥匙 |
| D14 | `secret_refs` 是否随云同步 | **不随同步（本机保留）+ 提供「重建引用」** | 引用语义依赖本机后端与 vault |
| D15 | 版本号 | 若 2.3.0 **未发布**：并入 2.3.0；**已发布**：2.3.1 | 以实际发布状态为准 |

---

## 8. 施工陷阱清单（与原方案 §12 合并使用）

1. **不要再写任何「先删后建」**——包括单条目覆盖写、AppSync、迁移。
2. **读 base_url 只能走端点表 / `resolve_base_url`**。自查：`grep -rn "provider_base_url(" src-tauri/src`，除迁移源、测试、`secrets/` 内部实现外应为 0。
3. **启动路径不得整文件重写 live**；也不得调用任何 `vault.fetch` / `vault.put`（由 F0-3 的启动次数断言守住）。
4. **取不到、存不进时不许丢明文**：Pi 同步、启动剥离、导入 scrub 三处都要有「保留 + pending + 提示」分支和对应测试。
5. **async 命令里也不能直接调阻塞的 op**，必须 `spawn_blocking`；新增命令同样适用。
6. **stderr 关键字匹配前先剔除回显内容**；not-found 判断拿不准时当错误处理，不能当「没有钥匙」。
7. **签名只认签名者证书**，校验失败时不得拉起 op.exe 的任何子进程。
8. **测试里的 1P 模式一律使用 `GuardedSecretStore`**，任何凭据管理器误用都应让测试立刻失败。
9. **旧占位引用**（`vault_id=""`、`item_id="provider/<app>/<id>"`，来自 v20 回填与 `CredentialMigrator`）不能当作 1P 的 item id 使用；判断「是否已迁移」时也要排除它们。
10. **`secret_refs` 只能通过统一辅助函数维护**（写 vault 与写引用在同一处完成），AppSync 也不例外。
11. **前后端都要处理「需要重启」状态**（`vault_restart_required`）。
12. **不要顺手重构**：`provider_env_pairs`、`fetch_provider_secrets`、严格投递强制、op 调用规范这些已正确的部分保持原样（见 §1）。

---

## 附录 A：本机只读实测记录（2026-09-26）

- op 路径：`C:\Users\jia\AppData\Local\Microsoft\WinGet\Packages\AgileBits.1Password.CLI_Microsoft.Winget.Source_8wekyb3d8bbwe\op.exe`，版本 `2.39.0`。
- `op item edit --help` 要点：
  - 「You can also edit an item using piped input: `cat updatedLogin.json | op item edit oldLogin`」；
  - 「you can't combine piped input and the `--template` flag in the same command」；
  - 「Command arguments can be visible to other processes on your machine」（赋值语句不能带值）；
  - 删除自定义字段：`'<field>[delete]'`（只含字段名）。
- Authenticode：`Status=Valid`；签名者 `CN=Agilebits, O=Agilebits, L=Toronto, S=Ontario, C=CA`；颁发者 `CN=Microsoft ID Verified CS EOC CA 03, O=Microsoft Corporation, C=US`。
- 施工者仍需在本机补测（需解锁，使用测试 vault）：管道 JSON 编辑对缺失字段是替换还是合并；`[delete]` 能否与管道输入同时使用；编辑后 item id 与归档变化；锁定、取消授权、断网、条目不存在、同名冲突五种 stderr 原文（写进分类测试样本）。

## 附录 B：D3-B（base_url 也视为秘密）时的差异

仅在用户明确要求时采用，替换 F1-2：
- base_url 继续存 vault，不建端点表；`live.rs:808` 改为在切换入口 `fetch_provider_secrets` 一次，把 base_url 传给 `write_live_snapshot`（Codex / Pi 切换各 1 次 op）。
- 切走回填：从 `config.toml` 提取出的 base_url 不触发写入；只有提取到 api_key 明文时才 fetch + put。用户直接在 `config.toml` 里改的 base_url 会在切走时丢失，需在手册写明。
- 启动路径只做 F1-8 的定点剥离，永远不重写 `config.toml`。
- 列表只显示「已配置（在 1Password）」，不显示端点值；`reveal(base_url)` 需要 1 次 op。
- 次数断言改为：切换 Codex fetch = 1、put = 0；切换 Claude fetch = 0。

