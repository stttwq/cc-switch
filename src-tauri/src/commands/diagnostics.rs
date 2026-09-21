#![allow(non_snake_case)]

//! 诊断信息打包（方案 4.2.4）。用户报 issue 时一键复制，不必翻目录。
//!
//! 只含版本/schema/迁移标记/计数/状态等元信息，外加**经脱敏**的最近日志尾；绝不含任何
//! 密钥明文、供应商名或完整 Base URL。日志用 `session_secret_snapshot()`（投递/迁移时登记的
//! 密钥字面量）走 `redact_known_secrets_strict`，与日志本身的护栏一致。

use std::fs;
use std::io::{Read, Seek, SeekFrom};

use tauri::State;

use crate::config::get_app_config_dir;
use crate::secrets::scan::session_secret_snapshot;
use crate::store::AppState;

/// 尾部读取的最大字节数：足够覆盖数百行日志，又不会把 20 MiB 的整份日志读进内存。
const LOG_TAIL_BYTES: u64 = 64 * 1024;
/// 诊断包最多附带的日志行数。
const MAX_LOG_LINES: usize = 50;

/// 把行内所有 `http(s)://…` 的 host 与路径打码，只保留协议（诊断包可能贴到公开 issue，
/// 同步端点 WebDAV/S3 的地址会暴露账户目录；供应商 API 主机同样隐去）。到下一个空白为止。
fn mask_urls(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(idx) = rest.find("http") {
        out.push_str(&rest[..idx]);
        let after = &rest[idx..];
        let scheme_len = if after.starts_with("https://") {
            8
        } else if after.starts_with("http://") {
            7
        } else {
            // 形如 "httpfoo" 的普通词，原样保留 4 个字符后继续。
            out.push_str("http");
            rest = &after[4..];
            continue;
        };
        let url_body = &after[scheme_len..];
        let url_end = url_body.find(char::is_whitespace).unwrap_or(url_body.len());
        out.push_str(&after[..scheme_len]);
        out.push_str("[已脱敏]");
        rest = &after[scheme_len + url_end..];
    }
    out.push_str(rest);
    out
}

/// 对日志逐行做已知密钥脱敏 + URL 打码（纯函数，便于单测）。
fn redact_log(lines: &[String], known_secrets: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|line| mask_urls(&crate::redact_known_secrets_strict(line, known_secrets)))
        .collect()
}

/// 读取日志文件尾部至多 `LOG_TAIL_BYTES` 字节，按行返回最后 `MAX_LOG_LINES` 行。
/// 文件不存在或读失败返回 `None`（诊断包据此写"无日志"，不阻断打包）。
fn read_log_tail(path: &std::path::Path) -> Option<Vec<String>> {
    let mut file = fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let start = size.saturating_sub(LOG_TAIL_BYTES);
    if start > 0 {
        file.seek(SeekFrom::Start(start)).ok()?;
    }
    let mut buf = Vec::with_capacity(size.min(LOG_TAIL_BYTES) as usize + 1);
    file.read_to_end(&mut buf).ok()?;
    // 尾部块可能从某行中间开始，丢掉第一行残段避免误导。
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && lines.len() > 1 {
        lines.remove(0);
    }
    let tail: Vec<String> = lines
        .into_iter()
        .rev()
        .take(MAX_LOG_LINES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|l| l.to_string())
        .collect();
    Some(tail)
}

/// 迁移标记摘要：把 `local_migrations` 里已置位（非 null）的字段名列出来（只有键名，无值）。
fn migration_markers() -> Vec<String> {
    let settings = crate::settings::get_settings();
    let Some(migrations) = settings.local_migrations else {
        return Vec::new();
    };
    let Ok(value) = serde_json::to_value(&migrations) else {
        return Vec::new();
    };
    match value.as_object() {
        Some(map) => map
            .iter()
            .filter(|(_, v)| !v.is_null())
            .map(|(k, _)| k.clone())
            .collect(),
        None => Vec::new(),
    }
}

