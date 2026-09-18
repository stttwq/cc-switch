//! §9 Phase 0 / Phase 4 验收：v18 明文夹具冷启动 → 无人工交互完成凭据迁移。
//!
//! 消费 Phase 0 夹具 `tests/fixtures/v18-home/`（由 `tests/fixture_v18.rs` 生成）：
//! 复制到独立的临时测试 home → 打开 DB（触发 v18→v19，置 `secrets_migration_pending=1`）
//! → 跑凭据迁移（用 `InMemorySecretStore`，不碰真实凭据管理器）→ 断言结果。
//!
//! 跑法：`cargo test --test migration_v18_cold_start`

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use cc_switch_lib::secrets::{
    load_known_targets, provider_target_prefix, CredentialMigrator, InMemorySecretStore,
    MigrationReport, SecretStore, SecretTarget,
};
use cc_switch_lib::{AppType, Database};

/// 夹具里的明文密钥字面量（与 tests/fixture_v18.rs 保持一致）。
const FIXTURE_PLAINTEXT: &[&str] = &[
    "sk-fixture-claude-0001",
    "sk-fixture-claude-0002",
    "sk-fixture-claude-0003",
    "sk-fixture-codex-official-0001",
    "sk-fixture-codex-3rd-0002",
    "sk-fixture-pi-0003",
    "sk-fixture-pi-0004",
    "sk-or-fixture-0007",
    "sk-fixture-pi-header-0008",
    "sk-fixture-usage-0005",
];

/// 夹具里的 7 个供应商（app, id）。
const FIXTURE_PROVIDERS: &[(&str, &str)] = &[
    ("claude", "fixture-claude-auth-token"),
    ("claude", "fixture-claude-api-key"),
    ("claude", "fixture-claude-extra-env"),
    ("codex", "fixture-codex-official"),
    ("codex", "fixture-codex-3rd"),
    ("pi", "fixture-pi-one"),
    ("pi", "fixture-pi-two"),
];

/// 本测试会改环境变量，必须独占运行。
fn home_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    match LOCK.get_or_init(|| Mutex::new(())).lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    futures::executor::block_on(fut)
}

fn fixture_home() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("v18-home")
}

