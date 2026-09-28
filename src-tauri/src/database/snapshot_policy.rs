//! S0（施工方案 §5.1 / §6）：同步与 SQL 导入导出的数据边界——策略集中定义。
//!
//! 施工方案 §2.2-2：「哪些表/键算设备本地、导出时怎么裁剪、导入时怎么合并」
//! **只允许在本模块定义**。同步导出、手动导出、同步导入、手动导入都必须调用
//! 本模块，禁止在各个命令里各写一套 `if is_1p`。
//!
//! 进度：S3-1/S3-2（导出裁剪与 meta）已落地；S4（导入合并）仍为骨架。
//! §5.1 的数据分级：
//! - A 共享配置：providers、mcp_servers、prompts、skills、skill_repos、profiles，
//!   以及 settings 中除 B 级以外的键；
//! - B 设备本地：[`DEVICE_LOCAL_SETTING_KEYS`]（本机投递登记、迁移标记、
//!   代理与日志偏好、当前 Profile 等）——导出删、导入保留本机值；
//! - C 凭据缓存：provider_endpoints——1P 模式不导出（D-S1），凭据管理器照旧随同步；
//! - D 引用：secret_refs——只带 `vault_id != ''` 的行（D-S3）。

use crate::error::AppError;
use rusqlite::Connection;

/// 导出用途（S3-1）：云同步快照与手动配置导出共用同一套裁剪规则，仅
/// `provider_endpoints`（C 级）与 meta 用途标记不同（D-S1 / S3-2）。
#[allow(dead_code)] // Sync/ConfigFile 仅在 meta 里区分（S3-2 之外的用途暂未分化）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExportPurpose {
    /// WebDAV / S3 云同步快照
    Sync,
    /// 手动 SQL 配置导出
    ConfigFile,
}

/// 导出元信息（S3-2）：写入 SQL 头部 `-- cc-switch-meta:` 行。
/// 只用于导入确认框的提示与统计，**绝不参与导入侧的安全决策**（§2.2-4：
/// 按本机模式决定，不信任文件）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExportMeta {
    /// 用途标记：`sync` / `config`
    pub purpose: String,
    /// 导出方后端（`onepassword` / `credential_manager`）
    pub backend: String,
    /// 是否携带端点缓存（1P 模式恒为 false，D-S1）
    pub endpoints: bool,
    /// 携带的 1P 引用行数（D-S3）
    pub refs: usize,
    /// 导出设备名（`normalize_device_name` 产物，不得含换行）
    pub device: String,
    /// 导出时间（RFC 3339）
    pub exported_at: String,
}

/// 本机凭据后端（S4-1）：导入侧的采纳/保留规则由**导入方**模式决定。
#[allow(dead_code)] // S0 骨架：S4-1 接线后启用
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackendKind {
    OnePassword,
    CredentialManager,
}

/// 导入合并策略（S4-1）：`merge_for_import` 的输入。`local_vault` 取
/// `settings.onepassword.vault`，与 `OnePasswordVault` 的 `self.vault` 同源
/// （施工时核实，见 §5.4 S4-1）。
#[allow(dead_code)] // S0 骨架：S4-1 接线后启用
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportPolicy {
    pub(crate) local_backend: BackendKind,
    /// 本机保险箱 id；仅凭据管理器模式为 `None`
    pub(crate) local_vault: Option<String>,
}

/// 导入报告（S4-1/S4-3）：三条导入路径（同步下载 / SQL 导入 / `.db` 恢复）
/// 共用的统计与「未关联供应商」清单。
#[allow(dead_code)] // S0 骨架：S4-1 生成、S4-3 消费后启用
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ImportReport {
    /// 采纳的远端引用行数（vault 匹配才采纳，D-S3）
    pub(crate) adopted_refs: usize,
    /// 导入后存在、但本机既无引用也未采纳到引用的供应商 `(app, id)`
    pub(crate) unlinked_providers: Vec<(String, String)>,
    /// 清理掉的失效引用行数（供应商已不存在）
    pub(crate) pruned_refs: usize,
    /// 清理掉的失效端点行数（供应商已不存在）
    pub(crate) pruned_endpoints: usize,
}

// ─── S3-1：导出侧裁剪 ───────────────────────────────────────────────

/// B 级「设备本地」settings 键（§5.1 / D-S2）。这些行描述的是**本机**的投递
/// 登记、迁移进度与运行环境偏好，被另一台设备的值覆盖会直接造成错误行为：
/// - `managed_env_vars`：本机向 `HKCU\Environment` 写过哪些变量，被覆盖后
///   清理投递会漏删自己写过的变量（明文钥匙残留），或把别人的变量当成自己的；
/// - `secrets_migration_*` / `live_reapply_pending` / `legacy_secret_recovery`：
///   被覆盖会让下次启动误触发迁移或整文件重写 live；
/// - `global_proxy_url*`：绕过了 DAO 的 `@` 校验，被远端值直写；
/// - `log_config` / `current_profile_id_*`：本机偏好与本机当前供应商配套。
#[allow(dead_code)] // S4-1 导入侧复用同一份名单
pub(crate) const DEVICE_LOCAL_SETTING_KEYS: &[&str] = &[
    "managed_env_vars",
    "known_secret_targets",
    "secrets_migration_pending",
    "secrets_migration_report",
    "secrets_migration_confirmed",
    "live_reapply_pending",
    "legacy_secret_recovery",
    "skills_ssot_migration_pending",
    "skills_ssot_migration_snapshot",
    "official_providers_seeded",
    "global_proxy_url",
    "global_proxy_url_invalidated",
    "log_config",
];

