//! `secret_refs` 引用表访问（施工方案 §4.3）。
//!
//! 只存「条目引用」（vault id / item id / 字段名清单），**不存值**——这些不是秘密。
//! 列表徽标、extra_env 变量名、删除找条目、`provider_has_stored_key` 全查这张表，
//! 零 vault 往返。写入侧在每次 `vault.put` / `vault.delete` 后同步维护。

use crate::database::{lock_conn, Database};
use crate::error::AppError;

impl Database {
    /// 整包写入后登记引用（覆盖式 upsert）。`fields` 只含字段名，绝不含值。
    pub fn upsert_secret_ref(
        &self,
        app: &str,
        provider_id: &str,
        vault_id: &str,
        item_id: &str,
        fields: &[String],
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let fields_json = serde_json::to_string(fields)
            .map_err(|e| AppError::Database(format!("secret_refs fields 序列化失败: {e}")))?;
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO secret_refs (app, provider_id, vault_id, item_id, fields, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(app, provider_id) DO UPDATE SET
                 vault_id = excluded.vault_id,
                 item_id = excluded.item_id,
                 fields = excluded.fields,
                 updated_at = excluded.updated_at",
            rusqlite::params![app, provider_id, vault_id, item_id, fields_json, now],
        )
        .map_err(|e| AppError::Database(format!("写入 secret_refs 失败: {e}")))?;
        Ok(())
    }

    /// 删除某供应商的引用行。
    pub fn delete_secret_ref(&self, app: &str, provider_id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "DELETE FROM secret_refs WHERE app = ?1 AND provider_id = ?2",
            rusqlite::params![app, provider_id],
        )
        .map_err(|e| AppError::Database(format!("删除 secret_refs 失败: {e}")))?;
        Ok(())
    }

    /// 读取某供应商登记的字段名清单；无引用行返回 `None`。
    pub fn get_secret_ref_fields(
        &self,
        app: &str,
        provider_id: &str,
    ) -> Result<Option<Vec<String>>, AppError> {
        let conn = lock_conn!(self.conn);
        let raw: Option<String> = conn
            .query_row(
                "SELECT fields FROM secret_refs WHERE app = ?1 AND provider_id = ?2",
                rusqlite::params![app, provider_id],
                |row| row.get(0),
            )
            .ok();
        match raw {
            Some(json) => {
                let fields: Vec<String> = serde_json::from_str(&json)
                    .map_err(|e| AppError::Database(format!("secret_refs fields 解析失败: {e}")))?;
                Ok(Some(fields))
            }
            None => Ok(None),
        }
    }

    /// 读取引用行的身份（vault_id, item_id）；无行返回 `None`。
    /// F2-1「已迁移」判定与 F2-2 item_id 直达读写都用它（0 次 vault 往返）。
    pub fn get_secret_ref_identity(
        &self,
        app: &str,
        provider_id: &str,
    ) -> Result<Option<(String, String)>, AppError> {
        let conn = lock_conn!(self.conn);
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT vault_id, item_id FROM secret_refs WHERE app = ?1 AND provider_id = ?2",
                rusqlite::params![app, provider_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();
        Ok(row)
    }

    /// F2-2：标题兜底找到条目后，把真实 item id 回写进已有引用行（字段清单保持不变）。
    /// 返回是否有行被更新（无行时调用方应走 `upsert_secret_ref` 整行登记）。
    pub fn repair_secret_ref_item_id(
        &self,
        app: &str,
        provider_id: &str,
        vault_id: &str,
        item_id: &str,
    ) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let now = chrono::Utc::now().timestamp();
        let n = conn
            .execute(
                "UPDATE secret_refs SET vault_id = ?3, item_id = ?4, updated_at = ?5
                 WHERE app = ?1 AND provider_id = ?2",
                rusqlite::params![app, provider_id, vault_id, item_id, now],
            )
            .map_err(|e| AppError::Database(format!("修复 secret_ref item_id 失败: {e}")))?;
        Ok(n > 0)
    }

    /// secret_refs 行数（诊断用）。
    pub fn count_secret_refs(&self) -> Result<i64, AppError> {
        let conn = lock_conn!(self.conn);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM secret_refs", [], |row| row.get(0))
            .map_err(|e| AppError::Database(format!("统计 secret_refs 失败: {e}")))?;
        Ok(count)
    }

    /// S4-5（端点对账）：列出全部引用行 `(app, provider_id, vault_id, item_id)`。
    ///
    /// 纯本地查询，0 次 vault 往返。诊断命令据此遍历「本机认为已关联的供应商」，
    /// 逐个与 1Password 真值比对。返回值只含结构定位信息，不含任何秘密。
    pub fn list_secret_ref_identities(
        &self,
    ) -> Result<Vec<(String, String, String, String)>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT app, provider_id, vault_id, item_id FROM secret_refs
                 ORDER BY app, provider_id",
            )
            .map_err(|e| AppError::Database(format!("准备读取 secret_refs 失败: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| AppError::Database(format!("读取 secret_refs 失败: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| AppError::Database(format!("读取 secret_refs 行失败: {e}")))?);
        }
        Ok(out)
    }
}
