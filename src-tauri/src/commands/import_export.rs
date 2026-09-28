#![allow(non_snake_case)]

use serde_json::{json, Value};
use std::path::PathBuf;
use tauri::State;
use tauri_plugin_dialog::DialogExt;
use zeroize::Zeroizing;

use crate::commands::sync_support::{
    combine_post_sync_results, post_sync_warning_from_result, run_post_import_sync,
    snapshot_pi_providers, success_payload_with_warning,
};
use crate::database::backup::BackupEntry;
use crate::database::Database;
use crate::error::AppError;
use crate::services::provider::ProviderService;
use crate::services::skill::skill_state_write_guard;
use crate::services::sync_protocol::{self, sync_mutex};
use crate::store::AppState;

/// 导出凭据便携包（加密）。
///
/// 对话框在 Rust 侧弹、路径不经前端往返（同 SQL 导出的约定）。取消返回 `Ok(None)`。
/// 口令只在本调用内使用，不落任何配置。
#[tauri::command]
pub async fn secrets_export_via_dialog<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    passphrase: String,
    state: State<'_, AppState>,
) -> Result<Option<Value>, String> {
    // F1-5（D9）：1P 模式下凭据在 1Password 里，凭据管理器是空的迁移源——
    // 导出只会产出空包或误导包，由后端直接拒绝（不能只靠前端隐藏按钮）。
    crate::secrets::portable::ensure_portable_export_allowed().map_err(|e| e.to_string())?;

    // 口令永不落盘：立即 move 进 Zeroizing，函数返回时自动抹除内存。
    let passphrase = Zeroizing::new(passphrase);
    let default_name = format!(
        "cc-switch-secrets-{}.json",
        chrono::Local::now().format("%Y%m%d")
    );
    let Some(target) = app
        .dialog()
        .file()
        .add_filter("JSON", &["json"])
        .set_file_name(&default_name)
        .blocking_save_file()
    else {
        return Ok(None);
    };

    let app_version = app.package_info().version.to_string();
    let (bytes, report) =
        crate::secrets::portable::export(state.secrets.as_ref(), passphrase.as_str(), &app_version)
            .await
            .map_err(|e| e.to_string())?;

    // 先写临时文件再原子替换：失败不会留下半个便携包（也避免明文密文混写）。
    let target_path = PathBuf::from(target.to_string());
    crate::config::atomic_write_private(&target_path, &bytes)
        .map_err(|e| format!("写入便携包失败: {e}"))?;

    Ok(Some(json!({
        "success": true,
        "filePath": target_path.to_string_lossy(),
        "exported": report.exported,
        "appSecrets": report.app_secrets,
    })))
}

/// 从便携包导入凭据（解密）。取消返回 `Ok(None)`。
///
/// 冲突策略：包里有的条目以包里为准（值不同则覆盖），本地独有条目保留不删。
#[tauri::command]
pub async fn secrets_import_via_dialog<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    passphrase: String,
    state: State<'_, AppState>,
) -> Result<Option<Value>, String> {
    // 口令永不落盘：立即 move 进 Zeroizing，函数返回时自动抹除内存。
    let passphrase = Zeroizing::new(passphrase);
    let Some(source) = app
        .dialog()
        .file()
        .add_filter("JSON", &["json"])
        .blocking_pick_file()
    else {
        return Ok(None);
    };

    let source_path = PathBuf::from(source.to_string());
    let bytes = std::fs::read(&source_path).map_err(|e| format!("读取便携包失败: {e}"))?;

    // F1-5（D9）：1P 模式导入改写 vault（分组 fetch+put + 登记 secret_refs），
    // 凭据管理器模式维持原路径。
    // F3-2：1P 分支是阻塞 op 调用，包进 `spawn_blocking`；凭据管理器分支是
    // async 本地 Win32 调用（无 op 往返），保持原样。
    let is_1p = crate::settings::is_onepassword_backend();
    let report = if is_1p {
        let app_state = state.inner().clone();
        tauri::async_runtime::spawn_blocking(move || {
            crate::secrets::portable::import_to_vault(&app_state, &bytes, passphrase.as_str())
        })
        .await
        .map_err(|e| format!("便携包导入任务执行失败: {e}"))?
        .map_err(|e| e.to_string())?
    } else {
        crate::secrets::portable::import(state.secrets.as_ref(), &bytes, passphrase.as_str())
            .await
            .map_err(|e| e.to_string())?
    };

    Ok(Some(json!({
        "success": true,
        "imported": report.imported,
        "overwritten": report.overwritten,
        "unchanged": report.unchanged,
        "appSecrets": report.app_secrets,
    })))
}

