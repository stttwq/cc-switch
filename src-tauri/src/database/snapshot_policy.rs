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

use crate::database::{lock_conn, Database};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackendKind {
    OnePassword,
    CredentialManager,
}

/// 导入合并策略（S4-1）：`merge_for_import` 的输入。`local_vault` 取
/// `settings.onepassword.vault`，与 `OnePasswordVault` 的 `self.vault` 同源
/// （施工时核实，见 §5.4 S4-1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportPolicy {
    pub(crate) local_backend: BackendKind,
    /// 本机保险箱 id；仅凭据管理器模式为 `None`
    pub(crate) local_vault: Option<String>,
}

/// 导入报告（S4-1/S4-3）：三条导入路径（同步下载 / SQL 导入 / `.db` 恢复）
/// 共用的统计与「未关联供应商」清单。
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
pub(crate) const DEVICE_LOCAL_SETTING_PREFIXES: &[&str] = &["current_profile_id_"];

/// 本机凭据后端（导出侧用；与 [`BackendKind`] 语义一致，此处独立构造以免
/// 导入侧类型进入导出路径）。
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
/// 2. `mcp_approvals`（SEC-A，B 级设备本地）——审批只对本机有效，外部文件里
///    的批准记录绝不能随导入复活（§4.3-8）；
/// 3. `local_sync_commit`（REL-A，B 级设备本地）——本机恢复状态不随同步传播；
/// 4. `provider_endpoints`——1P 模式删（D3-B 之后 1P 是端点真源，带出去既
///    多余又造成 P0-3）；凭据管理器模式保留（该模式没有自带同步的真源，随
///    同步走是 D3-A 给这类用户的便利，不回归，§2.1-5 / D-S1）；
/// 5. `secret_refs` 中 `vault_id = ''` 的行——凭据管理器的占位引用对其他设备
///    没有意义；1P 引用照常带出（D-S3）。
pub(crate) fn prune_for_export(
    snapshot: &Connection,
    purpose: ExportPurpose,
    local_backend: LocalBackend,
) -> Result<ExportMeta, AppError> {
    delete_device_local_settings(snapshot)?;

    // SEC-A：MCP 审批是设备本机数据（B 级），导出一律剔除。
    snapshot
        .execute("DELETE FROM mcp_approvals", [])
        .map_err(|e| AppError::Database(format!("裁剪 MCP 审批记录失败: {e}")))?;

    // REL-A：同步恢复 commit marker 是本机生成的恢复状态（B 级），不随同步传播。
    snapshot
        .execute("DELETE FROM local_sync_commit", [])
        .map_err(|e| AppError::Database(format!("裁剪同步 commit marker 失败: {e}")))?;

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

/// 一条 `secret_refs` 行（导入期间在内存里搬运，不留在暂存库里）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct RefRow {
    app: String,
    provider_id: String,
    vault_id: String,
    item_id: String,
    fields: String,
}

/// S4-1：在**暂存库**上按 §5.1 的分级合并本机数据，随后才整库替换主库。
///
/// 必须在持有主库锁、与整库替换同一时间点的块内调用（§9-2）——否则暂存期间
/// 本机新写入的引用/端点会丢失。全程 **0 次 op**（§9-3）：只碰两个连接。
///
/// 规则（由**导入方**模式决定，不信任文件里的 meta，§2.2-4）：
/// 1. **B 级 settings**：丢掉暂存库里的这些键，把主库当前值原样拷回（备份里的
///    `live_reapply_pending` 等标记不应复活）；
/// 2. **C 级端点**：1P 模式忽略文件内容、保留本机缓存；凭据管理器模式远端优先、
///    本机独有保留。两者都删除「供应商已不存在」的行（P1-2）；
/// 3. **D 级引用**：本机行一律保留；本机没有、且「本机是 1P 模式 + 行 vault 等于
///    本机保险箱 + 供应商存在于导入后的 providers」的远端行才**采纳**（D-S3——
///    同一保险箱的 item id 在各设备一致，采纳可把 N+1 次 op 降到 0）。最后删除
///    失效行。
pub(crate) fn merge_for_import(
    main: &Connection,
    staging: &Connection,
    policy: &ImportPolicy,
) -> Result<ImportReport, AppError> {
    let tx = staging
        .unchecked_transaction()
        .map_err(|e| AppError::Database(format!("开启导入合并事务失败: {e}")))?;

    merge_device_local_settings(main, &tx)?;
    merge_mcp_approvals(main, &tx)?;
    merge_local_sync_commit(main, &tx)?;
    let pruned_endpoints = merge_endpoints(main, &tx, policy)?;
    let (pruned_refs, adopted_refs, unlinked) = merge_secret_refs(main, &tx, policy)?;

    tx.commit()
        .map_err(|e| AppError::Database(format!("提交导入合并事务失败: {e}")))?;

    Ok(ImportReport {
        adopted_refs,
        unlinked_providers: unlinked,
        pruned_refs,
        pruned_endpoints,
    })
}

