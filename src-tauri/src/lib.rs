mod acl;
mod app_config;
mod app_store;
mod auto_launch;
mod claude_mcp;
mod claude_plugin;
mod codex_config;
mod codex_history_migration;
mod codex_state_db;
mod commands;
mod config;
mod database;
mod env_delivery;
mod error;
mod init_status;
mod lightweight;
mod mcp;
mod panic_hook;
mod pi_config;
mod prompt;
mod prompt_files;
mod provider;
pub mod secrets;
/// 2.2 方案 P1：`ccs env <app>` 控制台 shim 的核心逻辑（供 `src/bin/ccs.rs` 调用）。
pub mod cli;
mod services;
mod session_manager;
mod settings;
mod store;

mod tray;

pub use app_config::{AppType, InstalledSkill, McpApps, McpServer, MultiAppConfig, SkillApps};
pub use codex_config::{
    extract_codex_experimental_bearer_token, get_codex_auth_path, get_codex_config_path,
    read_codex_live_settings, write_codex_live_atomic,
};
pub use commands::open_provider_terminal;
pub use commands::*;
pub use config::{get_claude_mcp_path, get_claude_settings_path, read_json_file};
pub use database::{Database, Profile};
// 环境变量投递的所有权登记与内存 sink：集成测试要据此断言 §5.3.3 的投递与撤销结果。
pub use env_delivery::{EnvSink, InMemoryEnvSink, ManagedEnvVars};
pub use error::AppError;
pub use mcp::{
    import_from_claude, import_from_codex, remove_server_from_claude, remove_server_from_codex,
    sync_enabled_to_claude, sync_enabled_to_codex, sync_single_server_to_claude,
    sync_single_server_to_codex,
};
pub use prompt::Prompt;
pub use provider::{Provider, ProviderMeta};
pub use services::{
    profile::{ProfilePayload, ProfileScope, ProfileService},
    provider::{reapply_current_codex_official_live, reapply_live_after_migration},
    skill::{migrate_skills_to_ssot, ImportSkillSelection},
    ConfigService, McpService, PromptService, ProviderService, SkillService,
};
pub use settings::{update_settings, AppSettings};
pub use store::AppState;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::{fmt, sync::Arc};
use tauri::tray::TrayIconBuilder;
use tauri::Manager;
use tauri::RunEvent;
use tauri_plugin_window_state::{AppHandleExt, StateFlags};

#[cfg(target_os = "windows")]
fn set_windows_app_user_model_id(app: &tauri::AppHandle) {
    let app_id = app.config().identifier.clone();
    let wide_app_id: Vec<u16> = app_id.encode_utf16().chain(std::iter::once(0)).collect();

    let result = unsafe {
        windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(wide_app_id.as_ptr())
    };

    if result < 0 {
        log::warn!("设置 Windows AppUserModelID 失败: 0x{result:08X}");
    } else {
        log::debug!("Windows AppUserModelID 已设置为 {app_id}");
    }
}

pub(crate) struct RedactedUrl<'a> {
    url: &'a str,
    known_secrets: &'a [String],
}

impl fmt::Display for RedactedUrl<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&redact_url_for_log_with_secrets(
            self.url,
            self.known_secrets,
        ))
    }
}

/// 为持有确切认证材料的调用方提供优先精确匹配、再启发式兜底的 URL 脱敏。
pub(crate) fn url_for_log_with_secrets<'a>(
    url: &'a str,
    known_secrets: &'a [String],
) -> RedactedUrl<'a> {
    RedactedUrl { url, known_secrets }
}

/// 已知密钥参与子串脱敏的最短长度：过短的值(如 "api")当作子串会误伤无关文本，
/// 所以只对足够长、几乎不可能是普通词的值做替换。
const MIN_KNOWN_SECRET_LEN: usize = 6;

/// 唯一的密钥脱敏原语：把字符串里出现的、我们确切握有的密钥值替换为 [REDACTED]。
/// 不做任何“看起来像密钥”的形状猜测——只隐藏已知值，天然收敛、不误伤正常路径。
pub(crate) fn redact_known_secrets(text: &str, known_secrets: &[String]) -> String {
    redact_known_secrets_with_min_length(text, known_secrets, MIN_KNOWN_SECRET_LEN)
}

/// Exact-value redaction for user-visible error text. Unlike URL logging, a
/// short known credential must still be hidden because the result reaches the
/// UI rather than a diagnostic-only path.
pub(crate) fn redact_known_secrets_strict(text: &str, known_secrets: &[String]) -> String {
    redact_known_secrets_with_min_length(text, known_secrets, 1)
}

fn redact_known_secrets_with_min_length(
    text: &str,
    known_secrets: &[String],
    minimum_chars: usize,
) -> String {
    let mut output = text.to_string();
    for secret in known_secrets {
        if secret.chars().count() >= minimum_chars {
            output = output.replace(secret.as_str(), "[REDACTED]");
        }
    }
    // S2：本次会话写入 / 投递过的密钥（含 CC_SWITCH_* 环境变量的值）自动参与脱敏，
    // 不依赖每个调用方记得把它们传进来。
    for secret in crate::secrets::scan::session_secret_snapshot() {
        if secret.chars().count() >= minimum_chars {
            output = output.replace(secret.as_str(), "[REDACTED]");
        }
    }
    output
}

/// 无 scheme 的裸 authority 形态(如 `user:pass@host/path`)剥掉 userinfo：
/// 仅当 `@` 出现在第一个 `/` 之前时才视为凭据。
fn strip_bare_userinfo(input: &str) -> &str {
    let authority_end = input.find('/').unwrap_or(input.len());
    match input[..authority_end].rfind('@') {
        Some(at) => &input[at + 1..],
        None => input,
    }
}

pub(crate) fn redact_url_for_log(url_str: &str) -> String {
    redact_url_for_log_with_secrets(url_str, &[])
}

/// 为日志脱敏 URL：剥掉 userinfo(user:pass@) 与整个 query/fragment，保留
/// scheme/host/port/path 供诊断(如 base_url 配错路径导致 404)，最后再抹掉已知密钥值。
pub(crate) fn redact_url_for_log_with_secrets(url_str: &str, known_secrets: &[String]) -> String {
    let scheme_relative = url_str.starts_with("//");
    let parsed = if scheme_relative {
        url::Url::parse(&format!("https:{url_str}"))
    } else {
        url::Url::parse(url_str)
    };

    let sanitized = match parsed {
        Ok(mut url) if url.has_host() => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            let rendered = url.as_str();
            if scheme_relative {
                rendered
                    .strip_prefix("https:")
                    .unwrap_or(rendered)
                    .to_string()
            } else {
                rendered.to_string()
            }
        }
        _ => {
            // 解析失败(相对路径、含裸 userinfo 的非法 URL 等)：丢掉 query/fragment，
            // 尽力剥掉 userinfo，其余原样保留。
            let without_tail = url_str.split(['?', '#']).next().unwrap_or(url_str);
            strip_bare_userinfo(without_tail).to_string()
        }
    };

    redact_known_secrets(&sanitized, known_secrets)
}

/// 只保留 `scheme://host:port`，丢掉 path/query/userinfo。用于我们手里没有任何
/// 已知密钥可脱敏 path 的场景——凭据可能整个内嵌在 base_url 的 path 里，此时
/// 记录 path 无法保证不泄漏，只能退回到 origin。
fn runtime_log_level_allows(level: log::Level, max_level: log::LevelFilter) -> bool {
    max_level.to_level().is_some_and(|maximum| level <= maximum)
}

