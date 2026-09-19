use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use cc_switch_lib::{update_settings, AppSettings, AppState, Database, MultiAppConfig};

/// 为测试设置隔离的 HOME 目录，避免污染真实用户数据。
pub fn ensure_test_home() -> &'static Path {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let base = std::env::temp_dir().join("cc-switch-test-home");
        if base.exists() {
            let _ = std::fs::remove_dir_all(&base);
        }
        std::fs::create_dir_all(&base).expect("create test home");
        // Windows 上 `dirs::home_dir()` 不受 HOME/USERPROFILE 影响（走 Known Folder API），
        // 用 CC_SWITCH_TEST_HOME 显式覆盖，以确保测试不会污染真实用户目录。
        std::env::set_var("CC_SWITCH_TEST_HOME", &base);
        std::env::set_var("HOME", &base);
        #[cfg(windows)]
        std::env::set_var("USERPROFILE", &base);
        // Claude Desktop 的配置目录在 Windows 上只读 LOCALAPPDATA（见 claude_desktop_config.rs
        // 的 windows_local_app_data_dir），既不认 CC_SWITCH_TEST_HOME 也不认 HOME。不覆盖它，
        // 涉及 Claude Desktop 供应商切换的测试会写进开发者真实的桌面版配置。
        #[cfg(windows)]
        std::env::set_var("LOCALAPPDATA", base.join("AppData").join("Local"));
        base
    })
    .as_path()
}

/// 清理测试目录中生成的配置文件与缓存。
pub fn reset_test_fs() {
    let home = ensure_test_home();
    for sub in [
        ".claude",
        ".codex",
        ".cc-switch",
        ".gemini",
        ".grok",
        ".config",
        ".openclaw",
        "profiles",
    ] {
        let path = home.join(sub);
        if path.exists() {
            if let Err(err) = std::fs::remove_dir_all(&path) {
                eprintln!("failed to clean {}: {}", path.display(), err);
            }
        }
    }
    let claude_json = home.join(".claude.json");
    if claude_json.exists() {
        let _ = std::fs::remove_file(&claude_json);
    }

    // 重置内存中的设置缓存，确保测试环境不受上一次调用影响
    let _ = update_settings(AppSettings::default());
}

/// 全局互斥锁，避免多测试并发写入相同的 HOME 目录。
pub fn test_mutex() -> &'static Mutex<()> {
    static MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
    MUTEX.get_or_init(|| Mutex::new(()))
}

/// 将 DB 中的明文凭据迁入内存 SecretStore（测试用：模拟启动时的自动迁移）。
/// 在测试往 DB 直接插入供应商后调用，让切换路径能从 store 读到密钥。
#[allow(dead_code)]
pub fn seed_secrets_from_db(state: &AppState) {
    let migrator =
        cc_switch_lib::secrets::CredentialMigrator::new(&state.db, state.secrets.as_ref());
    futures::executor::block_on(migrator.run_migration()).expect("seed secrets from db");
}

/// 创建测试用的 AppState，包含一个空的数据库
#[allow(dead_code)]
pub fn create_test_state() -> Result<AppState, Box<dyn std::error::Error>> {
    let db = Arc::new(Database::init()?);
    let secrets = Arc::new(cc_switch_lib::secrets::InMemorySecretStore::new());
    let state = AppState::new(db, secrets);
    seed_secrets_from_db(&state);
    Ok(state)
}

/// 创建测试用的 AppState，并从 MultiAppConfig 迁移数据
#[allow(dead_code)]
pub fn create_test_state_with_config(
    config: &MultiAppConfig,
) -> Result<AppState, Box<dyn std::error::Error>> {
    let db = Arc::new(Database::init()?);
    db.migrate_from_json(config)?;
    let secrets = Arc::new(cc_switch_lib::secrets::InMemorySecretStore::new());
    let state = AppState::new(db, secrets);
    seed_secrets_from_db(&state);
    Ok(state)
}

/// 换上一个可被测试观察的内存 `EnvSink`，并把同一实例交给断言方。
///
/// 为什么要显式换：`default_sink()` 在 `CC_SWITCH_TEST_HOME` 下虽然也返回内存实现，
/// 但**每次调用都新建一个对象**，同一轮投递里的 `check_conflict` 与 `set` 看到的是
/// 两个空仓库，§5.3.3 的冲突检测与所有权登记在集成层面等于不存在。现在 sink 挂在
/// `AppState` 上，整个 state 共用一个实例，测试才拿得到真实投递结果。
#[allow(dead_code)]
pub fn attach_test_env_sink(state: &mut AppState) -> cc_switch_lib::InMemoryEnvSink {
    let sink = cc_switch_lib::InMemoryEnvSink::default();
    state.env_sink = Arc::new(sink.clone());
    sink
}
