# CC Switch 安全改造与瘦身实施方案

> 文档性质：施工方案（给实施模型 / 工程师阅读），不是设计讨论稿。
> 编写日期：2026-09-13。基线版本：cc-switch 3.20.3，`SCHEMA_VERSION = 18`。
> 所有 `文件:行号` 均以当前工作树为准，实施时若行号漂移，以函数名 / 符号名为准。

---

## 0. 怎么读这份文档

- 第 1 节是目标与非目标，第 2 节是**需要用户确认的决策清单**（每条都给了默认值，未获确认时按默认值施工）。
- 第 3 节是**总体施工原则**，任何与后文冲突的地方以第 3 节为准。
- 第 4 节是现状事实（密钥在哪、怎么流动、什么依赖什么），是后面所有改动的依据。
- 第 5 ～ 8 节是目标架构、迁移、删除面、安全加固的具体做法（关键点 + 怎么做 + 为什么）。
- 第 9 节是分阶段施工顺序与每阶段验收标准，**必须按顺序执行**，每阶段结束时 `pnpm typecheck && pnpm test:unit` 与 `cargo clippy -- -D warnings && cargo test` 必须全绿。
- 附录是速查表。

---

## 1. 目标与非目标

### 1.1 用户原始需求 → 本方案的映射

| 用户需求 | 本方案对应 |
|---|---|
| 密钥与 Base URL 当前明文存储，要移除明文存储 | 第 5.1、5.2 节：所有密钥与 Base URL 从 SQLite / `settings.json` / live 配置文件中剥离，唯一持久化位置是 Windows Credential Manager |
| 使用 Windows Credential Manager | 第 5.1 节：`SecretStore` 抽象 + `keyring` crate `windows-native` 后端 |
| 只保留环境变量方式 | 第 5.3 节：向 Claude Code / Codex / Pi 投递凭据的**唯一**方式是环境变量；live 配置文件里只允许出现环境变量的**名字**，不允许出现值 |
| 本地路由功能直接删除 | 第 7.1 节：`src-tauri/src/proxy/**` 及其全部附属（故障转移、用量看板、流式检测、定价、接管） |
| 加强对安全性的重视 | 第 3 节原则 + 第 8 节加固清单 |
| 除 Claude / Codex / Pi 外其他应用移除 | 第 7.3 节：Gemini、OpenCode(+OMO)、OpenClaw(+Workspace)、Hermes、GrokBuild(+xAI OAuth)、Claude Desktop、Copilot |
| 更新功能移除 | 第 7.2 节：`tauri-plugin-updater` 及发布链路中的签名 / `latest.json` |
| 已有配置自动迁移 | 第 6 节：schema v19 + 凭据迁移 v1 + live 文件重写 + 明文残留清理，启动时自动、幂等、失败可重试 |

### 1.2 非目标（明确不做）

- 不做跨平台的凭据存储实现。本方案只保证 Windows；macOS / Linux 构建保留可编译，但 `SecretStore` 返回"不支持"错误（见决策 D1）。
- 不引入任何新的网络监听端口、不保留任何形式的请求转发。
- 不做"用 helper 命令读取密钥"的投递方式（Claude 的 `apiKeyHelper`、Pi 的 `"!command"`）。原因：用户要求只保留环境变量方式；helper 方式会引入子进程执行面，与"减少攻击面"冲突。
- 不做密钥在内存中的硬件级保护；做到 `zeroize` 级别即可。

---

## 2. 需要用户确认的决策清单

实施前请用户逐条确认。未确认时按"默认"施工。每条都影响删除面或数据模型，改起来代价不同，所以先定。

| 编号 | 决策 | 默认 | 理由 | 若选另一项的代价 |
|---|---|---|---|---|
| D1 | 支持平台 | **仅 Windows**。CI 后端矩阵只保留 `windows-latest`（保留 WSL2 契约测试）；release 只产 Windows 安装包 | 用户明确要求 Windows Credential Manager；非 Windows 没有等价的"仅环境变量"语义验证 | 用 `keyring` 的 `apple-native` / `linux-native` 特性可扩展，但需要各平台的验收 |
| D2 | Base URL 是否也进凭据管理器 | **是**（用户原话"密钥和 baseurl"） | 一致的心智模型："凡是供应商身份信息都不在 DB 明文里" | 若只保护密钥，第 5.2 节中 `base_url` 分支全部退化为普通 JSON 字段，工作量减少约 15% |
| D3 | Codex OAuth 托管多账号（cc-switch 自己做 ChatGPT 登录并写 `~/.codex/auth.json`） | **移除** | 它把 refresh token 明文存于 `~/.cc-switch/codex_oauth_auth.json`；其"让 Claude Code 使用 ChatGPT 订阅"的路径依赖本地代理，代理删除后主要价值消失；Codex 自带 `codex login` 且支持 `cli_auth_credentials_store = "keyring"` | 保留则需把 token 迁入凭据管理器，并且 cc-switch 仍要写 `auth.json` 明文（Codex 原生格式），与"不写明文"原则冲突 |
| D4 | 供应商卡片"用量 / 余额 / 订阅额度"家族（`usage_script.rs` JS 执行器、`balance`、`coding_plan`、`subscription`、`usage_cache`） | **移除** | `meta.usage_script` 是第二个明文密钥存储点；`rquickjs` 执行用户脚本并可发网络请求，属于可删的攻击面；`subscription` 会读取各 CLI 自己的 OAuth 凭据 | 保留则 `UsageScript` 中的 `api_key / access_token / access_key_id / secret_access_key` 也要进凭据管理器，并保留 `rquickjs` |
| D5 | `ccswitch://` 深链接导入 | **移除**（含 `deplink.html`、`tauri-plugin-deep-link`、`src-tauri/src/deeplink/**`） | SECURITY.md 自己把它列为首要不可信输入；它的核心用途是从 URL 里携带 `apiKey` 导入，与本方案冲突 | 保留则必须剥离所有携带密钥的参数（`apiKey`、`usageApiKey`、`usageAccessToken`、base64 `config` 内嵌密钥），改为导入后由用户手动输入密钥 |
| D6 | "通用供应商"（一个 key 扇出到多个应用，`dao/universal_providers.rs`） | **移除** | 它在 `settings` 表以明文存 `apiKey`；扇出目标一半是被删除的应用 | 保留则需要给它单独的凭据条目并重写扇出逻辑 |
| D7 | 轻量模式（托盘常驻、销毁 WebView） | **保留** | 与安全无关，自包含 | 无 |
| D8 | Claude 插件集成 / 跳过 onboarding（`claude_plugin.rs`） | **保留** | Claude 相关，小而独立 | 无 |
| D9 | 端点测速与 `provider_endpoints` 表 | **移除** | 该表以明文存候选 Base URL，与 D2 冲突；测速功能价值低 | 保留则候选 URL 需要随 Base URL 一起进凭据管理器 |
| D10 | 全局出站 HTTP 代理设置（`global_proxy_url`，供 WebDAV/S3/Skills 下载使用，**不是**本地路由） | **保留，但禁止 URL 内嵌 `user:pass@`** | 同步与 Skills 下载仍需要它；代理认证极少用 | 若需要代理认证，把凭据也放进凭据管理器 |
| D11 | 用户级环境变量的写入位置 | **`HKCU\Environment`（用户级），永不写 `HKLM`** | 无需管理员权限；用户变量在同名时覆盖系统变量（`PATH` 除外，本方案不碰 `PATH`） | 无 |

---

## 3. 总体施工原则

### 3.1 安全原则（优先级最高）

1. **单一真源**：一个密钥在磁盘上只允许存在于一个地方——Windows Credential Manager。SQLite、`settings.json`、备份、导出、同步载荷、日志、临时文件、前端状态，都不得出现密钥值。Base URL 同样适用（D2）。
2. **值不落地，只落名字**：投递给 CLI 的 live 配置文件（`~/.claude/settings.json`、`~/.codex/config.toml`、`~/.pi/agent/models.json`）只允许写环境变量的**名字**（Codex `env_key`、Pi `"$VAR"`）或者什么都不写（Claude Code 直接读进程环境）。
3. **前端零密钥**：IPC 永不把密钥值返回给 WebView。前端只拿到"是否已配置 + 末 4 位提示"。写入方向允许（用户在表单输入）。
4. **失败关闭（fail-closed）**：凭据管理器不可用时，拒绝保存 / 切换并明确报错；绝不退回明文写入。迁移在凭据写入失败时不得剥离 DB 里的旧值。
5. **所有入口统一经过一个提取器**：新增、编辑、导入 SQL、从 live 文件回填、同步还原——所有让 `settings_config` 进入 DB 的路径都必须先经过 `SecretExtractor`（第 5.2.3 节），没有旁路。
6. **删除优先于加固**：能删掉的功能不加固。先做第 7 节的删除，再做第 5 节的改造，改造面会小很多。
7. **最小依赖 / 最少 unsafe**：凭据访问用 `keyring` crate 的 Windows 后端（成熟、内部使用 `CredWriteW/CredReadW`），不自己写 FFI。新增的 Windows API 调用仅限 `SendMessageTimeoutW` 广播（第 5.3.2 节）。
8. **日志与错误信息不含密钥**：`ProviderSecrets` 实现自定义 `Debug`；错误信息只报"哪个字段"，不报值。

### 3.2 工程原则

1. **不重编号、不改历史迁移**：`SCHEMA_VERSION` 从 18 升到 19，历史 `migrate_vN_to_vN+1` 函数原样保留（旧库要能一路升上来）。
2. **每个阶段可独立编译、测试、提交**。禁止一次性巨型改动。
3. **删除就是删除**：不用 `#[allow(dead_code)]`、不用 `cfg(feature)` 藏起来、不留 TODO 注释。`cargo clippy -- -D warnings` 会把死代码顶上来，这是设计好的护栏。
4. **i18n 四份语言文件同步删键**：`tests/config/localeCoverage.test.ts` 强制四份 locale 键集合一致，删键必须四份一起删。
5. **测试隔离**：单元测试不得触碰真实的 Credential Manager 和真实的 `HKCU\Environment`。通过 `SecretStore` / `EnvSink` trait 的内存实现来测。真实后端只在一个明确标记的集成测试里跑。
6. **命名约定固定**（附录 B、C），任何人不得临时发明新的 target 名或环境变量名。
7. **迁移必须幂等**：任何一步中断后，下次启动重跑不会造成重复、丢失或二次明文落地。

---

## 4. 现状关键事实（改造依据）

以下事实由代码阅读得出，是后面每一条改动的"为什么"。

### 4.1 密钥现在在哪里（持久化位置）

