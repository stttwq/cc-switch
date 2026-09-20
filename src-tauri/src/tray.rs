//! 托盘菜单管理模块
//!
//! 负责系统托盘图标和菜单的创建、更新和事件处理。

use once_cell::sync::Lazy;
use tauri::menu::{CheckMenuItem, Menu, MenuBuilder, MenuItem, Submenu, SubmenuBuilder};
use tauri::{Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::app_config::AppType;
use crate::error::AppError;
use crate::store::AppState;

/// 每个 app 分区的子菜单句柄，用于 usage 更新时就地改 label 而非整菜单重建。
/// `create_tray_menu` 每次重建都会整表覆盖写入，保证句柄始终指向当前活跃菜单。
static TRAY_SECTION_SUBMENUS: Lazy<
    std::sync::Mutex<std::collections::HashMap<AppType, Submenu<tauri::Wry>>>,
> = Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// 托盘菜单文本（国际化）
#[derive(Clone, Copy)]
pub struct TrayTexts {
    pub show_main: &'static str,
    pub open_website: &'static str,
    pub no_providers_label: &'static str,
    pub lightweight_mode: &'static str,
    pub quit: &'static str,
    pub projects_label: &'static str,
    pub no_project_label: &'static str,
}

/// 将系统区域标识映射为托盘支持的语言码。
///
/// 镜像前端 `i18n/getInitialLanguage` 的判定顺序，确保首次安装
/// （`settings.language` 尚未写入）时托盘语言与界面语言一致：
/// 繁中系统（zh-TW/HK/MO/Hant）→ `zh-TW`，其余 zh → `zh`，
/// 日文 → `ja`，英文 → `en`，未知区域回退到 `zh`（与前端默认一致）。
fn map_locale_to_tray_language(locale: &str) -> &'static str {
    let locale = locale.to_lowercase();
    if locale == "zh" {
        "zh"
    } else if locale.starts_with("zh-tw")
        || locale.starts_with("zh-hk")
        || locale.starts_with("zh-mo")
        || locale.starts_with("zh-hant")
    {
        "zh-TW"
    } else if locale.starts_with("zh") {
        "zh"
    } else if locale.starts_with("ja") {
        "ja"
    } else if locale.starts_with("en") {
        "en"
    } else {
        "zh"
    }
}

/// 读取系统区域并映射为托盘语言码；取不到区域时回退到 `zh`。
fn detect_system_tray_language() -> &'static str {
    sys_locale::get_locale()
        .as_deref()
        .map(map_locale_to_tray_language)
        .unwrap_or("zh")
}

impl TrayTexts {
    pub fn from_language(language: &str) -> Self {
        match language {
            "en" => Self {
                show_main: "Open main window",
                open_website: "Open Official Website",
                no_providers_label: "(no providers)",
                lightweight_mode: "Lightweight Mode",
                quit: "Quit",
                projects_label: "Projects",
                no_project_label: "No project",
            },
            "ja" => Self {
                show_main: "メインウィンドウを開く",
                open_website: "公式サイトを開く",
                no_providers_label: "(プロバイダーなし)",
                lightweight_mode: "軽量モード",
                quit: "終了",
                projects_label: "プロジェクト",
                no_project_label: "プロジェクトを使用しない",
            },
            "zh-TW" => Self {
                show_main: "開啟主介面",
                open_website: "開啟官方網站",
                no_providers_label: "(無供應商)",
                lightweight_mode: "輕量模式",
                quit: "退出",
                projects_label: "專案",
                no_project_label: "不使用專案",
            },
            _ => Self {
                show_main: "打开主界面",
                open_website: "打开官方网站",
                no_providers_label: "(无供应商)",
                lightweight_mode: "轻量模式",
                quit: "退出",
                projects_label: "项目",
                no_project_label: "不使用项目",
            },
        }
    }
}

/// 托盘应用分区配置
pub struct TrayAppSection {
    pub app_type: AppType,
    pub prefix: &'static str,
    pub empty_id: &'static str,
    pub header_label: &'static str,
    pub log_name: &'static str,
}

pub const TRAY_ID: &str = "cc-switch";

