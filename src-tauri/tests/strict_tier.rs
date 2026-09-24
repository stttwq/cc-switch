//! 2.2 方案 P2：严格模式分级的投递与按-app 清理粒度集成测试。

use serde_json::json;

use cc_switch_lib::{
    update_settings, AppSettings, AppType, ManagedEnvVars, MultiAppConfig, Provider,
    ProviderService,
};

#[path = "support.rs"]
mod support;
use support::{
    attach_test_env_sink, create_test_state_with_config, ensure_test_home, reset_test_fs,
    test_mutex,
};

fn tier_config() -> MultiAppConfig {
    let mut c = MultiAppConfig::default();
    {
        let cm = c.get_manager_mut(&AppType::Claude).expect("claude");
        cm.current = "a".to_string();
        cm.providers.insert(
            "a".to_string(),
            Provider::from_parts(
                "a".to_string(),
                "A".to_string(),
                json!({ "env": { "ANTHROPIC_AUTH_TOKEN": "claude-key" } }),
                None,
            ),
        );
    }
    c.ensure_app(&AppType::Codex);
    {
        let xm = c.get_manager_mut(&AppType::Codex).expect("codex");
        xm.current = "c".to_string();
        xm.providers.insert(
            "c".to_string(),
            Provider::from_parts(
                "c".to_string(),
                "C".to_string(),
                json!({ "auth": { "OPENAI_API_KEY": "codex-key" } }),
                None,
            ),
        );
    }
    c
}

#[test]
fn per_app_strict_only_affects_selected_app() {
    let _guard = test_mutex().lock().unwrap_or_else(|p| p.into_inner());
    reset_test_fs();
    let _home = ensure_test_home();

    // 仅 Claude 严格、Codex 宽松。
    update_settings(AppSettings {
        env_delivery_strict_mode: false,
        env_delivery_strict_apps: Some(vec!["claude".to_string()]),
        ..AppSettings::default()
    })
    .expect("set per-app strict");

    let mut state = create_test_state_with_config(&tier_config()).expect("state");
    let sink = attach_test_env_sink(&mut state);

    ProviderService::switch(&state, AppType::Codex, "c").expect("切换宽松 Codex 应成功");
    ProviderService::switch(&state, AppType::Claude, "a").expect("切换严格 Claude 应成功");

    let snap = sink.snapshot();
    assert_eq!(
        snap.get("CC_SWITCH_CODEX_API_KEY").map(String::as_str),
        Some("codex-key"),
        "Codex 宽松：密钥应正常投递"
    );
    assert!(
        !snap.contains_key("ANTHROPIC_AUTH_TOKEN"),
        "Claude 严格：绝不写环境变量，实际快照: {snap:?}"
    );
}

#[test]
fn purge_only_touches_selected_apps() {
    let _guard = test_mutex().lock().unwrap_or_else(|p| p.into_inner());
    reset_test_fs();
    let _home = ensure_test_home();

    update_settings(AppSettings::default()).expect("常规模式");

    let mut state = create_test_state_with_config(&tier_config()).expect("state");
    let sink = attach_test_env_sink(&mut state);

    ProviderService::switch(&state, AppType::Claude, "a").expect("投递 claude");
    ProviderService::switch(&state, AppType::Codex, "c").expect("投递 codex");

    // 只收回 Claude 的已投递变量，不误伤仍宽松的 Codex。
    ProviderService::purge_env_delivery_for_apps(&state, &["claude".to_string()])
        .expect("purge claude");

    let snap = sink.snapshot();
    assert!(
        !snap.contains_key("ANTHROPIC_AUTH_TOKEN"),
        "Claude 变量应被收回"
    );
    assert_eq!(
        snap.get("CC_SWITCH_CODEX_API_KEY").map(String::as_str),
        Some("codex-key"),
        "Codex 变量不应被动"
    );

    let managed = ManagedEnvVars::load(&state.db).expect("load");
    assert!(managed.vars_for_app("claude").is_empty(), "Claude 登记清空");
    assert!(!managed.vars_for_app("codex").is_empty(), "Codex 登记保留");
}