/// 更新托盘菜单的Tauri命令
#[tauri::command]
async fn update_tray_menu(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<bool, String> {
    match tray::create_tray_menu(&app, state.inner()) {
        Ok(new_menu) => {
            if let Some(tray) = app.tray_by_id(tray::TRAY_ID) {
                tray.set_menu(Some(new_menu))
                    .map_err(|e| format!("更新托盘菜单失败: {e}"))?;
                return Ok(true);
            }
            Ok(false)
        }
        Err(err) => {
            log::error!("创建托盘菜单失败: {err}");
            Ok(false)
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 设置 panic hook，在应用崩溃时记录日志到 <app_config_dir>/crash.log（默认 ~/.cc-switch/crash.log）
    panic_hook::setup_panic_hook();

    let mut builder = tauri::Builder::default();

    #[cfg(windows)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            log::info!("=== Single Instance Callback Triggered ===");

            if crate::lightweight::is_lightweight_mode() {
                if let Err(e) = crate::lightweight::exit_lightweight_mode(app) {
                    log::error!("退出轻量模式重建窗口失败: {e}");
                }
            }

            // 单实例回调只负责聚焦已有窗口。
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }));
    }

    #[cfg(target_os = "windows")]
    {
        let startup_page_handled = AtomicBool::new(false);
        builder = builder.on_page_load(move |webview, payload| {
            if webview.label() == "main"
                && payload.event() == tauri::webview::PageLoadEvent::Finished
                && payload.url().scheme() != "about"
                && !startup_page_handled.swap(true, Ordering::Relaxed)
                && !crate::settings::get_settings().silent_startup
            {
                let _ = webview.window().show();
                log::info!("主页面加载完成，主窗口已显示");
            }
        });
    }

    let builder = builder
        // 拦截窗口关闭：根据设置决定是否最小化到托盘
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // 数据库版本过新的恢复模式下没有托盘可唤回，关闭即退出，避免应用隐身后台
                let in_db_recovery = crate::init_status::get_init_error()
                    .map(|p| p.kind.as_deref() == Some("db_version_too_new"))
                    .unwrap_or(false);
                if in_db_recovery {
                    api.prevent_close();
                    window.app_handle().exit(0);
                    return;
                }

                let settings = crate::settings::get_settings();

                if settings.minimize_to_tray_on_close {
                    api.prevent_close();
                    let _ = window.hide();
                    #[cfg(target_os = "windows")]
                    {
                        let _ = window.set_skip_taskbar(true);
                    }
                } else {
                    api.prevent_close();
                    window.app_handle().exit(0);
                }
            }
        })
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(window_state_flags())
                .build(),
        )
        .setup(|app| {
            // 预先刷新 Store 覆盖配置，确保后续路径读取正确（日志/数据库等）
            app_store::refresh_app_config_dir_override(app.handle());
            panic_hook::init_app_config_dir(crate::config::get_app_config_dir());

            // 初始化日志（输出到 <app_config_dir>/logs/cc-switch.log）
            {
                use tauri_plugin_log::{RotationStrategy, Target, TargetKind, TimezoneStrategy};

                let log_dir = panic_hook::get_log_dir();

                // 确保日志目录存在
                if let Err(e) = std::fs::create_dir_all(&log_dir) {
                    eprintln!("创建日志目录失败: {e}");
                }

                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        // 底层保留 Trace 能力，便于加载用户配置后动态调高级别。
                        // 插件注册后会立即把全局级别收紧到 Info，避免启动阶段全量 Trace。
                        .level(log::LevelFilter::Trace)
                        // plugin-log 的前端 command 会直达 logger，绕过 log 宏的全局
                        // max_level；在分发层补一次过滤，确保动态总开关同样约束前端日志。
                        .filter(|metadata| {
                            runtime_log_level_allows(metadata.level(), log::max_level())
                        })
                        .targets([
                            Target::new(TargetKind::Stdout),
                            Target::new(TargetKind::Folder {
                                path: log_dir,
                                file_name: Some("cc-switch".into()),
                            }),
                        ])
                        // KeepSome(4) 保留 4 个轮转归档，加上当前文件最多约 100 MiB。
                        // 轮转仅按大小触发；跨重启继续追加，不再丢失上一次运行的日志。
                        .rotation_strategy(RotationStrategy::KeepSome(4))
                        .max_file_size(20 * 1024 * 1024)
                        .timezone_strategy(TimezoneStrategy::UseLocal)
                        .build(),
                )?;

                // 用户配置存在数据库中，数据库尚未打开时使用保守的 Info 级别。
                log::set_max_level(log::LevelFilter::Info);
                log::info!("=== CC Switch v{} started ===", env!("CARGO_PKG_VERSION"));
            }

            // 首次读取覆盖路径时 logger 尚未可用；此处重放一次，
            // 让 Store 损坏或路径无效等启动警告能够真正落盘。
            let _ = app_store::refresh_app_config_dir_override(app.handle());

            #[cfg(target_os = "windows")]
            set_windows_app_user_model_id(app.handle());

            // 初始化数据库
            let app_config_dir = crate::config::get_app_config_dir();
            let db_path = app_config_dir.join("cc-switch.db");
            let json_path = app_config_dir.join("config.json");

            // 检查是否需要从 config.json 迁移到 SQLite
            let has_json = json_path.exists();
            let has_db = db_path.exists();

            // 如果需要迁移，先验证 config.json 是否可以加载（在创建数据库之前）
            // 这样如果加载失败用户选择退出，数据库文件还没被创建，下次可以正常重试
            let migration_config = if !has_db && has_json {
                log::info!("检测到旧版配置文件，验证配置文件...");

                // 循环：支持用户重试加载配置文件
                loop {
                    match crate::app_config::MultiAppConfig::load() {
                        Ok(config) => {
                            log::info!("✓ 配置文件加载成功");
                            break Some(config);
                        }
                        Err(e) => {
                            log::error!("加载旧配置文件失败: {e}");
                            // 弹出系统对话框让用户选择
                            if !show_migration_error_dialog(app.handle(), &e.to_string()) {
                                // 用户选择退出（此时数据库还没创建，下次启动可以重试）
                                log::info!("用户选择退出程序");
                                std::process::exit(1);
                            }
                            // 用户选择重试，继续循环
                            log::info!("用户选择重试加载配置文件");
                        }
                    }
                }
            } else {
                None
            };

            // 现在创建数据库（包含 Schema 迁移）
            //
            // 说明：从 v3.8.* 升级的用户通常会走到这里的 SQLite schema 迁移，
            // 若迁移失败（数据库损坏/权限不足/user_version 过新等），需要给用户明确提示，
            // 否则表现可能只是“应用打不开/闪退”。
            //
            // 预检：数据库版本过新时，必须先于任何 schema 写操作（create_tables 内含
            // DROP/ALTER 等 DDL）进入恢复界面，避免旧应用对读不懂的更新版 DB 落写。
            match crate::database::Database::stored_user_version_exceeds_supported(&db_path) {
                Ok(Some(version)) => {
                    log::warn!("数据库版本过新（v{version}），引导用户在应用内升级应用");
                    crate::init_status::set_init_error(crate::init_status::InitErrorPayload {
                        path: db_path.display().to_string(),
                        error: format!(
                            "数据库版本过新（{version}），当前应用仅支持 {}，请升级应用后再尝试。",
                            crate::database::SCHEMA_VERSION
                        ),
                        kind: Some("db_version_too_new".to_string()),
                        db_version: Some(version),
                        supported_version: Some(crate::database::SCHEMA_VERSION),
                    });
                    // 主窗口默认 visible:false，恢复界面必须强制显示
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                    return Ok(());
                }
                Ok(None) => {}
                Err(e) => {
                    log::warn!("预检数据库版本失败，继续正常初始化流程: {e}");
                }
            }

            let db = loop {
                match crate::database::Database::init() {
                    Ok(db) => break Arc::new(db),
                    Err(e) => {
                        log::error!("Failed to init database: {e}");

                        if !show_database_init_error_dialog(app.handle(), &db_path, &e.to_string())
                        {
                            log::info!("用户选择退出程序");
                            std::process::exit(1);
                        }

                        log::info!("用户选择重试初始化数据库");
                    }
                }
            };

            // T-4（2.1 方案 4.2.4）：启动时 PRAGMA quick_check。失败则给"从最近备份
            // 恢复 / 退出"；恢复走离线文件顶替（损坏主库留证为 cc-switch.corrupt-*.db），
            // 完成后退出重启。不启用 WAL 的理由见 Database::init 的 busy_timeout 注释。
            if let Err(e) = db.startup_quick_check() {
                log::error!("启动完整性检查（quick_check）失败: {e}");
                if show_db_quick_check_failed_dialog(app.handle(), &e.to_string()) {
                    restore_db_from_latest_backup_at_startup(app.handle());
                }
                log::info!("启动完整性检查未通过，退出程序");
                std::process::exit(1);
            }

            // S-10（方案 4.2.3 / D2）：配置目录此时已存在，首次启动把 DACL 收紧为
            // 当前用户 + SYSTEM + Administrators。幂等（标记文件），失败只 warn 不阻断。
            acl::ensure_config_dir_acl_tightened(&crate::config::get_app_config_dir());

            // E2E-5 升级豁免：http 端点默认拒会打断存量用户，首次启动把升级前就配好
            // 的 http 远端预置 allow_insecure=true。必须在自动同步 worker 启动前跑。
            match crate::settings::grandfather_existing_insecure_http() {
                Ok(true) => log::info!("已为升级前配置的 http 同步远端豁免不安全连接"),
                Ok(false) => {}
                Err(e) => log::warn!("http 升级豁免迁移失败: {e}"),
            }

            // 数据库可用后立即应用持久化日志级别，避免后续服务初始化
            // 继续使用启动阶段的 Info 回退。损坏配置显式 fail-closed 到 Info。
            match db.get_log_config() {
                Ok(log_config) => {
                    log::set_max_level(log_config.to_level_filter());
                    log::info!(
                        "已加载日志配置: enabled={}, level={}",
                        log_config.enabled,
                        log_config.level
                    );
                }
                Err(e) => {
                    log::set_max_level(log::LevelFilter::Info);
                    log::warn!("读取日志配置失败，已回退到 info: {e}");
                }
            }

            // 如果有预加载的配置，执行迁移
            if let Some(config) = migration_config {
                log::info!("开始执行数据迁移...");

                match db.migrate_from_json(&config) {
                    Ok(_) => {
                        log::info!("✓ 配置迁移成功");
                        // 标记迁移成功，供前端显示 Toast
                        crate::init_status::set_migration_success();
                        // 归档旧配置文件（重命名而非删除，便于用户恢复）
                        if let Err(e) = std::fs::remove_file(&json_path) {
                            log::warn!("删除已迁移的旧配置文件失败: {e}");
                        } else {
                            log::info!("✓ 已删除旧 config.json，避免明文残留");
                        }
                    }
                    Err(e) => {
                        // 配置加载成功但迁移失败的情况极少（磁盘满等），仅记录日志
                        log::error!("配置迁移失败: {e}，将从现有配置导入");
                    }
                }
            }

            let secrets: Arc<dyn crate::secrets::SecretStore> = loop {
                let store = Arc::new(match crate::secrets::WindowsSecretStore::new() {
                    Ok(store) => store,
                    Err(e) => {
                        log::error!("创建凭据存储失败: {e}");
                        if !show_secrets_probe_error_dialog(app.handle(), &e.to_string()) {
                            log::info!("用户选择退出程序");
                            std::process::exit(1);
                        }
                        continue;
                    }
                });
                let probe_result = match tokio::runtime::Runtime::new() {
                    Ok(rt) => rt.block_on(crate::secrets::SecretStore::probe(&*store)),
                    Err(e) => Err(AppError::Config(format!("创建 tokio runtime 失败: {e}"))),
                };
                match probe_result {
                    Ok(()) => break store,
                    Err(e) => {
                        log::error!("凭据管理器自检失败: {e}");
                        if !show_secrets_probe_error_dialog(app.handle(), &e.to_string()) {
                            log::info!("用户选择退出程序");
                            std::process::exit(1);
                        }
                    }
                }
            };
            let app_state = AppState::new(db, secrets);

            // §6.1：DB init 只做纯 SQL，凭据迁移在 AppState::new（含 probe）之后
            // 用同一份已探测通过的 store 触发；§6.3 的明文残留清理紧随其后。
            // 失败按 §6.6 走「重试 / 退出」阻断对话框，不提供跳过。
            loop {
                match app_state
                    .db
                    .run_credential_migration_if_pending(app_state.secrets.as_ref())
                {
                    Ok(true) => {
                        crate::secrets::cleanup::cleanup_auto_deletable_plaintext();
                        break;
                    }
                    Ok(false) => break,
                    Err(e) => {
                        log::error!("凭据迁移失败: {e}");
                        if !show_secrets_probe_error_dialog(app.handle(), &e.to_string()) {
                            log::info!("用户选择退出程序");
                            std::process::exit(1);
                        }
                    }
                }
            }

            // settings.json 里的 WebDAV / S3 明文残留：无条件扫一次（幂等 no-op）。
            // 已清过 pending 标志的老用户不会再走上面的迁移批次，靠这条收掉明文。
            if let Ok(rt) = tokio::runtime::Runtime::new() {
                if let Err(e) = rt.block_on(crate::secrets::migration::sweep_plaintext_app_settings(
                    &app_state.db,
                    app_state.secrets.as_ref(),
                )) {
                    log::warn!("settings.json 明文凭据清扫失败: {e}");
                }
            }

            // 历史条目名找回：只装过中间开发版的机器上，凭据存在 `<user>.<规范名>`
            // 下，规范名查不到就会误报「需要密钥」。必须在 live 重写与投递之前做，
            // 否则那一步读不到凭据。跑完记一次标志，后续启动不再扫。
            if app_state
                .db
                .get_setting("legacy_secret_recovery")
                .ok()
                .flatten()
                .as_deref()
                != Some("1")
            {
                if let Ok(rt) = tokio::runtime::Runtime::new() {
                    match rt.block_on(crate::secrets::legacy::recover_legacy_named_secrets(
                        &app_state.db,
                        app_state.secrets.as_ref(),
                    )) {
                        Ok(out) => {
                            log::info!(
                                "凭据条目名找回完成: 搬迁 {}、清理旧名副本 {}、补登记 {}",
                                out.moved,
                                out.pruned,
                                out.registered
                            );
                            if let Err(e) = app_state.db.set_setting("legacy_secret_recovery", "1") {
                                log::warn!("写入 legacy_secret_recovery 标志失败: {e}");
                            }
                        }
                        Err(e) => log::warn!("凭据条目名找回失败（不影响本次启动）: {e}"),
                    }
                }
            }

            match app_state.db.get_setting("live_reapply_pending") {
                Ok(Some(flag)) if flag == "1" => {
                    log::info!("检测到 live_reapply_pending=1，开始重写 live 并投递环境变量");
                    if let Err(e) =
                        crate::services::provider::reapply_live_after_migration(&app_state)
                    {
                        log::warn!("live 重写批次异常: {e}");
                    }
                }
                Ok(_) => {}
                Err(e) => log::warn!("读取 live_reapply_pending 失败: {e}"),
            }

            // ============================================================
            // 按表独立判断的导入逻辑（各类数据独立检查，互不影响）
            // ============================================================

            // 1. 初始化默认 Skills 仓库（已有内置检查：表非空则跳过）
            match app_state.db.init_default_skill_repos() {
                Ok(count) if count > 0 => {
                    log::info!("✓ Initialized {count} default skill repositories");
                }
                Ok(_) => {} // 表非空，静默跳过
                Err(e) => log::warn!("✗ Failed to initialize default skill repos: {e}"),
            }

            // 1.1. Skills 统一管理迁移：当数据库迁移到 v3 结构后，自动从各应用目录导入到 SSOT
            // 触发条件由 schema 迁移设置 settings.skills_ssot_migration_pending = true 控制。
            match app_state.db.get_setting("skills_ssot_migration_pending") {
                Ok(Some(flag)) if flag == "true" || flag == "1" => {
                    // 安全保护：如果用户已经有 v3 结构的 Skills 数据，就不要自动清空重建。
                    let has_existing = app_state
                        .db
                        .get_all_installed_skills()
                        .map(|skills| !skills.is_empty())
                        .unwrap_or(false);

                    if has_existing {
                        log::info!(
                            "Detected skills_ssot_migration_pending but skills table not empty; skipping auto import."
                        );
                        let _ = app_state
                            .db
                            .set_setting("skills_ssot_migration_pending", "false");
                    } else {
                        match crate::services::skill::migrate_skills_to_ssot(&app_state.db) {
                            Ok(count) => {
                                log::info!("✓ Auto imported {count} skill(s) into SSOT");
                                if count > 0 {
                                    crate::init_status::set_skills_migration_result(count);
                                }
                                let _ = app_state
                                    .db
                                    .set_setting("skills_ssot_migration_pending", "false");
                            }
                            Err(e) => {
                                log::warn!("✗ Failed to auto import legacy skills to SSOT: {e}");
                                crate::init_status::set_skills_migration_error(e.to_string());
                                // 保留 pending 标志，方便下次启动重试
                            }
                        }
                    }
                }
                Ok(_) => {} // 未开启迁移标志，静默跳过
                Err(e) => log::warn!("✗ Failed to read skills migration flag: {e}"),
            }

            // 1.5. 自动导入 live 配置 + seed 官方预设供应商（Claude / Codex / Gemini）
            //
            // 先 import 后 seed 是有意为之：先把用户手动配置的 settings.json / auth.json / .env
            // 落成 "default" provider 设为 current，再追加官方预设（is_current=false）。
            // 这样用户切到官方预设时，回填机制会保护原 live 配置不丢失。
            //
            // 捕获首次运行快照：所有全新装用户都会看到欢迎弹窗介绍 CC Switch 的工作方式。
            // 读失败时默认不弹，宁可漏弹也不要因为故障打扰用户。
            let first_run_already_confirmed = crate::settings::get_settings()
                .first_run_notice_confirmed
                .unwrap_or(false);
            let fresh_install_at_startup =
                app_state.db.is_providers_empty().unwrap_or(false);

            for app_type in
                crate::app_config::AppType::all().filter(|t| !t.is_additive_mode())
            {
                if !crate::services::provider::should_import_default_config_on_startup(
                    &app_state,
                    &app_type,
                )
                .unwrap_or(false)
                {
                    log::debug!(
                        "○ {} already has providers; live import skipped",
                        app_type.as_str()
                    );
                    continue;
                }

                match crate::services::provider::import_default_config(
                    &app_state,
                    app_type.clone(),
                ) {
                    Ok(true) => log::info!(
                        "✓ Imported live config for {} as default provider",
                        app_type.as_str()
                    ),
                    Ok(false) => log::debug!(
                        "○ {} already has providers; live import skipped",
                        app_type.as_str()
                    ),
                    Err(e) => log::debug!(
                        "○ No live config to import for {}: {e}",
                        app_type.as_str()
                    ),
                }
            }

            match app_state.db.init_default_official_providers() {
                Ok(count) if count > 0 => {
                    log::info!("✓ Seeded {count} official provider(s)");
                }
                Ok(_) => {}
                Err(e) => log::warn!("✗ Failed to seed official providers: {e}"),
            }

            {
                let db_for_codex_history_migration = app_state.db.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    match crate::codex_history_migration::maybe_migrate_codex_third_party_history_provider_bucket(
                        &db_for_codex_history_migration,
                    ) {
                        Ok(outcome) => {
                            if let Some(reason) = outcome.skipped_reason {
                                log::debug!("○ Codex history provider bucket migration skipped: {reason}");
                            } else {
                                log::info!(
                                    "✓ Codex history provider bucket migration completed: sources={}, jsonl_files={}, state_rows={}",
                                    outcome.source_provider_ids.len(),
                                    outcome.migrated_jsonl_files,
                                    outcome.migrated_state_rows
                                );
                            }
                        }
                        Err(e) => {
                            log::warn!("✗ Codex history provider bucket migration failed: {e}");
                        }
                    }

                    match crate::codex_history_migration::maybe_migrate_codex_provider_template_bucket(
                        &db_for_codex_history_migration,
                    ) {
                        Ok(outcome) => {
                            if let Some(reason) = outcome.skipped_reason {
                                log::debug!("○ Codex provider template bucket migration skipped: {reason}");
                            } else if !outcome.migrated_provider_ids.is_empty() {
                                log::info!(
                                    "✓ Codex provider template bucket migration completed: providers={}",
                                    outcome.migrated_provider_ids.len()
                                );
                            }
                        }
                        Err(e) => {
                            log::warn!("✗ Codex provider template bucket migration failed: {e}");
                        }
                    }

                    // 统一会话开关的官方历史迁移：开关开启但上次未完成（如文件被占用
                    // 中途失败）时在启动期重试；函数内部自门控，开关关闭时直接跳过。
                    match crate::codex_history_migration::maybe_migrate_codex_official_history_to_unified_bucket() {
                        Ok(outcome) => {
                            if let Some(reason) = outcome.skipped_reason {
                                log::debug!("○ Codex official history unify migration skipped: {reason}");
                            } else {
                                log::info!(
                                    "✓ Codex official history unify migration completed: jsonl_files={}, state_rows={}",
                                    outcome.migrated_jsonl_files,
                                    outcome.migrated_state_rows
                                );
                            }
                        }
                        Err(e) => {
                            log::warn!("✗ Codex official history unify migration failed: {e}");
                        }
                    }
                });
            }

            // 老用户 / 已确认的路径由 `fresh_install_at_startup` 自行拦截，这里不做写入。
            // 字段只由前端在用户点击"我知道了"时 save_settings 回写，语义是"用户显式确认过"。
            if !first_run_already_confirmed && fresh_install_at_startup {
                log::info!("✓ First-run welcome notice pending");
            }

            // 1.6. 自动同步累加模式应用的原生 providers 到数据库
            //
            // additive 模式的 import 函数按 id 幂等——
            // 新 id 执行导入，已有 id 则更新 settings 和 display name，所以每次
            // 启动都跑是安全的：既保证新装用户开箱可见 live 中的供应商，也让外部
            // 修改的 live 文件能在重启后同步到数据库（与之前依赖前端"导入当前配置"
            // 按钮手动触发不同）。
            //
            // 底层 read_*_config 在文件不存在时返回默认空配置，因此新装且无
            // live 文件的用户走 Ok(0) 路径，不会产生错误日志噪音。
            match crate::services::provider::import_pi_providers_from_live(&app_state) {
                Ok(count) if count > 0 => {
                    log::info!("✓ Synced {count} Pi provider(s) from native config");
                }
                Ok(_) => log::debug!("○ No Pi provider changes from native config"),
                Err(e) => log::warn!("✗ Failed to import Pi providers: {e}"),
            }
            if let Ok(native) = crate::pi_config::read_pi_native_providers() {
                for id in native.keys() {
                    if let Ok(Some(provider)) =
                        app_state.db.get_provider_by_id(id, "pi")
                    {
                        let mut dummy = crate::services::SwitchResult::default();
                        if let Err(e) = crate::services::provider::ProviderService::deliver_env_credentials_pub(
                            &app_state,
                            &crate::app_config::AppType::Pi,
                            &provider,
                            &mut dummy,
                        ) {
                            log::warn!("✗ Failed to deliver Pi env for {id}: {e}");
                        }
                    }
                }
            }

            // 3. 导入 MCP 服务器配置（表空时触发）
            if app_state.db.is_mcp_table_empty().unwrap_or(false) {
                log::info!("MCP table empty, importing from live configurations...");

                match crate::services::mcp::McpService::import_from_claude(&app_state) {
                    Ok(count) if count > 0 => {
                        log::info!("✓ Imported {count} MCP server(s) from Claude");
                    }
                    Ok(_) => log::debug!("○ No Claude MCP servers found to import"),
                    Err(e) => log::warn!("✗ Failed to import Claude MCP: {e}"),
                }

                match crate::services::mcp::McpService::import_from_codex(&app_state) {
                    Ok(count) if count > 0 => {
                        log::info!("✓ Imported {count} MCP server(s) from Codex");
                    }
                    Ok(_) => log::debug!("○ No Codex MCP servers found to import"),
                    Err(e) => log::warn!("✗ Failed to import Codex MCP: {e}"),
                }
            }

            // 4. 导入提示词文件（表空时触发）
            if app_state.db.is_prompts_table_empty().unwrap_or(false) {
                log::info!("Prompts table empty, importing from live configurations...");

                for app in [
                    crate::app_config::AppType::Claude,
                    crate::app_config::AppType::Codex,
                    crate::app_config::AppType::Pi,
                ] {
                    match crate::services::prompt::PromptService::import_from_file_on_first_launch(
                        &app_state,
                        app.clone(),
                    ) {
                        Ok(count) if count > 0 => {
                            log::info!("✓ Imported {count} prompt(s) for {}", app.as_str());
                        }
                        Ok(_) => log::debug!("○ No prompt file found for {}", app.as_str()),
                        Err(e) => log::warn!("✗ Failed to import prompt for {}: {e}", app.as_str()),
                    }
                }
            }

            // 迁移旧的 app_config_dir 配置到 Store
            if let Err(e) = app_store::migrate_app_config_dir_from_settings(app.handle()) {
                log::warn!("迁移 app_config_dir 失败: {e}");
            }

            // 启动阶段不再无条件保存,避免意外覆盖用户配置。

            // 创建动态托盘菜单
            let menu = tray::create_tray_menu(app.handle(), &app_state)?;

            // 构建托盘
            let mut tray_builder = TrayIconBuilder::with_id(tray::TRAY_ID)
                .tooltip("CC Switch") // 鼠标悬停提示
                .on_tray_icon_event(|_tray, event| log::debug!("unhandled event {event:?}"))
                .menu(&menu)
                .on_menu_event(|app, event| {
                    tray::handle_tray_menu_event(app, &event.id.0);
                })
                .show_menu_on_left_click(true);

            if let Some(icon) = app.default_window_icon() {
                tray_builder = tray_builder.icon(icon.clone());
            } else {
                log::warn!("Failed to get default window icon for tray");
            }

            let _tray = tray_builder.build(app)?;
            crate::services::webdav_auto_sync::start_worker(
                app_state.db.clone(),
                app.handle().clone(),
            );
            crate::services::s3_auto_sync::start_worker(
                app_state.db.clone(),
                app.handle().clone(),
            );
            // 将同一个实例注入到全局状态，避免重复创建导致的不一致
            app.manage(app_state);

            // 初始化 SkillService
            let skill_service = SkillService::new();
            app.manage(commands::skill::SkillServiceState(Arc::new(skill_service)));

            // 初始化全局出站代理 HTTP 客户端
            {
                let db = &app.state::<AppState>().db;
                let proxy_url = db.get_global_proxy_url().ok().flatten();

                if let Err(e) = crate::services::http_client::init(proxy_url.as_deref()) {
                    log::error!(
                        "[GlobalProxy] [GP-005] Failed to initialize with saved config: {e}"
                    );

                    // 清除无效的代理配置
                    if proxy_url.is_some() {
                        log::warn!(
                            "[GlobalProxy] [GP-006] Clearing invalid proxy config from database"
                        );
                        if let Err(clear_err) = db.set_global_proxy_url(None) {
                            log::error!(
                                "[GlobalProxy] [GP-007] Failed to clear invalid config: {clear_err}"
                            );
                        }
                    }

                    // 使用直连模式重新初始化
                    if let Err(fallback_err) = crate::services::http_client::init(None) {
                        log::error!(
                            "[GlobalProxy] [GP-008] Failed to initialize direct connection: {fallback_err}"
                        );
                    }
                }
            }

            // 异常退出恢复
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = app_handle.state::<AppState>();

                initialize_common_config_snippets(&state);

                // Periodic backup check (on startup)
                if let Err(e) = state.db.periodic_backup_if_needed() {
                    log::warn!("Periodic backup failed on startup: {e}");
                }

                // Periodic maintenance timer: run once per day while the app is running
                let db_for_timer = state.db.clone();
                tauri::async_runtime::spawn(async move {
                    const PERIODIC_MAINTENANCE_INTERVAL_SECS: u64 = 24 * 60 * 60;
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                        PERIODIC_MAINTENANCE_INTERVAL_SECS,
                    ));
                    interval.tick().await; // skip immediate first tick (already checked above)
                    loop {
                        interval.tick().await;
                        if let Err(e) = db_for_timer.periodic_backup_if_needed() {
                            log::warn!("Periodic maintenance timer failed: {e}");
                        }
                    }
                });
            });

            // 静默启动：根据设置决定是否显示主窗口
            let settings = crate::settings::get_settings();
            if let Some(window) = app.get_webview_window("main") {
                if settings.silent_startup {
                    // 静默启动模式：保持窗口隐藏
                    let _ = window.hide();
                    #[cfg(target_os = "windows")]
                    let _ = window.set_skip_taskbar(true);
                    log::info!("静默启动模式：主窗口已隐藏");
                } else {
                    // 正常启动模式：显示窗口
                    #[cfg(not(target_os = "windows"))]
                    let _ = window.show();
                    #[cfg(target_os = "windows")]
                    log::info!("正常启动模式：等待主页面加载完成后显示主窗口");
                    #[cfg(not(target_os = "windows"))]
                    log::info!("正常启动模式：主窗口已显示");
                }
            }


            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_providers,
            commands::get_current_provider,
            commands::reveal_provider_secret,
            commands::add_provider,
            commands::update_provider,
            commands::delete_provider,
            commands::remove_provider_from_live_config,
            commands::switch_provider,
            commands::import_default_config,
            commands::ensure_codex_official_provider,
            commands::get_claude_config_status,
            commands::get_config_status,
            commands::get_claude_code_config_path,
            commands::get_config_dir,
            commands::open_config_folder,
            commands::pick_directory,
            commands::open_external,
            commands::get_init_error,
            commands::get_migration_result,
            commands::get_secrets_migration_report,
            commands::confirm_secrets_migration,
            commands::list_plaintext_backups,
            commands::delete_plaintext_backups,
            commands::get_live_reapply_status,
            commands::run_live_reapply_now,
            commands::retry_live_reapply,
            commands::secrets_cleanup_orphans,
            commands::get_skills_migration_result,
            commands::get_app_config_path,
            commands::open_app_config_folder,
            commands::get_claude_common_config_snippet,
            commands::set_claude_common_config_snippet,
            commands::get_common_config_snippet,
            commands::set_common_config_snippet,
            commands::update_toml_common_config_snippet,
            commands::extract_common_config_snippet,
            commands::read_live_provider_settings,
            commands::get_settings,
            commands::save_settings,
            commands::set_env_delivery_strict_mode,
            commands::set_env_delivery_strict_apps,
            commands::get_shim_activation_snippet,
            commands::get_diagnostics_bundle,
            commands::has_codex_unify_history_backup,
            commands::restore_codex_unified_history,
            commands::get_log_config,
            commands::set_log_config,
            commands::restart_app,
            commands::copy_text_to_clipboard,
            commands::get_claude_plugin_status,
            commands::read_claude_plugin_config,
            commands::apply_claude_plugin_config,
            commands::is_claude_plugin_applied,
            commands::apply_claude_onboarding_skip,
            commands::clear_claude_onboarding_skip,
            // Claude MCP management
            commands::get_claude_mcp_status,
            commands::read_claude_mcp_config,
            commands::upsert_claude_mcp_server,
            commands::delete_claude_mcp_server,
            commands::validate_mcp_command,
            // New MCP via config.json (SSOT)
            commands::get_mcp_config,
            commands::upsert_mcp_server_in_config,
            commands::delete_mcp_server_in_config,
            commands::set_mcp_enabled,
            // Unified MCP management
            commands::get_mcp_servers,
            commands::upsert_mcp_server,
            commands::delete_mcp_server,
            commands::toggle_mcp_app,
            commands::import_mcp_from_apps,
            // Prompt management
            commands::get_prompts,
            commands::upsert_prompt,
            commands::delete_prompt,
            commands::enable_prompt,
            commands::import_prompt_from_file,
            commands::get_current_prompt_file_content,
            commands::get_pi_prompt_file,
            commands::replace_pi_prompt_file,
            commands::delete_pi_prompt_file,
            commands::list_pi_prompt_templates,
            commands::upsert_pi_prompt_template,
            commands::delete_pi_prompt_template,
            // Pi native provider and session views
            commands::get_pi_current_state,
            commands::get_pi_session_discovery,
            // Profile management (项目配置方案)
            commands::list_profiles,
            commands::create_profile,
            commands::update_profile,
            commands::delete_profile,
            commands::clear_current_profile,
            commands::apply_profile,
            // model list fetch (OpenAI-compatible /v1/models)
            commands::fetch_models_for_config,
            // app_config_dir override via Store
            commands::get_app_config_dir_override,
            commands::set_app_config_dir_override,
            // provider sort order management
            commands::update_providers_sort_order,
            // theirs: config import/export and dialogs
            commands::export_config_via_dialog,
            commands::import_config_via_dialog,
            commands::webdav_test_connection,
            commands::webdav_sync_upload,
            commands::webdav_sync_download,
            commands::webdav_sync_save_settings,
            commands::webdav_sync_fetch_remote_info,
            commands::s3_test_connection,
            commands::s3_sync_upload,
            commands::s3_sync_download,
            commands::s3_sync_save_settings,
            commands::sync_e2e_set_passphrase,
            commands::sync_e2e_set_enabled,
            commands::sync_e2e_reset_remote,
            commands::sync_e2e_delete_legacy_remote,
            commands::sync_e2e_get_status,
            commands::s3_sync_fetch_remote_info,
            commands::open_zip_file_dialog,
            commands::create_db_backup,
            commands::list_db_backups,
            commands::restore_db_backup,
            commands::rename_db_backup,
            commands::delete_db_backup,
            commands::sync_current_providers_live,
            update_tray_menu,
            // Environment variable management
            commands::env_delivery_scan,
            commands::env_delivery_remove,
            commands::env_delivery_conflicts,
            commands::env_delivery_adopt,
            // Skill management (v3.10.0+ unified)
            commands::get_installed_skills,
            commands::get_skill_backups,
            commands::delete_skill_backup,
            commands::install_skill_unified,
            commands::uninstall_skill_unified,
            commands::restore_skill_backup,
            commands::toggle_skill_app,
            commands::scan_unmanaged_skills,
            commands::import_skills_from_apps,
            commands::discover_available_skills,
            commands::check_skill_updates,
            commands::update_skill,
            commands::migrate_skill_storage,
            commands::search_skills_sh,
            // Skill management (legacy API compatibility)
            commands::get_skills,
            commands::get_skills_for_app,
            commands::install_skill,
            commands::install_skill_for_app,
            commands::uninstall_skill,
            commands::uninstall_skill_for_app,
            commands::get_skill_repos,
            commands::add_skill_repo,
            commands::remove_skill_repo,
            commands::install_skills_from_zip,
            // Auto launch
            commands::set_auto_launch,
            commands::get_auto_launch_status,
            // Session manager
            commands::list_sessions,
            commands::get_session_messages,
            commands::delete_session,
            commands::delete_sessions,
            commands::get_tool_versions,
            commands::run_tool_lifecycle_action,
            commands::probe_tool_installations,
            // Provider terminal
            commands::open_provider_terminal,
            commands::run_provider_cli,
            // 资源管理器右键菜单
            commands::register_shell_menu,
            commands::unregister_shell_menu,
            commands::is_shell_menu_registered,
            // Global upstream proxy
            commands::get_global_proxy_url,
            commands::set_global_proxy_url,
            commands::test_proxy_url,
            // Window theme control
            commands::set_window_theme,
            // lightweight mode (for testing or low-resource environments)
            commands::enter_lightweight_mode,
            commands::exit_lightweight_mode,
            commands::is_lightweight_mode,
        ]);

    let app = builder
        .build(tauri::generate_context!())
        .expect("error while running tauri application");

    app.run(|app_handle, event| {
        // 处理退出请求（所有平台）
        if let RunEvent::ExitRequested { api, code, .. } = &event {
            match classify_exit_request(*code) {
                // code 为 None 表示运行时自动触发（如隐藏窗口的 WebView 被回收导致无存活窗口），
                // 此时应仅阻止退出、保持托盘后台运行。
                ExitRequestAction::StayInTray => {
                    log::info!("运行时触发退出请求（无存活窗口），阻止退出以保持托盘后台运行");
                    api.prevent_exit();
                    return;
                }
                // code 为 RESTART_EXIT_CODE：app.restart() / 自更新 relaunch 发起的重启。
                // 这条路径上 prevent_exit() 会被 Tauri 忽略，事件循环必定退出，随后由
                // Tauri 在 RunEvent::Exit 后用新二进制 re-exec。
                //
                // 绝不能复用下面的异步清理任务：该任务在 tokio 线程调 save_window_state，
                // 持有 window-state 插件锁的同时向主线程查询窗口几何；而主线程此刻正在
                // 退出事件循环，并在插件自带的 RunEvent::Exit 钩子里等待同一把锁——双方
                // 互等造成进程永久卡死（更新已安装但应用冻结、不再重启，见 #3998）。
                //
                // 重启路径交还 Tauri 默认流程即可：
                //   - 窗口状态：插件 Exit 钩子在主线程保存（同线程读取窗口几何，无死锁）
                //   - 托盘图标：Tauri 内部 cleanup_before_exit 清理，正常走 Drop
                //   - Live 配置：切换时已同步写盘，新实例启动按 DB 现状重新投递，无需恢复
                //   - 100ms 落盘等待：重启前的 DB 写入均为命令驱动、此刻已完成，
                //     与所有 Tauri 应用默认重启路径的行为一致，无需额外等待
                ExitRequestAction::DeferToTauriRestart => {
                    log::info!("收到重启请求 (code={code:?})，交由 Tauri 默认重启流程 re-exec");
                    return;
                }
                // 其它 Some(_)：用户主动调用 app.exit() 退出（如托盘菜单"退出"），
                // 此时执行清理后退出。
                ExitRequestAction::CleanupAndExit => {}
            }

            log::info!("收到用户主动退出请求 (code={code:?})，开始清理...");
            api.prevent_exit();

            let app_handle = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                save_window_state_before_exit(&app_handle);
                // 先于 std::process::exit 显式移除托盘图标。
                // 进程直接退出时 Tauri 运行时不走正常 Drop 流程，
                // 不会向 Windows Shell 发送 NIM_DELETE，导致已退出的进程
                // 注册的图标仍残留在系统托盘（鼠标悬停 Shell 才会重绘发现进程已死）。
                remove_tray_icon_before_exit(&app_handle);
                log::info!("清理完成，退出应用");

                // 短暂等待确保所有 I/O 操作（如数据库写入）刷新到磁盘
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;

                // 使用 std::process::exit 避免再次触发 ExitRequested
                std::process::exit(0);
            });
            return;
        }

        let _ = (app_handle, event);
    });
}

