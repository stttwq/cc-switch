//! 通用设置数据访问对象
//!
//! 提供键值对形式的通用设置存储。

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::params;

impl Database {
    const LEGACY_COMMON_CONFIG_MIGRATED_KEY: &'static str = "common_config_legacy_migrated_v1";

    fn config_snippet_cleared_key(app_type: &str) -> String {
        format!("common_config_{app_type}_cleared")
    }

    /// 获取设置值
    pub fn get_setting(&self, key: &str) -> Result<Option<String>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare("SELECT value FROM settings WHERE key = ?1")
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut rows = stmt
            .query(params![key])
            .map_err(|e| AppError::Database(e.to_string()))?;

        if let Some(row) = rows.next().map_err(|e| AppError::Database(e.to_string()))? {
            Ok(Some(
                row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
            ))
        } else {
            Ok(None)
        }
    }

    /// 以布尔语义读取 flag：`"true"` 或 `"1"` → true，其它全部 false。
    ///
    /// 用于一次性启动 flag（`official_providers_seeded` / `first_run_notice_shown` 等）。
    /// 与 `is_legacy_common_config_migrated` 等只认 `"true"` 的历史辅助函数**不同**——
    /// 这里同时接受 `"1"` 是为了兼容 `init_default_official_providers` 既有写法。
    pub fn get_bool_flag(&self, key: &str) -> Result<bool, AppError> {
        Ok(matches!(
            self.get_setting(key)?.as_deref(),
            Some("true") | Some("1")
        ))
    }

    /// 设置值
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            params![key, value],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    // --- 通用配置片段 (Common Config Snippet) ---

    /// 获取通用配置片段
    pub fn get_config_snippet(&self, app_type: &str) -> Result<Option<String>, AppError> {
        self.get_setting(&format!("common_config_{app_type}"))
    }

    /// 检查通用配置片段是否被用户显式清空
    pub fn is_config_snippet_cleared(&self, app_type: &str) -> Result<bool, AppError> {
        Ok(self
            .get_setting(&Self::config_snippet_cleared_key(app_type))?
            .as_deref()
            == Some("true"))
    }

    /// 设置通用配置片段是否被显式清空
    pub fn set_config_snippet_cleared(
        &self,
        app_type: &str,
        cleared: bool,
    ) -> Result<(), AppError> {
        let key = Self::config_snippet_cleared_key(app_type);
        if cleared {
            self.set_setting(&key, "true")
        } else {
            let conn = lock_conn!(self.conn);
            conn.execute("DELETE FROM settings WHERE key = ?1", params![key])
                .map_err(|e| AppError::Database(e.to_string()))?;
            Ok(())
        }
    }

    /// 当前是否允许从 live 配置自动抽取通用配置片段
    pub fn should_auto_extract_config_snippet(&self, app_type: &str) -> Result<bool, AppError> {
        Ok(self.get_config_snippet(app_type)?.is_none()
            && !self.is_config_snippet_cleared(app_type)?)
    }

    /// 检查历史通用配置迁移是否已经执行过
    pub fn is_legacy_common_config_migrated(&self) -> Result<bool, AppError> {
        Ok(self
            .get_setting(Self::LEGACY_COMMON_CONFIG_MIGRATED_KEY)?
            .as_deref()
            == Some("true"))
    }

    /// 标记历史通用配置迁移已经执行完成
    pub fn set_legacy_common_config_migrated(&self, migrated: bool) -> Result<(), AppError> {
        if migrated {
            self.set_setting(Self::LEGACY_COMMON_CONFIG_MIGRATED_KEY, "true")
        } else {
            let conn = lock_conn!(self.conn);
            conn.execute(
                "DELETE FROM settings WHERE key = ?1",
                params![Self::LEGACY_COMMON_CONFIG_MIGRATED_KEY],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
            Ok(())
        }
    }

    /// 设置通用配置片段
    pub fn set_config_snippet(
        &self,
        app_type: &str,
        snippet: Option<String>,
    ) -> Result<(), AppError> {
        let key = format!("common_config_{app_type}");
        if let Some(value) = snippet {
            self.set_setting(&key, &value)
        } else {
            // 如果为 None 则删除
            let conn = lock_conn!(self.conn);
            conn.execute("DELETE FROM settings WHERE key = ?1", params![key])
                .map_err(|e| AppError::Database(e.to_string()))?;
            Ok(())
        }
    }

    // --- 全局出站代理 ---

    /// 全局代理 URL 的存储键名
    const GLOBAL_PROXY_URL_KEY: &'static str = "global_proxy_url";

    /// 获取全局出站代理 URL
    ///
    /// 返回 None 表示未配置或已清除代理（直连）
    /// 返回 Some(url) 表示已配置代理
    pub fn get_global_proxy_url(&self) -> Result<Option<String>, AppError> {
        self.get_setting(Self::GLOBAL_PROXY_URL_KEY)
    }

    /// 设置全局出站代理 URL
    ///
    /// - 传入非空字符串：启用代理
    /// - 传入空字符串或 None：清除代理设置（直连）
    ///
    /// D10 / S10：在 DAO 这个唯一写入口拒绝 `user:pass@` 形式，
    /// 这样导入 SQL、同步还原等旁路也无法把代理凭据写进 DB。
    pub fn set_global_proxy_url(&self, url: Option<&str>) -> Result<(), AppError> {
        match url {
            Some(u) if !u.trim().is_empty() => {
                let trimmed = u.trim();
                if trimmed.contains('@') {
                    return Err(AppError::Config(
                        "代理 URL 不允许包含用户名或密码".to_string(),
                    ));
                }
                self.set_setting(Self::GLOBAL_PROXY_URL_KEY, trimmed)
            }
            _ => {
                // 清除代理设置
                let conn = lock_conn!(self.conn);
                conn.execute(
                    "DELETE FROM settings WHERE key = ?1",
                    params![Self::GLOBAL_PROXY_URL_KEY],
                )
                .map_err(|e| AppError::Database(e.to_string()))?;
                Ok(())
            }
        }
    }

    // --- 日志配置 ---

    /// 获取日志配置
    pub fn get_log_config(&self) -> Result<crate::settings::LogConfig, AppError> {
        match self.get_setting("log_config")? {
            Some(json) => serde_json::from_str(&json)
                .map_err(|e| AppError::Database(format!("解析日志配置失败: {e}"))),
            None => Ok(crate::settings::LogConfig::default()),
        }
    }

    /// 更新日志配置
    pub fn set_log_config(&self, config: &crate::settings::LogConfig) -> Result<(), AppError> {
        let json = serde_json::to_string(config)
            .map_err(|e| AppError::Database(format!("序列化日志配置失败: {e}")))?;
        self.set_setting("log_config", &json)
    }

    // --- 同步恢复 commit marker（REL-A）---

    // 「写入」在 `backup.rs` 的暂存库连接上直写（随整库替换原子生效），
    // 这里只提供针对主库的查询与清理。

    /// 查询某次同步操作的 commit marker 是否存在。
    pub(crate) fn has_sync_commit_marker(&self, op_id: &str) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM local_sync_commit WHERE op_id = ?1",
                params![op_id],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(format!("读取同步 commit marker 失败: {e}")))?;
        Ok(count > 0)
    }

    /// 清除某次操作的 marker（恢复完成后）。
    pub(crate) fn clear_sync_commit_marker(&self, op_id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "DELETE FROM local_sync_commit WHERE op_id = ?1",
            params![op_id],
        )
        .map_err(|e| AppError::Database(format!("清除同步 commit marker 失败: {e}")))?;
        Ok(())
    }

    /// 清除全部 marker。marker 只在有 journal 时有意义，无 journal 时属残留，
    /// 启动恢复顺手清理（幂等）。
    pub(crate) fn clear_all_sync_commit_markers(&self) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute("DELETE FROM local_sync_commit", [])
            .map_err(|e| AppError::Database(format!("清除同步 commit marker 失败: {e}")))?;
        Ok(())
    }
}
