# 施工方案：凭据后端从 Windows 凭据管理器切换到 1Password

状态：待施工
拟定版本：2.3.0
基线：`main` @ 99e14e6（2.2.9）
说明：本方案以当前代码为准，**不参考** `docs/plans/1password-secret-backend-plan-zh.md` 等旧文档。与旧文档冲突时以本文件为准。

---

## 0. 一页纸总结（给施工者）

| 项 | 结论 |
|---|---|
| 目标 | 运行时**只**从 1Password 取钥匙；CCS 本地（DB、内存、凭据管理器、注册表）不留任何钥匙明文 |
| 取钥匙方式 | 调用本机 `op.exe`（1Password CLI，已实测 2.39.0，走桌面 App 集成 + Windows Hello 解锁） |
| 最大约束 | **每次调用 `op` 约 6~9 秒**。所以设计核心是：**一次操作只调一次 `op`**，而不是一次读一个字段 |
| 存储布局 | **每个供应商 = 1Password 里的 1 个条目（item）**，api_key / base_url / 额外 env / 敏感 header 都是这个条目里的字段。一次 `op item get` 拿全 |
| 本地只存 | 「条目引用」（vault id + item id + 字段名清单），不存值 |
| 强制严格投递 | 1Password 模式下**不再往 `HKCU\Environment` 写钥匙**（那里同账号程序随手可读，等于换个抽屉），只在「打开终端 / `ccs env`」时临时注入 |
| 凭据管理器 | 只在一次性迁移中**读出 → 写入 1Password → 校验 → 删除**；之后运行时代码不再触碰 |
| 不解锁就不能用 | 1Password 锁定/未登录/断网 → 明确报错，**绝不静默降级为“没有钥匙”**，也绝不回退凭据管理器 |

---

## 1. 现状（从代码核实，非文档）

### 1.1 存储抽象

- `src-tauri/src/secrets/store.rs:85` `trait SecretStore`：**按单个字段**读写（`get/set/delete(&SecretTarget)`，外加 `probe`、`list_targets`、`get_target_raw/set_target_raw`）。
- 唯一真实实现：`WindowsSecretStore`（`keyring` crate，`CredReadW/CredWriteW`），另有 `InMemorySecretStore`（测试）、`UnsupportedSecretStore`。
- 命名：`secrets/target.rs`，`cc-switch/v1/provider/<app>/<id>/{api_key|base_url|env/<VAR>}`、`cc-switch/v1/app/<app>/<field>`、`cc-switch/v1/probe`。
- `keyring` 接口不能按前缀枚举（`windows_enumerate_targets` 是后加的 Win32 直调，只给导出与卸载用），所以 DB 设置项 `known_secret_targets`（`secrets/extractor.rs:246-286`）登记“有哪些条目”，extra_env 的变量名**只能**从这里知道。
- 构造点：GUI `lib.rs:517-543`（构造 + 启动 probe，失败弹“重试/退出”阻断对话框）；CLI `cli/mod.rs:197`（`ccs.exe` / `ccs_open.exe`，不 probe）。
- 内存里**没有**钥匙值缓存；`secrets/scan.rs:8` 的静态表只用于日志脱敏与导出防护。`AppState.sync_kek`（`store.rs:65`）缓存 E2E 同步口令派生出的密钥。

### 1.2 读取次数（这是 1Password 下必须重做的根本原因）

读取几乎都汇聚在 `ProviderService::provider_env_pairs`（`services/provider/mod.rs:1086`）：

| App | 每次调用的 store 读取 |
|---|---|
| Claude | api_key 1 + base_url 1 + extra_env N（`load_extra_env_pending`，`mod.rs:1903`） |
| Codex | api_key 1 |
| Pi | api_key 1 + 敏感 header H |

各流程叠加（非严格模式）：

- **切换**（`switch_normal`，`mod.rs:561`）：出场供应商回填写入 → Codex `provider_has_stored_key`（`live.rs:564`）→ `preflight_env_delivery`（`mod.rs:839`，调一次 `provider_env_pairs`）→ `deliver_env_credentials`（`mod.rs:882`，**又调一次**）→ `write_live`（Codex 再读一次 base_url，`live.rs:808`）。Claude 一次切换 = 2×(2+N) 次读取。
- **供应商列表** `get_providers`（`commands/provider.rs:18` → `load_secret_status` `:88`）：**每个有 base_url 的供应商读 1 次 base_url 值**（`:100-112`）拿去在卡片上显示端点；未登记的还要探测 api_key。
- **打开终端**（GUI `commands/misc.rs:~2920 open_provider_terminal`、右键 `bin/ccs_open.rs` → `cli/mod.rs:172`）、**`ccs env`**（`cli/mod.rs:257`）：各调一次 `provider_env_pairs`；Pi 对每个启用的供应商各调一次（`misc.rs:2965`）。
- **显示明文**：`reveal_provider_secret`（`commands/provider.rs:138`）1 次。
- **启动**：probe（写/读/删 probe 条目）、`reapply_live_after_migration`（`lib.rs:616`，等同全量切换）、Pi 原生供应商逐个 `deliver_env_credentials_pub`（`lib.rs:827`）。
- **同步**：WebDAV 密码 1、S3 两把、E2E 口令（`secrets/sync_secrets.rs`、`services/sync_e2e.rs`、`sync_protocol.rs`），**自动同步后台线程会周期性读**（`services/webdav_auto_sync.rs:111`、`s3_auto_sync.rs:111`）。

