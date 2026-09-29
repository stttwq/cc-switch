//! REL-A（§7）：同步恢复——覆盖进程中断，而不只是函数返回 `Err`。
//!
//! 同步下载的应用流程（[`super::sync_protocol::apply_snapshot`]）是「备份
//! Skills → 替换 Skills → 导入 DB → 后置投影」，普通错误回滚覆盖不了进程
//! 中途退出。本模块提供跨重启的最小恢复协议：
//!
//! - **持久 Skills 备份**：操作开始时把当前 SSOT 复制到应用数据根
//!   `sync-recovery/<op_id>/skills-backup/`（受保护目录、随 journal 一起
//!   管理），替代原先会随作用域消失的 `TempDir` 备份。
//! - **journal**：`sync-recovery/journal.json`，记录阶段、operation ID、
//!   快照标识与备份清单（文件数/字节）；不含密钥或原始载荷。
//! - **commit marker**：本机 DB 表 `local_sync_commit`，在暂存库整库替换
//!   主库之前写入、随替换原子生效——这是「DB 已提交」的唯一权威事实，用来
//!   区分「仅 Skills 已换」（需回滚旧 Skills）与「DB 已提交」（只补投影）。
//!
//! 阶段语义是**意图式**的：`SkillsReplaced` 在真正替换 Skills **之前**写入，
//! 因此「行动与记录之间崩溃」永远落在可安全恢复的一侧（恢复动作幂等）。
//! `DbCommitted` / `ProjectionsPending` 是事后记录，即使没写成，marker 与
//! journal 的组合仍能推出正确恢复动作。
//!
//! 恢复只做有限状态推进（§7.2 REL-A2）：不新增任何 1P 操作、不批准 MCP、
//! 不重放旧快照；投影按阶段表以最新 DB 重试（§7.2-8）。无法证明归属时停在
//! `NeedsAttention`，不做任何自动破坏动作。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{atomic_write_private, get_app_config_dir};
use crate::database::Database;
use crate::error::AppError;
use crate::services::skill::SkillService;
use crate::services::sync_protocol::{localized, sha256_hex};

/// journal 格式版本（结构不兼容时拒绝自动恢复）。
const JOURNAL_VERSION: u32 = 1;
/// 应用数据根下的恢复目录名。
const RECOVERY_DIR_NAME: &str = "sync-recovery";
const JOURNAL_FILE: &str = "journal.json";

/// 测试故障注入点环境变量（§7.3 故障注入矩阵）。仅在设置该变量时生效，
/// 生产路径零开销。
pub(crate) const FAULT_EXIT_ENV: &str = "CC_SWITCH_FAULT_EXIT";

/// 恢复阶段。`Complete` 不落盘——完成即删除 journal。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecoveryStage {
    /// 持久备份已就绪、journal 已落盘，现用数据尚未改动。
    Prepared,
    /// 意图式：Skills 替换已提交或正在进行（marker 判定 DB 侧事实）。
    SkillsReplaced,
    /// DB 已导入成功（marker 为权威事实，本条只是加速判定）。
    DbCommitted,
    /// 后置投影未完成，重启后按最新 DB 幂等重试。
    ProjectionsPending,
    /// 无法安全自动恢复（备份不完整/状态冲突），停止自动操作等人工处理。
    NeedsAttention,
}

/// 跨重启的恢复 journal（私有写入，不含密钥）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RecoveryJournal {
    version: u32,
    op_id: String,
    /// 本次快照标识（两份 artifact 哈希的短组合，仅作关联用途）。
    snapshot_id: String,
    stage: RecoveryStage,
    /// Skills 备份目录，相对 `get_app_config_dir()`。
    skills_backup_dir: String,
    /// 操作开始时本机 Skills 目录是否存在（决定回滚是「还原」还是「清空」）。
    skills_existed: bool,
    /// 备份清单：文件数与总字节，恢复前校验备份完整性（§7.2-5）。
    skills_backup_files: u64,
    skills_backup_bytes: u64,
    created_at: String,
    updated_at: String,
}

