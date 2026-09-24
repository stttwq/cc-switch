//! 2.2 方案 P1：`ccs env <app>` 控制台 shim 的核心逻辑。
//!
//! 安全边界与 GUI「打开终端」的 `Command::env` 注入完全一致：密钥只进目标 shell
//! 进程内存，绝不落 `HKCU\Environment` 或任何文件。凭据计算只走
//! [`ProviderService::provider_env_pairs`]，当前供应商选取只走
//! [`crate::settings::get_effective_current_provider`]，不新写取值/选取逻辑。
//!
//! 退出码分级（写进用户文档，原则 4）：
//! `0`=成功；`2`=用法/参数错误；`3`=缺密钥（fail-closed）；`4`=DB 版本不符；`5`=库/配置不可用。

use std::str::FromStr;
use std::sync::Arc;

use crate::app_config::AppType;
use crate::database::Database;
use crate::env_delivery::ManagedEnvVars;
use crate::error::AppError;
use crate::services::provider::ProviderService;
use crate::store::AppState;

// ===== 退出码（原则 4：稳定固定码）=====
pub const EXIT_OK: i32 = 0;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_MISSING_KEY: i32 = 3;
pub const EXIT_DB_VERSION: i32 = 4;
pub const EXIT_UNAVAILABLE: i32 = 5;

/// 目标 shell。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shell {
    PowerShell,
    Cmd,
    Bash,
}

impl Shell {
    /// 解析 `--shell` 值；大小写不敏感，未知返回 `None`。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "powershell" | "pwsh" => Some(Shell::PowerShell),
            "cmd" | "command_prompt" => Some(Shell::Cmd),
            "bash" => Some(Shell::Bash),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Shell::PowerShell => "powershell",
            Shell::Cmd => "cmd",
            Shell::Bash => "bash",
        }
    }
}

/// 探测启动 shim 的那层进程所用的 shell（粗判，命中与否都在 stderr 提示可覆盖）。
/// Windows 规则：`MSYSTEM`→Git Bash；否则 `PSModulePath`→PowerShell；否则缺省 PowerShell。
pub fn detect_shell() -> Shell {
    if std::env::var_os("MSYSTEM").is_some() {
        return Shell::Bash;
    }
    if std::env::var_os("PSModulePath").is_some() {
        return Shell::PowerShell;
    }
    Shell::PowerShell
}

/// shim 一次执行的产物。`stdout` 只含可被 `iex`/`eval` 执行的纯 shell 语句（原则 5）；
/// 人类可读提示一律进 `warnings`（由子二进制写 stderr）。`exit_code` 由 lib 全权算好。
#[derive(Debug)]
pub struct EnvCommandOutput {
    pub stdout: String,
    pub warnings: Vec<String>,
    pub exit_code: i32,
}

// ===== 渲染器（取数 → 渲染两段式；加 --format 只新增渲染器）=====

/// PowerShell 单引号串转义：唯一要处理的是 `'` → `''`。
fn ps_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// cmd `set "K=V"` 的 best-effort 值转义。
/// cmd 无 eval 管道，且引号内仅 `%` 会被后续展开，故只加倍 `%`；
/// 含 `"` 的值无法在 `set "K=V"` 内安全表达，属文档标注的 best-effort 限制。
fn cmd_set_value(value: &str) -> String {
    value.replace('%', "%%")
}

fn render_set(shell: Shell, name: &str, value: &str) -> String {
    match shell {
        Shell::PowerShell => format!("$env:{name}={}", ps_single_quote(value)),
        Shell::Bash => format!(
            "export {name}={}",
            crate::commands::shell_single_quote(value)
        ),
        Shell::Cmd => format!("set \"{name}={}\"", cmd_set_value(value)),
    }
}

fn render_unset(shell: Shell, name: &str) -> String {
    match shell {
        Shell::PowerShell => format!("Remove-Item \"Env:{name}\" -ErrorAction SilentlyContinue"),
        Shell::Bash => format!("unset {name}"),
        Shell::Cmd => format!("set {name}="),
    }
}