在凭据管理器下每次读 < 1ms，所以代码随手多读；换成 1Password 每次 6~9 秒，照搬 = 切一次 Claude 供应商 30~60 秒，列表加载 N×7 秒。**不能“只换一个 Store 实现”了事。**

### 1.3 钥匙最终落在哪里

- Claude：`~/.claude/settings.json` 经 `live_sanitizer.rs:19` 去掉钥匙；钥匙和 `ANTHROPIC_BASE_URL` 走环境变量。
- Codex：`auth.json` 去掉钥匙（`live.rs:806`），`config.toml` 写 `env_key`，**base_url 明文写进 config.toml**（`live.rs:808-821`）。
- Pi：`models.json` 写 `$CC_SWITCH_PI_<ID>_API_KEY` 引用，**baseUrl 明文写进文件**（`pi.rs:207-216`）。
- 非严格模式：钥匙写进 **`HKCU\Environment`**（`env_delivery/sink.rs`），**同账号任何程序可读**——和凭据管理器是同一等级的风险。
- 严格模式（`settings.rs:788 strict_for`，2.2.4 起默认开）：不写注册表，只在 CCS 拉起的终端进程里注入。

### 1.4 必须修掉的既有隐患

`provider_env_pairs` 里读取失败被 `.ok().flatten()` 吞掉（`mod.rs:1098` 起），在凭据管理器下几乎不会失败所以无害；**在 1Password 下“已锁定/用户取消/断网”会被当成“没有钥匙”**，结果是终端静默拿到空钥匙或 Codex 报“缺钥匙”。必须改为向上传播错误。

---

## 2. 目标与边界

### 2.1 目标（对应用户诉求）

1. 运行时不再读写 Windows 凭据管理器。
2. 所有钥匙（供应商 api_key、额外 env、Pi 敏感 header、WebDAV 密码、S3 两把、E2E 同步口令）放 1Password；base_url 是否也算秘密见 D3。
3. 要用时现取，一次操作用完即 `Zeroizing` 丢弃；CCS 不做跨操作缓存（进程内、磁盘都不留）。
4. 1Password 未解锁 → CCS 取不到 → 该操作明确失败并提示“请解锁 1Password”。
5. 接受：每次取数秒、断网不可用。

### 2.2 诚实边界（写进用户手册，不要夸大）

能防：拷走硬盘/DB/备份拿不到钥匙；同账号程序翻凭据管理器、翻注册表拿不到（凭据管理器已清空、注册表不再投递）；1Password 锁定期间任何人（含 CCS 自己）拿不到。

防不住：
- **已注入钥匙的终端进程**：它的环境块里有明文，同账号程序可以读子进程环境。这是“给 CLI 工具用钥匙”的物理下限。
- **Codex / Pi 的 base_url** 会以明文写进它们的配置文件（工具本身只认文件里的 URL）。见决策 D3。
- 1Password 解锁窗口内，同账号恶意程序也能调 `op`（1Password 的 App 集成按进程弹授权，会有提示，但用户可能误点）。
- CCS 进程内存在取钥匙到注入完成的几秒内有明文。

---

## 3. 总体施工原则

1. **一次操作，最多一次 `op` 往返**（每个供应商）。任何流程里对同一供应商读第二次，视为 bug。实现手段：把“按字段读”改成“按供应商整包读”，在流程入口读一次，把 `ProviderSecrets` 作为参数传下去。
2. **不缓存**。整包只活在一次函数调用栈里，类型用 `Zeroizing`；禁止放进 `AppState`、`static`、`OnceLock`、DB、日志。唯一例外见 D6（`sync_kek`）。
3. **失败即失败**。所有 1Password 错误必须分类（未安装 / 未登录 / 锁定或用户取消 / 网络 / 条目不存在 / 其它）并传播到 UI；禁止 `.ok()`、`unwrap_or_default()` 吞错；禁止回退凭据管理器。
4. **钥匙不进命令行参数**。`op item create/edit` 一律用 **stdin 管道 JSON**（CLI 2.39 已支持，`op item edit --help` 明确“piped input 与 `--template` 不可同时用”）；绝不用 `field=value` 赋值语法（命令行参数同账号可见）。也不写临时模板文件。
5. **`op.exe` 用绝对路径**，不依赖 `PATH` / 当前目录搜索（防 DLL/EXE 劫持）；可选校验签名（D8）。
6. **不阻塞 UI 线程**。任何可能调 `op` 的 Tauri 命令必须在 `spawn_blocking` 或 async 中执行（`delete_provider`、`open_provider_terminal` 等当前是同步命令，见 §6.5）。
7. **启动不取钥匙**。App 能在 1Password 锁定时正常打开、浏览、编辑非敏感字段；只有真正用钥匙时才调用 `op`。
8. **本地元数据只存“引用”**：vault id、item id、字段名、有无某字段的布尔值；这些不是秘密。
9. **测试靠 trait 注入**：保留 `InMemory` 实现，新增“计数 fake”，用断言锁死原则 1（每流程 op 调用次数）。
10. **迁移可中断、可重跑、先写后删**：凭据管理器里的条目只有在 1Password 写入并回读校验一致后才删除。

