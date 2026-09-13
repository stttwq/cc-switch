use crate::database::Database;
use crate::services::switch_lock::SwitchLockManager;
use std::sync::Arc;

/// 全局应用状态
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database>,
    pub switch_locks: SwitchLockManager,
}

impl AppState {
    /// 创建新的应用状态
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            db,
            switch_locks: SwitchLockManager::new(),
        }
    }
}
