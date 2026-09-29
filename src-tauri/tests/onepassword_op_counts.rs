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

// ─── P4（安全方案 §7.3 / §9.3 / §7.5）：正确更新三字段与写入顺序 ───

use cc_switch_lib::{CredentialIntent, CredentialPatch};

/// §9.3「仅名称变化」行：SecretVault 层恰 1 次 patch（1P 实现内部 1 get +
/// 至多 1 edit，op 级预算由 onepassword.rs 的 FakeOpRunner 单测锁定）；
/// DB 显示名更新。
#[test]
fn onepassword_codex_rename_only_edit_is_single_patch() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    ensure_test_home();
    set_onepassword_backend();

    let (state, counting) = codex_state_with_configured_current();

    let updated = Provider::from_parts(
        "a".to_string(),
        "A2".to_string(),
        json!({
            "auth": {},
            "config": "model_provider = \"a\"\n\n[model_providers.a]\nname = \"A\"\nbase_url = \"https://a.example/v1\"\nwire_api = \"responses\"\n"
        }),
        None,
    );
    ProviderService::update(&state, AppType::Codex, Some("a"), updated).expect("rename update");

    assert_eq!(
        counting.patch_count(),
        1,
        "§9.3：仅名称变化恰 1 次 patch（1 get + ≤1 edit）"
    );
    assert_eq!(counting.fetch_count(), 0, "patch 不得额外 fetch");
    assert_eq!(counting.put_count(), 0, "patch 不得额外 put");

    let renamed = state
        .db
        .get_provider_by_id("a", "codex")
        .expect("read provider")
        .expect("provider exists");
    assert_eq!(renamed.name, "A2", "显示名必须更新");
}

/// §9.3「Base URL/API Key 真变化」行：恰 1 次 patch；patch 后 vault 里是
/// 新钥匙（服务层不再重复取整包比较，§6.1「业务 fetch + put 不是一次 op」）。
#[test]
fn onepassword_codex_key_change_uses_single_patch() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    ensure_test_home();
    set_onepassword_backend();

    let (state, counting) = codex_state_with_configured_current();

    let updated = Provider::from_parts(
        "a".to_string(),
        "A".to_string(),
        json!({
            "auth": { "OPENAI_API_KEY": "key-a-new" },
            "config": "model_provider = \"a\"\n\n[model_providers.a]\nname = \"A\"\nbase_url = \"https://a.example/v1\"\nwire_api = \"responses\"\n"
        }),
        None,
    );
    ProviderService::update(&state, AppType::Codex, Some("a"), updated).expect("key update");

    assert_eq!(counting.patch_count(), 1, "真变化恰 1 次 patch");
    assert_eq!(counting.fetch_count(), 0, "服务层不得重复 fetch 整包");
    assert_eq!(counting.put_count(), 0, "真变化不得走整包 put");

    // vault 真源已是新钥匙（1P 严格投递不写环境变量，读 vault 校验）。
    let secrets =
        ProviderService::fetch_provider_secrets(&state, &AppType::Codex, "a").expect("fetch back");
    assert_eq!(
        secrets.api_key.as_ref().map(|k| k.as_str()),
        Some("key-a-new"),
        "patch 后 vault 里必须是新钥匙"
    );
}

/// §9.3「显式提交与旧值相同的 key」行：允许 1 次 patch 判等（内部 1 get +
/// 0 edit），不得产生整包写。
#[test]
fn onepassword_codex_same_value_key_set_is_patch_without_write() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    ensure_test_home();
    set_onepassword_backend();

    let (state, counting) = codex_state_with_configured_current();

    let updated = Provider::from_parts(
        "a".to_string(),
        "A".to_string(),
        json!({
            "auth": { "OPENAI_API_KEY": "key-a" },
            "config": "model_provider = \"a\"\n\n[model_providers.a]\nname = \"A\"\nbase_url = \"https://a.example/v1\"\nwire_api = \"responses\"\n"
        }),
        None,
    );
    ProviderService::update(&state, AppType::Codex, Some("a"), updated).expect("same key update");

    assert_eq!(counting.patch_count(), 1, "同值 set 允许 1 次判等 patch");
    assert_eq!(counting.put_count(), 0, "同值 set 不得 edit/put");
    assert_eq!(counting.fetch_count(), 0);
}

