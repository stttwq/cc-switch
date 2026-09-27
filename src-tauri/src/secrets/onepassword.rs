//! 1Password 后端：通过本机 `op.exe`（1Password CLI）按整包读写凭据（施工方案 §5）。
//!
//! 设计核心（§3）：
//! - 一次操作最多一次 `op` 往返（读走 title 直取，写才可能 get+edit）。
//! - 钥匙绝不进命令行参数：`op item create/edit` 一律走 **stdin 管道 JSON**。
//! - `op.exe` 用绝对路径调用；子进程清掉 `OP_SERVICE_ACCOUNT_TOKEN` / `OP_CONNECT_*`。
//! - 失败即失败：所有错误分类并传播，绝不静默降级、绝不回退凭据管理器。
//! - stdout（含钥匙）永不写日志；stderr 只按关键字分类后记录**分类结果**。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::app_config::AppType;
use crate::secrets::vault::{
    SecretBundle, SecretGroup, SecretVault, VaultError, VaultRef, VaultStatus, FIELD_API_KEY,
    FIELD_APP_PREFIX, FIELD_BASE_URL, FIELD_ENV_PREFIX,
};

/// 条目类别：API Credential。
const OP_CATEGORY: &str = "API_CREDENTIAL";
/// 兼容性判断用的非秘密标记字段。
const SCHEMA_FIELD_LABEL: &str = "cc-switch-schema";
const SCHEMA_FIELD_VALUE: &str = "1";
/// 条目标签。
const OP_TAG: &str = "cc-switch";
/// `op` 调用超时（给 Windows Hello 解锁弹窗留人手操作时间；网络本身约 6~9 秒）。
const OP_TIMEOUT: Duration = Duration::from_secs(120);
/// CreateProcess 的 CREATE_NO_WINDOW 标志，避免弹出控制台窗口。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `op` 调用的内部错误：把「条目不存在」与其它分类错误分开，
/// 让 `fetch` 能把不存在翻译成 `Ok(None)`。
pub(crate) enum RunErr {
    /// 条目不存在（退出码非零 + not-found 关键字，§12.12）。
    NotFound,
    /// 其它分类错误。
    Vault(VaultError),
}

impl RunErr {
    fn into_vault(self) -> VaultError {
        match self {
            RunErr::NotFound => VaultError::Other("item not found".to_string()),
            RunErr::Vault(e) => e,
        }
    }
}

/// 定位 `op.exe`（§5.1）。顺序：显式路径 → `where op` → 常见安装路径。
/// 返回**绝对路径**，之后固定用它调用（防 DLL/EXE 劫持）。
pub fn locate_op(configured: Option<&str>) -> Option<PathBuf> {
    // 1) 用户在设置里指定的绝对路径。
    if let Some(p) = configured {
        let path = PathBuf::from(p);
        if path.is_absolute() && path.is_file() {
            return Some(path);
        }
    }
    // 2) where.exe op（转绝对路径）。用 System32 绝对路径，不依赖 PATH 解析（F4-7）。
    #[cfg(windows)]
    {
        let where_exe = std::env::var_os("SystemRoot")
            .map(|root| Path::new(&root).join("System32").join("where.exe"))
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\where.exe"));
        if let Ok(output) = Command::new(where_exe).arg("op").output() {
            if output.status.success() {
                if let Ok(text) = String::from_utf8(output.stdout) {
                    for line in text.lines() {
                        let candidate = PathBuf::from(line.trim());
                        if candidate.is_file() {
                            if let Ok(abs) = candidate.canonicalize() {
                                return Some(strip_unc(abs));
                            }
                            return Some(candidate);
                        }
                    }
                }
            }
        }
    }
    // 3) 常见安装路径。
    for var in ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(base) = std::env::var(var) {
            for rel in ["Microsoft\\WinGet\\Links\\op.exe", "1Password CLI\\op.exe"] {
                let candidate = Path::new(&base).join(rel);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// `canonicalize` 在 Windows 上会带 `\\?\` UNC 前缀，去掉它以免某些子进程 API 不认。
#[cfg(windows)]
fn strip_unc(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path
}

/// 1Password 后端（生产唯一实现）。
///
/// F2-2：持有 `Arc<Database>`，`fetch` / `put` / `delete` 先查 `secret_refs`——
/// `vault_id` 与当前配置一致且 `item_id` 是真实 1P id 时按 id 直达（标题只作兜底），
/// 避免同名条目（用户手工复制、历史超时重复）把读取带偏（原方案 §4.3 / P1-2）。
pub struct OnePasswordVault {
    runner: Arc<dyn OpRunner>,
    account: String,
    vault: String,
    db: Arc<crate::database::Database>,
}

/// 串行化所有 `op` 调用（进程全局）：并发会叠加多个授权弹窗（§12.6）。
static OP_LOCK: Mutex<()> = Mutex::new(());

impl OnePasswordVault {
    pub fn new(
        op_path: PathBuf,
        account: impl Into<String>,
        vault: impl Into<String>,
        db: Arc<crate::database::Database>,
    ) -> Self {
        Self {
            runner: Arc::new(ProcessOpRunner::new(op_path)),
            account: account.into(),
            vault: vault.into(),
            db,
        }
    }

    /// 测试注入构造：用 `FakeOpRunner` 脚本化 `op` 行为（F0-1）。
    #[cfg(test)]
    pub(crate) fn with_runner(
        runner: Arc<dyn OpRunner>,
        account: impl Into<String>,
        vault: impl Into<String>,
        db: Arc<crate::database::Database>,
    ) -> Self {
        Self {
            runner,
            account: account.into(),
            vault: vault.into(),
            db,
        }
    }

    /// 执行一次 `op`（§5.2）。`stdin` 需要时以管道写入后立即关闭。
    /// 返回 stdout（`Zeroizing`，解析后立刻丢弃，永不写日志）。
    ///
    /// item 子命令可能弹授权，必须拿全局锁串行（F4-3）。
    fn run_op(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Zeroizing<Vec<u8>>, RunErr> {
        self.runner.run(args, stdin, true)
    }

    /// `op item get` 的公共参数；`reference` 可以是条目 id（F2-2 直达）或标题（兜底）。
    fn base_read_args<'a>(&'a self, reference: &'a str) -> Vec<&'a str> {
        vec![
            "item",
            "get",
            reference,
            "--vault",
            &self.vault,
            "--account",
            &self.account,
            "--reveal",
            "--format",
            "json",
            "--no-color",
        ]
    }

    /// `op item delete`（归档）的公共参数；`reference` 同上。
    fn base_delete_args<'a>(&'a self, reference: &'a str) -> Vec<&'a str> {
        vec![
            "item",
            "delete",
            reference,
            "--vault",
            &self.vault,
            "--account",
            &self.account,
            "--archive",
            "--no-color",
        ]
    }

    // ─── F2-2：item_id 直达 / 标题兜底 / 同名冲突取最新 ─────────

    /// 从 `secret_refs` 读出该组已登记的真实 item id（vault 一致且非占位形式）；
    /// 引用行缺失、vault 不一致或 item_id 是占位（空串 / `provider/<app>/<id>` 等
    /// v20 回填与旧后端写入的形式，§12.9）时返回 `None`。
    fn ref_item_id(&self, group: &SecretGroup) -> Option<String> {
        let (app, provider) = group.ref_key();
        let (vault_id, item_id) = self
            .db
            .get_secret_ref_identity(&app, &provider)
            .ok()
            .flatten()?;
        if vault_id != self.vault || is_placeholder_item_id(&item_id, group) {
            return None;
        }
        Some(item_id)
    }

    /// 标题兜底（或 id 失效后按标题重找）命中条目后，把真实 item id 回写引用行：
    /// 行已存在只修 id（字段清单保持原样）；无行则用 fetch 回的字段清单整行登记。
    fn repair_ref(&self, group: &SecretGroup, item_id: &str, bundle: &SecretBundle) {
        let (app, provider) = group.ref_key();
        match self
            .db
            .repair_secret_ref_item_id(&app, &provider, &self.vault, item_id)
        {
            Ok(true) => {}
            Ok(false) if !bundle.is_empty() => {
                if let Err(e) = self.db.upsert_secret_ref(
                    &app,
                    &provider,
                    &self.vault,
                    item_id,
                    &bundle.field_names(),
                ) {
                    log::warn!("回写 secret_ref 失败（不影响本次读取）: {e}");
                }
            }
            Ok(false) => {}
            Err(e) => log::warn!("回写 secret_ref 失败（不影响本次读取）: {e}"),
        }
    }

    /// F3-8：列出 vault 中带 `cc-switch` 标签的全部条目（`op item list`，不带
    /// `--reveal`，不弹解锁、不含值）。孤儿清理的候选来源。
    pub(crate) fn list_tagged_items(&self) -> Result<Vec<OpItemListEntry>, VaultError> {
        let args = [
            "item",
            "list",
            "--vault",
            &self.vault,
            "--account",
            &self.account,
            "--tags",
            OP_TAG,
            "--format",
            "json",
            "--no-color",
        ];
        let bytes = self.run_op(&args, None).map_err(RunErr::into_vault)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| VaultError::Other(format!("parse op item list failed: {e}")))
    }

    /// F3-8：按 item id 归档条目（用户在 UI 确认过的孤儿）。已不存在视为成功（幂等）。
    pub(crate) fn archive_item_by_id(&self, item_id: &str) -> Result<(), VaultError> {
        let args = self.base_delete_args(item_id);
        match self.run_op(&args, None) {
            Ok(_) => Ok(()),
            Err(RunErr::NotFound) => Ok(()),
            Err(RunErr::Vault(e)) => Err(e),
        }
    }

    /// F4-5：「从 1Password 重建引用」原语——按 id 读单个条目的托管字段 label 清单
    /// （不带 `--reveal`，CONCEALED 字段无值，绝不含钥匙明文）。
    pub(crate) fn read_item_labels(&self, item_id: &str) -> Result<Vec<String>, VaultError> {
        let args = [
            "item",
            "get",
            item_id,
            "--vault",
            &self.vault,
            "--account",
            &self.account,
            "--format",
            "json",
            "--no-color",
        ];
        let bytes = self.run_op(&args, None).map_err(RunErr::into_vault)?;
        parse_item_labels(&bytes)
    }

    /// 标题命中多个（`ItemConflict`）时：`op item list --tags cc-switch`（不带
    /// `--reveal`）筛出同标题条目，取 `updated_at` 最新的一条——不弹解锁、不自动归档，
    /// 只告警（UI 清理由后续阶段提供入口）。找不到同标题条目时返回 `Ok(None)`，
    /// 调用方按「条目不存在」处理。
    fn resolve_conflicting_item_id(&self, title: &str) -> Result<Option<String>, VaultError> {
        let items = self.list_tagged_items()?;
        let latest = items
            .into_iter()
            .filter(|i| i.title == title && !i.id.is_empty())
            .max_by(|a, b| a.updated_at.cmp(&b.updated_at));
        match latest {
            Some(item) => {
                log::warn!(
                    "1Password 中存在同标题条目，已取最近更新的条目读取；请在 1Password 中清理重复条目（仅结构定位，不含值）"
                );
                Ok(Some(item.id))
            }
            None => Ok(None),
        }
    }

    /// F2-2 读取主路径：id 直达 → 标题兜底 → 同名冲突取最新。命中时返回
    /// 整包、成功那次 `op item get` 的原文（put 的 edit stdin 必须基于 op 返回的
    /// 完整条目 JSON），以及「需要回写 ref 的 item id」（引用直达命中时为 `None`）。
    ///
    /// 真机实测（op 2.39）：`op item get <id>` 会命中**归档区**条目（返回 JSON 带
    /// `"state":"ARCHIVED"`），按标题则不会。归档 = 已删除，一律按 NotFound 处理。
    fn fetch_item_resolved(
        &self,
        group: &SecretGroup,
        title: &str,
    ) -> Result<Option<FetchedItem>, VaultError> {
        // 1) 引用行里的真实 item id 直达。
        if let Some(id) = self.ref_item_id(group) {
            match self.run_op(&self.base_read_args(&id), None) {
                Ok(raw) if !is_archived_item(&raw) => {
                    let bundle = parse_item_bundle(&raw)?;
                    return Ok(Some(FetchedItem {
                        bundle,
                        item_id: None,
                        raw,
                    }));
                }
                // id 失效（条目被删/已归档/换 vault）：按标题兜底一次。
                Ok(_) | Err(RunErr::NotFound) => {}
                Err(RunErr::Vault(e)) => return Err(e),
            }
        }
        // 2) 标题兜底（按标题不会命中归档区，无需再查状态）。
        match self.run_op(&self.base_read_args(title), None) {
            Ok(raw) => {
                let bundle = parse_item_bundle(&raw)?;
                Ok(Some(FetchedItem {
                    bundle,
                    item_id: parse_item_id(&raw),
                    raw,
                }))
            }
            Err(RunErr::NotFound) => Ok(None),
            // 3) 同标题多条：列条目取最新的一条按 id 读取。
            Err(RunErr::Vault(VaultError::ItemConflict)) => {
                let Some(id) = self.resolve_conflicting_item_id(title)? else {
                    return Ok(None);
                };
                match self.run_op(&self.base_read_args(&id), None) {
                    Ok(raw) if !is_archived_item(&raw) => {
                        let bundle = parse_item_bundle(&raw)?;
                        Ok(Some(FetchedItem {
                            bundle,
                            item_id: Some(id),
                            raw,
                        }))
                    }
                    Ok(_) | Err(RunErr::NotFound) => Ok(None),
                    Err(RunErr::Vault(e)) => Err(e),
                }
            }
            Err(RunErr::Vault(e)) => Err(e),
        }
    }
}

/// F2-2：一次成功定位的条目。
struct FetchedItem {
    bundle: SecretBundle,
    /// 需要回写 `secret_refs` 的 item id（`None` = 引用直达命中，无需回写）。
    item_id: Option<String>,
    /// 成功那次 `op item get --format json` 的 stdout 原文。
    raw: Zeroizing<Vec<u8>>,
}

/// `op` 调用抽象（F0-1）：`OnePasswordVault` 经它执行 `op`，生产实现是
/// [`ProcessOpRunner`]（真实子进程），测试用 `FakeOpRunner` 脚本化——
/// put 原子性、item_id 优先、「命令行不含任何值」这些关键性质由此可单测。
pub(crate) trait OpRunner: Send + Sync {
    /// 执行一次 `op`；`stdin` 需要时以管道写入。返回 stdout（含秘密，永不写日志）。
    /// `take_lock`：可能弹授权的命令（item 子命令、`vault list`）必须串行（true）；
    /// 非交互命令（`account list`、`--version`，不会弹授权）传 false，避免一次等解锁
    /// 的调用把状态探测卡在全局锁上（F4-3）。
    fn run(
        &self,
        args: &[&str],
        stdin: Option<&[u8]>,
        take_lock: bool,
    ) -> Result<Zeroizing<Vec<u8>>, RunErr>;
}

/// 生产实现：真实拉起 `op` 子进程（复用 [`exec_op`]）。
struct ProcessOpRunner {
    op_path: PathBuf,
}

impl ProcessOpRunner {
    fn new(op_path: PathBuf) -> Self {
        Self { op_path }
    }
}

impl OpRunner for ProcessOpRunner {
    fn run(
        &self,
        args: &[&str],
        stdin: Option<&[u8]>,
        take_lock: bool,
    ) -> Result<Zeroizing<Vec<u8>>, RunErr> {
        exec_op(&self.op_path, args, stdin, take_lock)
    }
}

/// 执行一次 `op`；`take_lock` 为真时进程全局串行（并发授权弹窗会叠加，§12.6）。
/// `stdin` 需要时管道写入后立即关闭。返回 stdout（`Zeroizing`，解析后立即丢弃，
/// 永不写日志）。
fn exec_op(
    op_path: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
    take_lock: bool,
) -> Result<Zeroizing<Vec<u8>>, RunErr> {
    let _guard = take_lock.then(|| OP_LOCK.lock().unwrap_or_else(|e| e.into_inner()));

    let mut cmd = Command::new(op_path);
    cmd.args(args);
    // 不让 op 在磁盘缓存条目；清掉可能悄悄切换认证方式的环境变量（§12.11）。
    cmd.env("OP_CACHE", "false");
    cmd.env_remove("OP_SERVICE_ACCOUNT_TOKEN");
    cmd.env_remove("OP_CONNECT_HOST");
    cmd.env_remove("OP_CONNECT_TOKEN");
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            RunErr::Vault(VaultError::NotInstalled)
        } else {
            RunErr::Vault(VaultError::Other(format!("spawn op failed: {}", e.kind())))
        }
    })?;

    if let Some(data) = stdin {
        if let Some(mut pipe) = child.stdin.take() {
            let _ = pipe.write_all(data);
            // pipe 在此 drop，关闭 stdin。
        }
    }

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let out_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let err_handle = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = pipe.read_to_string(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + OP_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RunErr::Vault(VaultError::Timeout));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return Err(RunErr::Vault(VaultError::Other(format!(
                    "wait op failed: {}",
                    e.kind()
                ))));
            }
        }
    };

    let stdout = out_handle.join().unwrap_or_default();
    let stderr = err_handle.join().unwrap_or_default();

    if status.success() {
        return Ok(Zeroizing::new(stdout));
    }
    // 调试开关（默认关）：设 CC_SWITCH_OP_DEBUG=1 时把原始 stderr 透出，便于本机排障。
    // 正常运行不会走这里（§12.4：stderr 不入日志）。
    if std::env::var("CC_SWITCH_OP_DEBUG").is_ok() {
        eprintln!("[op-debug] args={args:?}\n[op-debug] stderr={stderr}");
    }
    Err(classify_stderr(&stderr))
}

