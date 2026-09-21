#![allow(non_snake_case)]

use serde_json::{json, Value};
use std::sync::Arc;
use tauri::State;

use crate::commands::sync_support::{
    attach_warning, post_sync_warning_from_result, run_post_import_sync,
};
use crate::error::AppError;
use crate::services::webdav_sync as webdav_sync_service;
use crate::settings::{self, WebDavSyncSettings};
use crate::store::AppState;

fn persist_sync_error(settings: &mut WebDavSyncSettings, error: &AppError, source: &str) {
    settings.status.last_error = Some(error.to_string());
    settings.status.last_error_source = Some(source.to_string());
    let _ = settings::update_webdav_sync_status(settings.status.clone());
}

fn webdav_not_configured_error() -> String {
    AppError::localized(
        "webdav.sync.not_configured",
        "未配置 WebDAV 同步",
        "WebDAV sync is not configured.",
    )
    .to_string()
}

fn webdav_sync_disabled_error() -> String {
    AppError::localized(
        "webdav.sync.disabled",
        "WebDAV 同步未启用",
        "WebDAV sync is disabled.",
    )
    .to_string()
}

fn require_enabled_webdav_settings() -> Result<WebDavSyncSettings, String> {
    let settings = settings::get_webdav_sync_settings().ok_or_else(webdav_not_configured_error)?;
    if !settings.enabled {
        return Err(webdav_sync_disabled_error());
    }
    Ok(settings)
}

#[cfg(test)]
fn webdav_sync_mutex() -> &'static tokio::sync::Mutex<()> {
    webdav_sync_service::sync_mutex()
}

async fn run_with_webdav_lock<T, Fut>(operation: Fut) -> Result<T, AppError>
where
    Fut: std::future::Future<Output = Result<T, AppError>>,
{
    webdav_sync_service::run_with_sync_lock(operation).await
}

async fn run_download_with_webdav_lock<T, U, DownloadFut, Project, ProjectFut>(
    download: DownloadFut,
    project: Project,
) -> Result<U, AppError>
where
    DownloadFut: std::future::Future<Output = Result<T, AppError>>,
    Project: FnOnce(T) -> ProjectFut,
    ProjectFut: std::future::Future<Output = Result<U, AppError>>,
{
    run_with_webdav_lock(async {
        let result = {
            let _auto_sync_suppression =
                crate::services::webdav_auto_sync::AutoSyncSuppressionGuard::new();
            download.await?
        };
        project(result).await
    })
    .await
}

fn map_sync_result<T, F>(result: Result<T, AppError>, on_error: F) -> Result<T, String>
where
    F: FnOnce(&AppError),
{
    match result {
        Ok(value) => Ok(value),
        Err(err) => {
            on_error(&err);
            Err(err.to_string())
        }
    }
}

#[tauri::command]
pub async fn webdav_test_connection(
    state: State<'_, AppState>,
    settings: WebDavSyncSettings,
    password: Option<String>,
) -> Result<Value, String> {
    // 三态（§5.2.5）：Some(非空) = 用表单里刚输入、尚未保存的密码试连；否则读凭据管理器。
    let override_password = password.as_deref().filter(|p| !p.is_empty());
    webdav_sync_service::check_connection(&state.secrets, &settings, override_password)
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "success": true,
        "message": "WebDAV connection ok"
    }))
}

#[tauri::command]
pub async fn webdav_sync_upload(state: State<'_, AppState>) -> Result<Value, String> {
    let db = state.db.clone();
    let secrets = state.secrets.clone();
    let kek_cache = state.sync_kek.clone();
    let mut settings = require_enabled_webdav_settings()?;

    let result = run_with_webdav_lock(webdav_sync_service::upload(
        &db,
        &secrets,
        &mut settings,
        &kek_cache,
    ))
    .await;
    map_sync_result(result, |error| {
        persist_sync_error(&mut settings, error, "manual")
    })
}