---

## 4. 目标架构

### 4.1 新抽象：按供应商整包的 `SecretVault`

在 `secrets/` 新增 `vault.rs`，定义以**供应商**和**应用级组**为粒度的接口（同步接口即可——调用方几乎全是同步 + `block_on`，见 §1.2，1Password 实现本来就是阻塞子进程；避免再套 `futures::executor::block_on`）：

```rust
pub enum SecretGroup {
    Provider { app: AppType, provider_id: String },
    AppSync,            // WebDAV 密码 / S3 两把 / E2E 口令，放同一个条目
}

pub trait SecretVault: Send + Sync {
    /// 一次往返拿整包。条目不存在 => Ok(None)；锁定/断网等 => Err
    fn fetch(&self, group: &SecretGroup) -> Result<Option<SecretBundle>, VaultError>;
    /// 整包覆盖写（新建或编辑），返回引用
    fn put(&self, group: &SecretGroup, bundle: &SecretBundle) -> Result<VaultRef, VaultError>;
    fn delete(&self, group: &SecretGroup) -> Result<(), VaultError>;
    /// 轻量状态检测，不取值：已安装？已登录？（不强制解锁）
    fn status(&self) -> VaultStatus;
    fn backend_name(&self) -> &'static str;
}
```

- `SecretBundle`：`BTreeMap<String /*字段名*/, Zeroizing<String>>`，并提供到/从 `ProviderSecrets`（`secrets/types.rs`）的转换。字段名约定：`api_key`、`base_url`、`env.<VAR>`、`app.<field>`（如 `app.webdav_password`、`app.s3_access_key_id`、`app.s3_secret_access_key`、`app.e2e_passphrase`，以 `sync_secrets.rs`/`sync_e2e.rs` 中现用的 target 为准一一映射）。
- `Debug` 必须脱敏（仿 `ProviderSecrets` 的 `Debug`）。
- `VaultError` 映射到 `AppError::localized(code, zh, en)`，code 固定为：`vault_not_installed`、`vault_not_signed_in`、`vault_locked`（含用户取消授权）、`vault_network`、`vault_timeout`、`vault_item_conflict`、`vault_other`。前端按 code 显示提示与“重试”按钮。

实现：
- `OnePasswordVault`（新，`secrets/onepassword.rs`）——生产唯一实现。
- `InMemoryVault`（测试）+ `CountingVault` 包装器（测试，记录 fetch/put/delete 次数）。
- 旧的 `SecretStore` + `WindowsSecretStore` **保留但降级**为迁移源（只在 `secrets/migration_1p.rs` 与卸载清理里用），不再挂在 `AppState` 上。

**为什么新建而不是在 `SecretStore` 上加 `get_bundle`**：旧 trait 的每一个方法都是“按字段”的，留着就会有人继续逐字段调用，原则 1 守不住。换类型让编译器帮忙找出所有调用点。

### 4.2 `AppState` 改造

`store.rs:60` 的 `pub secrets: Arc<dyn SecretStore>` → `pub vault: Arc<dyn SecretVault>`。字段改名是有意的：强制所有调用点重新审视。`AppState::new` 签名同步修改；GUI（`lib.rs:517`）与 CLI（`cli/mod.rs:197`）构造 `OnePasswordVault`。

### 4.3 本地引用表（取代 `known_secret_targets`）

DB 升 `SCHEMA_VERSION` 19 → 20（`database/mod.rs:48`），新增表：

```sql
CREATE TABLE secret_refs (
  app          TEXT NOT NULL,     -- 'claude'|'codex'|'pi'|'_app'
  provider_id  TEXT NOT NULL,     -- '_sync' 表示应用级组
  vault_id     TEXT NOT NULL,
  item_id      TEXT NOT NULL,
  fields       TEXT NOT NULL,     -- JSON 数组，仅字段名，如 ["api_key","base_url","env.FOO"]
  updated_at   INTEGER NOT NULL,
  PRIMARY KEY (app, provider_id)
);
```

- 列表页“有没有钥匙 / 缺钥匙”徽标、extra_env 变量名、删除时找条目，全部查这张表，**零 `op` 调用**。
- `known_secret_targets` 在迁移完成后保留一个版本（只读，诊断用），2.4 删除。
- `commands/diagnostics.rs:170` 改读 `secret_refs`（只输出字段名）。

**为什么存 item_id**：`op item get <title>` 要先按标题搜索，item id 直达更快也不会被同名条目误导；标题仍然按规则命名，作为 item id 失效时的兜底查找（见 §5.4）。

---

## 5. `OnePasswordVault` 实现要点（`secrets/onepassword.rs`）

### 5.1 定位 `op.exe`

顺序：用户在设置里指定的绝对路径 → `where.exe op` 的结果**转为绝对路径后**固定下来 → 常见安装路径（`%LOCALAPPDATA%\Microsoft\WinGet\Links\op.exe`、`%ProgramFiles%\1Password CLI\op.exe`）。
- 结果写入设置 `onepassword.op_path`（非秘密），之后每次直接用绝对路径 `Command::new(abs_path)`。
- 找不到 → `vault_not_installed`。
- 可选（D8）：用 `WinVerifyTrust` 校验签名主体为 AgileBits。