/// 1Password 账户（`op account list` 行）。前端面向结构：`account_uuid` 已解析。
#[derive(Debug, Clone, Serialize)]
pub struct OpAccount {
    pub url: String,
    pub email: String,
    pub account_uuid: String,
}

/// op 原始输出：不同版本可能同时包含 `account_uuid` 与 `user_uuid`，
/// 不能用 serde alias（两者共存时会报 duplicate field），故分开接收再解析。
#[derive(Debug, Clone, Deserialize)]
struct OpAccountRaw {
    #[serde(default)]
    url: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    account_uuid: String,
    #[serde(default)]
    user_uuid: String,
}

impl From<OpAccountRaw> for OpAccount {
    fn from(r: OpAccountRaw) -> Self {
        // `--account` 接受账户 UUID 或用户 UUID；优先账户 UUID，缺失时回落用户 UUID。
        let account_uuid = if !r.account_uuid.is_empty() {
            r.account_uuid
        } else {
            r.user_uuid
        };
        OpAccount {
            url: r.url,
            email: r.email,
            account_uuid,
        }
    }
}

/// 1Password vault（`op vault list` 行）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpVault {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

/// F2-2：`op item list --format json` 行（解析同标题冲突 / F3-8 孤儿清理用；
/// 只要 id / 标题 / 更新时间，不含任何值）。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct OpItemListEntry {
    #[serde(default)]
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) title: String,
    #[serde(default, rename = "updatedAt")]
    pub(crate) updated_at: String,
}

/// 列出账户（不需解锁）。F4-3：不抢全局锁。
pub fn list_accounts(op_path: &Path) -> Result<Vec<OpAccount>, VaultError> {
    let out = exec_op(
        op_path,
        &["account", "list", "--format", "json", "--no-color"],
        None,
        false,
    )
    .map_err(RunErr::into_vault)?;
    let raw: Vec<OpAccountRaw> = serde_json::from_slice(&out)
        .map_err(|e| VaultError::Other(format!("parse accounts failed: {e}")))?;
    Ok(raw.into_iter().map(OpAccount::from).collect())
}

/// 列出 vault（需解锁：会触发授权弹窗）。
pub fn list_vaults(op_path: &Path, account: &str) -> Result<Vec<OpVault>, VaultError> {
    let out = exec_op(
        op_path,
        &[
            "vault",
            "list",
            "--account",
            account,
            "--format",
            "json",
            "--no-color",
        ],
        None,
        true,
    )
    .map_err(RunErr::into_vault)?;
    serde_json::from_slice(&out).map_err(|e| VaultError::Other(format!("parse vaults failed: {e}")))
}

/// 启动页状态探测（不需解锁）：定位 op + 版本 + 是否已登录。
pub struct OpProbe {
    pub installed: bool,
    pub op_path: Option<String>,
    pub version: Option<String>,
    pub signed_in: bool,
    pub signature_ok: Option<bool>,
}

pub fn probe(configured_path: Option<&str>, verify_signature: bool) -> OpProbe {
    let Some(path) = locate_op(configured_path) else {
        return OpProbe {
            installed: false,
            op_path: None,
            version: None,
            signed_in: false,
            signature_ok: None,
        };
    };
    let signature = if verify_signature {
        Some(verify_op_signature(&path))
    } else {
        None
    };
    let signature_ok = signature.as_ref().map(|r| r.is_ok());
    // F1-7（P0-7）：签名校验失败绝不执行 op 二进制（它可能是被篡改的仿冒品），
    // 其余字段一律为空，UI 据此提示修复而不是继续配置。
    let (version, signed_in) = if signature.as_ref().is_some_and(|r| r.is_err()) {
        (None, false)
    } else {
        (
            op_version(&path),
            list_accounts(&path).map(|a| !a.is_empty()).unwrap_or(false),
        )
    };
    OpProbe {
        installed: true,
        op_path: Some(path.to_string_lossy().to_string()),
        version,
        signed_in,
        signature_ok,
    }
}