/// 一次进行中的同步恢复操作。
pub(crate) struct SyncRestoreOperation {
    pub(crate) op_id: String,
    backup_dir: PathBuf,
    journal: std::cell::RefCell<RecoveryJournal>,
}

// ─── 基础设施 ────────────────────────────────────────────────

fn recovery_root() -> PathBuf {
    get_app_config_dir().join(RECOVERY_DIR_NAME)
}

fn journal_path() -> PathBuf {
    recovery_root().join(JOURNAL_FILE)
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn new_op_id() -> String {
    let nanos = chrono::Utc::now()
        .timestamp_nanos_opt()
        .unwrap_or_default()
        .max(0);
    format!("{:x}-{:x}", nanos, std::process::id())
}

/// 读取 journal。文件不存在返回 `None`；损坏（非合法 JSON / 版本不识别）
/// 返回 `Err`，调用方应停在 `NeedsAttention`，不做自动动作。
pub(crate) fn read_journal() -> Result<Option<RecoveryJournal>, AppError> {
    let path = journal_path();
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path).map_err(|e| AppError::io(&path, e))?;
    let journal: RecoveryJournal = serde_json::from_slice(&bytes).map_err(|e| {
        localized(
            "sync.recovery.journal_corrupt",
            format!("同步恢复 journal 损坏，已停止自动恢复: {e}"),
            format!("Sync recovery journal is corrupt; automatic recovery stopped: {e}"),
        )
    })?;
    if journal.version != JOURNAL_VERSION {
        return Err(localized(
            "sync.recovery.journal_version",
            format!(
                "同步恢复 journal 版本不识别（{}），已停止自动恢复",
                journal.version
            ),
            format!(
                "Unknown sync recovery journal version ({}); automatic recovery stopped",
                journal.version
            ),
        ));
    }
    Ok(Some(journal))
}

fn write_journal(journal: &RecoveryJournal) -> Result<(), AppError> {
    let bytes =
        serde_json::to_vec_pretty(journal).map_err(|e| AppError::JsonSerialize { source: e })?;
    atomic_write_private(&journal_path(), &bytes)
}

fn remove_journal_file() {
    if let Err(err) = fs::remove_file(journal_path()) {
        if err.kind() != std::io::ErrorKind::NotFound {
            log::warn!("[SyncRecovery] 删除 journal 失败: {err}");
        }
    }
}

fn remove_op_dir(op_id: &str) {
    let dir = recovery_root().join(op_id);
    if let Err(err) = fs::remove_dir_all(&dir) {
        if err.kind() != std::io::ErrorKind::NotFound {
            log::warn!("[SyncRecovery] 删除操作目录 {} 失败: {err}", dir.display());
        }
    }
}

/// 递归统计目录内文件数与总字节（canonical visited 集合防符号链接环路，
/// 与 `copy_dir_recursive` 的防环语义一致）。
fn measure_dir(dir: &Path) -> Result<(u64, u64), AppError> {
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut visited = std::collections::HashSet::new();
    walk(dir, &mut files, &mut bytes, &mut visited)?;
    Ok((files, bytes))
}

fn walk(
    dir: &Path,
    files: &mut u64,
    bytes: &mut u64,
    visited: &mut std::collections::HashSet<PathBuf>,
) -> Result<(), AppError> {
    let canonical = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    if !visited.insert(canonical) {
        return Ok(());
    }
    for entry in fs::read_dir(dir).map_err(|e| AppError::io(dir, e))? {
        let entry = entry.map_err(|e| AppError::io(dir, e))?;
        let path = entry.path();
        if path.is_dir() {
            walk(&path, files, bytes, visited)?;
        } else {
            let meta = fs::metadata(&path).map_err(|e| AppError::io(&path, e))?;
            *files += 1;
            *bytes += meta.len();
        }
    }
    Ok(())
}

