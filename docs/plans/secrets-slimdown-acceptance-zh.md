# 凭据瘦身验收记录与真机升级清单

> 依据：`secrets-credential-manager-slimdown-plan-zh.md` §9（各阶段验收）与 §10（测试策略）。
> 本文件只做两件事：登记**已经机器验证过**的门禁结果，以及列出**只能在真机上做**的升级验收步骤。
> 计划 §9 L559 要求"从 3.20.3 安装包升级到本版本的真实机器测试"，此前仓库内无任何留痕，本节即该留痕的载体。

## 一、已验证门禁

基线：`main` @ `60608ec`，日期 2026-09-19，Windows 11 本机（Node v24.18.0 / 本机 Rust 工具链）。

| 门禁 | 命令 | 结果 |
|---|---|---|
| 前端类型 | `npx tsc --noEmit` | 通过，0 错误 |
| 前端格式 | `npx prettier --check "src/**/*.{js,jsx,ts,tsx,css,json}"` | 通过（本轮顺手把前几轮工具写入产生的 CRLF 归一为 LF，与 `.gitattributes` 一致） |
| 前端单测 | `npx vitest run` | 93 files / 707 tests，连绿两次 |
| Rust 格式 | `cargo fmt --check` | 通过 |
| Rust lint | `cargo clippy --all-targets -- -D warnings` | 通过 |
| Rust 测试 | `cargo test --tests` | 全绿（lib 565 passed / 0 failed；集成含 `provider_service` 26、`import_export_sync` 24、`provider_commands` 8、`migration_v18_cold_start` 1、`log_no_secrets` 1、`app_config_load` 4） |
| 真实凭据管理器往返 | `cargo test --lib -- --ignored --exact secrets::store::tests::secrets_windows_roundtrip` | 通过 |
| 真实 `HKCU\Environment` 往返 | `cargo test --lib -- --ignored --exact env_delivery::sink::tests::env_sink_windows_roundtrip` | 通过（含 `Drop` 清理与"注册表无残留"断言） |
| 删除 `rustls` 直接依赖后的真实 HTTPS | `cargo test --lib -- --ignored --exact services::http_client::tests::https_roundtrip_uses_reqwest_default_crypto_provider` | 通过 |
| 明文字面量扫描（集成测试 home） | `scripts/secret-scan.ps1 -Path $env:TEMP\cc-switch-test-home` | 0 命中 |
| 冷启动迁移 + 整 home 明文断言 | `cargo test --test migration_v18_cold_start` | 通过（§6.2 产物、§6.3 删除面、DB free page 经 `secure_delete`+`VACUUM` 后 0 命中） |
| CI | Actions CI run | 见文末"CI 观察" |

### 已知未覆盖（不要当成已通过）

- **明文字面量扫描（全仓，豁免 fixtures）**：本机扫描在跑（仓库体积大），结果以 CI 为准。
- **S3 真实凭据往返**（`services::s3::integration_tests::live_s3_*`）：需要真实桶凭据，本机与 CI 均未跑。
- **WSL2 UNC 原子写**（`config::tests::atomic_write_replaces_existing_wsl_unc_file`）：只在 `backend-windows-wsl2` job 内有效，原生 Windows 下必然失败，属预期。
- **§6.4 的"live 文件只读 → `live_reapply_pending` 保持 1"**：仍无自动化用例。`EnvSink` 现已挂到 `AppState` 上，具备注入点，补这条测试的前置障碍已清除。
- **非 Windows 编译面**：本机只有 windows target，依赖 CI 复核。

## 二、真机升级验收清单（§9 Phase 4 / Phase 6，需人工执行）

前提：一台**已安装并实际使用过 CC Switch 3.20.3**、且装有 Claude Code / Codex / Pi 三者至少其一的 Windows 机器。**开始前先整目录备份 `%USERPROFILE%\.cc-switch`**（这是唯一回滚点）。

按顺序勾选，任一项与预期不符就记在第三节的"观察"里：