/// 合法环境变量名校验（POSIX/Windows 通用子集）。名称来自 provider `meta.api_key_field`
/// 等可配置字段，渲染前必须校验，否则含空格/`;`/`$(` 会产出损坏或可注入的 shell 语句。
fn is_valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ===== 生产入口（建无 tauri 的 AppState，Windows 真实凭据后端）=====

/// 子二进制调用的薄入口：建 state → 跑核心逻辑。
/// 版本门禁在 [`build_state`] 内完成，故 `--clear` 写路径与激活只读路径都受约束。
pub fn run_env_command(
    app: &str,
    provider_id: Option<&str>,
    shell: Shell,
    clear: bool,
) -> Result<EnvCommandOutput, AppError> {
    let state = build_state()?;
    env_command_core(&state, app, provider_id, shell, clear)
}

/// `ccs open <app> [--cwd <dir>]`：用当前激活的供应商，在 `cwd`（缺省=当前目录）
/// 直接跑该 app 对应的 CLI（claude/codex/pi）。资源管理器右键层叠菜单的落点，
/// 由无控制台的 `ccs-open.exe` 调用（避免闪窗）。
///
/// 安全边界与 GUI「运行 X」完全一致：密钥只经 `Command::env` 进目标 shell 进程，
/// 绝不落 `HKCU\Environment` 或任何文件。此处不新写取值/选取逻辑，全部复用现成内核。
/// Pi 无「当前供应商」概念，用 Pi 原生 `defaultProvider` 兜底。
pub fn run_open_command(app: &str, cwd: Option<String>) -> Result<(), AppError> {
    let state = build_state()?;
    let app_type = AppType::from_str(app)?;

    let provider_id: Option<String> = if app_type == AppType::Pi {
        let pi_state = crate::services::pi_state::PiStateService::current(&state)?;
        let id = match pi_state.default_provider_id {
            Some(id) => id,
            // Pi 无原生 defaultProvider 时：models.json 只有一个供应商就用它兜底
            // （唯一候选无歧义）；多于一个仍报错，避免替用户瞎选。
            None => {
                let mut enabled = pi_state.enabled_provider_ids;
                if enabled.len() == 1 {
                    enabled.remove(0)
                } else {
                    return Err(AppError::localized(
                        "ccs_no_current",
                        "Pi 尚未设置默认供应商，无法从右键菜单打开",
                        "Pi has no default provider set; cannot open from context menu",
                    ));
                }
            }
        };
        Some(id)
    } else {
        None
    };

    let provider = resolve_provider(&state, &app_type, provider_id.as_deref())?;

    crate::commands::launch_provider_terminal(
        &state,
        app.to_string(),
        provider.id.clone(),
        cwd,
        true, // 默认直接跑 CLI，省一步手动输入
    )
    .map_err(|msg| AppError::localized("ccs_open_failed", msg.clone(), msg))?;
    Ok(())
}

/// 构建无 tauri 的 `AppState`。shim 每次启动都要重新解析 `app_paths.json`
/// 并注入 override，且必须发生在任何 DB 打开/路径读取之前（多进程不共享缓存）。
fn build_state() -> Result<AppState, AppError> {
    crate::app_store::load_override_for_cli();

    #[cfg(windows)]
    {
        let db_path = crate::config::get_app_config_dir().join("cc-switch.db");
        gate_db_version(&db_path)?;

        // 版本相等才继续：Database::init 只在 user_version < SCHEMA_VERSION 时备份+迁移，
        // 相等时不做迁移写入，跨进程锁竞争由 busy_timeout(5000) 吸收。
        let db = Arc::new(Database::init()?);
        // 跳过 probe 以求快；真实凭据管理器不可用则映射为退出码 5。
        let secrets = Arc::new(crate::secrets::WindowsSecretStore::new().map_err(|e| {
            AppError::localized(
                "ccs_unavailable",
                format!("凭据管理器不可用: {e}"),
                format!("Credential store unavailable: {e}"),
            )
        })?);
        Ok(AppState::new(db, secrets))
    }
    #[cfg(not(windows))]
    {
        Err(AppError::localized(
            "ccs_unavailable",
            "ccs env 目前仅支持 Windows",
            "ccs env is only supported on Windows",
        ))
    }
}

