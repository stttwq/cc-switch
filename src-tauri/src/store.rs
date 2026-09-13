use crate::database::Database;
use crate::error::AppError;
use crate::secrets::SecretStore;
use crate::services::switch_lock::SwitchLockManager;
use std::sync::Arc;
use tauri::Manager;

/// 全局应用状态
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database>,
    pub switch_locks: SwitchLockManager,
    pub secrets: Arc<dyn SecretStore>,
}

impl AppState {
    /// 创建新的应用状态
    pub fn new(db: Arc<Database>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            db,
            switch_locks: SwitchLockManager::new(),
            secrets,
        }
    }
}

/// Extract AppState from AppHandle
pub fn get_app_state(app: &tauri::AppHandle) -> Result<AppState, AppError> {
    app.try_state::<AppState>()
        .map(|state| state.inner().clone())
        .ok_or_else(|| AppError::Config("AppState not initialized".to_string()))
}
