/// 敏感配置键判定的**唯一实现**（计划 §5.2.3：mod.rs 那份搬到此处）。
/// 覆盖 Anthropic / OpenAI / OpenRouter / Google / AWS Bedrock / Vertex 等
/// `*_API_KEY`、裸 `*_KEY`、各类 `*_TOKEN`（单数，不误伤 `*_TOKENS` 共享配置）、
/// `*_SECRET` / `*SECRET*`、口令类缩写与 `CREDENTIAL` / `PRIVATE_KEY` 等惯用命名。
/// 用显式名单 + 后缀 + 有限子串，避免误伤 `apiKeyHelper` / `includeCoAuthoredBy` /
/// `CLAUDE_CODE_MAX_OUTPUT_TOKENS` / `awsAuthRefresh`。
pub fn is_sensitive_config_key(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();

    const SENSITIVE_SUFFIXES: &[&str] = &[
        // 裸 `_KEY` 是最常见的凭据写法（OPENAI_KEY / GROQ_KEY / XAI_KEY…），
        // 必须单列；下面几条 `_*_KEY` 被它蕴含，保留只为说明覆盖面。
        "_KEY",
        "_API_KEY",
        "_ACCESS_KEY",
        "_ACCESS_KEY_ID",
        "_KEY_ID",
        "_PRIVATE_KEY",
        // 不带分隔符的复合写法各自成后缀：`_KEY` 够不着 `..._APIKEY`。
        "_APIKEY",
        "_ACCESSKEY",
        "_SECRETKEY",
        "_APITOKEN",
        "_AUTH_TOKEN",
        // 单数 `_TOKEN` 命中 AWS_SESSION_TOKEN 等，但**不**误伤复数 `_TOKENS`。
        "_TOKEN",
        // GITHUB_PAT / GITLAB_PAT 等 personal access token 惯用写法。
        "_PAT",
        // 口令类缩写：`_PASS` 不误伤 `*_BYPASS`，`_PWD` 不误伤 shell 的 PWD/OLDPWD。
        "_PWD",
        "_PASS",
        "_PASSPHRASE",
        "_CREDS",
    ];
    const SENSITIVE_EXACT: &[&str] = &[
        "APIKEY",
        "API_KEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "CREDENTIALS",
    ];
    // contains：覆盖 AWS_SECRET_ACCESS_KEY / *_CLIENT_SECRET /
    // GOOGLE_APPLICATION_CREDENTIALS / AWS_BEARER_TOKEN_BEDROCK，以及无分隔符的
    // HTTP 认证头 `Authorization` / `Proxy-Authorization`（后缀规则够不着它们）。
    const SENSITIVE_CONTAINS: &[&str] = &[
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "PRIVATE_KEY",
        "BEARER_TOKEN",
        "AUTHORIZATION",
    ];

    SENSITIVE_EXACT.contains(&upper.as_str())
        || SENSITIVE_SUFFIXES.iter().any(|s| upper.ends_with(s))
        || SENSITIVE_CONTAINS.iter().any(|c| upper.contains(c))
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
    if value.starts_with("$$") || value.starts_with("$!") {
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

/// 按 Unicode 标量取末 n 个字符，避免按字节切片 panic。
pub fn last_chars(value: &str, n: usize) -> &str {
    match value.char_indices().nth_back(n.saturating_sub(1)) {
        Some((i, _)) if n > 0 => &value[i..],
        _ => value,
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

/// 附录 C：Pi API key 环境变量名。
pub fn pi_api_key_env_name(provider_id: &str) -> String {
    format!(
        "CC_SWITCH_PI_{}_API_KEY",
        normalize_env_key_segment(provider_id)
    )
}

/// 附录 C：Pi 敏感 header 环境变量名。
pub fn pi_header_env_name(provider_id: &str, header_name: &str) -> String {
    format!(
        "CC_SWITCH_PI_{}_HEADER_{}",
        normalize_env_key_segment(provider_id),
        normalize_env_key_segment(header_name)
    )
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
        // 合并自 mod.rs 宽规则后新增覆盖：无分隔符认证头、裸 _KEY、PAT 等。
        assert!(is_sensitive_config_key("Authorization"));
        assert!(is_sensitive_config_key("Proxy-Authorization"));
        assert!(is_sensitive_config_key("GITHUB_PAT"));
        assert!(is_sensitive_config_key("AWS_SECRET_ACCESS_KEY"));

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

    #[test]
    fn last_chars_handles_non_ascii() {
        assert_eq!(last_chars("密钥测试中文", 4), "测试中文");
        assert_eq!(last_chars("ab", 4), "ab");
    }
}