fn copy_dir_all(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// 把 v18 夹具复制进一个全新的临时 home，并把 HOME 相关变量指过去。
/// Drop 时还原环境变量并删除临时目录——夹具本体始终保持只读，不会被迁移改写。
struct ColdStartHome {
    dir: PathBuf,
    prev: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl ColdStartHome {
    fn from_fixture() -> Self {
        let src = fixture_home();
        assert!(
            src.join(".cc-switch").join("cc-switch.db").exists(),
            "缺少夹具 {}，先跑 `cargo test --test fixture_v18 -- --ignored`",
            src.display()
        );

        let stamp = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("cc-switch-v18-cold-{}-{stamp}", std::process::id()));
        copy_dir_all(&src, &dir).expect("copy v18 fixture into temp home");

        let vars: Vec<&'static str> = vec!["CC_SWITCH_TEST_HOME", "HOME", "USERPROFILE"];
        let prev: Vec<(&'static str, Option<std::ffi::OsString>)> =
            vars.iter().map(|k| (*k, std::env::var_os(k))).collect();
        for key in &vars {
            std::env::set_var(key, &dir);
        }

        Self { dir, prev }
    }
}

impl Drop for ColdStartHome {
    fn drop(&mut self) {
        for (key, value) in self.prev.iter() {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// 把所有供应商行的 settings_config / meta 拼成文本，用于明文断言。
fn provider_row_text(db: &Database) -> String {
    let mut text = String::new();
    for app in ["claude", "codex", "pi"] {
        for (id, provider) in db.get_all_providers(app).expect("get_all_providers") {
            text.push_str(&format!("{app}/{id}\n"));
            text.push_str(&provider.settings_config.to_string());
            if let Some(meta) = &provider.meta {
                text.push_str(&serde_json::to_string(meta).expect("serialize meta"));
            }
            text.push('\n');
        }
    }
    text
}

#[test]
fn v18_cold_start_migrates_credentials_without_interaction() {
    let _guard = home_lock();
    let _home = ColdStartHome::from_fixture();
    let store = Arc::new(InMemorySecretStore::new());

    // 1. 冷启动打开 DB：v18→v19 只做纯 SQL，并置上迁移触发器
    let db = Database::init().expect("v18 夹具应能在当前版本打开并升到 v19");
    assert_eq!(
        db.get_setting("secrets_migration_pending")
            .unwrap()
            .as_deref(),
        Some("1"),
        "v18→v19 后 secrets_migration_pending 应为 1"
    );
    let before = provider_row_text(&db);
    // sk-fixture-usage-0005 在 meta.usage_script 里，v19 的纯 SQL 迁移已把它剥掉，
    // 其余字面量此时必须仍在 DB 行里。
    let absent_before: Vec<&str> = FIXTURE_PLAINTEXT
        .iter()
        .copied()
        .filter(|lit| !before.contains(lit))
        .collect();
    assert_eq!(
        absent_before,
        vec!["sk-fixture-usage-0005"],
        "迁移前 DB 行应仍含其余明文字面量"
    );

    // 2. 无人工交互完成迁移（§6.2：先全写凭据，再单事务改 DB）
    assert!(
        db.run_credential_migration_if_pending(store.as_ref())
            .expect("冷启动凭据迁移应成功"),
        "标志为 1 时应真的执行迁移"
    );

    // 3. providers 行不再含任何夹具明文
    let after = provider_row_text(&db);
    for lit in FIXTURE_PLAINTEXT {
        assert!(!after.contains(lit), "迁移后 DB 行仍含明文 {lit}");
    }

    // 4. store 里有对应 target（附录 B 命名），且值能读回
    let targets = block_on(store.list_targets("cc-switch/v1/")).expect("list_targets");
    assert!(!targets.is_empty(), "迁移应至少写入一批凭据 target");
    for (app, id) in FIXTURE_PROVIDERS {
        let app_type = AppType::from_str(app).expect("app type");
        let prefix = provider_target_prefix(&app_type, id);
        assert!(
            targets.iter().any(|t| t.starts_with(&prefix)),
            "供应商 {app}/{id} 应有凭据 target，实际: {targets:?}"
        );
    }
    // 抽查两个字段映射：Claude 的 ANTHROPIC_AUTH_TOKEN 与 Pi 的 apiKey
    let claude_key = block_on(store.get(&SecretTarget::provider_api_key(
        AppType::from_str("claude").unwrap(),
        "fixture-claude-auth-token",
    )))
    .expect("get claude api_key");
    assert_eq!(
        claude_key.as_deref().map(|s| s.as_str()),
        Some("sk-fixture-claude-0001")
    );
    let pi_key = block_on(store.get(&SecretTarget::provider_api_key(
        AppType::from_str("pi").unwrap(),
        "fixture-pi-one",
    )))
    .expect("get pi api_key");
    assert_eq!(
        pi_key.as_deref().map(|s| s.as_str()),
        Some("sk-fixture-pi-0003")
    );

    // 5. 标志位：pending 被清掉，live 重写待办被置起（§6.4 由启动流程接手）
    assert_ne!(
        db.get_setting("secrets_migration_pending")
            .unwrap()
            .as_deref(),
        Some("1"),
        "迁移完成后 secrets_migration_pending 不得仍为 1"
    );
    assert_eq!(
        db.get_setting("live_reapply_pending").unwrap().as_deref(),
        Some("1")
    );

    // 6. 幂等：反复“启动”不会重复迁移，也不会产生第二条迁移记录
    let report_raw = db
        .get_setting("secrets_migration_report")
        .unwrap()
        .expect("迁移后应落库 secrets_migration_report");
    let report: MigrationReport = serde_json::from_str(&report_raw).expect("parse report");
    let known_before = load_known_targets(&db).expect("known targets").len();
    assert_eq!(
        known_before,
        targets.len(),
        "known_secret_targets 应与写入的 target 数一致"
    );

    for _ in 0..2 {
        assert!(
            !db.run_credential_migration_if_pending(store.as_ref())
                .expect("重复启动不应报错"),
            "标志已清，重复启动不得再迁移"
        );
    }
    assert_eq!(
        db.get_setting("secrets_migration_report")
            .unwrap()
            .as_deref(),
        Some(report_raw.as_str()),
        "迁移报告应保持同一条记录"
    );

    // 7. 幂等（直接调 migrator）：DB 已剥离，再跑一次不应迁移任何东西
    let second = block_on(CredentialMigrator::new(&db, store.as_ref()).run_migration())
        .expect("二次迁移应成功");
    assert!(
        second.migrated_providers.is_empty(),
        "二次迁移不得重复登记: {:?}",
        second.migrated_providers
    );
    assert_eq!(known_before, load_known_targets(&db).unwrap().len());
    assert_eq!(
        report.migrated_providers.len(),
        db.get_setting("secrets_migration_report")
            .unwrap()
            .and_then(|raw| serde_json::from_str::<MigrationReport>(&raw).ok())
            .map(|r| r.migrated_providers.len())
            .unwrap_or_default(),
        "报告条数不受二次运行影响"
    );
}