pub const TRAY_SECTIONS: [TrayAppSection; 2] = [
    TrayAppSection {
        app_type: AppType::Claude,
        prefix: "claude_",
        empty_id: "claude_empty",
        header_label: "Claude",
        log_name: "Claude",
    },
    TrayAppSection {
        app_type: AppType::Codex,
        prefix: "codex_",
        empty_id: "codex_empty",
        header_label: "Codex",
        log_name: "Codex",
    },
];

/// 对供应商列表排序：sort_index → created_at → name
fn sort_providers(
    providers: &indexmap::IndexMap<String, crate::provider::Provider>,
) -> Vec<(&String, &crate::provider::Provider)> {
    let mut sorted: Vec<_> = providers.iter().collect();
    sorted.sort_by(|(_, a), (_, b)| {
        match (a.sort_index, b.sort_index) {
            (Some(idx_a), Some(idx_b)) => return idx_a.cmp(&idx_b),
            (Some(_), None) => return std::cmp::Ordering::Less,
            (None, Some(_)) => return std::cmp::Ordering::Greater,
            _ => {}
        }

        match (a.created_at, b.created_at) {
            (Some(time_a), Some(time_b)) => return time_a.cmp(&time_b),
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            _ => {}
        }

        a.name.cmp(&b.name)
    });
    sorted
}

/// 处理项目 Profile 托盘事件，返回是否已处理
///
/// 事件 id 形如 `profile_<scope>_<uuid>`（同一项目在各分组子菜单里各有一项，
/// 应用时只作用于该分组）；`profile_none_<scope>` 表示某分组"不使用项目"
/// （只清该分组标记，不动配置）。
pub fn handle_profile_tray_event(app: &tauri::AppHandle, event_id: &str) -> bool {
    let Some(suffix) = event_id.strip_prefix("profile_") else {
        return false;
    };

    if let Some(scope_str) = suffix.strip_prefix("none_") {
        let Ok(scope) = crate::services::profile::ProfileScope::parse(scope_str) else {
            log::error!("未知的项目分组托盘事件: {event_id}");
            return true;
        };
        if let Some(app_state) = app.try_state::<AppState>() {
            if let Err(e) = app_state.db.set_current_profile_id(scope.as_str(), None) {
                log::error!("清除当前项目失败: {e}");
            }
        }
        // 通知主窗口刷新（profileId=null 表示该分组已清除当前项目）
        if let Err(e) = app.emit(
            "profile-applied",
            serde_json::json!({ "profileId": null, "scope": scope.as_str() }),
        ) {
            log::error!("发射 profile-applied 事件失败: {e}");
        }
        refresh_tray_menu(app);
        return true;
    }

    // scope 是固定枚举字符串（不含下划线），uuid 只含连字符，首个下划线即分界
    let Some((scope_str, profile_id)) = suffix.split_once('_') else {
        log::error!("无法解析项目托盘事件: {event_id}");
        return true;
    };
    let Ok(scope) = crate::services::profile::ProfileScope::parse(scope_str) else {
        log::error!("未知的项目分组托盘事件: {event_id}");
        return true;
    };

    log::info!("应用项目: {profile_id}（{scope_str} 组）");
    let app_handle = app.clone();
    let profile_id = profile_id.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(app_state) = app_handle.try_state::<AppState>() else {
            return;
        };
        match crate::services::profile::ProfileService::apply(app_state.inner(), &profile_id, scope)
        {
            Ok(warnings) => {
                for warning in &warnings {
                    log::warn!("[Profile] 应用项目 {profile_id} 警告: {warning}");
                }

                crate::commands::emit_profile_apply_events(
                    &app_handle,
                    app_state.inner(),
                    &profile_id,
                    scope,
                );
            }
            Err(e) => {
                log::error!("应用项目 {profile_id} 失败: {e}");
                refresh_tray_menu(&app_handle);
            }
        }
    });
    true
}