/// 主动从系统托盘移除托盘图标。
///
/// `std::process::exit` 会绕过 Tauri 运行时，触发不了 `TrayIcon::drop()`，
/// 也就不会向 Windows Shell 发 `NIM_DELETE`。结果是进程退出后托盘里
/// 仍保留一个死图标的缓存占位（Shell 不会主动重绘，需要鼠标悬停才刷新）。
///
/// 通过 `set_visible(false)` 走 `WM_USER_HIDE_TRAYICON` 消息路径，
/// 触发 tray-icon 内部的 `remove_tray_icon` → `Shell_NotifyIconW(NIM_DELETE)`，
/// 在进程结束前干净地把图标摘掉。其它平台 `set_visible(false)` 也是
/// 正常的隐藏/移除语义，作为跨平台兜底也安全。
pub(crate) fn remove_tray_icon_before_exit(app_handle: &tauri::AppHandle) {
    if let Some(tray) = app_handle.tray_by_id(tray::TRAY_ID) {
        if let Err(e) = tray.set_visible(false) {
            log::warn!("退出时移除托盘图标失败: {e}");
        } else {
            log::info!("已显式从系统托盘移除图标");
        }
    }
}

fn initialize_common_config_snippets(state: &store::AppState) {
    // Auto-extract common config snippets from clean live files when snippet is missing.
    // This must run before proxy takeover is restored on startup, otherwise we'd read
    // proxy-placeholder configs instead of the user's actual live settings.
    for app_type in crate::app_config::AppType::all() {
        if !state
            .db
            .should_auto_extract_config_snippet(app_type.as_str())
            .unwrap_or(false)
        {
            continue;
        }

        let settings = match crate::services::provider::ProviderService::read_live_settings(
            app_type.clone(),
        ) {
            Ok(s) => s,
            Err(_) => continue,
        };

        match crate::services::provider::ProviderService::extract_common_config_snippet_from_settings(
            app_type.clone(),
            &settings,
        ) {
            Ok(snippet) if !snippet.is_empty() && snippet != "{}" => {
                match state.db.set_config_snippet(app_type.as_str(), Some(snippet)) {
                    Ok(()) => {
                        let _ = state.db.set_config_snippet_cleared(app_type.as_str(), false);
                        log::info!(
                            "✓ Auto-extracted common config snippet for {}",
                            app_type.as_str()
                        );
                    }
                    Err(e) => log::warn!(
                        "✗ Failed to save config snippet for {}: {e}",
                        app_type.as_str()
                    ),
                }
            }
            Ok(_) => log::debug!(
                "○ Live config for {} has no extractable common fields",
                app_type.as_str()
            ),
            Err(e) => log::warn!(
                "✗ Failed to extract config snippet for {}: {e}",
                app_type.as_str()
            ),
        }
    }

    let should_run_legacy_migration = state
        .db
        .is_legacy_common_config_migrated()
        .map(|done| !done)
        .unwrap_or(true);

    if should_run_legacy_migration {
        for app_type in [
            crate::app_config::AppType::Claude,
            crate::app_config::AppType::Codex,
        ] {
            if let Err(e) = crate::services::provider::ProviderService::migrate_legacy_common_config_usage_if_needed(
                state,
                app_type.clone(),
            ) {
                log::warn!(
                    "✗ Failed to migrate legacy common-config usage for {}: {e}",
                    app_type.as_str()
                );
            }
        }

        if let Err(e) = state.db.set_legacy_common_config_migrated(true) {
            log::warn!("✗ Failed to persist legacy common-config migration flag: {e}");
        }
    }
}

