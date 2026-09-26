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
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

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
enum RunErr {
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
    // 2) where.exe op（转绝对路径）。
    #[cfg(windows)]
    {
        if let Ok(output) = Command::new("where.exe").arg("op").output() {
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
            for rel in [
                "Microsoft\\WinGet\\Links\\op.exe",
                "1Password CLI\\op.exe",
            ] {
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
pub struct OnePasswordVault {
    op_path: PathBuf,
    account: String,
    vault: String,
}

/// 串行化所有 `op` 调用（进程全局）：并发会叠加多个授权弹窗（§12.6）。
static OP_LOCK: Mutex<()> = Mutex::new(());

impl OnePasswordVault {
    pub fn new(
        op_path: PathBuf,
        account: impl Into<String>,
        vault: impl Into<String>,
    ) -> Self {
        Self {
            op_path,
            account: account.into(),
            vault: vault.into(),
        }
    }

    pub fn op_path(&self) -> &Path {
        &self.op_path
    }

    /// 执行一次 `op`（§5.2）。`stdin` 需要时以管道写入后立即关闭。
    /// 返回 stdout（`Zeroizing`，解析后立刻丢弃，永不写日志）。
    fn run_op(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Zeroizing<Vec<u8>>, RunErr> {
        exec_op(&self.op_path, args, stdin)
    }

    fn base_read_args<'a>(&'a self, title: &'a str) -> Vec<&'a str> {
        vec![
            "item",
            "get",
            title,
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
}

/// 执行一次 `op`（§5.2）；进程全局串行。`stdin` 需要时管道写入后立即关闭。
/// 返回 stdout（`Zeroizing`，解析后立即丢弃，永不写日志）。
fn exec_op(
    op_path: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<Zeroizing<Vec<u8>>, RunErr> {
    let _guard = OP_LOCK.lock().unwrap_or_else(|e| e.into_inner());

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

/// 列出账户（不需解锁）。
pub fn list_accounts(op_path: &Path) -> Result<Vec<OpAccount>, VaultError> {
    let out = exec_op(
        op_path,
        &["account", "list", "--format", "json", "--no-color"],
        None,
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
            "vault", "list", "--account", account, "--format", "json", "--no-color",
        ],
        None,
    )
    .map_err(RunErr::into_vault)?;
    serde_json::from_slice(&out)
        .map_err(|e| VaultError::Other(format!("parse vaults failed: {e}")))
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
    let signature_ok = if verify_signature {
        Some(verify_op_signature(&path).is_ok())
    } else {
        None
    };
    let version = op_version(&path);
    let signed_in = list_accounts(&path).map(|a| !a.is_empty()).unwrap_or(false);
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

/// 该字段名是否属于 CC Switch 管理的秘密字段（过滤掉 op 默认字段与 schema 标记）。
fn is_managed_field(label: &str) -> bool {
    label == FIELD_API_KEY
        || label == FIELD_BASE_URL
        || label.starts_with(FIELD_ENV_PREFIX)
        || label.starts_with(FIELD_APP_PREFIX)
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
fn parse_item_bundle(bytes: &[u8]) -> Result<SecretBundle, VaultError> {
    let item: OpItemRead = serde_json::from_slice(bytes)
        .map_err(|e| VaultError::Other(format!("parse op item failed: {e}")))?;
    let mut bundle = SecretBundle::new();
    for field in item.fields {
        let (Some(label), Some(value)) = (field.label, field.value) else {
            continue;
        };
        if !is_managed_field(&label) {
            continue;
        }
        bundle.insert(label, Zeroizing::new(value));
    }
    Ok(bundle)
}

/// 从 `op item get` 输出解析条目 id（写回 secret_refs 的 item_id）。
fn parse_item_id(bytes: &[u8]) -> Option<String> {
    serde_json::from_slice::<OpItemRead>(bytes)
        .ok()
        .and_then(|i| i.id)
}

// ─── op 条目模板 JSON（写，走 stdin） ────────────────────────

#[derive(Serialize)]
struct OpItemTemplate {
    title: String,
    category: &'static str,
    tags: Vec<&'static str>,
    fields: Vec<OpFieldTemplate>,
}

#[derive(Serialize)]
struct OpFieldTemplate {
    label: String,
    #[serde(rename = "type")]
    field_type: &'static str,
    value: String,
}

/// 构造 create/edit 用的条目模板 JSON（stdin 管道）。所有秘密字段 CONCEALED，
/// 另加一个非秘密 STRING 标记字段。绝不把值放进命令行参数（§3.4 / §12.3）。
fn build_template_json(title: &str, bundle: &SecretBundle) -> Result<Vec<u8>, VaultError> {
    let mut fields: Vec<OpFieldTemplate> = bundle
        .iter()
        .map(|(label, value)| OpFieldTemplate {
            label: label.clone(),
            field_type: "CONCEALED",
            value: value.to_string(),
        })
        .collect();
    fields.push(OpFieldTemplate {
        label: SCHEMA_FIELD_LABEL.to_string(),
        field_type: "STRING",
        value: SCHEMA_FIELD_VALUE.to_string(),
    });
    let template = OpItemTemplate {
        title: title.to_string(),
        category: OP_CATEGORY,
        tags: vec![OP_TAG],
        fields,
    };
    serde_json::to_vec(&template)
        .map_err(|e| VaultError::Other(format!("serialize op template failed: {e}")))
}

// ─── stderr 分类（§5.2；关键字以本机实测为准，样本入单测） ───

fn classify_stderr(stderr: &str) -> RunErr {
    let lower = stderr.to_lowercase();
    let has = |needle: &str| lower.contains(needle);

    if has("not currently signed in")
        || has("no accounts configured")
        || has("no account found")
        || has("found no accounts")
        || has("no accounts for filter")
    {
        return RunErr::Vault(VaultError::NotSignedIn);
    }
    if has("authorization prompt dismissed")
        || has("authorization timeout")
        || has("is not unlocked")
        || has("account is not unlocked")
        || has("locked")
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
        match self.run_op(&self.base_read_args(&title), None) {
            Ok(bytes) => Ok(Some(parse_item_bundle(&bytes)?)),
            Err(RunErr::NotFound) => Ok(None),
            Err(RunErr::Vault(e)) => Err(e),
        }
    }

    fn put(&self, group: &SecretGroup, bundle: &SecretBundle) -> Result<VaultRef, VaultError> {
        let title = item_title(group);
        let template = build_template_json(&title, bundle)?;

        // 覆盖式写（§5.4）：先删旧条目（存在则归档，幂等），再新建。
        // 不用 `op item edit`：它靠 assignment 语句传值（会把密钥放命令行，§12.3 禁止）；
        // create 支持以 `-` 从 stdin 读 JSON 模板，恰好避开。op 允许同名条目，所以
        // 必须先删后建，否则会出现同标题重复条目、get 时报歧义。
        let delete_args = [
            "item",
            "delete",
            &title,
            "--vault",
            &self.vault,
            "--account",
            &self.account,
            "--archive",
            "--no-color",
        ];
        match self.run_op(&delete_args, None) {
            Ok(_) => {}
            Err(RunErr::NotFound) => {} // 新建场景：旧条目不存在，正常。
            Err(e) => return Err(e.into_vault()),
        }

        // create 以 `-` 作为位置参数，模板 JSON 走 stdin。
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
            .run_op(&create_args, Some(&template))
            .map_err(RunErr::into_vault)?;

        let item_id = parse_item_id(&out).unwrap_or_default();
        Ok(VaultRef {
            vault_id: self.vault.clone(),
            item_id,
            fields: bundle.field_names(),
        })
    }

    fn delete(&self, group: &SecretGroup) -> Result<(), VaultError> {
        let title = item_title(group);
        let args = [
            "item",
            "delete",
            &title,
            "--vault",
            &self.vault,
            "--account",
            &self.account,
            "--archive",
            "--no-color",
        ];
        match self.run_op(&args, None) {
            Ok(_) => Ok(()),
            // 已不存在视为删除成功（幂等）。
            Err(RunErr::NotFound) => Ok(()),
            Err(RunErr::Vault(e)) => Err(e),
        }
    }

    fn status(&self) -> VaultStatus {
        // 不需解锁、不联网或极快：判断「已安装 + 已配置账户」。
        let args = ["account", "list", "--format", "json", "--no-color"];
        match self.run_op(&args, None) {
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
}

/// 校验 op.exe 签名（D8 / §5.5）：WinVerifyTrust 确认 Authenticode 签名可信，
/// 再确认签名主体含 AgileBits。任一失败拒用（op.exe 是整个方案的信任根）。
#[cfg(windows)]
pub fn verify_op_signature(path: &Path) -> Result<(), VaultError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::Cryptography::{
        CertCloseStore, CertEnumCertificatesInStore, CertFreeCertificateContext,
        CertGetNameStringW, CryptQueryObject, CERT_NAME_SIMPLE_DISPLAY_TYPE,
        CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED, CERT_QUERY_FORMAT_FLAG_BINARY,
        CERT_QUERY_OBJECT_FILE,
    };
    use windows_sys::Win32::Security::WinTrust::{
        WinVerifyTrust, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_FILE_INFO,
        WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY,
        WTD_UI_NONE,
    };

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // 1) WinVerifyTrust：确认签名有效且链到可信根。
    // SAFETY: 结构体按 Win32 约定填充，wide 指针在调用期间有效。
    let trust_ok = unsafe {
        let mut file_info: WINTRUST_FILE_INFO = std::mem::zeroed();
        file_info.cbStruct = std::mem::size_of::<WINTRUST_FILE_INFO>() as u32;
        file_info.pcwszFilePath = wide.as_ptr();

        let mut wtd: WINTRUST_DATA = std::mem::zeroed();
        wtd.cbStruct = std::mem::size_of::<WINTRUST_DATA>() as u32;
        wtd.dwUIChoice = WTD_UI_NONE;
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

        // 关闭状态数据（无论成败都要调用）。
        wtd.dwStateAction = WTD_STATEACTION_CLOSE;
        WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            (&mut wtd as *mut WINTRUST_DATA).cast(),
        );

        status == 0
    };
    if !trust_ok {
        return Err(VaultError::Other(
            "op.exe 签名无效或不受信任，已拒绝使用".to_string(),
        ));
    }

    // 2) 主体校验：读嵌入证书，确认签名主体含 AgileBits。
    // SAFETY: CryptQueryObject 成功时给出 cert store 句柄，用后 CertCloseStore。
    let subject_ok = unsafe {
        let mut cert_store = std::ptr::null_mut();
        let ok = CryptQueryObject(
            CERT_QUERY_OBJECT_FILE,
            wide.as_ptr().cast(),
            CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED,
            CERT_QUERY_FORMAT_FLAG_BINARY,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut cert_store,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if ok == 0 || cert_store.is_null() {
            false
        } else {
            let mut found = false;
            let mut cert_ctx = CertEnumCertificatesInStore(cert_store, std::ptr::null_mut());
            while !cert_ctx.is_null() {
                // 先取所需缓冲长度，再取字符串。
                let len = CertGetNameStringW(
                    cert_ctx,
                    CERT_NAME_SIMPLE_DISPLAY_TYPE,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    0,
                );
                if len > 1 {
                    let mut buf = vec![0u16; len as usize];
                    CertGetNameStringW(
                        cert_ctx,
                        CERT_NAME_SIMPLE_DISPLAY_TYPE,
                        0,
                        std::ptr::null_mut(),
                        buf.as_mut_ptr(),
                        len,
                    );
                    let name = String::from_utf16_lossy(&buf);
                    let name_lower = name.to_lowercase();
                    if name_lower.contains("agilebits") || name_lower.contains("1password") {
                        found = true;
                    }
                }
                let next = CertEnumCertificatesInStore(cert_store, cert_ctx);
                // CertEnumCertificatesInStore 会释放传入的 ctx，这里不额外 free。
                cert_ctx = next;
                if found {
                    if !cert_ctx.is_null() {
                        CertFreeCertificateContext(cert_ctx);
                    }
                    break;
                }
            }
            CertCloseStore(cert_store, 0);
            found
        }
    };
    if !subject_ok {
        return Err(VaultError::Other(
            "op.exe 签名主体不是 AgileBits，已拒绝使用".to_string(),
        ));
    }
    Ok(())
}

/// 非 Windows 平台不做签名校验（本项目仅面向 Windows；留桩便于跨平台编译）。
#[cfg(not(windows))]
pub fn verify_op_signature(_path: &Path) -> Result<(), VaultError> {
    Ok(())
}

/// 探测 `op` 版本（诊断用）。不需解锁。
pub fn op_version(op_path: &Path) -> Option<String> {
    let mut cmd = Command::new(op_path);
    cmd.arg("--version");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = cmd.output().ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// 供 §4.2 构造：读取 1Password 设置并定位 op。缺任一项则返回错误。
pub fn from_settings() -> Result<OnePasswordVault, VaultError> {
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
    Ok(OnePasswordVault::new(op_path, account, vault))
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
        match from_settings() {
            Ok(v) => std::sync::Arc::new(v),
            Err(e) => {
                log::warn!("1Password 后端构造失败（code={}），App 照常打开，取钥匙将报错", e.code());
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

    #[test]
    fn item_title_maps_group() {
        assert_eq!(
            item_title(&SecretGroup::provider(AppType::Claude, "p1")),
            "cc-switch/claude/p1"
        );
        assert_eq!(item_title(&SecretGroup::AppSync), "cc-switch/app/sync");
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
        assert_eq!(bundle.get("api_key").map(|v| v.to_string()), Some("sk-real".into()));
        assert_eq!(bundle.get("base_url").map(|v| v.to_string()), Some("https://x".into()));
        assert_eq!(bundle.get("env.FOO").map(|v| v.to_string()), Some("foo".into()));
        assert!(bundle.get("cc-switch-schema").is_none());
        assert!(bundle.get("username").is_none());
    }

    #[test]
    fn parse_item_id_reads_id() {
        let json = br#"{"id": "xyz", "fields": []}"#;
        assert_eq!(parse_item_id(json).as_deref(), Some("xyz"));
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
        let text = String::from_utf8(bytes).unwrap();
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

    /// 端到端往返（需解锁，手动跑）：create → get → edit → get → delete。
    /// 用环境变量指定测试 vault：
    ///   CC_SWITCH_OP_TEST_ACCOUNT=<account>  CC_SWITCH_OP_TEST_VAULT=<vault id/name>
    ///   cargo test --lib real_op_roundtrip -- --ignored --nocapture
    #[test]
    #[ignore = "需解锁的 1Password + 测试 vault（环境变量）"]
    fn real_op_roundtrip() {
        let account = std::env::var("CC_SWITCH_OP_TEST_ACCOUNT")
            .expect("设 CC_SWITCH_OP_TEST_ACCOUNT");
        let vault = std::env::var("CC_SWITCH_OP_TEST_VAULT").expect("设 CC_SWITCH_OP_TEST_VAULT");
        let op_path = locate_op(None).expect("定位 op.exe");
        let v = OnePasswordVault::new(op_path, account, vault);

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
        assert_eq!(got.get(FIELD_API_KEY).map(|x| x.to_string()), Some("sk-roundtrip".into()));
        assert_eq!(got.get("env.FOO").map(|x| x.to_string()), Some("foo1".into()));

        // edit（改值 + 添字段）
        let mut b2 = SecretBundle::new();
        b2.insert(FIELD_API_KEY, Zeroizing::new("sk-updated".to_string()));
        b2.insert("env.FOO", Zeroizing::new("foo1".to_string()));
        b2.insert(FIELD_BASE_URL, Zeroizing::new("https://x".to_string()));
        v.put(&group, &b2).expect("edit");

        let got2 = v.fetch(&group).expect("fetch2").expect("exists2");
        assert_eq!(got2.get(FIELD_API_KEY).map(|x| x.to_string()), Some("sk-updated".into()));
        assert_eq!(got2.get(FIELD_BASE_URL).map(|x| x.to_string()), Some("https://x".into()));

        // delete
        v.delete(&group).expect("delete");
        assert!(v.fetch(&group).expect("fetch3").is_none(), "删除后应不存在");
    }
}