| 位置 | 内容 | 代码 |
|---|---|---|
| `~/.cc-switch/cc-switch.db` 表 `providers.settings_config`（TEXT） | 每个供应商的完整 JSON，含密钥与 Base URL | `database/schema.rs:26-46`；读写 `database/dao/providers.rs:20-109, 130-178, 180-278` |
| 同表 `providers.meta`（TEXT） | `usage_script` 内含 `api_key / access_token / access_key_id / secret_access_key` | `provider.rs:268-318, 444-565` |
| `settings` 表 | `universal_providers`（含 `apiKey`）、`global_proxy_url`（可含 `user:pass@`）、`claude_desktop_gateway_token` | `dao/universal_providers.rs:11,66-69`；`dao/settings.rs:141-171`；`claude_desktop_config.rs:278-289` |
| `proxy_live_backup.original_config` | 代理接管时对 live 文件的整份快照（含密钥） | `schema.rs:265-276`；`dao/proxy.rs:779-796` |
| `~/.cc-switch/settings.json` | `webdav_sync.password`、`s3_sync.access_key_id / secret_access_key` | `settings.rs:114-131, 189-210, 575-582, 688-724` |
| `~/.cc-switch/codex_oauth_auth.json`、`copilot_auth.json`、`xai_oauth_auth.json` | OAuth refresh / access token | `proxy/providers/codex_oauth_auth.rs:379,1944`；`copilot_auth.rs:442,1489`；`xai_oauth_auth.rs:211` |
| `~/.cc-switch/config.json`、`.bak`、`.migrated` | 旧版 JSON 配置，含全部供应商密钥；迁移后仅重命名不删除 | `app_config.rs:528-698`；`lib.rs:637-643` |
| `~/.cc-switch/backups/*.db` | DB 文件级备份，等于 DB 明文副本 | `database/backup.rs:476-573` |
| `~/.cc-switch/backups/env-backup-*.json` | 删除环境变量前的备份，含变量值 | `services/env_manager.rs:43-66` |
| `%TEMP%/claude_<id>_<pid>.json` | "打开终端"时把 `env.*`（含 token）写进临时文件传给 `--settings` | `commands/misc.rs:3745-3905` |

### 4.2 密钥在每个应用 JSON 里的位置（`Provider::resolve_usage_credentials`，`provider.rs:145-256`）

| 应用 | API Key | Base URL |
|---|---|---|
| Claude | `env.ANTHROPIC_AUTH_TOKEN` 或 `env.ANTHROPIC_API_KEY`（由 `meta.api_key_field` 决定，`provider.rs:103-109, 503-504`） | `env.ANTHROPIC_BASE_URL` |
| Codex | `auth.OPENAI_API_KEY`；第三方供应商在写 live 时被提升为 `[model_providers.<id>].experimental_bearer_token`（`codex_config.rs:4020-4045, 3349-3393`） | `config`（TOML 文本）中 `[model_providers.<id>].base_url` |
| Pi | 顶层 `apiKey` | 顶层 `baseUrl`（个别在 `models[i].baseUrl`） |

### 4.3 live 文件现在怎么写（切换供应商时）

- 入口：`services/provider/mod.rs:5185 ProviderService::switch` → `:5278 switch_normal` → `services/provider/live.rs:704 write_live_with_common_config_for_state` → `:1277 write_live_snapshot`。
- 切换前会把当前 live 文件**回填**进被切走的供应商的 DB 行（`mod.rs:5323-5352`，经 `live.rs:1707 read_live_settings`）。这意味着密钥也会从磁盘反向流入 DB，必须在回填处也过提取器。
- Claude：`live.rs:1279-1283` 把 `settings_config` 几乎原样写进 `~/.claude/settings.json`（只剥 `api_format` 等内部字段，`live.rs:168-178`）。
- Codex：`live.rs:1291-1320` → `codex_config.rs:2691 write_codex_provider_live_with_catalog` → `:3974 write_codex_live_for_provider` → `:3816 plan_codex_live_write`。官方卡整份写 `auth.json`；第三方只写 `config.toml` 并注入 bearer token。Codex 已原生支持 `env_key`（`codex_config.rs:2756-2803`，`:3384 codex_provider_table_declares_auth` 把带 `env_key` 的表视为"已有认证"），这是天然的落点。
- Pi：`pi_config/mod.rs:121, 143, 170` 把含 `apiKey` 的节点原样写进 `models.json`；`services/provider/pi.rs:221-250 sync_native_locked` 每次列表都把 `models.json` 里的 `apiKey` 反向导入 DB。

### 4.4 CLI 侧对"环境变量投递"的原生支持（已核实）

| CLI | 版本（本机） | 支持方式 | 依据 |
|---|---|---|---|
| Claude Code | — | 直接读进程环境变量 `ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_API_KEY` / `ANTHROPIC_BASE_URL`；`settings.json.env` 里的同名键会覆盖进程环境 | Claude Code 文档；因此 live 文件里**必须删掉**这些键，否则覆盖掉环境变量 |
| Codex | 0.153.4 | `[model_providers.<id>] env_key = "VAR"`：从该环境变量读 key；缺失则报错（fail-closed）。内置 `openai` 提供者读 `OPENAI_API_KEY`。`base_url` 只能写在 TOML 里，没有环境变量间接引用 | `codex_config.rs:2756-2803` 注释与 Codex 源码 `resolve_provider_auth` |
| Pi | 0.85.1 | `apiKey: "$VAR"` 或 `"${VAR}"` 做环境插值；缺失时值"unresolved"，模型不可用（fail-closed）。`headers` 同样支持。`baseUrl` 不支持插值 | Pi 包内 `docs/models.md` "Value Resolution" 一节 |

### 4.5 现有环境变量机制

- `services/env_checker.rs:20-101`：只读检测 `HKCU\Environment` 与 `HKLM\...\Environment` 里前缀匹配的变量，**把变量值明文返回给前端**（`EnvConflict.var_value`，前端 `EnvWarningBanner.tsx:199` 直接渲染）。
- `services/env_manager.rs:21-40, 153-197`：删除（先写明文备份文件）与恢复，用 `winreg` 直接改注册表，**没有广播 `WM_SETTINGCHANGE`**，所以新开的终端不一定能看到变化。
- 现在没有任何"把密钥当环境变量投递"的模式，环境变量只被当作"冲突"来删除。

### 4.6 本地路由与供应商切换的耦合（删除时必须拆开的地方）

- `ProviderService::switch`（`mod.rs:5185-5275`）：`:5216-5222` 取 `proxy_service.lock_switch_for_app`；`:5227-5249` 检测接管并阻止官方卡；`:5251-5271` 热切换后提前返回。删除后 `switch` 退化为"取锁 + `switch_normal`"。
- `live.rs:1513-1673`：`proxy_owns_live_config`、`sync_live_for_provider_respecting_takeover` 等一族，全部删除。
- `services/proxy.rs` 中的接管写入把 `ANTHROPIC_BASE_URL = http://127.0.0.1:<port>` 与占位符 `PROXY_MANAGED` 写进 live 文件（`:425-560, :3454-3700`）。迁移时必须识别并清掉这些占位符（第 6.4 节）。
- `proxy/switch_lock.rs`（`SwitchLockManager`）被非代理代码使用（`services/provider/pi.rs`、`services/pi_state.rs:21`、`mod.rs:4592,4707,5218`），**要搬迁不能删**。
- `proxy/http_client.rs` 是全局 reqwest 客户端，被 WebDAV/S3/Skills/模型拉取使用，**要搬迁不能删**。
- `proxy/types.rs::LogConfig` 被日志配置使用，**要搬到 `settings.rs`**。
- `proxy/providers/codex.rs` 的 `is_codex_official_provider`、`resolve_codex_catalog_tool_profile` 被 `services/provider/*`、`tray.rs:312` 使用，**要搬进 `codex_config.rs`**。

---

## 5. 目标架构

### 5.1 `SecretStore`：Windows Credential Manager 封装

**关键点**：新建 `src-tauri/src/secrets/` 模块，提供一个 trait 与两个实现。

```rust
// src-tauri/src/secrets/mod.rs
pub trait SecretStore: Send + Sync {
    fn get(&self, target: &SecretTarget) -> Result<Option<Zeroizing<String>>, SecretError>;
    fn set(&self, target: &SecretTarget, value: &str) -> Result<(), SecretError>;
    fn delete(&self, target: &SecretTarget) -> Result<(), SecretError>;   // 不存在视为成功
    fn probe(&self) -> Result<(), SecretError>;                            // 启动自检：写-读-删一个探针条目
}
```

- `SecretTarget` 是一个结构化类型，`Display` 输出即 Credential Manager 的 target name，格式固定为 `cc-switch/v1/<scope>/<owner>/<field>`（附录 B）。禁止用裸字符串拼 target。
- 实现一：`WindowsCredentialStore`，基于 `keyring = { version = "3", default-features = false, features = ["windows-native"] }`。用 `Entry::new_with_target(&target.to_string(), "cc-switch", &target.owner())` 精确控制 target name；`set_password / get_password / delete_credential`。`Error::NoEntry` 映射为 `Ok(None)`。
  - 为什么用 `keyring` 而不是自己调 `CredWriteW`：安全改造不应新增手写 FFI；`keyring` 的 Windows 后端就是 `CredWriteW/CredReadW/CredDeleteW` + `CRED_TYPE_GENERIC`，静态数据由 DPAPI 保护，本机 cargo 缓存已有 3.6.3。
  - 已知限制：Windows 单条 blob 上限 `CRED_MAX_CREDENTIAL_BLOB_SIZE = 2560` 字节，密码以 UTF-16 存，即最多 1280 个字符。`set` 前校验长度，超限报 `SecretError::TooLong { field, max }`。API key 与 URL 远小于该上限。
  - 已知限制：`keyring` 硬编码 `CRED_PERSIST_ENTERPRISE`（随漫游配置文件漫游）。接受。
  - 并发：`keyring` 文档说明同一条目的多线程读写不保证顺序。`WindowsCredentialStore` 内部用一把 `Mutex<()>` 把所有写操作串行化。
- 实现二：`InMemorySecretStore`（`HashMap<String, String>` + `Mutex`），仅 `cfg(test)`，供全部单元测试使用。
- 非 Windows：`UnsupportedSecretStore`，所有方法返回 `SecretError::Unsupported`。启动时 `probe()` 失败 → 走第 6.6 节的阻断式提示。
- `AppState` 新增 `secrets: Arc<dyn SecretStore>`。所有业务代码只通过 `AppState.secrets` 访问，禁止在别处直接 `keyring::Entry::new`。
- 内存中的密钥一律用 `zeroize::Zeroizing<String>`（新增依赖 `zeroize = "1"`）。