/// 测试故障注入：环境变量 `CC_SWITCH_FAULT_EXIT` 等于 `point` 时立即退出
/// 进程，模拟该边界上的断电/强杀（§7.3）。未设置变量时是 no-op。
pub(crate) fn fault_exit(point: &str) {
    if std::env::var(FAULT_EXIT_ENV).as_deref() == Ok(point) {
        log::error!("[FaultInject] 在边界 {point} 强制退出进程");
        std::process::exit(9);
    }
}

// ─── 操作生命周期 ────────────────────────────────────────────

impl SyncRestoreOperation {
    /// 开始一次同步恢复操作：持久备份当前 Skills → 写 journal(Prepared)。
    ///
    /// 任何失败都发生在现用数据被改动之前，直接清理后返回 `Err`。
    pub(crate) fn begin(snapshot_id: &str) -> Result<Self, AppError> {
        let op_id = new_op_id();
        let op_dir = recovery_root().join(&op_id);
        let backup_dir = op_dir.join("skills-backup");

        fs::create_dir_all(&backup_dir).map_err(|e| AppError::io(&backup_dir, e))?;

        let ssot = SkillService::get_ssot_dir().map_err(|e| {
            localized(
                "sync.recovery.ssot_dir_failed",
                format!("获取 Skills SSOT 目录失败: {e}"),
                format!("Failed to resolve Skills SSOT directory: {e}"),
            )
        })?;
        let skills_existed = ssot.exists();
        let (files, bytes) = if skills_existed {
            super::webdav_sync::archive::copy_dir_recursive(&ssot, &backup_dir)?;
            measure_dir(&backup_dir)?
        } else {
            (0, 0)
        };

        fault_exit("before_journal_prepared");

        let op = Self {
            journal: std::cell::RefCell::new(RecoveryJournal {
                version: JOURNAL_VERSION,
                op_id: op_id.clone(),
                snapshot_id: snapshot_id.to_string(),
                stage: RecoveryStage::Prepared,
                skills_backup_dir: backup_dir
                    .strip_prefix(get_app_config_dir())
                    .unwrap_or(&backup_dir)
                    .to_string_lossy()
                    .to_string(),
                skills_existed,
                skills_backup_files: files,
                skills_backup_bytes: bytes,
                created_at: now_rfc3339(),
                updated_at: now_rfc3339(),
            }),
            op_id,
            backup_dir,
        };
        op.write_stage(RecoveryStage::Prepared)?;
        fault_exit("after_journal_prepared");
        Ok(op)
    }

    /// 从既有 journal 重建操作句柄（启动恢复 / 新操作前清理用）。
    fn from_journal(journal: RecoveryJournal) -> Self {
        let backup_dir = get_app_config_dir().join(&journal.skills_backup_dir);
        Self {
            op_id: journal.op_id.clone(),
            backup_dir,
            journal: std::cell::RefCell::new(journal),
        }
    }

    fn write_stage(&self, stage: RecoveryStage) -> Result<(), AppError> {
        let journal = {
            let mut journal = self.journal.borrow_mut();
            journal.stage = stage;
            journal.updated_at = now_rfc3339();
            journal.clone()
        };
        write_journal(&journal)
    }

    /// 意图式记录：在真正替换 Skills 之前写入 `SkillsReplaced`（§模块文档）。
    pub(crate) fn mark_skills_intent(&self) -> Result<(), AppError> {
        self.write_stage(RecoveryStage::SkillsReplaced)
    }

    /// DB 提交后的加速记录。失败不致命——marker 在 DB 里是权威事实。
    pub(crate) fn mark_db_committed(&self) {
        if let Err(err) = self.write_stage(RecoveryStage::DbCommitted) {
            log::warn!("[SyncRecovery] 记录 db_committed 阶段失败（marker 仍权威）: {err}");
        }
    }

