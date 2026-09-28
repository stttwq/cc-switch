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
/// fetch = 0、put = 0。F1-8 落地，启用（P0-8 验收：整个启动路径 0 次 op）。
#[test]
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
    config.ensure_app(&AppType::Pi);
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

/// 构造 1P 模式的 Codex 当前供应商（钥匙与端点已落 vault/引用/端点缓存）。
/// 返回 (state, counting)。
///
/// 注意：`switch` 不会把目标供应商的秘密写进 vault/refs/端点缓存（strip_and_store
/// 只发生在切走回填），因此「已配置好的静止状态」需要两步引导：
/// ① 一次显式编辑（提交整份明文配置 = 显式凭据动作）把明文行迁进 vault，
///    同时写齐 secret_refs 与端点缓存、DB 行落 stripped；
/// ② 完整切换一次，把 live 与环境变量投好。
/// 之后清零计数，被测流程从「引用有效（§9.3 预算表前提）」的状态开始。
fn codex_state_with_configured_current() -> (AppState, Arc<CountingVault>) {
    let seeded = codex_seeded_provider();
    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Codex)
            .expect("codex manager");
        manager.current = "a".to_string();
        manager.providers.insert("a".to_string(), seeded.clone());
    }
    let (mut state, counting) = onepassword_test_state(&config);
    attach_test_env_sink(&mut state);
    // ① 显式凭据编辑：迁移进 vault（允许 1 fetch + 1 put）。
    ProviderService::update(&state, AppType::Codex, Some("a"), seeded).expect("initial persist");
    // ② 完整切换：写 live 与环境变量。
    ProviderService::switch(&state, AppType::Codex, "a").expect("switch to a");
    counting.reset();
    (state, counting)
}

/// 种子供应商：明文 key + TOML base_url（迁移前的原始形态）。
fn codex_seeded_provider() -> Provider {
    Provider::from_parts(
        "a".to_string(),
        "A".to_string(),
        json!({
            "auth": { "OPENAI_API_KEY": "key-a" },
            "config": "model_provider = \"a\"\n\n[model_providers.a]\nname = \"A\"\nbase_url = \"https://a.example/v1\"\nwire_api = \"responses\"\n"
        }),
        None,
    )
}

/// 安全方案 §9.1「普通配置」行：仅改模型（含回灌的同值 Base URL、API Key 输入框
/// 留空形态）的完整编辑链路必须精确 0 次 vault 往返。
///
/// 提交形态模拟修复后的前端：key 不回灌（keep）、Base URL 显示值被表单带回
/// （未编辑，与端点缓存同值）、仅模型字段变化。当前实现会先 fetch 旧整包再比较
/// （§6.1 表：merge_existing 只要包非空就 fetch）——该测试在 P3 落地前必须失败。
#[test]
fn onepassword_codex_model_only_edit_is_zero_op() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    ensure_test_home();
    set_onepassword_backend();

    let (state, counting) = codex_state_with_configured_current();

    // 用户在编辑框里只改了模型；Base URL 原样显示并被带回（真实前端经
    // setCodexBaseUrl 写回激活段，config 带顶层 model_provider 指针）；
    // API Key 留空（已配置不回显）。
    let updated = Provider::from_parts(
        "a".to_string(),
        "A".to_string(),
        json!({
            "auth": {},
            "config": "model_provider = \"a\"\n\n[model_providers.a]\nname = \"A\"\nbase_url = \"https://a.example/v1\"\nwire_api = \"responses\"\n\nmodel = \"gpt-5.2-turbo\"\n"
        }),
        None,
    );

    ProviderService::update(&state, AppType::Codex, Some("a"), updated).expect("update");

    assert_eq!(
        counting.fetch_count(),
        0,
        "SEC/§9.1：仅改模型的编辑不得 fetch（当前实现会取整包比较）"
    );
    assert_eq!(counting.put_count(), 0, "仅改模型的编辑不得 put");
    assert_eq!(counting.delete_count(), 0, "仅改模型的编辑不得 delete");
}

/// 安全方案 §9.1「完全无变化保存」行：打开编辑框 → 原样提交 → 关闭，
/// 全链路精确 0 次 vault 往返（连 status 探测也不得偷偷触发）。
#[test]
fn onepassword_codex_nochange_edit_is_zero_op() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    ensure_test_home();
    set_onepassword_backend();

    let (state, counting) = codex_state_with_configured_current();

    // 与初始提交完全一致的配置（Base URL 回灌同值、key 留空、模型不变）。
    let updated = Provider::from_parts(
        "a".to_string(),
        "A".to_string(),
        json!({
            "auth": {},
            "config": "model_provider = \"a\"\n\n[model_providers.a]\nname = \"A\"\nbase_url = \"https://a.example/v1\"\nwire_api = \"responses\"\n"
        }),
        None,
    );

    ProviderService::update(&state, AppType::Codex, Some("a"), updated).expect("update");

    assert_eq!(
        counting.fetch_count(),
        0,
        "§9.1：完全无变化的保存不得 fetch"
    );
    assert_eq!(counting.put_count(), 0, "完全无变化的保存不得 put");
    assert_eq!(counting.delete_count(), 0, "完全无变化的保存不得 delete");
}

