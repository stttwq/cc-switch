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
    /// §5.2.3：不阻断迁移的告警（Pi 模型级 baseUrl 等），只含字段名不含值。
    #[serde(default)]
    pub warnings: Vec<String>,
    /// §5.2.3 / D3：丢弃过 Codex OAuth 登录态的供应商 id，供 §6.5 提示改用 `codex login`。
    #[serde(default)]
    pub dropped_codex_oauth: Vec<String>,
    /// §6.4：live 重写失败的条目，供 §6.5 列出并提供重试。
    #[serde(default)]
    pub live_reapply_failures: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigratedProviderInfo {
    pub provider_id: String,
    pub provider_name: String,
    pub app_type: String,
    pub fields_count: usize,
    /// §6.2 第 6 步：迁移了哪些字段（字段名，不含值）。
    #[serde(default)]
    pub fields: Vec<String>,
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
            dropped_oauth_tokens: bool,
            warnings: Vec<String>,
        }

        let mut pending: Vec<PendingRow> = Vec::new();
        for app_type_str in ["claude", "codex", "pi"] {
            let app_type = AppType::from_str(app_type_str)
                .map_err(|e| AppError::Config(format!("Invalid app_type {app_type_str}: {e}")))?;
            let providers = self.db.get_all_providers(app_type_str)?;
            for (_id, provider) in providers {
                // §6.2 第 2 步：此阶段不写任何东西，任一行解析失败整批中止；
                // 错误只带 provider id 与字段名，绝不带值（原则 3.1-8）。
                let extracted = SecretExtractor::extract_with_meta(
                    &provider.id,
                    &app_type,
                    &provider.settings_config,
                    provider
                        .meta
                        .as_ref()
                        .and_then(|m| m.api_key_field.as_deref()),
                )
                .map_err(|e| {
                    AppError::Config(format!(
                        "供应商 {} ({app_type_str}) 凭据提取失败: {e}",
                        provider.id
                    ))
                })?;
                pending.push(PendingRow {
                    app_type: app_type.clone(),
                    app_type_str,
                    provider,
                    stripped: extracted.stripped,
                    secrets: extracted.secrets,
                    dropped_oauth_tokens: extracted.dropped_codex_oauth_tokens,
                    warnings: extracted.warnings,
                });
            }
        }

        let mut report = MigrationReport {
            migrated_providers: Vec::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            dropped_codex_oauth: Vec::new(),
            live_reapply_failures: Vec::new(),
        };

        for row in &pending {
            for warning in &row.warnings {
                report.warnings.push(warning.clone());
            }
            if row.dropped_oauth_tokens {
                report.dropped_codex_oauth.push(row.provider.id.clone());
            }
            if row.secrets.is_empty() {
                continue;
            }
            // §6.2 第 3 步：只写凭据；known_secret_targets 与 DB 剥离同笔事务落库。
            super::extractor::persist_secrets_only(
                self.store,
                &row.app_type,
                &row.provider.id,
                &row.secrets,
            )
            .await?;
        }

        {
            let mut targets = crate::secrets::load_known_targets(self.db).unwrap_or_else(|e| {
                log::warn!("读取 known_secret_targets 失败，按空集合重建: {e}");
                Vec::new()
            });
            for row in &pending {
                for target in super::extractor::provider_secret_targets(
                    &row.app_type,
                    &row.provider.id,
                    &row.secrets,
                ) {
                    let s = target.to_target_string();
                    if !targets.iter().any(|x| x == &s) {
                        targets.push(s);
                    }
                }
            }
            let targets_json = serde_json::to_string(&targets)
                .map_err(|e| AppError::Config(format!("known_secret_targets 序列化失败: {e}")))?;

            let conn = crate::database::lock_conn!(self.db.conn);
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| AppError::Database(format!("开启凭据迁移事务失败: {e}")))?;
            tx.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES ('known_secret_targets', ?1)",
                rusqlite::params![targets_json],
            )
            .map_err(|e| AppError::Database(format!("写入 known_secret_targets 失败: {e}")))?;
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
                let mut fields: Vec<String> = Vec::new();
                if row.secrets.api_key.is_some() {
                    fields.push("api_key".to_string());
                }
                if row.secrets.base_url.is_some() {
                    fields.push("base_url".to_string());
                }
                for name in row.secrets.extra_env.keys() {
                    fields.push(format!("env:{name}"));
                }
                let fields_count = fields.len();
                report.migrated_providers.push(MigratedProviderInfo {
                    provider_id: row.provider.id.clone(),
                    provider_name: row.provider.name.clone(),
                    app_type: row.app_type_str.to_string(),
                    fields_count,
                    fields,
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

    /// 把 settings.json 遗留的明文凭据迁入凭据管理器（见同名自由函数）。
    async fn migrate_app_settings(&self) -> Result<(), AppError> {
        sweep_plaintext_app_settings(self.db, self.store).await
    }
}

/// 把 settings.json 遗留的明文凭据迁入凭据管理器，并从文件剥离后原子重写。
/// 幂等：无明文字段时是 no-op。既作为 §6.2 第 5 步在迁移批次里跑，也由启动流程
/// 无条件跑一次——已经清过 `secrets_migration_pending` 的旧版本用户（当时的实现
/// 只认 snake_case 键、漏迁 `webdavSync`）靠这条把明文收掉。
pub async fn sweep_plaintext_app_settings(
    db: &Database,
    store: &dyn SecretStore,
) -> Result<(), AppError> {
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

    // §6.2 第 5 步：从 settings.json 剥出明文的 WebDAV / S3 凭据。
    let found = plaintext_sync_fields(&root);
    let mut stripped_any = false;
    for (parent_key, field, target, value) in found {
        store.store(&target, &value).await?;
        crate::secrets::scan::note_session_secret(&value);
        // §5.4：keyring 无法枚举，应用级条目同样要登记进 known_secret_targets。
        let mut targets = crate::secrets::load_known_targets(db)?;
        let name = target.to_target_string();
        if !targets.iter().any(|existing| existing == &name) {
            targets.push(name);
            crate::secrets::save_known_targets(db, &targets)?;
        }
        if let Some(map) = root.get_mut(&parent_key).and_then(Value::as_object_mut) {
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

/// §6.2 第 5 步：找出 settings.json 里残留明文的同步凭据字段。
///
/// `AppSettings` 带 `#[serde(rename_all = "camelCase")]`，磁盘上的父键实际是
/// `webdavSync` / `s3Sync`、字段是 `accessKeyId` / `secretAccessKey`；旧的
/// snake_case 写法一并兼容，避免手改过的文件漏迁。返回
/// `(父键, 字段名, 凭据 target, 明文值)`，空值与 `literal:` 前缀跳过。
fn plaintext_sync_fields(root: &Value) -> Vec<(String, &'static str, SecretTarget, String)> {
    const SPEC: &[(&[&str], &[&str], &str, &str)] = &[
        (
            &["webdavSync", "webdav_sync"],
            &["password"],
            "webdav",
            "password",
        ),
        (
            &["s3Sync", "s3_sync"],
            &["accessKeyId", "access_key_id"],
            "s3",
            "access_key_id",
        ),
        (
            &["s3Sync", "s3_sync"],
            &["secretAccessKey", "secret_access_key"],
            "s3",
            "secret_access_key",
        ),
    ];
    let mut found = Vec::new();
    for (parents, fields, scope, target_field) in SPEC {
        for parent_key in parents.iter().copied() {
            let Some(parent) = root.get(parent_key) else {
                continue;
            };
            for field_name in fields.iter().copied() {
                let Some(value) = parent.get(field_name).and_then(Value::as_str) else {
                    continue;
                };
                if value.is_empty() || value.starts_with("literal:") {
                    continue;
                }
                found.push((
                    parent_key.to_string(),
                    field_name,
                    SecretTarget::app((*scope).to_string(), (*target_field).to_string()),
                    value.to_string(),
                ));
            }
        }
    }
    found
}

/// §6.4：把 live 重写失败项写入已落库的迁移报告，供 §6.5 列出并提供重试。
/// 空集合表示本轮全部成功，此时清掉历史失败记录。
pub fn record_live_reapply_failures(db: &Database, failures: &[String]) {
    let mut report = db
        .get_setting("secrets_migration_report")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<MigrationReport>(&raw).ok())
        .unwrap_or_else(|| MigrationReport {
            migrated_providers: Vec::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            dropped_codex_oauth: Vec::new(),
            live_reapply_failures: Vec::new(),
        });
    report.live_reapply_failures = failures.to_vec();
    match serde_json::to_string(&report) {
        Ok(json) => {
            if let Err(e) = db.set_setting("secrets_migration_report", &json) {
                log::warn!("写入 live 重写失败报告失败: {e}");
            }
        }
        Err(e) => log::warn!("序列化 live 重写失败报告失败: {e}"),
    }
}

/// §6.4：`~/.codex/auth.json` 若**只**含 `OPENAI_API_KEY`（无 ChatGPT `tokens`）
/// 且该值与已迁入凭据管理器的某个 key 相同 → 删除该文件；否则不动并写入报告。
/// 含登录态的文件一律不动（D3 后登录态完全交给 Codex 自己）。
pub async fn prune_migrated_codex_auth_file(
    db: &Database,
    store: &dyn SecretStore,
) -> Result<(), AppError> {
    let path = crate::codex_config::get_codex_auth_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(()); // 文件不存在
    };
    let Ok(root) = serde_json::from_str::<Value>(&text) else {
        note_codex_auth_kept(db, "无法解析，已保留");
        return Ok(());
    };
    let Some(obj) = root.as_object() else {
        note_codex_auth_kept(db, "结构异常，已保留");
        return Ok(());
    };
    if obj.contains_key("tokens") || obj.contains_key("auth_mode") {
        return Ok(()); // 用户自己的 ChatGPT 登录态，一律不动
    }
    let Some(key) = obj.get("OPENAI_API_KEY").and_then(Value::as_str) else {
        return Ok(()); // 没有明文 key，无需处理
    };

    let mut matches_migrated = false;
    for app in [AppType::Claude, AppType::Codex, AppType::Pi] {
        let providers = db.get_all_providers(app.as_str())?;
        for id in providers.keys() {
            let target = SecretTarget::provider_api_key(app.clone(), id);
            if let Some(stored) = store.get(&target).await? {
                if stored.as_str() == key {
                    matches_migrated = true;
                }
            }
        }
    }

    if matches_migrated {
        match std::fs::remove_file(&path) {
            Ok(()) => log::info!("已删除只含已迁移 API key 的 {}", path.display()),
            Err(e) => note_codex_auth_kept(db, &format!("删除失败: {e}")),
        }
    } else {
        note_codex_auth_kept(db, "与已迁移密钥不匹配，已保留");
    }
    Ok(())
}

fn note_codex_auth_kept(db: &Database, reason: &str) {
    log::warn!("~/.codex/auth.json 未清理: {reason}");
    let Ok(mut report_json) = db.get_setting("secrets_migration_report") else {
        return;
    };
    let Some(raw) = report_json.as_mut() else {
        return;
    };
    let Ok(mut report) = serde_json::from_str::<Value>(raw) else {
        return;
    };
    if let Some(errors) = report.get_mut("errors").and_then(Value::as_array_mut) {
        errors.push(Value::String(format!("codex_auth_json_kept: {reason}")));
        if let Ok(rewritten) = serde_json::to_string(&report) {
            *raw = rewritten;
        }
    }
    let snapshot = raw.clone();
    if let Err(e) = db.set_setting("secrets_migration_report", &snapshot) {
        log::warn!("写入 codex_auth 保留原因失败: {e}");
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

    #[test]
    fn plaintext_sync_fields_reads_camel_case_keys() {
        // AppSettings 是 camelCase：只认 snake_case 会让 WebDAV/S3 明文永不迁移。
        let root = serde_json::json!({
            "webdavSync": { "password": "plain-webdav-pass", "enabled": false },
            "s3Sync": { "accessKeyId": "AKIA-plain-id", "secretAccessKey": "plain-secret" },
            "theme": "dark"
        });
        let found = plaintext_sync_fields(&root);
        assert_eq!(found.len(), 3);
        assert!(found
            .iter()
            .all(|(parent, _, _, _)| parent == "webdavSync" || parent == "s3Sync"));
        assert!(found
            .iter()
            .any(|(_, field, _, value)| *field == "password"
                && value.as_str() == "plain-webdav-pass"));

        // 旧 snake_case 与 literal: 占位（已迁移标记）都要正确处理
        let legacy = serde_json::json!({
            "webdav_sync": { "password": "legacy-pass" },
            "s3Sync": { "secretAccessKey": "literal:***" }
        });
        let found = plaintext_sync_fields(&legacy);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1, "password");
        assert!(plaintext_sync_fields(&serde_json::json!({})).is_empty());
    }

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