1. [ ] 旧版能正常启动并列出原有供应商；记下供应商数量、当前激活项、以及 `settings.json` 里是否配了 WebDAV / S3。
2. [ ] 关闭旧版，安装新版 MSI（覆盖安装，不先卸载）。
3. [ ] 首次启动后弹出一次性迁移对话框，且包含：已迁移 N 个供应商的密钥（只列名字不列值）、"已打开的终端需要重开"的提示。
4. [ ] 对话框列出仍含明文的历史备份 `.db` 文件路径并提供"全部删除"；点"稍后"后文件仍在，之后可从设置页再次触达。
5. [ ] 若旧版有 `auth.tokens` 形态的 Codex 官方卡：提示"已丢弃 Codex OAuth 登录态，请改用 `codex login`"。
6. [ ] 若旧版把供应商配成 `apiFormat = gemini_native`：提示"上游格式已归一，请自行核对端点"。
7. [ ] 若旧版全局出站代理 URL 含 `user:pass@`：提示该设置已作废、需重填不带凭据的地址。
8. [ ] 打开 Windows 凭据管理器，能看到 `cc-switch/v1/provider/<app>/<id>/api_key`（以及配了 Base URL 的 `.../base_url`）条目；`cc-switch/v1/probe` 不残留。
9. [ ] 用文本编辑器/`findstr` 检查 `%USERPROFILE%\.cc-switch\cc-switch.db`：搜不到任何原密钥明文（注意新版已做 `secure_delete`+`VACUUM`，free page 也应干净）。
10. [ ] 检查 `%USERPROFILE%\.cc-switch\settings.json`：WebDAV 密码与 S3 AK/SK 字段已不存在。
11. [ ] 检查 live 文件：`~/.claude/settings.json` 内没有 `ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_API_KEY` / `ANTHROPIC_BASE_URL`；`~/.codex/config.toml` 有 `env_key = "CC_SWITCH_CODEX_API_KEY"` 且没有 `experimental_bearer_token`；`~/.pi/agent/models.json` 的 `apiKey` 形如 `"$CC_SWITCH_PI_..._API_KEY"`。允许出现的唯一值是当前激活供应商的 `base_url`。
12. [ ] 在应用内切换 Claude / Codex / Pi 各一次，然后**新开**一个 PowerShell：`$env:ANTHROPIC_AUTH_TOKEN`、`$env:CC_SWITCH_CODEX_API_KEY` 等能看到值。
13. [ ] 真实发一次请求：`claude`、`codex`、`pi` 各自能正常调用当前供应商（这是唯一能证明"只环境变量投递"端到端可用的一步）。
14. [ ] 从应用内点"打开终端"，在**未重开**的会话里直接跑 CLI 也应当可用（它经 `Command::env()` 注入，不依赖注册表刷新）。
15. [ ] 设置页做一次 WebDAV/S3 连接测试并跑一次同步：能连通；导出/同步载荷中不含密钥。
16. [ ] 再次启动应用：迁移对话框不再出现；`secrets_migration_pending` 与 `live_reapply_pending` 均已清零。
17. [ ] 人为把当前激活供应商的 live 文件设为只读（`~/.claude/settings.json`，验前先把 `live_reapply_pending` 置 1）后重启：报告里出现该应用的失败项且 `live_reapply_pending` 保持 1；恢复可写后下次启动自动补完并清零。注意：`~/.pi/agent/models.json` 设只读**探不到**这条路径——Pi 的 `enable` 在节点已存在时只投环境变量、不写文件。
18. [ ] 回滚演练：退出新版，用 `~/.cc-switch/backups/pre-secrets-migration-*.db` 覆盖 `cc-switch.db`，旧版 3.20.3 能正常读取并继续使用（凭据管理器里多出的条目无害）。
19. [ ] 删除一个供应商后：其凭据条目与它托管的用户环境变量一并消失（凭据管理器与 `HKCU\Environment` 各查一次）。
20. [ ] 全仓 `ccswitch://` 深链接、自动更新检查、用量看板、端点测速均已无入口（托盘、设置页、供应商卡片三处）。

## 三、CI 观察

| 运行 | 提交 | 结论 |
|---|---|---|
| #16 | `4961510` | 仅 `Frontend Checks → Check formatting` 失败（那一版尚未含前几轮工作树改动，属历史行尾混用） |
| #17 | `e333a1a` | `Backend → Check Rust formatting` 失败（新增断言未格式化）+ `Frontend → Unit tests` 失败（默认 5s 超时在负载下不足，超时后残留 DOM 引发二次断言失败） |
| #18 | `60608ec` | 待补：两处已修（`cargo fmt`；`testTimeout`/`hookTimeout` 提到 15s，App 集成用例尾置超时 10s→40s） |

## 四、人工验收观察记录

### 第 1 轮：本机重置演练（2026-09-20）

机器与版本：本机 Windows 11（`jia`）。做法是先把 2.0.0 环境整体备份并拆干净，装上上游 3.20.3 配 3 个供应商（Claude / Codex / Pi，含自定义 Base URL 与官网地址），再用本地构建的 `CC Switch_2.0.0_x64_zh-CN.msi` 覆盖安装，逐项走第二节清单。还原点：`D:\LS\DM\ccs-verify\backup-2.0.0-20260920-105948`（配置目录、3 个 live 文件、70 条凭据的 DPAPI 导出、18 项 `HKCU\Environment`、两个 MSI 的 SHA-256）。

