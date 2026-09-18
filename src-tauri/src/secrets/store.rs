use crate::error::AppError;
use crate::secrets::target::SecretTarget;
use async_trait::async_trait;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use thiserror::Error;
use zeroize::Zeroizing;

/// Windows 凭据 blob 上限 `CRED_MAX_CREDENTIAL_BLOB_SIZE = 2560` 字节，密码按 UTF-16 存，
/// 即单条值最多 1280 个 UTF-16 单元（计划 §5.1 / 附录 B）。
const MAX_VALUE_UTF16_UNITS: usize = 1280;
/// 附录 B：target 名上限（keyring/`CredWriteW` 的 `TargetName` 上限）。
const MAX_TARGET_CHARS: usize = 32767;

/// 计划 §5.1：`SecretStore` 的类型化错误。
///
/// 错误文案一律只含字段名（target）与上限，**不含密钥值**（原则 3.1-8）。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SecretError {
    /// 非 Windows 平台没有凭据管理器后端
    #[error("secret store is not supported on this platform")]
    Unsupported,
    /// 值超出单条凭据上限（字段名 + 上限，不含值）
    #[error("secret value for target [{field}] exceeds the limit of {max} UTF-16 characters")]
    TooLong { field: String, max: usize },
    /// target 名本身超长（附录 B 的 32767 上限）
    #[error("credential target [{field}] exceeds the limit of {max} characters")]
    TargetTooLong { field: String, max: usize },
    /// 后端（keyring / Credential Manager）错误
    #[error("credential backend error: {0}")]
    Backend(String),
}

/// 对外仍走 `AppError`（`SecretStore` trait 的签名不变，避免波及迁移/提取等调用方），
/// 具体区分信息保留在文案里：字段名 + 上限，不含值。
impl From<SecretError> for AppError {
    fn from(err: SecretError) -> Self {
        AppError::SecretStoreError(err.to_string())
    }
}

/// 计划 §5.1：`Display` 输出即 Credential Manager 的 target 名（附录 B）。
///
/// 实现放在本文件（而不是定义 `SecretTarget` 的 `target.rs`，该文件归并行会话所有），
/// 逻辑直接复用 `to_target_string()`，两个名字保持同一个语义。
impl fmt::Display for SecretTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_target_string())
    }
}

/// Abstract interface for secret storage
/// Windows implementation uses keyring crate with windows-native backend
#[async_trait]
pub trait SecretStore: Send + Sync {
    /// Store a secret
    async fn set(&self, target: &SecretTarget, value: Zeroizing<String>) -> Result<(), AppError>;

    /// Retrieve a secret
    async fn get(&self, target: &SecretTarget) -> Result<Option<Zeroizing<String>>, AppError>;

    /// Delete a secret
    async fn delete(&self, target: &SecretTarget) -> Result<(), AppError>;

    /// Check if the secret store is available (startup self-check)
    async fn probe(&self) -> Result<(), AppError>;

    /// List all targets matching a prefix (for orphan cleanup)
    async fn list_targets(&self, prefix: &str) -> Result<Vec<String>, AppError>;

    /// Convenience method: store a plain string
    async fn store(&self, target: &SecretTarget, value: &str) -> Result<(), AppError> {
        self.set(target, Zeroizing::new(value.to_string())).await
    }

    /// 便捷读取：S3 要求密钥在内存里始终以 `Zeroizing` 承载，
    /// 因此这里不再降级成裸 String，由调用方在真正需要时自行短生命周期地取出。
    async fn retrieve(&self, target: &SecretTarget) -> Result<Option<Zeroizing<String>>, AppError> {
        self.get(target).await
    }
}

#[cfg(target_os = "windows")]
fn windows_entry(target: &SecretTarget) -> Result<keyring::Entry, SecretError> {
    keyring::Entry::new_with_target(
        &target.to_string(),
        SecretTarget::service(),
        &target.to_user_metadata(),
    )
    .map_err(|e| SecretError::Backend(format!("Failed to create entry: {e}")))
}