/// B 级：丢弃暂存库的值，拷回主机当前值。
fn merge_device_local_settings(main: &Connection, tx: &Connection) -> Result<(), AppError> {
    for (predicate, param) in device_local_setting_predicates() {
        tx.execute(
            &format!("DELETE FROM settings WHERE {predicate}"),
            [&param as &dyn rusqlite::ToSql],
        )
        .map_err(|e| AppError::Database(format!("清空暂存库设备本地设置失败: {e}")))?;
    }
    for (predicate, param) in device_local_setting_predicates() {
        let sql = format!("SELECT key, value FROM settings WHERE {predicate}");
        let mut stmt = main
            .prepare(&sql)
            .map_err(|e| AppError::Database(format!("读取本机设备本地设置失败: {e}")))?;
        let rows = stmt.query_map([&param as &dyn rusqlite::ToSql], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        });
        let rows = match rows {
            Ok(rows) => rows,
            Err(e) => return Err(AppError::Database(format!("读取本机设备本地设置失败: {e}"))),
        };
        for row in rows.flatten() {
            tx.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                rusqlite::params![row.0, row.1],
            )
            .map_err(|e| AppError::Database(format!("回拷设备本地设置失败: {e}")))?;
        }
    }
    Ok(())
}

/// B 级键的 (谓词, 绑定参数) 列表：精确键用 `=`，前缀用 `LIKE`。
fn device_local_setting_predicates() -> Vec<(&'static str, String)> {
    DEVICE_LOCAL_SETTING_KEYS
        .iter()
        .map(|key| ("key = ?1", (*key).to_string()))
        .chain(
            DEVICE_LOCAL_SETTING_PREFIXES
                .iter()
                .map(|prefix| ("key LIKE ?1", format!("{prefix}%"))),
        )
        .collect()
}

/// SEC-A（B 级）：MCP 审批记录。丢掉暂存库（外部文件）的值，拷回本机当前
/// 值——外部载荷里的「批准」绝不能被采纳，本机已批准的条目也不能被外部
/// 数据撤销。
fn merge_mcp_approvals(main: &Connection, tx: &Connection) -> Result<(), AppError> {
    tx.execute("DELETE FROM mcp_approvals", [])
        .map_err(|e| AppError::Database(format!("清空暂存库 MCP 审批记录失败: {e}")))?;
    let mut stmt = main
        .prepare("SELECT server_id, app, approved_revision, approved_at FROM mcp_approvals")
        .map_err(|e| AppError::Database(format!("读取本机 MCP 审批记录失败: {e}")))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|e| AppError::Database(format!("读取本机 MCP 审批记录失败: {e}")))?;
    for row in rows.flatten() {
        tx.execute(
            "INSERT OR REPLACE INTO mcp_approvals (server_id, app, approved_revision, approved_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![row.0, row.1, row.2, row.3],
        )
        .map_err(|e| AppError::Database(format!("回拷 MCP 审批记录失败: {e}")))?;
    }
    Ok(())
}

/// REL-A（B 级）：同步恢复 commit marker。本机生成的恢复状态不随导入变化——
/// 丢弃暂存库的值、拷回本机当前值（未解决的恢复在无关导入后仍可继续）。
fn merge_local_sync_commit(main: &Connection, tx: &Connection) -> Result<(), AppError> {
    tx.execute("DELETE FROM local_sync_commit", [])
        .map_err(|e| AppError::Database(format!("清空暂存库同步 commit marker 失败: {e}")))?;
    let mut stmt = main
        .prepare("SELECT op_id, committed_at FROM local_sync_commit")
        .map_err(|e| AppError::Database(format!("读取本机同步 commit marker 失败: {e}")))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|e| AppError::Database(format!("读取本机同步 commit marker 失败: {e}")))?;
    for row in rows.flatten() {
        tx.execute(
            "INSERT OR REPLACE INTO local_sync_commit (op_id, committed_at) VALUES (?1, ?2)",
            rusqlite::params![row.0, row.1],
        )
        .map_err(|e| AppError::Database(format!("回拷同步 commit marker 失败: {e}")))?;
    }
    Ok(())
}

