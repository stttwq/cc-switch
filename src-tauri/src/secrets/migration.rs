//! Automatic credential migration from database to credential manager
//!
//! Implements Phase 6 of the secrets-credential-manager-slimdown-plan:
//! - Extract credentials from providers.settings_config
//! - Store them in Windows Credential Manager
//! - Write back stripped configs to database
//! - Clean up plaintext residue

use crate::app_config::AppType;
use crate::database::Database;
use crate::error::AppError;
use crate::provider::Provider;
use crate::secrets::extractor::SecretExtractor;
use crate::secrets::store::SecretStore;
use crate::secrets::target::SecretTarget;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str::FromStr;

/// Migration report for one-time user notification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationReport {
    pub migrated_providers: Vec<MigratedProviderInfo>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigratedProviderInfo {
    pub provider_id: String,
    pub provider_name: String,
    pub app_type: String,
    pub fields_count: usize,
}

/// Credential migration coordinator
pub struct CredentialMigrator<'a> {
    db: &'a Database,
    store: &'a dyn SecretStore,
}

impl<'a> CredentialMigrator<'a> {
    pub fn new(db: &'a Database, store: &'a dyn SecretStore) -> Self {
        Self { db, store }
    }

    /// Phase 6: Migrate credentials from database to credential manager
    ///
    /// Steps:
    /// 1. Probe credential manager (fail early if unavailable)
    /// 2. For each app_type, extract provider secrets
    /// 3. Write to credential manager
    /// 4. Update database with stripped configs
    pub async fn run_migration(&self) -> Result<MigrationReport, AppError> {
        // Step 1: Probe credential manager
        self.store.probe().await?;

        log::info!("开始凭据迁移：先全部提取，再写凭据，最后一笔事务改 DB");

        struct PendingRow {
            app_type: AppType,
            app_type_str: &'static str,
            provider: Provider,
            stripped: serde_json::Value,
            secrets: crate::secrets::ProviderSecrets,
        }

        let mut pending: Vec<PendingRow> = Vec::new();
        for app_type_str in ["claude", "codex", "pi"] {
            let app_type = AppType::from_str(app_type_str)
                .map_err(|e| AppError::Config(format!("Invalid app_type {app_type_str}: {e}")))?;
            let providers = self.db.get_all_providers(app_type_str)?;
            for (_id, provider) in providers {
                let extracted = SecretExtractor::extract_with_meta(
                    &provider.id,
                    &app_type,
                    &provider.settings_config,
                    provider
                        .meta
                        .as_ref()
                        .and_then(|m| m.api_key_field.as_deref()),
                )?;
                pending.push(PendingRow {
                    app_type: app_type.clone(),
                    app_type_str,
                    provider,
                    stripped: extracted.stripped,
                    secrets: extracted.secrets,
                });
            }
        }

        let mut report = MigrationReport {
            migrated_providers: Vec::new(),
            errors: Vec::new(),
        };

        for row in &pending {
            if row.secrets.is_empty() {
                continue;
            }
            let extractor = SecretExtractor::new(self.store, row.app_type.clone()).with_db(self.db);
            super::extractor::persist_extracted_secrets(&extractor, &row.provider.id, &row.secrets)
                .await?;
        }

        {
            let conn = crate::database::lock_conn!(self.db.conn);
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| AppError::Database(format!("开启凭据迁移事务失败: {e}")))?;
            for row in &pending {
                if row.secrets.is_empty() {
                    continue;
                }
                let stripped_json = serde_json::to_string(&row.stripped)
                    .map_err(|e| AppError::Database(format!("Failed to serialize config: {e}")))?;
                tx.execute(
                    "UPDATE providers SET settings_config = ?1 WHERE app_type = ?2 AND id = ?3",
                    rusqlite::params![stripped_json, row.app_type_str, row.provider.id],
                )
                .map_err(|e| {
                    AppError::Database(format!(
                        "Failed to update provider {}: {e}",
                        row.provider.id
                    ))
                })?;
                let fields_count = usize::from(row.secrets.api_key.is_some())
                    + usize::from(row.secrets.base_url.is_some())
                    + row.secrets.extra_env.len();
                report.migrated_providers.push(MigratedProviderInfo {
                    provider_id: row.provider.id.clone(),
                    provider_name: row.provider.name.clone(),
                    app_type: row.app_type_str.to_string(),
                    fields_count,
                });
            }
            tx.commit()
                .map_err(|e| AppError::Database(format!("提交凭据迁移事务失败: {e}")))?;
        }