/// 双进程写库竞态门禁（P1.1）：过新报错、落后拒绝迁移，仅相等才放行。
/// shim 绝不走会自动迁移的写路径。
#[cfg(windows)]
fn gate_db_version(db_path: &std::path::Path) -> Result<(), AppError> {
    if !db_path.exists() {
        // 全新库：Database::init 会以当前 SCHEMA_VERSION 建表，无需门禁。
        return Ok(());
    }
    if Database::stored_user_version_exceeds_supported(db_path)?.is_some() {
        return Err(AppError::localized(
            "ccs_db_version",
            "数据库版本比当前 ccs 更新，请先升级 cc-switch 后重试",
            "Database schema is newer than this ccs build; please upgrade cc-switch first",
        ));
    }
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| {
        AppError::localized(
            "ccs_unavailable",
            format!("无法打开数据库: {e}"),
            format!("Failed to open database: {e}"),
        )
    })?;
    let version = Database::get_user_version(&conn)?;
    if version < crate::database::SCHEMA_VERSION {
        return Err(AppError::localized(
            "ccs_db_version",
            "数据库版本落后，请先启动一次新版 cc-switch GUI 完成升级",
            "Database schema is older than this ccs build; launch the cc-switch GUI once to upgrade it",
        ));
    }
    Ok(())
}

// ===== 可测核心（注入 state，绕开真实凭据管理器与版本门禁）=====

/// 核心逻辑：解析 app → 选供应商 → 取凭据 → 渲染。`clear=true` 走显式清除（写路径）。
/// 供集成测试直接注入 `create_test_state` 的内存后端调用。
pub fn env_command_core(
    state: &AppState,
    app: &str,
    provider_id: Option<&str>,
    shell: Shell,
    clear: bool,
) -> Result<EnvCommandOutput, AppError> {
    let app_type = AppType::from_str(app)?;

    if clear {
        return build_clear(state, &app_type, provider_id, shell);
    }

    // 选当前供应商
    let provider = resolve_provider(state, &app_type, provider_id)?;

    let mut warnings: Vec<String> = Vec::new();
    let pending = ProviderService::provider_env_pairs(state, &app_type, &provider, &mut warnings)
        .map_err(|e| {
        // provider_env_pairs 的唯一 Err 来自 Claude 缺密钥，归一到 fail-closed 退出码 3。
        AppError::localized(
            "ccs_missing_key",
            e.to_string(),
            format!("Missing credentials for provider '{}': {}", provider.id, e),
        )
    })?;

    let pending_names: Vec<String> = pending.iter().map(|(k, _)| k.clone()).collect();

    // 顺带 unset 陈旧变量（只读：仅输出清除语句，不动登记表）
    let managed = ManagedEnvVars::load(&state.db)?;
    let stale = stale_vars_for(&managed, &app_type, &provider.id, &pending_names);

    let mut lines: Vec<String> = Vec::new();
    for name in &stale {
        if is_valid_env_name(name) {
            lines.push(render_unset(shell, name));
        }
    }
    for (name, value) in &pending {
        if is_valid_env_name(name) {
            lines.push(render_set(shell, name, value.as_str()));
        } else {
            warnings.push(format!(
                "skipped invalid env var name '{name}' (must match [A-Za-z_][A-Za-z0-9_]*)"
            ));
        }
    }

    // fail-closed 判定：无注入内容且带缺 key 告警 → 非零、不输出空 export。
    let has_missing = warnings
        .iter()
        .any(|w| w.starts_with("codex_missing_api_key:") || w.starts_with("pi_missing_api_key:"));
    let (stdout, exit_code) = if pending.is_empty() && has_missing {
        (String::new(), EXIT_MISSING_KEY)
    } else {
        let mut out = lines.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        (out, EXIT_OK)
    };

    if exit_code == EXIT_MISSING_KEY {
        warnings.push(
            "No credentials available for this provider; nothing was emitted. \
             Add an API key in cc-switch or pass a provider that has one."
                .to_string(),
        );
    }

    Ok(EnvCommandOutput {
        stdout,
        warnings,
        exit_code,
    })
}

