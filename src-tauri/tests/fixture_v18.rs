//! Phase 0 fixture generator (plan: docs/plans/secrets-credential-manager-slimdown-plan-zh.md).
//!
//! Generates `tests/fixtures/v18-home/` — a complete CC_SWITCH_TEST_HOME snapshot
//! at schema v18 with recognizable plaintext secrets — plus a standalone copy of
//! the database at `tests/fixtures/v18-plaintext.db`.
//!
//! Run explicitly:
//!   cargo test --test fixture_v18 -- --ignored --nocapture
//!
//! Fixture secret literals (scanned by scripts/secret-scan.ps1):
//!   sk-fixture-claude-0001 .. sk-fixture-claude-0003
//!   sk-fixture-codex-official-0001
//!   sk-fixture-codex-3rd-0002
//!   sk-fixture-pi-0003 / sk-fixture-pi-0004
//!   sk-fixture-usage-0005
//!   webdav-fixture-pass-0006

use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde_json::{json, Value};

use cc_switch_lib::{Database, Provider, ProviderMeta};

/// Env-var mutation + shared home dir force these two tests to run serially.
fn fixture_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    match LOCK.get_or_init(|| Mutex::new(())).lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

const CLAUDE_KEY: &str = "sk-fixture-claude-0001";
const CLAUDE_KEY_ALT_FIELD: &str = "sk-fixture-claude-0002";
const CLAUDE_KEY_EXTRA_ENV: &str = "sk-fixture-claude-0003";
const CODEX_OFFICIAL_KEY: &str = "sk-fixture-codex-official-0001";
const CODEX_3RD_KEY: &str = "sk-fixture-codex-3rd-0002";
const PI_KEY_1: &str = "sk-fixture-pi-0003";
const PI_KEY_2: &str = "sk-fixture-pi-0004";
const USAGE_SCRIPT_KEY: &str = "sk-fixture-usage-0005";
const WEBDAV_PASSWORD: &str = "webdav-fixture-pass-0006";

pub fn fixture_secret_literals() -> Vec<&'static str> {
    vec![
        CLAUDE_KEY,
        CLAUDE_KEY_ALT_FIELD,
        CLAUDE_KEY_EXTRA_ENV,
        CODEX_OFFICIAL_KEY,
        CODEX_3RD_KEY,
        PI_KEY_1,
        PI_KEY_2,
        USAGE_SCRIPT_KEY,
        WEBDAV_PASSWORD,
    ]
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("v18-home")
}

fn provider(id: &str, name: &str, settings_config: Value, meta: Option<ProviderMeta>) -> Provider {
    let mut p = Provider::with_id(id.to_string(), name.to_string(), settings_config, None);
    p.meta = meta;
    p
}

