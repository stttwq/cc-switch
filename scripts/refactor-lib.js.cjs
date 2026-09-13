// one-off: apply all Phase-1a lib.rs removals (idempotent-ish, verify each)
const fs = require('fs');
const p = 'src-tauri/src/lib.rs';
let t = fs.readFileSync(p, 'utf8');
let failed = false;

function must(re, rep, label) {
  if (!re.test(t)) { console.error('NOT MATCHED: ' + label); failed = true; return; }
  t = t.replace(re, rep);
  console.log('ok', label);
}

// 1. mod declarations
must(/^mod model_capabilities;\r?\n/m, '', 'mod model_capabilities');
must(/^mod proxy;\r?\n/m, '', 'mod proxy');
must(/^mod usage_events;\r?\n/m, '', 'mod usage_events');
must(/^mod usage_script;\r?\n/m, '', 'mod usage_script');

// 2. services pub use
must(/    ConfigService, EndpointLatency, McpService, PromptService, ProviderService, ProxyService,\r?\n    SkillService, SpeedtestService,/,
     '    ConfigService, McpService, PromptService, ProviderService, SkillService,', 'pub use services');

// 3. usage_events init
must(/            \/\/ 注入 AppHandle 给 usage_events[\s\S]*?usage_events::init\(app\.handle\(\)\.clone\(\)\);\r?\n\r?\n/, '', 'usage_events init');

// 4. proxy_service.set_app_handle
must(/            \/\/ 设置 AppHandle 用于代理故障转移时的 UI 更新\r?\n            app_state\.proxy_service\.set_app_handle\(app\.handle\(\)\.clone\(\)\);\r?\n\r?\n/, '', 'set_app_handle');

// 5. tray usage refresh
must(/                \.on_tray_icon_event\(\|tray, event\| match event \{[\s\S]*?_ => log::debug!\("unhandled event \{event:\?\}"\),\r?\n                \}\)/,
     '                .on_tray_icon_event(|tray, event| match event {\n                    _ => log::debug!("unhandled event {event:?}"),\n                })', 'tray icon event');

// 6. oauth manager init blocks
must(/            \/\/ 初始化 CopilotAuthManager\r?\n            \{[\s\S]*?\r?\n            \}\r?\n\r?\n            \/\/ 初始化 CodexOAuthManager \(ChatGPT Plus\/Pro 反代\)\r?\n            \{[\s\S]*?\r?\n            \}\r?\n\r?\n            \/\/ 初始化 xAI OAuthManager \(Grok API 反代\)\r?\n            \{[\s\S]*?\r?\n            \}\r?\n\r?\n            \/\/ 初始化全局出站代理 HTTP 客户端/, '            // 初始化全局出站代理 HTTP 客户端', 'oauth init blocks');

// 7. http client path
must(/crate::proxy::http_client::init/g, 'crate::services::http_client::init', 'http_client init');

// 8. startup async: live backup recovery + proxy restore + session sync
must(/            \/\/ 异常退出恢复 \+ 代理状态自动恢复/, '            // 异常退出恢复', 'startup comment');
must(/                \/\/ 检查是否有 Live 备份[\s\S]*?restore_proxy_state_on_startup\(&state\)\.await;\r?\n\r?\n/, '', 'startup recovery + restore + before periodic');
must(/                \/\/ Session log usage sync: 启动时同步一次[\s\S]*?\r?\n            \}\);\r?\n            \}\);\r?\n            \}\);\r?\n            \}\);/, '', 'session sync block');

// 9. exit cleanup call
must(/                cleanup_before_exit\(&app_handle\)\.await;\r?\n/, '', 'cleanup_before_exit call');