| 项号 | 日期 | 机器/版本 | 结果 | 备注 |
|---|---|---|---|---|
| 1 | 09-20 | 本机 3.20.3 | 通过 | 11 个供应商（含官方卡），4 行 `settings_config` 含明文密钥特征；Claude / Codex 各有激活项，Pi 未激活；升级前配好 WebDAV（坚果云，密码明文在 `settings.json`）并跑通一次上传 |
| 2 | 09-20 | 覆盖安装 | 通过 | 不卸载直接 `msiexec /i` 2.0.0 MSI |
| 3 | 09-20 | 2.0.0 首启 | 通过 | 标题「密钥已迁入 Windows 凭据管理器」，正文「已迁移 3 个供应商的密钥。已打开的终端需要重开才能读到新环境变量。」，逐条列 `claude / 君的公益`、`codex / 君的`、`pi / 君的公益`，只列名不列值 |
| 4 | 09-20 | 2.0.0 首启 | 通过 | 列出 2 个含明文的历史备份路径并给「全部删除备份」；点「知道了」后文件仍在，设置 → 高级 → 管理自动备份 可再次触达，删除按钮有响应（未实际删除，留给第 18 步） |
| 5 | 09-20 | — | 不适用 | 本机无 `auth.tokens` 形态的 Codex 官方卡；迁移报告 `dropped_codex_oauth` 为空 |
| 6 | 09-20 | — | 不适用 | 无 `apiFormat = gemini_native` 存量；报告 `warnings` 为空 |
| 7 | 09-20 | — | 不适用 | 全局出站代理未配 `user:pass@` |
| 8 | 09-20 | 2.0.0 | 通过 | 7 条规范命名条目：3 个 `api_key` + 3 个 `base_url` + `cc-switch/v1/app/webdav/password`；无 `cc-switch/v1/probe` 残留 |
| 9 | 09-20 | 2.0.0 | 通过 | 整库二进制扫 `sk-` / `AKIA` 0 命中；库体积 8.4 MB → 80 KB（`secure_delete` + `VACUUM`） |
| 10 | 09-20 | 2.0.0 | 通过 | `webdavSync` 只剩地址/账号，`password` 键消失，全文件无明文凭据 |
| 11 | 09-20 | 2.0.0 | 通过 | Claude live 无 `ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_BASE_URL`；Codex `env_key = "CC_SWITCH_CODEX_API_KEY"` 且 `experimental_bearer_token` 消失；Pi `apiKey` 变为 `"$CC_SWITCH_PI_JUN_API_KEY"` |
| 12 | 09-20 | 2.0.0 | 通过 | 切换后新开 PowerShell：进程与注册表两侧同为 51 / 18 / 51 / 51 字符，`WM_SETTINGCHANGE` 广播生效 |
| 13 | 09-20 | 2.0.0 | 通过（带说明） | `claude -p` 走到模型目录校验并给出回复，鉴权与端点均通；报的 `deepseek-v4-flash-0731` 未知模型警告来自供应商自身配置，非本产品缺陷 |
| 14 | 09-20 | 2.0.0 | 通过 | 应用内「打开终端」里直接跑 CLI 可用（`Command::env()` 注入路径有效） |
| 15 | 09-20 | 2.0.0 | **不通过 → 缺陷 D-2** | 上传被拒：「配置错误: 导出护栏拒绝: 导出文本含本次会话写入过凭据管理器的密钥」。根因见下方缺陷清单 |
| 16 | 09-20 | 2.0.0 | 通过 | 再次启动不弹迁移对话框；`secrets_migration_pending` 不存在、`secrets_migration_confirmed=1`、`live_reapply_pending=0` |
| 17 | 09-20 | 2.0.0 | 通过（验法已改） | 三周期：置 `live_reapply_pending=1` + `~/.claude/settings.json` 只读 → 启动后标志保持 1，报告记 `claude: 原子替换失败: … 拒绝访问 (os error 5)`，Codex / Pi 仍成功 → 恢复可写再启动 → 标志归 0、失败项清空、live 在 12:55:08 被重写。原清单写的 Pi `models.json` 探针无效，已按上文改写法 |
| 18 | 09-20 | 3.20.3 | 通过（带注意事项） | 用 `pre-secrets-migration_20260920_113116.db` 覆盖后启动 3.20.3：7 个页签 / 10 个供应商齐全、切换成功。注意事项：① WebDAV 密码框为空（密码已在凭据管理器，旧版读不到），回滚后需重填；② 旧版表单里 Codex key 为明文，Pi 显示成 `$CC_SWITCH_PI_JUN_API_KEY`（3.20.3 从 2.0.0 写的 live 文件反向导入所致） |
| 19 | 09-20 | 2.0.0 | 通过 | 删除供应商后：凭据 7 条、托管变量 4 项、`known_secret_targets` 7 条，与现存 3 个配置了密钥的供应商一一对应，无孤儿 |
| 20 | 09-20 | 2.0.0 | 通过（带清理项） | 托盘与供应商卡片无深链接 / 检查更新 / 用量看板 / 端点测速入口；**设置 → 关于页仍保留上游的「更新日志 / 软件下载 / 官方网站」三个链接**，属施工方案第 3 部分 G 区待清项，但用户点「软件下载」会拿到上游 3.x，风险为真，已提级 |