#[test]
#[ignore = "fixture generator: run explicitly with `cargo test --test fixture_v18 -- --ignored`"]
fn generate_v18_plaintext_fixture() {
    let _guard = fixture_lock();
    let home = fixture_root();
    if home.exists() {
        fs::remove_dir_all(&home).unwrap();
    }
    fs::create_dir_all(home.join(".cc-switch")).unwrap();

    // SAFETY: fixture generation runs standalone (#[ignore]) and restores HOME vars on drop.
    let prev_vars: Vec<(&'static str, Option<std::ffi::OsString>)> = vec![
        (
            "CC_SWITCH_TEST_HOME",
            std::env::var_os("CC_SWITCH_TEST_HOME"),
        ),
        ("HOME", std::env::var_os("HOME")),
        ("USERPROFILE", std::env::var_os("USERPROFILE")),
    ];
    std::env::set_var("CC_SWITCH_TEST_HOME", &home);
    std::env::set_var("HOME", &home);
    std::env::set_var("USERPROFILE", &home);
    struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (k, v) in self.0.iter() {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
    let _restore = Restore(prev_vars);

    let db = Database::init().expect("Database::init should create a fresh v18 database");

    // --- Claude x3 ---
    let claude_meta = ProviderMeta::default();
    db.save_provider(
        "claude",
        &provider(
            "fixture-claude-auth-token",
            "Fixture Claude AuthToken",
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": CLAUDE_KEY,
                    "ANTHROPIC_BASE_URL": "https://fixture-claude.example.com/v1"
                }
            }),
            Some(claude_meta),
        ),
    )
    .unwrap();

    let meta_api_key_field = ProviderMeta {
        api_key_field: Some("ANTHROPIC_API_KEY".to_string()),
        ..Default::default()
    };
    db.save_provider(
        "claude",
        &provider(
            "fixture-claude-api-key",
            "Fixture Claude ApiKey",
            json!({
                "env": {
                    "ANTHROPIC_API_KEY": CLAUDE_KEY_ALT_FIELD,
                    "ANTHROPIC_BASE_URL": "https://fixture-claude-alt.example.com/v1"
                }
            }),
            Some(meta_api_key_field),
        ),
    )
    .unwrap();

    let meta_extra_env = ProviderMeta {
        usage_script: Some(
            serde_json::from_value(json!({
                "enabled": true,
                "language": "javascript",
                "code": "async function fetchUsage(context) { return { success: true, data: [] }; }",
                "apiKey": USAGE_SCRIPT_KEY,
                "baseUrl": "https://fixture-usage.example.com"
            }))
            .unwrap(),
        ),
        ..Default::default()
    };
    db.save_provider(
        "claude",
        &provider(
            "fixture-claude-extra-env",
            "Fixture Claude ExtraEnv",
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": CLAUDE_KEY_EXTRA_ENV,
                    "ANTHROPIC_BASE_URL": "https://fixture-claude-extra.example.com/v1",
                    "OPENROUTER_API_KEY": "sk-or-fixture-0007"
                }
            }),
            Some(meta_extra_env),
        ),
    )
    .unwrap();

    // --- Codex x2 (1 official + 1 third-party) ---
    db.save_provider(
        "codex",
        &provider(
            "fixture-codex-official",
            "Fixture Codex Official",
            json!({
                "auth": { "OPENAI_API_KEY": CODEX_OFFICIAL_KEY },
                "config": ""
            }),
            None,
        ),
    )
    .unwrap();

    let codex_3rd_config = "model_provider = \"fixture\"\n\
        [model_providers.fixture]\n\
        name = \"Fixture Third Party\"\n\
        base_url = \"https://fixture-codex.example.com/v1\"\n\
        experimental_bearer_token = \"sk-fixture-codex-3rd-0002\"\n\
        wire_api = \"responses\"\n";
    db.save_provider(
        "codex",
        &provider(
            "fixture-codex-3rd",
            "Fixture Codex Third Party",
            json!({
                "auth": {},
                "config": codex_3rd_config
            }),
            None,
        ),
    )
    .unwrap();

    // --- Pi x2 ---
    db.save_provider(
        "pi",
        &provider(
            "fixture-pi-one",
            "Fixture Pi One",
            json!({
                "baseUrl": "https://fixture-pi.example.com",
                "apiKey": PI_KEY_1,
                "model": "fixture-model-1"
            }),
            None,
        ),
    )
    .unwrap();
    db.save_provider(
        "pi",
        &provider(
            "fixture-pi-two",
            "Fixture Pi Two",
            json!({
                "baseUrl": "https://fixture-pi2.example.com",
                "apiKey": PI_KEY_2,
                "model": "fixture-model-2",
                "headers": { "Authorization": "Bearer sk-fixture-pi-header-0008" }
            }),
            None,
        ),
    )
    .unwrap();

    // Mark one current provider per app.
    db.set_current_provider("claude", "fixture-claude-auth-token")
        .unwrap();
    db.set_current_provider("codex", "fixture-codex-3rd")
        .unwrap();
    db.set_current_provider("pi", "fixture-pi-one").unwrap();

    // --- ~/.cc-switch/settings.json (WebDAV password) ---
    fs::write(
        home.join(".cc-switch").join("settings.json"),
        json!({
            "webdavSync": { "password": WEBDAV_PASSWORD, "enabled": false }
        })
        .to_string(),
    )
    .unwrap();

    // --- live files ---
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::write(
        home.join(".claude").join("settings.json"),
        json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": CLAUDE_KEY,
                "ANTHROPIC_BASE_URL": "https://fixture-claude.example.com/v1"
            },
            "model": "claude-sonnet-4-5"
        })
        .to_string(),
    )
    .unwrap();

    fs::create_dir_all(home.join(".codex")).unwrap();
    fs::write(home.join(".codex").join("config.toml"), codex_3rd_config).unwrap();

    let pi_agent = home.join(".pi").join("agent");
    fs::create_dir_all(&pi_agent).unwrap();
    fs::write(
        pi_agent.join("models.json"),
        json!({
            "providers": {
                "fixture-pi-one": {
                    "baseUrl": "https://fixture-pi.example.com",
                    "apiKey": PI_KEY_1,
                    "model": "fixture-model-1"
                },
                "fixture-pi-two": {
                    "baseUrl": "https://fixture-pi2.example.com",
                    "apiKey": PI_KEY_2,
                    "model": "fixture-model-2",
                    "headers": { "Authorization": "Bearer sk-fixture-pi-header-0008" }
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    // Standalone db copy for Phase 4 cold-start migration tests.
    let db_path = home.join(".cc-switch").join("cc-switch.db");
    let standalone = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("v18-plaintext.db");
    fs::copy(&db_path, &standalone).unwrap();

    println!("fixture home written to {}", home.display());
    println!("standalone db written to {}", standalone.display());
    let db_bytes = fs::read(&db_path).unwrap();
    for lit in fixture_secret_literals() {
        // WebDAV password lives in settings.json, not the database.
        if lit == WEBDAV_PASSWORD {
            continue;
        }
        assert!(
            db_bytes.windows(lit.len()).any(|w| w == lit.as_bytes()),
            "fixture db should contain plaintext literal {lit}"
        );
    }
}

/// Acceptance for Phase 0: the generated fixture loads cleanly on the current
/// (pre-slimdown) version, with all 7 providers readable.
#[test]
#[ignore = "fixture acceptance: run explicitly with `cargo test --test fixture_v18 -- --ignored`"]
fn load_v18_fixture_smoke() {
    let _guard = fixture_lock();
    let home = fixture_root();
    assert!(home.exists(), "run generate_v18_plaintext_fixture first");

    let prev = std::env::var_os("CC_SWITCH_TEST_HOME");
    std::env::set_var("CC_SWITCH_TEST_HOME", &home);
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match self.0.clone() {
                Some(v) => std::env::set_var("CC_SWITCH_TEST_HOME", v),
                None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
            }
        }
    }
    let _restore = Restore(prev);

    let db = Database::init().expect("fixture database should load");
    let claude = db.get_all_providers("claude").unwrap();
    let codex = db.get_all_providers("codex").unwrap();
    let pi = db.get_all_providers("pi").unwrap();
    assert_eq!(claude.len(), 3, "3 claude providers expected");
    assert_eq!(codex.len(), 2, "2 codex providers expected");
    assert_eq!(pi.len(), 2, "2 pi providers expected");
    assert_eq!(
        db.get_current_provider("claude").unwrap().as_deref(),
        Some("fixture-claude-auth-token")
    );
}
