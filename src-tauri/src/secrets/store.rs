use crate::error::AppError;
use crate::secrets::target::SecretTarget;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

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

    /// Convenience method: retrieve as plain String
    async fn retrieve(&self, target: &SecretTarget) -> Result<Option<String>, AppError> {
        Ok(self.get(target).await?.map(|z| z.to_string()))
    }
}

/// Windows Credential Manager implementation
pub struct WindowsSecretStore;

impl WindowsSecretStore {
    pub fn new() -> Result<Self, AppError> {
        // Verify we're on Windows
        #[cfg(not(target_os = "windows"))]
        {
            return Err(AppError::SecretStoreError(
                "Windows Credential Manager is only supported on Windows".to_string(),
            ));
        }

        Ok(Self)
    }
}

#[async_trait]
impl SecretStore for WindowsSecretStore {
    async fn set(&self, target: &SecretTarget, value: Zeroizing<String>) -> Result<(), AppError> {
        #[cfg(target_os = "windows")]
        {
            use keyring::Entry;

            // Check value length limit (1280 UTF-16 code units, ~2560 bytes for ASCII)
            if value.len() > 2560 {
                return Err(AppError::SecretStoreError(format!(
                    "Secret value too long ({} bytes, max 2560)",
                    value.len()
                )));
            }

            let target_str = target.to_target_string();
            // Check target length limit (32767 characters)
            if target_str.len() > 32767 {
                return Err(AppError::SecretStoreError(format!(
                    "Target string too long ({} chars, max 32767)",
                    target_str.len()
                )));
            }

            let entry = Entry::new(&target_str, &target.to_user_metadata())
                .map_err(|e| AppError::SecretStoreError(format!("Failed to create entry: {}", e)))?;

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
            use keyring::Entry;

            let entry = Entry::new(&target.to_target_string(), &target.to_user_metadata())
                .map_err(|e| AppError::SecretStoreError(format!("Failed to create entry: {}", e)))?;

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
            use keyring::Entry;

            let entry = Entry::new(&target.to_target_string(), &target.to_user_metadata())
                .map_err(|e| AppError::SecretStoreError(format!("Failed to create entry: {}", e)))?;

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

    async fn probe(&self) -> Result<(), AppError> {
        // Write and immediately delete a probe credential
        let probe_target = SecretTarget::probe();
        let test_value = Zeroizing::new("probe".to_string());

        self.set(&probe_target, test_value).await?;
        self.delete(&probe_target).await?;

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

/// In-memory implementation for testing (principle 3.2-5)
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
        self.storage
            .lock()
            .unwrap()
            .insert(key, value.to_string());
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
        assert_eq!(retrieved.as_deref().map(|s| s.as_str()), Some("sk-test-key-12345"));

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
}