### 本轮暴露的缺陷

| 编号 | 现象 | 根因 | 处置 |
|---|---|---|---|
| D-1 | 编辑当前激活供应商的密钥 / 地址后不生效，CLI 仍用旧 key | `ProviderService::update`（Claude / Codex）与 `pi::update` 只写凭据与 live，不重投环境变量；投递只挂在切换路径 | 已修：两处补按新值重投（无密钥的官方卡跳过）。新增 3 条用例 `update_current_{claude,codex,pi}_redelivers_env_vars` |
| D-2 | 同步上传被永久拒绝 | 投递时把 `ANTHROPIC_BASE_URL` 也登记进「本次会话已知密钥」，导出护栏按字面量比对；而 `providers.website_url` 与 Base URL 同域是常态 | 已修：`_BASE_URL` 结尾的变量名不再登记。新增用例 `switch_claude_does_not_register_base_url_as_session_secret` |
| D-3 | 迁移对话框确认之后，live 重写失败完全静默 | 对话框显示条件是 `secrets_migration_confirmed != 1`，「重试」按钮只存在于该一次性对话框内；确认之后 `live_reapply_pending` 卡在 1 而界面毫无线索 | 已修：设置 → 高级 → 凭据管理器维护 常驻一行状态（`get_live_reapply_status` + `LiveReapplyMaintenance`），并提供「立即重试」（`run_live_reapply_now`，不必重启）。真机复验见第 3 轮 |
| D-4 | 回滚到 3.20.3 再升级 2.0.0 时，三个应用的 live 重写全部失败且每次启动同样失败 | `check_conflict` 对「同名但未登记」的变量判 `foreign` 并拒写，而回滚后的 v18 库里没有 `managed_env_vars`，`HKCU\Environment` 却留着上一轮投递的值；对话框文案又写成「通常是文件被运行中的 CLI 占用」，误导排查 | 已修：live 自动重写前先 `adopt_unregistered_managed_env` 认领按我们命名规则存在的未登记变量（手动切换仍拒绝，保护用户自设同名变量）。单元测试 `adopt_registers_leftover_names_that_conflict` + 真机复验见第 3 轮。文案未拆分（`ENV_CONFLICT` 与文件占用仍共用一句），留作后续 |

附带结论：施工方案第 1 部分 1.1 标注的「顶层 `baseUrl` 可能明文落进 `providers.settings_config`」在本轮真机**未复现**——5 个供应商行的顶层键只有 `env` / `hooks` / `auth` / `config` / `name` / `models` / `api` / `apiKey`，无 `baseUrl`。1.4.2 的提取器防御仍建议保留（成本一行），但风险等级可从「待核实」降为「纵深防御」。

另记一次意外收获：第 18 步期间误启动了 2.0.0（覆盖安装时 MSI 就地升级了 3.20.3 的安装目录），等于对同一份 v18 备份**再迁移一次**——迁移可重入，仍报「已迁移 3 个供应商」并新出一份 `pre-secrets-migration_*.db`；同时暴露出 D-4。

### 第 2 轮：D-1 / D-2 修复复验（2026-09-20，同一台机器）

装的是含两处修复的本地构建（`CC Switch_2.0.0_x64_zh-CN.msi`，SHA-256 `2789B91E…`；被替换的基线包另存为 `ccs-verify/CC-Switch-2.0.0-baseline-64D25B8A.msi`）。

