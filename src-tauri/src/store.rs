use crate::database::Database;
use crate::env_delivery::EnvSink;
use crate::error::AppError;
use crate::secrets::SecretStore;
use crate::services::switch_lock::SwitchLockManager;
use std::sync::{Arc, Mutex};
use tauri::Manager;

/// E2E（方案 2.4.2）：缓存口令派生出的 KEK，避免每次自动同步都重跑一次
/// Argon2id（~1s）。按盐命中——换远端（盐变）或改口令（命令显式 `invalidate`）
/// 才会重派生。KEK 本身以 `Zeroizing` 持有。
#[derive(Default)]
pub(crate) struct SyncKekCache {
    inner: Mutex<Option<CachedKek>>,
}

struct CachedKek {
    salt: Vec<u8>,
    kek: Arc<crate::services::sync_e2e::Kek>,
}

impl SyncKekCache {
    /// 盐命中则返回缓存 KEK，否则用口令派生并缓存。
    pub(crate) fn get_or_derive(
        &self,
        passphrase: &str,
        kdf: &crate::services::sync_e2e::KdfParams,
    ) -> Result<Arc<crate::services::sync_e2e::Kek>, AppError> {
        // 边界：缓存只按盐命中。若口令在别的设备被改并随凭据漫游过来、而远端盐未变，
        // 本机要等到重启或本地重设口令（`invalidate`）才会用新口令重派生。
        if let Some(cached) = self.locked().as_ref() {
            if cached.salt == kdf.salt {
                return Ok(cached.kek.clone());
            }
        }
        let kek = Arc::new(crate::services::sync_e2e::derive_kek(passphrase, kdf)?);
        *self.locked() = Some(CachedKek {
            salt: kdf.salt.clone(),
            kek: kek.clone(),
        });
        Ok(kek)
    }

    /// 口令变更后作废，下次同步强制重派生。
    pub(crate) fn invalidate(&self) {
        *self.locked() = None;
    }

    /// 取锁；毒化时恢复内部值而不是 panic（一次上游 panic 不该让后续同步全崩）。
    fn locked(&self) -> std::sync::MutexGuard<'_, Option<CachedKek>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// 全局应用状态
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database>,
    pub switch_locks: SwitchLockManager,
    pub secrets: Arc<dyn SecretStore>,
    /// §5.3.2：整个进程共用一个 sink 实例。每次调用现取会让同一轮投递里的
    /// `check_conflict` 与 `set` 看到不同对象，冲突检测/所有权等于不存在。
    pub env_sink: Arc<dyn EnvSink>,
    /// E2E：同步口令派生的 KEK 缓存（进程内，不落盘、不上传）。
    pub(crate) sync_kek: Arc<SyncKekCache>,
}

impl AppState {
    /// 创建新的应用状态
    pub fn new(db: Arc<Database>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            db,
            switch_locks: SwitchLockManager::new(),
            secrets,
            env_sink: crate::env_delivery::default_sink(),
            sync_kek: Arc::new(SyncKekCache::default()),
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
