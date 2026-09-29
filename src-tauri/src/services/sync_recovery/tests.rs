//! REL-A（§7.3）故障注入矩阵：在同步应用的每个边界强制结束进程，再以
//! 「重启」语义恢复，断言 DB 与 Skills 属于同一个确定状态。
//!
//! 机制：父测试把**同一个测试二进制**以 `--exact sync_recovery_fault_child`
//! 再拉起为子进程，通过环境变量下发阶段（crash / restart / verify）与故障
//! 注入点；子进程在边界上 `std::process::exit(9)` 模拟断电/强杀。全部使用
//! `CC_SWITCH_TEST_HOME` 隔离主目录，不触碰真实数据，无真实 op 调用。

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use super::{
    read_journal, recover_at_startup, resolve_before_new_operation, snapshot_identity,
    RecoveryStage, StartupRecovery, SyncRestoreOperation,
};
use crate::database::Database;
use crate::error::AppError;
use crate::services::skill::SkillService;
use crate::store::AppState;

const CHILD_TEST: &str = "services::sync_recovery::tests::sync_recovery_fault_child";
const FAULT_PHASE_ENV: &str = "CC_SWITCH_FAULT_PHASE";
const SCENARIO_ENV: &str = "CC_SWITCH_FAULT_SCENARIO";
const EXPECT_SKILLS_ENV: &str = "CC_SWITCH_FAULT_EXPECT_SKILLS";
const EXPECT_DB_ENV: &str = "CC_SWITCH_FAULT_EXPECT_DB";
const EXPECT_JOURNAL_ENV: &str = "CC_SWITCH_FAULT_EXPECT_JOURNAL";
const CRASH_EXIT_CODE: i32 = 9;

// ─── 父进程侧：编排 ──────────────────────────────────────────

fn make_test_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ccs-sync-recovery-{}-{}",
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create test home");
    dir
}

fn spawn_child_phase(home: &Path, phase: &str, fault: &str, scenario: &str) -> i32 {
    let exe = std::env::current_exe().expect("current test exe");
    let status = Command::new(exe)
        .args([CHILD_TEST, "--exact", "--nocapture"])
        .env("CC_SWITCH_TEST_HOME", home)
        .env(FAULT_PHASE_ENV, phase)
        .env(super::FAULT_EXIT_ENV, fault)
        .env(SCENARIO_ENV, scenario)
        .status()
        .expect("spawn fault child");
    status.code().unwrap_or(-1)
}

fn spawn_verify_child(home: &Path, phase: &str, skills: &str, db: &str, journal: &str) -> i32 {
    let exe = std::env::current_exe().expect("current test exe");
    let status = Command::new(exe)
        .args([CHILD_TEST, "--exact", "--nocapture"])
        .env("CC_SWITCH_TEST_HOME", home)
        .env(FAULT_PHASE_ENV, phase)
        .env(super::FAULT_EXIT_ENV, "")
        .env(EXPECT_SKILLS_ENV, skills)
        .env(EXPECT_DB_ENV, db)
        .env(EXPECT_JOURNAL_ENV, journal)
        .status()
        .expect("spawn verify child");
    status.code().unwrap_or(-1)
}

/// 子进程断言失败会以 101 退出；这里转成可读的父测试失败信息。
fn expect_child_exit(code: i32, expected: i32, context: &str) {
    assert_eq!(
        code, expected,
        "{context}: 子进程退出码 {code}，期望 {expected}（101 = 子进程断言失败，查上方输出）"
    );
}

fn crash_scenario(tag: &str, fault: &str, scenario: &str) -> PathBuf {
    let home = make_test_home(tag);
    let code = spawn_child_phase(&home, "crash", fault, scenario);
    expect_child_exit(
        code,
        CRASH_EXIT_CODE,
        &format!("crash@{fault} 应在边界退出"),
    );
    home
}

// ─── 故障注入矩阵（§7.3）─────────────────────────────────────