| 项号 | 结果 | 证据 |
|---|---|---|
| 15 | **通过** | 上传成功，不再报「导出护栏拒绝」。把远端载荷拉回本地独立审计：`db.sql` 23 840 字节，密钥模式（`sk-` / `sk-ant-` / `AKIA` / `xai-` / `Bearer`）**0 命中**，WebDAV 口令 **不在载荷里**；`manifest.json` 同样 0 命中。域名 `api.llm.pm` 在 `db.sql` 出现 2 次，来自 `providers.website_url`（供应商官网地址，本就要同步，不是密钥） |
| D-1 复验 | **通过** | 编辑 Claude / Codex / Pi 的密钥与地址后**不做任何切换**，凭据管理器与 `HKCU\Environment` 两侧哈希与长度即一致；最硬的一条是 Codex `base_url` 从 18 字符变 20 字符且注册表同步。本地 `settings.json` 也不再含 WebDAV 口令 |
| 回归 | 通过 | 整库扫明文 0 命中；`live_reapply_pending=0`、失败项清空；三个 live 文件形态正确（Claude 无残留、Codex 有 `env_key`、Pi 为 `$` 引用）；`managed_env_vars` 四项登记齐全 |

D-3、D-4 未在本轮修复，另见任务清单。

### 第 3 轮：2.0.1 真机复验（2026-09-20，同一台机器）

装的是 `CC Switch_2.0.1_x64_zh-CN.msi`（跨版本升级会把 2.0.0 那条安装记录顶掉，卸载表里只剩一条）。四个缺陷全部复验通过：

| 项 | 结果 | 证据 |
|---|---|---|
| D-1 编辑即生效 | **通过** | 只改当前 Claude 供应商的请求地址（`https://api.llm.pm` → `…pmx` → 改回），全程不切换：凭据 `base_url` 与 `ANTHROPIC_BASE_URL` 同步变成 `len=19 sha=bb12b788`，再同步回到 `len=18 sha=31d259d1`。另外确认"只填地址不填 key"保存后 key 保持原值（`a2ecff15` 未变），"留空保持不变"语义正常 |
| D-2 同步不再被误拒 | **通过** | 上传成功；远端 `db.sql` 23 840 字节，密钥模式 0 命中、WebDAV 口令不在载荷内，`manifest.json` 同样 0 命中 |
| D-4 回滚后再升级 | **通过** | 构造场景：删掉 `managed_env_vars` 登记 + `live_reapply_pending=1` + 把 `ANTHROPIC_AUTH_TOKEN` 改成 42 字符假值。启动后日志出现三条「认领上一轮投递的用户环境变量」（`ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_BASE_URL` / `CC_SWITCH_CODEX_API_KEY`），三个应用 live 重写全部 `✓`，`pending` 归 0、失败项清空、登记重新齐全，假值被真值（51 字符）覆盖。修复前这里是三条 `ENV_CONFLICT` 且标志永久卡在 1 |
| D-3 失败不再静默 | **通过** | 构造只读的 `~/.claude/settings.json` + `pending=1` → 设置 → 高级 → 凭据管理器维护 出现「有 N 个 live 配置未完成重写」并列出 `claude: 原子替换失败: … 拒绝访问 (os error 5)`；解除只读后点「立即重试」→ 当场补完、该行消失、`pending=0`、live 文件 16:51:31 被重写，无需重启 |

过程中的两点记录：

- 新加的 `livePendingHint` 起初写成 `{count}`（i18next 需要 `{{count}}`），真机上直接显示出字面量；改回时又用批量替换误伤了 ja 的既有键 `cleanupOrphansDone`，被 `localeCoverage` 用例拦下。**教训：改语言文件必须逐键改，不能整串替换。**
- 第 17 步与第 15 步用到的探针文件属性、`live_reapply_pending`、`managed_env_vars` 均已归零/复原，本机处于干净的 2.0.1 状态。

### 回滚注意事项（第 18 步补充）

- 回滚到 3.20.3 后，WebDAV 密码框为空（口令已在凭据管理器，旧版读不到），需重填一次。
- **3.20.3 会重写 `settings.json`**：本轮演练期间它把 `webdavSync.remoteRoot` 从用户自填的 `ccs` 打回默认值 `cc-switch-sync`。也就是说回滚再升级后，新版会把数据传到**另一个远端目录**，旧目录成为孤儿。回滚前需提醒用户记下自己的 `remoteRoot`。
- 旧版表单里 Codex 的 key 是明文，Pi 的请求地址与 key 在 2.0.x 的编辑页**都不回显**（施工方案 §1.1 的断链判断在真机确认）——这正是 2.1 第 1 部分要解决的用户痛点。

