/// Security rules for identifying sensitive configuration keys
/// Moved from provider.rs:5821 is_sensitive_config_key

/// Check if a configuration key name is sensitive (should be treated as secret)
/// Used for Claude extra_env and Pi headers
pub fn is_sensitive_config_key(key: &str) -> bool {
    let key_lower = key.to_lowercase();

    // Patterns that indicate sensitive data
    let sensitive_patterns = [
        "key",
        "token",
        "secret",
        "password",
        "pass",
        "auth",
        "credential",
        "apikey",
        "api_key",
        "bearer",
        "access",
        "private",
    ];

    sensitive_patterns.iter().any(|pattern| key_lower.contains(pattern))
}

/// Check if a Pi apiKey or header value is a literal (not a variable reference)
/// Literals: plain text, or escaped variables ($$VAR, $!VAR)
/// Not literals: $VAR, !command
pub fn is_literal_value(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }

    // If starts with $ or ! without escaping, it's a variable/command reference
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
        assert!(is_sensitive_config_key("SECRET_VALUE"));
        assert!(is_sensitive_config_key("PASSWORD"));
        assert!(is_sensitive_config_key("access_token"));

        assert!(!is_sensitive_config_key("MODEL_NAME"));
        assert!(!is_sensitive_config_key("BASE_PATH"));
        assert!(!is_sensitive_config_key("TIMEOUT"));
    }

    #[test]
    fn test_is_literal_value() {
        // Literals
        assert!(is_literal_value("sk-1234567890"));
        assert!(is_literal_value("plain text"));
        assert!(is_literal_value("$$VAR"));  // Escaped variable
        assert!(is_literal_value("$!COMMAND"));  // Escaped command
        assert!(is_literal_value(""));

        // Not literals
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
}
