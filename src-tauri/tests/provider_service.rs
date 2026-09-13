use serde_json::json;

use cc_switch_lib::{
    get_claude_settings_path, read_json_file, write_codex_live_atomic, AppError, AppType, McpApps,
    McpServer, MultiAppConfig, Provider, ProviderMeta, ProviderService,
};

#[path = "support.rs"]
mod support;
use support::{
    create_test_state, create_test_state_with_config, enable_codex_official_auth_preservation,
    ensure_test_home, reset_test_fs, test_mutex,
};

fn sanitize_provider_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '-',
            _ => c,
        })
        .collect::<String>()
        .to_lowercase()
}

#[test]
fn migrate_legacy_common_config_usage_marks_historical_provider_enabled() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let _home = ensure_test_home();

    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Claude)
            .expect("claude manager");
        manager.current = "legacy-provider".to_string();
        manager.providers.insert(
            "legacy-provider".to_string(),
            Provider::with_id(
                "legacy-provider".to_string(),
                "Legacy".to_string(),
                json!({
                    "includeCoAuthoredBy": false,
                    "env": {
                        "ANTHROPIC_API_KEY": "legacy-key"
                    }
                }),
                None,
            ),
        );
    }

    let state = create_test_state_with_config(&config).expect("create test state");
    state
        .db
        .set_config_snippet(
            AppType::Claude.as_str(),
            Some(r#"{ "includeCoAuthoredBy": false }"#.to_string()),
        )
        .expect("set common config snippet");

    ProviderService::migrate_legacy_common_config_usage_if_needed(&state, AppType::Claude)
        .expect("migrate legacy common config");

    let providers = state
        .db
        .get_all_providers(AppType::Claude.as_str())
        .expect("get providers after migration");
    let provider = providers
        .get("legacy-provider")
        .expect("legacy provider exists");

    assert_eq!(
        provider
            .meta
            .as_ref()
            .and_then(|meta| meta.common_config_enabled),
        Some(true),
        "historical provider should be explicitly marked as using common config"
    );
    assert!(
        provider
            .settings_config
            .get("includeCoAuthoredBy")
            .is_none(),
        "common config fields should be stripped from provider storage after migration"
    );
    assert_eq!(
        provider
            .settings_config
            .get("env")
            .and_then(|v| v.get("ANTHROPIC_API_KEY"))
            .and_then(|v| v.as_str()),
        Some("legacy-key"),
        "provider-specific auth should remain untouched"
    );
}

#[test]
fn provider_service_switch_codex_updates_live_and_config() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    enable_codex_official_auth_preservation();
    let _home = ensure_test_home();

    let legacy_auth = json!({ "OPENAI_API_KEY": "legacy-key" });
    let legacy_config = r#"[mcp_servers.legacy]
type = "stdio"
command = "echo"
"#;
    write_codex_live_atomic(&legacy_auth, Some(legacy_config))
        .expect("seed existing codex live config");

    let mut initial_config = MultiAppConfig::default();
    {
        let manager = initial_config
            .get_manager_mut(&AppType::Codex)
            .expect("codex manager");
        manager.current = "old-provider".to_string();
        manager.providers.insert(
            "old-provider".to_string(),
            Provider::with_id(
                "old-provider".to_string(),
                "Legacy".to_string(),
                json!({
                    "auth": {"OPENAI_API_KEY": "stale"},
                    "config": "stale-config"
                }),
                None,
            ),
        );
        manager.providers.insert(
            "new-provider".to_string(),
            Provider::with_id(
                "new-provider".to_string(),
                "Latest".to_string(),
                json!({
                    "auth": {"OPENAI_API_KEY": "fresh-key"},
                    "config": r#"[mcp_servers.latest]
type = "stdio"
command = "say"
"#
                }),
                None,
            ),
        );
    }

    // 使用新的统一 MCP 结构（v3.7.0+）
    let servers = initial_config
        .mcp
        .servers
        .get_or_insert_with(Default::default);
    servers.insert(
        "echo-server".into(),
        McpServer {
            id: "echo-server".into(),
            name: "Echo Server".into(),
            server: json!({
                "type": "stdio",
                "command": "echo"
            }),
            apps: McpApps {
                claude: false,
                codex: true,
                gemini: false,
                grokbuild: false,
                opencode: false,
                hermes: false,
            },
            description: None,
            homepage: None,
            docs: None,
            tags: Vec::new(),
        },
    );

    let state = create_test_state_with_config(&initial_config).expect("create test state");

    ProviderService::switch(&state, AppType::Codex, "new-provider")
        .expect("switch provider should succeed");

    let auth_value: serde_json::Value =
        read_json_file(&cc_switch_lib::get_codex_auth_path()).expect("read auth.json");
    assert_eq!(
        auth_value.get("OPENAI_API_KEY").and_then(|v| v.as_str()),
        Some("legacy-key"),
        "Codex provider switching should preserve the existing live auth.json"
    );

    let config_text =
        std::fs::read_to_string(cc_switch_lib::get_codex_config_path()).expect("read config.toml");
    assert!(
        config_text.contains("mcp_servers.echo-server"),
        "config.toml should contain synced MCP servers"
    );
    assert!(
        config_text.contains("experimental_bearer_token"),
        "config.toml should carry the selected provider API key"
    );

    let current_id = state
        .db
        .get_current_provider(AppType::Codex.as_str())
        .expect("read current provider after switch");
    assert_eq!(
        current_id.as_deref(),
        Some("new-provider"),
        "current provider updated"
    );

    let providers = state
        .db
        .get_all_providers(AppType::Codex.as_str())
        .expect("read providers after switch");

    let new_provider = providers.get("new-provider").expect("new provider exists");
    let new_config_text = new_provider
        .settings_config
        .get("config")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    // provider 存储的是原始配置，不包含 MCP 同步后的内容
    assert!(
        new_config_text.contains("mcp_servers.latest"),
        "provider config should contain original MCP servers"
    );
    // live 文件额外包含同步的 MCP 服务器
    assert!(
        config_text.contains("mcp_servers.echo-server"),
        "live config should include synced MCP servers"
    );

    let legacy = providers
        .get("old-provider")
        .expect("legacy provider still exists");
    let legacy_auth_value = legacy
        .settings_config
        .get("auth")
        .and_then(|v| v.get("OPENAI_API_KEY"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(
        legacy_auth_value, "legacy-key",
        "previous provider should be backfilled with live auth"
    );
}

#[test]
fn provider_service_switch_codex_preserves_user_model_provider_id_after_migration() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let _home = ensure_test_home();

    let legacy_auth = json!({ "OPENAI_API_KEY": "rightcode-key" });
    let legacy_config = r#"model_provider = "rightcode"
model = "gpt-5.4"

[model_providers.rightcode]
name = "RightCode"
base_url = "https://rightcode.example/v1"
wire_api = "responses"
requires_openai_auth = true
"#;
    write_codex_live_atomic(&legacy_auth, Some(legacy_config))
        .expect("seed existing codex live config");

    let mut initial_config = MultiAppConfig::default();
    {
        let manager = initial_config
            .get_manager_mut(&AppType::Codex)
            .expect("codex manager");
        manager.current = "old-provider".to_string();
        manager.providers.insert(
            "old-provider".to_string(),
            Provider::with_id(
                "old-provider".to_string(),
                "RightCode".to_string(),
                json!({
                    "auth": {"OPENAI_API_KEY": "stale"},
                    "config": legacy_config
                }),
                None,
            ),
        );
        manager.providers.insert(
            "new-provider".to_string(),
            Provider::with_id(
                "new-provider".to_string(),
                "AiHubMix".to_string(),
                json!({
                    "auth": {"OPENAI_API_KEY": "fresh-key"},
                    "config": r#"model_provider = "aihubmix"
model = "gpt-5.4"

[model_providers.aihubmix]
name = "AiHubMix"
base_url = "https://aihubmix.example/v1"
wire_api = "responses"
requires_openai_auth = true
"#
                }),
                None,
            ),
        );
    }

    let state = create_test_state_with_config(&initial_config).expect("create test state");

    ProviderService::switch(&state, AppType::Codex, "new-provider")
        .expect("switch provider should succeed");

    let config_text =
        std::fs::read_to_string(cc_switch_lib::get_codex_config_path()).expect("read config.toml");
    let parsed: toml::Value = toml::from_str(&config_text).expect("parse config.toml");

    assert_eq!(
        parsed.get("model_provider").and_then(|v| v.as_str()),
        Some("aihubmix"),
        "provider switching should preserve user-editable model_provider after the one-time migration"
    );

    let model_providers = parsed
        .get("model_providers")
        .and_then(|v| v.as_table())
        .expect("model_providers table exists");
    assert!(
        model_providers.get("custom").is_none(),
        "provider switching should not force user-edited provider ids back to custom"
    );
    assert_eq!(
        model_providers
            .get("aihubmix")
            .and_then(|v| v.get("base_url"))
            .and_then(|v| v.as_str()),
        Some("https://aihubmix.example/v1"),
        "selected provider id should point at the newly selected supplier endpoint"
    );

    let providers = state
        .db
        .get_all_providers(AppType::Codex.as_str())
        .expect("read providers after switch");
    let new_config_text = providers
        .get("new-provider")
        .expect("new provider exists")
        .settings_config
        .get("config")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        new_config_text.contains("[model_providers.aihubmix]"),
        "stored provider template should remain provider-specific"
    );
}

