use crate::error::AppError;
use regex::Regex;
use std::sync::{Mutex, OnceLock};

/// §5.5：本次会话写入过凭据管理器的密钥字面量（`Zeroizing` 承载，上限 64 条）。
/// 正则只能挡住已知格式的密钥，这一层专门盯"我们自己存进去的值"有没有
/// 经由某条旁路又回到导出文本里。
fn session_secrets() -> &'static Mutex<Vec<zeroize::Zeroizing<String>>> {
    static SECRETS: OnceLock<Mutex<Vec<zeroize::Zeroizing<String>>>> = OnceLock::new();
    SECRETS.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn note_session_secret(value: &str) {
    if value.len() < 8 {
        return;
    }
    let mut guard = session_secrets().lock().expect("session secret lock");
    if guard.iter().any(|existing| existing.as_str() == value) {
        return;
    }
    if guard.len() >= 64 {
        guard.remove(0);
    }
    guard.push(zeroize::Zeroizing::new(value.to_string()));
}

fn matches_session_secret(text: &str) -> bool {
    let Ok(guard) = session_secrets().lock() else {
        return false;
    };
    guard.iter().any(|secret| text.contains(secret.as_str()))
}

/// S2：给日志脱敏用的一份快照（普通 String，仅在本进程内存里短暂存在）。
pub fn session_secret_snapshot() -> Vec<String> {
    let Ok(guard) = session_secrets().lock() else {
        return Vec::new();
    };
    guard.iter().map(|secret| secret.to_string()).collect()
}

fn patterns() -> &'static [Regex] {
    static RE: OnceLock<Vec<Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        vec![
            Regex::new(r"sk-ant-[A-Za-z0-9_-]{8,}").expect("regex"),
            Regex::new(r"sk-[A-Za-z0-9]{16,}").expect("regex"),
            Regex::new(r"xai-[A-Za-z0-9]{16,}").expect("regex"),
            Regex::new(r"AKIA[0-9A-Z]{16}").expect("regex"),
            Regex::new(r"Bearer [A-Za-z0-9._-]{20,}").expect("regex"),
        ]
    })
}

pub fn assert_no_secret_patterns_in_conn(conn: &rusqlite::Connection) -> Result<(), AppError> {
    let mut blob = String::new();
    if let Ok(mut stmt) = conn.prepare("SELECT settings_config FROM providers") {
        let rows = stmt.query_map([], |row| row.get::<_, String>(0));
        if let Ok(rows) = rows {
            for row in rows.flatten() {
                blob.push_str(&row);
                blob.push('\n');
            }
        }
    }
    if let Ok(mut stmt) = conn.prepare("SELECT key, value FROM settings") {
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        });
        if let Ok(rows) = rows {
            for (key, value) in rows.flatten() {
                if key == "managed_env_vars" || key == "known_secret_targets" {
                    continue;
                }
                blob.push_str(&value);
                blob.push('\n');
            }
        }
    }
    assert_no_secret_patterns(&blob)
}

pub fn assert_no_secret_patterns(text: &str) -> Result<(), AppError> {
    for re in patterns() {
        if let Some(m) = re.find(text) {
            return Err(AppError::Config(format!(
                "导出护栏拒绝：检测到疑似密钥模式 {}",
                &m.as_str()[..m.as_str().len().min(8)]
            )));
        }
    }
    if matches_session_secret(text) {
        // 只报"命中"，不回显值本身（原则 3.1-8）
        return Err(AppError::Config(
            "导出护栏拒绝：导出文本含本次会话写入过凭据管理器的密钥".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_sk_ant() {
        assert!(assert_no_secret_patterns("sk-ant-abcdefghijk").is_err());
    }

    #[test]
    fn accepts_clean() {
        assert!(assert_no_secret_patterns("INSERT INTO providers VALUES ('id','claude')").is_ok());
    }

    #[test]
    fn rejects_session_known_literal() {
        // §5.5：无格式特征的密钥（正则抓不到）也要被导出护栏拦住。
        let unique = "zq-9f3k1-x7m2p5t8w";
        note_session_secret(unique);
        let leaked = format!("INSERT INTO providers VALUES ('{unique}')");
        let err = assert_no_secret_patterns(&leaked).unwrap_err().to_string();
        assert!(err.contains("导出护栏拒绝"), "unexpected: {err}");
        // 错误信息不得回显值本身
        assert!(!err.contains(unique));
        assert!(assert_no_secret_patterns("INSERT INTO providers VALUES ('other')").is_ok());
    }
}