#[tauri::command]
pub async fn webdav_sync_download(
    state: State<'_, AppState>,
    allow_rollback: Option<bool>,
) -> Result<Value, String> {
    let db = state.db.clone();
    let secrets = state.secrets.clone();
    let kek_cache = state.sync_kek.clone();
    let app_state_for_sync = state.inner().clone();
    let mut settings = require_enabled_webdav_settings()?;

    // Keep the derived live configuration refresh in the same global sync
    // operation. Otherwise another WebDAV/S3 restore can start after the DB
    // apply but before this snapshot has finished projecting its live files.
    let sync_result = run_download_with_webdav_lock(
        webdav_sync_service::download(
            &db,
            &secrets,
            &mut settings,
            &kek_cache,
            allow_rollback.unwrap_or(false),
        ),
        |result| async move {
            let post_sync_result = tauri::async_runtime::spawn_blocking(move || {
                // 远端遗留快照可能含明文密钥：先 scrub（extract→凭据管理器→回写剥离），
                // 再刷新派生的 live 配置。
                app_state_for_sync
                    .scrub_imported_plaintext()
                    .and_then(|_| run_post_import_sync(&app_state_for_sync))
            })
            .await
            .map_err(|e| e.to_string());
            Ok((result, post_sync_result))
        },
    )
    .await;
    let (mut result, post_sync_result) = map_sync_result(sync_result, |error| {
        persist_sync_error(&mut settings, error, "manual")
    })?;

    // Post-download sync is best-effort: snapshot restore has already succeeded.
    let warning = post_sync_warning_from_result(post_sync_result);
    if let Some(msg) = warning.as_ref() {
        log::warn!("[WebDAV] post-download sync warning: {msg}");
    }
    result = attach_warning(result, warning);

    Ok(result)
}

/// `webdav_sync_save_settings` 的可测核心（命令本身只做 `State` 解包与错误转字符串）。
///
/// 三态（§5.2.5）：`None` = 未触碰，保持现值；`Some("")` = 清空并删除凭据；`Some(v)` = 写入。
async fn save_webdav_settings(
    secrets: &Arc<dyn crate::secrets::SecretStore>,
    settings: WebDavSyncSettings,
    password: Option<&str>,
) -> Result<(), AppError> {
    crate::secrets::extract_webdav_password(secrets, password).await?;

    let existing = settings::get_webdav_sync_settings();
    let mut sync_settings = settings;

    // Preserve server-owned fields that the frontend does not manage
    if let Some(existing_settings) = existing {
        sync_settings.status = existing_settings.status;
        // e2e_enabled / allow_insecure 是设备级、由高级设置里的"端到端加密"区单独管理，
        // 基础连接表单不携带 → 保留现值，否则每次"保存连接"都会把它们悄悄清零。
        sync_settings.e2e_enabled = existing_settings.e2e_enabled;
        sync_settings.allow_insecure = existing_settings.allow_insecure;
    }

    sync_settings.normalize();
    sync_settings.validate()?;
    settings::set_webdav_sync_settings(Some(sync_settings))
}

#[tauri::command]
pub async fn webdav_sync_save_settings(
    state: State<'_, AppState>,
    settings: WebDavSyncSettings,
    password: Option<String>,
) -> Result<Value, String> {
    save_webdav_settings(&state.secrets, settings, password.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({ "success": true }))
}

#[tauri::command]
pub async fn webdav_sync_fetch_remote_info(state: State<'_, AppState>) -> Result<Value, String> {
    let secrets = state.secrets.clone();
    let settings = require_enabled_webdav_settings()?;
    let info = webdav_sync_service::fetch_remote_info(&secrets, &settings)
        .await
        .map_err(|e| e.to_string())?;
    Ok(info.unwrap_or(json!({ "empty": true })))
}

#[cfg(test)]
mod tests {
    use super::{
        map_sync_result, persist_sync_error, require_enabled_webdav_settings,
        run_download_with_webdav_lock, run_with_webdav_lock, save_webdav_settings,
        webdav_sync_mutex,
    };
    use crate::error::AppError;
    use crate::settings::{AppSettings, WebDavSyncSettings};
    use serial_test::serial;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn webdav_sync_mutex_is_singleton() {
        let a = webdav_sync_mutex() as *const _;
        let b = webdav_sync_mutex() as *const _;
        assert_eq!(a, b);
    }

    #[tokio::test]
    #[serial]
    async fn webdav_sync_mutex_serializes_concurrent_access() {
        let guard = webdav_sync_mutex().lock().await;
        let acquired = Arc::new(AtomicBool::new(false));
        let acquired_bg = Arc::clone(&acquired);

        let waiter = tokio::spawn(async move {
            let _inner_guard = webdav_sync_mutex().lock().await;
            acquired_bg.store(true, Ordering::SeqCst);
        });

        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(!acquired.load(Ordering::SeqCst));

        drop(guard);
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("background task should complete after lock release")
            .expect("background task should not panic");

        assert!(acquired.load(Ordering::SeqCst));
    }