    pub(crate) fn mark_needs_attention(&self) {
        if let Err(err) = self.write_stage(RecoveryStage::NeedsAttention) {
            log::error!("[SyncRecovery] 记录 needs_attention 阶段失败: {err}");
        }
        log::error!(
            "[SyncRecovery] 同步操作 {} 进入 needs_attention：已停止自动恢复，\
             备份保留在 {}，请人工处理后删除 journal",
            self.op_id,
            self.backup_dir.display()
        );
    }

    /// 校验并回滚 Skills 到操作前的持久备份。
    ///
    /// `skills_existed == false` 时目标状态是「目录不存在」。已存在内容一律
    /// 先删除再复制（同卷/跨卷都安全，不用 rename 假装原子）。
    pub(crate) fn restore_skills_from_backup(&self) -> Result<(), AppError> {
        let ssot = SkillService::get_ssot_dir().map_err(|e| {
            localized(
                "sync.recovery.ssot_dir_failed",
                format!("获取 Skills SSOT 目录失败: {e}"),
                format!("Failed to resolve Skills SSOT directory: {e}"),
            )
        })?;

        let journal = self.journal.borrow();
        if journal.skills_existed {
            let (files, bytes) = measure_dir(&self.backup_dir).map_err(|err| {
                localized(
                    "sync.recovery.backup_incomplete",
                    format!("恢复备份不完整或不可读: {err}"),
                    format!("Recovery backup is incomplete or unreadable: {err}"),
                )
            })?;
            if files != journal.skills_backup_files || bytes != journal.skills_backup_bytes {
                return Err(localized(
                    "sync.recovery.backup_incomplete",
                    format!(
                        "恢复备份与 journal 清单不一致（记录 {}/{} 字节，实际 {files}/{bytes} 字节）",
                        journal.skills_backup_files, journal.skills_backup_bytes
                    ),
                    format!(
                        "Recovery backup does not match journal manifest (recorded {}/{}, actual {files}/{})",
                        journal.skills_backup_files, journal.skills_backup_bytes, bytes
                    ),
                ));
            }

            if ssot.exists() {
                fs::remove_dir_all(&ssot).map_err(|e| AppError::io(&ssot, e))?;
            }
            super::webdav_sync::archive::copy_dir_recursive(&self.backup_dir, &ssot)?;
            let (restored_files, restored_bytes) = measure_dir(&ssot)?;
            if restored_files != files || restored_bytes != bytes {
                return Err(localized(
                    "sync.recovery.restore_mismatch",
                    "Skills 回滚后内容与备份不一致",
                    "Skills content does not match backup after rollback",
                ));
            }
        } else if ssot.exists() {
            fs::remove_dir_all(&ssot).map_err(|e| AppError::io(&ssot, e))?;
        }

        // restore_skills_zip 中断在「ssot→.bak 改名」之后会留下 .bak 残留，
        // 回滚完成后顺手清掉（该固定名只由同步替换流程使用）。
        let leftover_bak = ssot.with_extension("bak");
        if leftover_bak.exists() {
            let _ = fs::remove_dir_all(&leftover_bak);
        }
        Ok(())
    }

    /// 完成清理：删 journal、删操作目录、清 marker。全部尽力而为。
    pub(crate) fn cleanup(&self, db: &Database) {
        let _ = fs::remove_file(journal_path());
        remove_op_dir(&self.op_id);
        if let Err(err) = db.clear_sync_commit_marker(&self.op_id) {
            log::warn!("[SyncRecovery] 清除 commit marker 失败: {err}");
        }
    }
}

// ─── apply_snapshot 前置：旧 journal 的会话内处理 ─────────────