#[test]
fn provider_service_switch_codex_default_removes_auth_json_when_preservation_off() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    // Intentionally do NOT enable preservation: this locks the default opt-out
    // behavior where a third-party switch deletes auth.json outright — the
    // official OAuth login is not preserved, and the third-party key never
    // lands there either (it travels as the provider-scoped bearer token in
    // config.toml). It is the dual of
    // `provider_service_switch_codex_preserves_oauth_and_backfills_api_key_from_live_token`.
    let _home = ensure_test_home();

    let live_auth = json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": null,
        "tokens": {
            "access_token": "official-oauth-token",
            "account_id": "acct-1"
        }
    });
    let legacy_config = r#"model_provider = "rightcode"
model = "gpt-5.4"

[model_providers.rightcode]
name = "RightCode"
base_url = "https://rightcode.example/v1"
wire_api = "responses"
requires_openai_auth = true
"#;
    write_codex_live_atomic(&live_auth, Some(legacy_config))
        .expect("seed existing Codex OAuth live config");

    let mut initial_config = MultiAppConfig::default();
    {
        let manager = initial_config
            .get_manager_mut(&AppType::Codex)
            .expect("codex manager");
        manager.current = "legacy-provider".to_string();
        manager.providers.insert(
            "legacy-provider".to_string(),
            Provider::with_id(
                "legacy-provider".to_string(),
                "RightCode".to_string(),
                json!({
                    "auth": {"OPENAI_API_KEY": "rightcode-key"},
                    "config": legacy_config
                }),
                None,
            ),
        );
        manager.providers.insert(
            "third-party".to_string(),
            Provider::with_id(
                "third-party".to_string(),
                "AiHubMix".to_string(),
                json!({
                    "auth": {"OPENAI_API_KEY": "third-party-key"},
                    "config": r#"model_provider = "aihubmix"
model = "gpt-5.4"

[model_providers.aihubmix]
name = "AiHubMix"
base_url = "https://aihubmix.example/v1"
wire_api = "responses"
requires_openai_auth = true
"#
                }),
                None,
            ),
        );
    }

    let state = create_test_state_with_config(&initial_config).expect("create test state");

    ProviderService::switch(&state, AppType::Codex, "third-party")
        .expect("switch to third-party provider should succeed");

    assert!(
        !cc_switch_lib::get_codex_auth_path().exists(),
        "default (preservation off) must delete auth.json on a third-party switch — \
         the official login goes away and the key rides in config.toml instead"
    );
    let live_config =
        std::fs::read_to_string(cc_switch_lib::get_codex_config_path()).expect("read config.toml");
    assert!(
        live_config.contains("experimental_bearer_token = \"third-party-key\""),
        "the third-party key must be injected as the provider-scoped bearer token; got:\n{live_config}"
    );
}

#[test]
fn provider_service_switch_codex_default_injects_bearer_token_into_config() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    // Preservation stays OFF (default). Since Codex 0.149 (openai/codex#39214)
    // custom providers no longer inherit ambient auth, so third-party switches
    // are config-only on every path: the key travels as a provider-scoped
    // `experimental_bearer_token` and auth.json is removed.
    let _home = ensure_test_home();

    let third_party_config = r#"model_provider = "aihubmix"
model = "gpt-5.4"

[model_providers.aihubmix]
name = "AiHubMix"
base_url = "https://aihubmix.example/v1"
wire_api = "responses"
requires_openai_auth = false
"#;

    let mut initial_config = MultiAppConfig::default();
    {
        let manager = initial_config
            .get_manager_mut(&AppType::Codex)
            .expect("codex manager");
        manager.providers.insert(
            "third-party".to_string(),
            Provider::with_id(
                "third-party".to_string(),
                "AiHubMix".to_string(),
                json!({
                    "auth": {"OPENAI_API_KEY": "third-party-key"},
                    "config": third_party_config
                }),
                None,
            ),
        );
    }

    let state = create_test_state_with_config(&initial_config).expect("create test state");

    ProviderService::switch(&state, AppType::Codex, "third-party")
        .expect("switch to third-party provider should succeed");

    assert!(
        !cc_switch_lib::get_codex_auth_path().exists(),
        "third-party switches are config-only: no auth.json is written"
    );

    let live_config =
        std::fs::read_to_string(cc_switch_lib::get_codex_config_path()).expect("read config.toml");
    assert!(
        live_config.contains("experimental_bearer_token = \"third-party-key\""),
        "default switch must inject the API key into config.toml so Codex >= 0.149 \
         custom providers authenticate (openai/codex#39214); got:\n{live_config}"
    );
}

#[test]
fn provider_service_switch_codex_preserved_login_rejects_empty_third_party_config() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    // Preservation ON + third-party provider with an empty config: auth.json is
    // not written, and an empty config.toml has no provider table to carry the
    // bearer token, so the API key has nowhere to land while the official
    // OAuth login stays live — Codex would silently fall back to the official
    // provider and bill the ChatGPT account. The switch must be refused, as it
    // was before the bearer-token injection change.
    let _home = ensure_test_home();
    enable_codex_official_auth_preservation();

    let mut initial_config = MultiAppConfig::default();
    {
        let manager = initial_config
            .get_manager_mut(&AppType::Codex)
            .expect("codex manager");
        manager.providers.insert(
            "empty-config".to_string(),
            Provider::with_id(
                "empty-config".to_string(),
                "EmptyConfig".to_string(),
                json!({
                    "auth": {"OPENAI_API_KEY": "third-party-key"},
                    "config": ""
                }),
                None,
            ),
        );
    }

    let state = create_test_state_with_config(&initial_config).expect("create test state");

    let err = ProviderService::switch(&state, AppType::Codex, "empty-config").expect_err(
        "switching to an empty-config third-party provider with preservation on must fail",
    );
    assert!(
        err.to_string().contains("config.toml"),
        "error should explain the missing config.toml, got: {err}"
    );
}