/// Windows Credential Manager implementation
pub struct WindowsSecretStore {
    write_lock: Mutex<()>,
}

impl WindowsSecretStore {
    pub fn new() -> Result<Self, AppError> {
        #[cfg(not(target_os = "windows"))]
        {
            return Err(AppError::SecretStoreError(
                "Windows Credential Manager is only supported on Windows".to_string(),
            ));
        }

        Ok(Self {
            write_lock: Mutex::new(()),
        })
    }

    /// 计划 §5.1：`keyring` 不保证同一条目的多线程读写顺序，这里用一把 Mutex 把
    /// **所有写操作**（`set` / `delete`，以及 `probe()` 的写-读-删三步）串行化。
    /// 普通 `get` 不持锁（计划只要求写串行化）；probe 在锁内的回读走 `read_locked`。
    fn lock_writes(&self) -> MutexGuard<'_, ()> {
        self.write_lock.lock().unwrap()
    }

    /// 写入单条凭据（不含长度校验，校验在 `set` 里做）。`_guard` 参数用于强制调用方已持锁。
    fn write_locked(
        &self,
        _guard: &MutexGuard<'_, ()>,
        target: &SecretTarget,
        value: &str,
    ) -> Result<(), SecretError> {
        #[cfg(target_os = "windows")]
        {
            windows_entry(target)?
                .set_password(value)
                .map_err(|e| SecretError::Backend(format!("Failed to set secret: {e}")))?;

            Ok(())
        }

        #[cfg(not(target_os = "windows"))]
        {
            let _ = (_guard, target, value);
            Err(SecretError::Unsupported)
        }
    }

    /// 读取单条凭据；`Error::NoEntry` 映射为 `Ok(None)`（计划 §5.1）。
    fn read_locked(
        &self,
        _guard: &MutexGuard<'_, ()>,
        target: &SecretTarget,
    ) -> Result<Option<Zeroizing<String>>, SecretError> {
        #[cfg(target_os = "windows")]
        {
            match windows_entry(target)?.get_password() {
                Ok(password) => Ok(Some(Zeroizing::new(password))),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(SecretError::Backend(format!("Failed to get secret: {e}"))),
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            let _ = (_guard, target);
            Err(SecretError::Unsupported)
        }
    }

    /// 删除单条凭据；条目不存在视为成功（幂等）。
    fn delete_locked(
        &self,
        _guard: &MutexGuard<'_, ()>,
        target: &SecretTarget,
    ) -> Result<(), SecretError> {
        #[cfg(target_os = "windows")]
        {
            match windows_entry(target)?.delete_credential() {
                Ok(()) => Ok(()),
                Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(SecretError::Backend(format!(
                    "Failed to delete secret: {e}"
                ))),
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            let _ = (_guard, target);
            Err(SecretError::Unsupported)
        }
    }
}

#[async_trait]
impl SecretStore for WindowsSecretStore {
    async fn set(&self, target: &SecretTarget, value: Zeroizing<String>) -> Result<(), AppError> {
        #[cfg(target_os = "windows")]
        {
            let field = target.to_string();
            let utf16_len = value.encode_utf16().count();
            if utf16_len > MAX_VALUE_UTF16_UNITS {
                return Err(SecretError::TooLong {
                    field: target.to_string(),
                    max: MAX_VALUE_UTF16_UNITS,
                }
                .into());
            }
            if field.chars().count() > MAX_TARGET_CHARS {
                return Err(SecretError::TargetTooLong {
                    field,
                    max: MAX_TARGET_CHARS,
                }
                .into());
            }

            let _guard = self.write_lock.lock().unwrap();
            let entry = windows_entry(target)?;

            entry
                .set_password(&value)
                .map_err(|e| AppError::SecretStoreError(format!("Failed to set secret: {}", e)))?;

            Ok(())
        }

        #[cfg(not(target_os = "windows"))]
        {
            let _ = (target, value);
            Err(AppError::SecretStoreError(
                "Not supported on this platform".to_string(),
            ))
        }
    }

    async fn get(&self, target: &SecretTarget) -> Result<Option<Zeroizing<String>>, AppError> {
        #[cfg(target_os = "windows")]
        {
            let entry = windows_entry(target)?;

            match entry.get_password() {
                Ok(password) => Ok(Some(Zeroizing::new(password))),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(AppError::SecretStoreError(format!(
                    "Failed to get secret: {}",
                    e
                ))),
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            let _ = target;
            Err(AppError::SecretStoreError(
                "Not supported on this platform".to_string(),
            ))
        }
    }

    async fn delete(&self, target: &SecretTarget) -> Result<(), AppError> {
        #[cfg(target_os = "windows")]
        {
            // §5.1：删除同样是写操作，与 set / probe 共用这把锁串行化
            let _guard = self.lock_writes();
            let entry = windows_entry(target)?;

            match entry.delete_credential() {
                Ok(()) => Ok(()),
                Err(keyring::Error::NoEntry) => Ok(()), // Already deleted, idempotent
                Err(e) => Err(AppError::SecretStoreError(format!(
                    "Failed to delete secret: {}",
                    e
                ))),
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            let _ = target;
            Err(AppError::SecretStoreError(
                "Not supported on this platform".to_string(),
            ))
        }
    }

    /// 计划 §5.1：启动自检 = 写-读-删附录 B 的探针条目 `cc-switch/v1/probe`，
    /// 并校验回读值与写入值一致（三步共用 `write_lock`，回读失败也要删掉探针条目）。
    async fn probe(&self) -> Result<(), AppError> {
        let target = SecretTarget::probe();
        let value = Zeroizing::new("cc-switch-probe".to_string());
        let guard = self.lock_writes();

        self.write_locked(&guard, &target, value.as_str())?;

        let readback = match self.read_locked(&guard, &target) {
            Ok(read) if read.as_deref().map(String::as_str) == Some(value.as_str()) => Ok(()),
            Ok(_) => Err(SecretError::Backend(format!(
                "probe readback mismatch for target [{target}]"
            ))),
            Err(e) => Err(e),
        };

        // 无论回读结果如何都清掉探针条目，不留注册残留
        let cleanup = self.delete_locked(&guard, &target);

        readback?;
        cleanup?;
        Ok(())
    }

    async fn list_targets(&self, prefix: &str) -> Result<Vec<String>, AppError> {
        #[cfg(target_os = "windows")]
        {
            // Windows Credential Manager doesn't provide efficient prefix search
            // We'll need to enumerate and filter
            // For now, return empty - this will be implemented if orphan cleanup is needed
            let _ = prefix;
            Ok(Vec::new())
        }

        #[cfg(not(target_os = "windows"))]
        {
            let _ = prefix;
            Err(AppError::SecretStoreError(
                "Not supported on this platform".to_string(),
            ))
        }
    }
}

/// 计划 §5.1 的非 Windows 实现：所有方法返回 `SecretError::Unsupported`。
/// 启动流程 `probe()` 失败后走 §6.6 的阻断式提示（重试 / 退出）。
pub struct UnsupportedSecretStore;

impl UnsupportedSecretStore {
    pub fn new() -> Self {
        Self
    }
}

impl Default for UnsupportedSecretStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SecretStore for UnsupportedSecretStore {
    async fn set(&self, target: &SecretTarget, value: Zeroizing<String>) -> Result<(), AppError> {
        let _ = (target, value);
        Err(SecretError::Unsupported.into())
    }

    async fn get(&self, target: &SecretTarget) -> Result<Option<Zeroizing<String>>, AppError> {
        let _ = target;
        Err(SecretError::Unsupported.into())
    }

    async fn delete(&self, target: &SecretTarget) -> Result<(), AppError> {
        let _ = target;
        Err(SecretError::Unsupported.into())
    }

    async fn probe(&self) -> Result<(), AppError> {
        Err(SecretError::Unsupported.into())
    }

    async fn list_targets(&self, prefix: &str) -> Result<Vec<String>, AppError> {
        let _ = prefix;
        Err(SecretError::Unsupported.into())
    }
}

/// In-memory implementation for testing (principle 3.2-5)
///
/// 计划 §5.1 写的是“仅 `cfg(test)`”，但 `src-tauri/tests/**` 是独立 crate（`cfg(test)`
/// 不覆盖它），`tests/support.rs` 与 lib 内的单元测试都依赖此实现，故保持公开导出。
pub struct InMemorySecretStore {
    storage: Arc<Mutex<HashMap<String, String>>>,
}

impl InMemorySecretStore {
    pub fn new() -> Self {
        Self {
            storage: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get all stored secrets (test helper)
    #[cfg(test)]
    pub fn dump(&self) -> HashMap<String, String> {
        self.storage.lock().unwrap().clone()
    }
}

impl Default for InMemorySecretStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SecretStore for InMemorySecretStore {
    async fn set(&self, target: &SecretTarget, value: Zeroizing<String>) -> Result<(), AppError> {
        let key = target.to_target_string();
        self.storage.lock().unwrap().insert(key, value.to_string());
        Ok(())
    }

    async fn get(&self, target: &SecretTarget) -> Result<Option<Zeroizing<String>>, AppError> {
        let key = target.to_target_string();
        Ok(self
            .storage
            .lock()
            .unwrap()
            .get(&key)
            .map(|v| Zeroizing::new(v.clone())))
    }

    async fn delete(&self, target: &SecretTarget) -> Result<(), AppError> {
        let key = target.to_target_string();
        self.storage.lock().unwrap().remove(&key);
        Ok(())
    }

    async fn probe(&self) -> Result<(), AppError> {
        Ok(())
    }

    async fn list_targets(&self, prefix: &str) -> Result<Vec<String>, AppError> {
        Ok(self
            .storage
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_config::AppType;

    #[tokio::test]
    async fn test_in_memory_store_roundtrip() {
        let store = InMemorySecretStore::new();
        let target = SecretTarget::provider_api_key(AppType::Claude, "test-provider");
        let secret = Zeroizing::new("sk-test-key-12345".to_string());

        // Set
        store.set(&target, secret.clone()).await.unwrap();

        // Get
        let retrieved = store.get(&target).await.unwrap();
        assert_eq!(
            retrieved.as_deref().map(|s| s.as_str()),
            Some("sk-test-key-12345")
        );

        // Delete
        store.delete(&target).await.unwrap();
        let after_delete = store.get(&target).await.unwrap();
        assert!(after_delete.is_none());
    }

    #[tokio::test]
    async fn test_in_memory_store_list_targets() {
        let store = InMemorySecretStore::new();

        store
            .set(
                &SecretTarget::provider_api_key(AppType::Claude, "p1"),
                Zeroizing::new("key1".to_string()),
            )
            .await
            .unwrap();
        store
            .set(
                &SecretTarget::provider_api_key(AppType::Claude, "p2"),
                Zeroizing::new("key2".to_string()),
            )
            .await
            .unwrap();
        store
            .set(
                &SecretTarget::provider_api_key(AppType::Codex, "p3"),
                Zeroizing::new("key3".to_string()),
            )
            .await
            .unwrap();

        let claude_targets = store
            .list_targets("cc-switch/v1/provider/claude/")
            .await
            .unwrap();
        assert_eq!(claude_targets.len(), 2);

        let all_targets = store.list_targets("cc-switch/v1/").await.unwrap();
        assert_eq!(all_targets.len(), 3);
    }

    #[tokio::test]
    async fn test_in_memory_store_probe() {
        let store = InMemorySecretStore::new();
        assert!(store.probe().await.is_ok());
    }

    /// 计划 §5.1 / 附录 B：`Display` 输出即凭据管理器的 target 名。
    #[test]
    fn secret_target_display_is_target_name() {
        let target = SecretTarget::provider_api_key(AppType::Claude, "p1");
        assert_eq!(
            target.to_string(),
            "cc-switch/v1/provider/claude/p1/api_key"
        );
        assert_eq!(SecretTarget::probe().to_string(), "cc-switch/v1/probe");
        assert_eq!(target.to_string(), target.to_target_string());
    }

    /// §5.1：超限要返回可区分的错误，文案含字段名与上限、不含值。
    /// 长度校验发生在触碰后端之前，因此本测试不会写进真实凭据管理器。
    #[tokio::test]
    #[cfg(target_os = "windows")]
    async fn set_rejects_overlong_value_without_touching_backend() {
        let store = WindowsSecretStore::new().expect("windows store");
        let target = SecretTarget::provider_api_key(AppType::Claude, "cc-switch-test-toolong");
        let secret = "k".repeat(MAX_VALUE_UTF16_UNITS + 1);

        let err = store
            .set(&target, Zeroizing::new(secret.clone()))
            .await
            .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains(&target.to_string()), "文案应含字段名: {msg}");
        assert!(msg.contains("1280"), "文案应含上限: {msg}");
        assert!(!msg.contains(&secret[..32]), "文案不得含密钥值: {msg}");
        assert!(
            store.get(&target).await.unwrap().is_none(),
            "超限写入不得落到后端"
        );
    }

    /// §10：真实凭据管理器条目用 RAII 在 `Drop` 中清理，panic 也不留残留。
    #[cfg(target_os = "windows")]
    struct RoundtripGuard {
        store: WindowsSecretStore,
        target: SecretTarget,
    }

    #[cfg(target_os = "windows")]
    impl RoundtripGuard {
        fn new(target: SecretTarget) -> Self {
            Self {
                store: WindowsSecretStore::new().expect("windows store"),
                target,
            }
        }
    }

    #[cfg(target_os = "windows")]
    impl Drop for RoundtripGuard {
        fn drop(&mut self) {
            // Drop 里不能 await，用 block_on（与 tests/support.rs 同一手法）
            let _ = futures::executor::block_on(self.store.delete(&self.target));
        }
    }

    /// §10 集成测试：真实凭据管理器往返。target 名由 `SecretTarget` 生成（附录 B 的
    /// `cc-switch/v1/provider/<app>/<id>/api_key`），清理走 guard 的 `Drop`。
    /// 跑法：`cargo test --lib secrets_windows_roundtrip -- --ignored`
    #[test]
    #[ignore]
    #[cfg(target_os = "windows")]
    fn secrets_windows_roundtrip() {
        let target = SecretTarget::provider_api_key(AppType::Claude, "cc-switch-test-roundtrip");
        assert!(target.to_string().starts_with("cc-switch/v1/provider/"));

        let guard = RoundtripGuard::new(target.clone());
        let secret = Zeroizing::new("sk-fixture-roundtrip-0001".to_string());

        futures::executor::block_on(guard.store.set(&target, secret)).expect("set");

        let got = futures::executor::block_on(guard.store.get(&target))
            .expect("get")
            .expect("present");
        assert_eq!(got.as_str(), "sk-fixture-roundtrip-0001");
        drop(got);

        // §5.1 的启动自检：写-读-删探针条目，含回读校验
        futures::executor::block_on(guard.store.probe()).expect("probe");

        futures::executor::block_on(guard.store.delete(&target)).expect("delete");
        assert!(futures::executor::block_on(guard.store.get(&target))
            .expect("get after delete")
            .is_none());
        // guard 的 Drop 会再删一次：幂等，且 panic 路径也能清干净
    }
}