### 5.2 调用规范（一个私有函数 `run_op(args, stdin: Option<&[u8]>) -> Result<Zeroizing<Vec<u8>>, VaultError>`）

- `creation_flags(CREATE_NO_WINDOW)`；stdout/stderr 管道；stdin 需要时管道写入后立即关闭。
- 固定参数：`--format json`、`--account <account_id>`（设置里保存，非秘密）、`--vault <vault_id>`、`--no-color`；设 `OP_CACHE=false`（不让 `op` 自己在磁盘缓存条目）。
- 不继承可能干扰的环境：清除 `OP_SERVICE_ACCOUNT_TOKEN`、`OP_CONNECT_*`（防止被环境变量悄悄切到别的认证方式）。
- **超时 120 秒**（要给 Windows Hello 弹窗留人手操作时间；网络本身 6~9 秒）。超时 kill 子进程 → `vault_timeout`。
- stdout 读入 `Zeroizing<Vec<u8>>`，解析后立刻丢弃；**stdout 永不写日志**；stderr 只按关键字分类后记录**分类结果**，不原样记录（stderr 可能回显条目名）。
- stderr 分类（关键字以实测为准，施工时必须在本机逐一复现并把样本写进单测）：
  - `not currently signed in` / `no accounts configured` → `vault_not_signed_in`
  - `authorization prompt dismissed` / `account is not unlocked` / `locked` → `vault_locked`
  - `isn't an item` / `not found` → 条目不存在（`fetch` 返回 `Ok(None)`）
  - `dial tcp` / `timeout` / `connection` / `no such host` → `vault_network`
  - 其它 → `vault_other`（只记退出码）
- 进程内一把 `Mutex` 串行化**写**操作（沿用 `WindowsSecretStore` 的做法）；读不锁。

### 5.3 条目布局

- Vault：用户在设置里选定（D2，推荐专用 vault “CC Switch”）。
- 类别：`API Credential`（`API_CREDENTIAL`）。
- 标题：`cc-switch/<app>/<provider_id>`，应用级组：`cc-switch/app/sync`。标签：`cc-switch`。
- 字段：全部 `CONCEALED` 类型，label 即 §4.1 字段名；另加一个非秘密字段 `cc-switch-schema = 1`（STRING）用于兼容性判断。
- 条目的 notes 写“由 CC Switch 管理，请勿手工改标题/字段名”。

### 5.4 各操作映射

| 操作 | `op` 调用 | 备注 |
|---|---|---|
| `fetch` | `op item get <item_id> --reveal --format json` | 1 次往返拿全部字段；`--reveal` 必须带否则 CONCEALED 值被遮。item_id 失效（not found）时再按标题 `op item get "cc-switch/<app>/<id>"` 兜底一次并更新 `secret_refs`。兜底只允许在“not found”时触发，不能成为常规第二次往返 |
| `put`（新建） | `op item create --vault <v> --format json`，stdin 管道 JSON | 返回 id 写入 `secret_refs` |
| `put`（覆盖） | `op item edit <item_id> --format json`，stdin 管道完整 JSON | 需要先有原条目 JSON 才能整包编辑：**在同一次写操作里**先 `get` 再 `edit`，所以写操作 = 2 次往返（写不频繁，可接受）。若字段集合只增不减，可直接用上次 `get` 的结构 |
| `delete` | `op item delete <item_id> --archive` | 默认进归档（D5），用户可在 1Password 里恢复 |
| `status` | `op account list --format json`（不需解锁、不联网或极快） | 判断“已安装+已配置账户”；**不**用 `op whoami`/`vault list` 做启动探测，避免每次启动触发解锁 |

> 施工者须在本机实测：`op item edit` 管道 JSON 时是“整体替换字段”还是“合并”，以及删除字段的写法；把结论写进 `onepassword.rs` 顶部注释和单测样本。不确定时采用“`get` → 在 JSON 上改 → `edit` 整包写回”。

---

## 6. 调用方改造（逐流程，关键点 + 做法 + 原因）

### 6.1 读取汇聚点：`provider_env_pairs`（最重要）

`services/provider/mod.rs:1086`

- 改签名：`fn provider_env_pairs(app_type, provider, secrets: &ProviderSecrets, warnings) -> Vec<(String, Zeroizing<String>)>` ——**变成纯函数**，不再自己读 store。
- 新增 `fn fetch_provider_secrets(state, app, provider_id) -> Result<ProviderSecrets, AppError>`：唯一调 `state.vault.fetch` 的地方。条目不存在 → 返回空 `ProviderSecrets`（让后续“缺钥匙”逻辑照旧工作）；其它错误原样上抛。
- 删除 `load_extra_env_pending`（`mod.rs:1903`）：extra_env 已在整包里。
- 删除 `.ok().flatten()` 吞错（§1.4）。

**原因**：把“取”和“用”分开，取只发生在流程入口一次，后面随便用几次都不增加 `op` 调用。

### 6.2 切换 `switch_normal`（`mod.rs:561`）与 Pi `enable`（`pi.rs:218`）