### 5.2 数据模型

#### 5.2.1 `Provider` 结构（`provider.rs:10-44`）

- `settings_config: Value` 保留，但**定义变更**为"已剥离密钥与 Base URL 的配置"。DB 里存的就是它。
- 新增运行时字段（不入 DB、不入 IPC）：

```rust
pub struct ProviderSecrets {
    pub api_key: Option<Zeroizing<String>>,
    pub base_url: Option<String>,
    /// 其他敏感 env 键（Claude 的 OPENROUTER_API_KEY 等、Pi 的敏感 header）
    pub extra_env: BTreeMap<String, Zeroizing<String>>,
}
impl fmt::Debug for ProviderSecrets { /* 只打印键名与是否存在 */ }
```

- `Provider` 不直接持有 `ProviderSecrets`；需要时通过 `SecretService::load(app, id) -> ProviderSecrets` 按需读取。这样 `get_all_providers` 之类的批量读取默认不触碰凭据管理器。
- 删除 `meta.usage_script`（D4）、`in_failover_queue`（`provider.rs:43,66,814`）、`custom_user_agent`（代理专用）。

#### 5.2.2 IPC 契约（前端可见的形状）

`get_providers` / `get_provider` 返回：

```ts
interface Provider {
  id: string; name: string; settingsConfig: object;   // 已剥离
  secretStatus: {
    apiKey: { present: boolean; hint: string | null };   // hint = 末 4 位，长度 < 8 时为 null
    baseUrl: string | null;                              // 允许回显（D2 下它仍来自凭据管理器，按需读取）
    extraEnv: string[];                                  // 只有名字
  };
  meta: ProviderMeta;  // 去掉 usageScript
}
```

`add_provider` / `update_provider` 入参新增：

```ts
interface ProviderSecretsInput {
  apiKey?: string | null;     // undefined = 不变；null = 删除；string = 设置
  baseUrl?: string | null;
  extraEnv?: Record<string, string | null>;
}
```

- 为什么 `baseUrl` 允许回显：它要在卡片与编辑表单里显示，用户要能看到自己切到了哪个端点。它仍然不进 DB、不进导出、不进同步。
- `read_live_settings`（`mod.rs:6354`）：Codex 分支现在返回 `{auth, config}`，其中 `auth` 是 `~/.codex/auth.json` 内容（可能含用户自己的 ChatGPT token）。改为只返回 `config`，且 `config` 文本经过 `SecretExtractor::strip_codex_toml` 后再返回。

#### 5.2.3 `SecretExtractor`：唯一入口

`src-tauri/src/secrets/extractor.rs`：

```rust
pub struct Extracted { pub stripped: Value, pub secrets: ProviderSecrets }
pub fn extract(app: &AppType, provider_id: &str, meta: &ProviderMeta, raw: &Value) -> Result<Extracted, AppError>;
pub fn hydrate(app: &AppType, stripped: &Value, secrets: &ProviderSecrets) -> Value; // 仅供写 live / 内存使用
```

按应用的提取规则（**只认这些位置，别处的疑似密钥按 `extra_env` 规则处理**）：

| 应用 | api_key 来源 | base_url 来源 | extra_env 来源 | 剥离后 JSON 的样子 |
|---|---|---|---|---|
| Claude | `/env/ANTHROPIC_AUTH_TOKEN` 或 `/env/ANTHROPIC_API_KEY`（按 `meta.api_key_field`；两者都在时以 `api_key_field` 为准并删掉另一个） | `/env/ANTHROPIC_BASE_URL` | `/env/<K>` 且 `is_sensitive_config_key(K)`（`mod.rs:5821`，把它搬到 `secrets/rules.rs`） | `env` 里不再有上述键 |
| Codex | `/auth/OPENAI_API_KEY`；以及 `/config` TOML 文本里任何 `[model_providers.*].experimental_bearer_token`（提取后删行） | `/config` TOML 里**当前激活的** `[model_providers.<id>].base_url`（`<id>` 取顶层 `model_provider`）；提取后删行 | 无 | `auth` 变为 `{}`；TOML 不含 bearer token 与激活表的 `base_url` |
| Pi | 顶层 `/apiKey`，仅当它是**字面量**（不以 `$`、`!` 开头；`$$`、`$!` 转义视为字面量） | 顶层 `/baseUrl` | `/headers/<K>` 中键名命中敏感规则且值为字面量 | 顶层无 `apiKey`、`baseUrl`；敏感 header 值改为 `"$<VAR>"`（VAR 见附录 C） |

- 已经是 `"$VAR"` 形式的 Pi `apiKey`：不提取、原样保留（用户自己在管理，我们不接管）。
- Pi `models[i].baseUrl`（模型级）：不提取，保留，并在表单校验时拒绝新增（提示"请使用供应商级 baseUrl"）。迁移时若存在，记录一条 warning 日志，不阻断。
- Codex 官方卡（`is_codex_official_provider`）：`auth.OPENAI_API_KEY` 若存在则提取为 `api_key`；OAuth `tokens` 字段**不提取、不保留、直接丢弃**（那是 Codex 的登录态，D3 之后 cc-switch 不再持有）。

#### 5.2.4 SQLite 变更（schema v19）

在 `schema.rs::apply_schema_migrations_on_conn` 的 `match version` 增加 `18 => migrate_v18_to_v19`，`SCHEMA_VERSION = 19`。内容：

1. `DROP TABLE IF EXISTS`：`proxy_config`、`provider_health`、`proxy_request_logs`、`model_pricing`、`stream_check_logs`、`proxy_live_backup`、`usage_daily_rollups`、`session_log_sync`、`session_usage_dedup`、`provider_endpoints`（D9）。
2. `DELETE FROM providers WHERE app_type NOT IN ('claude','codex','pi')`；同样清理 `prompts`、`mcp_servers` 的按应用启用位（列保留、置 0）、`skills` 相关按应用行、`profiles.payload` 中引用已删应用的条目（在 Rust 侧反序列化后过滤）。
3. `ALTER TABLE providers DROP COLUMN in_failover_queue`（bundled SQLite ≥ 3.35 支持）。
4. `DELETE FROM settings WHERE key IN (...)`：`universal_providers`、`claude_desktop_gateway_token`、所有 `proxy_takeover_*`、`rectifier_config`、`optimizer_config`、`copilot_optimizer_config`、`skills_ssot_migration_pending` 以外的代理相关键（以 `dao/settings.rs:147-311` 列出的为准）。`global_proxy_url` 保留但在 Rust 侧校验去掉 userinfo（D10）。
5. 对 `providers.meta` 逐行 `json_remove('$.usage_script')`（在 Rust 侧做，避免依赖 JSON1 的行为差异）。
6. `INSERT OR REPLACE INTO settings (key,value) VALUES ('secrets_migration_pending','1')`，作为第 6 节凭据迁移的触发器（模式与 `skills_ssot_migration_pending` 一致，`lib.rs:672-709`）。

注意：**v19 不剥离 `settings_config` 里的密钥**。剥离需要凭据管理器可用，放在启动流程的凭据迁移步骤里做（第 6.2 节），这样 schema 迁移仍是纯 SQL、可在内存库里单测。

#### 5.2.5 `AppSettings`（`settings.rs:347-502`）

- `WebDavSyncSettings.password`、`S3SyncSettings.access_key_id / secret_access_key` 从结构体删除，改为 `SecretTarget::app("webdav","password")` 等三个条目。
- `settings.rs:767-777 get_settings_for_frontend` 的"清空密码"逻辑与 `commands/settings.rs:17-42` 的"空则沿用旧值"逻辑改为：前端传 `password?: string | null` 三态；后端只在有值时写凭据管理器。`webdav_sync.rs:46-56 resolve_password_for_request`、`s3_sync.rs:47-48` 改为从凭据管理器读。
- 删除 `enable_local_proxy`、`proxy_confirmed`、`usage_confirmed`、`usage_dashboard_refresh_interval_ms`、`session_auto_sync_enabled`、`enable_failover_toggle`、`failover_confirmed`、`webdav_backup`（旧字段）以及被删应用的目录覆盖字段（`settings.rs:425-433`）。`serde(deny_unknown_fields)` 不要开，旧文件里的多余字段静默忽略，文件在下一次保存时自然变干净。

### 5.3 投递层：环境变量

#### 5.3.1 每个应用的投递规则

| 应用 | 用户级环境变量（写 `HKCU\Environment`） | live 文件里写什么 | live 文件里**禁止**出现什么 |
|---|---|---|---|
| Claude | `ANTHROPIC_AUTH_TOKEN` 或 `ANTHROPIC_API_KEY`（按 `api_key_field`），`ANTHROPIC_BASE_URL`，以及 `extra_env` 里的每一个键（原名） | `~/.claude/settings.json`：剥离后的 `settings_config`（仍含模型名、`CLAUDE_CODE_MAX_CONTEXT_TOKENS` 等非敏感 env） | `env.ANTHROPIC_AUTH_TOKEN`、`env.ANTHROPIC_API_KEY`、`env.ANTHROPIC_BASE_URL`、任何命中敏感规则的 env 键。写入前再过一次 `assert_no_secret_keys`，命中即报错不写 |
| Codex 第三方 | `CC_SWITCH_CODEX_API_KEY` | `~/.codex/config.toml`：`[model_providers.<id>]` 表中写 `env_key = "CC_SWITCH_CODEX_API_KEY"` 与 `base_url = "<来自凭据管理器>"`；删除 `experimental_bearer_token` | `experimental_bearer_token`、`requires_openai_auth = true`（保留现有安全门 `codex_config.rs:3918-3933` 的语义：无 key 且会回退到 `auth.json` 的配置拒绝写入） |
| Codex 官方（用 API key） | `OPENAI_API_KEY` | `config.toml` 去掉 `model_provider` 指向自定义表的行（沿用现有官方切换逻辑） | 不再写 `~/.codex/auth.json`（D3）。若旧 `auth.json` 里只有 `OPENAI_API_KEY` 且与迁移的 key 相同，迁移时删掉该键；有 ChatGPT `tokens` 的文件一律不动 |
| Codex 官方（ChatGPT 登录） | 无 | 同上 | 同上；登录态完全交给 Codex |
| Pi（可同时启用多个） | 每个启用的供应商一个：`CC_SWITCH_PI_<KEY>_API_KEY`；敏感 header：`CC_SWITCH_PI_<KEY>_HEADER_<NAME>` | `models.json.providers.<key>`：`apiKey: "$CC_SWITCH_PI_<KEY>_API_KEY"`，`baseUrl: "<来自凭据管理器>"`，敏感 header 值 `"$CC_SWITCH_PI_<KEY>_HEADER_<NAME>"` | 字面量 `apiKey`；字面量敏感 header |

