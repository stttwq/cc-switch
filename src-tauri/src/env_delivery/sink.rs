//! Environment variable sink implementations
//!
//! Provides write access to user-level environment variables with:
//! - Name whitelisting (only CC_SWITCH_*, ANTHROPIC_*, OPENAI_API_KEY allowed)
//! - Broadcast support (WM_SETTINGCHANGE for Windows Explorer refresh)
//! - Conflict detection

use crate::error::AppError;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Trait for writing user-level environment variables
pub trait EnvSink: Send + Sync {
    /// Set an environment variable
    fn set(&self, name: &str, value: &str) -> Result<(), AppError>;

    /// Remove an environment variable
    fn remove(&self, name: &str) -> Result<(), AppError>;

    /// Get an environment variable value (read-only from HKCU)
    fn get(&self, name: &str) -> Result<Option<String>, AppError>;

    /// Broadcast WM_SETTINGCHANGE after a batch of operations
    fn broadcast(&self) -> Result<(), AppError>;
}

/// Validate environment variable name against whitelist
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

    // Whitelist patterns (case-sensitive for actual check)
    if name.starts_with("ANTHROPIC_")
        || name.starts_with("CC_SWITCH_")
        || name == "OPENAI_API_KEY"
        || crate::secrets::is_sensitive_config_key(name)
    {
        return Ok(());
    }

    Err(AppError::Config(format!(
        "Environment variable name not in whitelist: {name}"
    )))
}

// ─── In-memory implementation for testing ────────────────────

/// In-memory environment variable sink for testing
#[derive(Clone, Default)]
pub struct InMemoryEnvSink {
    vars: Arc<Mutex<HashMap<String, String>>>,
}

impl InMemoryEnvSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read all stored variables (test helper)
    pub fn snapshot(&self) -> HashMap<String, String> {
        self.vars.lock().unwrap().clone()
    }
}

impl EnvSink for InMemoryEnvSink {
    fn set(&self, name: &str, value: &str) -> Result<(), AppError> {
        validate_env_name(name)?;
        self.vars.lock().unwrap().insert(name.to_string(), value.to_string());
        Ok(())
    }

    fn remove(&self, name: &str) -> Result<(), AppError> {
        validate_env_name(name)?;
        self.vars.lock().unwrap().remove(name);
        Ok(())
    }

    fn get(&self, name: &str) -> Result<Option<String>, AppError> {
        Ok(self.vars.lock().unwrap().get(name).cloned())
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
    fn set(&self, name: &str, value: &str) -> Result<(), AppError> {
        validate_env_name(name)?;

        use winreg::enums::*;
        use winreg::RegKey;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let env = hkcu
            .open_subkey_with_flags("Environment", KEY_SET_VALUE | KEY_QUERY_VALUE)
            .map_err(|e| AppError::Config(format!("Failed to open HKCU\\Environment: {e}")))?;

        // Write as REG_SZ (not REG_EXPAND_SZ, values may contain %)
        env.set_value(name, &value)
            .map_err(|e| AppError::Config(format!("Failed to set {name}: {e}")))?;

        // Update current process environment so cc-switch spawned processes see it
        std::env::set_var(name, value);

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
    fn set(&self, _name: &str, _value: &str) -> Result<(), AppError> {
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
        assert!(sink.set("CC_SWITCH_TEST", "value1").is_ok());
        assert_eq!(sink.get("CC_SWITCH_TEST").unwrap(), Some("value1".to_string()));

        sink.remove("CC_SWITCH_TEST").unwrap();
        assert_eq!(sink.get("CC_SWITCH_TEST").unwrap(), None);
    }

    #[test]
    fn in_memory_sink_enforces_whitelist() {
        let sink = InMemoryEnvSink::new();
        assert!(sink.set("PATH", "bad").is_err());
        assert!(sink.set("RANDOM_VAR", "bad").is_err());
    }
}