/// 组装诊断文本。`known_secrets` 供日志脱敏（一般传 `session_secret_snapshot()`），
/// `known_targets` 是 DB 里登记的密钥目标条数（由调用方从 `state.db` 取，纯函数便于单测）。
fn build_bundle(
    known_secrets: &[String],
    log_tail: Option<Vec<String>>,
    known_targets: usize,
) -> String {
    let strict = if crate::settings::env_delivery_strict_mode_enabled() {
        "已开启"
    } else {
        "关闭"
    };
    let markers = migration_markers();
    let crash_exists = get_app_config_dir().join("crash.log").exists();

    let mut out = String::new();
    out.push_str(&format!(
        "CC Switch 诊断信息\n- 版本: v{}\n",
        env!("CARGO_PKG_VERSION")
    ));
    out.push_str(&format!(
        "- 数据库 schema: v{}\n",
        crate::database::SCHEMA_VERSION
    ));
    out.push_str(&format!("- 严格投递模式: {strict}\n"));
    out.push_str(&format!("- 已登记密钥目标数: {known_targets}\n"));
    out.push_str(&format!(
        "- 迁移标记: {}\n",
        if markers.is_empty() {
            "无".to_string()
        } else {
            markers.join(", ")
        }
    ));
    out.push_str(&format!(
        "- crash.log: {}\n",
        if crash_exists { "存在" } else { "不存在" }
    ));
    out.push_str("----- 最近日志（已脱敏） -----\n");
    match log_tail {
        Some(lines) if !lines.is_empty() => {
            for line in redact_log(&lines, known_secrets) {
                out.push_str(&line);
                out.push('\n');
            }
        }
        _ => out.push_str("(无日志)\n"),
    }
    out
}

/// 生成诊断文本并返回给前端复制到剪贴板。
#[tauri::command]
pub async fn get_diagnostics_bundle(state: State<'_, AppState>) -> Result<String, String> {
    let log_path = crate::panic_hook::get_log_dir().join("cc-switch.log");
    let tail = read_log_tail(&log_path);
    let secrets = session_secret_snapshot();
    let known_targets = crate::secrets::load_known_targets(state.db.as_ref())
        .map(|v| v.len())
        .unwrap_or(0);
    // 目标名计数与已脱敏的日志尾拼成纯文本；整段绝不含明文密钥、供应商名或完整 Base URL。
    Ok(build_bundle(&secrets, tail, known_targets))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_log_hides_known_secret_values() {
        let secrets = vec!["sk-ant-SUPERSECRETVALUE123".to_string()];
        let lines = vec![
            "info: delivering ANTHROPIC_AUTH_TOKEN=sk-ant-SUPERSECRETVALUE123 to env".to_string(),
            "info: switched provider, nothing sensitive here".to_string(),
        ];
        let out = redact_log(&lines, &secrets);
        let joined = out.join("\n");
        assert!(
            !joined.contains("sk-ant-SUPERSECRETVALUE123"),
            "脱敏后不得残留明文密钥: {joined}"
        );
        assert!(joined.contains("[REDACTED]"), "命中密钥应替换为占位符");
        // 非敏感行原样保留。
        assert!(joined.contains("nothing sensitive here"));
    }

    #[test]
    fn mask_urls_hides_host_and_path() {
        let line = "[WebDAV] MKCOL ok: https://dav.jianguoyun.com/dav/ccs/v3/default/ done";
        let out = mask_urls(line);
        assert!(
            !out.contains("jianguoyun") && !out.contains("/dav/ccs"),
            "URL host/路径必须打码: {out}"
        );
        assert!(out.contains("https://[已脱敏]"), "应保留协议: {out}");
        assert!(out.contains("done"), "URL 之后的文本原样保留: {out}");
        // 无 URL 的行原样返回；"http" 出现在非协议词里不误伤。
        assert_eq!(mask_urls("protocol httpfoo bar"), "protocol httpfoo bar");
    }

    #[test]
    fn build_bundle_omits_missing_log_and_counts() {
        // 无日志尾时写"(无日志)"，且整段不出现传入的密钥明文。
        let secrets = vec!["TOPSECRET-abcdef123456".to_string()];
        let text = build_bundle(&secrets, None, 7);
        assert!(text.contains("(无日志)"));
        assert!(text.contains("版本:"));
        assert!(text.contains("数据库 schema:"));
        assert!(text.contains("已登记密钥目标数: 7"));
        assert!(!text.contains("TOPSECRET-abcdef123456"));
    }
}
