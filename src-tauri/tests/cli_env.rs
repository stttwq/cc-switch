//! 2.2 方案 P1：`ccs env` shim 核心逻辑集成测试。
//!
//! 全部走内存后端（`create_test_state_with_config` + `InMemorySecretStore`），
//! 调可注入 state 的 `env_command_core`，绕开真实凭据管理器与 DB 版本门禁。

use serde_json::json;
use zeroize::Zeroizing;

use cc_switch_lib::cli::{self, classify_exit, Shell, EXIT_MISSING_KEY, EXIT_USAGE};
use cc_switch_lib::secrets::SecretTarget;
use cc_switch_lib::{
    update_settings, AppSettings, AppType, EnvSink, ManagedEnvVars, MultiAppConfig, Provider,
};

#[path = "support.rs"]
mod support;
use support::{attach_test_env_sink, create_test_state_with_config, ensure_test_home, reset_test_fs, test_mutex};

fn default_settings() {
    update_settings(AppSettings::default()).expect("reset settings");
}

fn claude_config(current: &str, key: &str) -> MultiAppConfig {
    let mut config = MultiAppConfig::default();
    let manager = config
        .get_manager_mut(&AppType::Claude)
        .expect("claude manager");
    manager.current = current.to_string();
    manager.providers.insert(
        current.to_string(),
        Provider::from_parts(
            current.to_string(),
            "A".to_string(),
            json!({ "env": { "ANTHROPIC_AUTH_TOKEN": key } }),
            None,
        ),
    );
    config
}

#[test]
fn activate_claude_powershell_quotes_and_sets() {
    let _guard = test_mutex().lock().unwrap_or_else(|p| p.into_inner());
    reset_test_fs();
    let _home = ensure_test_home();
    default_settings();

    // 对抗值：含 ' " % & 与空格，验证 PowerShell 只把 ' 翻倍。
    let state = create_test_state_with_config(&claude_config("a", "A'B\"C%D&E"))
        .expect("create state");

    let out = cli::env_command_core(&state, "claude", None, Shell::PowerShell, false)
        .expect("activate should succeed");

    assert_eq!(out.exit_code, 0);
    assert_eq!(
        out.stdout,
        "$env:ANTHROPIC_AUTH_TOKEN='A''B\"C%D&E'\n",
        "stdout 只含可执行赋值语句"
    );
}

#[test]
fn activate_claude_bash_single_quotes() {
    let _guard = test_mutex().lock().unwrap_or_else(|p| p.into_inner());
    reset_test_fs();
    let _home = ensure_test_home();
    default_settings();

    let state =
        create_test_state_with_config(&claude_config("a", "A'B\"C%D&E")).expect("create state");
    let out = cli::env_command_core(&state, "claude", None, Shell::Bash, false).expect("activate");
    assert_eq!(
        out.stdout,
        "export ANTHROPIC_AUTH_TOKEN='A'\"'\"'B\"C%D&E'\n"
    );
}

#[test]
fn activate_claude_cmd_best_effort_plain_value() {
    let _guard = test_mutex().lock().unwrap_or_else(|p| p.into_inner());
    reset_test_fs();
    let _home = ensure_test_home();
    default_settings();

    let state = create_test_state_with_config(&claude_config("a", "plain123")).expect("state");
    let out = cli::env_command_core(&state, "claude", None, Shell::Cmd, false).expect("activate");
    assert_eq!(out.stdout, "set \"ANTHROPIC_AUTH_TOKEN=plain123\"\n");
}

#[test]
fn unsupported_app_maps_to_usage() {
    let _guard = test_mutex().lock().unwrap_or_else(|p| p.into_inner());
    reset_test_fs();
    let _home = ensure_test_home();
    default_settings();

    let state = create_test_state_with_config(&claude_config("a", "k")).expect("state");
    let err = cli::env_command_core(&state, "gemini", None, Shell::PowerShell, false)
        .expect_err("unsupported app must error");
    assert_eq!(classify_exit(&err), EXIT_USAGE);
}

#[test]
fn claude_missing_key_is_hard_error_code_3() {
    let _guard = test_mutex().lock().unwrap_or_else(|p| p.into_inner());
    reset_test_fs();
    let _home = ensure_test_home();
    default_settings();

    // 供应商存在但 SecretStore 里没有密钥（config 不带 env）→ provider_env_pairs 返 Err。
    let mut config = MultiAppConfig::default();
    let manager = config
        .get_manager_mut(&AppType::Claude)
        .expect("claude manager");
    manager.current = "a".to_string();
    manager.providers.insert(
        "a".to_string(),
        Provider::from_parts("a".to_string(), "A".to_string(), json!({}), None),
    );
    let state = create_test_state_with_config(&config).expect("state");

    let err = cli::env_command_core(&state, "claude", None, Shell::PowerShell, false)
        .expect_err("Claude 缺密钥是硬错误");
    assert_eq!(classify_exit(&err), EXIT_MISSING_KEY);
}