/// 把某组映射成 1Password 条目标题（§5.3）。标题由 CC Switch 规则命名，作为唯一定位键。
fn item_title(group: &SecretGroup) -> String {
    match group {
        SecretGroup::Provider { app, provider_id } => {
            format!("cc-switch/{}/{}", app.as_str(), provider_id)
        }
        SecretGroup::AppSync => "cc-switch/app/sync".to_string(),
    }
}

/// F4-5：[`item_title`] 的逆运算——把条目标题解析回所属组。
/// 「从 1Password 重建引用」按标题识别哪些条目属于 CC Switch；解析失败（用户手工
/// 建的同前缀条目等）返回 `None`，重建时跳过。
pub fn parse_group_from_title(title: &str) -> Option<SecretGroup> {
    let rest = title.strip_prefix("cc-switch/")?;
    if rest == "app/sync" {
        return Some(SecretGroup::AppSync);
    }
    let (app_str, provider_id) = rest.split_once('/')?;
    if provider_id.is_empty() {
        return None;
    }
    let app = app_str.parse::<AppType>().ok()?;
    Some(SecretGroup::provider(app, provider_id.to_string()))
}

/// 该字段名是否属于 CC Switch 管理的秘密字段（过滤掉 op 默认字段与 schema 标记）。
fn is_managed_field(label: &str) -> bool {
    label == FIELD_API_KEY
        || label == FIELD_BASE_URL
        || label.starts_with(FIELD_ENV_PREFIX)
        || label.starts_with(FIELD_APP_PREFIX)
}

/// F2-1 / 陷阱 9：item_id 是否为「占位形式」——空串、或 v20 回填与旧后端写入的
/// `provider/<app>/<id>` / `app/sync`（即 [`SecretGroup::key`]）。这些不能当作
/// 1P 的 item id 使用。
fn is_placeholder_item_id(item_id: &str, group: &SecretGroup) -> bool {
    item_id.is_empty() || item_id == group.key()
}

/// 真机实测（op 2.39）：`op item get <id>` 会命中归档区条目，JSON 顶层带
/// `"state":"ARCHIVED"`。归档 = 已删除，读取按 NotFound 处理。
fn is_archived_item(raw: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| {
            v.get("state")
                .and_then(|s| s.as_str())
                .map(|s| s.eq_ignore_ascii_case("ARCHIVED"))
        })
        .unwrap_or(false)
}

// ─── op 条目 JSON（读） ──────────────────────────────────────

#[derive(Deserialize)]
struct OpItemRead {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    fields: Vec<OpFieldRead>,
}

#[derive(Deserialize)]
struct OpFieldRead {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    value: Option<String>,
}

/// 把 `op item get --format json` 的输出解析成整包（只取 CC Switch 管理的字段）。
/// F4-2：空字符串值视为「无此字段」——否则会把空钥匙注入 live 配置（P2-2）。
fn parse_item_bundle(bytes: &[u8]) -> Result<SecretBundle, VaultError> {
    let item: OpItemRead = serde_json::from_slice(bytes)
        .map_err(|e| VaultError::Other(format!("parse op item failed: {e}")))?;
    let mut bundle = SecretBundle::new();
    for field in item.fields {
        let (Some(label), Some(value)) = (field.label, field.value) else {
            continue;
        };
        if !is_managed_field(&label) || value.is_empty() {
            continue;
        }
        bundle.insert(label, Zeroizing::new(value));
    }
    Ok(bundle)
}

/// F4-5：从条目 JSON 提取托管字段 label 清单（不含值）。「从 1Password 重建引用」
/// 用——`op item get` 不带 `--reveal` 时 CONCEALED 字段没有值，只有 label 可靠。
fn parse_item_labels(bytes: &[u8]) -> Result<Vec<String>, VaultError> {
    let item: OpItemRead = serde_json::from_slice(bytes)
        .map_err(|e| VaultError::Other(format!("parse op item failed: {e}")))?;
    Ok(item
        .fields
        .into_iter()
        .filter_map(|f| f.label)
        .filter(|l| is_managed_field(l) || l == SCHEMA_FIELD_LABEL)
        .collect())
}

/// 从 `op item get` 输出解析条目 id（写回 secret_refs 的 item_id）。
fn parse_item_id(bytes: &[u8]) -> Option<String> {
    serde_json::from_slice::<OpItemRead>(bytes)
        .ok()
        .and_then(|i| i.id)
}

// ─── op 条目模板 JSON（写，走 stdin） ────────────────────────

#[derive(Serialize)]
struct OpItemTemplate<'a> {
    title: &'a str,
    category: &'static str,
    tags: &'a [&'static str],
    fields: Vec<OpFieldTemplate<'a>>,
}

#[derive(Serialize)]
struct OpFieldTemplate<'a> {
    label: &'a str,
    #[serde(rename = "type")]
    field_type: &'static str,
    value: &'a str,
}

/// 构造 create 用的条目模板 JSON（stdin 管道）。所有秘密字段 CONCEALED，
/// 另加一个非秘密 STRING 标记字段。绝不把值放进命令行参数（§3.4 / §12.3）。
/// F4-2：字段名/值用 `&str` 借用（不留多余 String 副本）；序列化结果用
/// `Zeroizing<Vec<u8>>` 承载（含钥匙的明文字节）。
fn build_template_json(
    title: &str,
    bundle: &SecretBundle,
) -> Result<Zeroizing<Vec<u8>>, VaultError> {
    const TAGS: [&str; 1] = [OP_TAG];
    let mut fields: Vec<OpFieldTemplate> = bundle
        .iter()
        .map(|(label, value)| OpFieldTemplate {
            label: label.as_str(),
            field_type: "CONCEALED",
            value: value.as_str(),
        })
        .collect();
    fields.push(OpFieldTemplate {
        label: SCHEMA_FIELD_LABEL,
        field_type: "STRING",
        value: SCHEMA_FIELD_VALUE,
    });
    let template = OpItemTemplate {
        title,
        category: OP_CATEGORY,
        tags: &TAGS,
        fields,
    };
    serde_json::to_vec(&template)
        .map(Zeroizing::new)
        .map_err(|e| VaultError::Other(format!("serialize op template failed: {e}")))
}

/// F1-1：在 `op item get` 返回的条目 JSON 上**就地**应用目标整包（edit 的 stdin 输入）。
///
/// - 非托管字段（op 默认字段、用户自加字段）原样保留；
/// - 托管字段（[`is_managed_field`]）按目标整包设置值（类型 CONCEALED）；
/// - 目标里没有的托管字段**不包含**在编辑输入里——若 op 的管道编辑是「合并」语义，
///   残留字段由调用方用 `'<label>[delete]'` 定点删除（参数里只有字段名，§12.3）；
/// - 确保 `cc-switch-schema` 标记字段存在。
fn apply_managed_fields_to_item(item: &mut serde_json::Value, bundle: &SecretBundle) {
    let Some(fields) = item.get_mut("fields").and_then(|f| f.as_array_mut()) else {
        return;
    };
    // 先更新已存在的托管字段，非托管字段不动。
    for field in fields.iter_mut() {
        let Some(label) = field
            .get("label")
            .and_then(|l| l.as_str())
            .map(str::to_string)
        else {
            continue;
        };
        if let Some(value) = bundle.get(&label) {
            field["type"] = serde_json::Value::String("CONCEALED".to_string());
            field["value"] = serde_json::Value::String(value.to_string());
        }
    }
    // 追加目标里有、条目里没有的托管字段。
    let existing_labels: Vec<String> = fields
        .iter()
        .filter_map(|f| f.get("label").and_then(|l| l.as_str()).map(str::to_string))
        .collect();
    for (label, value) in bundle.iter() {
        if !existing_labels.iter().any(|l| l == label) {
            fields.push(serde_json::json!({
                "label": label,
                "type": "CONCEALED",
                "value": value.to_string(),
            }));
        }
    }
    // schema 标记字段。
    if !existing_labels.iter().any(|l| l == SCHEMA_FIELD_LABEL) {
        fields.push(serde_json::json!({
            "label": SCHEMA_FIELD_LABEL,
            "type": "STRING",
            "value": SCHEMA_FIELD_VALUE,
        }));
    }
}

/// 从条目 JSON 提取「托管字段 label 集合」（校验 edit 结果用）。
fn managed_labels_of(item: &serde_json::Value) -> Vec<String> {
    item.get("fields")
        .and_then(|f| f.as_array())
        .map(|fields| {
            fields
                .iter()
                .filter_map(|f| f.get("label").and_then(|l| l.as_str()).map(str::to_string))
                .filter(|l| is_managed_field(l))
                .collect()
        })
        .unwrap_or_default()
}

/// F1-1：edit 之后校验「托管字段集合 == 目标集合」。op 的管道编辑若为合并语义，
/// 目标里已删除的托管字段会残留——对每个残留字段发一次 `'<label>[delete]'`，
/// 参数里只有字段名、没有值（§12.3），保证整包语义最终成立。
fn leftover_managed_labels(edited: &serde_json::Value, bundle: &SecretBundle) -> Vec<String> {
    managed_labels_of(edited)
        .into_iter()
        .filter(|label| label != SCHEMA_FIELD_LABEL && !bundle.contains(label))
        .collect()
}

// ─── stderr 分类（§5.2；关键字以本机实测为准，样本入单测） ───