// 10. invoke_handler removals (individual lines)
const lines = [
  '            commands::get_rectifier_config,\n',
  '            commands::set_rectifier_config,\n',
  '            commands::get_optimizer_config,\n',
  '            commands::set_optimizer_config,\n',
  '            commands::get_copilot_optimizer_config,\n',
  '            commands::set_copilot_optimizer_config,\n',
  '            // usage query\n',
  '            commands::queryProviderUsage,\n',
  '            commands::testUsageScript,\n',
  '            // subscription quota\n',
  '            commands::get_subscription_quota,\n',
  '            commands::get_codex_oauth_quota,\n',
  '            commands::get_codex_oauth_models,\n',
  '            commands::get_xai_oauth_models,\n',
  '            commands::get_xai_oauth_quota,\n',
  '            commands::get_coding_plan_quota,\n',
  '            commands::get_balance,\n',
  '            commands::update_pi_provider_usage_script,\n',
  '            // ours: endpoint speed test + custom endpoint management\n',
  '            commands::test_api_endpoints,\n',
  '            // Proxy server management\n',
  '            commands::start_proxy_server,\n',
  '            commands::stop_proxy_server,\n',
  '            commands::stop_proxy_with_restore,\n',
  '            commands::get_proxy_takeover_status,\n',
  '            commands::set_proxy_takeover_for_app,\n',
  '            commands::get_proxy_status,\n',
  '            commands::get_proxy_config,\n',
  '            commands::update_proxy_config,\n',
  '            // Global & Per-App Config\n',
  '            commands::get_global_proxy_config,\n',
  '            commands::update_global_proxy_config,\n',
  '            commands::get_proxy_config_for_app,\n',
  '            commands::update_proxy_config_for_app,\n',
  '            commands::get_default_cost_multiplier,\n',
  '            commands::set_default_cost_multiplier,\n',
  '            commands::get_pricing_model_source,\n',
  '            commands::set_pricing_model_source,\n',
  '            commands::is_proxy_running,\n',
  '            commands::is_live_takeover_active,\n',
  '            commands::switch_proxy_provider,\n',
  '            // Proxy failover commands\n',
  '            commands::get_provider_health,\n',
  '            commands::reset_circuit_breaker,\n',
  '            commands::get_circuit_breaker_config,\n',
  '            commands::update_circuit_breaker_config,\n',
  '            commands::get_circuit_breaker_stats,\n',
  '            // Failover queue management\n',
  '            commands::get_failover_queue,\n',
  '            commands::get_available_providers_for_failover,\n',
  '            commands::add_to_failover_queue,\n',
  '            commands::remove_from_failover_queue,\n',
  '            commands::get_auto_failover_enabled,\n',
  '            commands::set_auto_failover_enabled,\n',
  '            // Usage statistics\n',
  '            commands::get_usage_summary,\n',
  '            commands::get_usage_summary_by_app,\n',
  '            commands::get_usage_trends,\n',
  '            commands::get_provider_stats,\n',
  '            commands::get_model_stats,\n',
  '            commands::get_request_logs,\n',
  '            commands::get_request_detail,\n',
  '            commands::get_model_pricing,\n',
  '            commands::update_model_pricing,\n',
  '            commands::update_model_pricing_batch,\n',
  '            commands::delete_model_pricing,\n',
  '            commands::get_models_dev_sync_config,\n',
  '            commands::save_models_dev_sync_config,\n',
  '            commands::record_models_dev_sync_result,\n',
  '            commands::check_provider_limits,\n',
  '            // Session usage sync\n',
  '            commands::sync_session_usage,\n',
  '            commands::rebuild_codex_usage,\n',
  '            commands::get_usage_data_sources,\n',
  '            // Stream health check\n',
  '            commands::stream_check_provider,\n',
  '            commands::stream_check_all_providers,\n',
  '            commands::get_stream_check_config,\n',
  '            commands::save_stream_check_config,\n',
  '            commands::get_upstream_proxy_status,\n',
  '            commands::scan_local_proxies,\n',
  '            // Generic managed auth commands\n',
  '            commands::auth_start_login,\n',
  '            commands::auth_poll_for_account,\n',
  '            commands::auth_cancel_login,\n',
  '            commands::auth_list_accounts,\n',
  '            commands::auth_get_status,\n',
  '            commands::auth_remove_account,\n',
  '            commands::auth_set_default_account,\n',
  '            commands::auth_logout,\n',
  '            // Copilot OAuth commands (multi-account support)\n',
  '            commands::copilot_start_device_flow,\n',
  '            commands::copilot_poll_for_auth,\n',
  '            commands::copilot_poll_for_account,\n',
  '            commands::copilot_list_accounts,\n',
  '            commands::copilot_remove_account,\n',
  '            commands::copilot_set_default_account,\n',
  '            commands::copilot_get_auth_status,\n',
  '            commands::copilot_logout,\n',
  '            commands::copilot_is_authenticated,\n',
  '            commands::copilot_get_token,\n',
  '            commands::copilot_get_token_for_account,\n',
  '            commands::copilot_get_models,\n',
  '            commands::copilot_get_models_for_account,\n',
  '            commands::copilot_get_usage,\n',
  '            commands::copilot_get_usage_for_account,\n',
];
for (const line of lines) {
  const idx = t.indexOf(line);
  if (idx < 0) { console.error('LINE NOT FOUND: ' + JSON.stringify(line)); failed = true; continue; }
  t = t.slice(0, idx) + t.slice(idx + line.length);
}
console.log('invoke_handler lines removed');

// 11. cleanup_before_exit fn + proxy startup restore fns
must(/\/\/ ============================================================\r?\n\/\/ 应用退出清理[\s\S]*?pub async fn cleanup_before_exit[\s\S]*?\n\}\r?\n\r?\n/, '', 'cleanup_before_exit fn');
must(/\/\/ ============================================================\r?\n\/\/ 启动时恢复代理状态[\s\S]*?async fn restore_proxy_state_on_startup[\s\S]*?\n\}\r?\n\r?\n/, '', 'restore_proxy_state_on_startup fn');

// 12. tests: imports + grokbuild proxy test
must(/        classify_exit_request, enabled_proxy_apps_on_startup, redact_url_for_log,/, '        classify_exit_request, redact_url_for_log,', 'test imports');
must(/\r?\n    #\[tokio::test\]\r?\n    async fn startup_restore_includes_enabled_grokbuild_route\(\) \{[\s\S]*?\n    \}\r?\n/, '', 'grokbuild test');

fs.writeFileSync(p, t);
console.log(failed ? 'FAILED (partially written)' : 'lib.rs done');