async fn run_with_database_restore_lock<T, Start, Fut>(start_operation: Start) -> T
where
    Start: FnOnce() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let _sync_guard = sync_mutex().lock().await;
    start_operation().await
}

/// S6-3（P1-8）：1P 模式下还有「明文导入待重试」的供应商时，拒绝手动 SQL 导入
/// 与 `.db` 恢复。
///
/// 为什么提前拦：导入/恢复会产生含明文行的安全备份，`assert_no_secret_patterns`
/// 扫描失败时报错晦涩，且备份本身已经写了一半。fail-fast 让用户先在横幅里重试
/// 导入（`retry_secrets_import_pending`）。凭据管理器模式明文落 DB 是合法状态，
/// 不拦。错误消息以 `import.plaintext_pending` 前缀作为错误码。
pub(crate) fn ensure_no_secrets_import_pending() -> Result<(), AppError> {
    if !crate::settings::is_onepassword_backend() {
        return Ok(());
    }
    let pending = crate::settings::get_secrets_import_pending();
    if pending.is_empty() {
        return Ok(());
    }
    Err(AppError::Message(format!(
        "import.plaintext_pending: 有 {} 个供应商的明文钥匙尚未导入 1Password，请先在横幅中重试导入",
        pending.len()
    )))
}

// ─── File import/export ──────────────────────────────────────

/// 选择 SQL 备份并导出（计划 4.2.1 S-2）。
///
/// 对话框在 Rust 侧弹、路径不经前端往返：命令本身不再接受任意绝对路径。
/// 取消返回 `Ok(None)`，与"导出失败"区分开。
#[tauri::command]
pub async fn export_config_via_dialog<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    #[allow(non_snake_case)] defaultName: String,
    state: State<'_, AppState>,
) -> Result<Option<Value>, String> {
    let Some(target) = app
        .dialog()
        .file()
        .add_filter("SQL", &["sql"])
        .set_file_name(&defaultName)
        .blocking_save_file()
    else {
        return Ok(None);
    };

    let target_path = PathBuf::from(target.to_string());
    let db = state.db.clone();
    let exported = tauri::async_runtime::spawn_blocking(move || {
        db.export_sql(&target_path)?;
        Ok::<_, AppError>(target_path)
    })
    .await
    .map_err(|e| format!("导出配置失败: {e}"))?
    .map_err(|e: AppError| e.to_string())?;

    Ok(Some(json!({
        "success": true,
        "message": "SQL exported successfully",
        "filePath": exported.to_string_lossy()
    })))
}

/// S6-2：预览后一次性路径令牌库——`path_token → 用户选中的路径`。
///
/// 路径不经前端往返（沿用 S-2 约定）：前端只拿到 token，实际路径留在后端内存。
/// 令牌一次性（确认导入即消费），10 分钟未使用自动过期；进程重启自然清空。
static IMPORT_PATH_TOKENS: std::sync::Mutex<
    Option<std::collections::HashMap<u64, (PathBuf, std::time::Instant)>>,
> = std::sync::Mutex::new(None);
const IMPORT_PATH_TOKEN_TTL: std::time::Duration = std::time::Duration::from_secs(600);

fn next_import_path_token() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    // 计数器以启动时刻作基址，避免同一进程内可预测的连续小整数。
    let base = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    base ^ COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn store_import_path(path: PathBuf) -> u64 {
    let token = next_import_path_token();
    let mut guard = IMPORT_PATH_TOKENS.lock().expect("导入路径令牌锁");
    let map = guard.get_or_insert_with(std::collections::HashMap::new);
    // 顺手清理过期令牌。
    map.retain(|_, (_, created)| created.elapsed() < IMPORT_PATH_TOKEN_TTL);
    map.insert(token, (path, std::time::Instant::now()));
    token
}

