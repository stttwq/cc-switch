/// 敏感配置键判定：显式黑名单 + 后缀匹配。
/// 不用子串 contains，避免误伤 apiKeyHelper / includeCoAuthoredBy / MAX_OUTPUT_TOKENS。

const EXACT_SECRET_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "OPENAI_API_KEY",
    "apiKey",
    "api_key",
    "OPENROUTER_API_KEY",
    "Authorization",
    "AUTH_TOKEN",
    "ACCESS_TOKEN",
    "BEARER_TOKEN",
    "PASSWORD",
    "SECRET",
];

const SECRET_SUFFIXES: &[&str] = &[
    "_api_key",
    "_auth_token",
    "_access_token",
    "_secret_access_key",
    "_secret",
    "_password",
    "_bearer_token",
];

/// Check if a configuration key name is sensitive (should be treated as secret)
/// Used for Claude extra_env and Pi headers
pub fn is_sensitive_config_key(key: &str) -> bool {
    if EXACT_SECRET_KEYS
        .iter()
        .any(|known| key.eq_ignore_ascii_case(known))
    {
        return true;
    }

    let key_lower = key.to_lowercase();
    if key_lower.ends_with("tokens") || key_lower.contains("max_output") {
        return false;
    }
    SECRET_SUFFIXES.iter().any(|suffix| key_lower.ends_with(suffix))
}

/// Check if a Pi apiKey or header value is a literal (not a variable reference)
/// Literals: plain text, or escaped variables ($$VAR, $!VAR)
/// Not literals: $VAR, !command
pub fn is_literal_value(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }

    if value.starts_with('$') && !value.starts_with("$$") && !value.starts_with("$!") {
        return false;
    }
    if value.starts_with('!') && !value.starts_with("$!") {
        return false;
    }

    true
}

/// Unescape a literal value ($$VAR -> $VAR, $!VAR -> !VAR)
pub fn unescape_literal(value: &str) -> String {
    if value.starts_with("$$") {
        value[1..].to_string()
    } else if value.starts_with("$!") {
        value[1..].to_string()
    } else {
        value.to_string()
    }
}

/// Escape a literal value for Pi format ($VAR -> $$VAR)
pub fn escape_literal(value: &str) -> String {
    if value.starts_with('$') || value.starts_with('!') {
        format!("${}", value)
    } else {
        value.to_string()
    }
}

/// 附录 C：Pi provider id / header 名规范化为环境变量片段。
pub fn normalize_env_key_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_underscore = false;
    for ch in raw.chars() {
        let up = ch.to_ascii_uppercase();
        if up.is_ascii_alphanumeric() {
            out.push(up);
            prev_underscore = false;
        } else if !prev_underscore {
            out.push('_');
            prev_underscore = true;
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.len() <= 64 {
        trimmed
    } else {
        trimmed.chars().take(64).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_sensitive_config_key() {
        assert!(is_sensitive_config_key("API_KEY"));
        assert!(is_sensitive_config_key("api_key"));
        assert!(is_sensitive_config_key("OPENROUTER_API_KEY"));
        assert!(is_sensitive_config_key("AUTH_TOKEN"));
        assert!(is_sensitive_config_key("bearer_token"));
        assert!(is_sensitive_config_key("MY_SECRET"));
        assert!(is_sensitive_config_key("PASSWORD"));
        assert!(is_sensitive_config_key("access_token"));
        assert!(is_sensitive_config_key("MY_PASSWORD"));

        assert!(!is_sensitive_config_key("MODEL_NAME"));
        assert!(!is_sensitive_config_key("BASE_PATH"));
        assert!(!is_sensitive_config_key("TIMEOUT"));
        assert!(!is_sensitive_config_key("apiKeyHelper"));
        assert!(!is_sensitive_config_key("includeCoAuthoredBy"));
        assert!(!is_sensitive_config_key("CLAUDE_CODE_MAX_OUTPUT_TOKENS"));
        assert!(!is_sensitive_config_key("awsAuthRefresh"));
    }

    #[test]
    fn test_is_literal_value() {
        assert!(is_literal_value("sk-1234567890"));
        assert!(is_literal_value("plain text"));
        assert!(is_literal_value("$$VAR"));
        assert!(is_literal_value("$!COMMAND"));
        assert!(is_literal_value(""));

        assert!(!is_literal_value("$VAR"));
        assert!(!is_literal_value("$ENV_KEY"));
        assert!(!is_literal_value("!command"));
    }

    #[test]
    fn test_unescape_literal() {
        assert_eq!(unescape_literal("$$VAR"), "$VAR");
        assert_eq!(unescape_literal("$!COMMAND"), "!COMMAND");
        assert_eq!(unescape_literal("plain"), "plain");
        assert_eq!(unescape_literal("sk-12345"), "sk-12345");
    }

    #[test]
    fn test_escape_literal() {
        assert_eq!(escape_literal("$VAR"), "$$VAR");
        assert_eq!(escape_literal("!COMMAND"), "$!COMMAND");
        assert_eq!(escape_literal("plain"), "plain");
        assert_eq!(escape_literal("sk-12345"), "sk-12345");
    }

    #[test]
    fn test_normalize_env_key_segment() {
        assert_eq!(normalize_env_key_segment("claude-1"), "CLAUDE_1");
        assert_eq!(normalize_env_key_segment("foo--bar"), "FOO_BAR");
        assert_eq!(normalize_env_key_segment("abc"), "ABC");
    }
}
