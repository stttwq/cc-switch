//! S2 回归护栏（施工方案 §8）：用内存 SecretStore 里的固定密钥跑一遍
//! 迁移 / 新增 / 切换，断言日志输出缓冲区不含任何密钥字面量。

use std::sync::{Mutex, OnceLock};

use serde_json::json;

use cc_switch_lib::{AppType, MultiAppConfig, Provider, ProviderService};

#[path = "support.rs"]
mod support;
use support::{
    create_test_state_with_config, ensure_test_home, reset_test_fs, seed_secrets_from_db,
    test_mutex,
};

const CLAUDE_KEY: &str = "sk-fixture-log-claude-0001";
const SWITCH_KEY: &str = "sk-fixture-log-claude-0002";
const ADD_KEY: &str = "sk-fixture-log-claude-0003";

static LOG_LINES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

struct CaptureLogger;

impl log::Log for CaptureLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        LOG_LINES
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .push(format!("{}", record.args()));
    }
    fn flush(&self) {}
}

fn captured_logs() -> Vec<String> {
    LOG_LINES
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .clone()
}

fn contains_any(haystack: &str) -> bool {
    haystack.contains(CLAUDE_KEY) || haystack.contains(SWITCH_KEY) || haystack.contains(ADD_KEY)
}

#[test]
fn logs_never_contain_secret_values_across_migrate_add_switch() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let _home = ensure_test_home();

    let _ = log::set_boxed_logger(Box::new(CaptureLogger));
    log::set_max_level(log::LevelFilter::Trace);

    let mut config = MultiAppConfig::default();
    {
        let manager = config
            .get_manager_mut(&AppType::Claude)
            .expect("claude manager");
        manager.current = "first".to_string();
        manager.providers.insert(
            "first".to_string(),
            Provider::from_parts(
                "first".to_string(),
                "First".to_string(),
                json!({ "env": { "ANTHROPIC_AUTH_TOKEN": CLAUDE_KEY } }),
                None,
            ),
        );
        manager.providers.insert(
            "second".to_string(),
            Provider::from_parts(
                "second".to_string(),
                "Second".to_string(),
                json!({ "env": { "ANTHROPIC_AUTH_TOKEN": SWITCH_KEY } }),
                None,
            ),
        );
    }

    // 迁移（启动时自动）+ 幂等重跑
    let state =
        create_test_state_with_config(&config).expect("create test state and migrate plaintext");
    seed_secrets_from_db(&state);

    // 新增（表单 JSON 里带明文密钥，必须经过提取器）
    let added = Provider::from_parts(
        "third".to_string(),
        "Third".to_string(),
        json!({ "env": { "ANTHROPIC_AUTH_TOKEN": ADD_KEY } }),
        None,
    );
    ProviderService::add(&state, AppType::Claude, added, false).expect("add provider");

    // 切换（读凭据 + 投递环境变量 + 写 live）
    ProviderService::switch(&state, AppType::Claude, "second").expect("switch claude provider");

    let logs = captured_logs();
    assert!(!logs.is_empty(), "日志捕获器未生效，本测试将失去回归价值");
    for line in &logs {
        assert!(!contains_any(line), "日志行泄漏了密钥字面量: {line}");
    }
    let joined = logs.join("\n");
    assert!(!contains_any(&joined), "聚合日志输出中泄漏了密钥字面量");
}