/// C 级：端点缓存。返回清理掉的孤儿行数。
fn merge_endpoints(
    main: &Connection,
    tx: &Connection,
    policy: &ImportPolicy,
) -> Result<usize, AppError> {
    if policy.local_backend == BackendKind::OnePassword {
        // 1P 模式：端点真源在 1Password，文件里的缓存一律忽略，只保留本机当前缓存。
        tx.execute("DELETE FROM provider_endpoints", [])
            .map_err(|e| AppError::Database(format!("清空暂存库端点缓存失败: {e}")))?;
        for row in endpoint_rows(main)? {
            insert_endpoint(tx, &row, true)?;
        }
    } else {
        // 凭据管理器：远端优先（暂存库已有），只补本机独有的键。
        for row in endpoint_rows(main)? {
            insert_endpoint(tx, &row, false)?;
        }
    }
    tx.execute(
        "DELETE FROM provider_endpoints
         WHERE NOT EXISTS (SELECT 1 FROM providers p
                           WHERE p.app_type = provider_endpoints.app
                             AND p.id = provider_endpoints.provider_id)",
        [],
    )
    .map_err(|e| AppError::Database(format!("清理失效端点行失败: {e}")))
}

type EndpointRow = (String, String, String, i64);

fn endpoint_rows(conn: &Connection) -> Result<Vec<EndpointRow>, AppError> {
    let mut stmt = conn
        .prepare("SELECT app, provider_id, base_url, updated_at FROM provider_endpoints")
        .map_err(|e| AppError::Database(format!("读取端点缓存失败: {e}")))?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    });
    let rows = match rows {
        Ok(rows) => rows,
        Err(e) => return Err(AppError::Database(format!("读取端点缓存失败: {e}"))),
    };
    Ok(rows.flatten().collect())
}

fn insert_endpoint(tx: &Connection, row: &EndpointRow, replace: bool) -> Result<(), AppError> {
    let verb = if replace { "OR REPLACE" } else { "OR IGNORE" };
    tx.execute(
        &format!(
            "INSERT {verb} INTO provider_endpoints (app, provider_id, base_url, updated_at)
             VALUES (?1, ?2, ?3, ?4)"
        ),
        rusqlite::params![row.0, row.1, row.2, row.3],
    )
    .map_err(|e| AppError::Database(format!("写入端点缓存失败: {e}")))?;
    Ok(())
}

/// D 级引用的合并结果：(清理掉的失效行数, 采纳的远端行数, 未关联供应商)。
type RefMergeOutcome = (usize, usize, Vec<(String, String)>);

/// D 级：引用合并（本机行保留 + 同保险箱时采纳远端 + 清理失效行）。
fn merge_secret_refs(
    main: &Connection,
    tx: &Connection,
    policy: &ImportPolicy,
) -> Result<RefMergeOutcome, AppError> {
    // 先把远端候选读到内存（下面要清空暂存库）。
    let remote_rows = ref_rows(tx)?;
    tx.execute("DELETE FROM secret_refs", [])
        .map_err(|e| AppError::Database(format!("清空暂存库引用失败: {e}")))?;

    // 本机行一律保留。
    let mut local_keys: Vec<(String, String)> = Vec::new();
    for row in ref_rows(main)? {
        local_keys.push((row.app.clone(), row.provider_id.clone()));
        insert_ref(tx, &row)?;
    }

    // 采纳远端引用：本机 1P 模式 + vault 匹配 + 本机没有同键行 + 供应商存在。
    let mut adopted_refs = 0usize;
    if policy.local_backend == BackendKind::OnePassword {
        if let Some(local_vault) = policy.local_vault.as_deref().filter(|v| !v.is_empty()) {
            for row in &remote_rows {
                let key = (row.app.clone(), row.provider_id.clone());
                if local_keys.contains(&key) || row.vault_id != local_vault {
                    continue;
                }
                if !provider_exists(tx, &row.app, &row.provider_id)? {
                    continue;
                }
                insert_ref(tx, row)?;
                adopted_refs += 1;
            }
        }
    }

    // 清理「供应商已不存在」的引用行（本机与远端都清）。
    let pruned = tx
        .execute(
            "DELETE FROM secret_refs
             WHERE NOT EXISTS (SELECT 1 FROM providers p
                               WHERE p.app_type = secret_refs.app
                                 AND p.id = secret_refs.provider_id)",
            [],
        )
        .map_err(|e| AppError::Database(format!("清理失效引用行失败: {e}")))?;

    let unlinked = collect_unlinked_providers(tx, policy)?;
    Ok((pruned, adopted_refs, unlinked))
}

