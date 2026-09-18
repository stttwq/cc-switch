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

    /// 导入/还原后对 providers 逐行 extract，把残留明文收进凭据管理器。
    pub fn scrub_imported_plaintext(&self) -> Result<(), AppError> {
        let migrator = crate::secrets::migration::CredentialMigrator::new(
            self.db.as_ref(),
            self.secrets.as_ref(),
        );
        let runtime = tokio::runtime::Handle::try_current();
        let report = match runtime {
            Ok(handle) => {
                tokio::task::block_in_place(|| handle.block_on(migrator.run_migration()))?
            }
            Err(_) => {
                let rt = tokio::runtime::Runtime::new()
                    .map_err(|e| AppError::Config(format!("创建 tokio runtime 失败: {e}")))?;
                rt.block_on(migrator.run_migration())?
            }
        };
        if !report.errors.is_empty() {
            return Err(AppError::Config(format!(
                "导入后凭据清理失败: {}",
                report.errors.join("; ")
            )));
        }
        Ok(())
    }
}

/// Extract AppState from AppHandle
pub fn get_app_state(app: &tauri::AppHandle) -> Result<AppState, AppError> {
    app.try_state::<AppState>()
        .map(|state| state.inner().clone())
        .ok_or_else(|| AppError::Config("AppState not initialized".to_string()))
}
