use crate::database::{lock_conn, Database};
use crate::error::AppError;
use crate::provider::{Provider, ProviderMeta};
use indexmap::IndexMap;
use rusqlite::{params, OptionalExtension};
use std::collections::HashSet;

impl Database {
    pub fn get_all_providers(
        &self,
        app_type: &str,
    ) -> Result<IndexMap<String, Provider>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn.prepare(
            "SELECT id, name, settings_config, website_url, category, created_at, sort_index, notes, icon, icon_color, meta
             FROM providers WHERE app_type = ?1
             ORDER BY COALESCE(sort_index, 999999), created_at ASC, id ASC"
        ).map_err(|e| AppError::Database(e.to_string()))?;

        let provider_iter = stmt
            .query_map(params![app_type], |row| {
                let id: String = row.get(0)?;
                let name: String = row.get(1)?;
                let settings_config_str: String = row.get(2)?;
                let website_url: Option<String> = row.get(3)?;
                let category: Option<String> = row.get(4)?;
                let created_at: Option<i64> = row.get(5)?;
                let sort_index: Option<usize> = row.get(6)?;
                let notes: Option<String> = row.get(7)?;
                let icon: Option<String> = row.get(8)?;
                let icon_color: Option<String> = row.get(9)?;
                let meta_str: String = row.get(10)?;

                let settings_config =
                    serde_json::from_str(&settings_config_str).unwrap_or(serde_json::Value::Null);
                let meta: ProviderMeta = serde_json::from_str(&meta_str).unwrap_or_default();

                Ok((
                    id,
                    Provider {
                        id: "".to_string(), // Placeholder, set below
                        name,
                        settings_config,
                        website_url,
                        category,
                        created_at,
                        sort_index,
                        notes,
                        meta: Some(meta),
                        icon,
                        icon_color,
                    },
                ))
            })
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut providers = IndexMap::new();
        for provider_res in provider_iter {
            let (id, mut provider) = provider_res.map_err(|e| AppError::Database(e.to_string()))?;
            provider.id = id.clone();
            providers.insert(id, provider);
        }