#[test]
fn provider_service_switch_codex_preserved_login_normalizes_legacy_reroute_config() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    // Preservation ON + a legacy-shape third-party config (top-level
    // openai_base_url rerouting the built-in `openai` provider): the shape
    // has no provider table to carry the bearer token — since 0.149 the
    // built-in provider would keep using the preserved official OAuth from
    // auth.json and send it to the third-party base URL. The switch must
    // normalize the config into a cc-switch-owned custom table with the key
    // injected, leaving the official login untouched.
    let _home = ensure_test_home();
    enable_codex_official_auth_preservation();

    let live_auth = json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": null,
        "tokens": {
            "access_token": "official-oauth-token",
            "account_id": "acct-1"
        }
    });
    write_codex_live_atomic(&live_auth, Some("")).expect("seed official OAuth live config");

    let legacy_shape_config = r#"model_provider = "openai"
model = "gpt-5.4"
openai_base_url = "https://relay.example/v1"
"#;

    let mut initial_config = MultiAppConfig::default();
    {
        let manager = initial_config
            .get_manager_mut(&AppType::Codex)
            .expect("codex manager");
        manager.providers.insert(
            "legacy-shape".to_string(),
            Provider::with_id(
                "legacy-shape".to_string(),
                "LegacyShape".to_string(),
                json!({
                    "auth": {"OPENAI_API_KEY": "third-party-key"},
                    "config": legacy_shape_config
                }),
                None,
            ),
        );
    }

    let state = create_test_state_with_config(&initial_config).expect("create test state");

    ProviderService::switch(&state, AppType::Codex, "legacy-shape")
        .expect("legacy reroute shape must be normalized, not rejected");

    let live_config =
        std::fs::read_to_string(cc_switch_lib::get_codex_config_path()).expect("read config.toml");
    assert!(
        !live_config.contains("openai_base_url"),
        "the top-level reroute must be rewritten away; got:\n{live_config}"
    );
    assert!(
        live_config.contains("[model_providers.cc-switch]")
            && live_config.contains("base_url = \"https://relay.example/v1\"")
            && live_config.contains("experimental_bearer_token = \"third-party-key\""),
        "routing and key must move into the cc-switch provider table; got:\n{live_config}"
    );

    let auth_value: serde_json::Value =
        read_json_file(&cc_switch_lib::get_codex_auth_path()).expect("read auth.json");
    assert_eq!(
        auth_value
            .pointer("/tokens/access_token")
            .and_then(|v| v.as_str()),
        Some("official-oauth-token"),
        "the preserved official OAuth login must stay untouched"
    );
}

#[test]
fn provider_service_switch_codex_preserved_login_normalizes_config_carried_token() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    // Same legacy reroute shape, but the key sits in the config text itself
    // (raw-edited provider with `auth = {}`): normalization must see
    // config-carried tokens too, not only auth.OPENAI_API_KEY, and the
    // injected token must land inside the rewritten provider table.
    let _home = ensure_test_home();
    enable_codex_official_auth_preservation();

    let raw_edited_config = r#"model_provider = "openai"
model = "gpt-5.4"
openai_base_url = "https://relay.example/v1"
experimental_bearer_token = "config-carried-key"
"#;

    let mut initial_config = MultiAppConfig::default();
    {
        let manager = initial_config
            .get_manager_mut(&AppType::Codex)
            .expect("codex manager");
        manager.providers.insert(
            "raw-edited".to_string(),
            Provider::with_id(
                "raw-edited".to_string(),
                "RawEdited".to_string(),
                json!({
                    "auth": {},
                    "config": raw_edited_config
                }),
                None,
            ),
        );
    }

    let state = create_test_state_with_config(&initial_config).expect("create test state");

    ProviderService::switch(&state, AppType::Codex, "raw-edited")
        .expect("legacy reroute with a config-carried token must be normalized");

    let live_config =
        std::fs::read_to_string(cc_switch_lib::get_codex_config_path()).expect("read config.toml");
    assert!(
        !live_config.contains("openai_base_url"),
        "the top-level reroute must be rewritten away; got:\n{live_config}"
    );
    assert!(
        live_config.contains("[model_providers.cc-switch]"),
        "a cc-switch provider table must be created; got:\n{live_config}"
    );
    assert_eq!(
        cc_switch_lib::extract_codex_experimental_bearer_token(&live_config).as_deref(),
        Some("config-carried-key"),
        "the config-carried key must resolve for the rewritten provider; got:\n{live_config}"
    );
}

#[test]
fn provider_service_switch_codex_default_normalizes_legacy_reroute_config() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    // Same legacy shape with preservation OFF (default): the switch is
    // config-only on every path, so instead of feeding the built-in
    // provider's ambient auth through auth.json the shape is normalized into
    // a custom table and auth.json is removed.
    let _home = ensure_test_home();

    let legacy_shape_config = r#"model_provider = "openai"
model = "gpt-5.4"
openai_base_url = "https://relay.example/v1"
"#;

    let mut initial_config = MultiAppConfig::default();
    {
        let manager = initial_config
            .get_manager_mut(&AppType::Codex)
            .expect("codex manager");
        manager.providers.insert(
            "legacy-shape".to_string(),
            Provider::with_id(
                "legacy-shape".to_string(),
                "LegacyShape".to_string(),
                json!({
                    "auth": {"OPENAI_API_KEY": "third-party-key"},
                    "config": legacy_shape_config
                }),
                None,
            ),
        );
    }

    let state = create_test_state_with_config(&initial_config).expect("create test state");

    ProviderService::switch(&state, AppType::Codex, "legacy-shape")
        .expect("default-path switch must normalize the legacy ambient-auth shape");

    assert!(
        !cc_switch_lib::get_codex_auth_path().exists(),
        "third-party switches are config-only: no auth.json is written"
    );
    let live_config =
        std::fs::read_to_string(cc_switch_lib::get_codex_config_path()).expect("read config.toml");
    assert!(
        !live_config.contains("openai_base_url")
            && live_config.contains("[model_providers.cc-switch]")
            && live_config.contains("experimental_bearer_token = \"third-party-key\""),
        "routing and key must move into the cc-switch provider table; got:\n{live_config}"
    );
}

#[test]
fn provider_service_switch_codex_preserved_login_rejects_keyless_official_auth_fallback() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    // Preservation ON + a header-auth card with NO API key anywhere
    // (`auth = {}`) whose config also sets `requires_openai_auth = true`:
    // there is no token to inject, so Codex 0.149 resolves auth from the
    // preserved official OAuth in auth.json and applies it AFTER provider
    // headers — the explicit Authorization header is overwritten and the
    // ChatGPT access token + account id go to the third-party endpoint.
    // The switch must be refused (fail closed).
    let _home = ensure_test_home();
    enable_codex_official_auth_preservation();

    let header_auth_with_fallback = r#"model_provider = "custom"
model = "gpt-5.4"

[model_providers.custom]
name = "Custom"
base_url = "https://relay.example/v1"
wire_api = "responses"
requires_openai_auth = true
http_headers = { Authorization = "Bearer explicit-header-token" }
"#;

    let good_config = r#"model_provider = "good"
model = "gpt-5.4"