/// 显式清除（写路径）：对已登记的变量**同时**从投递落点（非严格时=注册表 sink）删除、
/// 从 `managed_env_vars` 摘登记、广播，并输出 shell 的 `unset` 语句。
///
/// 关键：必须"删值 + 摘登记"成对做。只摘登记而留注册表真实值 → 下次 GUI 切换按登记簿删不到旧值、
/// 再写同名时 `check_conflict` 判 foreign 永久拒写（`mod.rs` 缺陷 D-4；且 D-4 的"认领"仅迁移期跑，
/// 手动切换不救）。严格模式下注册表本就无值，`sink.remove` 自然空转。
fn build_clear(
    state: &AppState,
    app_type: &AppType,
    provider_id: Option<&str>,
    shell: Shell,
) -> Result<EnvCommandOutput, AppError> {
    if *app_type == AppType::Pi && provider_id.is_none() {
        return Err(pi_requires_id(state, app_type));
    }

    let sink = state.env_sink.as_ref();
    let mut managed = ManagedEnvVars::load(&state.db)?;
    let names = if *app_type == AppType::Pi {
        managed.vars_for_provider("pi", provider_id.unwrap())
    } else {
        managed.vars_for_app(app_type.as_str())
    };

    // 与 GUI 收回成对：先删投递落点的真实值，**只有删成功才摘登记**。
    // 删不掉的保留登记（下次 GUI 切换仍能收回），并把失败打到 stderr——否则"值在、登记没了"
    // 会落成 D-4 孤儿态，令后续非严格切换被判 foreign 拒写。严格模式注册表本就无值，remove 空转。
    let mut warnings: Vec<String> = Vec::new();
    for name in &names {
        match sink.remove(name) {
            Ok(()) => managed.unregister(name),
            Err(e) => warnings.push(format!(
                "failed to reclaim '{name}' from user environment; it stays registered: {e}"
            )),
        }
    }
    managed.save(&state.db)?;
    if !names.is_empty() {
        if let Err(e) = sink.broadcast() {
            log::warn!("ccs env --clear 广播失败: {e}");
        }
    }

    let mut out = String::new();
    for name in &names {
        out.push_str(&render_unset(shell, name));
        out.push('\n');
    }
    Ok(EnvCommandOutput {
        stdout: out,
        warnings,
        exit_code: EXIT_OK,
    })
}

/// 选取当前供应商（激活路径）。
fn resolve_provider(
    state: &AppState,
    app_type: &AppType,
    provider_id: Option<&str>,
) -> Result<crate::provider::Provider, AppError> {
    if *app_type == AppType::Pi {
        let id = provider_id.ok_or_else(|| pi_requires_id(state, app_type))?;
        return state.db.get_provider_by_id(id, "pi")?.ok_or_else(|| {
            AppError::localized(
                "ccs_no_current",
                format!("Pi 供应商不存在: {id}"),
                format!("Pi provider not found: {id}"),
            )
        });
    }

    let id =
        crate::settings::get_effective_current_provider(&state.db, app_type)?.ok_or_else(|| {
            AppError::localized(
                "ccs_no_current",
                format!("{} 尚未选择当前供应商", app_type.as_str()),
                format!("No current provider selected for {}", app_type.as_str()),
            )
        })?;
    state
        .db
        .get_provider_by_id(&id, app_type.as_str())?
        .ok_or_else(|| {
            AppError::localized(
                "ccs_no_current",
                format!("当前供应商不存在: {id}"),
                format!("Current provider not found: {id}"),
            )
        })
}

/// Pi 无“当前供应商”概念，必须显式给 id；报错时在 stderr 列出可用 id。
fn pi_requires_id(state: &AppState, _app_type: &AppType) -> AppError {
    let ids = state
        .db
        .get_provider_ids("pi")
        .map(|set| {
            let mut v: Vec<String> = set.into_iter().collect();
            v.sort();
            v.join(", ")
        })
        .unwrap_or_default();
    AppError::localized(
        "ccs_usage",
        "Pi 需显式指定 provider id",
        format!("Pi requires an explicit provider id. Available: [{ids}]"),
    )
}