// ─── P3（安全方案 §8 P3 / §9.1 / §9.2-9）：编辑零调用全链路与 NoVaultAccess ───

use cc_switch_lib::secrets::{SecretBundle, SecretGroup, VaultError, VaultRef, VaultStatus};

/// §9.2-9：任何方法被调用即 panic 的 vault——配置专用操作必须在不触碰它的
/// 前提下保存成功；「零调用」由 panic 本身强制，而不是靠计数断言。
struct PanicVault;

impl SecretVault for PanicVault {
    fn fetch(&self, _group: &SecretGroup) -> Result<Option<SecretBundle>, VaultError> {
        panic!("P3：配置编辑不得触碰 vault（fetch）");
    }
    fn put(&self, _g: &SecretGroup, _b: &SecretBundle) -> Result<VaultRef, VaultError> {
        panic!("P3：配置编辑不得触碰 vault（put）");
    }
    fn delete(&self, _group: &SecretGroup) -> Result<(), VaultError> {
        panic!("P3：配置编辑不得触碰 vault（delete）");
    }
    fn status(&self) -> VaultStatus {
        panic!("P3：配置编辑不得触碰 vault（status）");
    }
    fn vault_id(&self) -> String {
        "panic".to_string()
    }
    fn backend_name(&self) -> &'static str {
        "panic"
    }
}

/// §9.1「端点缓存：敏感 URL 不缓存」行：敏感（带凭据）base_url 永不落端点缓存，
/// 仅改模型的编辑不得为它 fallback fetch（§6.1 表：live.rs:885 隐式取钥）；
/// live 投影走 NoVaultAccess 的「安全保留既有投影」回退——从当前 live
/// config.toml 读回切换时已校验的 base_url，0 次 op。
#[test]
fn onepassword_codex_sensitive_url_model_edit_is_zero_op_and_keeps_live_base_url() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    ensure_test_home();
    set_onepassword_backend();

    let sensitive_url = "https://user:secret@a.example/v1";
    let seeded = Provider::from_parts(
        "a".to_string(),
        "A".to_string(),
        json!({
            "auth": { "OPENAI_API_KEY": "key-a" },
            "config": format!("model_provider = \"a\"\n\n[model_providers.a]\nname = \"A\"\nbase_url = \"{sensitive_url}\"\nwire_api = \"responses\"\n")
        }),
        None,
    );
    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Codex)
            .expect("codex manager");
        manager.current = "a".to_string();
        manager.providers.insert("a".to_string(), seeded.clone());
    }
    let (mut state, counting) = onepassword_test_state(&config);
    attach_test_env_sink(&mut state);
    // 引导：显式凭据编辑迁 vault（敏感 URL 进 vault/refs，不落缓存）→ 切换写 live
    //（此处允许 fetch：真正要用端点的显式动作）。
    ProviderService::update(&state, AppType::Codex, Some("a"), seeded).expect("initial persist");
    ProviderService::switch(&state, AppType::Codex, "a").expect("switch to a");
    let live_has_url = std::fs::read_to_string(cc_switch_lib::get_codex_config_path())
        .expect("read config.toml")
        .contains(sensitive_url);
    assert!(live_has_url, "引导后 live 必须已含敏感 base_url");
    counting.reset();

    // 模型编辑：敏感 URL 不在 secretStatus（列表 0 次 op 不取值），前端无从回灌，
    // 提交的 TOML 不含 base_url 行；key 留空（keep）。
    let updated = Provider::from_parts(
        "a".to_string(),
        "A".to_string(),
        json!({
            "auth": {},
            "config": "model_provider = \"a\"\n\n[model_providers.a]\nname = \"A\"\nwire_api = \"responses\"\n\nmodel = \"gpt-5.2-turbo\"\n"
        }),
        None,
    );
    ProviderService::update(&state, AppType::Codex, Some("a"), updated).expect("update");

    assert_eq!(
        counting.fetch_count(),
        0,
        "§9.1：敏感 URL 缓存 miss 时普通编辑不得 fallback fetch"
    );
    assert_eq!(counting.put_count(), 0, "普通编辑不得 put");
    // NoVaultAccess 回退生效：live 仍保留 base_url（不得删端点让 CLI 落回默认主机），
    // 且新模型已投影。
    let live = std::fs::read_to_string(cc_switch_lib::get_codex_config_path())
        .expect("read config.toml after edit");
    assert!(
        live.contains(sensitive_url),
        "配置编辑后 live 必须保留既有 base_url，实际：{live}"
    );
    assert!(
        live.contains("gpt-5.2-turbo"),
        "配置编辑后 live 必须投影新模型，实际：{live}"
    );
}