        log::info!(
            "凭据迁移完成: 迁移 {} 个 provider",
            report.migrated_providers.len()
        );

        // §6.2 第 5 步：迁移 settings.json 里的 WebDAV 密码 / S3 双密钥，
        // 否则老用户的同步凭据会被 typed 结构（字段已删）静默丢弃。
        self.migrate_app_settings().await?;

        Ok(report)
    }

    /// 把 settings.json 遗留的明文凭据迁入凭据管理器，并从文件剥离后原子重写。
    /// 幂等：字段缺失或已为 `literal:` 前缀时跳过。
    async fn migrate_app_settings(&self) -> Result<(), AppError> {
        // 原则 3.2-5：单元测试不得触碰真实 home。未显式指向测试 home 时跳过，
        // 生产构建（非 test）与设置 CC_SWITCH_TEST_HOME 的集成测试仍照常迁移。
        if cfg!(test) && std::env::var("CC_SWITCH_TEST_HOME").is_err() {
            return Ok(());
        }
        let path = crate::config::get_home_dir()
            .join(".cc-switch")
            .join("settings.json");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Ok(()); // 文件不存在，无需迁移
        };
        let Ok(mut root) = serde_json::from_str::<serde_json::Value>(&content) else {
            return Ok(()); // 解析失败交给 typed loader 处理，这里不动
        };

        // (JSON 父键, 字段名, 凭据 target)
        let fields = [
            (
                "webdav_sync",
                "password",
                SecretTarget::app("webdav", "password"),
            ),
            (
                "s3_sync",
                "access_key_id",
                SecretTarget::app("s3", "access_key_id"),
            ),
            (
                "s3_sync",
                "secret_access_key",
                SecretTarget::app("s3", "secret_access_key"),
            ),
        ];
        let mut stripped_any = false;
        for (parent_key, field, target) in fields {
            let value = root
                .get(parent_key)
                .and_then(|o| o.get(field))
                .and_then(Value::as_str)
                .map(str::to_string);
            let Some(value) = value else { continue };
            if value.is_empty() || value.starts_with("literal:") {
                continue;
            }
            self.store.store(&target, &value).await?;
            if let Some(map) = root.get_mut(parent_key).and_then(Value::as_object_mut) {
                map.remove(field);
                stripped_any = true;
            }
        }

        // 仅在确实剥离了字段时重写文件；serde 对已删除字段本就忽略，
        // 重写让磁盘上不再残留明文。
        if stripped_any {
            let rewritten = serde_json::to_string_pretty(&root)
                .map_err(|e| AppError::Config(format!("settings.json 序列化失败: {e}")))?;
            crate::config::atomic_write_private(&path, rewritten.as_bytes())
                .map_err(|e| AppError::Config(format!("settings.json 重写失败: {e}")))?;
            log::info!("已将 settings.json 中的 WebDAV/S3 凭据迁入凭据管理器并剥离明文");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_config::AppType;
    use crate::database::Database;
    use crate::secrets::store::SecretStore;
    use crate::secrets::target::SecretTarget;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use zeroize::Zeroizing;

    struct MockCredentialStore {
        storage: Arc<Mutex<HashMap<String, String>>>,
        fail_set: bool,
    }

    impl MockCredentialStore {
        fn new() -> Self {
            Self {
                storage: Arc::new(Mutex::new(HashMap::new())),
                fail_set: false,
            }
        }
    }

    #[async_trait]
    impl SecretStore for MockCredentialStore {
        async fn set(
            &self,
            target: &SecretTarget,
            value: Zeroizing<String>,
        ) -> Result<(), AppError> {
            if self.fail_set {
                return Err(AppError::SecretStoreError("mock set failed".to_string()));
            }
            self.storage
                .lock()
                .unwrap()
                .insert(target.to_target_string(), value.to_string());
            Ok(())
        }

        async fn get(&self, target: &SecretTarget) -> Result<Option<Zeroizing<String>>, AppError> {
            Ok(self
                .storage
                .lock()
                .unwrap()
                .get(&target.to_target_string())
                .map(|v| Zeroizing::new(v.clone())))
        }

        async fn delete(&self, target: &SecretTarget) -> Result<(), AppError> {
            self.storage
                .lock()
                .unwrap()
                .remove(&target.to_target_string());
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

    #[tokio::test]
    async fn test_migrate_provider_with_secrets() -> Result<(), AppError> {
        let db = Database::memory()?;

        // 插入一个包含敏感信息的 provider
        {
            let conn = crate::database::lock_conn!(db.conn);
            conn.execute(
                r#"INSERT INTO providers (id, name, app_type, settings_config, created_at, meta)
                   VALUES ('test-claude-1', 'Test Claude', 'claude',
                           '{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-ant-test123"}}',
                           1234567890, '{}')"#,
                [],
            )?;
        }

        let store = Arc::new(MockCredentialStore::new());
        let migrator = CredentialMigrator::new(&db, store.as_ref());

        // 验证 provider 已插入
        let providers = db.get_all_providers("claude")?;
        eprintln!("Providers in DB: {}", providers.len());
        assert_eq!(providers.len(), 1, "Provider should be in database");

        // 执行迁移
        let result = migrator.run_migration().await?;

        // 打印调试信息
        eprintln!(
            "Migration result: {} migrated, {} errors",
            result.migrated_providers.len(),
            result.errors.len()
        );
        for error in &result.errors {
            eprintln!("Error: {}", error);
        }

        // 验证迁移报告
        assert_eq!(
            result.migrated_providers.len(),
            1,
            "Expected 1 migrated provider, got {}. Errors: {:?}",
            result.migrated_providers.len(),
            result.errors
        );
        assert_eq!(result.errors.len(), 0);

        let migrated = &result.migrated_providers[0];
        assert_eq!(migrated.provider_id, "test-claude-1");
        assert_eq!(migrated.app_type, "claude");
        assert_eq!(
            migrated.fields_count, 1,
            "Expected 1 field (ANTHROPIC_AUTH_TOKEN)"
        );

        // 验证凭据已存储
        let api_key_target = SecretTarget::provider_api_key(AppType::Claude, "test-claude-1");
        let api_key = store.get(&api_key_target).await?;

        assert_eq!(
            api_key.as_deref().map(|s| s.as_str()),
            Some("sk-ant-test123")
        );

        // 验证数据库中的配置已被清理
        let config: String = {
            let conn = crate::database::lock_conn!(db.conn);
            conn.query_row(
                "SELECT settings_config FROM providers WHERE id = 'test-claude-1'",
                [],
                |row| row.get(0),
            )?
        };

        eprintln!("Cleaned config: {}", config);
        assert!(!config.contains("sk-ant-test123"));
        assert!(!config.contains("ANTHROPIC_AUTH_TOKEN"));

        Ok(())
    }

    #[tokio::test]
    async fn test_migrate_idempotent() -> Result<(), AppError> {
        let db = Database::memory()?;

        {
            let conn = crate::database::lock_conn!(db.conn);
            conn.execute(
                r#"INSERT INTO providers (id, name, app_type, settings_config, created_at)
                   VALUES ('test-claude-2', 'Test Claude 2', 'claude',
                           '{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-ant-test456"}}',
                           1234567890)"#,
                [],
            )?;
        }

        let store = Arc::new(MockCredentialStore::new());
        let migrator = CredentialMigrator::new(&db, store.as_ref());

        // 第一次迁移
        let result1 = migrator.run_migration().await?;
        assert_eq!(result1.migrated_providers.len(), 1);

        // 第二次迁移（应该跳过已迁移的）
        let result2 = migrator.run_migration().await?;
        assert_eq!(result2.migrated_providers.len(), 0);

        // 验证凭据仍然存在
        let api_key_target = SecretTarget::provider_api_key(AppType::Claude, "test-claude-2");
        assert_eq!(
            store
                .get(&api_key_target)
                .await?
                .as_deref()
                .map(|s| s.as_str()),
            Some("sk-ant-test456")
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_migrate_multiple_providers() -> Result<(), AppError> {
        let db = Database::memory()?;

        // 插入多个 providers
        {
            let conn = crate::database::lock_conn!(db.conn);
            conn.execute(
                r#"INSERT INTO providers (id, name, app_type, settings_config, created_at)
                   VALUES
                   ('claude-1', 'Claude 1', 'claude', '{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-ant-key1"}}', 1234567890),
                   ('claude-2', 'Claude 2', 'claude', '{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-ant-key2"}}', 1234567890),
                   ('codex-1', 'Codex 1', 'codex', '{"auth":{"OPENAI_API_KEY":"sk-ant-key3"}}', 1234567890)"#,
                [],
            )?;
        }

        let store = Arc::new(MockCredentialStore::new());
        let migrator = CredentialMigrator::new(&db, store.as_ref());

        let result = migrator.run_migration().await?;

        // 验证所有 providers 都被迁移
        assert_eq!(result.migrated_providers.len(), 3);
        assert_eq!(result.errors.len(), 0);

        // 验证所有凭据都被存储
        let claude1_target = SecretTarget::provider_api_key(AppType::Claude, "claude-1");
        let claude2_target = SecretTarget::provider_api_key(AppType::Claude, "claude-2");
        let codex1_target = SecretTarget::provider_api_key(AppType::Codex, "codex-1");

        assert!(store.get(&claude1_target).await?.is_some());
        assert!(store.get(&claude2_target).await?.is_some());
        assert!(store.get(&codex1_target).await?.is_some());

        Ok(())
    }

    #[tokio::test]
    async fn test_migrate_no_secrets() -> Result<(), AppError> {
        let db = Database::memory()?;

        // 插入一个没有敏感信息的 provider
        {
            let conn = crate::database::lock_conn!(db.conn);
            conn.execute(
                r#"INSERT INTO providers (id, name, app_type, settings_config, created_at)
                   VALUES ('test-1', 'Test', 'claude', '{}', 1234567890)"#,
                [],
            )?;
        }

        let store = Arc::new(MockCredentialStore::new());
        let migrator = CredentialMigrator::new(&db, store.as_ref());

        let result = migrator.run_migration().await?;

        // 应该没有任何 provider 被迁移
        assert_eq!(result.migrated_providers.len(), 0);
        assert_eq!(result.errors.len(), 0);

        Ok(())
    }

    #[tokio::test]
    async fn migrate_aborts_before_writing_db_when_persist_fails() -> Result<(), AppError> {
        let db = Database::memory()?;
        {
            let conn = crate::database::lock_conn!(db.conn);
            conn.execute(
                r#"INSERT INTO providers (id, name, app_type, settings_config, created_at)
                   VALUES ('ok-1', 'Ok', 'claude', '{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-ant-ok"}}', 1)"#,
                [],
            )?;
        }

        let store = Arc::new(MockCredentialStore {
            storage: Arc::new(Mutex::new(HashMap::new())),
            fail_set: true,
        });
        let migrator = CredentialMigrator::new(&db, store.as_ref());
        assert!(migrator.run_migration().await.is_err());

        let config: String = {
            let conn = crate::database::lock_conn!(db.conn);
            conn.query_row(
                "SELECT settings_config FROM providers WHERE id = 'ok-1'",
                [],
                |row| row.get(0),
            )?
        };
        assert!(
            config.contains("sk-ant-ok"),
            "persist 失败必须整批中止，不得先写 DB"
        );
        Ok(())
    }
}