- 1Password 模式下**严格投递强制开启**（§6.8），因此 `preflight_env_delivery` / `deliver_env_credentials` 走撤回分支，不需要钥匙。
- 仍需钥匙的只有：Codex `provider_has_stored_key`（`live.rs:564`）和 Codex/Pi `write_live` 写 base_url（`live.rs:808`、`pi.rs:210`）。
  - `provider_has_stored_key` → 改查 `secret_refs.fields` 是否含 `api_key`，**不调 op**。
  - base_url：见 D3。推荐方案 A（base_url 作为非秘密配置随供应商保存在 DB）→ 切换 **0 次** `op`；方案 B（base_url 也放 1P）→ Codex/Pi 切换 1 次 `fetch`，并在函数入口取一次后传给 `write_live`。
- 出场供应商“回填写入”（`mod.rs:606` `strip_and_store_provider_secrets`）：只有在 live 文件里检测到**新的明文钥匙**时才 `put`；没有变化必须跳过（当前是否无条件写需核实，若是则加比较——比较对象是 `secret_refs.fields` 与抽取结果的字段名集合 + 值是否非空，不能为了比较去 `fetch`）。

### 6.3 列表 `get_providers` / `load_secret_status`（`commands/provider.rs:18/:88`）

- `apiKey.present`、`extraEnv` 名单：查 `secret_refs`。
- `baseUrl`：D3 方案 A 时从 DB 读；方案 B 时**不返回值**，只返回 `present: bool`，前端卡片（`src/components/providers/ProviderCard.tsx:227,251`）端点处显示“已配置（在 1Password）”。
- 删除 `ensure_registered` 里的 `get` 探测（`:78`）。
- 断言：列表加载 `op` 调用数 = 0。

### 6.4 显示明文 `reveal_provider_secret`（`commands/provider.rs:138`）

- 调 `fetch` 一次，取所需字段返回；保留 `note_session_secret`（`provider.rs:192`，用于日志脱敏）。
- 前端 `ApiKeyInput.tsx:75`：点击后显示“正在向 1Password 请求…（可能需要解锁）”加载态，超时/锁定时显示分类错误。

### 6.5 打开终端 / 右键 / `ccs env`

- `open_provider_terminal`（`commands/misc.rs:~2920`）目前是**同步命令，跑在 IPC 线程**，调 `op` 会让界面冻结 6~9 秒 + 等解锁。改为 `async` 命令内部 `spawn_blocking`；前端按钮显示加载态。
- `launch_provider_terminal`（`misc.rs:2933`，GUI 与 `cli/mod.rs:172` 共用）：入口处 `fetch_provider_secrets` 一次，传给 `provider_env_pairs`。
- Pi 多供应商（`misc.rs:2965`）：每个启用供应商 1 次 `fetch`，**串行**（并发调 `op` 可能触发多次授权弹窗）；数量多时在 UI 提示预计耗时。可选优化（D7）：Pi 所有供应商放同一条目。
- `ccs_open.exe`（右键）：独立进程，没有 GUI 可弹错误——失败时用 `MessageBoxW` 显示分类错误（参照现有 `ccs_unavailable` 的处理路径），不能静默开一个没有钥匙的终端。
- `ccs env`（`cli/mod.rs:257`）：锁定/失败 → 非零退出码（新增退出码 6 = vault 锁定/取消，7 = 网络/超时；5 保留“后端不可用”），stderr 输出本地化提示。`--clear` 不调 op（现状即如此，保持）。
- CLI 的 `build_state`（`cli/mod.rs:188`）构造 `OnePasswordVault`，不做 status 探测（原来就跳过 probe 求快，保持）。

### 6.6 新增 / 编辑 / 删除

- `add`（`mod.rs:295`）/ `update`（`mod.rs:336`）/ Pi `add`/`update`（`pi.rs`）：
  - 抽取逻辑 `SecretExtractor::extract_with_meta`（`extractor.rs:43`，纯函数）**保留**。
  - `strip_and_store_provider_secrets`（`mod.rs:1932`）与 Pi 的 `strip_and_store_pi_secrets`（`pi.rs:347`）改为：抽出 `ProviderSecrets` → 若编辑时某字段未提供（表单里“保留原值”）需要与旧值合并 → **合并时必须 `fetch` 一次**，然后整包 `put`。新增时直接 `put`。
  - 校验里“候选 id 缺钥匙时 get 一下”（`mod.rs:1657/:1693`）改查 `secret_refs`。
  - 编辑的是当前供应商时，后续 `deliver_env_credentials`（严格模式下不需要钥匙）不再额外读。
  - DB 写失败回滚：新增的条目 `delete`（归档）；编辑的条目无法原子回滚——**先写 DB 的非敏感部分失败概率低，顺序改为：先 `put` 到 1P，成功后写 DB + `secret_refs`；DB 失败则记录孤儿，由 §6.10 清理**。
- `delete_provider`（`commands/provider.rs:244`，**同步命令**）→ 改 async + `spawn_blocking`；`delete_provider_secrets`（`mod.rs:1851`）→ 查 `secret_refs` 拿 item_id → `vault.delete` → 删 `secret_refs` 行。1P 删除失败（锁定）：供应商照删，`secret_refs` 行标记 `pending_delete`，下次成功调用 op 时顺手清理，并在 UI 提示。

### 6.7 启动流程（`lib.rs`）