/// journal 写入前中断：现用数据未动，无 journal，无需恢复。
#[test]
fn crash_before_journal_leaves_state_unchanged() {
    let home = crash_scenario("before-journal", "before_journal_prepared", "with_skills");
    expect_child_exit(
        spawn_verify_child(&home, "verify", "old", "old", "absent"),
        0,
        "verify",
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// journal(prepared) 落盘后中断：重启丢弃 staging，现用数据未动。
#[test]
fn crash_after_journal_prepared_discards_staging() {
    let home = crash_scenario("after-journal", "after_journal_prepared", "with_skills");
    expect_child_exit(
        spawn_verify_child(&home, "verify", "old", "old", "absent"),
        0,
        "verify",
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// 意图（skills_replaced）写入后、替换执行前中断：回滚是幂等空转。
#[test]
fn crash_after_skills_intent_restores_old_skills() {
    let home = crash_scenario("after-intent", "after_skills_intent", "with_skills");
    expect_child_exit(
        spawn_verify_child(&home, "verify", "old", "old", "absent"),
        0,
        "verify",
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// Skills 已替换、DB 未导入时中断：重启回滚旧 Skills，DB 保持旧。
#[test]
fn crash_after_skills_replace_restores_old_skills() {
    let home = crash_scenario("after-skills", "after_skills_replace", "with_skills");
    expect_child_exit(
        spawn_verify_child(&home, "verify", "old", "old", "absent"),
        0,
        "verify",
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// DB 导入开始前中断：与 Skills 已替换同态，回滚后一致。
#[test]
fn crash_before_db_import_restores_old_skills() {
    let home = crash_scenario("before-db", "before_db_import", "with_skills");
    expect_child_exit(
        spawn_verify_child(&home, "verify", "old", "old", "absent"),
        0,
        "verify",
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// DB 已提交（marker 命中）后中断：不得回滚 Skills、不得重新导入；
/// 投影在重启补齐后清理 journal 与 marker。用户在恢复前的编辑必须保留。
#[test]
fn crash_after_db_commit_keeps_new_state_and_completes() {
    let home = crash_scenario("after-db", "after_db_commit", "with_skills");
    let code = spawn_verify_child(&home, "verify_no_reimport", "new", "edited", "absent");
    expect_child_exit(code, 0, "verify(no-reimport + complete)");
    let _ = std::fs::remove_dir_all(&home);
}

/// 投影进行中中断（projections_pending 已落盘）：再次重启幂等重试投影。
#[test]
fn crash_during_projections_retries_idempotently() {
    let home = crash_scenario("mid-projections", "after_db_commit", "with_skills");
    let code = spawn_child_phase(&home, "restart", "after_projections_pending", "with_skills");
    expect_child_exit(code, CRASH_EXIT_CODE, "restart@投影中段应再次退出");
    expect_child_exit(
        spawn_verify_child(&home, "verify", "new", "new", "absent"),
        0,
        "verify",
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// 原来不存在 Skills 目录：回滚目标是「无 Skills 内容」，而不是报错。
#[test]
fn crash_after_skills_replace_without_prior_skills_dir() {
    let home = crash_scenario("no-skills", "after_skills_replace", "no_skills");
    expect_child_exit(
        spawn_verify_child(&home, "verify", "none", "old", "absent"),
        0,
        "verify",
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// 备份与 journal 清单不一致（备份不完整）：拒绝回滚并停在 needs_attention，
/// 启动恢复与新操作都被暂停（§7.2-5 / §7.3）。
#[test]
#[serial_test::serial]
fn backup_incomplete_enters_needs_attention() -> Result<(), AppError> {
    let home = make_test_home("backup-incomplete");
    std::env::set_var("CC_SWITCH_TEST_HOME", &home);
    let db = Database::init().expect("init db");
    let op =
        SyncRestoreOperation::begin(&snapshot_identity(b"db", b"skills")).expect("begin operation");
    // 破坏备份：删掉备份目录，制造「备份不完整」。
    std::fs::remove_dir_all(&op.backup_dir).expect("corrupt backup");
    assert!(
        op.restore_skills_from_backup().is_err(),
        "备份不完整必须拒绝回滚"
    );
    op.mark_needs_attention();
    assert_eq!(
        read_journal()
            .expect("read journal")
            .expect("journal")
            .stage,
        RecoveryStage::NeedsAttention
    );
    assert_eq!(
        recover_at_startup(&db),
        StartupRecovery::NeedsAttention,
        "needs_attention 必须暂停自动恢复"
    );
    assert!(
        resolve_before_new_operation(&db).is_err(),
        "needs_attention 必须拒绝新同步操作"
    );
    std::env::remove_var("CC_SWITCH_TEST_HOME");
    let _ = std::fs::remove_dir_all(&home);
    Ok(())
}

/// 会话内旧 journal 归位规则（apply_snapshot 前置守卫，§7.2-10）：
/// prepared → 丢弃放行；db_committed → 拒绝要求重启；needs_attention → 拒绝。
#[test]
#[serial_test::serial]
fn unresolved_journal_blocks_or_discards_before_new_operation() -> Result<(), AppError> {
    let home = make_test_home("resolve-guard");
    std::env::set_var("CC_SWITCH_TEST_HOME", &home);
    let db = Database::init().expect("init db");

    // prepared：丢弃并放行。
    let _op = SyncRestoreOperation::begin(&snapshot_identity(b"db", b"skills"))
        .expect("begin prepared op");
    assert!(
        resolve_before_new_operation(&db).is_ok(),
        "prepared 应丢弃放行"
    );
    assert!(
        read_journal().expect("read").is_none(),
        "prepared 应清理 journal"
    );

    // db_committed（有 marker）：拒绝并要求重启（投影需完整 AppState）。
    let op = SyncRestoreOperation::begin(&snapshot_identity(b"db", b"skills"))
        .expect("begin db_committed op");
    op.mark_db_committed();
    (|| -> Result<(), AppError> {
        let conn = crate::database::lock_conn!(db.conn);
        conn.execute(
            "INSERT OR REPLACE INTO local_sync_commit (op_id, committed_at) VALUES (?1, 0)",
            [op.op_id.as_str()],
        )
        .map_err(|e| AppError::Database(format!("seed marker failed: {e}")))?;
        Ok(())
    })()
    .expect("seed marker");
    let err = resolve_before_new_operation(&db).expect_err("db_committed 必须拒绝新操作");
    assert!(
        err.to_string().contains("重启") || err.to_string().contains("restart"),
        "错误应引导重启，实际: {err}"
    );
    // needs_attention：同样拒绝。
    op.mark_needs_attention();
    assert!(
        resolve_before_new_operation(&db).is_err(),
        "needs_attention 必须拒绝新操作"
    );
    std::env::remove_var("CC_SWITCH_TEST_HOME");
    let _ = std::fs::remove_dir_all(&home);
    Ok(())
}

// ─── 子进程侧：故障注入执行体 ────────────────────────────────

/// 子进程入口。正常运行（无 CC_SWITCH_FAULT_PHASE）时是 no-op，
/// 父测试经 `--exact` 显式拉起并注入环境变量。
#[test]
fn sync_recovery_fault_child() {
    let Ok(phase) = std::env::var(FAULT_PHASE_ENV) else {
        return;
    };
    let home = PathBuf::from(std::env::var("CC_SWITCH_TEST_HOME").expect("test home env"));
    match phase.as_str() {
        "crash" => child_crash_phase(&home),
        "restart" => child_restart_phase(&home),
        "verify" | "verify_no_reimport" => child_verify_phase(&home, &phase),
        other => panic!("未知故障注入阶段: {other}"),
    }
}

/// 隔离主目录的最小骨架：sentinel 数据库文件保证 `get_app_config_dir`
/// 解析在测试主目录内（防御 Windows 的 HOME 回退）。
fn child_prepare_home(home: &Path) {
    let config_dir = home.join(".cc-switch");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    let sentinel = config_dir.join("cc-switch.db");
    if !sentinel.exists() {
        std::fs::File::create(&sentinel).expect("create db sentinel");
    }
}

fn build_app_state(db: Arc<Database>) -> AppState {
    let store: Arc<dyn crate::secrets::SecretStore> =
        Arc::new(crate::secrets::InMemorySecretStore::new());
    AppState::new(db, store)
}

fn insert_provider(db: &Database, id: &str, name: &str) -> Result<(), AppError> {
    let conn = crate::database::lock_conn!(db.conn);
    conn.execute(
        "INSERT OR IGNORE INTO providers (id, app_type, name, settings_config, meta)
         VALUES (?1, 'claude', ?2, '{}', '{}')",
        rusqlite::params![id, name],
    )
    .map_err(|e| AppError::Database(format!("insert provider failed: {e}")))?;
    Ok(())
}

fn provider_names(db: &Database) -> Result<Vec<String>, AppError> {
    let conn = crate::database::lock_conn!(db.conn);
    let mut stmt = conn
        .prepare("SELECT id FROM providers")
        .map_err(|e| AppError::Database(format!("prepare failed: {e}")))?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| AppError::Database(format!("query failed: {e}")))?
        .map(|r| r.expect("row"))
        .collect();
    Ok(names)
}

fn build_skills_zip(entries: &[(&str, &str)]) -> Vec<u8> {
    let buf = std::io::Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(buf);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, content) in entries {
        writer.start_file(*name, options).expect("zip start file");
        writer.write_all(content.as_bytes()).expect("zip write");
    }
    writer.finish().expect("zip finish").into_inner()
}

fn build_remote_snapshot() -> (Vec<u8>, Vec<u8>) {
    let remote_db = Database::memory().expect("memory db");
    insert_provider(&remote_db, "new-provider", "new").expect("seed remote provider");
    let db_sql = remote_db
        .export_sql_string_for_sync()
        .expect("export remote sql")
        .into_bytes();
    let skills_zip = build_skills_zip(&[("new-skill.txt", "new")]);
    (db_sql, skills_zip)
}

fn seed_old_state(db: &Database, scenario: &str) {
    insert_provider(db, "old-provider", "old").expect("seed old provider");
    if scenario == "with_skills" {
        let ssot = SkillService::get_ssot_dir().expect("ssot dir");
        std::fs::write(ssot.join("old-skill.txt"), "old").expect("seed old skill");
    }
}

fn child_open_db(home: &Path) -> Arc<Database> {
    child_prepare_home(home);
    Arc::new(Database::init().expect("open isolated db"))
}

fn child_crash_phase(home: &Path) {
    let scenario = std::env::var(SCENARIO_ENV).unwrap_or_else(|_| "with_skills".into());
    let db = child_open_db(home);
    seed_old_state(&db, &scenario);
    let (db_sql, skills_zip) = build_remote_snapshot();
    // 故障注入点在内部触发 std::process::exit(9)。
    let _ = crate::services::sync_protocol::apply_snapshot(&db, &db_sql, &skills_zip);
    // 未命中注入点却正常返回：边界配置有误，让父测试看到 0 以便暴露。
    log::error!("[FaultInject] 未在边界退出，apply_snapshot 正常完成");
}

fn child_restart_phase(home: &Path) {
    let db = child_open_db(home);
    let state = build_app_state(db);
    match recover_at_startup(&state.db) {
        StartupRecovery::None => {}
        StartupRecovery::PendingProjections => {
            crate::commands::sync_support::run_post_import_sync(&state, None)
                .expect("recovery projections");
        }
        StartupRecovery::NeedsAttention => panic!("restart 阶段不应进入 needs_attention"),
    }
}

fn child_verify_phase(home: &Path, phase: &str) {
    let db = child_open_db(home);

    // 「不得重新导入」：恢复前先编辑 DB，恢复后编辑必须保留（§7.3）。
    if phase == "verify_no_reimport" {
        insert_provider(&db, "new-provider", "new").expect("ensure provider");
        (|| -> Result<(), AppError> {
            let conn = crate::database::lock_conn!(db.conn);
            conn.execute(
                "UPDATE providers SET name = 'edited-after-crash' WHERE id = 'new-provider'",
                [],
            )
            .map_err(|e| AppError::Database(format!("edit failed: {e}")))?;
            Ok(())
        })()
        .expect("edit after crash");
    }

    // 执行一次启动恢复：有待恢复事务则补投影，之后重复恢复必须幂等。
    let state = build_app_state(db.clone());
    if recover_at_startup(&state.db) == StartupRecovery::PendingProjections {
        crate::commands::sync_support::run_post_import_sync(&state, None)
            .expect("recovery projections");
    }
    assert_eq!(
        recover_at_startup(&state.db),
        StartupRecovery::None,
        "二次恢复必须为 no-op"
    );

    // Skills 期望。
    let expect_skills = std::env::var(EXPECT_SKILLS_ENV).unwrap_or_default();
    let ssot = SkillService::get_ssot_dir().expect("ssot dir");
    let old_exists = ssot.join("old-skill.txt").exists();
    let new_exists = ssot.join("new-skill.txt").exists();
    match expect_skills.as_str() {
        "old" => assert!(old_exists && !new_exists, "Skills 应为旧内容: {ssot:?}"),
        "new" => assert!(new_exists && !old_exists, "Skills 应为新内容: {ssot:?}"),
        "none" => assert!(!old_exists && !new_exists, "Skills 应不存在: {ssot:?}"),
        other => panic!("未知 EXPECT_SKILLS: {other}"),
    }

    // DB 期望。
    let expect_db = std::env::var(EXPECT_DB_ENV).unwrap_or_default();
    let names = provider_names(&db).expect("list providers");
    match expect_db.as_str() {
        "old" => {
            assert!(
                names.contains(&"old-provider".into()),
                "DB 应为旧: {names:?}"
            );
            assert!(
                !names.contains(&"new-provider".into()),
                "DB 不得含新快照: {names:?}"
            );
        }
        "new" => assert!(
            names.contains(&"new-provider".into()),
            "DB 应为新: {names:?}"
        ),
        "edited" => {
            let name: String = (|| -> Result<String, AppError> {
                let conn = crate::database::lock_conn!(db.conn);
                conn.query_row(
                    "SELECT name FROM providers WHERE id = 'new-provider'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| AppError::Database(format!("query failed: {e}")))
            })()
            .expect("query edited provider");
            assert_eq!(name, "edited-after-crash", "恢复不得重新导入覆盖编辑");
        }
        other => panic!("未知 EXPECT_DB: {other}"),
    }

    // journal 期望：恢复完成后 journal 与 marker 都必须清干净。
    if std::env::var(EXPECT_JOURNAL_ENV).as_deref() == Ok("absent") {
        assert!(
            read_journal().expect("read journal").is_none(),
            "journal 应已清理"
        );
        let count: i64 = (|| -> Result<i64, AppError> {
            let conn = crate::database::lock_conn!(db.conn);
            conn.query_row("SELECT COUNT(*) FROM local_sync_commit", [], |row| {
                row.get(0)
            })
            .map_err(|e| AppError::Database(format!("count failed: {e}")))
        })()
        .expect("count markers");
        assert_eq!(count, 0, "marker 应已清理");
    }
}
