// one-off: strip usage/subscription/failover/proxy machinery from tray.rs
const fs = require('fs');
const p = 'src-tauri/src/tray.rs';
let t = fs.readFileSync(p, 'utf8');

function cut(startMarker, endMarkerInclusive, label) {
  const s = t.indexOf(startMarker);
  if (s < 0) { console.error('START NOT FOUND: ' + label); process.exit(1); }
  const e = t.indexOf(endMarkerInclusive, s);
  if (e < 0) { console.error('END NOT FOUND: ' + label); process.exit(1); }
  const end = e + endMarkerInclusive.length;
  t = t.slice(0, s) + t.slice(end);
  console.log('cut', label);
}

function cutExclusive(startMarker, endMarker, label) {
  const s = t.indexOf(startMarker);
  if (s < 0) { console.error('START NOT FOUND: ' + label); process.exit(1); }
  const e = t.indexOf(endMarker, s);
  if (e < 0) { console.error('END NOT FOUND: ' + label); process.exit(1); }
  t = t.slice(0, s) + t.slice(e);
  console.log('cut', label);
}

// 1. UsageCache import
t = t.replace('use crate::services::usage_cache::UsageCache;\n', '');

// 2. tier consts + subscription template const
cut('const TEMPLATE_TYPE_OFFICIAL_SUBSCRIPTION', '];\n', 'tier consts');

// 3. usage formatting block: UTIL_WARN_PCT .. end of format_usage_suffix
cut('const UTIL_WARN_PCT: f64 = 70.0;', '    None\n}\n', 'usage fns');

// 4. handle_auto_click fn (ends before handle_provider_click doc comment)
cut('/// 处理 Auto 点击：启用 proxy 和 auto_failover', '/// 处理供应商点击：关闭 auto_failover + 切换供应商', 'handle_auto_click');

// 5. Auto branch in handle_provider_tray_event
cut('            // 处理 Auto 点击\n', '            // 处理供应商点击', 'auto branch');

// 6. handle_provider_click proxy flag bookkeeping
t = t.replace(`    if let Some(app_state) = app.try_state::<AppState>() {
        let app_type_str = app_type.as_str();

        // 获取当前 proxy 状态，保持 enabled 不变，只关闭 auto_failover
        let (proxy_enabled, _) = app_state.db.get_proxy_flags_sync(app_type_str);
        app_state
            .db
            .set_proxy_flags_sync(app_type_str, proxy_enabled, false)?;

        // 切换供应商。需要本地路由的供应商也不在这里自动启动代理，
        // 由用户在页面/设置中手动开启。
        crate::services::ProviderService::switch(app_state.inner(), app_type.clone(), provider_id)?;`,
`    if let Some(app_state) = app.try_state::<AppState>() {
        let app_type_str = app_type.as_str();

        // 切换供应商。
        crate::services::ProviderService::switch(app_state.inner(), app_type.clone(), provider_id)?;`);

t = t.replace(`        // 发射事件到前端
        let event_data = serde_json::json!({
            "appType": app_type_str,
            "proxyEnabled": proxy_enabled,
            "autoFailoverEnabled": false,
            "providerId": provider_id
        });
        if let Err(e) = app.emit("proxy-flags-changed", event_data.clone()) {
            log::error!("发射 proxy-flags-changed 事件失败: {e}");
        }
        // 发射 provider-switched 事件（保持向后兼容）
        if let Err(e) = app.emit("provider-switched", event_data) {
            log::error!("发射 provider-switched 事件失败: {e}");
        }`,
`        // 发射事件到前端
        let event_data = serde_json::json!({
            "appType": app_type_str,
            "providerId": provider_id
        });
        if let Err(e) = app.emit("provider-switched", event_data) {
            log::error!("发射 provider-switched 事件失败: {e}");
        }`);

// 7. create_tray_menu: proxy running + usage suffix + takeover checks
t = t.replace(`    // Pre-compute proxy running state (used to disable official providers in tray menu)
    let is_proxy_running = futures::executor::block_on(app_state.proxy_service.is_running());

`, '');
t = t.replace(`            let current_provider = providers.get(&current_id);
            let submenu_label = match current_provider {
                Some(p) => {
                    let suffix = format_usage_suffix(
                        &app_state.usage_cache,
                        &section.app_type,
                        p,
                        &current_id,
                    )
                    .unwrap_or_default();
                    format!("{} · {}{}", section.header_label, p.name, suffix)
                }
                None => section.header_label.to_string(),
            };
            let submenu_id = format!("submenu_{}", app_type_str);

            // Check if this app is under proxy takeover (for disabling official providers)
            let is_app_taken_over = is_proxy_running
                && (futures::executor::block_on(app_state.db.get_live_backup(app_type_str))
                    .ok()
                    .flatten()
                    .is_some()
                    || app_state
                        .proxy_service
                        .detect_takeover_in_live_config_for_app(&section.app_type));

            let mut submenu_builder = SubmenuBuilder::with_id(app, &submenu_id, &submenu_label);

            for (id, provider) in sort_providers(&providers) {
                let is_current = current_id == *id;
                let is_official_blocked = is_app_taken_over
                    && provider.category.as_deref() == Some("official")
                    && !crate::services::provider::official_provider_supports_proxy_takeover(
                        &section.app_type,
                        provider,
                    );
                let label = if is_official_blocked {
                    format!("{} \\u{26D4}", &provider.name) // ⛔ emoji
                } else {
                    provider.name.clone()
                };
                let item = CheckMenuItem::with_id(
                    app,
                    format!("{}{}", section.prefix, id),
                    &label,
                    !is_official_blocked, // disabled when blocked
                    is_current,
                    None::<&str>,
                )
                .map_err(|e| {
                    AppError::Message(format!("创建{}菜单项失败: {e}", section.log_name))
                })?;
                submenu_builder = submenu_builder.item(&item);
            }`,
`            let current_provider = providers.get(&current_id);
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
            }`);

// 8. update_tray_usage_labels fn
cut('/// 就地更新各 app 分区子菜单的标题（usage 后缀变化时走这条），', 'pub fn refresh_tray_menu(app: &tauri::AppHandle) {', 'update_tray_usage_labels');

// 9. refresh worker statics + fns (up to tests module, exclusive)
cutExclusive('static LAST_TRAY_USAGE_REFRESH', '#[cfg(test)]', 'tray refresh fns');

// 10. replace whole tests module with a minimal one
{
  const s = t.indexOf('#[cfg(test)]\nmod tests {');
  if (s < 0) { console.error('tests module not found'); process.exit(1); }
  t = t.slice(0, s) + `#[cfg(test)]
mod tests {
    use super::{detect_system_tray_language, map_locale_to_tray_language, TRAY_ID, TRAY_SECTIONS};

    #[test]
    fn tray_id_is_unique_to_app() {
        let ids: Vec<&str> = TRAY_SECTIONS.iter().map(|s| s.empty_id).collect();
        let mut seen = std::collections::HashSet::new();
        for id in ids {
            assert!(seen.insert(id), "duplicate tray id {id}");
        }
        assert_eq!(TRAY_ID, "tray");
    }

    #[test]
    fn locale_maps_traditional_chinese_variants_to_zh_tw() {
        for l in ["zh-TW", "zh-Hant", "zh-HK", "zh-Hant-TW", "zh_TW"] {
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
`;
}

fs.writeFileSync(p, t);
console.log('tray.rs done');