    #[tokio::test]
    #[serial]
    async fn download_suppression_starts_after_webdav_lock_acquisition() {
        assert!(!crate::services::webdav_auto_sync::is_auto_sync_suppressed());
        let guard = webdav_sync_mutex().lock().await;
        let download_entered = AtomicBool::new(false);
        let projection_entered = AtomicBool::new(false);
        let download = run_download_with_webdav_lock(
            async {
                download_entered.store(true, Ordering::SeqCst);
                assert!(crate::services::webdav_auto_sync::is_auto_sync_suppressed());
                Ok::<(), AppError>(())
            },
            |_| async {
                projection_entered.store(true, Ordering::SeqCst);
                assert!(!crate::services::webdav_auto_sync::is_auto_sync_suppressed());
                Ok::<(), AppError>(())
            },
        );
        tokio::pin!(download);

        assert!(
            tokio::time::timeout(Duration::from_millis(40), download.as_mut())
                .await
                .is_err(),
            "download must wait while another sync operation holds the global lock"
        );
        assert!(!download_entered.load(Ordering::SeqCst));
        assert!(!projection_entered.load(Ordering::SeqCst));
        assert!(
            !crate::services::webdav_auto_sync::is_auto_sync_suppressed(),
            "local changes must remain observable while the download waits for the global lock"
        );

        drop(guard);
        tokio::time::timeout(Duration::from_secs(1), download.as_mut())
            .await
            .expect("download should start after lock release")
            .expect("download operation should complete");

        assert!(download_entered.load(Ordering::SeqCst));
        assert!(projection_entered.load(Ordering::SeqCst));
        assert!(!crate::services::webdav_auto_sync::is_auto_sync_suppressed());
    }

    #[tokio::test]
    #[serial]
    async fn map_sync_result_runs_error_handler_after_lock_release() {
        let result = run_with_webdav_lock(async {
            Err::<(), AppError>(AppError::Config("boom".to_string()))
        })
        .await;

        let mut lock_released = false;
        let mapped = map_sync_result(result, |_| {
            lock_released = webdav_sync_mutex().try_lock().is_ok();
        });

        assert!(mapped.is_err());
        assert!(lock_released);
    }

    #[test]
    #[serial]
    fn persist_sync_error_updates_status_without_overwriting_credentials() {
        let test_home = std::env::temp_dir().join("cc-switch-sync-error-status-test");
        let _ = std::fs::remove_dir_all(&test_home);
        std::fs::create_dir_all(&test_home).expect("create test home");
        std::env::set_var("CC_SWITCH_TEST_HOME", &test_home);

        crate::settings::update_settings(AppSettings::default()).expect("reset settings");
        let mut current = WebDavSyncSettings {
            enabled: true,
            base_url: "https://dav.example.com/dav/".to_string(),
            username: "alice".to_string(),
            remote_root: "cc-switch-sync".to_string(),
            profile: "default".to_string(),
            ..WebDavSyncSettings::default()
        };
        crate::settings::set_webdav_sync_settings(Some(current.clone()))
            .expect("seed webdav settings");

        persist_sync_error(
            &mut current,
            &crate::error::AppError::Config("boom".to_string()),
            "manual",
        );

        let after = crate::settings::get_webdav_sync_settings().expect("read webdav settings");
        assert_eq!(after.base_url, "https://dav.example.com/dav/");
        assert_eq!(after.username, "alice");
        assert_eq!(after.remote_root, "cc-switch-sync");
        assert_eq!(after.profile, "default");
        assert!(
            after
                .status
                .last_error
                .as_deref()
                .unwrap_or_default()
                .contains("boom"),
            "status error should be updated"
        );
        assert_eq!(after.status.last_error_source.as_deref(), Some("manual"));
    }

    #[test]
    #[serial]
    fn require_enabled_webdav_settings_rejects_disabled_config() {
        let test_home = std::env::temp_dir().join("cc-switch-sync-enabled-disabled-test");
        let _ = std::fs::remove_dir_all(&test_home);
        std::fs::create_dir_all(&test_home).expect("create test home");
        std::env::set_var("CC_SWITCH_TEST_HOME", &test_home);

        crate::settings::update_settings(AppSettings::default()).expect("reset settings");
        crate::settings::set_webdav_sync_settings(Some(WebDavSyncSettings {
            enabled: false,
            base_url: "https://dav.example.com/dav/".to_string(),
            username: "alice".to_string(),
            ..WebDavSyncSettings::default()
        }))
        .expect("seed disabled webdav settings");

        let err = require_enabled_webdav_settings().expect_err("disabled settings should fail");
        assert!(
            err.contains("disabled") || err.contains("未启用"),
            "unexpected error: {err}"
        );
    }

