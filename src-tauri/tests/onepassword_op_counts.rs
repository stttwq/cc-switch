//! F0-3 次数断言（施工方案 §4 F0-3 / §9.3）：锁死 1Password 模式下关键流程的
//! vault.fetch / put 次数——启动、列表、状态查询、切换一律 0 次 op。
//!
//! 这些断言按方案先写出来；部分在 F1–F3 落地前会失败，故标
//! `#[ignore = "F1 后启用"]`，对应修复提交时逐条去掉标记。不许删、不许放宽。

use cc_switch_lib::secrets::{CountingVault, InMemoryVault, SecretVault};
use cc_switch_lib::{AppState, AppType, Database, MultiAppConfig, Provider, ProviderService};
use serde_json::json;
use std::sync::Arc;

mod support;
use support::{attach_test_env_sink, ensure_test_home, reset_test_fs, test_mutex};

/// 1P 模式的测试 AppState：CountingVault 包 InMemoryVault（不碰真实 op）。
fn onepassword_test_state(config: &MultiAppConfig) -> (AppState, Arc<CountingVault>) {
    let db = Arc::new(Database::init().expect("init db"));
    db.migrate_from_json(config).expect("migrate config");
    let secrets = Arc::new(cc_switch_lib::secrets::InMemorySecretStore::new());
    let mut state = AppState::new(db, secrets);
    let inner: Arc<dyn SecretVault> = Arc::new(InMemoryVault::new());
    let counting = Arc::new(CountingVault::new(inner));
    state.vault = counting.clone();
    (state, counting)
}

/// 把当前后端切到 1Password（测试 home 里的 settings.json，不污染真实配置）。
fn set_onepassword_backend() {
    let mut settings = cc_switch_lib::get_settings_for_frontend();
    settings.secret_backend = Some("onepassword".to_string());
    cc_switch_lib::update_settings(settings).expect("set backend");
}

/// Codex 切换（含「切走回填」）：fetch = 0、put = 0。
///
/// F1-2 落地后 base_url 走端点表，切换 0 次 op。
#[test]
fn onepassword_codex_switch_is_zero_op() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let home = ensure_test_home();
    set_onepassword_backend();

    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Codex)
            .expect("codex manager");
        manager.current = "a".to_string();
        manager.providers.insert(
            "a".to_string(),
            Provider::from_parts(
                "a".to_string(),
                "A".to_string(),
                json!({
                    "auth": { "OPENAI_API_KEY": "key-a" },
                    "config": "[model_providers.a]\nname = \"A\"\nbase_url = \"https://a.example/v1\"\nwire_api = \"responses\"\n"
                }),
                None,
            ),
        );
        manager.providers.insert(
            "b".to_string(),
            Provider::from_parts(
                "b".to_string(),
                "B".to_string(),
                json!({
                    "auth": { "OPENAI_API_KEY": "key-b" },
                    "config": "[model_providers.b]\nname = \"B\"\nbase_url = \"https://b.example/v1\"\nwire_api = \"responses\"\n"
                }),
                None,
            ),
        );
    }

    let (mut state, counting) = onepassword_test_state(&config);
    attach_test_env_sink(&mut state);
    let _sink = state.env_sink.clone();

    // 首次切换落 live（这一步允许写 vault：A/B 的钥匙从表单进来是显式动作）。
    ProviderService::switch(&state, AppType::Codex, "a").expect("switch to a");
    ProviderService::switch(&state, AppType::Codex, "b").expect("switch to b");
    counting.reset();

    // 被测流程：b → a 的完整切换（含对 a 的切走回填）。
    ProviderService::switch(&state, AppType::Codex, "a").expect("switch back to a");

    // P0-1 验收：切换后的 config.toml 含 base_url（来自端点表 / 懒迁移）。
    let config_text =
        std::fs::read_to_string(cc_switch_lib::get_codex_config_path()).expect("read config.toml");
    assert!(
        config_text.contains("https://a.example/v1"),
        "config.toml 必须含 base_url，实际：{config_text}"
    );

    assert_eq!(
        counting.fetch_count(),
        0,
        "1P 模式切换 Codex 不得 fetch（base_url 走端点表）"
    );
    assert_eq!(counting.put_count(), 0, "1P 模式切换 Codex 不得 put");
    let _ = home;
}

