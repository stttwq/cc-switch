use serde_json::Value;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};
use tauri_plugin_store::StoreExt;

use crate::error::AppError;

/// Store 中的键名
const STORE_KEY_APP_CONFIG_DIR: &str = "app_config_dir_override";

/// 缓存当前的 app_config_dir 覆盖路径，避免存储 AppHandle
static APP_CONFIG_DIR_OVERRIDE: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

fn override_cache() -> &'static RwLock<Option<PathBuf>> {
    APP_CONFIG_DIR_OVERRIDE.get_or_init(|| RwLock::new(None))
}

fn update_cached_override(value: Option<PathBuf>) {
    if let Ok(mut guard) = override_cache().write() {
        *guard = value;
    }
}

/// 获取缓存中的 app_config_dir 覆盖路径
pub fn get_app_config_dir_override() -> Option<PathBuf> {
    override_cache().read().ok()?.clone()
}

/// 应用标识（与 `tauri.conf.json` 的 `identifier` 保持一致）。
/// Tauri store 插件把 `app_paths.json` 放在 `%APPDATA%\<identifier>\` 下，
/// 该位置固定、不受 app_config_dir override 影响，因此 CLI shim 可据此定位。
const APP_IDENTIFIER: &str = "com.ccswitch.desktop";

/// CLI（无 AppHandle）用：直接把解析出的 override 写入本进程缓存。
/// shim 每次启动都须重新解析文件后再调用，且必须发生在任何 DB 打开/路径读取之前。
pub(crate) fn set_override_for_cli(value: Option<PathBuf>) {
    update_cached_override(value);
}

/// 从 `app_paths.json` 的 JSON 文本解析 override（纯函数，便于单测）。
/// 语义与 `read_override_from_store` 保持一致：空串/类型不符/目录不存在 → `None`。
pub(crate) fn parse_app_paths_override(json_str: &str) -> Option<PathBuf> {
    let value: Value = serde_json::from_str(json_str).ok()?;
    let path_str = value.get(STORE_KEY_APP_CONFIG_DIR)?.as_str()?.trim();
    if path_str.is_empty() {
        return None;
    }
    let path = resolve_path(path_str);
    if !path.exists() {
        log::warn!("app_paths.json 中配置的 app_config_dir 不存在: {path:?}，回落默认路径");
        return None;
    }
    Some(path)
}

/// Tauri store 文件 `app_paths.json` 的默认物理位置（Windows: `%APPDATA%\<identifier>\`）。
fn default_app_paths_store_file() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let appdata = std::env::var("APPDATA").ok()?;
        Some(
            PathBuf::from(appdata)
                .join(APP_IDENTIFIER)
                .join("app_paths.json"),
        )
    }
    #[cfg(not(windows))]
    {
        let config = dirs::config_dir()?;
        Some(config.join(APP_IDENTIFIER).join("app_paths.json"))
    }
}

/// CLI 建库前调用：读取默认 store 文件解析 override 并注入缓存。
/// 文件缺失/解析失败 → 保持 `None`（回落默认 `~/.cc-switch`），不报错。
pub(crate) fn load_override_for_cli() {
    let Some(file) = default_app_paths_store_file() else {
        return;
    };
    match std::fs::read_to_string(&file) {
        Ok(text) => {
            let value = parse_app_paths_override(&text);
            if value.is_some() {
                log::info!("CLI 已从 {file:?} 解析 app_config_dir override: {value:?}");
            }
            set_override_for_cli(value);
        }
        Err(_) => set_override_for_cli(None),
    }
}

fn read_override_from_store(app: &tauri::AppHandle) -> Option<PathBuf> {
    let store = match app.store_builder("app_paths.json").build() {
        Ok(store) => store,
        Err(e) => {
            log::warn!("无法创建 Store: {e}");
            return None;
        }
    };

    match store.get(STORE_KEY_APP_CONFIG_DIR) {
        Some(Value::String(path_str)) => {
            let path_str = path_str.trim();
            if path_str.is_empty() {
                return None;
            }

            let path = resolve_path(path_str);

            if !path.exists() {
                log::warn!(
                    "Store 中配置的 app_config_dir 不存在: {path:?}\n\
                     将使用默认路径。"
                );
                return None;
            }

            log::info!("使用 Store 中的 app_config_dir: {path:?}");
            Some(path)
        }
        Some(_) => {
            log::warn!("Store 中的 {STORE_KEY_APP_CONFIG_DIR} 类型不正确，应为字符串");
            None
        }
        None => None,
    }
}

/// 从 Store 刷新 app_config_dir 覆盖值并更新缓存
pub fn refresh_app_config_dir_override(app: &tauri::AppHandle) -> Option<PathBuf> {
    let value = read_override_from_store(app);
    update_cached_override(value.clone());
    value
}

/// 写入 app_config_dir 到 Tauri Store
pub fn set_app_config_dir_to_store(
    app: &tauri::AppHandle,
    path: Option<&str>,
) -> Result<(), AppError> {
    let store = app
        .store_builder("app_paths.json")
        .build()
        .map_err(|e| AppError::Message(format!("创建 Store 失败: {e}")))?;

    match path {
        Some(p) => {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                store.set(STORE_KEY_APP_CONFIG_DIR, Value::String(trimmed.to_string()));
                log::info!("已将 app_config_dir 写入 Store: {trimmed}");
            } else {
                store.delete(STORE_KEY_APP_CONFIG_DIR);
                log::info!("已从 Store 中删除 app_config_dir 配置");
            }
        }
        None => {
            store.delete(STORE_KEY_APP_CONFIG_DIR);
            log::info!("已从 Store 中删除 app_config_dir 配置");
        }
    }

    store
        .save()
        .map_err(|e| AppError::Message(format!("保存 Store 失败: {e}")))?;

    refresh_app_config_dir_override(app);
    Ok(())
}

/// 解析路径，支持 ~ 开头的相对路径
fn resolve_path(raw: &str) -> PathBuf {
    if raw == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    } else if let Some(stripped) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    } else if let Some(stripped) = raw.strip_prefix("~\\") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }

    PathBuf::from(raw)
}

/// 从旧的 settings.json 迁移 app_config_dir 到 Store
pub fn migrate_app_config_dir_from_settings(app: &tauri::AppHandle) -> Result<(), AppError> {
    // app_config_dir 已从 settings.json 移除，此函数保留但不再执行迁移
    // 如果用户在旧版本设置过 app_config_dir，需要在 Store 中手动配置
    log::info!("app_config_dir 迁移功能已移除，请在设置中重新配置");

    let _ = refresh_app_config_dir_override(app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_override_returns_existing_dir() {
        let dir = std::env::temp_dir().join("ccs-override-parse-case");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let raw = dir.to_string_lossy().replace('\\', "\\\\");
        let json = format!(r#"{{"{STORE_KEY_APP_CONFIG_DIR}":"{raw}"}}"#);
        let parsed = parse_app_paths_override(&json);
        assert_eq!(parsed.as_deref(), Some(dir.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_override_none_for_missing_empty_or_bad() {
        // 空串
        assert_eq!(
            parse_app_paths_override(&format!(r#"{{"{STORE_KEY_APP_CONFIG_DIR}":""}}"#)),
            None
        );
        // 不存在的目录
        assert_eq!(
            parse_app_paths_override(
                r#"{"app_config_dir_override":"Z:\\definitely\\does\\not\\exist\\ccs-xyz"}"#
            ),
            None
        );
        // 非法 JSON / 缺键
        assert_eq!(parse_app_paths_override("not json"), None);
        assert_eq!(parse_app_paths_override("{}"), None);
    }
}