- 为什么 Codex 第三方用专名 `CC_SWITCH_CODEX_API_KEY` 而不是 `OPENAI_API_KEY`：避免覆盖用户给别的工具设置的 `OPENAI_API_KEY`，且所有权一目了然。官方卡必须用 `OPENAI_API_KEY`，因为 Codex 内置 provider 只认它。
- 为什么 Codex 与 Pi 的 Base URL 仍会出现在 live 文件里：两者的 CLI 对 `base_url` 都没有环境变量间接引用（4.4 节），这是 CLI 侧硬约束。这是本方案唯一接受的"Base URL 落地"例外，且只落**当前激活**供应商的那一个，其余供应商的 Base URL 仍只在凭据管理器里。在 SECURITY.md 里如实写明。
- Claude 的 `settings.json` 若在 `~/.claude/settings.local.json` 或项目级 `.claude/settings.json` 中另有 `env.ANTHROPIC_*`，会覆盖进程环境。cc-switch 不改这些文件，但冲突检测（5.3.3）要扫描 `~/.claude/settings.local.json` 并给出只读警告。

#### 5.3.2 `EnvSink`：用户级环境变量写入

新建 `src-tauri/src/env_delivery/`：

```rust
pub trait EnvSink: Send + Sync {
    fn set(&self, name: &str, value: &str) -> Result<(), AppError>;
    fn remove(&self, name: &str) -> Result<(), AppError>;
    fn get(&self, name: &str) -> Result<Option<String>, AppError>;   // 只读 HKCU
    fn broadcast(&self) -> Result<(), AppError>;                       // 一批操作后调用一次
}
```

- `WindowsUserEnvSink`：`winreg` 打开 `HKCU\Environment`（`KEY_SET_VALUE | KEY_QUERY_VALUE`），`set_value(name, &value)` 写 `REG_SZ`（**不是** `REG_EXPAND_SZ`，值里可能有 `%`）；`delete_value`；写完后同时 `std::env::set_var / remove_var` 更新本进程环境（让 cc-switch 自己启动的子进程立即继承）。
- `broadcast()`：`SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, 0, "Environment" 的宽字符指针, SMTO_ABORTIFHUNG, 5000, null)`。需要给 `windows-sys` 增加特性 `Win32_UI_WindowsAndMessaging`。为什么必须广播：不广播则 Explorer 不刷新，之后从开始菜单 / 任务栏新开的终端仍看到旧值（4.5 节指出现有代码缺这一步）。
- 名字白名单：`EnvSink::set` 只接受附录 C 列出的名字模式（`ANTHROPIC_*`、`OPENAI_API_KEY`、`CC_SWITCH_*`、以及 Claude `extra_env` 里经敏感规则认定的键）。**任何情况下不得写 `PATH`、`Path`、`PATHEXT`、`ComSpec`、`TEMP`、`TMP`、`USERPROFILE` 等系统语义变量**，白名单之外直接报错。
- `InMemoryEnvSink`：测试用。
- 永不写 `HKLM`（D11）。同名 `HKLM` 变量只在冲突检测里报告。

#### 5.3.3 所有权与冲突检测（替换 `env_checker.rs` / `env_manager.rs`）

- DB `settings` 键 `managed_env_vars`，值为 JSON：`{"ANTHROPIC_AUTH_TOKEN": {"app":"claude","provider":"<id>","set_at":"..."}, ...}`。这是"哪些用户环境变量是 cc-switch 写的"的唯一记录。
- 切换 Claude / Codex 供应商：先 `remove` 该应用在 `managed_env_vars` 里的全部条目，再 `set` 新供应商的，再更新记录，最后 `broadcast()` 一次。Pi 启用 / 移除单个供应商只增删自己的条目。
- 删除供应商：若它在 `managed_env_vars` 里有条目，先 `remove` 再删凭据再删 DB 行。
- 冲突：`set` 前 `get` 现值；若存在、且不在 `managed_env_vars` 里、且与将写入的值不同 → 不写，向前端返回 `EnvConflict { name, owner: "foreign", masked_value }`（值只给末 4 位），由用户在对话框里选择"接管（覆盖）"或"取消切换"。接管后进入 `managed_env_vars`。
- 同名 `HKLM` 变量：只读报告"系统级同名变量存在，用户级会覆盖它（`PATH` 除外）"。
- 删除 `env_manager.rs` 的备份 / 恢复功能与 `env-backup-*.json`：明文备份与本方案冲突；"取消接管"只是从 `managed_env_vars` 移除记录并 `remove` 变量，用户原值不再由我们保管（在接管对话框里明确告知"原值将被覆盖且不保留"）。
- 前端 `EnvWarningBanner.tsx` 改为展示 `masked_value`，永不显示完整值。

#### 5.3.4 内置"打开终端"（`commands/misc.rs:3745-3905`）

- 删除临时 `--settings` 文件路径。
- 改为：从凭据管理器读当前供应商的 `ProviderSecrets`，通过 `Command::env(name, value)` 注入到要启动的终端进程（Windows 上是 `cmd /c start` / `wt` 等已有分支），不经过任何文件、不写日志。
- 这样即使用户没有重开终端，从 cc-switch 里点"打开终端"也一定拿到正确的密钥。

### 5.4 运行时流程

| 操作 | 步骤（顺序即事务边界） | 失败处理 |
|---|---|---|
| 新增供应商 | ① `extract` 表单 JSON → `stripped + secrets`；② 校验（Claude 必须有 api_key；Codex 第三方必须有 api_key + base_url；Pi 必须有 base_url，api_key 可选但为空时前端警告）；③ 写凭据管理器（api_key、base_url、extra_env 各一条）；④ 写 DB 行 | ③ 失败 → 不写 DB，报错；④ 失败 → 删除 ③ 写入的条目（best-effort）后报错 |
| 编辑供应商 | 同上，但 `secrets` 三态合并：`undefined` 不动、`null` 删条目、`string` 覆盖 | 同上 |
| 切换（Claude / Codex） | ① 取 `SwitchLockManager` 锁；② 读新供应商 `secrets`（缺 api_key 直接拒绝："请先补全密钥"）；③ 回填：把当前 live 文件读回、经 `extract` 后只把 `stripped` 部分写入旧供应商 DB 行（**秘密部分丢弃**，因为凭据管理器里已有）；④ 预检 live 写入（沿用 `preflight_codex_live_write` 思路）；⑤ `EnvSink` 移除旧、写新、`broadcast`；⑥ 写 live 文件（`hydrate` 后再按 5.3.1 剥离，Codex 注入 `env_key`+`base_url`）；⑦ 更新 `is_current`、`managed_env_vars` | ⑤ 出现外来冲突 → 返回冲突给前端，不继续；⑥ 失败 → 回滚 ⑤（恢复旧供应商的变量）并报错 |
| Pi 启用 | ① 读 `secrets`；② `EnvSink.set` 该供应商变量；③ `models.json` 写入节点（`apiKey: "$VAR"`）；④ `broadcast` | ③ 失败 → 撤销 ② |
| Pi 移除 | ① 删 `models.json` 节点；② `EnvSink.remove`；③ `broadcast` | — |
| 删除供应商 | ① 若为当前 / 已启用，先执行"取消当前 / Pi 移除"；② 删凭据条目；③ 删 DB 行 | ② 失败仅记 warning，继续 ③（孤儿条目可由"清理孤儿凭据"处理） |
| Pi 列表同步（`pi.rs:221 sync_native_locked`） | 读 `models.json` → 每个节点经 `extract` → `stripped` 写 DB；若节点 `apiKey` 是字面量（用户在 cc-switch 外面手写的），把它写进凭据管理器并**把 `models.json` 里该值改写为 `"$VAR"`** | 改写 `models.json` 失败 → 不写凭据管理器，保留原样并记 warning |
| 清理孤儿凭据 | 设置页按钮：遍历 DB 所有供应商生成期望 target 集合；对附录 B 中每类 field 尝试 `delete` 不在集合内的候选（因 `keyring` 无枚举，候选来自 `managed_env_vars`、上次运行记录的 target 列表 `settings.known_secret_targets`） | — |

- `settings.known_secret_targets`：每次 `SecretStore.set` 成功后把 target 追加进这个 JSON 数组；`delete` 成功后移除。它只含 target 名（不含值），是 `keyring` 不支持枚举的补偿。

### 5.5 导入 / 导出 / 同步 / 备份

- SQL 导出（`database/backup.rs:118 export_sql_string`，`:124 export_sql_string_for_sync`）：DB 已无密钥，导出天然干净。**追加护栏**：导出文本经 `secrets::scan::assert_no_secret_patterns`（正则：`sk-[A-Za-z0-9]{16,}`、`sk-ant-`、`xai-`、`AKIA[0-9A-Z]{16}`、`Bearer [A-Za-z0-9._-]{20,}`、以及本次会话内已知密钥的字面量）命中即拒绝导出并报错。为什么：这是回归护栏，防止未来有人往 DB 里塞回密钥。
- SQL 导入（`backup.rs:141`）与 DB 文件还原（`:1002`）：导入完成后，对 `providers` 表逐行跑 `extract` → 凭据管理器 → 回写 `stripped`（老导出文件里可能带明文密钥）。这一步复用第 6.2 节的凭据迁移函数。
- WebDAV / S3 同步（`services/sync_protocol.rs:149-210`）：载荷不含密钥。**行为变化必须写进文档与 UI**：在另一台机器还原后，所有供应商 `secretStatus.apiKey.present = false`，卡片显示"需要重新输入密钥"徽标，切换时拒绝。
- DB 文件备份（`backup.rs:476-573`）：继续保留，备份现在不含密钥。
- 删除 `env-backup-*.json` 机制（5.3.3）。

---

## 6. 自动迁移方案

### 6.1 触发与顺序（`lib.rs` setup 段，在 DB 初始化之后、`AppState::new` 之后）

```
DB init（含 schema v18→v19，纯 SQL）
 └─ AppState::new（含 SecretStore.probe()）
     └─ if settings.secrets_migration_pending == "1":
          6.2 凭据迁移（providers + AppSettings）
          6.3 明文残留清理
          settings.secrets_migration_pending = "0"
          settings.live_reapply_pending = "1"
 └─ if settings.live_reapply_pending == "1":
          6.4 live 文件重写 + 环境变量投递
          settings.live_reapply_pending = "0"
 └─ 常规启动（live 导入、官方 seed 等）
```