[model_providers.good]
name = "Good"
base_url = "https://good.example/v1"
wire_api = "responses"
"#;

    let mut initial_config = MultiAppConfig::default();
    {
        let manager = initial_config
            .get_manager_mut(&AppType::Codex)
            .expect("codex manager");
        manager.providers.insert(
            "good".to_string(),
            Provider::with_id(
                "good".to_string(),
                "Good".to_string(),
                json!({
                    "auth": {"OPENAI_API_KEY": "sk-good"},
                    "config": good_config
                }),
                None,
            ),
        );
        manager.providers.insert(
            "header-auth".to_string(),
            Provider::with_id(
                "header-auth".to_string(),
                "HeaderAuth".to_string(),
                json!({
                    "auth": {},
                    "config": header_auth_with_fallback
                }),
                None,
            ),
        );
    }

    let state = create_test_state_with_config(&initial_config).expect("create test state");

    ProviderService::switch(&state, AppType::Codex, "good").expect("switch to the good provider");

    ProviderService::switch(&state, AppType::Codex, "header-auth").expect_err(
        "preservation-on switch must fail when a keyless config falls back to the official auth",
    );

    // The refusal happens in the pre-commit preflight: current must not move,
    // otherwise the next switch would backfill the good provider's live
    // config into the refused card's DB row.
    let current = state
        .db
        .get_current_provider(AppType::Codex.as_str())
        .expect("read current provider");
    assert_eq!(
        current.as_deref(),
        Some("good"),
        "a refused switch must leave current on the previous provider"
    );
}

#[test]
fn provider_service_switch_codex_preserved_login_allows_keyless_header_auth_provider() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    // Same keyless header-auth card WITHOUT the fallback flag: 0.149 resolves
    // this provider as unauthenticated, provider headers survive untouched,
    // and the third-party key in http_headers.Authorization does the auth.
    // This legitimate shape must keep switching under preservation.
    let _home = ensure_test_home();
    enable_codex_official_auth_preservation();

    let header_auth_config = r#"model_provider = "custom"
model = "gpt-5.4"

[model_providers.custom]
name = "Custom"
base_url = "https://relay.example/v1"
wire_api = "responses"
http_headers = { Authorization = "Bearer explicit-header-token" }
"#;

    let mut initial_config = MultiAppConfig::default();
    {
        let manager = initial_config
            .get_manager_mut(&AppType::Codex)
            .expect("codex manager");
        manager.providers.insert(
            "header-auth".to_string(),
            Provider::with_id(
                "header-auth".to_string(),
                "HeaderAuth".to_string(),
                json!({
                    "auth": {},
                    "config": header_auth_config
                }),
                None,
            ),
        );
    }

    let state = create_test_state_with_config(&initial_config).expect("create test state");

    ProviderService::switch(&state, AppType::Codex, "header-auth")
        .expect("preservation-on switch must keep supporting keyless header-auth providers");

    let config_text =
        std::fs::read_to_string(cc_switch_lib::get_codex_config_path()).expect("read config.toml");
    assert!(
        config_text.contains("Authorization = \"Bearer explicit-header-token\""),
        "the provider's own Authorization header must be written verbatim; got:\n{config_text}"
    );
    assert!(
        !config_text.contains("experimental_bearer_token"),
        "no token exists, nothing must be injected; got:\n{config_text}"
    );
}

#[test]
fn explicitly_cleared_common_snippet_is_not_auto_extracted() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let _home = ensure_test_home();

    let state = create_test_state().expect("create test state");
    state
        .db
        .set_config_snippet_cleared(AppType::Claude.as_str(), true)
        .expect("mark snippet explicitly cleared");

    assert!(
        !state
            .db
            .should_auto_extract_config_snippet(AppType::Claude.as_str())
            .expect("check auto-extract eligibility"),
        "explicitly cleared snippets should block auto-extraction"
    );

    state
        .db
        .set_config_snippet(AppType::Claude.as_str(), Some("{}".to_string()))
        .expect("set snippet");
    state
        .db
        .set_config_snippet_cleared(AppType::Claude.as_str(), false)
        .expect("clear explicit-empty marker");

    assert!(
        !state
            .db
            .should_auto_extract_config_snippet(AppType::Claude.as_str())
            .expect("check auto-extract after snippet saved"),
        "existing snippets should also block auto-extraction"
    );
}

#[test]
fn legacy_common_config_migration_flag_roundtrip() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let _home = ensure_test_home();

    let state = create_test_state().expect("create test state");

    assert!(
        !state
            .db
            .is_legacy_common_config_migrated()
            .expect("initial migration flag"),
        "migration flag should default to false"
    );

    state
        .db
        .set_legacy_common_config_migrated(true)
        .expect("set migration flag");
    assert!(
        state
            .db
            .is_legacy_common_config_migrated()
            .expect("read migration flag"),
        "migration flag should persist once set"
    );

    state
        .db
        .set_legacy_common_config_migrated(false)
        .expect("clear migration flag");
    assert!(
        !state
            .db
            .is_legacy_common_config_migrated()
            .expect("read migration flag after clear"),
        "migration flag should be removable for tests/debugging"
    );
}

#[test]
fn switch_packycode_gemini_updates_security_selected_type() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let home = ensure_test_home();

    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Gemini)
            .expect("gemini manager");
        manager.current = "packy-gemini".to_string();
        manager.providers.insert(
            "packy-gemini".to_string(),
            Provider::with_id(
                "packy-gemini".to_string(),
                "PackyCode".to_string(),
                json!({
                    "env": {
                        "GEMINI_API_KEY": "pk-key",
                        "GOOGLE_GEMINI_BASE_URL": "https://www.packyapi.com"
                    }
                }),
                Some("https://www.packyapi.com".to_string()),
            ),
        );
    }

    let state = create_test_state_with_config(&config).expect("create test state");

    ProviderService::switch(&state, AppType::Gemini, "packy-gemini")
        .expect("switching to PackyCode Gemini should succeed");

    // Gemini security settings are written to ~/.gemini/settings.json, not ~/.cc-switch/settings.json
    let settings_path = home.join(".gemini").join("settings.json");
    assert!(
        settings_path.exists(),
        "Gemini settings.json should exist at {}",
        settings_path.display()
    );
    let raw = std::fs::read_to_string(&settings_path).expect("read gemini settings.json");
    let value: serde_json::Value =
        serde_json::from_str(&raw).expect("parse gemini settings.json after switch");

    assert_eq!(
        value
            .pointer("/security/auth/selectedType")
            .and_then(|v| v.as_str()),
        Some("gemini-api-key"),
        "PackyCode Gemini should set security.auth.selectedType"
    );
}

#[test]
fn packycode_partner_meta_triggers_security_flag_even_without_keywords() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let home = ensure_test_home();

    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Gemini)
            .expect("gemini manager");
        manager.current = "packy-meta".to_string();
        let mut provider = Provider::with_id(
            "packy-meta".to_string(),
            "Generic Gemini".to_string(),
            json!({
                "env": {
                    "GEMINI_API_KEY": "pk-meta",
                    "GOOGLE_GEMINI_BASE_URL": "https://generativelanguage.googleapis.com"
                }
            }),
            Some("https://example.com".to_string()),
        );
        provider.meta = Some(ProviderMeta {
            partner_promotion_key: Some("packycode".to_string()),
            ..ProviderMeta::default()
        });
        manager.providers.insert("packy-meta".to_string(), provider);
    }

    let state = create_test_state_with_config(&config).expect("create test state");

    ProviderService::switch(&state, AppType::Gemini, "packy-meta")
        .expect("switching to partner meta provider should succeed");

    // Gemini security settings are written to ~/.gemini/settings.json, not ~/.cc-switch/settings.json
    let settings_path = home.join(".gemini").join("settings.json");
    assert!(
        settings_path.exists(),
        "Gemini settings.json should exist at {}",
        settings_path.display()
    );
    let raw = std::fs::read_to_string(&settings_path).expect("read gemini settings.json");
    let value: serde_json::Value =
        serde_json::from_str(&raw).expect("parse gemini settings.json after switch");

    assert_eq!(
        value
            .pointer("/security/auth/selectedType")
            .and_then(|v| v.as_str()),
        Some("gemini-api-key"),
        "Partner meta should set security.auth.selectedType even without packy keywords"
    );
}