// ============================================================
// 迁移错误对话框辅助函数
// ============================================================

/// 检测是否为中文环境
fn is_chinese_locale() -> bool {
    std::env::var("LANG")
        .or_else(|_| std::env::var("LC_ALL"))
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .map(|lang| lang.starts_with("zh"))
        .unwrap_or(false)
}

/// 显示迁移错误对话框
/// 返回 true 表示用户选择重试，false 表示用户选择退出
fn show_migration_error_dialog(app: &tauri::AppHandle, error: &str) -> bool {
    let title = if is_chinese_locale() {
        "配置迁移失败"
    } else {
        "Migration Failed"
    };

    let message = if is_chinese_locale() {
        format!(
            "从旧版本迁移配置时发生错误：\n\n{error}\n\n\
            您的数据尚未丢失，旧配置文件仍然保留。\n\
            建议回退到旧版本 CC Switch 以保护数据。\n\n\
            点击「重试」重新尝试迁移\n\
            点击「退出」关闭程序（可回退版本后重新打开）"
        )
    } else {
        format!(
            "An error occurred while migrating configuration:\n\n{error}\n\n\
            Your data is NOT lost - the old config file is still preserved.\n\
            Consider rolling back to an older CC Switch version.\n\n\
            Click 'Retry' to attempt migration again\n\
            Click 'Exit' to close the program"
        )
    };

    let retry_text = if is_chinese_locale() {
        "重试"
    } else {
        "Retry"
    };
    let exit_text = if is_chinese_locale() {
        "退出"
    } else {
        "Exit"
    };

    // 使用 blocking_show 同步等待用户响应
    // OkCancelCustom: 第一个按钮（重试）返回 true，第二个按钮（退出）返回 false
    app.dialog()
        .message(&message)
        .title(title)
        .kind(MessageDialogKind::Error)
        .buttons(MessageDialogButtons::OkCancelCustom(
            retry_text.to_string(),
            exit_text.to_string(),
        ))
        .blocking_show()
}