/// 已登记但本轮未注入的变量（激活时顺带清除）。
/// Claude/Codex 按 app 圈定；Pi 必须按 provider 圈定，否则 `ccs env pi idA` 会误清 idB。
fn stale_vars_for(
    managed: &ManagedEnvVars,
    app_type: &AppType,
    provider_id: &str,
    pending_names: &[String],
) -> Vec<String> {
    let registered = if *app_type == AppType::Pi {
        managed.vars_for_provider("pi", provider_id)
    } else {
        managed.vars_for_app(app_type.as_str())
    };
    registered
        .into_iter()
        .filter(|name| !pending_names.contains(name))
        .collect()
}

/// 把 [`AppError`] 映射到原则 4 约定的固定退出码（`Err` 路径，子二进制侧调用）。
pub fn classify_exit(err: &AppError) -> i32 {
    match err {
        AppError::Localized { key, .. } => match *key {
            "unsupported_app" | "ccs_usage" => EXIT_USAGE,
            "ccs_missing_key" => EXIT_MISSING_KEY,
            "ccs_db_version" => EXIT_DB_VERSION,
            _ => EXIT_UNAVAILABLE,
        },
        _ => EXIT_UNAVAILABLE,
    }
}

/// 取 [`AppError`] 的英文文案（shim 输出固定英文，原则 8）。
pub fn error_message_en(err: &AppError) -> String {
    match err {
        AppError::Localized { en, .. } => en.clone(),
        other => other.to_string(),
    }
}

/// 取 [`AppError`] 的中文文案（无控制台启动器 `ccs-open` 弹框提示用）。
pub fn error_message_zh(err: &AppError) -> String {
    match err {
        AppError::Localized { zh, .. } => zh.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_parse_and_detect() {
        assert_eq!(Shell::parse("PowerShell"), Some(Shell::PowerShell));
        assert_eq!(Shell::parse("cmd"), Some(Shell::Cmd));
        assert_eq!(Shell::parse("BASH"), Some(Shell::Bash));
        assert_eq!(Shell::parse("zsh"), None);
        assert_eq!(Shell::PowerShell.as_str(), "powershell");
    }

    #[test]
    fn ps_single_quote_doubles_only_quotes() {
        assert_eq!(ps_single_quote("abc"), "'abc'");
        assert_eq!(ps_single_quote("a'b"), "'a''b'");
        assert_eq!(ps_single_quote(""), "''");
        // PowerShell 单引号串里 % & 空格都是字面量，不转义
        assert_eq!(ps_single_quote("a%b&c d"), "'a%b&c d'");
    }

    #[test]
    fn render_set_lines_per_shell() {
        assert_eq!(render_set(Shell::PowerShell, "K", "v"), "$env:K='v'");
        assert_eq!(render_set(Shell::Bash, "K", "v"), "export K='v'");
        assert_eq!(render_set(Shell::Cmd, "K", "v"), "set \"K=v\"");
    }

    #[test]
    fn render_unset_lines_per_shell() {
        assert_eq!(
            render_unset(Shell::PowerShell, "K"),
            "Remove-Item \"Env:K\" -ErrorAction SilentlyContinue"
        );
        assert_eq!(render_unset(Shell::Bash, "K"), "unset K");
        assert_eq!(render_unset(Shell::Cmd, "K"), "set K=");
    }

    #[test]
    fn classify_exit_maps_keys() {
        assert_eq!(
            classify_exit(&AppError::localized("unsupported_app", "z", "e")),
            EXIT_USAGE
        );
        assert_eq!(
            classify_exit(&AppError::localized("ccs_missing_key", "z", "e")),
            EXIT_MISSING_KEY
        );
        assert_eq!(
            classify_exit(&AppError::localized("ccs_db_version", "z", "e")),
            EXIT_DB_VERSION
        );
        assert_eq!(
            classify_exit(&AppError::Database("x".into())),
            EXIT_UNAVAILABLE
        );
    }

    #[test]
    fn cmd_value_doubles_percent() {
        assert_eq!(cmd_set_value("50%"), "50%%");
    }

    #[test]
    fn env_name_validation_rejects_injection() {
        assert!(is_valid_env_name("ANTHROPIC_AUTH_TOKEN"));
        assert!(is_valid_env_name("_X1"));
        assert!(!is_valid_env_name("1BAD"));
        assert!(!is_valid_env_name("A B"));
        assert!(!is_valid_env_name("A;rm -rf"));
        assert!(!is_valid_env_name(""));
    }
}