/// 处理供应商托盘事件
pub fn handle_provider_tray_event(app: &tauri::AppHandle, event_id: &str) -> bool {
    for section in TRAY_SECTIONS.iter() {
        if let Some(suffix) = event_id.strip_prefix(section.prefix) {
            log::info!("切换到{}供应商: {suffix}", section.log_name);
            let app_handle = app.clone();
            let provider_id = suffix.to_string();
            let app_type = section.app_type.clone();
            tauri::async_runtime::spawn_blocking(move || {
                if let Err(e) = handle_provider_click(&app_handle, &app_type, &provider_id) {
                    log::error!("切换{}供应商失败: {e}", section.log_name);
                }
            });
            return true;
        }
    }
    false
}

fn handle_provider_click(
    app: &tauri::AppHandle,
    app_type: &AppType,
    provider_id: &str,
) -> Result<(), AppError> {
    if let Some(app_state) = app.try_state::<AppState>() {
        let app_type_str = app_type.as_str();

        // 切换供应商。
        crate::services::ProviderService::switch(app_state.inner(), app_type.clone(), provider_id)?;

        // 更新托盘菜单
        if let Ok(new_menu) = create_tray_menu(app, app_state.inner()) {
            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                let _ = tray.set_menu(Some(new_menu));
            }
        }

        // 发射事件到前端
        let event_data = serde_json::json!({
            "appType": app_type_str,
            "providerId": provider_id
        });
        if let Err(e) = app.emit("provider-switched", event_data) {
            log::error!("发射 provider-switched 事件失败: {e}");
        }
    }
    Ok(())
}