/// 显示数据库初始化/Schema 迁移失败对话框
/// 返回 true 表示用户选择重试，false 表示用户选择退出
fn show_database_init_error_dialog(
    app: &tauri::AppHandle,
    db_path: &std::path::Path,
    error: &str,
) -> bool {
    let title = if is_chinese_locale() {
        "数据库初始化失败"
    } else {
        "Database Initialization Failed"
    };

    let message = if is_chinese_locale() {
        format!(
            "初始化数据库或迁移数据库结构时发生错误：\n\n{error}\n\n\
            数据库文件路径：\n{db}\n\n\
            您的数据尚未丢失，应用不会自动删除数据库文件。\n\
            常见原因包括：数据库版本过新、文件损坏、权限不足、磁盘空间不足等。\n\n\
            建议：\n\
            1) 先备份整个配置目录（包含 cc-switch.db）\n\
            2) 如果提示“数据库版本过新”，请升级到更新版本\n\
            3) 如果刚升级出现异常，可回退旧版本导出/备份后再升级\n\n\
            点击「重试」重新尝试初始化\n\
            点击「退出」关闭程序",
            db = db_path.display()
        )
    } else {
        format!(
            "An error occurred while initializing or migrating the database:\n\n{error}\n\n\
            Database file path:\n{db}\n\n\
            Your data is NOT lost - the app will not delete the database automatically.\n\
            Common causes include: newer database version, corrupted file, permission issues, or low disk space.\n\n\
            Suggestions:\n\
            1) Back up the entire config directory (including cc-switch.db)\n\
            2) If you see “database version is newer”, please upgrade CC Switch\n\
            3) If this happened right after upgrading, consider rolling back to export/backup then upgrade again\n\n\
            Click 'Retry' to attempt initialization again\n\
            Click 'Exit' to close the program",
            db = db_path.display()
        )
    };

    let retry_text = if is_chinese_locale() {
        "重试"
    } else {
        "Retry"
    };
    let exit_text = if is_chinese_locale() {
        "退出"
    } else {
        "Exit"
    };

    app.dialog()
        .message(&message)
        .title(title)
        .kind(MessageDialogKind::Error)
        .buttons(MessageDialogButtons::OkCancelCustom(
            retry_text.to_string(),
            exit_text.to_string(),
        ))
        .blocking_show()
}