/// F4-1（P2-1）：把 stderr 中成对引号里的内容替换成占位符——op 会在这些位置回显
/// 条目标题、vault 名等用户可控字符串，子串匹配会被它们带偏（如 id 为
/// `unlocked-proxy` 的条目不存在时，`locked` 子串会让「条目不存在」被判成 Locked）。
///
/// 双引号一律成对剔除；单引号只在「词边界开始、词边界结束」时成对剔除，
/// 避免 "isn't" 这类撇号把 not-found 关键字夹进占位符里。
fn mask_quoted_spans(stderr: &str) -> String {
    let chars: Vec<char> = stderr.chars().collect();
    let mut out = String::with_capacity(stderr.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let close = match c {
            '"' => chars[i + 1..]
                .iter()
                .position(|&x| x == '"')
                .map(|p| p + i + 1),
            '\'' => {
                // 开引号必须是词边界（前一个字符不是字母数字）。
                let opens_at_boundary = i == 0 || !chars[i - 1].is_alphanumeric();
                if opens_at_boundary {
                    chars[i + 1..]
                        .iter()
                        .position(|&x| x == '\'')
                        .map(|p| p + i + 1)
                        .filter(|&close| {
                            // 闭引号也必须是词边界（后一个字符不是字母数字）。
                            close + 1 >= chars.len() || !chars[close + 1].is_alphanumeric()
                        })
                } else {
                    None
                }
            }
            _ => None,
        };
        match close {
            Some(close) => {
                out.push('"');
                out.push('…');
                out.push('"');
                i = close + 1;
            }
            None => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn classify_stderr(stderr: &str) -> RunErr {
    // F4-1：先剔除引号回显再匹配关键字（§12.6 陷阱：stderr 会回显条目名与 vault 名）。
    let lower = mask_quoted_spans(stderr).to_lowercase();
    let has = |needle: &str| lower.contains(needle);

    if has("not currently signed in")
        || has("no accounts configured")
        || has("no account found")
        || has("found no accounts")
        || has("no accounts for filter")
    {
        return RunErr::Vault(VaultError::NotSignedIn);
    }
    // F4-1：不再匹配裸 "locked"（会被回显的条目名误命中），只认明确短语。
    if has("authorization prompt dismissed")
        || has("authorization timeout")
        || has("is not unlocked")
        || has("account is not unlocked")
        || has("is locked")
        || has("cannot connect to 1password")
        || has("connecting to desktop app")
        || has("make sure it is running")
    {
        return RunErr::Vault(VaultError::Locked);
    }
    // not-found 必须同时满足退出码非零（已由调用点保证）与关键字（§12.12）。
    // 注意：“isn't a vault” 是 vault 配错（配置错误），不归 not-found——否则会被当成“没钥匙”静默刷过。
    if has("isn't an item")
        || has("no item matching")
        || has("not found")
        || has("doesn't seem to be an item")
    {
        return RunErr::NotFound;
    }
    if has("dial tcp")
        || has("no such host")
        || has("connection refused")
        || has("network is unreachable")
        || has("timeout")
        || has("i/o timeout")
    {
        return RunErr::Vault(VaultError::Network);
    }
    if has("conflict") || has("already exists") || has("more than one item matches") {
        return RunErr::Vault(VaultError::ItemConflict);
    }
    // 兜底：只记「有 stderr」这一事实，绝不原样记录（stderr 可能回显条目名）。
    RunErr::Vault(VaultError::Other("unclassified op error".to_string()))
}

impl SecretVault for OnePasswordVault {
    fn fetch(&self, group: &SecretGroup) -> Result<Option<SecretBundle>, VaultError> {
        let title = item_title(group);
        match self.fetch_item_resolved(group, &title)? {
            None => Ok(None),
            Some(fetched) => {
                // F2-2：标题兜底 / 冲突取最新命中条目后，把真实 item id 回写引用行。
                if let Some(id) = &fetched.item_id {
                    self.repair_ref(group, id, &fetched.bundle);
                }
                Ok(Some(fetched.bundle))
            }
        }
    }

    fn put(&self, group: &SecretGroup, bundle: &SecretBundle) -> Result<VaultRef, VaultError> {
        let title = item_title(group);

        // F1-1（P0-2）：覆盖写改为原子的「读取 → 就地编辑」，不再先归档删除再新建。
        // 旧实现（delete + create）非原子：删除成功、新建失败会把条目留在归档里
        // （CCS 视为没钥匙），每次覆盖写还在归档里多留一份旧钥匙副本，item id 也
        // 每次都变。`op item edit` 支持管道 JSON（本机 op 2.39 实测），值走 stdin，
        // 不进命令行（§12.3）。写操作仍是 get + edit 两次 op。
        // F2-2：读取定位按「id 直达 → 标题兜底 → 同名冲突取最新」。
        let fetched = self.fetch_item_resolved(group, &title)?;

        let item_id = match fetched {
            None => {
                // 新建：create 以 `-` 作为位置参数，模板 JSON 走 stdin。
                let template = build_template_json(&title, bundle)?;
                let create_args = [
                    "item",
                    "create",
                    "--vault",
                    &self.vault,
                    "--account",
                    &self.account,
                    "--format",
                    "json",
                    "--no-color",
                    "-",
                ];
                let out = self
                    .run_op(&create_args, Some(template.as_slice()))
                    .map_err(RunErr::into_vault)?;
                parse_item_id(&out).unwrap_or_default()
            }
            Some(fetched) => {
                // 已存在：定位到的 item id（标题兜底命中时先回写引用行）。
                let item_id = match fetched.item_id {
                    Some(id) => {
                        self.repair_ref(group, &id, &fetched.bundle);
                        id
                    }
                    None => self.ref_item_id(group).ok_or_else(|| {
                        VaultError::Other("op 条目缺少 id，无法就地编辑".to_string())
                    })?,
                };
                // 在条目 JSON 原文上就地改托管字段后整份走 stdin edit。
                let mut item: serde_json::Value = serde_json::from_slice(&fetched.raw)
                    .map_err(|e| VaultError::Other(format!("parse op item failed: {e}")))?;
                apply_managed_fields_to_item(&mut item, bundle);
                let edited_input =
                    Zeroizing::new(serde_json::to_vec(&item).map_err(|e| {
                        VaultError::Other(format!("serialize op item failed: {e}"))
                    })?);
                // edit 后立即 drop 反序列化用的 Value（内存卫生）。
                drop(item);
                let edit_args = [
                    "item",
                    "edit",
                    item_id.as_str(),
                    "--vault",
                    &self.vault,
                    "--account",
                    &self.account,
                    "--format",
                    "json",
                    "--no-color",
                    "-",
                ];
                let out = self
                    .run_op(&edit_args, Some(&edited_input))
                    .map_err(RunErr::into_vault)?;
                // 校验托管字段集合 == 目标集合；合并语义残留用 `[delete]` 定点删。
                let edited: serde_json::Value = serde_json::from_slice(&out)
                    .map_err(|e| VaultError::Other(format!("parse op item failed: {e}")))?;
                for label in leftover_managed_labels(&edited, bundle) {
                    let delete_field_arg = format!("{label}[delete]");
                    let delete_args = [
                        "item",
                        "edit",
                        item_id.as_str(),
                        "--vault",
                        &self.vault,
                        "--account",
                        &self.account,
                        delete_field_arg.as_str(),
                        "--no-color",
                    ];
                    self.run_op(&delete_args, None)
                        .map_err(RunErr::into_vault)?;
                }
                item_id
            }
        };
        Ok(VaultRef {
            vault_id: self.vault.clone(),
            item_id,
            fields: bundle.field_names(),
        })
    }

    fn delete(&self, group: &SecretGroup) -> Result<(), VaultError> {
        let title = item_title(group);

        // F2-2：id 直达，失效时按标题兜底一次；同标题多条取最新的一条按 id 删。
        // 已不存在视为删除成功（幂等）。
        if let Some(id) = self.ref_item_id(group) {
            let args = self.base_delete_args(&id);
            match self.run_op(&args, None) {
                Ok(_) => return Ok(()),
                Err(RunErr::NotFound) => {}
                Err(RunErr::Vault(e)) => return Err(e),
            }
        }
        let args = self.base_delete_args(title.as_str());
        match self.run_op(&args, None) {
            Ok(_) => Ok(()),
            Err(RunErr::NotFound) => Ok(()),
            Err(RunErr::Vault(VaultError::ItemConflict)) => {
                let Some(id) = self.resolve_conflicting_item_id(&title)? else {
                    return Ok(());
                };
                let args = self.base_delete_args(&id);
                match self.run_op(&args, None) {
                    Ok(_) => Ok(()),
                    Err(RunErr::NotFound) => Ok(()),
                    Err(RunErr::Vault(e)) => Err(e),
                }
            }
            Err(RunErr::Vault(e)) => Err(e),
        }
    }

    fn status(&self) -> VaultStatus {
        // 不需解锁、不联网或极快：判断「已安装 + 已配置账户」。
        // F4-3：`account list` 不会弹授权，不抢全局锁——一次等解锁的 item 调用
        // 不该把状态查询卡住最长 120 秒。
        let args = ["account", "list", "--format", "json", "--no-color"];
        match self.runner.run(&args, None, false) {
            Ok(bytes) => {
                let accounts: Vec<serde_json::Value> =
                    serde_json::from_slice(&bytes).unwrap_or_default();
                if accounts.is_empty() {
                    VaultStatus::NotSignedIn
                } else {
                    VaultStatus::Ready
                }
            }
            Err(RunErr::Vault(VaultError::NotInstalled)) => VaultStatus::NotInstalled,
            Err(RunErr::Vault(VaultError::NotSignedIn)) => VaultStatus::NotSignedIn,
            Err(RunErr::NotFound) => VaultStatus::NotSignedIn,
            Err(RunErr::Vault(e)) => VaultStatus::Unknown(e.code().to_string()),
        }
    }

    fn backend_name(&self) -> &'static str {
        "1password"
    }

    fn vault_id(&self) -> String {
        self.vault.clone()
    }
}

/// 校验 op.exe 签名（D8 / §5.5）：WinVerifyTrust 确认 Authenticode 签名可信，
/// 再确认**签名者证书**（链首）的 O 字段就是 AgileBits。任一失败拒用（op.exe 是
/// 整个方案的信任根）。
///
/// F1-7（P0-7）：只认签名者证书，不再遍历证书包里的所有证书——夹带一张名字好看的
/// 证书即可绕过子串校验；主体判定只认 O 完整等于 AgileBits，不再做
/// "agilebits"/"1password" 子串匹配。
#[cfg(windows)]
pub fn verify_op_signature(path: &Path) -> Result<(), VaultError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::Cryptography::{
        szOID_ORGANIZATION_NAME, CertGetNameStringW, CERT_NAME_ATTR_TYPE,
    };
    use windows_sys::Win32::Security::WinTrust::{
        WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData, WinVerifyTrust,
        WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_FILE_INFO, WTD_CHOICE_FILE,
        WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
    };

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: 结构体按 Win32 约定填充，wide 指针在调用期间有效。
    let signer_org: Option<String> = unsafe {
        let mut file_info: WINTRUST_FILE_INFO = std::mem::zeroed();
        file_info.cbStruct = std::mem::size_of::<WINTRUST_FILE_INFO>() as u32;
        file_info.pcwszFilePath = wide.as_ptr();

        let mut wtd: WINTRUST_DATA = std::mem::zeroed();
        wtd.cbStruct = std::mem::size_of::<WINTRUST_DATA>() as u32;
        wtd.dwUIChoice = WTD_UI_NONE;
        // 保持 WTD_REVOKE_NONE（不联网查吊销）：签名来自 Azure Trusted Signing，
        // 证书短期轮换；联网吊销检查会让每次启动都发网络请求，且吊销并非现实攻击面
        // （信任根是「签名者 O = AgileBits」这一主体判定本身）。
        wtd.fdwRevocationChecks = WTD_REVOKE_NONE;
        wtd.dwUnionChoice = WTD_CHOICE_FILE;
        wtd.dwStateAction = WTD_STATEACTION_VERIFY;
        wtd.Anonymous.pFile = &mut file_info;

        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        let status = WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            (&mut wtd as *mut WINTRUST_DATA).cast(),
        );

        // 1) 签名有效且链到可信根。失败即拒用（先关闭状态数据再返回）。
        if status != 0 {
            wtd.dwStateAction = WTD_STATEACTION_CLOSE;
            WinVerifyTrust(
                std::ptr::null_mut(),
                &mut action,
                (&mut wtd as *mut WINTRUST_DATA).cast(),
            );
            return Err(VaultError::Other(
                "op.exe 签名无效或不受信任，已拒绝使用".to_string(),
            ));
        }

        // 2) 从 state data 取**签名者证书**（链首），读它的 O 字段。
        //    取完后再 WTD_STATEACTION_CLOSE。
        let mut org = None;
        let prov = WTHelperProvDataFromStateData(wtd.hWVTStateData);
        if !prov.is_null() {
            let sgnr = WTHelperGetProvSignerFromChain(prov, 0, 0, 0);
            if !sgnr.is_null() && (*sgnr).csCertChain > 0 && !(*sgnr).pasCertChain.is_null() {
                let cert = (*(*sgnr).pasCertChain).pCert;
                if !cert.is_null() {
                    // 先取所需缓冲长度，再取字符串。
                    let len = CertGetNameStringW(
                        cert,
                        CERT_NAME_ATTR_TYPE,
                        0,
                        szOID_ORGANIZATION_NAME.cast(),
                        std::ptr::null_mut(),
                        0,
                    );
                    if len > 1 {
                        let mut buf = vec![0u16; len as usize];
                        CertGetNameStringW(
                            cert,
                            CERT_NAME_ATTR_TYPE,
                            0,
                            szOID_ORGANIZATION_NAME.cast(),
                            buf.as_mut_ptr(),
                            len,
                        );
                        // 返回长度含结尾 NUL（len = 字符数 + 1），剥掉它再判定。
                        org = Some(
                            String::from_utf16_lossy(&buf)
                                .trim_end_matches('\0')
                                .to_string(),
                        );
                    }
                }
            }
        }
        wtd.dwStateAction = WTD_STATEACTION_CLOSE;
        WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            (&mut wtd as *mut WINTRUST_DATA).cast(),
        );
        org
    };
    let Some(org) = signer_org else {
        return Err(VaultError::Other(
            "无法读取 op.exe 的签名者证书，已拒绝使用".to_string(),
        ));
    };
    if !is_trusted_signer_org(&org) {
        return Err(VaultError::Other(
            "op.exe 签名者不是 AgileBits，已拒绝使用".to_string(),
        ));
    }
    Ok(())
}