/// §7.1-2 / §7.3-5：显式 clear 意图（credentialPatch）删除 vault 里的
/// api_key 字段，base_url 等其他字段保留。
#[test]
fn onepassword_codex_clear_key_intent_removes_only_api_key() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    ensure_test_home();
    set_onepassword_backend();

    let (state, counting) = codex_state_with_configured_current();

    let updated = Provider::from_parts(
        "a".to_string(),
        "A".to_string(),
        json!({
            "auth": {},
            "config": "model_provider = \"a\"\n\n[model_providers.a]\nname = \"A\"\nbase_url = \"https://a.example/v1\"\nwire_api = \"responses\"\n"
        }),
        None,
    );
    let patch = CredentialPatch {
        api_key: CredentialIntent::Clear,
        base_url: CredentialIntent::Keep,
    };
    ProviderService::update_with_credential_patch(
        &state,
        AppType::Codex,
        Some("a"),
        updated,
        Some(&patch),
    )
    .expect("clear update");

    assert_eq!(counting.patch_count(), 1, "显式 clear 恰 1 次 patch");
    let secrets =
        ProviderService::fetch_provider_secrets(&state, &AppType::Codex, "a").expect("fetch back");
    assert!(
        secrets.api_key.is_none(),
        "显式 clear 后 vault 里不得再有 api_key"
    );
    assert!(
        secrets.base_url.is_some(),
        "clear 只删目标字段，base_url 必须保留"
    );
}

/// §7.5-3/§7.5-4：vault 失败时不得写新端点缓存/引用，DB 行不动——
/// 缓存只允许在 vault 成功后提交。
struct ErrorVault;

impl SecretVault for ErrorVault {
    fn fetch(&self, _g: &SecretGroup) -> Result<Option<SecretBundle>, VaultError> {
        Err(VaultError::Locked)
    }
    fn put(&self, _g: &SecretGroup, _b: &SecretBundle) -> Result<VaultRef, VaultError> {
        Err(VaultError::Locked)
    }
    fn delete(&self, _g: &SecretGroup) -> Result<(), VaultError> {
        Err(VaultError::Locked)
    }
    fn patch(
        &self,
        _g: &SecretGroup,
        _p: &cc_switch_lib::secrets::VaultFieldPatch,
        _t: Option<&str>,
    ) -> Result<cc_switch_lib::secrets::VaultPatchOutcome, VaultError> {
        Err(VaultError::Locked)
    }
    fn status(&self) -> cc_switch_lib::secrets::VaultStatus {
        cc_switch_lib::secrets::VaultStatus::Ready
    }
    fn vault_id(&self) -> String {
        "err".to_string()
    }
    fn backend_name(&self) -> &'static str {
        "err"
    }
}

#[test]
fn onepassword_vault_failure_leaves_cache_refs_and_db_unchanged() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    ensure_test_home();
    set_onepassword_backend();

    let (mut state, _counting) = codex_state_with_configured_current();
    let old_endpoint = state
        .db
        .get_provider_endpoint("codex", "a")
        .expect("read endpoint")
        .expect("endpoint cached");
    let old_ref = state
        .db
        .get_secret_ref_fields("codex", "a")
        .expect("read refs")
        .expect("refs exist");
    let old_provider = state
        .db
        .get_provider_by_id("a", "codex")
        .expect("read provider")
        .expect("provider exists");

    state.vault = Arc::new(ErrorVault);
    let updated = Provider::from_parts(
        "a".to_string(),
        "A".to_string(),
        json!({
            "auth": { "OPENAI_API_KEY": "key-a-new" },
            "config": "model_provider = \"a\"\n\n[model_providers.a]\nname = \"A\"\nbase_url = \"https://a.example/v1\"\nwire_api = \"responses\"\n"
        }),
        None,
    );
    let result = ProviderService::update(&state, AppType::Codex, Some("a"), updated);
    assert!(result.is_err(), "vault 锁定必须让凭据更新失败");

    let endpoint_after = state
        .db
        .get_provider_endpoint("codex", "a")
        .expect("read endpoint");
    assert_eq!(
        endpoint_after.as_deref(),
        Some(old_endpoint.as_str()),
        "§7.5-3：vault 失败不得改端点缓存"
    );
    let ref_after = state
        .db
        .get_secret_ref_fields("codex", "a")
        .expect("read refs");
    assert_eq!(ref_after, Some(old_ref), "vault 失败不得改引用");
    let provider_after = state
        .db
        .get_provider_by_id("a", "codex")
        .expect("read provider")
        .expect("provider exists");
    assert_eq!(
        provider_after.settings_config, old_provider.settings_config,
        "vault 失败不得保存新 DB 行"
    );
}