#[test]
fn codex_missing_key_warns_fails_closed_no_empty_export() {
    let _guard = test_mutex().lock().unwrap_or_else(|p| p.into_inner());
    reset_test_fs();
    let _home = ensure_test_home();
    default_settings();

    let mut config = MultiAppConfig::default();
    config.ensure_app(&AppType::Codex);
    let manager = config
        .get_manager_mut(&AppType::Codex)
        .expect("codex manager");
    manager.current = "c".to_string();
    manager.providers.insert(
        "c".to_string(),
        Provider::from_parts("c".to_string(), "C".to_string(), json!({}), None),
    );
    let state = create_test_state_with_config(&config).expect("state");

    let out = cli::env_command_core(&state, "codex", None, Shell::PowerShell, false)
        .expect("codex 缺密钥走告警不返 Err");
    assert_eq!(out.exit_code, EXIT_MISSING_KEY, "无内容且有缺 key 告警 → 码 3");
    assert!(out.stdout.is_empty(), "绝不输出空 export");
    assert!(
        out.warnings.iter().any(|w| w.contains("No credentials available")),
        "stderr 要有明确缺密钥说明，实得: {:?}",
        out.warnings
    );
}

fn pi_config() -> MultiAppConfig {
    let mut config = MultiAppConfig::default();
    config.ensure_app(&AppType::Pi);
    let manager = config.get_manager_mut(&AppType::Pi).expect("pi manager");
    for id in ["p1", "p2"] {
        manager.providers.insert(
            id.to_string(),
            Provider::from_parts(id.to_string(), id.to_uppercase(), json!({}), None),
        );
    }
    config
}

/// 直接播种 Pi 密钥到内存凭据存储，绕开迁移对 `literal:` 前缀的处理差异。
fn seed_pi_key(state: &cc_switch_lib::AppState, id: &str, key: &str) {
    futures::executor::block_on(
        state
            .secrets
            .set(
                &SecretTarget::provider_api_key(AppType::Pi, id.to_string()),
                Zeroizing::new(key.to_string()),
            ),
    )
    .expect("seed pi key");
}

#[test]
fn pi_without_id_maps_to_usage_and_lists_ids() {
    let _guard = test_mutex().lock().unwrap_or_else(|p| p.into_inner());
    reset_test_fs();
    let _home = ensure_test_home();
    default_settings();

    let state = create_test_state_with_config(&pi_config()).expect("state");
    let err = cli::env_command_core(&state, "pi", None, Shell::Bash, false)
        .expect_err("Pi 不给 id 必须报错");
    assert_eq!(classify_exit(&err), EXIT_USAGE);
    assert!(
        err.to_string().contains("p1") && err.to_string().contains("p2"),
        "stderr 应列出可用 Pi 供应商 id，实得: {err}"
    );
}

#[test]
fn pi_with_id_emits_provider_scoped_env_name() {
    let _guard = test_mutex().lock().unwrap_or_else(|p| p.into_inner());
    reset_test_fs();
    let _home = ensure_test_home();
    default_settings();

    let state = create_test_state_with_config(&pi_config()).expect("state");
    seed_pi_key(&state, "p1", "key-p1");
    let out = cli::env_command_core(&state, "pi", Some("p1"), Shell::Bash, false).expect("activate");
    assert_eq!(
        out.stdout,
        "export CC_SWITCH_PI_P1_API_KEY='key-p1'\n",
        "Pi 必须用 per-provider 变量名"
    );
}

#[test]
fn activate_emits_unset_for_stale_registered_var() {
    let _guard = test_mutex().lock().unwrap_or_else(|p| p.into_inner());
    reset_test_fs();
    let _home = ensure_test_home();
    default_settings();

    let state = create_test_state_with_config(&claude_config("a", "k")).expect("state");

    // 模拟 GUI 上轮投递登记了一个本轮不注入的变量。
    let mut managed = ManagedEnvVars::load(&state.db).expect("load");
    managed.register("ANTHROPIC_AUTH_TOKEN", "claude", "a");
    managed.register("STALE_OLD_VAR", "claude", "a");
    managed.save(&state.db).expect("save");

    let out = cli::env_command_core(&state, "claude", None, Shell::PowerShell, false).expect("ok");
    // 陈旧变量先清除，再注入本轮值。
    assert_eq!(
        out.stdout,
        "Remove-Item \"Env:STALE_OLD_VAR\" -ErrorAction SilentlyContinue\n$env:ANTHROPIC_AUTH_TOKEN='k'\n"
    );
    // 激活是只读提示，不动登记表。
    let after = ManagedEnvVars::load(&state.db).expect("reload");
    assert!(
        after.is_managed("STALE_OLD_VAR"),
        "激活顺带 unset 不改登记表（只读取舍）"
    );
}