        Ok(providers)
    }

    pub fn get_current_provider(&self, app_type: &str) -> Result<Option<String>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare("SELECT id FROM providers WHERE app_type = ?1 AND is_current = 1 LIMIT 1")
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut rows = stmt
            .query(params![app_type])
            .map_err(|e| AppError::Database(e.to_string()))?;

        if let Some(row) = rows.next().map_err(|e| AppError::Database(e.to_string()))? {
            Ok(Some(
                row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
            ))
        } else {
            Ok(None)
        }
    }

    pub fn get_provider_by_id(
        &self,
        id: &str,
        app_type: &str,
    ) -> Result<Option<Provider>, AppError> {
        let conn = lock_conn!(self.conn);
        let result = conn.query_row(
            "SELECT name, settings_config, website_url, category, created_at, sort_index, notes, icon, icon_color, meta
             FROM providers WHERE id = ?1 AND app_type = ?2",
            params![id, app_type],
            |row| {
                let name: String = row.get(0)?;
                let settings_config_str: String = row.get(1)?;
                let website_url: Option<String> = row.get(2)?;
                let category: Option<String> = row.get(3)?;
                let created_at: Option<i64> = row.get(4)?;
                let sort_index: Option<usize> = row.get(5)?;
                let notes: Option<String> = row.get(6)?;
                let icon: Option<String> = row.get(7)?;
                let icon_color: Option<String> = row.get(8)?;
                let meta_str: String = row.get(9)?;

                let settings_config = serde_json::from_str(&settings_config_str).unwrap_or(serde_json::Value::Null);
                let meta: ProviderMeta = serde_json::from_str(&meta_str).unwrap_or_default();

                Ok(Provider {
                    id: id.to_string(),
                    name,
                    settings_config,
                    website_url,
                    category,
                    created_at,
                    sort_index,
                    notes,
                    meta: Some(meta),
                    icon,
                    icon_color,
                })
            },
        );

        match result {
            Ok(provider) => Ok(Some(provider)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    pub fn save_provider(&self, app_type: &str, provider: &Provider) -> Result<(), AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;

        let meta_clone = provider.meta.clone().unwrap_or_default();

        let existing: Option<bool> = tx
            .query_row(
                "SELECT is_current FROM providers WHERE id = ?1 AND app_type = ?2",
                params![provider.id, app_type],
                |row| row.get(0),
            )
            .ok();

        let is_update = existing.is_some();
        let is_current = existing.unwrap_or(false);

        if is_update {
            tx.execute(
                "UPDATE providers SET
                    name = ?1,
                    settings_config = ?2,
                    website_url = ?3,
                    category = ?4,
                    created_at = ?5,
                    sort_index = ?6,
                    notes = ?7,
                    icon = ?8,
                    icon_color = ?9,
                    meta = ?10,
                    is_current = ?11
                WHERE id = ?12 AND app_type = ?13",
                params![
                    provider.name,
                    serde_json::to_string(&provider.settings_config).map_err(|e| {
                        AppError::Database(format!("Failed to serialize settings_config: {e}"))
                    })?,
                    provider.website_url,
                    provider.category,
                    provider.created_at,
                    provider.sort_index,
                    provider.notes,
                    provider.icon,
                    provider.icon_color,
                    serde_json::to_string(&meta_clone).map_err(|e| AppError::Database(format!(
                        "Failed to serialize meta: {e}"
                    )))?,
                    is_current,
                    provider.id,
                    app_type,
                ],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        } else {
            tx.execute(
                "INSERT INTO providers (
                    id, app_type, name, settings_config, website_url, category,
                    created_at, sort_index, notes, icon, icon_color, meta, is_current
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    provider.id,
                    app_type,
                    provider.name,
                    serde_json::to_string(&provider.settings_config).map_err(|e| {
                        AppError::Database(format!("Failed to serialize settings_config: {e}"))
                    })?,
                    provider.website_url,
                    provider.category,
                    provider.created_at,
                    provider.sort_index,
                    provider.notes,
                    provider.icon,
                    provider.icon_color,
                    serde_json::to_string(&meta_clone).map_err(|e| AppError::Database(format!(
                        "Failed to serialize meta: {e}"
                    )))?,
                    is_current,
                ],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        }

        tx.commit().map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    /// Replace a provider row under a new ID without exposing an intermediate
    /// duplicate or missing row. Existing endpoint and health references move
    /// with the provider, while its current-state bit is preserved.
    pub fn replace_provider_id(
        &self,
        app_type: &str,
        original_id: &str,
        provider: &Provider,
    ) -> Result<(), AppError> {
        if original_id == provider.id {
            return self.save_provider(app_type, provider);
        }

        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;

        let is_current = tx
            .query_row(
                "SELECT is_current FROM providers WHERE id = ?1 AND app_type = ?2",
                params![original_id, app_type],
                |row| row.get::<_, bool>(0),
            )
            .optional()
            .map_err(|e| AppError::Database(e.to_string()))?
            .ok_or_else(|| {
                AppError::Database(format!(
                    "Provider '{original_id}' does not exist in app '{app_type}'"
                ))
            })?;

        let target_exists = tx
            .query_row(
                "SELECT 1 FROM providers WHERE id = ?1 AND app_type = ?2",
                params![provider.id, app_type],
                |_| Ok(()),
            )
            .optional()
            .map_err(|e| AppError::Database(e.to_string()))?
            .is_some();
        if target_exists {
            return Err(AppError::Database(format!(
                "Provider '{}' already exists in app '{app_type}'",
                provider.id
            )));
        }

        let meta = provider.meta.clone().unwrap_or_default();
        tx.execute(
            "INSERT INTO providers (
                id, app_type, name, settings_config, website_url, category,
                created_at, sort_index, notes, icon, icon_color, meta,
                is_current
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                provider.id,
                app_type,
                provider.name,
                serde_json::to_string(&provider.settings_config).map_err(|e| {
                    AppError::Database(format!("Failed to serialize settings_config: {e}"))
                })?,
                provider.website_url,
                provider.category,
                provider.created_at,
                provider.sort_index,
                provider.notes,
                provider.icon,
                provider.icon_color,
                serde_json::to_string(&meta).map_err(|e| {
                    AppError::Database(format!("Failed to serialize meta: {e}"))
                })?,
                is_current,
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        tx.execute(
            "UPDATE provider_health SET provider_id = ?1 WHERE provider_id = ?2 AND app_type = ?3",
            params![provider.id, original_id, app_type],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        tx.execute(
            "DELETE FROM providers WHERE id = ?1 AND app_type = ?2",
            params![original_id, app_type],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        tx.commit().map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn delete_provider(&self, app_type: &str, id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "DELETE FROM providers WHERE id = ?1 AND app_type = ?2",
            params![id, app_type],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn set_current_provider(&self, app_type: &str, id: &str) -> Result<(), AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;

        tx.execute(
            "UPDATE providers SET is_current = 0 WHERE app_type = ?1",
            params![app_type],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        tx.execute(
            "UPDATE providers SET is_current = 1 WHERE id = ?1 AND app_type = ?2",
            params![id, app_type],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        tx.commit().map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_provider_settings_config(
        &self,
        app_type: &str,
        provider_id: &str,
        settings_config: &serde_json::Value,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE providers SET settings_config = ?1 WHERE id = ?2 AND app_type = ?3",
            params![
                serde_json::to_string(settings_config).map_err(|e| AppError::Database(format!(
                    "Failed to serialize settings_config: {e}"
                )))?,
                provider_id,
                app_type
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    /// 判断 providers 表是否为空（全 app_type 一起算）。
    ///
    /// 用于区分"全新安装"和"升级用户"：在启动流程 import/seed 之前调用。
    /// 使用 `EXISTS` 短路查询，比 `COUNT(*)` 在将来表变大时更高效。
    pub fn is_providers_empty(&self) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let exists: bool = conn
            .query_row("SELECT EXISTS(SELECT 1 FROM providers)", [], |row| {
                row.get(0)
            })
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(!exists)
    }

    /// 仅获取指定 app 下所有 provider 的 id 集合。
    ///
    /// 比 `get_all_providers` 轻量得多：只读 id 列、无 endpoint 子查询。
    /// 用于只需要做存在性检查的场景（如 additive 模式的 live 同步去重）。
    pub fn get_provider_ids(&self, app_type: &str) -> Result<HashSet<String>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare("SELECT id FROM providers WHERE app_type = ?1")
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![app_type], |row| row.get::<_, String>(0))
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut ids = HashSet::new();
        for row in rows {
            ids.insert(row.map_err(|e| AppError::Database(e.to_string()))?);
        }
        Ok(ids)
    }

    /// 判断指定 app 下是否已存在任意 provider。
    ///
    /// 启动阶段的 live import 需要使用这个更严格的判断：
    /// 只要该 app 已经有任何 provider（包括官方 seed），就不应再自动导入 `default`。
    pub fn has_any_provider_for_app(&self, app_type: &str) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM providers WHERE app_type = ?1)",
                params![app_type],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(exists)
    }

    /// 判断指定 app 下是否存在非官方种子的供应商。
    ///
    /// 比 `get_all_providers` 轻量得多：只读 id 列、无 endpoint 子查询、首条命中即返回。
    /// 用于 `import_default_config` 决定是否跳过 live 导入。
    pub fn has_non_official_seed_provider(&self, app_type: &str) -> Result<bool, AppError> {
        use crate::database::dao::providers_seed::is_official_seed_id;
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare("SELECT id FROM providers WHERE app_type = ?1")
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut rows = stmt
            .query(params![app_type])
            .map_err(|e| AppError::Database(e.to_string()))?;
        while let Some(row) = rows.next().map_err(|e| AppError::Database(e.to_string()))? {
            let id: String = row.get(0).map_err(|e| AppError::Database(e.to_string()))?;
            if !is_official_seed_id(&id) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// 计算指定 app 下一个可用的 sort_index（追加到末尾）。
    fn next_sort_index_for_app(&self, app_type: &str) -> Result<usize, AppError> {
        let conn = lock_conn!(self.conn);
        let max: Option<i64> = conn
            .query_row(
                "SELECT MAX(sort_index) FROM providers WHERE app_type = ?1",
                params![app_type],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(max.map(|v| (v + 1) as usize).unwrap_or(0))
    }

    /// 启动时调用：补齐缺失的官方预设供应商（Claude / Codex / Gemini）。
    ///
    /// 使用 settings flag `official_providers_seeded` 保证每个数据库只执行一次：
    /// - 全新用户：seed 三条官方预设
    /// - 老用户升级：同样会触发一次（flag 不存在），追加到末尾，不影响已有排序
    /// - 用户删除 seed 后：不再重建（flag 已为 true），尊重用户意图
    ///
    /// 与 `Database::save_provider` 的 UPSERT 语义配合，即使被意外重复调用
    /// 也不会覆盖用户当前激活的供应商（is_current 字段会被保留）。
    pub fn init_default_official_providers(&self) -> Result<usize, AppError> {
        use crate::database::dao::providers_seed::OFFICIAL_SEEDS;

        if self
            .get_bool_flag("official_providers_seeded")
            .unwrap_or(false)
        {
            return Ok(0);
        }

        let mut inserted = 0_usize;
        let now_ms = chrono::Utc::now().timestamp_millis();

        for seed in OFFICIAL_SEEDS {
            let app_type_str = seed.app_type.as_str();

            // 若该 id 已存在（极端情况：用户曾手动用过同 id），跳过
            if self.get_provider_by_id(seed.id, app_type_str)?.is_some() {
                continue;
            }

            let next_sort_index = self.next_sort_index_for_app(app_type_str)?;

            let settings_config: serde_json::Value =
                serde_json::from_str(seed.settings_config_json).map_err(|e| {
                    AppError::Database(format!("Seed JSON parse failed for {}: {e}", seed.id))
                })?;

            let mut provider = Provider::with_id(seed.id.to_string());
            provider.name = seed.name.to_string();
            provider.settings_config = settings_config;
            provider.website_url = Some(seed.website_url.to_string());
            provider.category = Some("official".to_string());
            provider.icon = Some(seed.icon.to_string());
            provider.icon_color = Some(seed.icon_color.to_string());
            provider.sort_index = Some(next_sort_index);
            provider.created_at = Some(now_ms);

            self.save_provider(app_type_str, &provider)?;
            inserted += 1;
            log::info!(
                "✓ Seeded official provider: {} ({})",
                seed.name,
                app_type_str
            );
        }

        // 即使 inserted=0（例如用户手动创建过同 id）也设置 flag 防止反复检查
        self.set_setting("official_providers_seeded", "true")?;

        Ok(inserted)
    }

    /// 按 id 兜底插入单条 official seed（仅当目标表中该 id 不存在时插入）。
    ///
    /// 与 `init_default_official_providers` 不同：
    /// - 不触碰 `official_providers_seeded` 全局 flag，是 on-demand 修复
    /// - 只处理一条 seed，由调用方决定 id + app_type
    /// - 已存在则尊重用户自定义，不覆盖
    ///
    /// 返回 Ok(true) 表示插入了新行，Ok(false) 表示已存在被跳过。
    pub fn ensure_official_seed_by_id(
        &self,
        seed_id: &str,
        app_type: crate::app_config::AppType,
    ) -> Result<bool, AppError> {
        use crate::database::dao::providers_seed::OFFICIAL_SEEDS;

        let seed = OFFICIAL_SEEDS
            .iter()
            .find(|s| s.id == seed_id && s.app_type == app_type)
            .ok_or_else(|| {
                AppError::Database(format!(
                    "unknown official seed: id={seed_id}, app_type={}",
                    app_type.as_str()
                ))
            })?;

        let app_type_str = seed.app_type.as_str();

        if self.get_provider_by_id(seed_id, app_type_str)?.is_some() {
            return Ok(false);
        }

        let settings_config: serde_json::Value = serde_json::from_str(seed.settings_config_json)
            .map_err(|e| {
                AppError::Database(format!("Seed JSON parse failed for {}: {e}", seed.id))
            })?;

        let next_sort_index = self.next_sort_index_for_app(app_type_str)?;
        let now_ms = chrono::Utc::now().timestamp_millis();

        let mut provider = Provider::with_id(seed.id.to_string());
        provider.name = seed.name.to_string();
        provider.settings_config = settings_config;
        provider.website_url = Some(seed.website_url.to_string());
        provider.category = Some("official".to_string());
        provider.icon = Some(seed.icon.to_string());
        provider.icon_color = Some(seed.icon_color.to_string());
        provider.sort_index = Some(next_sort_index);
        provider.created_at = Some(now_ms);

        self.save_provider(app_type_str, &provider)?;

        Ok(true)
    }
}

#[cfg(test)]
mod ensure_official_seed_tests {
    use crate::app_config::AppType;
    use crate::database::{Database, CODEX_OFFICIAL_PROVIDER_ID};

    const CLAUDE_OFFICIAL_PROVIDER_ID: &str = "claude-official";

    #[test]
    fn ensure_inserts_when_missing() {
        let db = Database::memory().expect("memory db");
        let inserted = db
            .ensure_official_seed_by_id(CLAUDE_OFFICIAL_PROVIDER_ID, AppType::Claude)
            .expect("ensure ok");
        assert!(inserted, "should insert when missing");

        let provider = db
            .get_provider_by_id(CLAUDE_OFFICIAL_PROVIDER_ID, AppType::Claude.as_str())
            .expect("query ok")
            .expect("provider exists after ensure");

        assert_eq!(provider.id, CLAUDE_OFFICIAL_PROVIDER_ID);
        assert_eq!(provider.name, "Claude Official");
        assert_eq!(provider.category.as_deref(), Some("official"));
        assert_eq!(provider.icon.as_deref(), Some("anthropic"));
        assert_eq!(provider.icon_color.as_deref(), Some("#D4915D"));
    }

    #[test]
    fn ensure_skips_when_present_and_preserves_customization() {
        let db = Database::memory().expect("memory db");
        db.init_default_official_providers().expect("seed");

        let mut renamed = db
            .get_provider_by_id(CLAUDE_OFFICIAL_PROVIDER_ID, AppType::Claude.as_str())
            .expect("query ok")
            .expect("seed present");
        renamed.name = "My Custom Backup".to_string();
        db.save_provider(AppType::Claude.as_str(), &renamed)
            .expect("save customization");

        let inserted = db
            .ensure_official_seed_by_id(CLAUDE_OFFICIAL_PROVIDER_ID, AppType::Claude)
            .expect("ensure ok");
        assert!(!inserted, "should skip when present");

        let after = db
            .get_provider_by_id(CLAUDE_OFFICIAL_PROVIDER_ID, AppType::Claude.as_str())
            .expect("query ok")
            .expect("still present");
        assert_eq!(
            after.name, "My Custom Backup",
            "customization must not be overwritten"
        );
    }

    #[test]
    fn ensure_recreates_codex_official_seed_after_deletion() {
        let db = Database::memory().expect("memory db");
        db.init_default_official_providers().expect("seed");
        db.delete_provider(AppType::Codex.as_str(), CODEX_OFFICIAL_PROVIDER_ID)
            .expect("delete Codex official");

        let inserted = db
            .ensure_official_seed_by_id(CODEX_OFFICIAL_PROVIDER_ID, AppType::Codex)
            .expect("ensure Codex official");
        assert!(inserted);
        let provider = db
            .get_provider_by_id(CODEX_OFFICIAL_PROVIDER_ID, AppType::Codex.as_str())
            .expect("query")
            .expect("Codex official restored");
        assert_eq!(provider.category.as_deref(), Some("official"));
        assert_eq!(provider.settings_config["auth"], serde_json::json!({}));
    }

    #[test]
    fn ensure_rejects_unknown_seed() {
        let db = Database::memory().expect("memory db");
        let result = db.ensure_official_seed_by_id("nonexistent-id", AppType::Claude);
        assert!(result.is_err(), "unknown seed id should be Err");
    }

    #[test]
    fn ensure_rejects_seed_app_type_mismatch() {
        let db = Database::memory().expect("memory db");
        let result = db.ensure_official_seed_by_id(CLAUDE_OFFICIAL_PROVIDER_ID, AppType::Codex);
        assert!(result.is_err(), "(id, app_type) mismatch should be Err");
    }
}
