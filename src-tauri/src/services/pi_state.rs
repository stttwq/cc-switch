//! Read-only Pi provider membership and global default reference.

use crate::error::AppError;
use crate::pi_config::{read_pi_native_defaults, read_pi_native_providers};
use crate::store::AppState;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PiCurrentState {
    pub enabled_provider_ids: Vec<String>,
    pub default_provider_id: Option<String>,
}

pub(crate) struct PiStateService;

impl PiStateService {
    pub(crate) fn current(_state: &AppState) -> Result<PiCurrentState, AppError> {
        // S1-5（施工方案 §4.2 S1-5）：不再等待 Pi 切换锁。这里只读 models.json
        // 与 Pi 全局 settings.json，两者都是原子写入（pi-native-contract-zh.md:43），
        // 读到的一定是某个完整版本；写路径（原生同步 / 增删改）由切换锁互斥，
        // 读侧无需共享锁。前端在 mutation 后本就会失效重取（invalidatePiProviderCaches
        // 已同时失效 piKeys.currentState 与 ["providers","pi"]，勿改坏）。
        // 历史：本函数曾被同步 Tauri 命令 get_pi_current_state 在主线程上
        // block_on 这把可能被原生同步长时间持有的锁，是「整个窗口冻结」的直接
        // 原因。命令层同时改为 async + spawn_blocking 双保险，结构上消除主线程
        // 阻塞；若将来发现确需与写操作互斥，改用 try_lock 读当前文件，绝不在
        // 主线程 block_on。
        let native = read_pi_native_providers()?;
        let enabled_provider_ids = native.keys().cloned().collect::<Vec<_>>();
        let default_provider_id = match read_pi_native_defaults() {
            Ok(defaults) => defaults.default_provider,
            Err(error) => {
                log::warn!("Failed to read Pi global default provider for advisory UI: {error}");
                None
            }
        };
        Ok(PiCurrentState {
            enabled_provider_ids,
            default_provider_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 锁键（服务实现去锁后仅本测试仍需持锁模拟慢路径）。
    const PI_APP_LOCK: &str = "pi";
    use crate::database::Database;
    use crate::pi_config::test_support::TestAgentDir;
    use serial_test::serial;
    use std::fs;
    use std::sync::Arc;

    #[test]
    #[serial]
    fn state_exposes_every_explicit_provider_node() {
        let _agent = TestAgentDir::new();
        let secrets = Arc::new(crate::secrets::InMemorySecretStore::new());
        let state = AppState::new(
            Arc::new(Database::memory().expect("create in-memory database")),
            secrets,
        );
        let models_path = crate::pi_config::get_pi_models_path().expect("models path");
        fs::create_dir_all(models_path.parent().expect("models directory"))
            .expect("create models directory");
        fs::write(
            models_path,
            r#"{
                "providers": {
                    "cc-switch-managed": {
                        "name": "Managed",
                        "baseUrl": "https://api.example.com/v1",
                        "api": "openai-completions",
                        "models": [{ "id": "model-a" }]
                    },
                    "native-oauth": {
                        "oauth": "example",
                        "baseUrl": "https://api.example.com/v1",
                        "api": "openai-completions",
                        "models": [{ "id": "model-b" }]
                    },
                    "anthropic": {},
                    "unsupported": {
                        "futureField": true
                    }
                }
            }"#,
        )
        .expect("write models");
        let settings_path = crate::pi_config::get_pi_settings_path().expect("settings path");
        fs::write(
            settings_path,
            r#"{"defaultProvider":"cc-switch-managed","defaultModel":"model-a"}"#,
        )
        .expect("write settings");

        let current = PiStateService::current(&state).expect("read state");
        assert_eq!(
            current.enabled_provider_ids,
            vec![
                "cc-switch-managed".to_string(),
                "native-oauth".to_string(),
                "anthropic".to_string(),
                "unsupported".to_string(),
            ]
        );
        assert_eq!(
            current.default_provider_id.as_deref(),
            Some("cc-switch-managed")
        );
    }

    #[test]
    #[serial]
    fn invalid_global_settings_do_not_hide_provider_membership() {
        let _agent = TestAgentDir::new();
        let secrets = Arc::new(crate::secrets::InMemorySecretStore::new());
        let state = AppState::new(
            Arc::new(Database::memory().expect("create in-memory database")),
            secrets,
        );
        let models_path = crate::pi_config::get_pi_models_path().expect("models path");
        fs::create_dir_all(models_path.parent().expect("models directory"))
            .expect("create models directory");
        fs::write(
            models_path,
            r#"{
                "providers": {
                    "cc-switch-managed": {
                        "name": "Managed",
                        "baseUrl": "https://api.example.com/v1",
                        "api": "openai-completions",
                        "models": [{ "id": "model-a" }]
                    }
                }
            }"#,
        )
        .expect("write models");
        let settings_path = crate::pi_config::get_pi_settings_path().expect("settings path");
        fs::write(settings_path, "[]").expect("write invalid settings");

        let current = PiStateService::current(&state).expect("read membership");
        assert_eq!(
            current.enabled_provider_ids,
            vec!["cc-switch-managed".to_string()]
        );
        assert_eq!(current.default_provider_id, None);
    }

    /// S1-5 验收（施工方案 §4.3）：另一线程持有 Pi 切换锁（模拟慢的原生同步）
    /// 时，`current` 不等待锁、照常返回结果。历史实现会 block_on 等锁，
    /// 冻结主线程。
    #[test]
    #[serial]
    fn current_does_not_block_while_pi_switch_lock_is_held() {
        use std::sync::mpsc;

        let _agent = TestAgentDir::new();
        let secrets = Arc::new(crate::secrets::InMemorySecretStore::new());
        let state = AppState::new(
            Arc::new(Database::memory().expect("create in-memory database")),
            secrets,
        );
        let models_path = crate::pi_config::get_pi_models_path().expect("models path");
        fs::create_dir_all(models_path.parent().expect("models directory"))
            .expect("create models directory");
        fs::write(
            models_path,
            r#"{"providers":{"cc-switch-managed":{"name":"Managed","baseUrl":"https://api.example.com/v1","api":"openai-completions","models":[{"id":"model-a"}]}}}"#,
        )
        .expect("write models");

        // 主测试线程持有 Pi 切换锁，等价于「原生同步正在进行」。
        let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(PI_APP_LOCK));

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = PiStateService::current(&state);
            let _ = tx.send(result);
        });
        let result = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("current 在锁被持有时不得阻塞（S1-5）");
        let current = result.expect("read state");
        assert_eq!(current.enabled_provider_ids, vec!["cc-switch-managed"]);
    }
}