#[test]
fn switch_google_official_gemini_preserves_env_vars() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let home = ensure_test_home();

    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Gemini)
            .expect("gemini manager");
        manager.current = "google-official".to_string();
        let mut provider = Provider::with_id(
            "google-official".to_string(),
            "Google".to_string(),
            json!({
                "env": {
                    "GEMINI_MODEL": "gemini-2.5-pro"
                }
            }),
            Some("https://ai.google.dev".to_string()),
        );
        provider.meta = Some(ProviderMeta {
            partner_promotion_key: Some("google-official".to_string()),
            ..ProviderMeta::default()
        });
        manager
            .providers
            .insert("google-official".to_string(), provider);
    }

    let state = create_test_state_with_config(&config).expect("create test state");

    ProviderService::switch(&state, AppType::Gemini, "google-official")
        .expect("switching to Google official Gemini should succeed");

    // Verify env vars are preserved in ~/.gemini/.env
    let env_path = home.join(".gemini").join(".env");
    assert!(
        env_path.exists(),
        "Gemini .env should exist at {}",
        env_path.display()
    );
    let env_content = std::fs::read_to_string(&env_path).expect("read gemini .env");
    assert!(
        env_content.contains("GEMINI_MODEL=gemini-2.5-pro"),
        "GEMINI_MODEL should be preserved in .env, got: {env_content}"
    );

    // Verify OAuth security flag is still set correctly
    let gemini_settings = home.join(".gemini").join("settings.json");
    let gemini_raw = std::fs::read_to_string(&gemini_settings).expect("read gemini settings");
    let gemini_value: serde_json::Value =
        serde_json::from_str(&gemini_raw).expect("parse gemini settings");
    assert_eq!(
        gemini_value
            .pointer("/security/auth/selectedType")
            .and_then(|v| v.as_str()),
        Some("oauth-personal"),
        "OAuth security flag should still be set"
    );
}

#[test]
fn provider_service_switch_claude_updates_live_and_state() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let _home = ensure_test_home();

    let settings_path = get_claude_settings_path();
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent).expect("create claude settings dir");
    }
    let legacy_live = json!({
        "env": {
            "ANTHROPIC_API_KEY": "legacy-key"
        },
        "workspace": {
            "path": "/tmp/workspace"
        }
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&legacy_live).expect("serialize legacy live"),
    )
    .expect("seed claude live config");

    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Claude)
            .expect("claude manager");
        manager.current = "old-provider".to_string();
        manager.providers.insert(
            "old-provider".to_string(),
            Provider::with_id(
                "old-provider".to_string(),
                "Legacy Claude".to_string(),
                json!({
                    "env": { "ANTHROPIC_API_KEY": "stale-key" }
                }),
                None,
            ),
        );
        manager.providers.insert(
            "new-provider".to_string(),
            Provider::with_id(
                "new-provider".to_string(),
                "Fresh Claude".to_string(),
                json!({
                    "env": { "ANTHROPIC_API_KEY": "fresh-key" },
                    "workspace": { "path": "/tmp/new-workspace" }
                }),
                None,
            ),
        );
    }

    let state = create_test_state_with_config(&config).expect("create test state");

    ProviderService::switch(&state, AppType::Claude, "new-provider")
        .expect("switch provider should succeed");

    let live_after: serde_json::Value =
        read_json_file(&settings_path).expect("read claude live settings");
    assert_eq!(
        live_after
            .get("env")
            .and_then(|env| env.get("ANTHROPIC_API_KEY"))
            .and_then(|key| key.as_str()),
        Some("fresh-key"),
        "live settings.json should reflect new provider auth"
    );

    let providers = state
        .db
        .get_all_providers(AppType::Claude.as_str())
        .expect("get all providers");
    let current_id = state
        .db
        .get_current_provider(AppType::Claude.as_str())
        .expect("get current provider");
    assert_eq!(
        current_id.as_deref(),
        Some("new-provider"),
        "current provider updated"
    );

    let legacy_provider = providers
        .get("old-provider")
        .expect("legacy provider still exists");
    assert_eq!(
        legacy_provider.settings_config, legacy_live,
        "previous provider should receive backfilled live config"
    );
}

/// 切走勾选了通用配置的 Claude 供应商时，应把它 live 里新增的可共享键
/// （用户直接在应用内装插件/改偏好）捕获进通用配置片段，并带到下一个供应商。
#[test]
fn switch_claude_syncs_new_shared_keys_from_live_into_common_config() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let _home = ensure_test_home();

    let settings_path = get_claude_settings_path();
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent).expect("create claude settings dir");
    }
    // A 的 live = A 私有密钥（含非 Anthropic 的 OpenRouter 凭据）+ 已共享的 theme
    // + 用户刚在应用内新增的 enableAllProjectMcpServers
    let live = json!({
        "env": { "ANTHROPIC_API_KEY": "a-key", "OPENROUTER_API_KEY": "sk-or-leak" },
        "theme": "dark",
        "enableAllProjectMcpServers": true
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&live).expect("serialize live"),
    )
    .expect("seed claude live config");

    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Claude)
            .expect("claude manager");
        manager.current = "a".to_string();
        let mut provider_a = Provider::with_id(
            "a".to_string(),
            "A".to_string(),
            json!({ "env": { "ANTHROPIC_API_KEY": "a-key" } }),
            None,
        );
        provider_a.meta = Some(ProviderMeta {
            common_config_enabled: Some(true),
            ..Default::default()
        });
        manager.providers.insert("a".to_string(), provider_a);
        let mut provider_b = Provider::with_id(
            "b".to_string(),
            "B".to_string(),
            json!({ "env": { "ANTHROPIC_API_KEY": "b-key" } }),
            None,
        );
        provider_b.meta = Some(ProviderMeta {
            common_config_enabled: Some(true),
            ..Default::default()
        });
        manager.providers.insert("b".to_string(), provider_b);
    }

    let state = create_test_state_with_config(&config).expect("create test state");
    state
        .db
        .set_config_snippet(
            AppType::Claude.as_str(),
            Some(r#"{"theme":"dark"}"#.to_string()),
        )
        .expect("seed common config snippet");

    ProviderService::switch(&state, AppType::Claude, "b").expect("switch should succeed");

    // 片段应捕获到新增键，并保留已有共享键，且绝不含密钥
    let snippet = state
        .db
        .get_config_snippet(AppType::Claude.as_str())
        .expect("read snippet")
        .expect("snippet present");
    let snippet_value: serde_json::Value =
        serde_json::from_str(&snippet).expect("snippet is valid JSON");
    assert_eq!(
        snippet_value.get("enableAllProjectMcpServers"),
        Some(&json!(true)),
        "newly added shared key should be captured into common config"
    );
    assert_eq!(
        snippet_value.get("theme").and_then(|v| v.as_str()),
        Some("dark"),
        "previously shared key should be preserved"
    );
    assert!(
        snippet_value
            .get("env")
            .and_then(|env| env.get("ANTHROPIC_API_KEY"))
            .is_none(),
        "secrets must never leak into the shared snippet"
    );
    assert!(
        snippet_value
            .get("env")
            .and_then(|env| env.get("OPENROUTER_API_KEY"))
            .is_none(),
        "non-Anthropic Claude credentials must never leak into the shared snippet"
    );

    // 新增键应通过通用配置带到 B 的 live
    let live_after: serde_json::Value =
        read_json_file(&settings_path).expect("read live after switch");
    assert_eq!(
        live_after.get("enableAllProjectMcpServers"),
        Some(&json!(true)),
        "shared key should propagate to the next provider's live config"
    );
    assert!(
        live_after
            .get("env")
            .and_then(|env| env.get("OPENROUTER_API_KEY"))
            .is_none(),
        "leaked credential must not be injected into the next provider's live"
    );
    assert_eq!(
        live_after
            .get("env")
            .and_then(|env| env.get("ANTHROPIC_API_KEY"))
            .and_then(|v| v.as_str()),
        Some("b-key"),
        "live should reflect new provider's own auth"
    );
}