/// 取走令牌对应的路径（一次性：取出即删除）。过期或不存在返回 `None`。
fn take_import_path(token: u64) -> Option<PathBuf> {
    let mut guard = IMPORT_PATH_TOKENS.lock().expect("导入路径令牌库锁");
    let map = guard.as_mut()?;
    let (path, created) = map.remove(&token)?;
    if created.elapsed() >= IMPORT_PATH_TOKEN_TTL {
        return None;
    }
    Some(path)
}

/// S6-2：选择 SQL 文件并预览（P0-4 前端侧 / P2 路径不回传）。
///
/// 只读文件头：校验 CC Switch 导出前缀、解析 meta（S3-2，旧文件为 `null`），
/// 返回 `{ pathToken, meta, sizeBytes }`。实际导入由确认后的
/// [`import_config_confirmed`] 执行——弹确认框让用户看清楚来源与影响范围。
/// 取消返回 `Ok(None)`。
#[tauri::command]
pub async fn preview_sql_import_via_dialog<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<Option<Value>, String> {
    // S6-3（P1-8）：明文暂留未处理时 fail-fast，连文件选择对话框都不弹。
    ensure_no_secrets_import_pending().map_err(|e| e.to_string())?;

    let Some(source) = app
        .dialog()
        .file()
        .add_filter("SQL", &["sql"])
        .blocking_pick_file()
    else {
        return Ok(None);
    };
    let source_path = PathBuf::from(source.to_string());

    tauri::async_runtime::spawn_blocking(move || {
        let size_bytes = std::fs::metadata(&source_path)
            .map_err(|e| format!("读取文件信息失败: {e}"))?
            .len();
        let mut file =
            std::fs::File::open(&source_path).map_err(|e| format!("打开文件失败: {e}"))?;
        // 只读头部 8 KiB：meta 在第 2 行，足够；避免为预览读整个文件。
        let mut buffer = [0u8; 8192];
        let n = std::io::Read::read(&mut file, &mut buffer).unwrap_or(0);
        let head = String::from_utf8_lossy(&buffer[..n]);
        let head = head.trim_start_matches('\u{feff}');
        let meta =
            crate::database::backup::preview_sql_export_head(head).map_err(|e| e.to_string())?;

        let token = store_import_path(source_path);
        let meta_json = meta.map(|m| {
            json!({
                "purpose": m.purpose,
                "backend": m.backend,
                "endpoints": m.endpoints,
                "refs": m.refs,
                "device": m.device,
                "exportedAt": m.exported_at,
            })
        });
        Ok::<_, String>(Some(json!({
            "pathToken": token.to_string(),
            "meta": meta_json,
            "sizeBytes": size_bytes,
        })))
    })
    .await
    .map_err(|e| format!("导入预览任务失败: {e}"))?
}

/// S6-2：确认导入——消费预览返回的一次性 `pathToken`，执行真正的导入。
#[tauri::command]
pub async fn import_config_confirmed(
    state: State<'_, AppState>,
    #[allow(non_snake_case)] pathToken: String,
) -> Result<Option<Value>, String> {
    let token: u64 = pathToken.parse().map_err(|_| "导入确认令牌无效")?;
    let path = take_import_path(token)
        .ok_or_else(|| "导入确认已过期或已使用，请重新选择文件".to_string())?;

    import_config_from_path(state.inner().clone(), path.to_string_lossy().into_owned())
        .await
        .map(Some)
}

