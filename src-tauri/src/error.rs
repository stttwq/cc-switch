use std::path::Path;
use std::sync::PoisonError;

use thiserror::Error;

/// `Localized` 错误的 Display 渲染。`vault_*` key（1Password 后端错误分类）输出
/// JSON 字符串，保证 `to_string()` / 序列化 / `format!` 包装等所有路径一致，
/// 前端可解析出 code；其余 key 维持原有的 `{zh} ({en})` 双语格式。
fn render_localized(key: &str, zh: &str, en: &str) -> String {
    if key.starts_with("vault_") {
        serde_json::json!({ "code": key, "message": zh, "messageEn": en }).to_string()
    } else {
        format!("{zh} ({en})")
    }
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error("配置错误: {0}")]
    Config(String),
    #[error("无效输入: {0}")]
    InvalidInput(String),
    /// Native files changed after CC Switch last read them.
    #[error("并发冲突: {0}")]
    Conflict(String),
    #[error("IO 错误: {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{context}: {source}")]
    IoContext {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("JSON 解析错误: {path}: {source}")]
    Json {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("JSON 序列化失败: {source}")]
    JsonSerialize {
        #[source]
        source: serde_json::Error,
    },
    #[error("TOML 解析错误: {path}: {source}")]
    Toml {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("锁获取失败: {0}")]
    Lock(String),
    #[error("MCP 校验失败: {0}")]
    McpValidation(String),
    #[error("{0}")]
    Message(String),
    #[error("HTTP {status}: {body}")]
    HttpStatus { status: u16, body: String },
    /// F5-3：`vault_*` 错误对前端渲染为 JSON 字符串 `{"code":"vault_locked","message":"…"}`，
    /// 前端按 code 映射 i18n 并提供「重试」（沿用 ENV_CONFLICT 的 JSON-in-string 约定）；
    /// 其它 key 维持 `{zh} ({en})`。CLI 侧取文案直接读字段（`error_message_zh/en`），不经这里。
    #[error("{rendered}", rendered = render_localized(.key, .zh, .en))]
    Localized {
        key: &'static str,
        zh: String,
        en: String,
    },
    #[error("数据库错误: {0}")]
    Database(String),
    #[error("凭据存储错误: {0}")]
    SecretStoreError(String),
}

impl AppError {
    pub fn io(path: impl AsRef<Path>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.as_ref().display().to_string(),
            source,
        }
    }

    pub fn json(path: impl AsRef<Path>, source: serde_json::Error) -> Self {
        Self::Json {
            path: path.as_ref().display().to_string(),
            source,
        }
    }

    pub fn toml(path: impl AsRef<Path>, source: toml::de::Error) -> Self {
        Self::Toml {
            path: path.as_ref().display().to_string(),
            source,
        }
    }

    pub fn localized(key: &'static str, zh: impl Into<String>, en: impl Into<String>) -> Self {
        Self::Localized {
            key,
            zh: zh.into(),
            en: en.into(),
        }
    }
}

impl<T> From<PoisonError<T>> for AppError {
    fn from(err: PoisonError<T>) -> Self {
        Self::Lock(err.to_string())
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Database(err.to_string())
    }
}

impl From<AppError> for String {
    fn from(err: AppError) -> Self {
        err.to_string()
    }
}

impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

/// 格式化为 JSON 错误字符串，前端可解析为结构化错误
pub fn format_skill_error(
    code: &str,
    context: &[(&str, &str)],
    suggestion: Option<&str>,
) -> String {
    use serde_json::json;

    let mut ctx_map = serde_json::Map::new();
    for (key, value) in context {
        ctx_map.insert(key.to_string(), json!(value));
    }

    let error_obj = json!({
        "code": code,
        "context": ctx_map,
        "suggestion": suggestion,
    });

    serde_json::to_string(&error_obj).unwrap_or_else(|_| {
        // 如果 JSON 序列化失败，返回简单格式
        format!("ERROR:{code}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_localized_renders_json_payload() {
        let err = AppError::localized(
            "vault_locked",
            "1Password 已锁定，请解锁后重试",
            "1Password is locked; unlock it and retry",
        );
        let text = err.to_string();
        let parsed: serde_json::Value =
            serde_json::from_str(&text).expect("vault_* 错误的 Display 应输出可解析的 JSON");
        assert_eq!(parsed["code"], "vault_locked");
        assert_eq!(parsed["message"], "1Password 已锁定，请解锁后重试");
        assert_eq!(
            parsed["messageEn"],
            "1Password is locked; unlock it and retry"
        );
    }

    #[test]
    fn non_vault_localized_keeps_bilingual_format() {
        let err = AppError::localized(
            "env_delivery.adopt_strict",
            "严格模式下不可用",
            "unavailable in strict mode",
        );
        assert_eq!(
            err.to_string(),
            "严格模式下不可用 (unavailable in strict mode)"
        );
    }
}