- 为什么拆成两个 pending 标志：6.2 只依赖 DB 与凭据管理器；6.4 依赖 live 文件可写（可能被 CLI 占用）。分开后 6.4 失败不会让 6.2 重跑。
- `Database::init` 现有的"升级前备份"（`database/mod.rs:127-140`）会在 v18→v19 前生成一份含明文的 `.db` 备份。保留这一份（回滚用），但把它**改名为 `pre-secrets-migration-<ts>.db`** 并在 6.3 中登记，供一次性提示（6.5）。

### 6.2 凭据迁移 v1（`src-tauri/src/secrets/migration.rs`）

1. `probe()` 失败 → 直接进入 6.6 阻断，DB 不动。
2. 读取全部 `providers` 行（此时只剩 claude/codex/pi）。对每行：`extract(app, id, meta, settings_config)`。收集 `(row_id, stripped, secrets)`。**此阶段不写任何东西**，先全部提取，任何一行解析失败就整批中止并报错（报错内容只含 provider id 与字段名）。
3. 逐条写凭据管理器：`api_key`、`base_url`、`extra_env.*`。任一写入失败 → 中止，**不写 DB**，已写的条目保留（幂等，下次覆盖）。
4. 单个 SQLite 事务：`UPDATE providers SET settings_config = ?stripped WHERE id = ?`（全部行）+ 更新 `known_secret_targets`。提交。
5. `AppSettings`：读 `settings.json`，把 `webdav_sync.password`、`s3_sync.*` 写入凭据管理器，去掉字段后原子重写文件。
6. 记录 `secrets_migration_report`（JSON，含每个 provider 的迁移字段名、被丢弃的 OAuth `tokens` 是否存在、Pi 模型级 baseUrl warning），供 6.5 的提示使用。**报告不含值。**
7. 幂等性：重跑时 `extract` 对已剥离的 JSON 返回空 `secrets`，第 3、4 步对空集合是 no-op。

### 6.3 明文残留清理

自动删除（这些文件已被 DB 取代，删除不损失功能）：

- `~/.cc-switch/config.json`、`config.json.bak`、`config.json.migrated`
- `~/.cc-switch/backups/env-backup-*.json`
- `~/.cc-switch/codex_oauth_auth.json`、`copilot_auth.json`、`xai_oauth_auth.json`（对应功能已删除）
- `%TEMP%/claude_*_*.json`（旧终端启动器残留，按文件名模式匹配且内容能解析为含 `env` 的 JSON 才删）

不自动删除、只列出并提供一键删除（6.5）：

- `~/.cc-switch/backups/*.db`（含刚改名的 `pre-secrets-migration-*.db`）——它们是用户的备份，且是唯一的回滚路径。

### 6.4 live 文件重写与首次投递

对 Claude 当前供应商、Codex 当前供应商、所有在 `models.json` 里存在的 Pi 供应商，执行第 5.4 节"切换 / Pi 启用"的 ⑤⑥ 步（不改 `is_current`）。额外的迁移专属处理：

- Claude `settings.json`：若 `env.ANTHROPIC_BASE_URL` 是 `http://127.0.0.1:<port>` 或 token 是 `PROXY_MANAGED`（代理接管残留，`services/proxy.rs:25`），视为无效值，直接用凭据管理器里的值覆盖并清理。
- Codex `config.toml`：删除所有 `experimental_bearer_token`；对当前激活表写 `env_key`；删除 `codex_config.rs:3519-3579` 那类官方代理路由表（`apply_codex_official_proxy_route` 写的）。`~/.codex/auth.json`：若存在且**只**含 `OPENAI_API_KEY`（无 `tokens`），且该值等于迁移进凭据管理器的某个 key → 删除该文件；否则不动并写入报告。
- Pi `models.json`：所有字面量 `apiKey` → `"$VAR"`；敏感 header 同理。
- 任一文件写失败：记录到报告，`live_reapply_pending` 保持 `1`，下次启动重试；前端显示"有 N 个 live 配置未完成重写，原因：文件被占用"。

### 6.5 一次性提示（前端）

启动后若 `secrets_migration_report` 存在且未确认，弹一个对话框（沿用 `FirstRunNoticeDialog` 的样式），内容：

- 已迁移 N 个供应商的密钥到 Windows 凭据管理器（列出名字，不列值）。
- 密钥现在通过用户环境变量投递，**已打开的终端需要重开**。
- 列出仍含明文的历史备份文件路径，提供"全部删除"与"稍后"。
- 若报告里有"丢弃了 Codex OAuth 登录态"，提示用户改用 `codex login`。
- 若有 live 重写失败项，列出并提供"重试"。

### 6.6 迁移失败的阻断式处理

复用 `show_database_init_error_dialog`（`lib.rs:600`）的模式：凭据管理器探针失败或 6.2 第 3 步失败时，弹系统对话框"无法访问 Windows 凭据管理器：<错误>。重试 / 退出"。**不提供"跳过"**——跳过等于继续明文运行，违反原则 3.1-4。

### 6.7 回滚

- 回滚到旧版本：用 `pre-secrets-migration-*.db` 覆盖 `cc-switch.db`，旧版本可正常读取（它是 v18）。凭据管理器里多出来的条目无害，可在"凭据管理器"控制面板按 `cc-switch/` 前缀手动清理。
- 这也是为什么 6.3 不自动删这份备份。

---

## 7. 删除面

原则：整目录 / 整文件删除优先；部分删除按下面列出的锚点。每个小节末尾是"必须搬迁而不是删除"的清单。

### 7.1 本地路由（代理）及附属

**整删（Rust）**：`src-tauri/src/proxy/**`（74 文件）、`services/proxy.rs`、`commands/proxy.rs`、`commands/failover.rs`、`commands/stream_check.rs`、`commands/usage.rs`、`services/stream_check.rs`、`services/usage_stats.rs`、`services/model_pricing.rs`、`services/session_usage*.rs`（6 个）、`database/dao/proxy.rs`、`dao/failover.rs`、`dao/stream_check.rs`、`dao/usage_rollup.rs`、`usage_events.rs`、`model_capabilities.rs`（保留其中被 `codex_config.rs:9,1470` 使用的 `image_input_capability_from_modalities` / `ImageInputCapability`，搬进 `codex_config.rs`）。D4 生效时另加：`usage_script.rs`、`services/provider/usage.rs`、`services/usage_cache.rs`、`services/balance.rs`、`services/coding_plan.rs`、`services/subscription.rs`、`services/subscription_grok.rs`、对应 `commands/*.rs`。

**部分删（Rust，按锚点）**：

- `lib.rs`：`mod proxy` L32、`mod usage_events` L39、`mod usage_script` L40、`pub use services::{… ProxyService …}` L67、L520、L655、L1173-1202（全局代理客户端初始化改为调用搬迁后的 `services::http_client::init`）、L1204-1227、L1243、L1266-1316、L1882-1917、L1947-1997、测试 L2284 与 L2396-2411；`invoke_handler` 中 L1409-1414、L1436-1437、L1556-1614、L1661-1666。
- `commands/mod.rs`、`services/mod.rs`、`database/mod.rs:39-43`、`database/dao/mod.rs` 对应 `mod` / `pub use` 行。
- `commands/settings.rs:625-715`（rectifier / optimizer 命令）；`dao/settings.rs:180-311`。
- `database/backup.rs:87-104` 表清单、`:457-462` `rollup_and_prune`；`database/mod.rs:146-157`（定价 seed、日志清理、rollup 调用）。
- `provider.rs`：`in_failover_queue`、`custom_user_agent`（`:572-600`，随之删 `http` 依赖）。
- `settings.rs`：5.2.5 列出的字段；`app_config.rs:424-429 supports_local_proxy`；`error.rs:63,65`。
- `services/provider/mod.rs`：`:56-59`、`:84`、`:4880-5014`（托管 Codex 接管事务）、`:5216-5271` 收敛为取锁 + `switch_normal`；`live.rs:780-815`、`:1513-1673`、`:1849-1855`。
- `services/profile.rs:368` 及 `ProfileService::apply` 的 `should_stop_proxy` 返回值（`commands/profile.rs:173-190`、`tray.rs:486-500`）。
- `tray.rs`：`:493, 558-720, 768, 815`（Auto / 故障转移菜单）、D4 下的用量渲染 `:372-406, 796, 998, 1135-1250`。
- `commands/config.rs:75`、`commands/provider.rs:221`（`claude_desktop_config::get_status(db, proxy_running)`，随 Claude Desktop 一起删）。
- `codex_config.rs`：`:3253 neutralize_codex_official_auth_fallback_for_proxy_oauth`、`:3519 apply_codex_official_proxy_route`、`:3561`、`:3579`。

**必须搬迁**：

| 原位置 | 新位置 | 说明 |
|---|---|---|
| `proxy/http_client.rs` | `services/http_client.rs` | 删掉 `set_proxy_port` / `CC_SWITCH_PROXY_PORT`（L20-25，本机端口绕过）；保留上游代理 URL 支持（D10）并在 `dao/settings.rs:147-158` 的 setter 里拒绝含 userinfo 的 URL |
| `proxy/switch_lock.rs` | `services/switch_lock.rs` | `AppState` 直接持有 `switch_locks: SwitchLockManager`；调用点 `services/provider/pi.rs`、`pi_state.rs:21`、`mod.rs:4592,4707,5218` 改为 `state.switch_locks.lock(app)` |
| `proxy/types.rs::LogConfig` | `settings.rs` | 使用点 `dao/settings.rs:313-322`、`commands/settings.rs:699-707`、`lib.rs:613`、`commands/sync_support.rs:24` |
| `proxy/providers/codex.rs::is_codex_official_provider`、`resolve_codex_catalog_tool_profile` | `codex_config.rs` | 去掉对 `ProxyError` 的依赖 |
| `proxy/providers/codex_oauth_auth.rs` | **不搬**（D3 移除） | 若 D3 改为保留，则搬到 `src-tauri/src/codex_oauth_auth.rs` 并把 token 迁入凭据管理器 |

**Cargo 依赖删除**（已逐个 grep 验证仅代理使用）：`axum`、`tower`、`tower-http`、`hyper`、`hyper-util`、`hyper-rustls`、`http`、`http-body`、`http-body-util`、`httparse`、`tokio-rustls`、`rustls`（删掉 `lib.rs:451` 的 `install_default()` 后验证 reqwest 的 `rustls-tls` 自带 provider 能正常工作，跑一次真实 HTTPS 请求测试）、`webpki-roots`、`rustls-native-certs`、`brotli`、`zstd`、`flate2`、`async-stream`、`bytes`、`rust_decimal`、`rquickjs`（含 aarch64 的 bindgen 块，D4）。保留：`reqwest`、`tokio`、`futures`、`uuid`、`sha2`、`hmac`（S3 签名）、`base64`、`regex`、`url`、`once_cell`、`tempfile`、`indexmap`、`toml_edit`、`serde_yaml`（Skills）、`json5`（Pi）。