// ─── P5（安全方案 §7.5 / §9.2-4/5/6/7）：结果分阶段、重试与并发 ───

type AppErrorForTest = cc_switch_lib::AppError;

/// P5 辅助：断言错误文本是 vault_* JSON-in-string 且 code 匹配（§7.5-4/5/6
/// 阶段化报告的机器可读契约，前端 parseVaultErrorText 按此解析）。
fn assert_phase_code(err: &AppErrorForTest, expected: &str) {
    let text = err.to_string();
    let parsed: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("阶段错误必须是 JSON-in-string，实际：{text}（{e}）"));
    assert_eq!(
        parsed["code"].as_str(),
        Some(expected),
        "阶段错误 code 不匹配，实际：{text}"
    );
    assert!(
        parsed["message"].as_str().is_some_and(|m| !m.is_empty()),
        "阶段错误必须带可读 message：{text}"
    );
}

/// §7.5-5：vault patch 成功后 `save_provider` 失败（SQLite 触发器注入）——
/// 必须返回阶段错误 `vault_saved_local_failed`；vault 已是新钥匙、DB 行与端点
/// 缓存保持旧值，原条目不归档、不清空钥匙。
#[test]
fn onepassword_update_db_failure_after_patch_reports_vault_saved_local_failed() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    ensure_test_home();
    set_onepassword_backend();

    let (state, counting) = codex_state_with_configured_current();
    let old_provider = state
        .db
        .get_provider_by_id("a", "codex")
        .expect("read provider")
        .expect("provider exists");
    let old_endpoint = state
        .db
        .get_provider_endpoint("codex", "a")
        .expect("read endpoint")
        .expect("endpoint cached");

    state
        .db
        .execute_batch_for_tests(
            "CREATE TRIGGER block_provider_update BEFORE UPDATE ON providers
             BEGIN SELECT RAISE(ABORT, 'injected update failure'); END;",
        )
        .expect("install trigger");

    let updated = Provider::from_parts(
        "a".to_string(),
        "A".to_string(),
        json!({
            "auth": { "OPENAI_API_KEY": "key-a-new" },
            "config": "model_provider = \"a\"\n\n[model_providers.a]\nname = \"A\"\nbase_url = \"https://a.example/v1\"\nwire_api = \"responses\"\n"
        }),
        None,
    );
    let err = ProviderService::update(&state, AppType::Codex, Some("a"), updated)
        .expect_err("DB 失败必须让 update 报错");
    assert_phase_code(&err, "vault_saved_local_failed");

    // vault 已提交新钥匙（不回滚、不归档）。
    let secrets =
        ProviderService::fetch_provider_secrets(&state, &AppType::Codex, "a").expect("fetch back");
    assert_eq!(
        secrets.api_key.as_ref().map(|k| k.as_str()),
        Some("key-a-new"),
        "§7.5-5：vault 已更新，保留新钥匙"
    );
    // DB 行保持旧值（save_provider 被拦）。
    let provider_after = state
        .db
        .get_provider_by_id("a", "codex")
        .expect("read provider")
        .expect("provider exists");
    assert_eq!(
        provider_after.settings_config, old_provider.settings_config,
        "§7.5-5：DB 保存失败时 DB 行不得变化"
    );
    // 端点缓存保持旧值。
    let endpoint_after = state
        .db
        .get_provider_endpoint("codex", "a")
        .expect("read endpoint");
    assert_eq!(
        endpoint_after.as_deref(),
        Some(old_endpoint.as_str()),
        "§7.5-5：端点缓存不得在 DB 失败时被改"
    );
    assert_eq!(counting.patch_count(), 1, "恰 1 次 patch");
    assert_eq!(counting.put_count(), 0, "不得整包 put");
}