- 删除 `WindowsSecretStore::new` + `probe` 阻断循环（`lib.rs:517-543`）。改为构造 `OnePasswordVault`（只定位 op.exe，不调用），然后异步 `status()`，结果推给前端显示横幅（未安装/未登录/正常），**不阻断启动**。
- `run_credential_migration_if_pending`（`lib.rs:552`）：这是“从 live 文件/DB 明文抽钥匙”的旧迁移，仍需保留（新装用户导入现有 `~/.claude/settings.json` 的钥匙），但写入目标改为 vault；它在启动时会触发 op 与解锁——改成**有待迁移项时弹对话框让用户确认后执行**，不自动跑。
- `reapply_live_after_migration`（`lib.rs:616`）、Pi 原生供应商逐个投递（`lib.rs:827`）：严格模式下不需要钥匙；确认改造后它们在 1P 模式下 0 次 `op` 调用（用 `CountingVault` 测试断言）。
- `recover_legacy_named_secrets`（`lib.rs:592`）：只和凭据管理器历史命名有关，并入 §7 迁移，从启动路径移除。
- `sweep_plaintext_app_settings`（`lib.rs:572`）：发现 settings 里的 WebDAV/S3 明文时写入 `AppSync` 组（需要 op），同样改为“提示用户后执行”。

### 6.8 环境投递：1Password 模式强制严格

- `settings.rs:788 strict_for`：vault 后端为 1Password 时恒返回 `true`。
- `EnvDeliverySection.tsx`：开关置灰并说明“使用 1Password 时，钥匙不写入系统环境变量（注册表同账号程序可读）”。
- 切换到 1P 后执行一次 `purge_all_env_delivery`（`mod.rs:997`），把注册表里历史投递的钥匙清掉。**这一步是用户诉求 1 的一部分，不能漏**。
- `set_env_delivery_strict_mode/apps`（`commands/settings.rs:117/:135`）在 1P 模式下拒绝关闭。

**原因**：注册表 `HKCU\Environment` 和凭据管理器一样对同账号程序零门槛；只把钥匙搬到 1P 却在每次切换后写回注册表，等于没改。

### 6.9 同步（WebDAV / S3 / E2E）

- `secrets/sync_secrets.rs` 的 extract/restore 改成读写 `AppSync` 组（整包：先 fetch 再改字段再 put）。
- 一次同步操作开始时 `fetch` 一次 `AppSync`，把需要的值传进 `services/webdav_sync.rs` / `s3_sync.rs` / `sync_e2e.rs` 的函数（这些函数签名目前接 `&Arc<dyn SecretStore>`，改为接具体值或一个 `SyncCredentials` 结构）。
- **自动同步后台线程**（`webdav_auto_sync.rs:111`、`s3_auto_sync.rs:111`）：每次跑都要取钥匙，会周期性弹解锁。见 D4，推荐：1P 模式下自动同步在 vault 锁定时**跳过本轮**（不弹窗：先用不需要交互的方式判断，若无法判断则直接关闭自动同步并在 UI 说明），手动同步照常。
- `sync_kek`（`store.rs:65`）：见 D6。

### 6.10 维护类

- `secrets_cleanup_orphans`（`mod.rs:113`）：改为对比 DB 供应商与 `secret_refs`，以及 `op item list --tags cc-switch --format json`（1 次往返，只拿标题/id，不带 `--reveal`）找 1P 里没有对应供应商的条目，列给用户确认后归档。
- **便携包导出/导入**（`secrets/portable.rs`、`SecretsPortableSection.tsx`）：1P 自己跨设备同步，便携包的意义消失且导出=把钥匙搬出 1P。推荐（D9）：1P 模式下**隐藏导出**；**保留导入**并改为写入 1P（方便老设备导出的包迁入）。
- 卸载清理（`uninstall_cleanup.rs:166`）：继续清凭据管理器残留（`windows_enumerate_targets` + `windows_delete_credential`，这两个函数保留）；**不动 1Password 条目**（D5），卸载完成页提示“1Password 中 `cc-switch` 标签的条目需手动处理”。
- 诊断包（`diagnostics.rs`）：增加 vault 状态、op 版本、op 路径、`secret_refs` 行数与字段名，不含值。

---

## 7. 从凭据管理器迁移（一次性）

新模块 `secrets/migration_1p.rs`，由设置页“迁移到 1Password”向导触发（首次升级时弹引导）。

步骤：
1. **前置**：`status()` 正常；用户选定 account 与 vault（`op account list` / `op vault list`，vault list 会要求解锁——这是用户主动操作，可以）。
2. **盘点**：`windows_enumerate_targets("cc-switch/")`（凭据管理器可以枚举，`store.rs:141`）+ `known_secret_targets` + legacy 命名（`legacy.rs`）→ 按 `(app, provider_id)` 分组；`app/*` 归入 `AppSync`；`probe` 丢弃。
3. **读出**：逐条 `WindowsSecretStore::get`（本地，快）。
4. **写入**：每组 1 次 `put`（新建）。进度条显示“第 k/N 个，约 7 秒/个”。
5. **校验**：每组 `fetch` 回来逐字段比对（值用常量时间比较）。
6. **提交**：写 `secret_refs`；设置 `secret_backend = "onepassword"`、`onepassword.migrated_at`。
7. **删除**：校验全部通过后，逐条 `windows_delete_credential` 删凭据管理器条目；再 `purge_all_env_delivery` 清注册表。
8. **可重跑**：每组完成后落库一个进度标记；中断后重跑跳过已完成组（已完成=`secret_refs` 有行且 1P 条目存在）。若 1P 已有同标题条目（上次中断写了一半），用它而不是重复创建（`vault_item_conflict` 时按标题查找复用并整包覆盖）。
9. 失败任何一步：**不删除**凭据管理器数据，停在当前组，报告分类错误。