async fn import_config_from_path(
    app_state_for_sync: AppState,
    file_path: String,
) -> Result<Value, String> {
    let db = app_state_for_sync.db.clone();
    run_with_database_restore_lock(move || {
        tauri::async_runtime::spawn_blocking(move || {
            // S5-2（P1-4）：导入会改写 providers/settings 等表并触发后处理写入，
            // 这些都不是「用户改了配置」，包上全局抑制守卫防止回声触发自动上传。
            let _auto_sync_suppression = sync_protocol::AutoSyncSuppressionGuard::new();
            let path_buf = PathBuf::from(&file_path);
            let outcome = {
                // SQL restore replaces the `skills` table. Exclude local Skill
                // mutations while the database image is being swapped.
                let _skill_state_guard = skill_state_write_guard();
                // S4-7：导入前拍 Pi DB 快照，供后处理判断哪些供应商变了。
                let pi_before = snapshot_pi_providers(&app_state_for_sync)?;
                let outcome = db.import_sql_with_report(&path_buf)?;
                (outcome, pi_before)
            };
            let (outcome, pi_before) = outcome;
            let backup_id = outcome.backup_id;
            // S4-1：合并统计只记结构信息（计数），不含任何值（§9-13）。
            log::info!(
                "[Import] merged: adopted_refs={}, pruned_refs={}, pruned_endpoints={}, unlinked={}",
                outcome.report.adopted_refs,
                outcome.report.pruned_refs,
                outcome.report.pruned_endpoints,
                outcome.report.unlinked_providers.len()
            );
            let adopted_refs = outcome.report.adopted_refs;
            // S5-3（P1-5）：主库已被替换，scrub 失败不再让整个导入返回失败（会误导
            // 用户以为导入没成功），降级为 warning 并附上待导入明文数量；后处理
            // （live 刷新等）独立执行、不受 scrub 失败影响。
            let scrub = app_state_for_sync.scrub_imported_plaintext();
            let scrub_failed = scrub.is_err();
            let post = run_post_import_sync(&app_state_for_sync, Some(&pi_before));
            let mut warning = post_sync_warning_from_result(Ok(combine_post_sync_results(vec![
                scrub, post,
            ])));
            if let Some(message) = warning.as_mut() {
                if scrub_failed {
                    let pending = crate::settings::get_secrets_import_pending().len();
                    message.push_str(&format!(
                        "（{pending} 个供应商的明文钥匙待导入，可在横幅中重试）"
                    ));
                }
            }
            if let Some(msg) = warning.as_ref() {
                log::warn!("[Import] post-import sync warning: {msg}");
            }
            // S4-3：后处理刚全量重建过「未关联」清单，直接读回给前端。
            let unlinked_providers = crate::settings::get_onepassword_unlinked().len();
            Ok::<_, AppError>(success_payload_with_warning(
                backup_id,
                warning,
                adopted_refs,
                unlinked_providers,
            ))
        })
    })
    .await
    .map_err(|e| format!("导入配置失败: {e}"))?
    .map_err(|e: AppError| e.to_string())
}

#[tauri::command]
pub async fn sync_current_providers_live(state: State<'_, AppState>) -> Result<Value, String> {
    // F3-5（P1-7）：复用运行时 AppState。原先 `AppState::new` 会另起一个新状态——
    // vault 被重置成 LegacyWindowsVault（1P 模式下守卫直接报错）、切换互斥锁与
    // KEK 缓存也都脱离运行时实例。
    let app_state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        ProviderService::sync_current_to_live(&app_state)?;
        Ok::<_, AppError>(json!({
            "success": true,
            "message": "Live configuration synchronized"
        }))
    })
    .await
    .map_err(|e| format!("同步当前供应商失败: {e}"))?
    .map_err(|e: AppError| e.to_string())
}

// ─── File dialogs ────────────────────────────────────────────

/// 打开 ZIP 文件选择对话框
#[tauri::command]
pub async fn open_zip_file_dialog<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<Option<String>, String> {
    let dialog = app.dialog();
    let result = dialog
        .file()
        .add_filter("ZIP / Skill", &["zip", "skill"])
        .blocking_pick_file();

    Ok(result.map(|p| p.to_string()))
}

// ─── Database backup management ─────────────────────────────

/// Manually create a database backup
#[tauri::command]
pub async fn create_db_backup(state: State<'_, AppState>) -> Result<String, String> {
    let db = state.db.clone();
    tauri::async_runtime::spawn_blocking(move || match db.backup_database_file()? {
        Some(path) => Ok(path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default()),
        None => Err(AppError::Config(
            "Database file not found, backup skipped".to_string(),
        )),
    })
    .await
    .map_err(|e| format!("Backup failed: {e}"))?
    .map_err(|e: AppError| e.to_string())
}

