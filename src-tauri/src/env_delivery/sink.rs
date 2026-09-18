//! Environment variable sink implementations
//!
//! Provides write access to user-level environment variables with:
//! - Name whitelisting (only CC_SWITCH_*, ANTHROPIC_*, OPENAI_API_KEY allowed)
//! - Broadcast support (WM_SETTINGCHANGE for Windows Explorer refresh)
//! - Conflict detection

use crate::error::AppError;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

/// 返回当前环境应使用的 EnvSink。
///
/// 集成测试（设置了 `CC_SWITCH_TEST_HOME`）返回内存实现，避免读写真实的
/// `HKCU\Environment`；生产环境返回平台实现。
pub fn default_sink() -> Arc<dyn EnvSink> {
    if std::env::var_os("CC_SWITCH_TEST_HOME").is_some() {
        Arc::new(InMemoryEnvSink::default())
    } else {
        #[cfg(target_os = "windows")]
        {
            Arc::new(WindowsUserEnvSink::new())
        }
        #[cfg(not(target_os = "windows"))]
        {
            Arc::new(UnsupportedEnvSink::new())
        }
    }
}

/// Trait for writing user-level environment variables
pub trait EnvSink: Send + Sync {
    /// Set an environment variable (value is zeroized by the caller's `Zeroizing` on drop)
    fn set(&self, name: &str, value: &Zeroizing<String>) -> Result<(), AppError>;

    /// Remove an environment variable
    fn remove(&self, name: &str) -> Result<(), AppError>;

    /// Get an environment variable value (read-only from HKCU)
    fn get(&self, name: &str) -> Result<Option<String>, AppError>;

    /// Broadcast WM_SETTINGCHANGE after a batch of operations
    fn broadcast(&self) -> Result<(), AppError>;
}

/// Validate environment variable name against whitelist
fn is_env_sink_secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with("_api_key")
        || lower.ends_with("_auth_token")
        || lower.ends_with("_access_token")
        || lower.ends_with("_secret")
        || lower.ends_with("_password")
        || lower.ends_with("_bearer_token")
        || lower.ends_with("_key")
        || lower.ends_with("_token")
}

fn validate_env_name(name: &str) -> Result<(), AppError> {
    // Forbidden system variables (case-insensitive)
    const FORBIDDEN: &[&str] = &[
        "PATH",
        "PATHEXT",
        "COMSPEC",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "HOMEPATH",
        "HOMEDRIVE",
        "SYSTEMROOT",
        "WINDIR",
        "APPDATA",
        "LOCALAPPDATA",
        "PROGRAMFILES",
        "PROGRAMFILES(X86)",
        "PSMODULEPATH",
    ];

    let name_upper = name.to_uppercase();
    for forbidden in FORBIDDEN {
        if name_upper == *forbidden {
            return Err(AppError::Config(format!(
                "Forbidden environment variable: {name}"
            )));
        }
    }

    // 附录 C：独立白名单，不复用提取规则。
    if name.starts_with("ANTHROPIC_")
        || name.starts_with("CC_SWITCH_")
        || name == "OPENAI_API_KEY"
        || is_env_sink_secret_name(name)
    {
        return Ok(());
    }

    Err(AppError::Config(format!(
        "Environment variable name not in whitelist: {name}"
    )))
}

// ─── In-memory implementation for testing ────────────────────

/// In-memory environment variable sink for testing
///
/// 当进程环境存在 `CC_SWITCH_TEST_HOME` 时，`default_sink()` 会返回此实现，
/// 保证集成测试不会读写真实的 HKCU\Environment。
#[derive(Clone, Default)]
pub struct InMemoryEnvSink {
    vars: Arc<Mutex<HashMap<String, Zeroizing<String>>>>,
}

impl InMemoryEnvSink {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }
}

impl EnvSink for InMemoryEnvSink {
    fn set(&self, name: &str, value: &Zeroizing<String>) -> Result<(), AppError> {
        validate_env_name(name)?;
        self.vars
            .lock()
            .unwrap()
            .insert(name.to_string(), value.clone());
        Ok(())
    }

    fn remove(&self, name: &str) -> Result<(), AppError> {
        validate_env_name(name)?;
        self.vars.lock().unwrap().remove(name);
        Ok(())
    }

    fn get(&self, name: &str) -> Result<Option<String>, AppError> {
        Ok(self.vars.lock().unwrap().get(name).map(|v| v.to_string()))
    }

    fn broadcast(&self) -> Result<(), AppError> {
        // No-op for in-memory
        Ok(())
    }
}

// ─── Windows implementation ──────────────────────────────────

#[cfg(target_os = "windows")]
pub struct WindowsUserEnvSink;

#[cfg(target_os = "windows")]
impl WindowsUserEnvSink {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(target_os = "windows")]
impl EnvSink for WindowsUserEnvSink {
    fn set(&self, name: &str, value: &Zeroizing<String>) -> Result<(), AppError> {
        validate_env_name(name)?;

        use winreg::enums::*;
        use winreg::RegKey;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let env = hkcu
            .open_subkey_with_flags("Environment", KEY_SET_VALUE | KEY_QUERY_VALUE)
            .map_err(|e| AppError::Config(format!("Failed to open HKCU\\Environment: {e}")))?;

        // Write as REG_SZ (not REG_EXPAND_SZ, values may contain %)
        env.set_value(name, &value.as_str().to_string())
            .map_err(|e| AppError::Config(format!("Failed to set {name}: {e}")))?;

        // Update current process environment so cc-switch spawned processes see it
        std::env::set_var(name, value.as_str());

        Ok(())
    }