/// §9.2-9：配置专用操作在 vault 模拟为「任何调用即失败」时也必须保存成功——
/// 不能靠预先解锁才能通过测试。
#[test]
fn onepassword_config_only_edit_succeeds_without_touching_vault() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    ensure_test_home();
    set_onepassword_backend();

    let (mut state, _counting) = codex_state_with_configured_current();
    state.vault = Arc::new(PanicVault);

    let updated = Provider::from_parts(
        "a".to_string(),
        "A".to_string(),
        json!({
            "auth": {},
            "config": "model_provider = \"a\"\n\n[model_providers.a]\nname = \"A\"\nbase_url = \"https://a.example/v1\"\nwire_api = \"responses\"\n\nmodel = \"gpt-5.2-turbo\"\n"
        }),
        None,
    );
    ProviderService::update(&state, AppType::Codex, Some("a"), updated)
        .expect("vault 不可用时配置编辑必须成功");
}

/// §9.1「普通配置」行（Claude）：仅改模型（Base URL 同值回灌、key 留空）精确 0 op。
#[test]
fn onepassword_claude_model_only_edit_is_zero_op() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    ensure_test_home();
    set_onepassword_backend();

    let seeded = Provider::from_parts(
        "a".to_string(),
        "A".to_string(),
        json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": "sk-a",
                "ANTHROPIC_BASE_URL": "https://a.example"
            }
        }),
        None,
    );
    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Claude)
            .expect("claude manager");
        manager.current = "a".to_string();
        manager.providers.insert("a".to_string(), seeded.clone());
    }
    let (mut state, counting) = onepassword_test_state(&config);
    attach_test_env_sink(&mut state);
    ProviderService::update(&state, AppType::Claude, Some("a"), seeded).expect("initial persist");
    ProviderService::switch(&state, AppType::Claude, "a").expect("switch to a");
    counting.reset();

    // 仅改模型；Base URL 同值回灌（applyBaseUrlForApp 形态）；key 留空（keep）。
    let updated = Provider::from_parts(
        "a".to_string(),
        "A".to_string(),
        json!({
            "env": {
                "ANTHROPIC_BASE_URL": "https://a.example",
                "ANTHROPIC_MODEL": "claude-sonnet-5"
            }
        }),
        None,
    );
    ProviderService::update(&state, AppType::Claude, Some("a"), updated).expect("update");

    assert_eq!(counting.fetch_count(), 0, "Claude 仅改模型不得 fetch");
    assert_eq!(counting.put_count(), 0, "Claude 仅改模型不得 put");
    assert_eq!(counting.delete_count(), 0, "Claude 仅改模型不得 delete");
}

/// §9.1「普通配置」行（Pi）：仅改模型（baseUrl 同值回灌、apiKey 保持 $VAR 引用）
/// 精确 0 op；未启用（不在 models.json）也不得偷偷投递取钥。
#[test]
fn onepassword_pi_model_only_edit_is_zero_op() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    ensure_test_home();
    set_onepassword_backend();

    let seeded = Provider::from_parts(
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
    );
    let mut config = MultiAppConfig::default();
    config.ensure_app(&AppType::Pi);
    {
        let manager = config.get_manager_mut(&AppType::Pi).expect("pi manager");
        manager.providers.insert("jun".to_string(), seeded.clone());
    }
    let (mut state, counting) = onepassword_test_state(&config);
    attach_test_env_sink(&mut state);
    // 引导：一次显式编辑把顶层 baseUrl 迁进 vault/缓存（$VAR 引用不进 vault）。
    ProviderService::update(&state, AppType::Pi, Some("jun"), seeded).expect("initial persist");
    counting.reset();

    // 仅改模型列表；baseUrl 同值回灌；apiKey 保持 $VAR 引用。
    let updated = Provider::from_parts(
        "jun".to_string(),
        "Jun".to_string(),
        json!({
            "name": "Jun",
            "api": "openai-responses",
            "baseUrl": "https://jun.example.com",
            "apiKey": "$CC_SWITCH_PI_JUN_API_KEY",
            "models": [{ "id": "m1" }, { "id": "m2" }]
        }),
        None,
    );
    ProviderService::update(&state, AppType::Pi, Some("jun"), updated).expect("update");

    assert_eq!(counting.fetch_count(), 0, "Pi 仅改模型不得 fetch");
    assert_eq!(counting.put_count(), 0, "Pi 仅改模型不得 put");
    assert_eq!(counting.delete_count(), 0, "Pi 仅改模型不得 delete");
}
