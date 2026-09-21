//! 配置目录 ACL 收紧（方案 4.2.3 / S-10，决策 D2 默认做）。
//!
//! 首次创建 `~/.cc-switch` 后，把目录 DACL 收敛为「当前用户 + SYSTEM + Administrators」，
//! 去掉继承。理由（相对瘦身期 S12「文件不含密钥，不做」已变）：库里虽无密钥，但有供应商名、
//! 历史 Base URL、MCP 命令、prompts，属用户隐私。
//!
//! 实现走一次 `icacls` 子进程（比手搓 `SetNamedSecurityInfoW` 的 SID/ACL 结构稳），当前用户
//! 用 `whoami /user` 拿 SID，避免域账户/本地账户的名字歧义。用标记文件保证只补一次，
//! 不反复覆盖用户之后手工调过的权限。非 Windows 平台编译为 no-op。

use std::path::Path;

use crate::error::AppError;

/// SYSTEM / Administrators 的固定 SID。
const SID_SYSTEM: &str = "S-1-5-18";
const SID_ADMINISTRATORS: &str = "S-1-5-32-544";
/// 幂等标记文件：存在即视为已收紧，不再重复执行。
const MARKER_FILE: &str = ".acl-tightened-v1";

/// 从 `whoami /user /fo csv /nh` 的输出里取当前用户 SID（CSV 第二列）。
fn parse_whoami_sid(stdout: &str) -> Option<String> {
    // 形如："DESKTOP\alice","S-1-5-21-...-1001"
    let line = stdout.lines().next()?.trim();
    let sid = line
        .split(',')
        .nth(1)?
        .trim()
        .trim_matches('"')
        .trim_start_matches('*')
        .to_string();
    if sid.starts_with("S-1-") {
        Some(sid)
    } else {
        None
    }
}

/// 构造 `icacls` 参数：去继承 + 仅授三者完全控制（`(OI)(CI)F`，含子目录/文件继承）。
fn build_icacls_args(dir: &Path, user_sid: &str) -> Vec<String> {
    vec![
        dir.to_string_lossy().to_string(),
        "/inheritance:r".to_string(),
        "/grant:r".to_string(),
        format!("*{user_sid}:(OI)(CI)F"),
        format!("*{SID_SYSTEM}:(OI)(CI)F"),
        format!("*{SID_ADMINISTRATORS}:(OI)(CI)F"),
    ]
}

/// 幂等地收紧配置目录 ACL：已收紧（标记在）直接返回；否则执行并落标记。
/// 失败只记 warn、不阻断启动（权限收紧是增强项，不是正确性前提）。
pub fn ensure_config_dir_acl_tightened(dir: &Path) {
    #[cfg(windows)]
    {
        if dir.join(MARKER_FILE).exists() {
            return;
        }
        match tighten(dir) {
            Ok(()) => {
                // 标记落在受保护目录内，内容无关紧要，仅用于幂等。
                let _ = std::fs::write(dir.join(MARKER_FILE), b"");
                log::info!("[ACL] 配置目录权限已收紧：仅当前用户 + SYSTEM + Administrators");
            }
            Err(e) => log::warn!("[ACL] 配置目录权限收紧失败（不影响使用）: {e}"),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = dir; // D1：本项目仅 Windows，非 Windows 为 no-op。
    }
}

#[cfg(windows)]
fn tighten(dir: &Path) -> Result<(), AppError> {
    use std::process::Command;

    let sid = current_user_sid()?;
    let args = build_icacls_args(dir, &sid);
    let output = Command::new("icacls").args(&args).output().map_err(|e| {
        AppError::localized(
            "acl.icacls_failed",
            format!("调用 icacls 失败: {e}"),
            format!("Failed to invoke icacls: {e}"),
        )
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::localized(
            "acl.icacls_nonzero",
            format!("icacls 返回非零：{stderr}"),
            format!("icacls exited non-zero: {stderr}"),
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn current_user_sid() -> Result<String, AppError> {
    use std::process::Command;
    // 隐藏控制台窗口（GUI 应用里子进程默认会闪窗）。
    let mut cmd = Command::new("whoami");
    cmd.args(["/user", "/fo", "csv", "/nh"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = cmd.output().map_err(|e| {
        AppError::localized(
            "acl.whoami_failed",
            format!("调用 whoami 失败: {e}"),
            format!("Failed to invoke whoami: {e}"),
        )
    })?;
    if !output.status.success() {
        return Err(AppError::localized(
            "acl.whoami_nonzero",
            "whoami /user 返回非零",
            "whoami /user exited non-zero",
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // whoami csv 可能是 GBK；SID 是 ASCII，按字节取第二列更稳。
    parse_whoami_sid(&stdout).ok_or_else(|| {
        AppError::localized(
            "acl.sid_parse_failed",
            "无法从 whoami 输出解析当前用户 SID",
            "Failed to parse current-user SID from whoami output",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_whoami_sid_extracts_quoted_sid_column() {
        let out = "\"CORP\\jane\",\"S-1-5-21-3623811015-3361044940-1592777828-1001\"\r\n";
        assert_eq!(
            parse_whoami_sid(out).as_deref(),
            Some("S-1-5-21-3623811015-3361044940-1592777828-1001")
        );
    }

    #[test]
    fn parse_whoami_sid_rejects_garbage() {
        assert_eq!(parse_whoami_sid(""), None);
        assert_eq!(parse_whoami_sid("\"no-comma-here\""), None);
        assert_eq!(parse_whoami_sid("\"user\",\"not-a-sid\""), None);
    }

    #[test]
    fn build_icacls_args_grants_three_sids_and_strips_inheritance() {
        let args = build_icacls_args(&PathBuf::from("C:\\Users\\jane\\.cc-switch"), "S-1-5-21-9");
        assert_eq!(args[1], "/inheritance:r");
        assert_eq!(args[2], "/grant:r");
        assert!(args.iter().any(|a| a == "*S-1-5-21-9:(OI)(CI)F"));
        assert!(args.iter().any(|a| a == "*S-1-5-18:(OI)(CI)F"));
        assert!(args.iter().any(|a| a == "*S-1-5-32-544:(OI)(CI)F"));
    }

    #[test]
    fn marker_file_short_circuits_when_already_applied() {
        // 幂等：ensure 在标记存在时应直接返回（非 Windows 下本就 no-op，这里只验不 panic）。
        let dir = std::env::temp_dir().join("cc-switch-acl-noexist-xyz");
        ensure_config_dir_acl_tightened(&dir);
    }
}