/// 新同步操作开始前的守卫：旧 journal 必须先归位。
///
/// - `Prepared`：现用数据未动 → 丢弃本次 staging，放行新操作；
/// - `SkillsReplaced` 且无 marker：DB 未提交 → 回滚旧 Skills 后放行；
/// - 其余状态：需要完整投影/人工判断（需 AppState），拒绝并要求重启，
///   由启动恢复流程处理（§7.2-10）。
pub(crate) fn resolve_before_new_operation(db: &Database) -> Result<(), AppError> {
    let Some(journal) = read_journal()? else {
        // 无 journal：marker 属残留，顺手清空（幂等）。
        let _ = db.clear_all_sync_commit_markers();
        return Ok(());
    };

    match journal.stage {
        RecoveryStage::Prepared => {
            log::info!(
                "[SyncRecovery] 上次操作 {} 停在 prepared，丢弃 staging 后继续",
                journal.op_id
            );
            remove_op_dir(&journal.op_id);
            remove_journal_file();
            let _ = db.clear_sync_commit_marker(&journal.op_id);
            Ok(())
        }
        RecoveryStage::SkillsReplaced => {
            if db.has_sync_commit_marker(&journal.op_id)? {
                return Err(pending_restart_error());
            }
            let op = SyncRestoreOperation::from_journal(journal);
            if let Err(err) = op.restore_skills_from_backup() {
                op.mark_needs_attention();
                return Err(err);
            }
            log::info!("[SyncRecovery] 已回滚上次未提交操作的 Skills，继续新操作");
            op.cleanup(db);
            Ok(())
        }
        RecoveryStage::DbCommitted | RecoveryStage::ProjectionsPending => {
            Err(pending_restart_error())
        }
        RecoveryStage::NeedsAttention => Err(localized(
            "sync.recovery.needs_attention",
            "存在待人工处理的同步恢复状态（详见日志），已暂停同步操作",
            "A sync recovery state requires manual attention (see logs); sync operations are paused",
        )),
    }
}

fn pending_restart_error() -> AppError {
    localized(
        "sync.recovery.pending_restart",
        "上次同步未完成恢复，请重启应用以继续（数据处于一致状态，无需手动修复）",
        "The previous sync needs recovery; restart the app to continue (data is consistent, no manual fix required)",
    )
}

// ─── 启动恢复与投影跟踪 ──────────────────────────────────────

/// 启动恢复结果。`PendingProjections` 由调用方（lib.rs）执行
/// `run_post_import_sync` 后经 [`note_projections_finished`] 收尾。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupRecovery {
    None,
    PendingProjections,
    NeedsAttention,
}

/// 启动恢复（§7.2-10）：必须在自动同步与任何会修改 live 的启动任务之前执行。
///
/// 只做非投影类恢复（丢弃 staging / 回滚旧 Skills）；DB 已提交时返回
/// `PendingProjections` 交由上层重试幂等投影。不触碰 1P、不批准 MCP。
pub(crate) fn recover_at_startup(db: &Database) -> StartupRecovery {
    let journal = match read_journal() {
        Ok(Some(journal)) => journal,
        Ok(None) => {
            // 无 journal：残留 marker 清理（幂等、尽力而为）。
            let _ = db.clear_all_sync_commit_markers();
            return StartupRecovery::None;
        }
        Err(err) => {
            log::error!("[SyncRecovery] journal 不可读，停止自动恢复等待人工处理: {err}");
            return StartupRecovery::NeedsAttention;
        }
    };

    match journal.stage {
        RecoveryStage::Prepared => {
            log::info!(
                "[SyncRecovery] 上次操作 {} 停在 prepared，现用数据未动，丢弃 staging",
                journal.op_id
            );
            remove_op_dir(&journal.op_id);
            remove_journal_file();
            let _ = db.clear_sync_commit_marker(&journal.op_id);
            StartupRecovery::None
        }
        RecoveryStage::SkillsReplaced => {
            let marker = match db.has_sync_commit_marker(&journal.op_id) {
                Ok(marker) => marker,
                Err(err) => {
                    log::error!("[SyncRecovery] 读取 commit marker 失败，停止自动恢复: {err}");
                    return StartupRecovery::NeedsAttention;
                }
            };
            if marker {
                // journal 可能停在 skills_replaced（db_committed 记录未写成），
                // marker 存在即 DB 已提交：不回滚 Skills，只补投影（§7.2-6）。
                log::info!(
                    "[SyncRecovery] 操作 {} 的 DB 已提交（marker 命中），转投影恢复",
                    journal.op_id
                );
                StartupRecovery::PendingProjections
            } else {
                let op = SyncRestoreOperation::from_journal(journal);
                match op.restore_skills_from_backup() {
                    Ok(()) => {
                        log::info!("[SyncRecovery] 操作 {} 未提交 DB，已回滚 Skills", op.op_id);
                        op.cleanup(db);
                        StartupRecovery::None
                    }
                    Err(err) => {
                        log::error!("[SyncRecovery] Skills 回滚失败: {err}");
                        op.mark_needs_attention();
                        StartupRecovery::NeedsAttention
                    }
                }
            }
        }
        RecoveryStage::DbCommitted | RecoveryStage::ProjectionsPending => {
            match db.has_sync_commit_marker(&journal.op_id) {
                Ok(true) => StartupRecovery::PendingProjections,
                Ok(false) => {
                    // stage 声称已提交但 marker 缺失：状态冲突，不自动动作。
                    log::error!(
                        "[SyncRecovery] 操作 {} 声称 DB 已提交但 marker 缺失，\
                         停止自动恢复等待人工处理",
                        journal.op_id
                    );
                    StartupRecovery::NeedsAttention
                }
                Err(err) => {
                    log::error!("[SyncRecovery] 读取 commit marker 失败: {err}");
                    StartupRecovery::NeedsAttention
                }
            }
        }
        RecoveryStage::NeedsAttention => {
            log::error!(
                "[SyncRecovery] 操作 {} 处于 needs_attention，保持暂停等待人工处理",
                journal.op_id
            );
            StartupRecovery::NeedsAttention
        }
    }
}