/// B 级键的前缀匹配（`current_profile_id_*`：当前 Profile 与本机当前供应商配套）。
#[allow(dead_code)] // S4-1 导入侧复用
pub(crate) const DEVICE_LOCAL_SETTING_PREFIXES: &[&str] = &["current_profile_id_"];

/// 本机凭据后端（导出侧用；与 [`BackendKind`] 语义一致，此处独立构造以免
/// 导入侧类型进入导出路径）。
#[allow(dead_code)] // S4-1 起由 ImportPolicy 取代
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalBackend {
    OnePassword,
    CredentialManager,
}

/// 在**导出副本**上按 §5.1 的分级裁剪，返回本次导出的元信息（S3-1/S3-2）。
///
/// `snapshot` 必须是 [`crate::database::Database::snapshot_to_memory`] 得到的
/// 内存连接：**绝不能在主库上裁剪**——主库 DELETE 会触发 update_hook → 连带
/// 上传一份残缺快照（§9-1）。内存连接没有注册 update_hook（只在主库 `init` /
/// `memory()` 里注册），在这里 DELETE 不会触发自动同步。
///
/// 裁剪内容：
/// 1. B 级 settings 键（含前缀）——两种模式都删；
/// 2. `provider_endpoints`——1P 模式删（D3-B 之后 1P 是端点真源，带出去既
///    多余又造成 P0-3）；凭据管理器模式保留（该模式没有自带同步的真源，随
///    同步走是 D3-A 给这类用户的便利，不回归，§2.1-5 / D-S1）；
/// 3. `secret_refs` 中 `vault_id = ''` 的行——凭据管理器的占位引用对其他设备
///    没有意义；1P 引用照常带出（D-S3）。
pub(crate) fn prune_for_export(
    snapshot: &Connection,
    purpose: ExportPurpose,
    local_backend: LocalBackend,
) -> Result<ExportMeta, AppError> {
    delete_device_local_settings(snapshot)?;

    let endpoints_included = match local_backend {
        LocalBackend::OnePassword => {
            snapshot
                .execute("DELETE FROM provider_endpoints", [])
                .map_err(|e| AppError::Database(format!("裁剪端点缓存失败: {e}")))?;
            false
        }
        LocalBackend::CredentialManager => true,
    };

    snapshot
        .execute("DELETE FROM secret_refs WHERE vault_id = ''", [])
        .map_err(|e| AppError::Database(format!("裁剪占位引用失败: {e}")))?;
    let refs_included: i64 = snapshot
        .query_row("SELECT COUNT(*) FROM secret_refs", [], |row| row.get(0))
        .map_err(|e| AppError::Database(format!("统计引用行数失败: {e}")))?;

    Ok(ExportMeta {
        purpose: match purpose {
            ExportPurpose::Sync => "sync".to_string(),
            ExportPurpose::ConfigFile => "config".to_string(),
        },
        backend: match local_backend {
            LocalBackend::OnePassword => "onepassword".to_string(),
            LocalBackend::CredentialManager => "credential_manager".to_string(),
        },
        endpoints: endpoints_included,
        refs: refs_included as usize,
        device: crate::services::sync_protocol::detect_system_device_name()
            .unwrap_or_else(|| "unknown".to_string()),
        exported_at: chrono::Utc::now().to_rfc3339(),
    })
}

/// 删除 B 级 settings 行（键名精确匹配或前缀匹配）。
pub(crate) fn delete_device_local_settings(conn: &Connection) -> Result<(), AppError> {
    for key in DEVICE_LOCAL_SETTING_KEYS {
        conn.execute("DELETE FROM settings WHERE key = ?1", [key])
            .map_err(|e| AppError::Database(format!("裁剪设备本地设置 {key} 失败: {e}")))?;
    }
    for prefix in DEVICE_LOCAL_SETTING_PREFIXES {
        // 前缀本身不含 LIKE 通配符（`_` 结尾后直接接 `%`），无需 ESCAPE。
        conn.execute(
            "DELETE FROM settings WHERE key LIKE ?1",
            [format!("{prefix}%")],
        )
        .map_err(|e| AppError::Database(format!("裁剪设备本地设置前缀 {prefix} 失败: {e}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! S0：仅锁定类型的可构造性与接口形状，行为断言由 S3/S4 的测试补齐。

    use super::*;

    #[test]
    fn skeleton_types_are_constructible() {
        assert_eq!(ExportPurpose::Sync, ExportPurpose::Sync);
        assert_ne!(BackendKind::OnePassword, BackendKind::CredentialManager);
        let policy = ImportPolicy {
            local_backend: BackendKind::OnePassword,
            local_vault: Some("vault-x".to_string()),
        };
        assert_eq!(policy.local_vault.as_deref(), Some("vault-x"));
        let report = ImportReport::default();
        assert_eq!(report.adopted_refs, 0);
        assert!(report.unlinked_providers.is_empty());
    }
}