    #[test]
    #[serial]
    fn require_enabled_webdav_settings_returns_settings_when_enabled() {
        let test_home = std::env::temp_dir().join("cc-switch-sync-enabled-ok-test");
        let _ = std::fs::remove_dir_all(&test_home);
        std::fs::create_dir_all(&test_home).expect("create test home");
        std::env::set_var("CC_SWITCH_TEST_HOME", &test_home);

        crate::settings::update_settings(AppSettings::default()).expect("reset settings");
        crate::settings::set_webdav_sync_settings(Some(WebDavSyncSettings {
            enabled: true,
            base_url: "https://dav.example.com/dav/".to_string(),
            username: "alice".to_string(),
            ..WebDavSyncSettings::default()
        }))
        .expect("seed enabled webdav settings");

        let settings =
            require_enabled_webdav_settings().expect("enabled settings should be accepted");
        assert!(settings.enabled);
        assert_eq!(settings.base_url, "https://dav.example.com/dav/");
    }

    #[tokio::test]
    #[serial]
    async fn webdav_sync_save_settings_applies_password_three_state() {
        let test_home = std::env::temp_dir().join("cc-switch-webdav-extract-test");
        let _ = std::fs::remove_dir_all(&test_home);
        std::fs::create_dir_all(&test_home).expect("create test home");
        std::env::set_var("CC_SWITCH_TEST_HOME", &test_home);

        crate::settings::update_settings(AppSettings::default()).expect("reset settings");

        let secrets: Arc<dyn crate::secrets::SecretStore> =
            Arc::new(crate::secrets::InMemorySecretStore::new());

        let settings = || WebDavSyncSettings {
            enabled: true,
            base_url: "https://dav.example.com/dav/".to_string(),
            username: "alice".to_string(),
            remote_root: "cc-switch-sync".to_string(),
            profile: "default".to_string(),
            ..WebDavSyncSettings::default()
        };

        // P0-1 回归：走命令真正执行的那段代码（含入参三态），而不是绕过命令签名。
        // ① Some(v) → 写入
        save_webdav_settings(&secrets, settings(), Some("secret-password"))
            .await
            .expect("save should succeed");
        assert_eq!(
            crate::secrets::restore_webdav_password(&secrets)
                .await
                .expect("restore should succeed")
                .expect("password should be stored")
                .as_str(),
            "secret-password"
        );

        // ② None → 保持现值（不能被空值冲掉）
        save_webdav_settings(&secrets, settings(), None)
            .await
            .expect("save should succeed");
        assert!(crate::secrets::restore_webdav_password(&secrets)
            .await
            .expect("restore should succeed")
            .is_some());

        // ③ Some("") → 删除条目（清空密码框必须真的删掉）
        save_webdav_settings(&secrets, settings(), Some(""))
            .await
            .expect("save should succeed");
        assert!(crate::secrets::restore_webdav_password(&secrets)
            .await
            .expect("restore should succeed")
            .is_none());
    }

    #[tokio::test]
    #[serial]
    async fn webdav_sync_save_settings_applies_s3_three_state() {
        let test_home = std::env::temp_dir().join("cc-switch-s3-extract-test");
        let _ = std::fs::remove_dir_all(&test_home);
        std::fs::create_dir_all(&test_home).expect("create test home");
        std::env::set_var("CC_SWITCH_TEST_HOME", &test_home);

        let secrets: Arc<dyn crate::secrets::SecretStore> =
            Arc::new(crate::secrets::InMemorySecretStore::new());

        crate::secrets::extract_s3_credentials(&secrets, Some("AKIA-1"), Some("secret-1"))
            .await
            .expect("store should succeed");
        let (id, secret) = crate::secrets::restore_s3_credentials(&secrets)
            .await
            .expect("restore should succeed");
        assert_eq!(id.as_deref().map(|v| v.as_str()), Some("AKIA-1"));
        assert_eq!(secret.as_deref().map(|v| v.as_str()), Some("secret-1"));

        // None → 两条都保持；Some("") 只删被清空的那条
        crate::secrets::extract_s3_credentials(&secrets, None, Some(""))
            .await
            .expect("delete should succeed");
        let (id, secret) = crate::secrets::restore_s3_credentials(&secrets)
            .await
            .expect("restore should succeed");
        assert_eq!(id.as_deref().map(|v| v.as_str()), Some("AKIA-1"));
        assert!(secret.is_none());
    }
}