/// T-4：quick_check 失败对话框。返回 true = 用户选择"从最近备份恢复"，false = 退出。
fn show_db_quick_check_failed_dialog(app: &tauri::AppHandle, error: &str) -> bool {
    let (title, message, ok_text, cancel_text) = if is_chinese_locale() {
        (
            "数据库完整性检查失败",
            format!(
                "启动时 quick_check 未通过：\n\n{error}\n\n\
                「从最近备份恢复」会把当前数据库改名为 cc-switch.corrupt-*.db 留证，\
                用 backups 目录里最近的完好备份顶替，然后需要重新启动应用。\n\
                若所有备份都不可用，应用会原样退出，不会丢任何文件。"
            ),
            "从最近备份恢复",
            "退出",
        )
    } else {
        (
            "Database Integrity Check Failed",
            format!(
                "The startup quick_check did not pass:\n\n{error}\n\n\
                'Restore from latest backup' renames the current database to \
                cc-switch.corrupt-*.db, replaces it with the newest intact backup in \
                the backups directory, and requires an app restart.\n\
                If no backup is usable the app exits without touching any file."
            ),
            "Restore from latest backup",
            "Exit",
        )
    };

    app.dialog()
        .message(&message)
        .title(title)
        .kind(MessageDialogKind::Error)
        .buttons(MessageDialogButtons::OkCancelCustom(
            ok_text.to_string(),
            cancel_text.to_string(),
        ))
        .blocking_show()
}

