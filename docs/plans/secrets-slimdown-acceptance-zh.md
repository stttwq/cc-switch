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
17. [ ] 人为把 `~/.pi/agent/models.json` 设为只读后重启：提示有 N 个 live 配置未完成重写；恢复可写后下次启动自动补完。
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

| 项号 | 日期 | 机器/版本 | 结果 | 备注 |
|---|---|---|---|---|
|  |  |  |  |  |