/// F1-7：签名者证书 O 字段判定（纯函数，便于单测）。必须完整等于 AgileBits
/// （忽略大小写与首尾空白）。不做 "agilebits"/"1password" 子串匹配——
/// 子串会被夹带证书绕过（如 "O=Evil, CN=1Password Fake"）。
pub(crate) fn is_trusted_signer_org(org: &str) -> bool {
    org.trim().eq_ignore_ascii_case("agilebits")
}

/// 非 Windows 平台不做签名校验（本项目仅面向 Windows；留桩便于跨平台编译）。
#[cfg(not(windows))]
pub fn verify_op_signature(_path: &Path) -> Result<(), VaultError> {
    Ok(())
}

/// 探测 `op` 版本（诊断用）。不需解锁。F4-3：不抢全局锁。
/// F1-7：改走 `exec_op`（环境变量清理 + 超时），不再裸起进程。
pub fn op_version(op_path: &Path) -> Option<String> {
    let out = exec_op(op_path, &["--version"], None, false).ok()?;
    Some(String::from_utf8_lossy(&out).trim().to_string())
}

/// 供 §4.2 构造：读取 1Password 设置并定位 op。缺任一项则返回错误。
pub fn from_settings(
    db: std::sync::Arc<crate::database::Database>,
) -> Result<OnePasswordVault, VaultError> {
    let op_path = locate_op(crate::settings::get_onepassword_op_path().as_deref())
        .ok_or(VaultError::NotInstalled)?;
    // D8：默认校验 op.exe 签名（信任根），失败拒用。
    if crate::settings::onepassword_verify_signature() {
        verify_op_signature(&op_path)?;
    }
    let account = crate::settings::get_onepassword_account()
        .ok_or_else(|| VaultError::Other("1Password 账户未配置".to_string()))?;
    let vault = crate::settings::get_onepassword_vault()
        .ok_or_else(|| VaultError::Other("1Password vault 未配置".to_string()))?;
    Ok(OnePasswordVault::new(op_path, account, vault, db))
}

