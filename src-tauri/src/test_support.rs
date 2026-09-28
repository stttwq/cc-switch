//! S0（施工方案 §6）：跨模块共用的测试护栏。
//!
//! 只在 `cfg(test)` 下编译，任何内容不得进入生产二进制。
//! 凡是动到 `settings.json` 的测试，必须使用 [`TestHomeGuard`] /
//! [`OnePasswordBackendGuard`] 并标注 `#[serial]`（施工方案 §9-12）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// S0-1：update_hook 计数器（施工方案 §6 S0、§9-1）。
///
/// rusqlite 每个连接只允许挂一个 update_hook；[`HookCounts::install`] 会**替换**
/// `register_db_change_hook` 装上的自动同步通知钩子。测试连接是独立的内存库，
/// 替换不影响其他测试；hook 随连接存活，无需摘除。
///
/// 只记录动作类型与表名——仅结构信息，不含行数据（施工方案 §9-13）。
#[derive(Clone)]
pub(crate) struct HookCounts {
    inner: Arc<HookCountsInner>,
}

struct HookCountsInner {
    total: AtomicUsize,
    events: Mutex<Vec<(&'static str, String)>>,
}

impl HookCounts {
    /// 在该数据库连接上安装计数 hook（替换既有 hook）。
    pub(crate) fn install(db: &crate::database::Database) -> Self {
        let counts = Self {
            inner: Arc::new(HookCountsInner {
                total: AtomicUsize::new(0),
                events: Mutex::new(Vec::new()),
            }),
        };
        let hook = counts.clone();
        let conn = db.conn.lock().expect("lock db conn to install update_hook");
        conn.update_hook(Some(
            move |action: rusqlite::hooks::Action, _db: &str, table: &str, _row_id: i64| {
                let action_name = match action {
                    rusqlite::hooks::Action::SQLITE_INSERT => "insert",
                    rusqlite::hooks::Action::SQLITE_UPDATE => "update",
                    rusqlite::hooks::Action::SQLITE_DELETE => "delete",
                    _ => "other",
                };
                hook.inner.total.fetch_add(1, Ordering::SeqCst);
                if let Ok(mut events) = hook.inner.events.lock() {
                    events.push((action_name, table.to_string()));
                }
            },
        ));
        counts
    }

    /// 全部写入事件数。
    pub(crate) fn total(&self) -> usize {
        self.inner.total.load(Ordering::SeqCst)
    }

    /// 指定表的写入事件数。
    pub(crate) fn count_for_table(&self, table: &str) -> usize {
        self.inner
            .events
            .lock()
            .map(|events| events.iter().filter(|(_, t)| t == table).count())
            .unwrap_or(0)
    }

    /// 清零计数（保留 hook 安装状态）。
    pub(crate) fn reset(&self) {
        self.inner.total.store(0, Ordering::SeqCst);
        if let Ok(mut events) = self.inner.events.lock() {
            events.clear();
        }
    }
}

/// 隔离 `settings.json` 的测试家目录（S0；施工方案 §9-12）。
///
/// 设置 `CC_SWITCH_TEST_HOME` 指向临时目录，并预置 `.cc-switch/cc-switch.db`
/// 哨兵文件，防止 Windows 的 legacy-HOME 回退把配置目录锚回真实家目录
/// （与 `backup.rs` 测试模块内置的同名守卫语义一致）。使用时必须先于任何
/// `update_settings` 调用创建，且测试函数标注 `#[serial]`。
pub(crate) struct TestHomeGuard {
    previous_test_home: Option<std::ffi::OsString>,
    _temp_dir: tempfile::TempDir,
}

impl TestHomeGuard {
    pub(crate) fn new() -> Self {
        let temp_dir = tempfile::tempdir().expect("create isolated test home");
        let previous_test_home = std::env::var_os("CC_SWITCH_TEST_HOME");
        std::env::set_var("CC_SWITCH_TEST_HOME", temp_dir.path());
        let config_dir = temp_dir.path().join(".cc-switch");
        std::fs::create_dir_all(&config_dir).expect("create isolated config directory");
        std::fs::File::create(config_dir.join("cc-switch.db"))
            .expect("create isolated database sentinel");
        let resolved = crate::config::get_app_config_dir();
        assert!(
            resolved.starts_with(temp_dir.path()),
            "isolated test home resolved outside its temp directory: {}",
            resolved.display()
        );
        Self {
            previous_test_home,
            _temp_dir: temp_dir,
        }
    }
}

impl Drop for TestHomeGuard {
    fn drop(&mut self) {
        match self.previous_test_home.as_ref() {
            Some(previous) => std::env::set_var("CC_SWITCH_TEST_HOME", previous),
            None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
        }
    }
}

/// 把 `secret_backend` 临时切到 1Password 的守卫（S0）。
///
/// 必须在 [`TestHomeGuard`] 之后创建（写 settings.json 依赖测试家目录），
/// 配合 `#[serial]` 使用；drop 时无论测试成败都还原原值。
pub(crate) struct OnePasswordBackendGuard {
    previous: crate::settings::AppSettings,
}

impl OnePasswordBackendGuard {
    pub(crate) fn new() -> Self {
        let previous = crate::settings::get_settings();
        let mut next = previous.clone();
        next.secret_backend = Some("onepassword".to_string());
        crate::settings::update_settings(next).expect("set secret_backend=onepassword");
        Self { previous }
    }
}

impl Drop for OnePasswordBackendGuard {
    fn drop(&mut self) {
        let _ = crate::settings::update_settings(self.previous.clone());
    }
}

/// S4-1：设置本机 1Password 保险箱 id（`local_vault` 的来源）。
///
/// 依赖 [`OnePasswordBackendGuard`] 已经把 `secret_backend` 切到 1P；必须在
/// [`TestHomeGuard`] 之后调用，并配 `#[serial]`。
pub(crate) fn set_onepassword_vault(vault: &str) -> Result<(), crate::error::AppError> {
    let mut next = crate::settings::get_settings();
    let mut onepassword = next.onepassword.clone().unwrap_or_default();
    onepassword.vault = Some(vault.to_string());
    next.onepassword = Some(onepassword);
    crate::settings::update_settings(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;

    #[test]
    fn hook_counts_records_every_write() {
        let db = Database::memory().expect("memory db");
        let counts = HookCounts::install(&db);
        db.set_setting("s0-probe", "v1").expect("first write");
        db.set_setting("s0-probe", "v2").expect("second write");
        assert_eq!(counts.total(), 2, "update_hook 必须把每次写都记下来");
        assert_eq!(counts.count_for_table("settings"), 2);
        counts.reset();
        db.set_setting("s0-probe", "v3").expect("third write");
        assert_eq!(counts.total(), 1);
        assert_eq!(counts.count_for_table("settings"), 1);
    }
}