**前端整删**：`src/components/proxy/*`、`components/usage/*`、`components/settings/ProxyTabContent.tsx`、`GlobalProxySettings.tsx`（D10 下改为只有 URL 输入、无用户名密码）、`RectifierConfigPanel.tsx`、`components/providers/ProviderHealthBadge.tsx`、`FailoverPriorityBadge.tsx`、`forms/LocalProxyRequestOverridesField.tsx`、`forms/CustomUserAgentField.tsx`、`forms/EndpointSpeedTest.tsx`（D9）、`hooks/useProxyStatus.ts`、`useGlobalProxy.ts`（D10 下保留精简版）、`useStreamCheck.ts`、`useUsageEventBridge.ts`、`useUsageCacheBridge.ts`（D4）、`lib/api/{proxy,failover,globalProxy,connectivity-check,usage,subscription}.ts`、`lib/query/{proxy,failover,usage,subscription}.ts`、`types/{proxy,usage,subscription}.ts`、`lib/{requestOverrides,modelsDevAutoSync,modelsDevPricing,usageRange,userAgent}.ts`、`utils/usageDisplay.ts`、`config/userAgentPresets.ts`、`config/codingPlanProviders.ts`（D4）、`components/{UsageFooter,UsageScriptModal,SubscriptionQuotaFooter,CodexOauthQuotaFooter,CodexOauthAccountQuota,CopilotQuotaFooter,XaiOauthQuotaFooter}.tsx`。

**前端部分删（锚点）**：`main.tsx:22-25, 135-152`；`App.tsx:34, 47-48, 74-77, 260, 279, 448-450, 1326, 1351-1367, 1379-1386`；`settings/SettingsPage.tsx:49-52, 231, 238, 295-302, 460-482, 513-516`；`ProviderCard.tsx:27-28, 39, 73-76, 190-193, 235, 356-374, 487-497`；`ProviderList.tsx:37-41, 152-163`；`hooks/useProviderActions.ts:196-270`；`utils/providerCapabilities.ts`；`lib/api/index.ts`；`lib/query/index.ts:4`；`types.ts:169-181, 229-232, 374-394`；`config/appConfig.tsx:54-69`；`forms/ProviderForm.tsx`、`ClaudeFormFields.tsx`、`CodexFormFields.tsx`、`CodexConfigEditor.tsx`、`CodexConfigSections.tsx`、`EditProviderDialog.tsx`、`ProviderActions.tsx` 中的接管 / 代理分支。

**i18n 键（以 `en.json` 为锚，四份同步）**：`failover` 2776、`proxy` 2791-2973、`streamCheck` 2974、`proxyConfig` 2988-3003、`usage` 1670-1880、`health` 2769、`settings.tabProxy` 349、`settings.proxy` 363-376、`settings.failover` 377、`settings.globalProxy` 385/876（D10 下精简）、`settings.rectifier` 401、`settings.optimizer` 417-423、`notifications.proxyRequiredForSwitch` / `proxyReason*` / `officialBlockedByProxy` / `proxyOfficialWarning` 276-297、`subscription` 3242-3264（D4）。

**测试**：删 `tests/components/{ProxyTabContent.apps,ProxyToggle,RoutingActivationBrand,GlobalProxySettings,RequestLogTable,UsageDashboard,UsageTrendChart,usageFormat,PricingEditModal,ModelsDevAutoSyncPanel,ModelsDevPickerDialog}.test.*`、`tests/hooks/useProxyStatus.test.tsx`、`tests/lib/{modelsDevAutoSync,requestOverrides,keepLastGoodUsage}.test.ts`、`tests/types/usage.test.ts`、`tests/utils/usageDisplay.test.ts`、`src-tauri/tests/proxy_commands.rs`；改 `tests/msw/handlers.ts:329-400`、`tests/integration/App.test.tsx`、`database/tests.rs` 中 34 处代理相关断言。`package.json` 中 `recharts` 删除前 grep 确认无其他使用。

### 7.2 更新功能

- `Cargo.toml:34 tauri-plugin-updater`；`tauri.conf.json:39 createUpdaterArtifacts`、`:62-68 plugins.updater`；`capabilities/default.json:9 updater:default`。
- `lib.rs:505-515` 插件注册；`invoke_handler` L1417-1420 中 `install_update_and_restart`、`check_app_update_available`、`check_for_updates`（`restart_app` 保留，配置目录变更后要用）；`restart_process` L2275-2279、`destroy_single_instance_lock` L2260-2263 删除。
- `commands/settings.rs:4` `UpdaterExt` 导入、`:192-285` 两个命令与 `UpdateDownloadProgress`；`commands/misc.rs:53-65 check_for_updates`（只是打开 GitHub 页面）连同 About 页按钮一起删。
- `package.json:74 @tauri-apps/plugin-updater`；前端 `lib/updater.ts`、`contexts/UpdateContext.tsx`、`components/UpdateBadge.tsx`；`main.tsx:5, 125-128`；`App.tsx:72, 1345-1350`；`settings/AboutSection.tsx:34, 248-249, 441-514, 943-990`；`lib/api/settings.ts:56-62`；`components/DatabaseUpgrade.tsx:65-66, 102`（"数据库版本过新"恢复页改为只给 GitHub Releases 链接）。
- i18n：`settings.checkForUpdates` 808、`updateTo` 809、`updateAvailable` 815、`updateBadge` 816、`updateFailed` 817、`checkUpdateFailed` 818、`dbUpgrade` 1969-1985 中与更新相关的键。
- 发布链路：`.github/workflows/release.yml:125-179`（签名密钥准备）、`:287-307`、`:444-461`、`:515-523`、`:547-552`、`:620`、job `assemble-latest-json` `:628-731`；`.github/workflows/sync-r2.yml` 整删；`scripts/rewrite-updater-manifest.mjs` 整删；`scripts/generate-download-manifest.mjs:48` 注释。D1 下 release.yml 只保留 Windows 构建 job，`flatpak/` 目录整删。
- `tauri-plugin-process` **保留**（前端 `exit()` 用，`main.tsx:16,75`、`DatabaseUpgrade.tsx:5,292`）；`process:allow-restart` 能力保留给 `restart_app`。

### 7.3 非目标应用与附属功能

`AppType`（`app_config.rs:380-395`）只保留 `Claude`、`Codex`、`Pi`。删除变体后 `cargo build` 会在每个 `match` 处报错，这是清单的自动校验；下面是已知的密集区，按文件处理：

| 变体 | 密集文件（引用数） |
|---|---|
| `Gemini` | `app_config.rs`(19)、`services/provider/live.rs`(6)、`services/provider/mod.rs`(6)、`services/mcp.rs`(3)、`commands/config.rs`(3)、`services/config.rs`(3)、`settings.rs`(3)、`provider.rs`(3) |
| `GrokBuild` | `services/provider/live.rs`(11)、`app_config.rs`(17)、`commands/provider.rs`(4)、`services/provider/mod.rs`(4)、`dao/providers.rs`(3) |
| `OpenCode` | **`services/provider/mod.rs`(39)**、`app_config.rs`(17)、`live.rs`(7) |
| `OpenClaw` | `services/provider/mod.rs`(23)、`app_config.rs`(14)、`live.rs`(7) |
| `Hermes` | `app_config.rs`(16)、`services/provider/mod.rs`(10)、`live.rs`(7) |
| `ClaudeDesktop` | `app_config.rs`(15)、`dao/providers.rs`(7)、`services/profile.rs`(6)、`services/skill.rs`(6)、`live.rs`(7)、`mod.rs`(7) |

**整删（Rust）**：`gemini_config.rs`、`gemini_mcp.rs`、`grok_config.rs`、`opencode_config.rs`、`openclaw_config.rs`、`hermes_config.rs`、`claude_desktop_config.rs`、`services/omo.rs`、`services/provider/gemini_auth.rs`、`commands/{omo,openclaw,workspace,hermes,copilot,xai_oauth,codex_oauth,auth,coding_plan,balance,subscription}.rs`（后五个按 D3 / D4）、`services/codex_oauth_models.rs`（D3）、`mcp/{gemini,grokbuild,hermes,opencode}.rs`、`session_manager/providers/{gemini,grokbuild,hermes,openclaw,opencode}.rs`、`database/dao/universal_providers.rs`（D6）、`services/speedtest.rs`（D9）、`deeplink/**` 与 `commands/deeplink.rs`（D5）、`src-tauri/tests/{hermes_roundtrip,deeplink_import}.rs`。

**部分删（Rust 锚点）**：`app_config.rs:360-370, 398-470, 474-577`；`commands/config.rs:70-189`；`services/config.rs:88-263`；`services/mcp.rs:32-47, 113-185, 232, 378-526`；`services/skill.rs:577-626, 685, 2242, 2429, 2466`；`prompt_files.rs:13-40`；`provider.rs:176-236, 620-920`（Universal）；`settings.rs:29-50 VisibleApps, 425-433, 927-959`；`tray.rs:179-190, 1451, 1537-1796`；`lib.rs:53-60, 846-924, 946-1004, 1229-1238, 2056-2060`，以及 deep-link 注册 `:408, 1013-1075`；`session_manager/mod.rs:58-95`；`mcp/mod.rs:16-39`；`database/dao/providers_seed.rs:46, 66, 76` 与 `database/mod.rs:35-38` 的 seed id 常量；`commands/provider.rs::ensure_grokbuild_official_provider`；`services/provider/mod.rs:5196-5206, 5288-5300`（OMO 互斥）、`:6898-6960`（Universal）；`lib.rs:1232 scrub_leaked_gemini_common_config`；`services/env_checker.rs:41-55` 关键词表只留 claude / codex（并按 5.3.3 重写）。`Cargo.toml`：删 `tauri-plugin-deep-link`（D5）、`json-five`（仅 openclaw / omo 使用）；`tauri.conf.json` 删 `plugins.deep-link`。