/// 用户在应用内删掉一个已共享的键后，切换应把删除同步进通用配置，
/// 且不会在切到下一个供应商时被重新注入（否则会"删不掉"）。
#[test]
fn switch_claude_syncs_deletions_from_live_into_common_config() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let _home = ensure_test_home();

    let settings_path = get_claude_settings_path();
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent).expect("create claude settings dir");
    }
    // live 里 theme 还在，但用户已删掉 enableAllProjectMcpServers
    let live = json!({
        "env": { "ANTHROPIC_API_KEY": "a-key" },
        "theme": "dark"
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&live).expect("serialize live"),
    )
    .expect("seed claude live config");

    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Claude)
            .expect("claude manager");
        manager.current = "a".to_string();
        let mut provider_a = Provider::with_id(
            "a".to_string(),
            "A".to_string(),
            json!({ "env": { "ANTHROPIC_API_KEY": "a-key" } }),
            None,
        );
        provider_a.meta = Some(ProviderMeta {
            common_config_enabled: Some(true),
            ..Default::default()
        });
        manager.providers.insert("a".to_string(), provider_a);
        let mut provider_b = Provider::with_id(
            "b".to_string(),
            "B".to_string(),
            json!({ "env": { "ANTHROPIC_API_KEY": "b-key" } }),
            None,
        );
        provider_b.meta = Some(ProviderMeta {
            common_config_enabled: Some(true),
            ..Default::default()
        });
        manager.providers.insert("b".to_string(), provider_b);
    }

    let state = create_test_state_with_config(&config).expect("create test state");
    // 片段里仍残留 enableAllProjectMcpServers（上次共享的）
    state
        .db
        .set_config_snippet(
            AppType::Claude.as_str(),
            Some(r#"{"theme":"dark","enableAllProjectMcpServers":true}"#.to_string()),
        )
        .expect("seed common config snippet");

    ProviderService::switch(&state, AppType::Claude, "b").expect("switch should succeed");

    let snippet = state
        .db
        .get_config_snippet(AppType::Claude.as_str())
        .expect("read snippet")
        .expect("snippet present");
    let snippet_value: serde_json::Value =
        serde_json::from_str(&snippet).expect("snippet is valid JSON");
    assert!(
        snippet_value.get("enableAllProjectMcpServers").is_none(),
        "deleted key should be removed from common config"
    );
    assert_eq!(
        snippet_value.get("theme").and_then(|v| v.as_str()),
        Some("dark"),
        "untouched shared key should remain"
    );

    // 切到 B 后 live 不应再出现被删除的键
    let live_after: serde_json::Value =
        read_json_file(&settings_path).expect("read live after switch");
    assert!(
        live_after.get("enableAllProjectMcpServers").is_none(),
        "deleted shared key must not be re-injected into the next provider"
    );
}

/// Codex 版切换自动回写：live 里新增的共享键被捕获进通用配置片段并传递给
/// 下一个供应商；供应商专属字段、密钥与 cc-switch 注入产物绝不进片段；
/// 回填后旧供应商的存储配置不残留片段内容（autosync 先于 strip，值必然匹配）。
#[test]
fn switch_codex_syncs_shared_keys_from_live_into_common_config() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let _home = ensure_test_home();

    // A 激活状态下的 live：A 专属路由 + 已共享的 [tui] + 用户刚加的
    // disable_response_storage + cc-switch 注入产物 + MCP 同步投影
    // + 顶层 wire_api（无 model_provider 时的 fallback 写法，属 A 的路由语义）
    // + 历史错误格式 [mcp.servers]（sync_all_enabled 清不掉的孤儿形态）
    let live_config = r#"model = "gpt-5.5"
model_provider = "aprov"
wire_api = "chat"
experimental_bearer_token = "sk-a-live-secret"
model_catalog_json = "cc-switch-model-catalog.json"
web_search = "disabled"
disable_response_storage = true

[tui]
notifications = true

[model_providers.aprov]
name = "A Prov"
base_url = "https://a.example/v1"
wire_api = "responses"

[mcp_servers.echo]
type = "stdio"
command = "echo"