/// 后置投影开始时调用（`run_post_import_sync` 入口）：把 journal 推进到
/// `ProjectionsPending`。无 journal 或状态不匹配时是 no-op。
pub(crate) fn note_projections_started(db: &Database) {
    let Ok(Some(journal)) = read_journal() else {
        return;
    };
    if !matches!(
        journal.stage,
        RecoveryStage::SkillsReplaced
            | RecoveryStage::DbCommitted
            | RecoveryStage::ProjectionsPending
    ) {
        return;
    }
    match db.has_sync_commit_marker(&journal.op_id) {
        Ok(true) => {}
        Ok(false) => {
            log::error!(
                "[SyncRecovery] 投影开始但操作 {} 无 commit marker，标记 needs_attention",
                journal.op_id
            );
            SyncRestoreOperation::from_journal(journal).mark_needs_attention();
            return;
        }
        Err(err) => {
            log::warn!("[SyncRecovery] 投影开始时读取 marker 失败: {err}");
            return;
        }
    }
    let op = SyncRestoreOperation::from_journal(journal);
    if let Err(err) = op.write_stage(RecoveryStage::ProjectionsPending) {
        log::warn!("[SyncRecovery] 记录 projections_pending 阶段失败: {err}");
    }
    fault_exit("after_projections_pending");
}

/// 后置投影成功后调用：journal → 清理 → 完成（journal 删除即 Complete）。
pub(crate) fn note_projections_finished(db: &Database) {
    let Ok(Some(journal)) = read_journal() else {
        return;
    };
    if journal.stage != RecoveryStage::ProjectionsPending {
        return;
    }
    let op = SyncRestoreOperation::from_journal(journal);
    log::info!("[SyncRecovery] 操作 {} 恢复完成，清理恢复资源", op.op_id);
    op.cleanup(db);
}

/// 供 journal 构造测试与故障注入场景使用的快照标识。
pub(crate) fn snapshot_identity(db_sql: &[u8], skills_zip: &[u8]) -> String {
    format!(
        "{}-{}",
        &sha256_hex(db_sql)[..16],
        &sha256_hex(skills_zip)[..16]
    )
}

#[cfg(test)]
mod tests;