/// List all database backup files
#[tauri::command]
pub fn list_db_backups() -> Result<Vec<BackupEntry>, String> {
    Database::list_backups().map_err(|e| e.to_string())
}

/// Restore database from a backup file
#[tauri::command]
pub async fn restore_db_backup(
    state: State<'_, AppState>,
    filename: String,
) -> Result<String, String> {
    // S6-3（P1-8）：同手动导入——恢复会在替换主库前生成安全备份，明文暂留
    // 未处理时提前拒绝。
    ensure_no_secrets_import_pending().map_err(|e| e.to_string())?;

    let app_state_for_sync = state.inner().clone();
    let db = app_state_for_sync.db.clone();
    run_with_database_restore_lock(move || {
        tauri::async_runtime::spawn_blocking(move || {
            // S5-2（P1-4）：同 SQL 导入——恢复 + 后处理的写入不触发自动上传。
            let _auto_sync_suppression = sync_protocol::AutoSyncSuppressionGuard::new();
            let restored = {
                let _skill_state_guard = skill_state_write_guard();
                // S4-7：导入前拍 Pi DB 快照，供后处理判断哪些供应商变了。
                let pi_before = snapshot_pi_providers(&app_state_for_sync)?;
                let restored = db.restore_from_backup(&filename)?;
                (restored, pi_before)
            };
            let (restored, pi_before) = restored;
            // S5-3（P1-5）：同手动导入——恢复成功后 scrub 失败只记 warning，
            // 不让整个恢复命令失败；后处理独立执行。
            let scrub = app_state_for_sync.scrub_imported_plaintext();
            let post = run_post_import_sync(&app_state_for_sync, Some(&pi_before));
            let warning =
                post_sync_warning_from_result(Ok(combine_post_sync_results(vec![scrub, post])));
            if let Some(message) = warning {
                // This legacy command returns only the restored filename, so keep
                // restore success and surface incomplete projection in the log.
                log::warn!("[Restore] post-import sync warning: {message}");
            }
            Ok::<_, AppError>(restored)
        })
    })
    .await
    .map_err(|e| format!("Restore failed: {e}"))?
    .map_err(|e: AppError| e.to_string())
}

/// Rename a database backup file
#[tauri::command]
pub fn rename_db_backup(
    #[allow(non_snake_case)] oldFilename: String,
    #[allow(non_snake_case)] newName: String,
) -> Result<String, String> {
    Database::rename_backup(&oldFilename, &newName).map_err(|e| e.to_string())
}