迁移完成前：运行时仍用旧后端（`secret_backend` 缺省 = `"windows"`）。即 `AppState` 在 2.3.0 需要能按设置选后端——**这是唯一允许旧后端继续跑的窗口**。为不让两套 trait 并存扩散，旧后端也包一层实现 `SecretVault` 的适配器 `LegacyWindowsVault`（fetch = 逐字段读 + 按 `known_secret_targets` 拼包），迁移完成后不再构造它。2.4 删除适配器与 `keyring` 依赖（卸载清理改用 Win32 直接调用，已有 `windows_enumerate_targets` 可复用）。

新装用户：首次启动直接引导设置 1Password，没有旧后端。

---

## 8. 前端改动清单

- 新增 `src/components/settings/OnePasswordSection.tsx`：状态（未安装/未登录/正常 + op 版本 + 路径）、选择 account/vault、迁移向导入口、“测试取钥匙”按钮（会触发解锁，用于用户自检）。
- 全局横幅：vault 不可用时提示（不阻断浏览）。
- 所有会触发 op 的按钮（切换 Codex/Pi 时若 D3 选 B、打开终端、显示明文、保存、删除、同步）统一加载态文案“正在向 1Password 请求，可能需要解锁…”，并处理 `vault_*` 错误码（显示本地化原因 + 重试）。
- `ProviderCard.tsx`：端点显示按 D3。
- `EnvDeliverySection.tsx`：1P 模式置灰（§6.8）。
- `SecretsPortableSection.tsx`：按 D9。
- `SecretStoreMaintenance.tsx`、`SecretsMigrationDialog.tsx`：文案从“凭据管理器”改为按后端显示。
- 四语言 i18n（与现有 2.2.9 一致的语言集合）全部补齐新文案与错误码。

---

## 9. 测试方案

单元测试（CI 可跑，不依赖真实 op）：
1. `run_op` 的 stderr 分类：用施工者在本机采集的真实 stderr 样本（锁定、取消授权、未登录、断网、not found）做表驱动测试。
2. 条目 JSON 的序列化/反序列化：字段 label、CONCEALED 类型、`--reveal` 输出解析、未知字段容忍。
3. **调用次数断言（原则 1 的护栏）** —— 用 `CountingVault`：
   - `get_providers`：fetch = 0
   - 切换 Claude（严格）：fetch = 0；切换 Codex：fetch = 0（D3-A）/ 1（D3-B）
   - 打开终端（单供应商）：fetch = 1
   - `ccs env`：fetch = 1；`ccs env --clear`：0
   - 显示明文：1；新增：put = 1，fetch = 0；编辑（部分字段保留）：fetch = 1、put = 1
   - 启动（无待迁移、严格模式）：0
4. **错误传播**：`CountingVault` 注入 `vault_locked`，断言打开终端 / `ccs env` 失败而不是注入空钥匙（§1.4 的回归测试）。
5. 1P 模式下 `strict_for` 恒 true，`set_env_delivery_strict_mode(false)` 被拒。
6. 迁移：InMemory 旧库 → InMemoryVault，覆盖中断重跑、校验失败不删旧数据、legacy 命名条目、`app/*` 归组、probe 丢弃。
7. `SecretBundle`/`VaultError` 的 `Debug`/`Display` 不含值。

集成测试（`#[ignore]`，本机手动跑，仿 2.2.9 的真实凭据端到端测试写法）：
- 真实 op：create → get → edit → get → delete(archive) 往返，使用独立测试 vault 与随机标题，`Drop` 守卫清理。
- 锁定 1Password 后调用 fetch，断言 `vault_locked`（或超时）。

手动验收：
- 断网 → 打开终端报“网络”错误；恢复后可用。
- 1P 锁定 → 右键打开终端弹对话框提示解锁；解锁后重试成功，全程无空钥匙终端。
- 迁移后：`cmdkey /list | findstr cc-switch` 无结果；`reg query HKCU\Environment` 无 CCS 投递的钥匙变量；DB 中 `grep` 不到任何钥匙片段。
- 用 Process Explorer 查看 `op.exe` 的命令行参数，确认不含钥匙。

---

## 10. 施工顺序（每步可独立合并、可编译、测试通过）

| 阶段 | 内容 | 验收 |
|---|---|---|
| P0 | 修 §1.4 吞错：`provider_env_pairs` 读取失败向上传播（凭据管理器下也应如此） | 新测试：注入失败时打开终端报错 |
| P1 | 新增 `SecretVault` trait、`SecretBundle`、`VaultError`、`InMemoryVault`、`CountingVault`、`LegacyWindowsVault` 适配器；`AppState.secrets` → `vault`；所有调用点改用整包接口（§6.1–6.6、6.9、6.10），行为仍跑在凭据管理器上 | 全量测试通过 + §9.3 次数断言全部通过（此时旧后端下断言同样成立） |
| P2 | DB v20 `secret_refs`；列表/校验/删除改查 `secret_refs`；`known_secret_targets` 同步写入供回退 | 列表 fetch=0 |
| P3 | `OnePasswordVault` 实现 + stderr 分类 + op 定位；设置页 1Password 区块（状态、account/vault 选择、测试取钥匙） | 本机集成测试通过 |
| P4 | 迁移向导 `migration_1p.rs`；`secret_backend` 设置切换；强制严格投递 + 注册表清理；启动流程去阻断 probe | 手动验收迁移三项检查 |
| P5 | 同步（AppSync 组、自动同步策略）、便携包、卸载、诊断、四语言 i18n、用户手册与 CHANGELOG | 全部手动验收项 |