**前端整删**：`config/{gemini,grokBuild,hermes,openclaw,opencode,claudeDesktop,universal}ProviderPresets.ts`（含 `.test.ts`）、`forms/{Gemini*,GrokBuildProviderForm,XaiOAuthSection,HermesFormFields,OpenClawFormFields,OpenCodeFormFields,OmoFormFields,ClaudeDesktopProviderForm,CopilotAuthSection,CodexOAuthSection}.tsx`、`forms/hooks/{useGeminiCommonConfig,useGeminiConfigState,useXaiOauth,useOpencodeFormState,useOmoDraftState,useOmoModelSource,useOpenclawFormState,useHermesFormState,useCopilotAuth,useCodexOauth,useManagedAuth}.ts`、`forms/helpers/opencodeFormUtils.ts`、`utils/grokBuildConfig.ts`（+test）、`components/{openclaw,workspace,hermes,universal,deeplink}/*`、`components/DeepLinkImportDialog.tsx`、`utils/{deeplinkRisk,deepLinkConfigPreview}.ts`、`hooks/{useOpenClaw,useHermes}.ts`、`lib/api/{omo,openclaw,workspace,hermes,copilot,auth,deeplink}.ts`、`lib/query/{omo,copilot}.ts`、`types/omo.ts`、`lib/authBinding.ts`、`settings/{AuthCenterPanel,CodexAuthSettings}.tsx`、`BrandIcons.tsx` 中 `GeminiIcon` / `OpenClawIcon`、`deplink.html`（仓库根）。

**前端部分删（锚点）**：`config/appConfig.tsx:19-196` 全部表只留三项；`lib/api/types.ts:3-12 AppId`；`types.ts:291-300, 413-421, 509-515, 558, 584`；`components/AppSwitcher.tsx:19-50`；`settings/AppVisibilitySettings.tsx:24-33`；`settings/DirectorySettings.tsx` + `hooks/useDirectorySettings.ts`；`App.tsx:44-46, 99-109, 126-130, 161-165, 181-182, 302-309, 690, 1031, 1082-1088, 1316-1321, 1630, 1812`；`forms/ProviderForm.tsx`（opencode 82 / openclaw 72 / hermes 71 / copilot 37 / xai_oauth 11 处）、`ClaudeFormFields.tsx`（copilot 46 / xai 16）、`ClaudeDesktopProviderForm.tsx`、`CodexFormFields.tsx`（xai 4）、`ProviderList.tsx:110-150`、`AddProviderDialog.tsx`、`EditProviderDialog.tsx`、`ProviderPresetSelector.tsx`、`hooks/useProviderCategory.ts`、`useApiKeyLink.ts`、`mcp/McpFormModal.tsx`、`skills/UnifiedSkillsPanel.tsx`、`sessions/SessionManagerPage.tsx`、`lib/query/mutations.ts`、`lib/api/providers.ts:99-119, 162-205`、`lib/api/skills.ts`、`config/constants.ts`（`PROVIDER_TYPES.XAI_OAUTH` / copilot）、`claudeProviderPresets.ts` / `codexProviderPresets.ts` 中 copilot / xai_oauth 条目、`components/profiles/scope.ts`、`lib/api/profiles.ts`、`icons/extracted/metadata.ts`。

**i18n 键**：`apps` 906-918 收敛、`geminiConfig` 1587-1607、`grokBuild` 1063-1075、`xaiOauth` 1440-1460、`copilot` 1360-1398、`codexOauth` 1399-1439、`managedAuth` 1461、`opencode` 1608-1656、`omo` 3052-3212、`workspace` 2241-2281、`openclaw` 2282-2384、`openclawConfig` 3213-3241、`hermes` 2385-2445、`claudeDesktop` 202-246、`settings.{gemini,grok,opencode,openclaw,hermes}ConfigDir*` 786-795、`browsePlaceholder{Gemini,Grok,Opencode,Openclaw,Hermes}` 800-804、`settings.advanced.{gemini,opencode}Desc` 722-723、`settings.authCenter.*` 354、`notifications.{grokBuildRestartRequired,claudeDesktopProxyRestartRequired,openclaw*,proxyReasonCopilot,copilotProxyHint,proxyReasonClaudeDesktop}`、`deeplink.*`（D5）。

**测试**：`tests/components/{AddProviderDialog,DeepLinkImportDialog,EditProviderDialog,McpFormModal,ProviderActions,ProviderList,UnifiedMcpPanel,UnifiedSkillsPanel,GrokBuildProviderForm,XaiOAuthSection,xaiOauthLocales,xaiOauthProviderPresets,grokBuildConfig,OpenCodeFormFields,OmoFormFields.*,OpenClawFormFields,OpenClawProviderActions,openclaw.utils,HermesFormFields,ClaudeDesktopProviderForm}.test.*`、`tests/config/{appConfig,opencodeProviderPresets,therouterOpenCodeOpenClawPresets,codingPlanProviders,omoConfig}.test.*`、`tests/hooks/{useAddProviderMutation,useDirectorySettings,useImportSkillsFromApps,useProviderActions,useSettings,useUpdateProviderMutation,useOpencodeFormState,useManagedAuth}.test.tsx`、`tests/integration/{App,SettingsDialog}.test.tsx`、`tests/msw/{handlers.ts:74-86, state.ts}`。

---

## 8. 安全加固清单（在删除与改造之外必须做的）

| # | 项 | 做法 | 为什么 |
|---|---|---|---|
| S1 | IPC 零密钥 | 全部 `#[tauri::command]` 返回类型 grep `api_key / apiKey / password / secret / token`，逐个确认不返回值。`get_providers`、`get_settings`、`read_live_settings`、`check_env_conflicts` 是重点 | 原则 3.1-3 |
| S2 | 日志 | `ProviderSecrets` 自定义 `Debug`；`lib.rs:137 redact_known_secrets` 的最小长度 8 改为 6 并加入 `CC_SWITCH_*` 变量值；新增测试：用 `InMemorySecretStore` 里的固定密钥跑一遍新增 / 切换 / 迁移，再断言日志缓冲区不含该密钥 | 原则 3.1-8 |
| S3 | 内存 | `Zeroizing<String>` 贯穿 `ProviderSecrets`、`SecretStore` 返回值、`EnvSink.set` 参数 | 降低崩溃转储 / 内存扫描暴露 |
| S4 | 导出护栏 | 5.5 节 `assert_no_secret_patterns`，对 SQL 导出、同步载荷、DB 文件备份（备份前对 `providers.settings_config` 与 `settings` 表跑一次扫描）都启用 | 回归防线 |
| S5 | live 写入护栏 | 5.3.1 节 `assert_no_secret_keys`：Claude `settings.json` 写入前确认 `env` 里没有敏感键；Codex TOML 写入前确认没有 `experimental_bearer_token`；Pi 节点写入前确认 `apiKey` 以 `$` 开头 | 防止未来改动把值写回文件 |
| S6 | CSP 收紧 | `tauri.conf.json` `connect-src` 从 `https: http:` 收敛为 `'self' ipc: http://ipc.localhost`（代理与定价同步删除后，前端不再需要直连外网；Skills / 同步都走后端）。`img-src` 保留 `https:`（供应商头像） | 缩小 WebView 出网面 |
| S7 | 能力最小化 | `capabilities/default.json` 删 `updater:default`；逐项确认其余 `core:window:*` 仍被使用 | 最小权限 |
| S8 | `SECURITY.md` 更新 | 删除代理监听相关威胁模型段落；新增"凭据存储：Windows 凭据管理器；投递：用户级环境变量（值在 `HKCU\Environment` 明文，仅当前激活供应商）；Codex / Pi 的 Base URL 落地例外"；删除 deeplink 段落（D5） | 如实描述边界 |
| S9 | 单实例参数 | D5 之后 `tauri_plugin_single_instance` 的回调不再处理 URL 参数，只做窗口聚焦 | 关闭外部输入通道 |
| S10 | `global_proxy_url` | setter 拒绝含 userinfo 的 URL；前端去掉用户名 / 密码输入框 | D10 |
| S11 | 冲突值遮罩 | `EnvConflict` 只带 `masked_value`（末 4 位） | 4.5 节现状是明文回传 |
| S12 | 文件权限 | 保留 `atomic_write_private`，但因文件不再含密钥，不额外做 Windows ACL | 不做无收益的复杂度 |

---

## 9. 施工阶段与验收

每阶段一个（或一组）提交；阶段结束必须全绿：`pnpm typecheck && pnpm format:check && pnpm test:unit`、`cargo fmt --check && cargo clippy -- -D warnings && cargo test`。

### Phase 0：基线与护栏（半天）

1. 打 tag `pre-slimdown-baseline`。
2. 造测试夹具：`src-tauri/tests/fixtures/v18-plaintext.db`——用当前版本生成一个含 3 个 Claude、2 个 Codex（1 官方 1 第三方）、2 个 Pi 供应商的 v18 库，密钥用固定可识别字面量（如 `sk-fixture-claude-0001`），`meta.usage_script` 也塞一个。另造对应的 `settings.json`（含 WebDAV 密码）、`~/.claude/settings.json`、`~/.codex/config.toml`（含 `experimental_bearer_token`）、`~/.pi/agent/models.json`（含字面量 `apiKey`）。
3. 写 `scripts/secret-scan.ps1`：对给定目录递归 grep 夹具密钥字面量；后续每阶段验收都跑它扫描测试 home 目录。
4. 验收：CI 绿；夹具能被当前版本正常加载。

### Phase 1：删除（3 ～ 4 天，三个子阶段各自可提交）

- **1a 本地路由 + 用量家族 + Codex OAuth + Copilot / xAI**（7.1 全部 + 7.3 中 D3/D4 部分）。先做搬迁（`http_client`、`switch_lock`、`LogConfig`、两个 Codex 函数），再整删目录，最后按编译错误清部分删除点。
- **1b 非目标应用**（7.3 其余）。先删 `AppType` 变体，跟着编译错误走。
- **1c 更新器 + deeplink + Universal + 端点测速 + 依赖与配置清理**（7.2、D5、D6、D9、Cargo / package / tauri.conf / capabilities / workflows）。
- 验收：应用可启动；Claude / Codex / Pi 三个页签正常；新增 / 编辑 / 切换 / 删除供应商行为与基线一致（此时仍是明文，属预期）；`Cargo.lock` 中不再出现 axum / hyper / rquickjs 等；`cargo tree` 无 `tauri-plugin-updater`。

### Phase 2：SecretStore 与数据模型（2 天）

- 5.1、5.2.1、5.2.2、5.2.3、5.2.5。`SCHEMA_VERSION` 暂不动（v19 放到 Phase 4，避免中间状态的库无法回退）。
- 此阶段结束时：新增 / 编辑走凭据管理器；DB 不再写入密钥；切换仍按旧方式把 `hydrate` 后的完整 JSON 写 live（临时，Phase 3 替换）。
- 验收：用 `InMemorySecretStore` 的单测覆盖 `extract / hydrate` 三个应用的全部规则（含 Pi `$VAR` 不提取、Codex OAuth `tokens` 丢弃、`api_key_field` 二选一）；真实后端集成测试 `secrets_windows_roundtrip`（`#[ignore]`，CI 上单独一个 step 用 `--ignored` 跑，失败则记录并允许失败）；`secret-scan.ps1` 扫 DB 文件为 0 命中。