[mcp.servers.ghost-legacy]
command = "ghost-cmd"
"#;
    write_codex_live_atomic(&json!({ "OPENAI_API_KEY": "sk-a" }), Some(live_config))
        .expect("seed codex live config");

    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Codex)
            .expect("codex manager");
        manager.current = "a".to_string();
        let mut provider_a = Provider::with_id(
            "a".to_string(),
            "A".to_string(),
            json!({
                "auth": { "OPENAI_API_KEY": "sk-a" },
                "config": "model = \"gpt-5.5\"\nmodel_provider = \"aprov\"\n\n[model_providers.aprov]\nname = \"A Prov\"\nbase_url = \"https://a.example/v1\"\nwire_api = \"responses\"\n"
            }),
            None,
        );
        provider_a.meta = Some(ProviderMeta {
            common_config_enabled: Some(true),
            ..Default::default()
        });
        manager.providers.insert("a".to_string(), provider_a);
        let mut provider_b = Provider::with_id(
            "b".to_string(),
            "B".to_string(),
            json!({
                "auth": { "OPENAI_API_KEY": "sk-b" },
                "config": "model = \"gpt-5.5\"\nmodel_provider = \"bprov\"\n\n[model_providers.bprov]\nname = \"B Prov\"\nbase_url = \"https://b.example/v1\"\nwire_api = \"responses\"\n"
            }),
            None,
        );
        provider_b.meta = Some(ProviderMeta {
            common_config_enabled: Some(true),
            ..Default::default()
        });
        manager.providers.insert("b".to_string(), provider_b);
    }

    let state = create_test_state_with_config(&config).expect("create test state");
    state
        .db
        .set_config_snippet(
            AppType::Codex.as_str(),
            Some("[tui]\nnotifications = true\n".to_string()),
        )
        .expect("seed codex common config snippet");

    ProviderService::switch(&state, AppType::Codex, "b").expect("switch should succeed");

    // 片段：捕获新增共享键、保留既有共享键；专属字段/密钥/注入产物一律不进
    let snippet = state
        .db
        .get_config_snippet(AppType::Codex.as_str())
        .expect("read snippet")
        .expect("snippet present");
    assert!(
        snippet.contains("disable_response_storage = true"),
        "newly added shared key should be captured, got: {snippet}"
    );
    assert!(
        snippet.contains("notifications = true"),
        "previously shared key should be preserved, got: {snippet}"
    );
    for forbidden in [
        "experimental_bearer_token",
        "sk-a-live-secret",
        "model_catalog_json",
        "web_search",
        "mcp_servers",
        "model_providers",
        "model_provider",
        "wire_api",
        "ghost-legacy",
    ] {
        assert!(
            !snippet.contains(forbidden),
            "'{forbidden}' must never enter the shared snippet, got: {snippet}"
        );
    }

    // B 的 live：共享键传递到位，A 的密钥/投影不得跟过来
    let live_after = std::fs::read_to_string(cc_switch_lib::get_codex_config_path())
        .expect("read config.toml after switch");
    assert!(
        live_after.contains("disable_response_storage = true"),
        "shared key should propagate to the next provider's live, got: {live_after}"
    );
    assert!(
        live_after.contains("model_provider = \"bprov\""),
        "live should be provider B's own config, got: {live_after}"
    );
    assert!(
        !live_after.contains("sk-a-live-secret"),
        "provider A's bearer token must not leak into B's live, got: {live_after}"
    );
    assert!(
        !live_after.contains("mcp_servers"),
        "no DB-enabled MCP servers, so live must not resurrect stale entries, got: {live_after}"
    );
    assert!(
        !live_after.contains("ghost-legacy"),
        "the legacy [mcp.servers] orphan must not propagate to B's live, got: {live_after}"
    );
    assert!(
        !live_after.contains("wire_api = \"chat\""),
        "provider A's top-level wire_api must not rewrite B's protocol, got: {live_after}"
    );

    // A 的存储配置：回填后不残留片段内容 / MCP 投影 / 注入产物
    let providers = state
        .db
        .get_all_providers(AppType::Codex.as_str())
        .expect("read providers after switch");
    let stored_a = providers.get("a").expect("provider a exists");
    let stored_a_config = stored_a
        .settings_config
        .get("config")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        stored_a_config.contains("model_provider = \"aprov\""),
        "provider-owned routing must survive backfill, got: {stored_a_config}"
    );
    // 顶层 wire_api 是 A 自己的路由语义：不进片段，但回填时留在 A 的快照里
    assert!(
        stored_a_config.contains("wire_api = \"chat\""),
        "provider-owned top-level wire_api must survive backfill, got: {stored_a_config}"
    );
    for forbidden in [
        "disable_response_storage",
        "notifications",
        "mcp_servers",
        "experimental_bearer_token",
        "ghost-legacy",
    ] {
        assert!(
            !stored_a_config.contains(forbidden),
            "'{forbidden}' must be stripped from the stored provider config on backfill, got: {stored_a_config}"
        );
    }
}

/// Codex 版删除同步：用户在 live 里删掉一个已共享的键后，切换应把删除
/// 同步进通用配置，且不会在切到下一个供应商时被重新注入（否则"删不掉"）。
#[test]
fn switch_codex_syncs_deletions_from_live_into_common_config() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let _home = ensure_test_home();

    // 片段里有两个共享键，但用户已在 live 里删掉 disable_response_storage
    let live_config = r#"model_provider = "aprov"

[tui]
notifications = true

[model_providers.aprov]
name = "A Prov"
base_url = "https://a.example/v1"
wire_api = "responses"
"#;
    write_codex_live_atomic(&json!({ "OPENAI_API_KEY": "sk-a" }), Some(live_config))
        .expect("seed codex live config");

    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Codex)
            .expect("codex manager");
        manager.current = "a".to_string();
        for (id, name, prov_key) in [("a", "A", "aprov"), ("b", "B", "bprov")] {
            let mut provider = Provider::with_id(
                id.to_string(),
                name.to_string(),
                json!({
                    "auth": { "OPENAI_API_KEY": format!("sk-{id}") },
                    "config": format!("model_provider = \"{prov_key}\"\n\n[model_providers.{prov_key}]\nname = \"{name} Prov\"\nbase_url = \"https://{id}.example/v1\"\nwire_api = \"responses\"\n")
                }),
                None,
            );
            provider.meta = Some(ProviderMeta {
                common_config_enabled: Some(true),
                ..Default::default()
            });
            manager.providers.insert(id.to_string(), provider);
        }
    }

    let state = create_test_state_with_config(&config).expect("create test state");
    state
        .db
        .set_config_snippet(
            AppType::Codex.as_str(),
            Some("disable_response_storage = true\n\n[tui]\nnotifications = true\n".to_string()),
        )
        .expect("seed codex common config snippet");

    ProviderService::switch(&state, AppType::Codex, "b").expect("switch should succeed");

    let snippet = state
        .db
        .get_config_snippet(AppType::Codex.as_str())
        .expect("read snippet")
        .expect("snippet present");
    assert!(
        !snippet.contains("disable_response_storage"),
        "deleted shared key must be removed from the snippet, got: {snippet}"
    );
    assert!(
        snippet.contains("notifications = true"),
        "kept shared key should remain in the snippet, got: {snippet}"
    );

    let live_after = std::fs::read_to_string(cc_switch_lib::get_codex_config_path())
        .expect("read config.toml after switch");
    assert!(
        !live_after.contains("disable_response_storage"),
        "deleted shared key must not be re-injected into the next provider, got: {live_after}"
    );
    assert!(
        live_after.contains("notifications = true"),
        "kept shared key should propagate to the next provider, got: {live_after}"
    );
}

/// 未勾选"写入通用配置"的供应商，其 live 改动不应自动污染通用配置片段。
#[test]
fn switch_claude_does_not_sync_common_config_for_opted_out_provider() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let _home = ensure_test_home();

    let settings_path = get_claude_settings_path();
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent).expect("create claude settings dir");
    }
    let live = json!({
        "env": { "ANTHROPIC_API_KEY": "a-key" },
        "providerSpecific": "x"
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&live).expect("serialize live"),
    )
    .expect("seed claude live config");

    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Claude)
            .expect("claude manager");
        manager.current = "a".to_string();
        // A 未勾选通用配置（meta = None）
        manager.providers.insert(
            "a".to_string(),
            Provider::with_id(
                "a".to_string(),
                "A".to_string(),
                json!({ "env": { "ANTHROPIC_API_KEY": "a-key" } }),
                None,
            ),
        );
        manager.providers.insert(
            "b".to_string(),
            Provider::with_id(
                "b".to_string(),
                "B".to_string(),
                json!({ "env": { "ANTHROPIC_API_KEY": "b-key" } }),
                None,
            ),
        );
    }

    let state = create_test_state_with_config(&config).expect("create test state");
    state
        .db
        .set_config_snippet(
            AppType::Claude.as_str(),
            Some(r#"{"theme":"dark"}"#.to_string()),
        )
        .expect("seed common config snippet");

    ProviderService::switch(&state, AppType::Claude, "b").expect("switch should succeed");

    let snippet = state
        .db
        .get_config_snippet(AppType::Claude.as_str())
        .expect("read snippet")
        .expect("snippet present");
    let snippet_value: serde_json::Value =
        serde_json::from_str(&snippet).expect("snippet is valid JSON");
    assert!(
        snippet_value.get("providerSpecific").is_none(),
        "opted-out provider's live changes must not pollute the shared snippet"
    );
    assert_eq!(
        snippet_value.get("theme").and_then(|v| v.as_str()),
        Some("dark"),
        "snippet should stay unchanged for opted-out providers"
    );
}