**P1 是风险最大、改动最广的一步**：先在旧后端上把“按字段”全部改成“按整包”，用次数断言锁住，再接 1Password。这样 1P 接入时只是换实现，不会同时调试“调用结构”和“外部进程”两类问题。

---

## 11. 待用户确认的决策（施工默认值已给出）

| # | 问题 | 默认（推荐） | 理由 |
|---|---|---|---|
| D1 | 迁移完成后凭据管理器数据是否删除 | **删除**（校验通过后） | 用户诉求 1；留着等于没搬 |
| D2 | 使用哪个 vault | **专用 vault “CC Switch”**，由用户在设置中选择 | 权限隔离；`op` 调用固定 `--vault` |
| D3 | base_url 是否也放 1Password | **A：base_url 视为非秘密，存 DB**；api_key/env/header 放 1P | Codex/Pi 本来就把 URL 明文写进配置文件，放 1P 只增加每次切换 7 秒+解锁，却挡不住文件泄露；列表还能照常显示端点。**若用户坚持 URL 也是秘密 → 选 B**：切换 Codex/Pi 各 1 次 op、列表不显示端点、Claude 的 URL 只进终端环境 |
| D4 | 自动同步（WebDAV/S3）怎么办 | vault 锁定时跳过本轮、不弹窗；手动同步照常 | 后台周期性弹解锁不可接受 |
| D5 | 删除供应商/卸载时 1P 条目处理 | 删除供应商 → **归档**；卸载 → **不动**，提示手动处理 | 可恢复；卸载程序不应碰用户的密码库 |
| D6 | E2E 同步口令派生密钥 `sync_kek` 的进程内缓存 | **1P 模式下取消缓存**，每次同步现取口令现派生 | 与“CCS 一把不留”一致；代价是每次同步多一次 Argon2（约百毫秒级）+ 一次 op |
| D7 | Pi 多供应商是否合并成一个条目 | **不合并**（每供应商一个条目） | 结构统一；Pi 同时启用多个供应商且频繁开终端的场景少。若实际觉得慢再改 |
| D8 | 是否校验 `op.exe` 签名 | **校验**（WinVerifyTrust + 签名主体含 AgileBits），失败拒用 | `op.exe` 是整个方案的信任根，被替换就全泄露；成本低 |
| D9 | 便携包导出/导入 | 1P 模式下**隐藏导出、保留导入（写入 1P）** | 1P 自带跨设备同步；导出=把钥匙搬出保险箱 |
| D10 | 是否保留“回退到凭据管理器”的开关 | **不保留**（迁移后单向） | 用户明确不再使用凭据管理器；双后端长期并存会让原则 3 守不住 |

---

## 12. 施工陷阱清单（务必逐条自查）

1. **吞错**：全局搜 `.ok().flatten()`、`unwrap_or_default()`、`if let Ok(Some(` 出现在 vault 调用附近的地方，一律改为传播。锁定≠没钥匙。
2. **重复取**：一个流程里 `fetch_provider_secrets` 只能出现一次；Code review 时对每个命令画调用链。次数断言测试不许删、不许放宽。
3. **钥匙进命令行**：`Command::arg` 的参数里绝不能出现值；只允许 item id、vault id、account id、标题、字段 label。
4. **`op` 输出进日志**：`run_op` 以外不得接触原始 stdout；错误信息只用分类码。
5. **UI 冻结**：任何同步 Tauri 命令若可能触发 op，必须改 async/spawn_blocking（至少 `delete_provider`、`open_provider_terminal`、`import_default_config`、`remove_provider_from_live_config` 需核查）。
6. **并发弹窗**：不要并行发起多个 `op` 调用（授权弹窗会叠）。Pi 多供应商串行。
7. **启动触发解锁**：启动路径（`lib.rs` setup）里不得有任何 fetch/put；用 `CountingVault` 测试守住。
8. **注册表残留**：切到 1P 后必须执行一次 `purge_all_env_delivery`，并且 1P 模式下不存在任何写 `HKCU\Environment` 钥匙的路径。
9. **迁移先删后写**：禁止。必须写入 → 回读校验 → 再删。
10. **`ccs_open.exe` 静默失败**：它没有控制台，失败必须弹窗；绝不能打开一个缺钥匙的终端让用户以为成功。
11. **`OP_SERVICE_ACCOUNT_TOKEN` 等环境变量**：子进程启动前清掉，避免被外部环境悄悄改变认证方式与信任边界。
12. **stderr 关键字依赖**：不同 op 版本措辞可能变化；分类失败时归入 `vault_other` 并提示“重试”，不要误判为 not found（误判 not found 会导致“以为没钥匙”甚至重建条目覆盖）。**not found 的判断必须同时满足退出码与关键字**，拿不准就当错误。