    fn remove(&self, name: &str) -> Result<(), AppError> {
        validate_env_name(name)?;

        use winreg::enums::*;
        use winreg::RegKey;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let env = hkcu
            .open_subkey_with_flags("Environment", KEY_SET_VALUE)
            .map_err(|e| AppError::Config(format!("Failed to open HKCU\\Environment: {e}")))?;

        // Delete value (ignore if doesn't exist)
        let _ = env.delete_value(name);

        // Update current process environment
        std::env::remove_var(name);

        Ok(())
    }

    fn get(&self, name: &str) -> Result<Option<String>, AppError> {
        use winreg::enums::*;
        use winreg::RegKey;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let env = hkcu
            .open_subkey_with_flags("Environment", KEY_QUERY_VALUE)
            .map_err(|e| AppError::Config(format!("Failed to open HKCU\\Environment: {e}")))?;

        match env.get_value::<String, _>(name) {
            Ok(value) => Ok(Some(value)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(AppError::Config(format!("Failed to read {name}: {e}"))),
        }
    }

    fn broadcast(&self) -> Result<(), AppError> {
        #[cfg(target_os = "windows")]
        unsafe {
            use std::ptr;
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
            };

            let env_str: Vec<u16> = "Environment\0".encode_utf16().collect();
            let result = SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                env_str.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                5000,
                ptr::null_mut(),
            );

            if result == 0 {
                log::warn!("SendMessageTimeoutW returned 0 (broadcast may have timed out)");
            }
        }
        Ok(())
    }
}

// ─── Unsupported platform stub ───────────────────────────────

#[cfg(not(target_os = "windows"))]
pub struct UnsupportedEnvSink;

#[cfg(not(target_os = "windows"))]
impl UnsupportedEnvSink {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(not(target_os = "windows"))]
impl EnvSink for UnsupportedEnvSink {
    fn set(&self, _name: &str, _value: &Zeroizing<String>) -> Result<(), AppError> {
        Err(AppError::Config(
            "Environment variable delivery not supported on this platform".to_string(),
        ))
    }

    fn remove(&self, _name: &str) -> Result<(), AppError> {
        Err(AppError::Config(
            "Environment variable delivery not supported on this platform".to_string(),
        ))
    }

    fn get(&self, _name: &str) -> Result<Option<String>, AppError> {
        Err(AppError::Config(
            "Environment variable delivery not supported on this platform".to_string(),
        ))
    }

    fn broadcast(&self) -> Result<(), AppError> {
        Err(AppError::Config(
            "Environment variable delivery not supported on this platform".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_env_name_accepts_whitelist() {
        assert!(validate_env_name("ANTHROPIC_API_KEY").is_ok());
        assert!(validate_env_name("ANTHROPIC_BASE_URL").is_ok());
        assert!(validate_env_name("CC_SWITCH_CODEX_API_KEY").is_ok());
        assert!(validate_env_name("CC_SWITCH_PI_DEFAULT_API_KEY").is_ok());
        assert!(validate_env_name("OPENAI_API_KEY").is_ok());
        assert!(validate_env_name("OPENROUTER_API_KEY").is_ok());
        assert!(validate_env_name("MY_KEY").is_ok());
    }

    #[test]
    fn validate_env_name_rejects_forbidden() {
        assert!(validate_env_name("PATH").is_err());
        assert!(validate_env_name("path").is_err());
        assert!(validate_env_name("Path").is_err());
        assert!(validate_env_name("TEMP").is_err());
        assert!(validate_env_name("USERPROFILE").is_err());
        assert!(validate_env_name("SYSTEMROOT").is_err());
    }

    #[test]
    fn validate_env_name_rejects_non_whitelist() {
        assert!(validate_env_name("MY_CUSTOM_VAR").is_err());
        assert!(validate_env_name("OPENAI_ORG_ID").is_err());
    }

    #[test]
    fn in_memory_sink_roundtrip() {
        let sink = InMemoryEnvSink::new();
        assert!(sink
            .set("CC_SWITCH_TEST", &Zeroizing::new("value1".to_string()))
            .is_ok());
        assert_eq!(
            sink.get("CC_SWITCH_TEST").unwrap(),
            Some("value1".to_string())
        );

        sink.remove("CC_SWITCH_TEST").unwrap();
        assert_eq!(sink.get("CC_SWITCH_TEST").unwrap(), None);
    }

    #[test]
    fn in_memory_sink_enforces_whitelist() {
        let sink = InMemoryEnvSink::new();
        assert!(sink
            .set("PATH", &Zeroizing::new("bad".to_string()))
            .is_err());
        assert!(sink
            .set("RANDOM_VAR", &Zeroizing::new("bad".to_string()))
            .is_err());
    }
}