### Phase 3：环境变量投递与 live 重写（2 ～ 3 天）

- 5.3 全部、5.4 全部、5.3.4 终端启动器。
- 验收：切换 Claude 后 `HKCU\Environment` 出现对应变量且 `settings.json` 无敏感键；新开 PowerShell `$env:ANTHROPIC_AUTH_TOKEN` 可见；`claude` 能正常发请求。Codex 第三方切换后 `config.toml` 有 `env_key` 无 bearer，`codex` 正常；Pi 启用后 `models.json` 里 `apiKey` 为 `$CC_SWITCH_PI_*`，`pi /model` 里模型可用。外来同名变量触发冲突对话框。`secret-scan.ps1` 扫 `~/.claude`、`~/.codex/config.toml`、`~/.pi/agent/models.json` 为 0 命中（`~/.codex/auth.json` 若有用户自己的登录态不在扫描范围）。

### Phase 4：自动迁移（2 天）

- 5.2.4 schema v19、第 6 节全部。
- 验收：用 Phase 0 夹具冷启动 → 无交互完成迁移 → `secret-scan.ps1` 扫描整个测试 home（除 `pre-secrets-migration-*.db`）为 0 命中；一次性提示内容正确；点击"全部删除"后备份消失；用旧版本 + `pre-secrets-migration-*.db` 能回滚启动；反复启动不重复迁移（`secrets_migration_pending = 0`）；人为把 `models.json` 设为只读再启动 → `live_reapply_pending` 保持 1 并提示，恢复可写后下次启动自动补完。

### Phase 5：安全加固（1 天）

- 第 8 节 S1 ～ S11。
- 验收：S2 的日志断言测试通过；S4 的导出护栏测试（往内存库里手工塞一条含 `sk-` 的行，导出必须报错）通过；CSP 收紧后 Skills 安装、WebDAV 同步、头像加载正常。

### Phase 6：文档、CI、发布、收尾（1 天）

- README（四语）删除代理 / 多应用 / 更新相关段落，新增"凭据存储与投递"一节；`docs/guides/*routing*`、`proxy-guide-zh.md`、`docs/user-manual/*/4-proxy` 删除；`SECURITY.md`（S8）；`CONTRIBUTING.md` 的命令表更新。
- `.github/workflows/ci.yml` 后端矩阵改为 `windows-latest`（保留 `backend-windows-wsl2`）；`release.yml` 只保留 Windows；删 `sync-r2.yml`、`flatpak/`。
- i18n 四份 locale 跑 `localeCoverage.test.ts`。
- 最终验收：从 3.20.3 安装包升级到本版本的真实机器测试（用户自己的机器），走一遍 6.5 的提示流程。

---

## 10. 测试策略

- **单元（Rust）**：`secrets::extractor` 规则表驱动测试；`secrets::migration` 用内存库 + `InMemorySecretStore`；`env_delivery` 用 `InMemoryEnvSink` 测所有权与冲突逻辑；`codex_config` 的 `env_key` 注入 / 剥离；`pi_config` 的 `$VAR` 改写。
- **单元（前端）**：`ApiKeyInput` 在 `secretStatus.apiKey.present = true` 时不回显且显示 hint；表单提交时 `secrets` 三态正确；`EnvWarningBanner` 只显示遮罩值。
- **集成（Rust，`#[ignore]`）**：`secrets_windows_roundtrip`（真实凭据管理器）、`env_sink_windows_roundtrip`（真实 `HKCU`，使用 `CC_SWITCH_TEST_*` 名字并在 `Drop` 中清理）。
- **端到端（人工，Phase 3 / 4 / 6 验收）**：真实 Claude Code、Codex 0.153.x、Pi 0.85.x。
- **回归护栏（持续）**：`secret-scan.ps1` 进 CI 的 Windows job，在 `cargo test` 后扫描 `CC_SWITCH_TEST_HOME`。

---

## 11. 风险与应对

| 风险 | 应对 |
|---|---|
| 用户已打开的终端看不到新变量 | 5.3.2 广播 + 6.5 / 切换后 toast 明确提示"需重开终端"；内置"打开终端"总是正确（5.3.4） |
| 用户在别处（`settings.local.json`、项目级设置、shell profile）另有 `ANTHROPIC_*` 覆盖 | 5.3.3 只读扫描并提示；不代改 |
| Codex 未来版本改变 `env_key` 语义 | `codex_config.rs` 已有版本注释习惯，保持；Phase 3 验收记录测试时的 Codex 版本 |
| `keyring` 的 `CRED_PERSIST_ENTERPRISE` 让凭据随漫游配置漫游 | 记入 SECURITY.md；域环境用户可自行在凭据管理器改为本机持久化 |
| 迁移中途断电 | 6.2 的"先全写凭据、再单事务改 DB"保证要么旧明文完整、要么新状态完整；6.4 独立标志可重试 |
| 用户想回旧版 | 6.7 |
| 删除面过大导致隐藏依赖漏网 | 原则 3.2-3：不允许 `allow(dead_code)`，让 clippy 与编译器把漏网点顶出来；Phase 1 分三次提交 |
| 同步到另一台机器后没有密钥 | 5.5 的徽标与拒绝切换；文档明确"密钥不随同步走，这是设计" |

---

## 附录 A：新增 / 保留 / 删除的 Tauri 命令一览

新增：`get_provider_secret_status`（可并入 `get_providers`）、`set_provider_secrets`（可并入 `update_provider`）、`env_delivery_conflicts`、`env_delivery_adopt`（接管外来变量）、`secrets_cleanup_orphans`、`secrets_migration_report_get / _confirm`、`plaintext_backups_list / _delete`、`settings_secret_set`（WebDAV / S3）。

保留（改造）：`get_providers`、`add_provider`、`update_provider`、`delete_provider`、`switch_provider`、`read_live_settings`、`get_settings` / `save_settings`、`export_config_to_file` / `import_config_from_file`、`webdav_*` / `s3_*`、`open_provider_terminal`、`restart_app`、Pi 的 `pi_enable_provider` / `pi_remove_provider` 等。

删除：`invoke_handler` 中 7.1 / 7.2 / 7.3 列出的全部条目，以及 `check_env_conflicts` / `delete_env_vars` / `restore_env_backup`（被新的 `env_delivery_*` 取代）。

## 附录 B：Credential Manager target 命名

```
cc-switch/v1/provider/<app>/<provider_id>/api_key
cc-switch/v1/provider/<app>/<provider_id>/base_url
cc-switch/v1/provider/<app>/<provider_id>/env/<VAR_NAME>      # Claude extra_env、Pi 敏感 header
cc-switch/v1/app/webdav/password
cc-switch/v1/app/s3/access_key_id
cc-switch/v1/app/s3/secret_access_key
cc-switch/v1/probe                                              # 启动自检，写后即删
```

- `<app>` ∈ `claude | codex | pi`；`<provider_id>` 为 DB 主键原样；`<VAR_NAME>` 为大写环境变量名。
- `service` 固定 `"cc-switch"`，`user`（凭据管理器里的"用户名"元数据）为 `<app>/<provider_id>` 或 `app`。
- 长度：target ≤ 32767 字符（远够）；值 ≤ 1280 字符（UTF-16 计），超限报错。

## 附录 C：环境变量命名

| 用途 | 名字 | 备注 |
|---|---|---|
| Claude API key | `ANTHROPIC_AUTH_TOKEN` 或 `ANTHROPIC_API_KEY` | 由 `meta.api_key_field` 决定，默认 `ANTHROPIC_AUTH_TOKEN` |
| Claude Base URL | `ANTHROPIC_BASE_URL` | |
| Claude 其他敏感 env | 原名（如 `OPENROUTER_API_KEY`） | 仅限 `is_sensitive_config_key` 命中的键 |
| Codex 第三方 API key | `CC_SWITCH_CODEX_API_KEY` | `config.toml` 中 `env_key = "CC_SWITCH_CODEX_API_KEY"` |
| Codex 官方 API key | `OPENAI_API_KEY` | Codex 内置 provider 只认它 |
| Pi API key | `CC_SWITCH_PI_<KEY>_API_KEY` | `<KEY>` = provider key 转大写、非 `[A-Z0-9]` 替换为 `_`、连续 `_` 合并、长度 ≤ 64 |
| Pi 敏感 header | `CC_SWITCH_PI_<KEY>_HEADER_<NAME>` | `<NAME>` 同上规则处理 header 名 |
| 测试专用 | `CC_SWITCH_TEST_*` | 集成测试用，`Drop` 清理 |

`EnvSink::set` 的白名单正则：`^(ANTHROPIC_[A-Z0-9_]+|OPENAI_API_KEY|CC_SWITCH_[A-Z0-9_]+|<Claude extra_env 命中的键>)$`。禁止列表（无条件拒绝）：`PATH`、`PATHEXT`、`COMSPEC`、`TEMP`、`TMP`、`USERPROFILE`、`HOMEPATH`、`HOMEDRIVE`、`SYSTEMROOT`、`WINDIR`、`APPDATA`、`LOCALAPPDATA`、`PROGRAMFILES*`、`PSMODULEPATH`。

## 附录 D：`settings` 表新增键

| 键 | 值 | 用途 |
|---|---|---|
| `secrets_migration_pending` | `"1"` / `"0"` | 由 schema v19 置 1，6.2 完成后置 0 |
| `live_reapply_pending` | `"1"` / `"0"` | 6.2 完成后置 1，6.4 完成后置 0 |
| `secrets_migration_report` | JSON（无值） | 6.5 提示；用户确认后加 `confirmed: true` |
| `managed_env_vars` | JSON | 5.3.3 所有权记录 |
| `known_secret_targets` | JSON 数组 | 5.4 孤儿清理的索引 |

## 附录 E：新增 Cargo 依赖与特性

```toml
keyring = { version = "3", default-features = false, features = ["windows-native"] }
zeroize = "1"

[target.'cfg(target_os = "windows")'.dependencies]
windows-sys = { version = "0.61", features = [
    "Win32_Globalization",
    "Win32_Storage_FileSystem",
    "Win32_UI_Shell",
    "Win32_UI_WindowsAndMessaging",   # 新增：SendMessageTimeoutW / WM_SETTINGCHANGE / HWND_BROADCAST / SMTO_ABORTIFHUNG
] }
```

`winreg = "0.52"` 继续使用（`HKCU\Environment` 读写）。