/// 创建动态托盘菜单
pub fn create_tray_menu(
    app: &tauri::AppHandle,
    app_state: &AppState,
) -> Result<Menu<tauri::Wry>, AppError> {
    let app_settings = crate::settings::get_settings();
    // 用户未显式设置语言（首次安装）时，按系统区域回退而非硬编码简体，
    // 否则繁中系统的托盘会固定显示简体直到用户手动切换一次。
    let language: &str = match app_settings.language.as_deref() {
        Some(lang) => lang,
        None => detect_system_tray_language(),
    };
    let tray_texts = TrayTexts::from_language(language);

    // Get visible apps setting, default to all visible
    let visible_apps = app_settings.visible_apps.unwrap_or_default();

    let mut menu_builder = MenuBuilder::new(app);
    let mut section_handles: std::collections::HashMap<AppType, Submenu<tauri::Wry>> =
        std::collections::HashMap::new();

    // 顶部：打开主界面 / 打开官方网站
    let show_main_item =
        MenuItem::with_id(app, "show_main", tray_texts.show_main, true, None::<&str>)
            .map_err(|e| AppError::Message(format!("创建打开主界面菜单失败: {e}")))?;
    let open_website_item = MenuItem::with_id(
        app,
        "open_website",
        tray_texts.open_website,
        true,
        None::<&str>,
    )
    .map_err(|e| AppError::Message(format!("创建打开官方网站菜单失败: {e}")))?;
    menu_builder = menu_builder
        .item(&show_main_item)
        .item(&open_website_item)
        .separator();

    // 每个应用类型折叠为子菜单，避免供应商过多时菜单过长
    for section in TRAY_SECTIONS.iter() {
        if !visible_apps.is_visible(&section.app_type) {
            continue;
        }

        let app_type_str = section.app_type.as_str();
        let providers = app_state.db.get_all_providers(app_type_str)?;

        let current_id =
            crate::settings::get_effective_current_provider(&app_state.db, &section.app_type)?
                .unwrap_or_default();

        if providers.is_empty() {
            // 空供应商：显示禁用的菜单项
            let label = format!("{} {}", section.header_label, tray_texts.no_providers_label);
            let empty_item = MenuItem::with_id(app, section.empty_id, &label, false, None::<&str>)
                .map_err(|e| {
                    AppError::Message(format!("创建{}空提示失败: {e}", section.log_name))
                })?;
            menu_builder = menu_builder.item(&empty_item);
        } else {
            let current_provider = providers.get(&current_id);
            let submenu_label = match current_provider {
                Some(p) => format!("{} · {}", section.header_label, p.name),
                None => section.header_label.to_string(),
            };
            let submenu_id = format!("submenu_{}", app_type_str);

            let mut submenu_builder = SubmenuBuilder::with_id(app, &submenu_id, &submenu_label);

            for (id, provider) in sort_providers(&providers) {
                let is_current = current_id == *id;
                let item = CheckMenuItem::with_id(
                    app,
                    format!("{}{}", section.prefix, id),
                    &provider.name,
                    true,
                    is_current,
                    None::<&str>,
                )
                .map_err(|e| {
                    AppError::Message(format!("创建{}菜单项失败: {e}", section.log_name))
                })?;
                submenu_builder = submenu_builder.item(&item);
            }

            let submenu = submenu_builder.build().map_err(|e| {
                AppError::Message(format!("构建{}子菜单失败: {e}", section.log_name))
            })?;
            section_handles.insert(section.app_type.clone(), submenu.clone());
            menu_builder = menu_builder.item(&submenu);
        }

        menu_builder = menu_builder.separator();
    }

    // 项目 Profile 子菜单：项目列表全应用共享，按分组嵌套子菜单各自勾选/应用
    // （组内应用可见且存在项目时才显示该组）
    {
        use crate::services::profile::ProfileScope;

        let any_scope_visible = ProfileScope::ALL.iter().any(|scope| {
            scope
                .apps()
                .iter()
                .any(|app_type| visible_apps.is_visible(app_type))
        });
        let profiles = if any_scope_visible {
            app_state.db.get_all_profiles()?
        } else {
            Vec::new()
        };

        let mut scope_submenus = Vec::new();
        for scope in ProfileScope::ALL {
            if profiles.is_empty()
                || !scope
                    .apps()
                    .iter()
                    .any(|app_type| visible_apps.is_visible(app_type))
            {
                continue;
            }
            let current_profile_id = app_state
                .db
                .get_current_profile_id(scope.as_str())?
                .unwrap_or_default();
            // 分组标签用产品名，不进 i18n
            let scope_label = match scope {
                ProfileScope::Claude => "Claude Code",
                ProfileScope::Codex => "Codex",
            };
            let mut scope_builder = SubmenuBuilder::with_id(
                app,
                format!("submenu_profiles_{}", scope.as_str()),
                scope_label,
            );
            for profile in &profiles {
                let item = CheckMenuItem::with_id(
                    app,
                    format!("profile_{}_{}", scope.as_str(), profile.id),
                    &profile.name,
                    true,
                    current_profile_id == profile.id,
                    None::<&str>,
                )
                .map_err(|e| AppError::Message(format!("创建项目菜单项失败: {e}")))?;
                scope_builder = scope_builder.item(&item);
            }
            let none_item = CheckMenuItem::with_id(
                app,
                format!("profile_none_{}", scope.as_str()),
                tray_texts.no_project_label,
                true,
                current_profile_id.is_empty(),
                None::<&str>,
            )
            .map_err(|e| AppError::Message(format!("创建不使用项目菜单项失败: {e}")))?;
            let scope_submenu = scope_builder
                .separator()
                .item(&none_item)
                .build()
                .map_err(|e| AppError::Message(format!("构建项目分组子菜单失败: {e}")))?;
            scope_submenus.push(scope_submenu);
        }

        if !scope_submenus.is_empty() {
            let mut profiles_builder =
                SubmenuBuilder::with_id(app, "submenu_profiles", tray_texts.projects_label);
            for scope_submenu in &scope_submenus {
                profiles_builder = profiles_builder.item(scope_submenu);
            }
            let profiles_submenu = profiles_builder
                .build()
                .map_err(|e| AppError::Message(format!("构建项目子菜单失败: {e}")))?;
            menu_builder = menu_builder.item(&profiles_submenu).separator();
        }
    }

    let lightweight_item = CheckMenuItem::with_id(
        app,
        "lightweight_mode",
        tray_texts.lightweight_mode,
        true,
        crate::lightweight::is_lightweight_mode(),
        None::<&str>,
    )
    .map_err(|e| AppError::Message(format!("创建轻量模式菜单失败: {e}")))?;

    menu_builder = menu_builder.item(&lightweight_item).separator();

    // 退出菜单（分隔符已在上面的 section 循环中添加）
    let quit_item = MenuItem::with_id(app, "quit", tray_texts.quit, true, None::<&str>)
        .map_err(|e| AppError::Message(format!("创建退出菜单失败: {e}")))?;

    menu_builder = menu_builder.item(&quit_item);

    let menu = menu_builder
        .build()
        .map_err(|e| AppError::Message(format!("构建菜单失败: {e}")))?;

    *TRAY_SECTION_SUBMENUS
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = section_handles;

    Ok(menu)
}