fn ref_rows(conn: &Connection) -> Result<Vec<RefRow>, AppError> {
    let mut stmt = conn
        .prepare("SELECT app, provider_id, vault_id, item_id, fields FROM secret_refs")
        .map_err(|e| AppError::Database(format!("读取引用行失败: {e}")))?;
    let rows = stmt.query_map([], |row| {
        Ok(RefRow {
            app: row.get(0)?,
            provider_id: row.get(1)?,
            vault_id: row.get(2)?,
            item_id: row.get(3)?,
            fields: row.get(4)?,
        })
    });
    let rows = match rows {
        Ok(rows) => rows,
        Err(e) => return Err(AppError::Database(format!("读取引用行失败: {e}"))),
    };
    Ok(rows.flatten().collect())
}

fn insert_ref(tx: &Connection, row: &RefRow) -> Result<(), AppError> {
    tx.execute(
        "INSERT OR REPLACE INTO secret_refs (app, provider_id, vault_id, item_id, fields, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            row.app,
            row.provider_id,
            row.vault_id,
            row.item_id,
            row.fields,
            chrono::Utc::now().timestamp()
        ],
    )
    .map_err(|e| AppError::Database(format!("写入引用行失败: {e}")))?;
    Ok(())
}

fn provider_exists(conn: &Connection, app: &str, provider_id: &str) -> Result<bool, AppError> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM providers WHERE app_type = ?1 AND id = ?2",
            rusqlite::params![app, provider_id],
            |row| row.get(0),
        )
        .ok();
    Ok(exists.is_some())
}

/// S4-3：列出本机「未关联 1Password」的供应商，`<app>/<id>` 形式。
///
/// 供 `run_post_import_sync` 统一重建 `settings.onepassword_unlinked`——三条导入
/// 路径（同步下载 / SQL 导入 / `.db` 恢复）都走那一个入口，所以这里也只需一处。
/// 凭据管理器模式下恒为空：该模式没有「引用」概念，供应商缺钥匙就是真缺。
pub(crate) fn list_unlinked_providers(
    db: &Database,
    local_backend: BackendKind,
) -> Result<Vec<String>, AppError> {
    let conn = lock_conn!(db.conn);
    let policy = ImportPolicy {
        local_backend,
        local_vault: None,
    };
    Ok(collect_unlinked_providers(&conn, &policy)?
        .into_iter()
        .map(|(app, id)| format!("{app}/{id}"))
        .collect())
}

/// S4-3：导入后仍无引用、且属于 1P 模式需要钥匙的供应商清单。
///
/// 官方供应商除外（它们本来就没有钥匙），避免横幅把官方供应商报成「未关联」。
fn collect_unlinked_providers(
    conn: &Connection,
    policy: &ImportPolicy,
) -> Result<Vec<(String, String)>, AppError> {
    if policy.local_backend != BackendKind::OnePassword {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(
            "SELECT app_type, id FROM providers
             WHERE NOT EXISTS (SELECT 1 FROM secret_refs r
                               WHERE r.app = providers.app_type AND r.provider_id = providers.id)
             ORDER BY app_type, id",
        )
        .map_err(|e| AppError::Database(format!("统计未关联供应商失败: {e}")))?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    });
    let rows = match rows {
        Ok(rows) => rows,
        Err(e) => return Err(AppError::Database(format!("统计未关联供应商失败: {e}"))),
    };
    Ok(rows
        .flatten()
        .filter(|(_, id)| !crate::database::is_official_seed_id(id))
        .collect())
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