/// §7.5-6：DB 已保存但 live 写入失败（live 文件只读注入，T-7 同款机制）——
/// 必须返回阶段错误 `vault_saved_pending_live`，DB 行已是新值；解除只读后用
/// **全 keep**（不携带 credentialPatch）重试即可恢复 live，且重试不得整包改
/// 1P（put = 0、delete = 0）。
#[cfg(windows)]
#[test]
fn onepassword_update_live_failure_reports_saved_pending_live_and_retry_is_projection_only() {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        SetFileAttributesW, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY,
    };

    fn set_readonly(path: &std::path::Path, ro: bool) {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);
        let attr = if ro {
            FILE_ATTRIBUTE_READONLY
        } else {
            FILE_ATTRIBUTE_NORMAL
        };
        // SAFETY: `wide` 是以 NUL 结尾的 UTF-16 路径，调用期间保持存活。
        let ok = unsafe { SetFileAttributesW(wide.as_ptr(), attr) };
        assert_ne!(ok, 0, "SetFileAttributesW 失败: {}", path.display());
    }

    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    ensure_test_home();
    set_onepassword_backend();

    let (state, counting) = codex_state_with_configured_current();
    let live_path = cc_switch_lib::get_codex_config_path();
    assert!(live_path.exists(), "初始 switch 应已写出 config.toml");

    // 提交：key 真变化 + 模型名变化（后者用于在 DB 配置文本里观察「已保存」）。
    let updated = Provider::from_parts(
        "a".to_string(),
        "A".to_string(),
        json!({
            "auth": { "OPENAI_API_KEY": "key-a-new" },
            "config": "model_provider = \"a\"\n\n[model_providers.a]\nname = \"A-p5\"\nbase_url = \"https://a.example/v1\"\nwire_api = \"responses\"\n"
        }),
        None,
    );

    set_readonly(&live_path, true);
    let err = ProviderService::update_with_credential_patch(
        &state,
        AppType::Codex,
        Some("a"),
        updated.clone(),
        None,
    )
    .expect_err("live 只读必须让 update 报错");
    assert_phase_code(&err, "vault_saved_pending_live");

    // DB 已保存新配置；vault 已是新钥匙。
    let saved = state
        .db
        .get_provider_by_id("a", "codex")
        .expect("read provider")
        .expect("provider exists");
    assert!(
        saved.settings_config.to_string().contains("A-p5"),
        "§7.5-6：DB 必须已保存新配置"
    );
    let secrets =
        ProviderService::fetch_provider_secrets(&state, &AppType::Codex, "a").expect("fetch back");
    assert_eq!(
        secrets.api_key.as_ref().map(|k| k.as_str()),
        Some("key-a-new"),
        "§7.5-6：vault 已是新钥匙"
    );
    let live_text = std::fs::read_to_string(&live_path).expect("read live");
    assert!(
        !live_text.contains("A-p5"),
        "live 写入失败时 live 不得含新模型名"
    );

    // 重试（全 keep，不携带 credentialPatch）：仅恢复投影，不得再改 1P。
    set_readonly(&live_path, false);
    counting.reset();
    ProviderService::update_with_credential_patch(&state, AppType::Codex, Some("a"), updated, None)
        .expect("retry update");
    assert_eq!(
        counting.put_count(),
        0,
        "§7.5-6：重试不得整包 put（不能重新改 1P）"
    );
    assert_eq!(counting.delete_count(), 0, "重试不得删除条目");
    let live_text = std::fs::read_to_string(&live_path).expect("read live after retry");
    assert!(
        live_text.contains("A-p5"),
        "重试后 live 必须投影已保存的新配置"
    );
}