/// 用户显式清空过通用配置（_cleared）后，切换不应把片段重新塞回来。
#[test]
fn switch_claude_respects_explicitly_cleared_common_config() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let _home = ensure_test_home();

    let settings_path = get_claude_settings_path();
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent).expect("create claude settings dir");
    }
    let live = json!({
        "env": { "ANTHROPIC_API_KEY": "a-key" },
        "theme": "dark"
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&live).expect("serialize live"),
    )
    .expect("seed claude live config");

    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Claude)
            .expect("claude manager");
        manager.current = "a".to_string();
        let mut provider_a = Provider::with_id(
            "a".to_string(),
            "A".to_string(),
            json!({ "env": { "ANTHROPIC_API_KEY": "a-key" } }),
            None,
        );
        provider_a.meta = Some(ProviderMeta {
            common_config_enabled: Some(true),
            ..Default::default()
        });
        manager.providers.insert("a".to_string(), provider_a);
        let mut provider_b = Provider::with_id(
            "b".to_string(),
            "B".to_string(),
            json!({ "env": { "ANTHROPIC_API_KEY": "b-key" } }),
            None,
        );
        provider_b.meta = Some(ProviderMeta {
            common_config_enabled: Some(true),
            ..Default::default()
        });
        manager.providers.insert("b".to_string(), provider_b);
    }

    let state = create_test_state_with_config(&config).expect("create test state");
    state
        .db
        .set_config_snippet_cleared(AppType::Claude.as_str(), true)
        .expect("mark snippet cleared");

    ProviderService::switch(&state, AppType::Claude, "b").expect("switch should succeed");

    assert!(
        state
            .db
            .get_config_snippet(AppType::Claude.as_str())
            .expect("read snippet")
            .is_none(),
        "explicitly cleared snippet must not be resurrected by switch-away sync"
    );
}

#[test]
fn provider_service_switch_missing_provider_returns_error() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let _home = ensure_test_home();

    let state = create_test_state().expect("create test state");

    let err = ProviderService::switch(&state, AppType::Claude, "missing")
        .expect_err("switching missing provider should fail");
    match err {
        AppError::Message(msg) => {
            assert!(
                msg.contains("不存在") || msg.contains("not found"),
                "expected provider not found message, got {msg}"
            );
        }
        other => panic!("expected Message error for provider not found, got {other:?}"),
    }
}

#[test]
fn provider_service_switch_codex_missing_auth_returns_error() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let _home = ensure_test_home();

    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Codex)
            .expect("codex manager");
        manager.providers.insert(
            "invalid".to_string(),
            Provider::with_id(
                "invalid".to_string(),
                "Broken Codex".to_string(),
                json!({
                    "config": "[mcp_servers.test]\ncommand = \"noop\""
                }),
                None,
            ),
        );
    }

    let state = create_test_state_with_config(&config).expect("create test state");

    let err = ProviderService::switch(&state, AppType::Codex, "invalid")
        .expect_err("switching should fail without auth");
    match err {
        AppError::Config(msg) => assert!(
            msg.contains("auth"),
            "expected auth related message, got {msg}"
        ),
        other => panic!("expected config error, got {other:?}"),
    }
}

#[test]
fn provider_service_delete_codex_removes_provider_and_files() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let home = ensure_test_home();

    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Codex)
            .expect("codex manager");
        manager.current = "keep".to_string();
        manager.providers.insert(
            "keep".to_string(),
            Provider::with_id(
                "keep".to_string(),
                "Keep".to_string(),
                json!({
                    "auth": {"OPENAI_API_KEY": "keep-key"},
                    "config": ""
                }),
                None,
            ),
        );
        manager.providers.insert(
            "to-delete".to_string(),
            Provider::with_id(
                "to-delete".to_string(),
                "DeleteCodex".to_string(),
                json!({
                    "auth": {"OPENAI_API_KEY": "delete-key"},
                    "config": ""
                }),
                None,
            ),
        );
    }

    let sanitized = sanitize_provider_name("DeleteCodex");
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir).expect("create codex dir");
    let auth_path = codex_dir.join(format!("auth-{sanitized}.json"));
    let cfg_path = codex_dir.join(format!("config-{sanitized}.toml"));
    std::fs::write(&auth_path, "{}").expect("seed auth file");
    std::fs::write(&cfg_path, "base_url = \"https://example\"").expect("seed config file");

    let app_state = create_test_state_with_config(&config).expect("create test state");

    ProviderService::delete(&app_state, AppType::Codex, "to-delete")
        .expect("delete provider should succeed");

    let providers = app_state
        .db
        .get_all_providers(AppType::Codex.as_str())
        .expect("get all providers");
    assert!(
        !providers.contains_key("to-delete"),
        "provider entry should be removed"
    );
    // v3.7.0+ 不再使用供应商特定文件（如 auth-*.json, config-*.toml）
    // 删除供应商只影响数据库记录，不清理这些旧格式文件
}

#[test]
fn provider_service_delete_claude_removes_provider_files() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let home = ensure_test_home();

    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Claude)
            .expect("claude manager");
        manager.current = "keep".to_string();
        manager.providers.insert(
            "keep".to_string(),
            Provider::with_id(
                "keep".to_string(),
                "Keep".to_string(),
                json!({
                    "env": { "ANTHROPIC_API_KEY": "keep-key" }
                }),
                None,
            ),
        );
        manager.providers.insert(
            "delete".to_string(),
            Provider::with_id(
                "delete".to_string(),
                "DeleteClaude".to_string(),
                json!({
                    "env": { "ANTHROPIC_API_KEY": "delete-key" }
                }),
                None,
            ),
        );
    }

    let sanitized = sanitize_provider_name("DeleteClaude");
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(&claude_dir).expect("create claude dir");
    let by_name = claude_dir.join(format!("settings-{sanitized}.json"));
    let by_id = claude_dir.join("settings-delete.json");
    std::fs::write(&by_name, "{}").expect("seed settings by name");
    std::fs::write(&by_id, "{}").expect("seed settings by id");

    let app_state = create_test_state_with_config(&config).expect("create test state");

    ProviderService::delete(&app_state, AppType::Claude, "delete").expect("delete claude provider");

    let providers = app_state
        .db
        .get_all_providers(AppType::Claude.as_str())
        .expect("get all providers");
    assert!(
        !providers.contains_key("delete"),
        "claude provider should be removed"
    );
    // v3.7.0+ 不再使用供应商特定文件（如 settings-*.json）
    // 删除供应商只影响数据库记录，不清理这些旧格式文件
}

#[test]
fn provider_service_delete_current_provider_returns_error() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let _home = ensure_test_home();

    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Claude)
            .expect("claude manager");
        manager.current = "keep".to_string();
        manager.providers.insert(
            "keep".to_string(),
            Provider::with_id(
                "keep".to_string(),
                "Keep".to_string(),
                json!({
                    "env": { "ANTHROPIC_API_KEY": "keep-key" }
                }),
                None,
            ),
        );
    }

    let app_state = create_test_state_with_config(&config).expect("create test state");

    let err = ProviderService::delete(&app_state, AppType::Claude, "keep")
        .expect_err("deleting current provider should fail");
    match err {
        AppError::Localized { zh, .. } => assert!(
            zh.contains("不能删除当前正在使用的供应商")
                || zh.contains("无法删除当前正在使用的供应商"),
            "unexpected message: {zh}"
        ),
        AppError::Config(msg) => assert!(
            msg.contains("不能删除当前正在使用的供应商")
                || msg.contains("无法删除当前正在使用的供应商"),
            "unexpected message: {msg}"
        ),
        AppError::Message(msg) => assert!(
            msg.contains("不能删除当前正在使用的供应商")
                || msg.contains("无法删除当前正在使用的供应商"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected Config/Message error, got {other:?}"),
    }
}