/// Delete a database backup file
#[tauri::command]
pub fn delete_db_backup(filename: String) -> Result<(), String> {
    Database::delete_backup(&filename).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::run_with_database_restore_lock;
    use crate::services::sync_protocol::sync_mutex;
    use serial_test::serial;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    // S6-3（P1-8）：明文暂留未处理时，手动导入 / `.db` 恢复的前置闸门必须拦截。
    #[test]
    #[serial]
    fn import_gate_blocks_when_secrets_import_pending_in_onepassword_mode() {
        let _home = crate::test_support::TestHomeGuard::new();
        let _onepassword = crate::test_support::OnePasswordBackendGuard::new();
        crate::settings::set_secrets_import_pending(vec!["claude/p1".to_string()])
            .expect("set pending");

        let err = super::ensure_no_secrets_import_pending().expect_err("必须拦截");
        let message = err.to_string();
        assert!(
            message.starts_with("import.plaintext_pending"),
            "错误码前缀缺失: {message}"
        );
        assert!(message.contains('1'), "错误消息应含待处理数量: {message}");

        // 重试成功（清单清空）后放行。
        crate::settings::set_secrets_import_pending(vec![]).expect("clear pending");
        super::ensure_no_secrets_import_pending().expect("清单清空后必须放行");
    }

    #[test]
    #[serial]
    fn import_gate_ignores_pending_list_outside_onepassword_mode() {
        let _home = crate::test_support::TestHomeGuard::new();
        // 显式切回非 1P 后端（不依赖前序测试的环境残留），再塞 pending 清单：
        // 凭据管理器模式下明文落 DB 是合法状态，不能拦。
        crate::settings::mutate_settings(|s| s.secret_backend = Some("windows".to_string()))
            .expect("set backend");
        crate::settings::set_secrets_import_pending(vec!["claude/p1".to_string()])
            .expect("set pending");

        super::ensure_no_secrets_import_pending().expect("凭据管理器模式必须放行");

        crate::settings::set_secrets_import_pending(vec![]).expect("cleanup pending");
    }

    // S6-2：预览头部校验——CC Switch 导出前缀 + meta 解析（旧文件 meta 为 None）。
    #[test]
    fn preview_head_validates_prefix_and_parses_meta() {
        let head = "-- CC Switch SQLite 导出\n-- cc-switch-meta: {\"purpose\":\"config\",\"backend\":\"onepassword\",\"endpoints\":false,\"refs\":3,\"device\":\"PC-1\",\"exported_at\":\"2026-09-28T00:00:00Z\"}\nPRAGMA foreign_keys=ON;\n";
        let meta = crate::database::backup::preview_sql_export_head(head)
            .expect("合法头部必须放行")
            .expect("有 meta 行");
        assert_eq!(meta.purpose, "config");
        assert_eq!(meta.backend, "onepassword");
        assert!(!meta.endpoints);
        assert_eq!(meta.refs, 3);

        let legacy = "-- CC Switch SQLite 导出\nPRAGMA foreign_keys=ON;\n";
        assert!(
            crate::database::backup::preview_sql_export_head(legacy)
                .expect("合法头部必须放行")
                .is_none(),
            "旧格式文件 meta 为 None"
        );

        let foreign = "PRAGMA foreign_keys=ON;\nDROP TABLE x;";
        assert!(
            crate::database::backup::preview_sql_export_head(foreign).is_err(),
            "非 CC Switch 导出必须拒绝"
        );
    }

    // S6-2：路径令牌一次性、可过期，且路径永不回传前端（token → 路径只在后端）。
    #[test]
    #[serial]
    fn import_path_token_is_one_time_and_expires() {
        let path = PathBuf::from("C:\\tmp\\never-returned.sql");
        let token = super::store_import_path(path.clone());

        let taken = super::take_import_path(token).expect("未过期应取到");
        assert_eq!(taken, path);
        assert!(
            super::take_import_path(token).is_none(),
            "令牌必须一次性：第二次取用为空"
        );

        // 过期：把创建时间拨回 TTL 之前。
        let token = super::store_import_path(path.clone());
        {
            let mut guard = super::IMPORT_PATH_TOKENS.lock().expect("令牌锁");
            let map = guard.as_mut().expect("令牌表");
            let entry = map.get_mut(&token).expect("条目");
            entry.1 = std::time::Instant::now() - super::IMPORT_PATH_TOKEN_TTL;
        }
        assert!(
            super::take_import_path(token).is_none(),
            "过期令牌必须被拒绝"
        );
    }

    #[tokio::test]
    async fn manual_restore_starts_blocking_work_after_global_lock_acquisition() {
        let guard = sync_mutex().lock().await;
        let entered = Arc::new(AtomicBool::new(false));
        let entered_in_task = Arc::clone(&entered);
        let restore = run_with_database_restore_lock(move || {
            tokio::task::spawn_blocking(move || {
                entered_in_task.store(true, Ordering::SeqCst);
            })
        });
        tokio::pin!(restore);

        assert!(
            tokio::time::timeout(Duration::from_millis(40), restore.as_mut())
                .await
                .is_err(),
            "restore must wait while another sync operation holds the global lock"
        );
        assert!(!entered.load(Ordering::SeqCst));

        drop(guard);
        tokio::time::timeout(Duration::from_secs(1), restore.as_mut())
            .await
            .expect("restore should start after lock release")
            .expect("blocking restore task should complete");
        assert!(entered.load(Ordering::SeqCst));
    }
}