/// T-4：从最近备份逐个尝试离线恢复。成功返回 true（调用方退出，重启后生效）。
fn restore_db_from_latest_backup_at_startup(app: &tauri::AppHandle) -> bool {
    let backups = match crate::database::Database::list_backups() {
        Ok(list) => list,
        Err(e) => {
            log::error!("枚举数据库备份失败: {e}");
            return false;
        }
    };
    for entry in &backups {
        match crate::database::Database::restore_main_db_file_from_backup(&entry.filename) {
            Ok(path) => {
                log::info!("已从备份 {} 恢复主库文件", entry.filename);
                let (title, msg) = if is_chinese_locale() {
                    (
                        "已恢复",
                        format!(
                            "已从备份 {} 恢复数据库：\n{}\n\n点「确定」退出后请重新启动应用。",
                            entry.filename,
                            path.display()
                        ),
                    )
                } else {
                    (
                        "Restored",
                        format!(
                            "Database restored from backup {}:\n{}\n\nPress OK to quit, then start the app again.",
                            entry.filename,
                            path.display()
                        ),
                    )
                };
                app.dialog()
                    .message(&msg)
                    .title(title)
                    .kind(MessageDialogKind::Info)
                    .buttons(MessageDialogButtons::Ok)
                    .blocking_show();
                return true;
            }
            Err(e) => {
                log::warn!("备份 {} 恢复失败，尝试下一个: {e}", entry.filename);
            }
        }
    }
    false
}
/// 返回 true 表示用户选择重试，false 表示用户选择退出。
/// 刻意不提供「跳过」：跳过等于退回明文存储，违反 fail-closed 原则。
fn show_secrets_probe_error_dialog(app: &tauri::AppHandle, error: &str) -> bool {
    let (title, message) = if is_chinese_locale() {
        (
            "无法访问 Windows 凭据管理器",
            format!(
                "凭据管理器自检（写入-读取-删除探针条目）失败：\n\n{error}\n\n\
                本版本的 API 密钥与 Base URL 只保存在 Windows 凭据管理器中，\n\
                无法访问时应用不会退回明文存储。\n\
                常见原因包括：凭据管理器服务被禁用、组策略限制、磁盘或注册表权限不足。\n\n\
                点击「重试」重新自检\n\
                点击「退出」关闭程序"
            ),
        )
    } else {
        (
            "Cannot Access Windows Credential Manager",
            format!(
                "The Credential Manager self-check (write-read-delete a probe entry) failed:\n\n{error}\n\n\
                This build stores API keys and Base URLs exclusively in the Windows Credential Manager.\n\
                The app will NOT fall back to plaintext storage when it is unavailable.\n\
                Common causes: the Credential Manager service is disabled, group policy restrictions, or insufficient permissions.\n\n\
                Click 'Retry' to run the self-check again\n\
                Click 'Exit' to close the program"
            ),
        )
    };

    let retry_text = if is_chinese_locale() {
        "重试"
    } else {
        "Retry"
    };
    let exit_text = if is_chinese_locale() {
        "退出"
    } else {
        "Exit"
    };

    app.dialog()
        .message(&message)
        .title(title)
        .kind(MessageDialogKind::Error)
        .buttons(MessageDialogButtons::OkCancelCustom(
            retry_text.to_string(),
            exit_text.to_string(),
        ))
        .blocking_show()
}

