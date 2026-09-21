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

    /// 读取用户级环境变量（只读 HKCU）。S3：值以 `Zeroizing` 承载，不降级成裸 String。
    fn get(&self, name: &str) -> Result<Option<Zeroizing<String>>, AppError>;

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
    if name_upper.starts_with("PROGRAMFILES") {
        return Err(AppError::Config(format!(
            "Forbidden environment variable: {name}"
        )));
    }

    // 附录 C 白名单：`ANTHROPIC_*` / `OPENAI_API_KEY` / `CC_SWITCH_*`，
    // 外加 Claude extra_env 里**经 `is_sensitive_config_key` 认定**的键。
    // 不用宽松的后缀匹配：那等于允许往用户环境里写任意 `*_KEY`。
    if (name_upper.starts_with("ANTHROPIC_")
        || name_upper.starts_with("CC_SWITCH_")
        || name_upper == "OPENAI_API_KEY"
        || crate::secrets::is_sensitive_config_key(name))
        && name_upper
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
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
    /// 测试注入：置位后对该变量名的 `set` 返回 `Err`，模拟注册表写入失败，
    /// 供切换投递的事务回滚用例（T-7 事务性）断言"写新失败→旧值原样回写"。
    fail_set_for: Arc<Mutex<Option<String>>>,
}

impl InMemoryEnvSink {
    // 生产代码一律走 `default_sink()`（用 `Default`），`new()` 只服务单元测试。
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前变量快照（集成测试据此断言 §5.3.3 的投递与撤销结果）。
    pub fn snapshot(&self) -> HashMap<String, String> {
        self.vars
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.to_string()))
            .collect()
    }

    /// 测试注入：让**下一次**对该变量名的 `set` 失败一次（一次性，模拟注册表偶发写失败），
    /// 供切换投递的事务回滚用例（T-7 事务性）断言"写新失败→旧值原样回写"。
    pub fn fail_set_for(&self, name: &str) {
        *self.fail_set_for.lock().unwrap() = Some(name.to_string());
    }
}

impl EnvSink for InMemoryEnvSink {
    fn set(&self, name: &str, value: &Zeroizing<String>) -> Result<(), AppError> {
        validate_env_name(name)?;
        // 一次性：命中即清除注入，随后的回滚写回不再被挡。
        let mut inject = self.fail_set_for.lock().unwrap();
        if inject.as_deref() == Some(name) {
            *inject = None;
            return Err(AppError::Config(format!("injected set failure for {name}")));
        }
        drop(inject);
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

    fn get(&self, name: &str) -> Result<Option<Zeroizing<String>>, AppError> {
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

    fn get(&self, name: &str) -> Result<Option<Zeroizing<String>>, AppError> {
        use winreg::enums::*;
        use winreg::RegKey;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let env = hkcu
            .open_subkey_with_flags("Environment", KEY_QUERY_VALUE)
            .map_err(|e| AppError::Config(format!("Failed to open HKCU\\Environment: {e}")))?;

        match env.get_value::<String, _>(name) {
            Ok(value) => Ok(Some(Zeroizing::new(value))),
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

    fn get(&self, _name: &str) -> Result<Option<Zeroizing<String>>, AppError> {
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
            sink.get("CC_SWITCH_TEST").unwrap().map(|v| v.to_string()),
            Some("value1".to_string())
        );

        sink.remove("CC_SWITCH_TEST").unwrap();
        assert!(sink.get("CC_SWITCH_TEST").unwrap().is_none());
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

    /// §10 要求的真实注册表往返测试的 RAII 清理：变量在 `Drop` 里删除，
    /// 断言失败也不会往 `HKCU\Environment` 留残留。
    #[cfg(target_os = "windows")]
    struct TestEnvVarGuard {
        sink: WindowsUserEnvSink,
        name: &'static str,
    }

    #[cfg(target_os = "windows")]
    impl TestEnvVarGuard {
        fn new(name: &'static str) -> Self {
            Self {
                sink: WindowsUserEnvSink::new(),
                name,
            }
        }
    }

    #[cfg(target_os = "windows")]
    impl Drop for TestEnvVarGuard {
        fn drop(&mut self) {
            let _ = self.sink.remove(self.name);
        }
    }

    /// 计划 §10 / 附录 C：`env_sink_windows_roundtrip` —— 真实读写 `HKCU\Environment`，
    /// 只使用 `CC_SWITCH_TEST_*` 变量名，清理放在 `Drop` 中。
    ///
    /// 跑法：`cargo test --lib env_sink_windows_roundtrip -- --ignored`
    ///
    /// 这里刻意**不走** `default_sink()`：它在 `CC_SWITCH_TEST_HOME` 存在时返回内存实现，
    /// 那样就测不到真实的注册表分支。
    #[test]
    #[ignore = "写入真实 HKCU\\Environment：cargo test --lib env_sink_windows_roundtrip -- --ignored"]
    #[cfg(target_os = "windows")]
    fn env_sink_windows_roundtrip() {
        let name = "CC_SWITCH_TEST_ENV_SINK_ROUNDTRIP";
        let guard = TestEnvVarGuard::new(name);
        let value = |v: &str| Zeroizing::new(v.to_string());

        guard.sink.set(name, &value("roundtrip-0001")).unwrap();
        assert_eq!(
            guard.sink.get(name).unwrap().as_deref().map(String::as_str),
            Some("roundtrip-0001")
        );

        // 覆盖写：REG_SZ 更新后读回应为新值
        guard.sink.set(name, &value("roundtrip-0002")).unwrap();
        assert_eq!(
            guard.sink.get(name).unwrap().as_deref().map(String::as_str),
            Some("roundtrip-0002")
        );

        // WM_SETTINGCHANGE 广播（超时只记日志，不返回错误）
        guard.sink.broadcast().unwrap();

        guard.sink.remove(name).unwrap();
        assert!(guard.sink.get(name).unwrap().is_none());

        // guard 的 Drop 会再删一次（兜住 panic 路径）；显式 drop 后再读一次注册表，
        // 证明测试结束时 `HKCU\Environment` 里没有残留。
        drop(guard);
        assert_eq!(WindowsUserEnvSink::new().get(name).unwrap(), None);
    }
}