/// §4.2 / §6.7：按当前 `secret_backend` 设置构造运行时保险箱。
/// - `onepassword`：构 OnePasswordVault；构造失败（op 未装/签名不信任/未配置）
///   用 UnavailableVault 占位，让 App 仍能打开（启动不取钥匙，§6.7）。
/// - 其它（缺省 windows）：包旧 store 的 LegacyWindowsVault。
pub fn build_runtime_vault(
    store: std::sync::Arc<dyn crate::secrets::SecretStore>,
    db: std::sync::Arc<crate::database::Database>,
) -> std::sync::Arc<dyn SecretVault> {
    use crate::secrets::vault::{LegacyWindowsVault, UnavailableVault};
    if crate::settings::is_onepassword_backend() {
        match from_settings(db.clone()) {
            Ok(v) => std::sync::Arc::new(v),
            Err(e) => {
                log::warn!(
                    "1Password 后端构造失败（code={}），App 照常打开，取钥匙将报错",
                    e.code()
                );
                std::sync::Arc::new(UnavailableVault::new(e))
            }
        }
    } else {
        std::sync::Arc::new(LegacyWindowsVault::new(store, db))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_config::AppType;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// F0-1 测试替身：按脚本顺序返回预设结果，并记录每次调用的 args 与 stdin
    /// （args 断言用：任何调用的命令行里不得出现 bundle 中的值）。
    pub(crate) struct FakeOpRunner {
        script: Mutex<VecDeque<Result<Vec<u8>, RunErr>>>,
        calls: Mutex<Vec<RecordedCall>>,
    }

    /// 一次被记录的 `op` 调用。
    pub(crate) struct RecordedCall {
        pub(crate) args: Vec<String>,
        pub(crate) stdin: Option<Vec<u8>>,
    }

    impl FakeOpRunner {
        pub(crate) fn new(script: Vec<Result<Vec<u8>, RunErr>>) -> Self {
            Self {
                script: Mutex::new(script.into_iter().collect()),
                calls: Mutex::new(Vec::new()),
            }
        }

        /// 第 n 次（0 起）调用的 args 与 stdin。
        pub(crate) fn call(&self, index: usize) -> (Vec<String>, Option<Vec<u8>>) {
            let call = &self.calls.lock().unwrap()[index];
            (call.args.clone(), call.stdin.clone())
        }

        pub(crate) fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }

        /// 断言辅助：所有已记录调用的 args 拼接文本。
        fn all_args_text(&self) -> String {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|c| c.args.join(" "))
                .collect::<Vec<_>>()
                .join("\n")
        }

        /// F1-1 验收：任何调用的 args 里不出现 bundle 中任何值。
        #[allow(dead_code)]
        pub(crate) fn assert_args_contain_none_of(&self, values: &[&str]) {
            let text = self.all_args_text();
            for value in values {
                assert!(
                    !text.contains(value),
                    "命令行参数中出现了秘密值（违反 §12.3）"
                );
            }
        }
    }

    impl OpRunner for FakeOpRunner {
        fn run(
            &self,
            args: &[&str],
            stdin: Option<&[u8]>,
            _take_lock: bool,
        ) -> Result<Zeroizing<Vec<u8>>, RunErr> {
            self.calls.lock().unwrap().push(RecordedCall {
                args: args.iter().map(|s| s.to_string()).collect(),
                stdin: stdin.map(|b| b.to_vec()),
            });
            self.script
                .lock()
                .unwrap()
                .pop_front()
                .map(|r| r.map(Zeroizing::new))
                .unwrap_or_else(|| Err(RunErr::Vault(VaultError::Other("脚本耗尽".into()))))
        }
    }

    /// 用 FakeOpRunner 构造被测 vault 的便捷函数（db 用内存库，引用行为空）。
    fn vault_with(script: Vec<Result<Vec<u8>, RunErr>>) -> (OnePasswordVault, Arc<FakeOpRunner>) {
        let runner = Arc::new(FakeOpRunner::new(script));
        let db = Arc::new(crate::database::Database::memory().expect("memory db"));
        let vault = OnePasswordVault::with_runner(runner.clone(), "acct", "vault-x", db);
        (vault, runner)
    }

    #[test]
    fn fake_runner_records_calls_and_replays_script() {
        // F0-1 自测：替身按脚本返回、按序记录。
        let (vault, runner) = vault_with(vec![
            Ok(br#"{"id":"i1","fields":[]}"#.to_vec()),
            Err(RunErr::NotFound),
        ]);
        assert!(vault.fetch(&SecretGroup::AppSync).unwrap().is_some());
        assert!(vault.fetch(&SecretGroup::AppSync).unwrap().is_none());
        assert_eq!(runner.call_count(), 2);
        let (args, stdin) = runner.call(0);
        assert_eq!(args[0], "item");
        assert_eq!(args[1], "get");
        assert!(stdin.is_none());
    }

    /// 构造 `op item get` 形态的条目 JSON。
    fn item_json(id: &str, fields: &[(&str, &str, &str)]) -> String {
        let fields: Vec<String> = fields
            .iter()
            .map(|(label, ftype, value)| {
                format!(
                    r#"{{"id":"f-{label}","type":"{ftype}","label":"{label}","value":"{value}"}}"#
                )
            })
            .collect();
        format!(
            r#"{{"id":"{id}","title":"cc-switch/claude/p1","category":"API_CREDENTIAL","version":3,"fields":[{}]}}"#,
            fields.join(",")
        )
    }

    fn sample_bundle() -> SecretBundle {
        let mut bundle = SecretBundle::new();
        bundle.insert(FIELD_API_KEY, Zeroizing::new("sk-new-value".to_string()));
        bundle.insert("env.FOO", Zeroizing::new("foo-new".to_string()));
        bundle
    }

    /// F1-1 验收：条目已存在时，put 的调用序列为 [get, edit]，没有 delete / create；
    /// 非托管字段原样保留、托管字段按目标更新；所有调用的 args 不含任何值。
    #[test]
    fn put_existing_edits_in_place_without_delete_or_create() {
        let existing = item_json(
            "item-42",
            &[
                ("username", "STRING", "ignored-user"),
                ("api_key", "CONCEALED", "sk-old-value"),
            ],
        );
        let (vault, runner) = vault_with(vec![
            Ok(existing.into_bytes()),
            Ok(item_json(
                "item-42",
                &[
                    ("username", "STRING", "ignored-user"),
                    ("api_key", "CONCEALED", "sk-new-value"),
                    ("env.FOO", "CONCEALED", "foo-new"),
                    (SCHEMA_FIELD_LABEL, "STRING", SCHEMA_FIELD_VALUE),
                ],
            )
            .into_bytes()),
        ]);
        let group = SecretGroup::provider(AppType::Claude, "p1");
        let vref = vault.put(&group, &sample_bundle()).expect("put");

        assert_eq!(runner.call_count(), 2, "已存在时只能是 get + edit");
        let (get_args, _) = runner.call(0);
        assert_eq!(
            (get_args[0].as_str(), get_args[1].as_str()),
            ("item", "get")
        );
        let (edit_args, edit_stdin) = runner.call(1);
        assert_eq!(
            (edit_args[0].as_str(), edit_args[1].as_str()),
            ("item", "edit")
        );
        assert_eq!(edit_args[2], "item-42", "编辑按 item id 定位");
        // item id 保持不变。
        assert_eq!(vref.item_id, "item-42");
        // stdin 里：非托管字段保留、托管字段更新、schema 标记存在。
        let input: serde_json::Value =
            serde_json::from_slice(&edit_stdin.expect("edit stdin")).unwrap();
        let by_label = |label: &str| {
            input["fields"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["label"] == label)
                .map(|f| {
                    (
                        f["type"].as_str().unwrap().to_string(),
                        f["value"].as_str().unwrap().to_string(),
                    )
                })
        };
        assert_eq!(
            by_label("username"),
            Some(("STRING".into(), "ignored-user".into())),
            "非托管字段必须原样保留"
        );
        assert_eq!(
            by_label("api_key"),
            Some(("CONCEALED".into(), "sk-new-value".into()))
        );
        assert!(by_label("env.FOO").is_some(), "新字段必须追加");
        assert!(
            by_label(SCHEMA_FIELD_LABEL).is_some(),
            "schema 标记必须存在"
        );
        // 命令行参数中绝不出现任何值。
        runner.assert_args_contain_none_of(&["sk-new-value", "sk-old-value", "foo-new"]);
    }

    /// F1-1 验收：条目不存在时序列为 [get(NotFound), create]。
    #[test]
    fn put_missing_creates_without_delete() {
        let (vault, runner) = vault_with(vec![
            Err(RunErr::NotFound),
            Ok(item_json("item-new", &[(FIELD_API_KEY, "CONCEALED", "sk-new-value")]).into_bytes()),
        ]);
        let group = SecretGroup::provider(AppType::Claude, "p1");
        let vref = vault.put(&group, &sample_bundle()).expect("put");
        assert_eq!(runner.call_count(), 2);
        let (args0, _) = runner.call(0);
        assert_eq!(args0[1], "get");
        let (args1, _) = runner.call(1);
        assert_eq!(args1[1], "create");
        assert_eq!(vref.item_id, "item-new");
        runner.assert_args_contain_none_of(&["sk-new-value", "foo-new"]);
    }

    /// F1-1 验收：edit 失败时返回 Err 且没有发出任何 delete（不存在半截成功）。
    #[test]
    fn put_edit_failure_leaves_no_delete() {
        let existing = item_json("item-42", &[(FIELD_API_KEY, "CONCEALED", "sk-old-value")]);
        let (vault, runner) = vault_with(vec![
            Ok(existing.into_bytes()),
            Err(RunErr::Vault(VaultError::Locked)),
        ]);
        let group = SecretGroup::provider(AppType::Claude, "p1");
        let err = vault.put(&group, &sample_bundle()).expect_err("edit 失败");
        assert_eq!(err.code(), "vault_locked");
        assert_eq!(runner.call_count(), 2, "失败后不得追加任何调用");
        for i in 0..runner.call_count() {
            let (args, _) = runner.call(i);
            assert!(
                !args.contains(&"delete".to_string()),
                "任何步骤都不得 delete"
            );
        }
    }

    /// F1-1 验收：若 op 的管道编辑是「合并」语义（目标删除的字段残留），
    /// 对每个残留托管字段追加一次 `'<label>[delete]'`——参数只有字段名，没有值。
    #[test]
    fn put_deletes_leftover_fields_when_edit_merges() {
        let existing = item_json(
            "item-42",
            &[
                (FIELD_API_KEY, "CONCEALED", "sk-old-value"),
                ("env.OLD", "CONCEALED", "old-env"),
            ],
        );
        // edit 返回值模拟合并语义：env.OLD 残留。
        let merged = item_json(
            "item-42",
            &[
                (FIELD_API_KEY, "CONCEALED", "sk-new-value"),
                ("env.FOO", "CONCEALED", "foo-new"),
                ("env.OLD", "CONCEALED", "old-env"),
                (SCHEMA_FIELD_LABEL, "STRING", SCHEMA_FIELD_VALUE),
            ],
        );
        let after_delete = item_json(
            "item-42",
            &[
                (FIELD_API_KEY, "CONCEALED", "sk-new-value"),
                ("env.FOO", "CONCEALED", "foo-new"),
                (SCHEMA_FIELD_LABEL, "STRING", SCHEMA_FIELD_VALUE),
            ],
        );
        let (vault, runner) = vault_with(vec![
            Ok(existing.into_bytes()),
            Ok(merged.into_bytes()),
            Ok(after_delete.into_bytes()),
        ]);
        let group = SecretGroup::provider(AppType::Claude, "p1");
        let vref = vault.put(&group, &sample_bundle()).expect("put");
        assert_eq!(runner.call_count(), 3, "get + edit + 一次定点 delete 字段");
        let (args, stdin) = runner.call(2);
        assert_eq!(args[1], "edit");
        assert_eq!(args[2], "item-42");
        assert!(
            args.iter().any(|a| a == "env.OLD[delete]"),
            "定点删除参数应为字段名[delete]，实际: {args:?}"
        );
        assert!(stdin.is_none(), "[delete] 走参数，不走 stdin");
        assert_eq!(vref.item_id, "item-42");
        runner.assert_args_contain_none_of(&["sk-new-value", "foo-new", "old-env"]);
    }

    #[test]
    fn item_title_maps_group() {
        assert_eq!(
            item_title(&SecretGroup::provider(AppType::Claude, "p1")),
            "cc-switch/claude/p1"
        );
        assert_eq!(item_title(&SecretGroup::AppSync), "cc-switch/app/sync");
    }

    /// F3-8：孤儿清理的原语——`list_tagged_items` 走 `op item list`（不带 --reveal，
    /// 不含值）；`archive_item_by_id` 按 id 归档（--archive，不走 stdin）。
    #[test]
    fn list_tagged_items_and_archive_by_id() {
        let db = std::sync::Arc::new(crate::database::Database::memory().expect("db"));
        let list_json = br#"[
            {"id":"aaa","title":"cc-switch/claude/p1","updatedAt":"2026-01-01T00:00:00Z"},
            {"id":"bbb","title":"cc-switch/claude/gone","updatedAt":"2026-01-02T00:00:00Z"}
        ]"#;
        let runner = std::sync::Arc::new(FakeOpRunner::new(vec![
            Ok(list_json.to_vec()),
            Ok(br#"{"id":"bbb"}"#.to_vec()),
        ]));
        let vault = OnePasswordVault::with_runner(runner.clone(), "acc", "vault", db);

        let items = vault.list_tagged_items().expect("list");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "aaa");
        assert_eq!(items[1].title, "cc-switch/claude/gone");
        // 不带 --reveal：列表调用不需要解锁、不含值。
        let (args, _) = runner.call(0);
        assert!(args.iter().all(|a| a != "--reveal"));

        vault.archive_item_by_id("bbb").expect("archive");
        let (args, stdin) = runner.call(1);
        assert!(args.iter().any(|a| a == "delete"));
        assert!(args.iter().any(|a| a == "bbb"));
        assert!(args.iter().any(|a| a == "--archive"));
        assert!(stdin.is_none(), "归档不需要 stdin");
    }

    #[test]
    fn parse_item_bundle_keeps_managed_fields_only() {
        // 含 op 默认字段（username / notesPlain）与 schema 标记，都应被过滤掉。
        let json = br#"{
            "id": "abc123",
            "title": "cc-switch/claude/p1",
            "category": "API_CREDENTIAL",
            "fields": [
                {"id": "username", "type": "STRING", "label": "username", "value": "ignored"},
                {"id": "credential", "type": "CONCEALED", "label": "api_key", "value": "sk-real"},
                {"id": "u1", "type": "CONCEALED", "label": "base_url", "value": "https://x"},
                {"id": "e1", "type": "CONCEALED", "label": "env.FOO", "value": "foo"},
                {"id": "s1", "type": "STRING", "label": "cc-switch-schema", "value": "1"},
                {"id": "n1", "type": "STRING", "label": "notesPlain", "value": "note"}
            ]
        }"#;
        let bundle = parse_item_bundle(json).expect("parse");
        assert_eq!(bundle.len(), 3);
        assert_eq!(
            bundle.get("api_key").map(|v| v.to_string()),
            Some("sk-real".into())
        );
        assert_eq!(
            bundle.get("base_url").map(|v| v.to_string()),
            Some("https://x".into())
        );
        assert_eq!(
            bundle.get("env.FOO").map(|v| v.to_string()),
            Some("foo".into())
        );
        assert!(bundle.get("cc-switch-schema").is_none());
        assert!(bundle.get("username").is_none());
    }

    #[test]
    fn parse_item_id_reads_id() {
        let json = br#"{"id": "xyz", "fields": []}"#;
        assert_eq!(parse_item_id(json).as_deref(), Some("xyz"));
    }

    /// F4-2 验收：空字符串值视为「无此字段」，不得把空钥匙注入 bundle（P2-2）。
    #[test]
    fn parse_item_bundle_treats_empty_value_as_missing() {
        let json = br#"{
            "id": "abc",
            "fields": [
                {"id": "f1", "type": "CONCEALED", "label": "api_key", "value": ""},
                {"id": "f2", "type": "CONCEALED", "label": "env.FOO", "value": "foo"}
            ]
        }"#;
        let bundle = parse_item_bundle(json).expect("parse");
        assert!(bundle.get(FIELD_API_KEY).is_none(), "空值不得进入整包");
        assert!(bundle.contains("env.FOO"));
    }

    /// F4-5：「从 1Password 重建引用」按标题识别归属组；解析不出（未知 app、
    /// 缺 provider id、非 cc-switch 前缀）返回 None，重建时跳过。
    #[test]
    fn parse_group_from_title_inverts_item_title() {
        let group = parse_group_from_title("cc-switch/claude/p1").expect("provider 组");
        assert_eq!(group.ref_key(), ("claude".to_string(), "p1".to_string()));
        assert!(matches!(
            parse_group_from_title("cc-switch/app/sync"),
            Some(SecretGroup::AppSync)
        ));
        // 解析失败形态：非 cc-switch 前缀、未知 app、缺 id。
        assert!(parse_group_from_title("other/claude/p1").is_none());
        assert!(parse_group_from_title("cc-switch/notanapp/p1").is_none());
        assert!(parse_group_from_title("cc-switch/claude/").is_none());
        assert!(parse_group_from_title("cc-switch/claude").is_none());
    }

    /// F4-5：`read_item_labels` 的解析——只取托管字段 label（含 schema 标记），
    /// 不含值（`op item get` 不带 `--reveal` 时 CONCEALED 字段本来就没有值）。
    #[test]
    fn parse_item_labels_keeps_managed_and_schema_labels() {
        let json = br#"{
            "id": "abc",
            "fields": [
                {"id": "u", "type": "STRING", "label": "username", "value": "me"},
                {"id": "k", "type": "CONCEALED", "label": "api_key", "value": ""},
                {"id": "e", "type": "CONCEALED", "label": "env.FOO", "value": ""},
                {"id": "s", "type": "STRING", "label": "cc-switch-schema", "value": "1"}
            ]
        }"#;
        let labels = parse_item_labels(json).expect("parse");
        assert!(labels.contains(&FIELD_API_KEY.to_string()));
        assert!(labels.contains(&"env.FOO".to_string()));
        assert!(labels.contains(&SCHEMA_FIELD_LABEL.to_string()));
        assert!(!labels.iter().any(|l| l == "username"), "op 默认字段不收");
    }

    /// F4-5：`read_item_labels` 走 `op item get`（不带 `--reveal`，不含值）。
    #[test]
    fn read_item_labels_calls_get_without_reveal() {
        let (vault, runner) = vault_with(vec![Ok(br#"{"id":"item-9","fields":[]}"#.to_vec())]);
        let labels = vault.read_item_labels("item-9").expect("labels");
        assert!(labels.is_empty());
        assert_eq!(runner.call_count(), 1);
        let (args, stdin) = runner.call(0);
        assert_eq!((args[0].as_str(), args[1].as_str()), ("item", "get"));
        assert_eq!(args[2], "item-9");
        assert!(
            !args.iter().any(|a| a == "--reveal"),
            "重建引用不得带 --reveal（只取 label，不取值）"
        );
        assert!(stdin.is_none());
    }

    /// F4-5 验收：重建引用的组合流程——`op item list` 按标题解析归属组
    /// （解析不出则跳过），逐条取 label 后重建 `secret_refs` 行（0 次值读取）。
    #[test]
    fn rebuild_refs_reconstructs_rows_from_listed_items() {
        let list_json = br#"[
            {"id":"aaa","title":"cc-switch/claude/p1","updatedAt":"2026-01-01T00:00:00Z"},
            {"id":"bbb","title":"cc-switch/app/sync","updatedAt":"2026-01-02T00:00:00Z"},
            {"id":"ccc","title":"user-made-item","updatedAt":"2026-01-03T00:00:00Z"}
        ]"#;
        let labels_a = br#"{"id":"aaa","fields":[
            {"id":"k","type":"CONCEALED","label":"api_key","value":""},
            {"id":"s","type":"STRING","label":"cc-switch-schema","value":"1"}
        ]}"#;
        let labels_b = br#"{"id":"bbb","fields":[
            {"id":"p","type":"CONCEALED","label":"app.e2e_passphrase","value":""}
        ]}"#;
        let (vault, runner) = vault_with(vec![
            Ok(list_json.to_vec()),
            Ok(labels_a.to_vec()),
            Ok(labels_b.to_vec()),
        ]);
        // 模拟命令的主循环（commands/onepassword.rs `onepassword_rebuild_refs`）。
        let items = vault.list_tagged_items().expect("list");
        let mut rebuilt = 0;
        let mut skipped = 0;
        for item in &items {
            let Some(group) = parse_group_from_title(&item.title) else {
                skipped += 1;
                continue;
            };
            let labels = vault.read_item_labels(&item.id).expect("labels");
            let (app, provider) = group.ref_key();
            vault
                .db
                .upsert_secret_ref(&app, &provider, "vault-x", &item.id, &labels)
                .unwrap();
            rebuilt += 1;
        }
        assert_eq!(rebuilt, 2, "provider 组与 AppSync 组重建");
        assert_eq!(skipped, 1, "非 cc-switch 命名规则的条目跳过");
        assert_eq!(runner.call_count(), 3, "list + 2 次 get（N+1 次 op）");
        let fields = vault.db.get_secret_ref_fields("claude", "p1").unwrap();
        assert_eq!(
            fields.unwrap(),
            vec![FIELD_API_KEY.to_string(), SCHEMA_FIELD_LABEL.to_string()]
        );
    }

    // ─── F2-2 验收：item_id 直达 / 标题兜底回写 / 冲突取最新 ────────

    /// 引用行里有真实 item id（vault 一致）时，get 直接用 id 而不是标题。
    #[test]
    fn fetch_uses_ref_item_id_directly() {
        let (vault, runner) = vault_with(vec![Ok(item_json(
            "item-real",
            &[(FIELD_API_KEY, "CONCEALED", "sk-1")],
        )
        .into_bytes())]);
        vault
            .db
            .upsert_secret_ref(
                "claude",
                "p1",
                "vault-x",
                "item-real",
                &[FIELD_API_KEY.to_string()],
            )
            .unwrap();
        let group = SecretGroup::provider(AppType::Claude, "p1");
        let got = vault.fetch(&group).expect("fetch").expect("exists");
        assert_eq!(
            got.get(FIELD_API_KEY).map(|v| v.to_string()),
            Some("sk-1".into())
        );
        assert_eq!(runner.call_count(), 1);
        let (args, _) = runner.call(0);
        assert_eq!(args[2], "item-real", "直达读取应按 item id");
    }

    /// 引用行的 id 失效（NotFound）→ 按标题兜底一次，并把找到的 id 回写引用行
    /// （字段清单保持不变）。
    #[test]
    fn fetch_stale_id_falls_back_to_title_and_repairs_ref() {
        let (vault, runner) = vault_with(vec![
            Err(RunErr::NotFound),
            Ok(item_json("item-42", &[(FIELD_API_KEY, "CONCEALED", "sk-1")]).into_bytes()),
        ]);
        vault
            .db
            .upsert_secret_ref(
                "claude",
                "p1",
                "vault-x",
                "item-gone",
                &[FIELD_API_KEY.to_string()],
            )
            .unwrap();
        let group = SecretGroup::provider(AppType::Claude, "p1");
        let got = vault.fetch(&group).expect("fetch").expect("exists");
        assert!(got.contains(FIELD_API_KEY));
        assert_eq!(runner.call_count(), 2, "id 失效后只兜底一次");
        let (args0, _) = runner.call(0);
        assert_eq!(args0[2], "item-gone");
        let (args1, _) = runner.call(1);
        assert_eq!(args1[2], "cc-switch/claude/p1", "兜底按标题");
        // 回写：item_id 已修复，字段清单保持原样。
        let (vid, iid) = vault
            .db
            .get_secret_ref_identity("claude", "p1")
            .unwrap()
            .unwrap();
        assert_eq!((vid.as_str(), iid.as_str()), ("vault-x", "item-42"));
        let fields = vault
            .db
            .get_secret_ref_fields("claude", "p1")
            .unwrap()
            .unwrap();
        assert_eq!(fields, vec![FIELD_API_KEY.to_string()]);
    }

    /// 标题命中多个（ItemConflict）→ `op item list`（不带 --reveal）筛同标题，
    /// 取 updated_at 最新的一条按 id 读取，并把 id 回写引用行。
    #[test]
    fn fetch_title_conflict_picks_latest_and_repairs_ref() {
        let list_json = br#"[
            {"id":"aaa","title":"cc-switch/claude/p1","updatedAt":"2026-01-01T00:00:00Z"},
            {"id":"bbb","title":"cc-switch/claude/p1","updatedAt":"2026-02-01T00:00:00Z"}
        ]"#;
        let (vault, runner) = vault_with(vec![
            Err(RunErr::Vault(VaultError::ItemConflict)),
            Ok(list_json.to_vec()),
            Ok(item_json("bbb", &[(FIELD_API_KEY, "CONCEALED", "sk-1")]).into_bytes()),
        ]);
        let group = SecretGroup::provider(AppType::Claude, "p1");
        let got = vault.fetch(&group).expect("fetch").expect("exists");
        assert!(got.contains(FIELD_API_KEY));
        assert_eq!(runner.call_count(), 3);
        let (list_args, _) = runner.call(1);
        assert_eq!(
            (list_args[0].as_str(), list_args[1].as_str()),
            ("item", "list")
        );
        assert!(
            !list_args.contains(&"--reveal".to_string()),
            "列条目不得带 --reveal"
        );
        assert!(
            list_args.contains(&"cc-switch".to_string()),
            "按 cc-switch 标签过滤"
        );
        let (get_args, _) = runner.call(2);
        assert_eq!(get_args[2], "bbb", "应取 updated_at 最新的一条");
        let (_, iid) = vault
            .db
            .get_secret_ref_identity("claude", "p1")
            .unwrap()
            .unwrap();
        assert_eq!(iid, "bbb");
    }

    /// 占位 item_id（v20 回填的 `provider/<app>/<id>`、空串）不能当作 1P id 使用。
    #[test]
    fn placeholder_item_ids_are_not_used_for_direct_access() {
        let (vault, runner) = vault_with(vec![Err(RunErr::NotFound)]);
        for placeholder in ["", "provider/claude/p1"] {
            let _ = vault
                .db
                .upsert_secret_ref("claude", "p1", "vault-x", placeholder, &[]);
            assert!(
                vault
                    .ref_item_id(&SecretGroup::provider(AppType::Claude, "p1"))
                    .is_none(),
                "占位 id 不应直达: {placeholder:?}"
            );
        }
        // vault 不一致的引用也不能用。
        let _ = vault
            .db
            .upsert_secret_ref("claude", "p1", "other-vault", "item-real", &[]);
        assert!(vault
            .ref_item_id(&SecretGroup::provider(AppType::Claude, "p1"))
            .is_none());
        assert_eq!(runner.call_count(), 0);
    }

    /// 真机回归（op 2.39 实测）：`op item get <id>` 会命中**归档区**条目。
    /// 引用 id 指向已归档条目时必须按「不存在」处理（归档 = 已删除），
    /// 转按标题兜底；标题也没有 → None。
    #[test]
    fn fetch_treats_archived_item_as_missing() {
        let archived = r#"{"id":"item-archived","title":"cc-switch/claude/p1","state":"ARCHIVED","fields":[{"id":"f","type":"CONCEALED","label":"api_key","value":"sk-old"}]}"#;
        let (vault, runner) = vault_with(vec![
            Ok(archived.as_bytes().to_vec()),
            Err(RunErr::NotFound),
        ]);
        vault
            .db
            .upsert_secret_ref(
                "claude",
                "p1",
                "vault-x",
                "item-archived",
                &[FIELD_API_KEY.to_string()],
            )
            .unwrap();
        let group = SecretGroup::provider(AppType::Claude, "p1");
        let got = vault.fetch(&group).expect("fetch");
        assert!(got.is_none(), "归档条目应视为不存在");
        assert_eq!(runner.call_count(), 2, "id 直达命中归档后按标题兜底一次");
        let (args1, _) = runner.call(1);
        assert_eq!(args1[2], "cc-switch/claude/p1");
    }

    /// 活跃条目（无 state 字段）不受归档判定影响。
    #[test]
    fn fetch_active_item_without_state_is_found() {
        let (vault, _runner) = vault_with(vec![Ok(item_json(
            "item-1",
            &[(FIELD_API_KEY, "CONCEALED", "sk-1")],
        )
        .into_bytes())]);
        vault
            .db
            .upsert_secret_ref(
                "claude",
                "p1",
                "vault-x",
                "item-1",
                &[FIELD_API_KEY.to_string()],
            )
            .unwrap();
        let got = vault
            .fetch(&SecretGroup::provider(AppType::Claude, "p1"))
            .expect("fetch")
            .expect("exists");
        assert_eq!(
            got.get(FIELD_API_KEY).map(|v| v.to_string()),
            Some("sk-1".into())
        );
    }

    #[test]
    fn parse_accounts_tolerates_both_uuid_fields() {
        // op 2.39 同时输出 account_uuid 与 user_uuid；不能用 alias（会 duplicate field）。
        let json = br#"[{"url":"my.1password.com","email":"u@e.com","user_uuid":"UUUU","account_uuid":"AAAA"}]"#;
        let raw: Vec<OpAccountRaw> = serde_json::from_slice(json).expect("parse");
        let accounts: Vec<OpAccount> = raw.into_iter().map(OpAccount::from).collect();
        assert_eq!(accounts.len(), 1);
        // 优先 account_uuid。
        assert_eq!(accounts[0].account_uuid, "AAAA");
        assert_eq!(accounts[0].email, "u@e.com");
    }

    #[test]
    fn parse_accounts_falls_back_to_user_uuid() {
        let json = br#"[{"url":"x","email":"e","user_uuid":"UUUU"}]"#;
        let raw: Vec<OpAccountRaw> = serde_json::from_slice(json).unwrap();
        let accounts: Vec<OpAccount> = raw.into_iter().map(OpAccount::from).collect();
        assert_eq!(accounts[0].account_uuid, "UUUU");
    }

    #[test]
    fn build_template_json_shape_has_concealed_fields_and_schema() {
        let mut bundle = SecretBundle::new();
        bundle.insert(FIELD_API_KEY, Zeroizing::new("sk-x".to_string()));
        bundle.insert("env.FOO", Zeroizing::new("foo".to_string()));
        let bytes = build_template_json("cc-switch/claude/p1", &bundle).expect("build");
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["title"], "cc-switch/claude/p1");
        assert_eq!(value["category"], "API_CREDENTIAL");
        assert_eq!(value["tags"][0], "cc-switch");
        let fields = value["fields"].as_array().unwrap();
        // 2 个秘密字段 + 1 个 schema 标记。
        assert_eq!(fields.len(), 3);
        assert!(fields.iter().any(|f| f["label"] == "cc-switch-schema"
            && f["type"] == "STRING"
            && f["value"] == "1"));
        assert!(fields
            .iter()
            .any(|f| f["label"] == "api_key" && f["type"] == "CONCEALED"));
    }

    #[test]
    fn build_template_never_puts_values_in_a_shape_that_leaks() {
        // 值只出现在 JSON 的 value 字段里（走 stdin），不构造任何命令行参数。
        let mut bundle = SecretBundle::new();
        bundle.insert(FIELD_API_KEY, Zeroizing::new("super-secret".to_string()));
        let bytes = build_template_json("t", &bundle).unwrap();
        let text = String::from_utf8_lossy(&bytes).to_string();
        assert!(text.contains("super-secret"), "值应在模板 JSON 里");
    }

    #[test]
    fn classify_stderr_signed_out() {
        assert!(matches!(
            classify_stderr("[ERROR] you are not currently signed in"),
            RunErr::Vault(VaultError::NotSignedIn)
        ));
    }

    #[test]
    fn classify_stderr_locked_and_dismissed() {
        assert!(matches!(
            classify_stderr("[ERROR] authorization prompt dismissed, please try again"),
            RunErr::Vault(VaultError::Locked)
        ));
        assert!(matches!(
            classify_stderr("[ERROR] 2026/09/25 authorization timeout"),
            RunErr::Vault(VaultError::Locked)
        ));
        // 桌面 App 未运行也归 Locked（用户侧修复 = 打开/解锁 1Password）。
        assert!(matches!(
            classify_stderr(
                "[ERROR] connecting to desktop app: cannot connect to 1Password app, make sure it is running"
            ),
            RunErr::Vault(VaultError::Locked)
        ));
    }

    #[test]
    fn classify_stderr_account_filter_miss_is_signed_out() {
        assert!(matches!(
            classify_stderr("[ERROR] found no accounts for filter \"user@example.com\""),
            RunErr::Vault(VaultError::NotSignedIn)
        ));
    }

    #[test]
    fn classify_stderr_not_found() {
        assert!(matches!(
            classify_stderr(r#""cc-switch/claude/p1" isn't an item. Specify..."#),
            RunErr::NotFound
        ));
        assert!(matches!(
            classify_stderr("no item matching that identifier was found"),
            RunErr::NotFound
        ));
    }

    /// F4-1 验收：引号里回显的条目名 / vault 名不得参与关键字匹配——
    /// id 为 `unlocked-proxy` 的条目不存在时，回显里的 "locked" 子串
    /// 不能把「条目不存在」判成 Locked（P2-1）。
    #[test]
    fn classify_stderr_quoted_display_text_is_masked() {
        assert!(matches!(
            classify_stderr(r#"[ERROR] "unlocked-proxy" isn't an item. Specify an existing item."#),
            RunErr::NotFound
        ));
        assert!(matches!(
            classify_stderr(r#"[ERROR] "my locked vault" no item matching that identifier"#),
            RunErr::NotFound
        ));
        // 真正的锁定错误（关键字在引号外）仍判 Locked。
        assert!(matches!(
            classify_stderr(r#"vault "personal" is locked. Unlock it and try again."#),
            RunErr::Vault(VaultError::Locked)
        ));
    }

    /// F4-1：成对引号整体替换为占位符；撇号（isn't）不成对、未闭合引号不动。
    #[test]
    fn mask_quoted_spans_masks_pairs_not_apostrophes() {
        assert_eq!(
            mask_quoted_spans("vault \"my vault\" is locked"),
            "vault \"…\" is locked"
        );
        // 词中的撇号不是开引号：not-found 关键字不能被夹进占位符。
        assert_eq!(mask_quoted_spans("isn't an item"), "isn't an item");
        // 词边界上的成对单引号同样剔除。
        assert_eq!(mask_quoted_spans("a 'unlocked-proxy' b"), "a \"…\" b");
        // 未闭合的引号原样保留（不做半截剔除）。
        assert_eq!(mask_quoted_spans("vault \"my"), "vault \"my");
    }

    #[test]
    fn classify_stderr_network() {
        assert!(matches!(
            classify_stderr("Post \"https://...\": dial tcp: lookup ...: no such host"),
            RunErr::Vault(VaultError::Network)
        ));
    }

    #[test]
    fn classify_stderr_other_is_opaque() {
        // 未知 stderr 归 other，且不原样携带（防条目名泄露）。
        match classify_stderr("some brand new error phrasing with secret-item-name") {
            RunErr::Vault(VaultError::Other(detail)) => {
                assert!(!detail.contains("secret-item-name"));
            }
            _ => panic!("expected Other"),
        }
    }

    /// 本机实测（需真实 op.exe，不需解锁）：签名校验应通过。
    #[test]
    #[ignore = "需本机安装 1Password CLI"]
    fn real_op_signature_verifies() {
        let path = locate_op(None).expect("本机应能定位 op.exe");
        verify_op_signature(&path).expect("真实 op.exe 签名应通过");
    }

    /// F1-7（P0-7）：签名者 O 字段判定——只认完整等于 AgileBits，不做子串匹配。
    #[test]
    fn signer_org_must_be_exactly_agilebits() {
        assert!(super::is_trusted_signer_org("Agilebits"));
        assert!(super::is_trusted_signer_org("AGILEBITS"));
        assert!(super::is_trusted_signer_org("  agilebits  "));
        // 反例：空主体、别名子串、夹带证书、带城市/部门的完整主体都必须拒绝。
        assert!(!super::is_trusted_signer_org(""));
        assert!(!super::is_trusted_signer_org("   "));
        assert!(!super::is_trusted_signer_org("1Password"));
        assert!(!super::is_trusted_signer_org("Agilebits Inc"));
        assert!(!super::is_trusted_signer_org("Evil, CN=1Password Fake"));
        assert!(!super::is_trusted_signer_org("AgileBits Wizard"));
    }

    /// 端到端往返（需解锁，手动跑）：create → get → edit → get → delete。
    /// 用环境变量指定测试 vault：
    ///   CC_SWITCH_OP_TEST_ACCOUNT=<account>  CC_SWITCH_OP_TEST_VAULT=<vault id/name>
    ///   cargo test --lib real_op_roundtrip -- --ignored --nocapture
    #[test]
    #[ignore = "需解锁的 1Password + 测试 vault（环境变量）"]
    fn real_op_roundtrip() {
        let account =
            std::env::var("CC_SWITCH_OP_TEST_ACCOUNT").expect("设 CC_SWITCH_OP_TEST_ACCOUNT");
        let vault = std::env::var("CC_SWITCH_OP_TEST_VAULT").expect("设 CC_SWITCH_OP_TEST_VAULT");
        let op_path = locate_op(None).expect("定位 op.exe");
        let db = Arc::new(crate::database::Database::memory().expect("memory db"));
        let v = OnePasswordVault::new(op_path, account, vault, db);

        // 用随机后缀避免撞名（复用 provider 组，但 id 随机）。
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let group = SecretGroup::provider(AppType::Claude, format!("devtest-{suffix}"));

        // 清理守卫：无论断言成败都尝试删除。
        struct Guard<'a>(&'a OnePasswordVault, &'a SecretGroup);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                let _ = self.0.delete(self.1);
            }
        }
        let _guard = Guard(&v, &group);

        // create
        let mut b1 = SecretBundle::new();
        b1.insert(FIELD_API_KEY, Zeroizing::new("sk-roundtrip".to_string()));
        b1.insert("env.FOO", Zeroizing::new("foo1".to_string()));
        v.put(&group, &b1).expect("create");

        // get
        let got = v.fetch(&group).expect("fetch").expect("exists");
        assert_eq!(
            got.get(FIELD_API_KEY).map(|x| x.to_string()),
            Some("sk-roundtrip".into())
        );
        assert_eq!(
            got.get("env.FOO").map(|x| x.to_string()),
            Some("foo1".into())
        );

        // edit（改值 + 添字段）
        let mut b2 = SecretBundle::new();
        b2.insert(FIELD_API_KEY, Zeroizing::new("sk-updated".to_string()));
        b2.insert("env.FOO", Zeroizing::new("foo1".to_string()));
        b2.insert(FIELD_BASE_URL, Zeroizing::new("https://x".to_string()));
        v.put(&group, &b2).expect("edit");

        let got2 = v.fetch(&group).expect("fetch2").expect("exists2");
        assert_eq!(
            got2.get(FIELD_API_KEY).map(|x| x.to_string()),
            Some("sk-updated".into())
        );
        assert_eq!(
            got2.get(FIELD_BASE_URL).map(|x| x.to_string()),
            Some("https://x".into())
        );

        // delete
        v.delete(&group).expect("delete");
        assert!(v.fetch(&group).expect("fetch3").is_none(), "删除后应不存在");
    }
}