/// 1P 模式启动路径（启动剥离 + 默认导入 + Pi 原生同步 + Pi 投递）：
/// fetch = 0、put = 0。F1-8 / F1-2 落地后启用。
#[test]
#[ignore = "F1-2/F1-8 后启用"]
fn onepassword_startup_path_is_zero_op() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let home = ensure_test_home();
    set_onepassword_backend();

    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Codex)
            .expect("codex manager");
        manager.current = "a".to_string();
        manager.providers.insert(
            "a".to_string(),
            Provider::from_parts(
                "a".to_string(),
                "A".to_string(),
                json!({
                    "auth": { "OPENAI_API_KEY": "key-a" },
                    "config": "[model_providers.a]\nname = \"A\"\nbase_url = \"https://a.example/v1\"\nwire_api = \"responses\"\n"
                }),
                None,
            ),
        );
    }
    {
        let manager = config.get_manager_mut(&AppType::Pi).expect("pi manager");
        manager.providers.insert(
            "jun".to_string(),
            Provider::from_parts(
                "jun".to_string(),
                "Jun".to_string(),
                json!({
                    "name": "Jun",
                    "api": "openai-responses",
                    "baseUrl": "https://jun.example.com",
                    "apiKey": "$CC_SWITCH_PI_JUN_API_KEY",
                    "models": [{ "id": "m1" }]
                }),
                None,
            ),
        );
    }

    let (mut state, counting) = onepassword_test_state(&config);
    attach_test_env_sink(&mut state);

    // 启动路径四件套（与 lib.rs 启动序列一致）。
    cc_switch_lib::strip_current_live_plaintext(&state).expect("启动剥离");
    for app_type in [AppType::Claude, AppType::Codex] {
        let _ = cc_switch_lib::import_default_config(&state, app_type.clone());
    }
    ProviderService::list(&state, AppType::Pi).expect("pi 原生同步");
    cc_switch_lib::reapply_pi_live(&state).expect("pi 投递");

    assert_eq!(
        counting.fetch_count(),
        0,
        "1P 模式启动路径不得 fetch（§6.7 启动不取钥匙）"
    );
    assert_eq!(counting.put_count(), 0, "1P 模式启动路径不得 put");
    let _ = home;
}

/// Pi 列表（models.json 含明文 key，1P 模式）：fetch = 0、put = 0。
/// 列表是启动/高频路径，任何 op 都会卡 UI（原则 5）。
#[test]
fn onepassword_pi_list_with_plaintext_is_zero_op() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let home = ensure_test_home();
    set_onepassword_backend();

    // models.json 直接手填明文 apiKey + baseUrl（模拟用户手工编辑）。
    let models_path = home.join(".pi").join("agent").join("models.json");
    std::fs::create_dir_all(models_path.parent().expect("parent")).expect("mkdir");
    std::fs::write(
        &models_path,
        r#"{
  "providers": {
    "jun": {
      "name": "Jun",
      "api": "openai-responses",
      "baseUrl": "https://jun.example.com",
      "apiKey": "plain-key-from-user",
      "models": [{ "id": "m1" }]
    }
  }
}"#,
    )
    .expect("seed models.json");

    let (state, counting) = onepassword_test_state(&MultiAppConfig::default());

    ProviderService::list(&state, AppType::Pi).expect("pi list");

    assert_eq!(counting.fetch_count(), 0, "Pi 列表不得 fetch");
    assert_eq!(counting.put_count(), 0, "Pi 列表不得 put");
}
