use crate::error::AppError;
use regex::Regex;
use std::sync::OnceLock;

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
}