// ============================================================
// 退出请求分类
// ============================================================

/// `RunEvent::ExitRequested` 的三类来源，处理方式必须区分。
///
/// 关键约束：重启请求（`code == RESTART_EXIT_CODE`）上 `prevent_exit()` 会被
/// Tauri 静默忽略（见 `ExitRequestApi::prevent_exit` 文档），事件循环必定继续
/// 退出并触发各插件的 `RunEvent::Exit` 钩子；任何与之并发的自定义清理任务都
/// 可能与插件退出钩子争用同一状态而死锁。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitRequestAction {
    /// `code` 为 `None`：运行时自动触发（如隐藏窗口的 WebView 被回收导致无存活
    /// 窗口），阻止退出、保持托盘后台运行。
    StayInTray,
    /// `code` 为 `RESTART_EXIT_CODE`：`app.restart()` / 自更新 relaunch 发起的
    /// 重启，不拦截、不做自定义清理，交还 Tauri 默认 re-exec 流程。
    DeferToTauriRestart,
    /// 其它 `Some(_)`：用户主动退出（托盘「退出」等），执行完整异步清理后结束进程。
    CleanupAndExit,
}

fn classify_exit_request(code: Option<i32>) -> ExitRequestAction {
    match code {
        None => ExitRequestAction::StayInTray,
        Some(tauri::RESTART_EXIT_CODE) => ExitRequestAction::DeferToTauriRestart,
        Some(_) => ExitRequestAction::CleanupAndExit,
    }
}

// ============================================================
// 在应用主动退出前显式持久化窗口状态
// ============================================================

fn window_state_flags() -> StateFlags {
    StateFlags::POSITION | StateFlags::SIZE | StateFlags::MAXIMIZED
}

/// 当前应用的退出路径会拦截 `ExitRequested` 并最终直接 `std::process::exit(0)`，
/// 这里需要在真正结束进程前手动落盘，避免 window-state 插件的默认退出钩子被绕过。
pub fn save_window_state_before_exit(app_handle: &tauri::AppHandle) {
    if let Err(err) = app_handle.save_window_state(window_state_flags()) {
        log::error!("退出前保存窗口状态失败: {err}");
    } else {
        log::info!("已在退出前保存窗口状态");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        classify_exit_request, redact_url_for_log, redact_url_for_log_with_secrets,
        runtime_log_level_allows, ExitRequestAction,
    };

    #[test]
    fn log_url_redaction_strips_credentials_and_query_keeps_path() {
        // userinfo 与整个 query 剥离，path 保留用于诊断 base_url 配错。
        assert_eq!(
            redact_url_for_log(
                "https://user:secret@example.com:8443/v1/models?key=top-secret&alt=sse"
            ),
            "https://example.com:8443/v1/models"
        );
        // scheme-relative 保持形态，userinfo 去掉。
        assert_eq!(
            redact_url_for_log("//user:sk-secret@gw.example.com/v1"),
            "//gw.example.com/v1"
        );
        // 无 scheme 的裸 userinfo。
        assert_eq!(
            redact_url_for_log("user:sk-secret@gw.example.com/v1"),
            "gw.example.com/v1"
        );
        // 无法解析为绝对 URL 时：丢 query，其余原样保留。
        assert_eq!(redact_url_for_log("not-a-url?token=secret"), "not-a-url");
        // 不再对 path 段做“看起来像密钥”的形状猜测，正常路径完整保留。
        assert_eq!(
            redact_url_for_log("https://host.example/v1/models/gemini-2.5-pro"),
            "https://host.example/v1/models/gemini-2.5-pro"
        );
    }

    #[test]
    fn log_url_redaction_replaces_known_secret_values() {
        // 精确匹配已知密钥值：无论它出现在 path 还是别处都被抹掉。
        let secrets = vec!["k-9f3a7c2b1e".to_string()];
        assert_eq!(
            redact_url_for_log_with_secrets("https://gw.example.com/k-9f3a7c2b1e/v1", &secrets),
            "https://gw.example.com/[REDACTED]/v1"
        );
        // 过短(<8)的已知值不参与子串脱敏，避免误伤 /v1/ 之类的正常路径。
        let short_secrets = vec!["api".to_string()];
        assert_eq!(
            redact_url_for_log_with_secrets("https://api.example.com/v1", &short_secrets),
            "https://api.example.com/v1"
        );
    }

    #[test]
    fn runtime_log_filter_honors_dynamic_max_level() {
        assert!(!runtime_log_level_allows(
            log::Level::Error,
            log::LevelFilter::Off
        ));
        assert!(runtime_log_level_allows(
            log::Level::Error,
            log::LevelFilter::Info
        ));
        assert!(runtime_log_level_allows(
            log::Level::Info,
            log::LevelFilter::Info
        ));
        assert!(!runtime_log_level_allows(
            log::Level::Debug,
            log::LevelFilter::Info
        ));
    }

    #[test]
    fn no_code_keeps_app_alive_in_tray() {
        assert_eq!(classify_exit_request(None), ExitRequestAction::StayInTray);
    }

    #[test]
    fn restart_exit_code_defers_to_tauri_default_restart() {
        assert_eq!(
            classify_exit_request(Some(tauri::RESTART_EXIT_CODE)),
            ExitRequestAction::DeferToTauriRestart
        );
    }

    #[test]
    fn user_exit_codes_run_cleanup_then_exit() {
        assert_eq!(
            classify_exit_request(Some(0)),
            ExitRequestAction::CleanupAndExit
        );
        assert_eq!(
            classify_exit_request(Some(1)),
            ExitRequestAction::CleanupAndExit
        );
    }
}