/// §9.2-5（Pi）：vault patch 成功后 `save_provider` 失败——阶段错误
/// `vault_saved_local_failed`；vault 已是新钥匙；native live 节点按既有回滚
/// 行为恢复原样。
#[test]
fn onepassword_pi_update_db_failure_reports_vault_saved_local_failed() {
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
            "apiKey": "jun-key-old",
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
    ProviderService::update(&state, AppType::Pi, Some("jun"), seeded).expect("initial persist");
    counting.reset();

    let old_provider = state
        .db
        .get_provider_by_id("jun", "pi")
        .expect("read provider")
        .expect("provider exists");

    state
        .db
        .execute_batch_for_tests(
            "CREATE TRIGGER block_provider_update BEFORE UPDATE ON providers
             BEGIN SELECT RAISE(ABORT, 'injected update failure'); END;",
        )
        .expect("install trigger");

    let updated = Provider::from_parts(
        "jun".to_string(),
        "Jun".to_string(),
        json!({
            "name": "Jun",
            "api": "openai-responses",
            "baseUrl": "https://jun.example.com",
            "apiKey": "jun-key-new",
            "models": [{ "id": "m1" }]
        }),
        None,
    );
    let err = ProviderService::update(&state, AppType::Pi, Some("jun"), updated)
        .expect_err("DB 失败必须让 Pi update 报错");
    assert_phase_code(&err, "vault_saved_local_failed");

    let secrets =
        ProviderService::fetch_provider_secrets(&state, &AppType::Pi, "jun").expect("fetch back");
    assert_eq!(
        secrets.api_key.as_ref().map(|k| k.as_str()),
        Some("jun-key-new"),
        "§7.5-5：Pi vault 已更新，保留新钥匙"
    );
    let provider_after = state
        .db
        .get_provider_by_id("jun", "pi")
        .expect("read provider")
        .expect("provider exists");
    assert_eq!(
        provider_after.settings_config, old_provider.settings_config,
        "§7.5-5：Pi DB 行不得变化"
    );
    assert_eq!(counting.patch_count(), 1, "Pi 恰 1 次 patch");
    assert_eq!(counting.put_count(), 0, "Pi 不得整包 put");
}

/// §9.2-7：同 provider 两窗口并发编辑——app 锁串行化，最终 DB 与 vault 必须来自
/// 同一次提交（不存在「vault 是 key-w1、DB 配置是 w2」的交叉状态）。
#[test]
fn onepassword_concurrent_updates_serialize_on_app_lock() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    ensure_test_home();
    set_onepassword_backend();

    let (state, counting) = codex_state_with_configured_current();
    let state = Arc::new(state);

    fn submission(marker: &str, key: &str) -> Provider {
        Provider::from_parts(
            "a".to_string(),
            "A".to_string(),
            json!({
                "auth": { "OPENAI_API_KEY": key },
                "config": format!("model_provider = \"a\"\n\n[model_providers.a]\nname = \"{marker}\"\nbase_url = \"https://a.example/v1\"\nwire_api = \"responses\"\n")
            }),
            None,
        )
    }

    let s1 = state.clone();
    let t1 = std::thread::spawn(move || {
        ProviderService::update(&s1, AppType::Codex, Some("a"), submission("w1", "key-w1"))
    });
    let s2 = state.clone();
    let t2 = std::thread::spawn(move || {
        ProviderService::update(&s2, AppType::Codex, Some("a"), submission("w2", "key-w2"))
    });
    t1.join().expect("t1 join").expect("t1 update");
    t2.join().expect("t2 join").expect("t2 update");

    let secrets =
        ProviderService::fetch_provider_secrets(&state, &AppType::Codex, "a").expect("fetch back");
    let final_key = secrets
        .api_key
        .as_ref()
        .map(|k| k.as_str().to_string())
        .expect("vault 有钥匙");
    assert!(
        final_key == "key-w1" || final_key == "key-w2",
        "最终钥匙必须来自其中一次提交，实际 {final_key}"
    );
    let winner_marker = if final_key == "key-w1" { "w1" } else { "w2" };
    let final_provider = state
        .db
        .get_provider_by_id("a", "codex")
        .expect("read provider")
        .expect("provider exists");
    assert!(
        final_provider
            .settings_config
            .to_string()
            .contains(winner_marker),
        "DB 配置必须与 vault 钥匙来自同一次提交（{winner_marker}），实际：{}",
        final_provider.settings_config
    );
    assert_eq!(counting.patch_count(), 2, "两次并发编辑共 2 次 patch");
}