#[test]
fn clear_unregisters_and_emits_only_unsets() {
    let _guard = test_mutex().lock().unwrap_or_else(|p| p.into_inner());
    reset_test_fs();
    let _home = ensure_test_home();
    default_settings();

    let state = create_test_state_with_config(&claude_config("a", "k")).expect("state");
    let mut managed = ManagedEnvVars::load(&state.db).expect("load");
    managed.register("ANTHROPIC_AUTH_TOKEN", "claude", "a");
    managed.save(&state.db).expect("save");

    let out = cli::env_command_core(&state, "claude", None, Shell::PowerShell, true).expect("clear");
    assert_eq!(out.exit_code, 0);
    assert!(
        !out.stdout.contains("$env:"),
        "--clear 不注入任何新值，实得: {}",
        out.stdout
    );
    assert_eq!(
        out.stdout,
        "Remove-Item \"Env:ANTHROPIC_AUTH_TOKEN\" -ErrorAction SilentlyContinue\n"
    );

    // 写路径：必须已从登记表移除，否则下次激活被判 foreign 拒写。
    let after = ManagedEnvVars::load(&state.db).expect("reload");
    assert!(
        after.vars_for_app("claude").is_empty(),
        "--clear 必须同步清登记（写路径）"
    );
}

#[test]
fn clear_pi_only_touches_given_id() {
    let _guard = test_mutex().lock().unwrap_or_else(|p| p.into_inner());
    reset_test_fs();
    let _home = ensure_test_home();
    default_settings();

    let state = create_test_state_with_config(&pi_config()).expect("state");
    let mut managed = ManagedEnvVars::load(&state.db).expect("load");
    managed.register("CC_SWITCH_PI_P1_API_KEY", "pi", "p1");
    managed.register("CC_SWITCH_PI_P2_API_KEY", "pi", "p2");
    managed.save(&state.db).expect("save");

    let out = cli::env_command_core(&state, "pi", Some("p1"), Shell::Bash, true).expect("clear p1");
    assert_eq!(out.stdout, "unset CC_SWITCH_PI_P1_API_KEY\n");

    let after = ManagedEnvVars::load(&state.db).expect("reload");
    assert!(
        after.vars_for_provider("pi", "p1").is_empty(),
        "p1 应被清除"
    );
    assert_eq!(
        after.vars_for_provider("pi", "p2"),
        vec!["CC_SWITCH_PI_P2_API_KEY".to_string()],
        "Pi additive：清 p1 绝不能波及 p2"
    );
}

#[test]
fn clear_removes_real_value_and_registration_together() {
    let _guard = test_mutex().lock().unwrap_or_else(|p| p.into_inner());
    reset_test_fs();
    let _home = ensure_test_home();
    default_settings();

    let mut state = create_test_state_with_config(&claude_config("a", "k")).expect("state");
    let sink = attach_test_env_sink(&mut state);

    // 模拟非严格投递后的状态：注册表(sink)有值 + 登记在册。
    sink.set(
        "ANTHROPIC_AUTH_TOKEN",
        &Zeroizing::new("delivered-value".to_string()),
    )
    .expect("seed sink");
    let mut managed = ManagedEnvVars::load(&state.db).expect("load");
    managed.register("ANTHROPIC_AUTH_TOKEN", "claude", "a");
    managed.save(&state.db).expect("save");

    let out = cli::env_command_core(&state, "claude", None, Shell::PowerShell, true)
        .expect("clear");
    assert_eq!(out.exit_code, 0);

    // 回归 D-4：删值与摘登记必须成对，否则下次切换会判 foreign 拒写。
    assert!(
        !sink.snapshot().contains_key("ANTHROPIC_AUTH_TOKEN"),
        "--clear 必须把投递落点的真实值也删掉，不能只摘登记留孤儿值"
    );
    let after = ManagedEnvVars::load(&state.db).expect("reload");
    assert!(
        after.vars_for_app("claude").is_empty(),
        "登记簿同步清空"
    );
}