pub fn refresh_tray_menu(app: &tauri::AppHandle) {
    use crate::store::AppState;

    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(new_menu) = create_tray_menu(app, state.inner()) {
            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                if let Err(e) = tray.set_menu(Some(new_menu)) {
                    log::error!("刷新托盘菜单失败: {e}");
                }
            }
        }
    }
}

/// 处理托盘菜单事件
pub fn handle_tray_menu_event(app: &tauri::AppHandle, event_id: &str) {
    log::info!("处理托盘菜单事件: {event_id}");

    match event_id {
        "show_main" => {
            if let Some(window) = app.get_webview_window("main") {
                #[cfg(target_os = "windows")]
                {
                    let _ = window.set_skip_taskbar(false);
                }
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            } else if crate::lightweight::is_lightweight_mode() {
                if let Err(e) = crate::lightweight::exit_lightweight_mode(app) {
                    log::error!("退出轻量模式重建窗口失败: {e}");
                }
            }
        }
        "open_website" => {
            if let Err(e) = app
                .opener()
                .open_url("https://github.com/stttwq/cc-switch", None::<String>)
            {
                log::error!("打开项目主页失败: {e}");
            }
        }
        "lightweight_mode" => {
            if crate::lightweight::is_lightweight_mode() {
                if let Err(e) = crate::lightweight::exit_lightweight_mode(app) {
                    log::error!("退出轻量模式失败: {e}");
                }
            } else if let Err(e) = crate::lightweight::enter_lightweight_mode(app) {
                log::error!("进入轻量模式失败: {e}");
            }
        }
        "quit" => {
            log::info!("退出应用");
            app.exit(0);
        }
        _ => {
            if handle_profile_tray_event(app, event_id) {
                return;
            }
            if handle_provider_tray_event(app, event_id) {
                return;
            }
            log::warn!("未处理的菜单事件: {event_id}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{detect_system_tray_language, map_locale_to_tray_language, TRAY_ID, TRAY_SECTIONS};

    #[test]
    fn tray_id_is_unique_to_app() {
        let ids: Vec<&str> = TRAY_SECTIONS.iter().map(|s| s.empty_id).collect();
        let mut seen = std::collections::HashSet::new();
        for id in ids {
            assert!(seen.insert(id), "duplicate tray id {id}");
        }
        assert_eq!(TRAY_ID, "cc-switch");
    }

    #[test]
    fn locale_maps_traditional_chinese_variants_to_zh_tw() {
        for l in ["zh-TW", "zh-Hant", "zh-HK", "zh-Hant-TW"] {
            assert_eq!(map_locale_to_tray_language(l), "zh-TW");
        }
    }

    #[test]
    fn locale_maps_simplified_chinese_variants_to_zh() {
        for l in ["zh", "zh-CN", "zh-SG", "zh-Hans", "zh-Hans-CN", "zh_CN"] {
            assert_eq!(map_locale_to_tray_language(l), "zh");
        }
    }

    #[test]
    fn locale_maps_japanese_and_english() {
        assert_eq!(map_locale_to_tray_language("ja"), "ja");
        assert_eq!(map_locale_to_tray_language("ja-JP"), "ja");
        assert_eq!(map_locale_to_tray_language("en"), "en");
        assert_eq!(map_locale_to_tray_language("en-US"), "en");
    }

    #[test]
    fn locale_unknown_falls_back_to_zh() {
        assert_eq!(map_locale_to_tray_language("fr-FR"), "zh");
        assert_eq!(map_locale_to_tray_language(""), "zh");
        assert_eq!(detect_system_tray_language(), detect_system_tray_language());
    }
}
