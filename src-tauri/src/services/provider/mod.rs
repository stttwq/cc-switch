//! Provider service module
//!
//! Handles provider CRUD operations, switching, and configuration management.

pub(crate) mod codex_sanitizer;
mod live;
mod live_sanitizer;
mod pi;
pub(crate) mod pi_sanitizer;

use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;

use crate::app_config::AppType;
use crate::error::AppError;
use crate::provider::Provider;
use crate::secrets::{ProviderSecrets, SecretExtractor};
use crate::services::mcp::McpService;
use crate::store::AppState;
use std::str::FromStr;
use zeroize::Zeroizing;

// Re-export sub-module functions for external access
pub use live::{
    import_default_config, live_config_has_plaintext_secrets, read_live_settings,
    should_import_default_config_on_startup, sync_current_to_live,
    update_toml_common_config_snippet,
};

pub fn import_pi_providers_from_live(state: &AppState) -> Result<usize, AppError> {
    pi::import_from_live(state)
}

pub fn reapply_pi_live(state: &AppState) -> Result<usize, AppError> {
    pi::reapply_live(state)
}

/// §6.4：凭据迁移后的 live 重写 + 环境变量投递（Claude / Codex / Pi 全覆盖）。
///
/// 返回失败项（`<app>: <错误>`），供 §6.5 展示并提供重试。全部成功时才清
/// `live_reapply_pending` 并清理已迁移的 Codex `auth.json`——留着登录态文件
/// 与"已迁移"的 API key 并存才是泄漏形态。
///
/// 抽成独立函数是为了可测：冷启动验收需要走完 §6.2 → §6.3 → §6.4 才能断言
/// "整个 home 0 明文命中"。
pub fn reapply_live_after_migration(state: &AppState) -> Result<Vec<String>, AppError> {
    /// 把一次 live 重写失败归类成稳定原因码，供前端本地化展示——
    /// 不再把 `原子替换失败: <临时文件> -> <目标>: 拒绝访问 (os error 5)` 这种
    /// 含临时路径的原始串直接甩给用户（2.0.1 遗留）。
    fn classify(e: &AppError) -> &'static str {
        let s = e.to_string();
        if s.contains("ENV_CONFLICT") {
            "env_conflict"
        } else if s.contains("原子替换失败") || s.contains("拒绝访问") || s.contains("os error 5")
        {
            "file_locked"
        } else {
            "other"
        }
    }

    let mut failures: Vec<String> = Vec::new();

    // 缺陷 D-4：回滚到旧版再升级时，注册表里还留着上一轮投递的变量，而恢复出来的旧库
    // 没有 `managed_env_vars` 登记，冲突检测会判成 foreign 并永久拒写。
    ProviderService::adopt_unregistered_managed_env(state)?;

    for app_type in [AppType::Claude, AppType::Codex] {
        match crate::settings::get_effective_current_provider(&state.db, &app_type) {
            Ok(Some(id)) => match ProviderService::switch(state, app_type.clone(), &id) {
                Ok(_) => log::info!("✓ live reapply {}", app_type.as_str()),
                Err(e) => {
                    log::warn!("✗ live reapply {} failed: {e}", app_type.as_str());
                    failures.push(format!("{}|{}", app_type.as_str(), classify(&e)));
                }
            },
            Ok(None) => {}
            Err(e) => {
                log::warn!("✗ live reapply 读取当前供应商失败: {e}");
                failures.push(format!("settings|{}", classify(&e)));
            }
        }
    }

    match reapply_pi_live(state) {
        Ok(n) => log::info!("✓ live reapply pi ({n} providers)"),
        Err(e) => {
            log::warn!("✗ live reapply pi failed: {e}");
            failures.push(format!("pi|{}", classify(&e)));
        }
    }

    if failures.is_empty() {
        if let Ok(rt) = tokio::runtime::Runtime::new() {
            if let Err(e) = rt.block_on(crate::secrets::migration::prune_migrated_codex_auth_file(
                &state.db,
                state.secrets.as_ref(),
            )) {
                log::warn!("Codex auth.json 残留检查跳过: {e}");
            }
        }
    }

    crate::secrets::migration::record_live_reapply_failures(&state.db, &failures);
    if failures.is_empty() {
        let _ = state.db.set_setting("live_reapply_pending", "0");
        log::info!("live_reapply_pending 已清零");
    }

    Ok(failures)
}

/// §6.7：1Password 模式下的启动 live 明文剥离（key-free，不走 switch/backfill/hydrate，
/// 不触发任何 op / 解锁）。
///
/// F1-8（P0-8）：改「就地、定点、有备份才剥」——启动路径没有回填，整文件重写 live 会
/// 吞掉用户在 CCS 外对 live 的改动；无条件删钥匙可能删掉 vault 里没有的新钥匙。现在：
/// 只删 Claude settings.json 的敏感键、Codex auth.json 的 OPENAI_API_KEY 与 config.toml
/// 的 experimental_bearer_token，其余字节原样；**剥离前提**是当前供应商的 `secret_refs`
/// 含 api_key（vault 里有备份，存在性按 refs 判定、0 次往返），不满足时不动文件、记入
/// 本机设置 `live_plaintext_pending`，UI 提示「检测到 live 文件含明文钥匙，[导入到
/// 1Password 并剥离]」。无明文零写入（幂等）。Pi 的 models.json 本就只含引用，不处理
/// （明文走 F1-4 的 pi_plaintext_pending）。
pub fn strip_current_live_plaintext(state: &AppState) -> Result<(), AppError> {
    let mut deferred: Vec<String> = Vec::new();
    for app_type in [AppType::Claude, AppType::Codex] {
        let id = match crate::settings::get_effective_current_provider(&state.db, &app_type) {
            Ok(Some(id)) => id,
            Ok(None) => continue,
            Err(e) => {
                log::warn!("读当前供应商失败 {}: {e}", app_type.as_str());
                continue;
            }
        };
        let has_backup = match ProviderService::provider_has_stored_key(state, &app_type, &id) {
            Ok(has) => has,
            Err(e) => {
                log::warn!("读 secret_refs 失败 {}/{id}: {e}", app_type.as_str());
                continue;
            }
        };
        let outcome = match app_type {
            AppType::Claude => live::strip_claude_live_plaintext_in_place(has_backup),
            AppType::Codex => live::strip_codex_live_plaintext_in_place(has_backup),
            _ => Ok(crate::codex_config::PlaintextStripOutcome::Clean),
        };
        match outcome {
            Ok(crate::codex_config::PlaintextStripOutcome::Stripped) => {
                log::info!("✓ 已定点剥离 {}/{} 的 live 明文", app_type.as_str(), id);
            }
            Ok(crate::codex_config::PlaintextStripOutcome::Deferred) => {
                log::info!(
                    "{}/{} 的 live 文件含明文钥匙但 vault 无备份，待导入",
                    app_type.as_str(),
                    id
                );
                deferred.push(format!("{}/{}", app_type.as_str(), id));
            }
            Ok(crate::codex_config::PlaintextStripOutcome::Clean) => {}
            Err(e) => log::warn!("剥离 live 明文失败 {}/{id}: {e}", app_type.as_str()),
        }
    }
    // pending 全量重建：无变化不落盘。
    if crate::settings::get_live_plaintext_pending() != deferred {
        crate::settings::set_live_plaintext_pending(deferred)?;
    }
    Ok(())
}

/// F1-8：「导入到 1Password 并剥离」——把 live 文件里的明文钥匙收进 vault，然后
/// 就地剥离 live。用户主动触发（允许 op 往返与解锁弹窗）。返回成功导入的数量。
pub(crate) fn import_live_plaintext_to_vault(state: &AppState) -> Result<usize, AppError> {
    let pending = crate::settings::get_live_plaintext_pending();
    let mut imported = 0;
    let mut remaining = Vec::new();
    for key in &pending {
        let Some((app_str, id)) = key.split_once('/') else {
            continue;
        };
        let Ok(app_type) = AppType::from_str(app_str) else {
            continue;
        };
        match import_one_live_plaintext(state, &app_type, id) {
            Ok(true) => imported += 1,
            Ok(false) => {}
            Err(error) => {
                log::warn!("{key} 导入 live 明文失败，保留 pending 待重试: {error}");
                remaining.push(key.clone());
            }
        }
    }
    if remaining.len() != pending.len() {
        crate::settings::set_live_plaintext_pending(remaining)?;
    }
    Ok(imported)
}

/// 导入单个 `<app>/<id>` 的 live 明文。返回 `false` 表示 live 里已没有明文钥匙（出队）。
fn import_one_live_plaintext(
    state: &AppState,
    app_type: &AppType,
    id: &str,
) -> Result<bool, AppError> {
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(app_type.as_str()));
    // 提取源是 live 文件本身（明文本就在用户文件里）；meta 只为 Claude 的 api_key_field。
    let meta_field = state
        .db
        .get_provider_by_id(id, app_type.as_str())?
        .and_then(|p| p.meta)
        .and_then(|m| m.api_key_field);
    let extracted = match app_type {
        AppType::Claude => {
            let path = crate::config::get_claude_settings_path();
            let text = std::fs::read_to_string(&path)
                .map_err(|e| AppError::Config(format!("读取 live settings.json 失败: {e}")))?;
            let config: Value = serde_json::from_str(&text)
                .map_err(|e| AppError::Config(format!("解析 live settings.json 失败: {e}")))?;
            SecretExtractor::extract_with_meta(id, app_type, &config, meta_field.as_deref())?
        }
        AppType::Codex => {
            // 合成提取源：auth.json 根 OPENAI_API_KEY + config.toml 全文，
            // 与 extract_codex 期望的 settings_config 形状对齐。
            let auth_text = std::fs::read_to_string(crate::codex_config::get_codex_auth_path())
                .unwrap_or_default();
            let auth: Value = serde_json::from_str(&auth_text).unwrap_or(Value::Null);
            let key = auth.get("OPENAI_API_KEY").and_then(Value::as_str);
            let config_text = std::fs::read_to_string(crate::codex_config::get_codex_config_path())
                .unwrap_or_default();
            let synthetic = serde_json::json!({
                "auth": { "OPENAI_API_KEY": key },
                "config": config_text,
            });
            SecretExtractor::extract(id, app_type, &synthetic)?
        }
        _ => return Ok(false),
    };
    // 与启动剥离的 pending 判据对齐：只有钥匙类明文才算待导入（非敏感 base_url 不算）。
    let had_plaintext = extracted.secrets.api_key.is_some()
        || !extracted.secrets.extra_env.is_empty()
        || extracted
            .secrets
            .base_url
            .as_ref()
            .is_some_and(|url| crate::secrets::is_credential_bearing_url(url.as_str()));
    if !had_plaintext {
        return Ok(false);
    }
    // ① 钥匙进 vault（merge 语义；非敏感 base_url 由 store_provider_bundle 拆去端点表）。
    store_provider_bundle(state, app_type, id, &extracted.secrets, true, None)?;
    // ② refs 已登记 api_key，就地剥离现在满足前提。
    let outcome = match app_type {
        AppType::Claude => live::strip_claude_live_plaintext_in_place(true)?,
        AppType::Codex => live::strip_codex_live_plaintext_in_place(true)?,
        _ => crate::codex_config::PlaintextStripOutcome::Clean,
    };
    log::info!(
        "✓ 已导入 {}/{id} 的 live 明文并剥离（{outcome:?}）",
        app_type.as_str()
    );
    Ok(true)
}

pub fn cleanup_orphan_secrets(state: &AppState) -> Result<usize, AppError> {
    // F3-8：1P 模式不碰凭据管理器与 known_secret_targets（凭据管理器残留由 F2-1
    // 的专用按钮处理），改走 1P 孤儿清单 + 用户确认的候选条目归档。
    if crate::settings::is_onepassword_backend() {
        return cleanup_onepassword_orphans(state, &[]);
    }
    cleanup_windows_orphan_secrets(state)
}

/// Windows（凭据管理器）模式的孤儿清理——`cleanup_orphan_secrets` 的原有实现。
fn cleanup_windows_orphan_secrets(state: &AppState) -> Result<usize, AppError> {
    let mut targets = crate::secrets::load_known_targets(state.db.as_ref())?;
    let mut expected = Vec::new();
    for app in [AppType::Claude, AppType::Codex, AppType::Pi] {
        let providers = state.db.get_all_providers(app.as_str())?;
        for id in providers.keys() {
            expected.push(crate::secrets::provider_target_prefix(&app, id));
        }
    }
    // §5.4：应用级条目（WebDAV / S3）不在任何 provider 前缀下，且 keyring 无法
    // 枚举，known_secret_targets 也不覆盖它们 → 这里按附录 B 的固定名字直接探测，
    // 对应同步配置已不存在时才判定为孤儿。
    let settings = crate::settings::get_settings();
    let app_level: [(crate::secrets::SecretTarget, bool); 3] = {
        let has_webdav = settings.webdav_sync.is_some();
        let has_s3 = settings.s3_sync.is_some();
        [
            (
                crate::secrets::SecretTarget::app("webdav", "password"),
                has_webdav,
            ),
            (
                crate::secrets::SecretTarget::app("s3", "access_key_id"),
                has_s3,
            ),
            (
                crate::secrets::SecretTarget::app("s3", "secret_access_key"),
                has_s3,
            ),
        ]
    };
    let mut removed = 0;
    for (target, configured) in app_level {
        if configured {
            continue;
        }
        match futures::executor::block_on(state.secrets.delete(&target)) {
            Ok(()) => removed += 1,
            Err(e) => log::warn!("清理应用级孤儿凭据失败 {}: {e}", target.to_target_string()),
        }
    }
    let mut kept = Vec::new();
    for target_str in targets.drain(..) {
        let still_needed = expected.iter().any(|prefix| target_str.starts_with(prefix));
        if still_needed {
            kept.push(target_str);
            continue;
        }
        if let Some(target) = parse_secret_target(&target_str) {
            if let Err(e) = futures::executor::block_on(state.secrets.delete(&target)) {
                log::warn!("清理孤儿凭据失败 {target_str}: {e}");
                kept.push(target_str);
                continue;
            }
        }
        removed += 1;
    }
    crate::secrets::save_known_targets(state.db.as_ref(), &kept)?;
    Ok(removed)
}

/// F3-8：1Password 孤儿条目候选（给 UI 确认用；只有条目结构信息，不含值）。
///
/// SEC-03：`confidence` 区分两类候选——
/// - `confirmed`：组在「显式删除供应商后待重试」清单里，有本机删除记录背书；
/// - `needs_review`：结构列表不足以证明孤儿（新设备、引用丢失、另一设备尚未
///   同步到本机等），**默认不清理**，仅作人工核对候选。
#[derive(Debug, serde::Serialize)]
pub struct OnePasswordOrphan {
    pub item_id: String,
    pub title: String,
    pub updated_at: String,
    pub confidence: &'static str,
    pub reason: String,
}

/// SEC-03：孤儿候选判定核心（纯函数，与显示标题解耦——在用判定只看本机
/// `secret_refs` 登记的真实 item id，不再按标题字符串匹配；新格式
/// `<app>/<显示名>` 标题的在用条目因此不会被误列为孤儿）。
///
/// - `items`：vault 中带 cc-switch 标签的全部条目（不含值）；
/// - `in_use_item_ids`：本机当前 vault 的全部真实 item id（`secret_refs` 行 +
///   待重试清单解析出的组也经它确认）；
/// - `pending_group_keys`：显式删除后待重试的组键（`<app>/<id>` / `app/sync`）；
/// - `provider_exists`：该组键对应的供应商当前是否存在于本机 DB。
fn orphan_candidates(
    items: &[crate::secrets::OpItemListEntry],
    in_use_item_ids: &std::collections::HashSet<String>,
    pending_group_keys: &[String],
    provider_exists: impl Fn(&crate::secrets::SecretGroup) -> bool,
) -> Vec<OnePasswordOrphan> {
    use crate::secrets::parse_group_from_title;
    let mut out = Vec::new();
    for item in items {
        let id = item.id.as_str();
        // 在用判定只看 item id：任何被本机引用登记的条目绝不是孤儿。
        if id.is_empty() || in_use_item_ids.contains(id) {
            continue;
        }
        // AppSync 组始终视为在用（应用级同步秘密，不受供应商增删影响）。
        if parse_group_from_title(&item.title)
            .is_some_and(|g| g == crate::secrets::SecretGroup::AppSync)
        {
            continue;
        }
        let group = parse_group_from_title(&item.title);
        let pending = group.as_ref().is_some_and(|g| {
            let crate::secrets::SecretGroup::Provider { app, provider_id } = g else {
                return false;
            };
            pending_group_keys
                .iter()
                .any(|k| k == &format!("{}/{}", app.as_str(), provider_id))
        });
        let (confidence, reason) = if pending {
            (
                "confirmed",
                "已删除供应商的待重试条目（本机有显式删除记录）".to_string(),
            )
        } else {
            // 无本机删除记录：另一设备可能仍在用（引用尚未同步到本机、或该
            // provider 在本机无引用行）。只列人工核对候选，不自动归档。
            let exists = group.as_ref().map(&provider_exists).unwrap_or(false);
            let reason = if exists {
                "供应商仍存在但本机无该条目的引用：可能是引用丢失或尚未同步，请先尝试「从 1Password 重建引用」".to_string()
            } else {
                "本机没有该条目的归属或删除记录：可能属于其他设备，请核对后再手动处理".to_string()
            };
            ("needs_review", reason)
        };
        out.push(OnePasswordOrphan {
            item_id: id.to_string(),
            title: item.title.clone(),
            updated_at: item.updated_at.clone(),
            confidence,
            reason,
        });
    }
    out
}

/// F3-8（P1-10）：`op item list --tags cc-switch`（不带 `--reveal`，不弹解锁）与
/// 本机引用对比，列出 vault 里有、本机没有在用引用的孤儿条目候选。用户在 UI
/// 确认后由 [`cleanup_onepassword_orphans`] 归档。
///
/// SEC-03：在用判定以本机 `secret_refs` 的真实 item id 集合为准（新标题、旧标题
/// 一视同仁）；无引用的条目默认按「待核验」列出，不称为可安全删除。
pub fn list_onepassword_orphans(state: &AppState) -> Result<Vec<OnePasswordOrphan>, AppError> {
    let vault = crate::secrets::onepassword_from_settings(state.db.clone())?;
    let items = vault
        .list_tagged_items()
        .map_err(crate::error::AppError::from)?;
    let in_use = in_use_item_ids(state)?;
    let pending = crate::settings::get_onepassword_orphans();
    Ok(orphan_candidates(&items, &in_use, &pending, |group| {
        let crate::secrets::SecretGroup::Provider { app, provider_id } = group else {
            return false;
        };
        state
            .db
            .get_provider_by_id(provider_id, app.as_str())
            .map(|p| p.is_some())
            .unwrap_or(false)
    }))
}

/// SEC-03：本机当前 vault 的「在用条目」item id 集合——`secret_refs` 中
/// vault 匹配、item id 非占位的全部行。不依赖标题字符串。
fn in_use_item_ids(state: &AppState) -> Result<std::collections::HashSet<String>, AppError> {
    let configured_vault = crate::settings::get_onepassword_vault().unwrap_or_default();
    let mut ids = std::collections::HashSet::new();
    if !crate::settings::is_onepassword_backend() {
        return Ok(ids);
    }
    for (app_str, provider_id, vault_id, item_id) in state.db.list_secret_ref_identities()? {
        if vault_id.is_empty() || vault_id != configured_vault {
            continue;
        }
        if item_id.is_empty() {
            continue;
        }
        let _ = app_str;
        let _ = provider_id;
        ids.insert(item_id);
    }
    Ok(ids)
}

/// F3-8：1P 模式孤儿清理。两部分：
/// ① 删除供应商时 `vault.delete` 失败记入 `onepassword_orphans` 的组键——逐组归档，
///    成功即出队（删除供应商本身已是用户的确认动作，无需再问）；
/// ② [`list_onepassword_orphans`] 列出、用户在 UI 确认过的条目按 item id 归档。
/// 返回本次归档的条目数。
///
/// SEC-03：①② 归档前都必须复检——前端确认列表不是后端永久授权凭证。
/// ① 检查该 provider 是否已被重新创建且引用恢复（恢复即出队、不再归档）；
/// ② 重新计算在用 item id 与当前合法候选集合，拒绝已变成在用、归属变化或
///    不在候选范围的 id；归档动作本身只按核验过的 id 走
///    [`OnePasswordVault::archive_item_by_id`]（保持归档而非永久删除）。
pub fn cleanup_onepassword_orphans(
    state: &AppState,
    confirmed_item_ids: &[String],
) -> Result<usize, AppError> {
    let mut removed = 0usize;

    // ① 处理删除供应商时记下的孤儿组。
    let mut orphans = crate::settings::get_onepassword_orphans();
    let original_count = orphans.len();
    let mut kept = Vec::new();
    for key in orphans.drain(..) {
        let Some((app_str, id)) = key.split_once('/') else {
            continue;
        };
        let Ok(app) = AppType::from_str(app_str) else {
            log::warn!("1Password 孤儿清单里有无法解析的组键 {key}，保留待人工处理");
            kept.push(key);
            continue;
        };
        // SEC-03 复检：provider 已重新创建且引用已恢复 = 该组重新在用，不得归档。
        let provider_back = state
            .db
            .get_provider_by_id(id, app.as_str())
            .map(|p| p.is_some())
            .unwrap_or(false);
        let ref_restored = state
            .db
            .get_secret_ref_identity(app.as_str(), id)
            .map(|r| r.is_some())
            .unwrap_or(false);
        if provider_back && ref_restored {
            log::info!("1Password 孤儿组 {key} 的供应商已重建且引用恢复，出队不再归档");
            continue;
        }
        let group = crate::secrets::SecretGroup::provider(app, id);
        match state.vault.delete(&group) {
            Ok(()) => removed += 1,
            Err(e) => {
                log::warn!("归档 1Password 孤儿条目 {key} 失败，保留待重试: {e}");
                kept.push(key);
            }
        }
    }
    if kept.len() != original_count {
        crate::settings::set_onepassword_orphans(kept)?;
    }

    // ② 用户确认过的候选条目。SEC-03：先重新取当前在用集合与候选集合——
    //    预览之后引用可能已恢复（重建、重新关联），这些 id 一律拒绝归档。
    if !confirmed_item_ids.is_empty() {
        let vault = crate::secrets::onepassword_from_settings(state.db.clone())?;
        let in_use = in_use_item_ids(state)?;
        let items = vault
            .list_tagged_items()
            .map_err(crate::error::AppError::from)?;
        let pending = crate::settings::get_onepassword_orphans();
        let legal: std::collections::HashSet<String> =
            orphan_candidates(&items, &in_use, &pending, |group| {
                let crate::secrets::SecretGroup::Provider { app, provider_id } = group else {
                    return false;
                };
                state
                    .db
                    .get_provider_by_id(provider_id, app.as_str())
                    .map(|p| p.is_some())
                    .unwrap_or(false)
            })
            .into_iter()
            .map(|o| o.item_id)
            .collect();
        for item_id in filter_archivable(confirmed_item_ids, &in_use, &legal) {
            match vault.archive_item_by_id(&item_id) {
                Ok(()) => removed += 1,
                Err(e) => log::warn!("归档 1Password 孤儿条目 {item_id} 失败: {e}"),
            }
        }
    }
    Ok(removed)
}

/// SEC-03：提交复检的过滤核心（纯函数）——从用户确认的 id 里剔除
/// 「已重新关联（在用）」与「不在当前合法候选范围（归属/状态已变化）」的 id。
/// 前端的确认列表不是后端的永久授权凭证。
fn filter_archivable(
    confirmed: &[String],
    in_use: &std::collections::HashSet<String>,
    legal: &std::collections::HashSet<String>,
) -> Vec<String> {
    confirmed
        .iter()
        .filter(|id| {
            if in_use.contains(id.as_str()) {
                log::warn!("拒绝归档 {}：该条目已被本机引用重新关联（在用）", id);
                return false;
            }
            if !legal.contains(id.as_str()) {
                log::warn!(
                    "拒绝归档 {}：不在当前合法候选范围内（归属或状态已变化）",
                    id
                );
                return false;
            }
            true
        })
        .cloned()
        .collect()
}

// Internal re-exports (pub(crate))
pub(crate) use live::sanitize_claude_settings_for_live;
pub(crate) use live::{
    normalize_provider_common_config_for_storage, provider_exists_in_live_config,
    strip_common_config_from_live_settings, sync_current_provider_for_app_to_live,
    write_live_with_common_config_for_state, write_live_with_common_config_for_state_no_vault,
};
pub(crate) use pi::apply_imported_configs_to_native;
pub(crate) use pi::flush_endpoint_vault_pending;
pub(crate) use pi::import_pi_plaintext_to_vault;
pub(crate) use pi::invalidate_native_fingerprint;

// Internal re-exports

/// 统一会话开关变更后，立即按新开关状态重写当前官方 Codex 供应商的
/// live 配置，使开关即时生效（无需等下一次切换）。
/// 当前供应商非官方（或不存在）时为 no-op：注入只作用于官方配置，
/// 第三方 live 配置不受开关影响。
pub fn reapply_current_codex_official_live(state: &AppState) -> Result<bool, AppError> {
    let current_id = ProviderService::current(state, AppType::Codex)?;
    if current_id.is_empty() {
        return Ok(false);
    }
    let providers = state.db.get_all_providers(AppType::Codex.as_str())?;
    let Some(provider) = providers.get(&current_id) else {
        return Ok(false);
    };
    if provider.category.as_deref() != Some("official")
        && !crate::codex_config::is_codex_official_provider(provider)
    {
        return Ok(false);
    }

    // 重写 live 会整体替换 config.toml（有意设计），[mcp_servers] 随之丢失，
    // 写完必须立刻从 DB 重新投影启用的 MCP。只投影 Codex 而非
    // sync_all_enabled：后者按 AppType::all() 顺序逐应用短路，排在 Codex
    // 前面的无关应用 live 损坏（如 ~/.claude.json 坏 JSON）会阻断 Codex
    // 的重投影，让刚被清掉的 [mcp_servers] 无人补回。
    // 投影失败降级为警告：走到这里 live 已按新开关状态落盘，开关事实上
    // 已生效；若把错误上抛，save_settings 会回滚开关设置，制造"设置=旧值、
    // live=新桶"的会话分裂——正是该回滚要防止的状态。MCP 投影可自愈
    // （下次切换 / 任一 MCP 启停操作都会重新投影）。
    write_live_with_common_config_for_state(state, &AppType::Codex, provider)?;
    if let Err(err) = McpService::sync_enabled_for_app(state, &AppType::Codex) {
        log::warn!("统一会话开关重写 live 后重投影 Codex MCP 失败（将在下次同步时自愈）: {err}");
    }
    Ok(true)
}

/// Provider business logic service
pub struct ProviderService;

/// Result of a provider switch operation, including any non-fatal warnings
#[derive(Debug, serde::Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SwitchResult {
    pub warnings: Vec<String>,
}

impl ProviderService {
    fn normalize_provider_if_claude(app_type: &AppType, provider: &mut Provider) {
        if matches!(app_type, AppType::Claude) {
            let mut v = provider.settings_config.clone();
            if normalize_claude_models_in_value(&mut v) {
                provider.settings_config = v;
            }
        }
    }

    /// Check whether a provider exists in live config, tolerating parse errors
    /// only for providers that are explicitly marked as DB-only.
    fn check_live_config_exists(
        app_type: &AppType,
        provider_id: &str,
        live_config_managed: Option<bool>,
    ) -> Result<bool, AppError> {
        if live_config_managed == Some(false) {
            Ok(provider_exists_in_live_config(app_type, provider_id).unwrap_or(false))
        } else {
            provider_exists_in_live_config(app_type, provider_id)
        }
    }

    fn provider_live_config_managed(provider: &Provider) -> Option<bool> {
        provider
            .meta
            .as_ref()
            .and_then(|meta| meta.live_config_managed)
    }

    fn set_provider_live_config_managed(provider: &mut Provider, managed: bool) {
        provider
            .meta
            .get_or_insert_with(Default::default)
            .live_config_managed = Some(managed);
    }

    /// List all providers for an app type
    pub fn list(
        state: &AppState,
        app_type: AppType,
    ) -> Result<IndexMap<String, Provider>, AppError> {
        if app_type == AppType::Pi {
            return pi::list(state);
        }
        state.db.get_all_providers(app_type.as_str())
    }

    /// Get current provider ID
    ///
    /// 使用有效的当前供应商 ID（验证过存在性）。
    /// 优先从本地 settings 读取，验证后 fallback 到数据库的 is_current 字段。
    /// 这确保了云同步场景下多设备可以独立选择供应商，且返回的 ID 一定有效。
    ///
    /// 对于累加模式应用（Pi），不存在"当前供应商"概念，直接返回空字符串。
    pub fn current(state: &AppState, app_type: AppType) -> Result<String, AppError> {
        // Additive mode apps have no "current" provider concept
        if app_type.is_additive_mode() {
            return Ok(String::new());
        }
        crate::settings::get_effective_current_provider(&state.db, &app_type)
            .map(|opt| opt.unwrap_or_default())
    }

    /// Add a new provider
    pub fn add(
        state: &AppState,
        app_type: AppType,
        provider: Provider,
        add_to_live: bool,
    ) -> Result<bool, AppError> {
        if app_type == AppType::Pi {
            return pi::add(state, provider, add_to_live);
        }

        let mut provider = provider;
        // Normalize Claude model keys
        Self::normalize_provider_if_claude(&app_type, &mut provider);
        Self::validate_provider_settings(state, &app_type, &provider, None)?;
        normalize_provider_common_config_for_storage(state.db.as_ref(), &app_type, &mut provider)?;
        if app_type.is_additive_mode() {
            Self::set_provider_live_config_managed(&mut provider, add_to_live);
        }
        strip_and_store_provider_secrets(state, &app_type, &mut provider, false)?;

        // Save to database
        if let Err(e) = state.db.save_provider(app_type.as_str(), &provider) {
            // §5.4：写 DB 失败 → best-effort 撤掉刚写入的凭据条目，不留孤儿。
            delete_provider_secrets(state, &app_type, &provider.id);
            return Err(e);
        }

        // For other apps: Check if sync is needed (if this is current provider, or no current provider)
        let current = state.db.get_current_provider(app_type.as_str())?;
        if current.is_none() {
            // No current provider, set as current and sync.
            state
                .db
                .set_current_provider(app_type.as_str(), &provider.id)?;
            write_live_with_common_config_for_state(state, &app_type, &provider)?;
        }

        Ok(true)
    }

    /// Update a provider
    pub fn update(
        state: &AppState,
        app_type: AppType,
        original_id: Option<&str>,
        provider: Provider,
    ) -> Result<bool, AppError> {
        if app_type == AppType::Pi {
            return pi::update(state, original_id, provider);
        }

        // 编辑当前供应商会改 live 与环境变量，与切换是同一类互斥操作：按 app 取锁，
        // 避免与并发切换交错出 `managed_env_vars` 与实际变量不一致的中间态。
        let _switch_guard =
            futures::executor::block_on(state.switch_locks.lock_for_app(app_type.as_str()));

        let mut provider = provider;
        let original_id = original_id.unwrap_or(provider.id.as_str()).to_string();
        let provider_id_changed = original_id != provider.id;
        let existing_provider = state
            .db
            .get_provider_by_id(&original_id, app_type.as_str())?;
        // Normalize Claude model keys
        Self::normalize_provider_if_claude(&app_type, &mut provider);
        Self::validate_provider_settings(state, &app_type, &provider, Some(&original_id))?;
        normalize_provider_common_config_for_storage(state.db.as_ref(), &app_type, &mut provider)?;
        if matches!(app_type, AppType::Codex) && provider.category.as_deref() == Some("official") {
            crate::codex_config::strip_codex_unified_session_bucket_from_settings(
                &mut provider.settings_config,
            )?;
        }

        if provider_id_changed {
            if !app_type.is_additive_mode() {
                return Err(AppError::Message(
                    "Only additive-mode providers support changing provider key".to_string(),
                ));
            }

            let Some(existing_provider) = existing_provider else {
                return Err(AppError::Message(format!(
                    "Original provider '{}' does not exist in app '{}'",
                    original_id,
                    app_type.as_str()
                )));
            };

            let original_in_live = Self::check_live_config_exists(
                &app_type,
                &original_id,
                Self::provider_live_config_managed(&existing_provider),
            )?;
            if original_in_live {
                return Err(AppError::Message(
                    "Provider key cannot be changed after the provider has been added to the app config"
                        .to_string(),
                ));
            }

            let next_id_in_live = Self::check_live_config_exists(
                &app_type,
                &provider.id,
                Self::provider_live_config_managed(&existing_provider),
            )?;
            if state
                .db
                .get_provider_by_id(&provider.id, app_type.as_str())?
                .is_some()
                || next_id_in_live
            {
                return Err(AppError::Message(format!(
                    "Provider '{}' already exists in app '{}'",
                    provider.id,
                    app_type.as_str()
                )));
            }

            Self::set_provider_live_config_managed(&mut provider, false);
            strip_and_store_provider_secrets(state, &app_type, &mut provider, false)?;
            state.db.save_provider(app_type.as_str(), &provider)?;
            state.db.delete_provider(app_type.as_str(), &original_id)?;

            if crate::settings::get_current_provider(&app_type).as_deref() == Some(&original_id) {
                crate::settings::set_current_provider(&app_type, Some(provider.id.as_str()))?;
            }

            return Ok(true);
        }

        // For other apps: Check if this is current provider (use effective current, not just DB)
        let effective_current =
            crate::settings::get_effective_current_provider(&state.db, &app_type)?;
        let is_current = effective_current.as_deref() == Some(provider.id.as_str());

        // P3（安全方案 §7.2）：纯提取 → 分类 → 零调用配置分支 / 显式凭据分支。
        // 提取与持久化解耦：先剥离所有已知秘密得到 sanitized config，再由后端
        // 依据已保存状态（而非前端 dirty 提示）决定是否进入 vault 流程。
        let edit_field = provider
            .meta
            .as_ref()
            .and_then(|m| m.api_key_field.as_deref());
        let extracted = SecretExtractor::extract_with_meta(
            &provider.id,
            &app_type,
            &provider.settings_config,
            edit_field,
        )?;
        provider.settings_config = extracted.stripped;

        let credentials_changed =
            match classify_edit_secrets(state, &app_type, &provider.id, &extracted.secrets)? {
                EditSecretClassification::ConfigOnly => {
                    // §7.2-5：无凭据差异 → 只写 sanitized config；vault 的
                    // fetch/put/delete/status 全为 0，不改引用和端点缓存。
                    // 钥匙未变，也不重投环境变量（D-1 只针对密钥变化）。
                    state.db.save_provider(app_type.as_str(), &provider)?;
                    if is_current {
                        // §7.4：投影上下文显式禁取凭据，缓存 miss 不偷偷 fetch。
                        write_live_with_common_config_for_state_no_vault(
                            state, &app_type, &provider,
                        )?;
                        if let Err(err) = McpService::sync_enabled_for_app(state, &app_type) {
                            log::warn!(
                            "保存供应商后重投影 {app_type:?} MCP 失败（将在下次同步时自愈）: {err}"
                        );
                        }
                    }
                    return Ok(true);
                }
                EditSecretClassification::VaultRequired(secrets) => {
                    store_provider_bundle(
                        state,
                        &app_type,
                        &provider.id,
                        &secrets,
                        true,
                        Some(&provider.name),
                    )?;
                    true
                }
            };

        // 缺陷 D-1：编辑当前供应商的密钥后必须按新值重投环境变量，否则 live 里的
        // `$VAR` 引用与 Codex 的 env_key 仍解析到旧密钥。放在写 DB 之前：投递失败时
        // DB 与 live 都未改动，用户看到的「保存失败」与实际状态一致。
        // 零调用分支已在上方提前返回，只有凭据分支才会重投。
        if credentials_changed
            && is_current
            && Self::provider_has_stored_key(state, &app_type, &provider.id)?
        {
            let mut delivered = SwitchResult::default();
            Self::deliver_env_credentials(state, &app_type, &provider, &mut delivered)?;
            for warning in &delivered.warnings {
                log::warn!("编辑当前供应商后重投环境变量的提醒: {warning}");
            }
        }

        // Save to database。F1-3（P0-3）：编辑路径不做凭据回滚——该条目本来就存在，
        // 删光等于把用户已有的全部钥匙丢掉。vault 里已是新值、DB 还是旧行，暂时
        // 不一致但没有丢失，用户重新保存即可恢复一致。（新增路径的归档回滚见 `add`。）
        state.db.save_provider(app_type.as_str(), &provider)?;

        if is_current {
            write_live_with_common_config_for_state(state, &app_type, &provider)?;
            if let Err(err) = McpService::sync_enabled_for_app(state, &app_type) {
                log::warn!("保存供应商后重投影 {app_type:?} MCP 失败（将在下次同步时自愈）: {err}");
            }
        }

        Ok(true)
    }

    /// Delete a provider
    ///
    /// 同时检查本地 settings 和数据库的当前供应商，防止删除任一端正在使用的供应商。
    /// 对于累加模式应用（Pi），可以随时删除任意供应商，同时从 live 配置中移除。
    pub fn delete(state: &AppState, app_type: AppType, id: &str) -> Result<(), AppError> {
        if app_type == AppType::Pi {
            return pi::delete(state, id);
        }

        // For other apps: Check both local settings and database
        let local_current = crate::settings::get_current_provider(&app_type);
        let db_current = state.db.get_current_provider(app_type.as_str())?;

        if local_current.as_deref() == Some(id) || db_current.as_deref() == Some(id) {
            return Err(AppError::Message(
                "无法删除当前正在使用的供应商".to_string(),
            ));
        }

        // §5.3.3 第 3 条：先移除托管的用户环境变量，再删凭据条目，最后删 DB 行。
        Self::release_provider_managed_env(state, &app_type, id);
        delete_provider_secrets(state, &app_type, id);
        state.db.delete_provider(app_type.as_str(), id)
    }

    /// §5.3.3 第 3 条：删除供应商 / Pi 移除时，先移除它托管的用户环境变量并更新
    /// `managed_env_vars`，避免孤儿变量残留在 `HKCU\Environment`。best-effort。
    pub(crate) fn release_provider_managed_env(state: &AppState, app_type: &AppType, id: &str) {
        use crate::env_delivery::ManagedEnvVars;
        match ManagedEnvVars::load(&state.db) {
            Ok(mut managed) => {
                let vars = managed.take_vars_for_provider(app_type.as_str(), id);
                if !vars.is_empty() {
                    let sink = state.env_sink.clone();
                    for name in &vars {
                        if let Err(e) = sink.remove(name) {
                            log::warn!("删除供应商时移除环境变量 {name} 失败: {e}");
                        }
                    }
                    if let Err(e) = managed.save(&state.db) {
                        log::warn!("删除供应商时更新 managed_env_vars 失败: {e}");
                    }
                    if let Err(e) = sink.broadcast() {
                        log::warn!("删除供应商后广播 WM_SETTINGCHANGE 失败: {e}");
                    }
                }
            }
            Err(e) => log::warn!("读取 managed_env_vars 以清理供应商失败: {e}"),
        }
    }

    /// Remove provider from live config only (for additive mode apps like Pi)
    ///
    /// Does NOT delete from database - provider remains in the list.
    /// This is used when user wants to "remove" a provider from active config
    /// but keep it available for future use.
    pub fn remove_from_live_config(
        state: &AppState,
        app_type: AppType,
        id: &str,
    ) -> Result<(), AppError> {
        if app_type == AppType::Pi {
            return pi::remove(state, id);
        }

        Err(AppError::Message(format!(
            "App {} does not support remove from live config",
            app_type.as_str()
        )))
    }

    /// Switch to a provider
    ///
    /// Switch flow:
    /// 1. Validate target provider exists
    /// 2. Check if proxy takeover mode is active AND proxy server is running
    /// 3. If takeover mode active: hot-switch proxy target and refresh proxy-safe Live labels
    /// 4. If normal mode:
    ///    a. **Backfill mechanism**: Backfill current live config to current provider
    ///    b. Update local settings current_provider_xxx (device-level)
    ///    c. Update database is_current (as default for new devices)
    ///    d. Write target provider config to live files
    ///    e. Sync MCP configuration
    pub fn switch(state: &AppState, app_type: AppType, id: &str) -> Result<SwitchResult, AppError> {
        if app_type == AppType::Pi {
            return pi::enable(state, id);
        }

        // Check if provider exists
        let providers = state.db.get_all_providers(app_type.as_str())?;
        providers
            .get(id)
            .ok_or_else(|| AppError::Message(format!("供应商 {id} 不存在")))?;

        // Provider switches mutate live config. Serialize them per app.
        let _switch_guard =
            futures::executor::block_on(state.switch_locks.lock_for_app(app_type.as_str()));

        Self::switch_normal(state, app_type, id, &providers)
    }

    /// Normal switch flow (non-proxy mode)
    fn switch_normal(
        state: &AppState,
        app_type: AppType,
        id: &str,
        providers: &indexmap::IndexMap<String, Provider>,
    ) -> Result<SwitchResult, AppError> {
        let provider = providers
            .get(id)
            .ok_or_else(|| AppError::Message(format!("供应商 {id} 不存在")))?;

        let mut result = SwitchResult::default();

        // Backfill: Backfill current live config to current provider
        // Use effective current provider (validated existence) to ensure backfill targets valid provider
        let current_id = crate::settings::get_effective_current_provider(&state.db, &app_type)?;
        if let Some(current_id) = current_id {
            if current_id != id {
                // Additive mode apps - all providers coexist in the same file,
                // no backfill needed (backfill is for exclusive mode apps Claude/Codex)
                if !app_type.is_additive_mode() {
                    // Only backfill when switching to a different provider
                    if let Ok(live_config) = live::read_live_settings_for_backfill(app_type.clone())
                    {
                        if let Some(mut current_provider) = providers.get(&current_id).cloned() {
                            // 切走前先把 live 里的可共享改动（含用户直接在应用内
                            // 装插件/加 hook/改偏好）同步进通用配置片段，再做剥离回填。
                            // 详见 sync_common_config_snippet_from_live 的文档。
                            Self::sync_common_config_snippet_from_live(
                                state,
                                &app_type,
                                &current_provider,
                                &live_config,
                                &mut result,
                            );

                            current_provider.settings_config =
                                strip_common_config_from_live_settings(
                                    state.db.as_ref(),
                                    &app_type,
                                    &current_provider,
                                    live_config,
                                );
                            // §3.1-5 / §5.4-③：回填是密钥进入 DB 的入口之一，
                            // 必须先过提取器——live 里用户手改的字面量密钥剥进
                            // 凭据管理器，DB 行只留 stripped 部分。
                            strip_and_store_provider_secrets(
                                state,
                                &app_type,
                                &mut current_provider,
                                true,
                            )?;
                            if let Err(e) =
                                state.db.save_provider(app_type.as_str(), &current_provider)
                            {
                                log::warn!("Backfill failed: {e}");
                                result
                                    .warnings
                                    .push(format!("backfill_failed:{current_id}"));
                            }
                        }
                    }
                }
            }
        }

        {
            // Codex: validate the live projection before committing current —
            // the write-layer safety gates can refuse the switch, and a
            // refusal after current moved would let the next switch backfill
            // the old live config into the new provider's DB row.
            if matches!(app_type, AppType::Codex) {
                live::preflight_codex_live_write_for_state(state, provider)?;
            }

            Self::preflight_env_delivery(state, &app_type, provider)?;

            // §5.4：顺序是 ⑤ 投递环境变量 → ⑥ 写 live → ⑦ 移动 is_current。
            // 这样 ⑥ 失败时 current 还没动，可以撤销 ⑤ 回到切换前状态；
            // 反过来（先移动 current 再写 live）会让下一次切换把旧 live
            // 回填进新供应商的行。
            let previous_current = if app_type.is_additive_mode() {
                None
            } else {
                crate::settings::get_effective_current_provider(&state.db, &app_type)?
            };

            Self::deliver_env_credentials(state, &app_type, provider, &mut result)?;

            if let Err(e) = write_live_with_common_config_for_state(state, &app_type, provider) {
                // ⑥ 失败 → 回滚 ⑤：撤下刚写入的变量，并把上一个当前供应商的变量投回去。
                Self::undo_env_delivery(state, &app_type, provider);
                if let Some(prev_id) = previous_current.as_deref() {
                    if prev_id != id {
                        if let Some(prev) = providers.get(prev_id) {
                            let mut discard = SwitchResult::default();
                            if let Err(re) =
                                Self::deliver_env_credentials(state, &app_type, prev, &mut discard)
                            {
                                log::warn!("回滚投递上一个供应商 {prev_id} 的环境变量失败: {re}");
                            }
                        }
                    }
                }
                return Err(e);
            }

            // Additive mode apps skip setting is_current (no such concept).
            if !app_type.is_additive_mode() {
                crate::settings::set_current_provider(&app_type, Some(id))?;
                state.db.set_current_provider(app_type.as_str(), id)?;
            }
        }
        // 切换重写了目标应用的 live，只重投影该应用的 MCP（Codex 的
        // [mcp_servers] 与 live 同文件，整体替换后必须补回；其余应用的
        // MCP 文件独立于 live，投影是幂等维护）。不用全量 sync_all_enabled：
        // 无关应用的 live 损坏（如 ~/.claude.json 坏 JSON）不该阻断切换。
        // 走到这里 DB is_current 与 live 都已落盘，切换事实上已成功；
        // 投影失败上抛会让前端报"切换失败"制造分裂假象，故降级为警告
        // （MCP 投影可自愈：下次切换 / 任一 MCP 启停都会重新投影）。
        if let Err(err) = McpService::sync_enabled_for_app(state, &app_type) {
            log::warn!("切换供应商后重投影 {app_type:?} MCP 失败（将在下次同步时自愈）: {err}");
        }

        Ok(result)
    }

    /// Sync current provider to live configuration (re-export)
    pub fn sync_current_to_live(state: &AppState) -> Result<(), AppError> {
        sync_current_to_live(state)
    }

    pub fn sync_current_provider_for_app(
        state: &AppState,
        app_type: AppType,
    ) -> Result<(), AppError> {
        if app_type.is_additive_mode() {
            return sync_current_provider_for_app_to_live(state, &app_type);
        }

        let current_id =
            match crate::settings::get_effective_current_provider(&state.db, &app_type)? {
                Some(id) => id,
                None => return Ok(()),
            };

        let providers = state.db.get_all_providers(app_type.as_str())?;
        let Some(provider) = providers.get(&current_id) else {
            return Ok(());
        };

        write_live_with_common_config_for_state(state, &app_type, provider)?;

        McpService::sync_enabled_for_app(state, &app_type)
    }

    pub fn migrate_legacy_common_config_usage(
        state: &AppState,
        app_type: AppType,
        legacy_snippet: &str,
    ) -> Result<(), AppError> {
        if app_type.is_additive_mode() || legacy_snippet.trim().is_empty() {
            return Ok(());
        }

        let providers = state.db.get_all_providers(app_type.as_str())?;

        for provider in providers.values() {
            if provider
                .meta
                .as_ref()
                .and_then(|meta| meta.common_config_enabled)
                .is_some()
            {
                continue;
            }

            if !live::provider_uses_common_config(&app_type, provider, Some(legacy_snippet)) {
                continue;
            }

            let mut updated_provider = provider.clone();
            updated_provider
                .meta
                .get_or_insert_with(Default::default)
                .common_config_enabled = Some(true);

            match live::remove_common_config_from_settings(
                &app_type,
                &updated_provider.settings_config,
                legacy_snippet,
            ) {
                Ok(settings) => updated_provider.settings_config = settings,
                Err(err) => {
                    log::warn!(
                        "Failed to normalize legacy common config for {} provider '{}': {err}",
                        app_type.as_str(),
                        updated_provider.id
                    );
                }
            }

            state
                .db
                .save_provider(app_type.as_str(), &updated_provider)?;
        }

        Ok(())
    }

    pub(crate) fn deliver_env_credentials_pub(
        state: &AppState,
        app_type: &AppType,
        provider: &Provider,
        result: &mut SwitchResult,
    ) -> Result<(), AppError> {
        Self::deliver_env_credentials(state, app_type, provider, result)
    }

    /// 凭据管理器里是否存有该供应商的 `api_key`。投递与 live 写入两侧都要按这个判定：
    /// 无密钥的官方卡不该因为「投不出值」而失败，有密钥才走间接引用注入。
    pub(crate) fn provider_has_stored_key(
        state: &AppState,
        app_type: &AppType,
        provider_id: &str,
    ) -> Result<bool, AppError> {
        // §6.2/4.3：查 secret_refs 字段名清单是否含 api_key，不碰 vault / store。
        let has = state
            .db
            .get_secret_ref_fields(app_type.as_str(), provider_id)?
            .map(|fields| fields.iter().any(|f| f == crate::secrets::FIELD_API_KEY))
            .unwrap_or(false);
        Ok(has)
    }

    /// 缺陷 D-4：把注册表里「按我们的命名规则、但库里没有登记」的用户环境变量认领回来。
    ///
    /// 只用于迁移后的 live 自动重写：回滚到旧版再升级时，旧库里没有 `managed_env_vars`，
    /// 而 `HKCU\Environment` 还留着上一轮投递的值，冲突检测判成 foreign 后每次启动都拒写，
    /// 用户没有任何出路。手动切换不调用本函数，保留「不许覆盖用户自设同名变量」的保护。
    pub(crate) fn adopt_unregistered_managed_env(state: &AppState) -> Result<(), AppError> {
        use crate::env_delivery::ManagedEnvVars;

        let sink = state.env_sink.clone();
        let mut managed = ManagedEnvVars::load(&state.db)?;
        let mut adopted: Vec<String> = Vec::new();

        for app_type in [AppType::Claude, AppType::Codex, AppType::Pi] {
            let Some(id) = crate::settings::get_effective_current_provider(&state.db, &app_type)?
            else {
                continue;
            };
            let providers = state.db.get_all_providers(app_type.as_str())?;
            let Some(provider) = providers.get(&id) else {
                continue;
            };
            let mut warnings = Vec::new();
            // §6.1：恢复例程为尽力而为，某供应商取包失败则跳过他。
            let Ok(secrets) = Self::fetch_provider_secrets(state, &app_type, &id) else {
                continue;
            };
            let Ok(pending) =
                Self::provider_env_pairs(&app_type, provider, &secrets, &mut warnings)
            else {
                continue;
            };
            for (name, _) in pending {
                if managed.is_managed(&name) || sink.get(&name)?.is_none() {
                    continue;
                }
                managed.register(&name, app_type.as_str(), &id);
                adopted.push(name);
            }
        }

        if adopted.is_empty() {
            return Ok(());
        }
        managed.save(&state.db)?;
        for name in &adopted {
            log::warn!("认领上一轮投递的用户环境变量 {name}（回滚后再升级的常见形态）");
        }
        Ok(())
    }

    pub(crate) fn preflight_env_delivery(
        state: &AppState,
        app_type: &AppType,
        provider: &Provider,
    ) -> Result<(), AppError> {
        // 严格模式（P2 起逐 app 判定）不写环境变量，冲突预检既无意义也不该挡住切换。
        if crate::settings::strict_for(app_type) {
            return Ok(());
        }
        let mut warnings = SwitchResult::default();
        let pending = Self::collect_pending_env(state, app_type, provider, &mut warnings)?;
        Self::reject_if_env_conflicts(state, app_type, &pending)
    }

    fn reject_if_env_conflicts(
        state: &AppState,
        app_type: &AppType,
        pending: &[(String, Zeroizing<String>)],
    ) -> Result<(), AppError> {
        use crate::env_delivery::ManagedEnvVars;
        let sink = state.env_sink.clone();
        let sink = sink.as_ref();
        let managed = ManagedEnvVars::load(&state.db)?;
        let mut conflicts = Vec::new();
        for (name, value) in pending {
            if let Some(conflict) =
                crate::env_delivery::check_conflict(sink, &managed, name, value.as_str())?
            {
                conflicts.push(conflict);
            }
        }
        if conflicts.is_empty() {
            return Ok(());
        }
        let payload = serde_json::json!({
            "code": "ENV_CONFLICT",
            "app": app_type.as_str(),
            "conflicts": conflicts,
        });
        Err(AppError::Message(payload.to_string()))
    }

    /// Deliver credentials via environment variables after switching provider
    fn deliver_env_credentials(
        state: &AppState,
        app_type: &AppType,
        provider: &Provider,
        result: &mut SwitchResult,
    ) -> Result<(), AppError> {
        use crate::env_delivery::ManagedEnvVars;

        let sink = state.env_sink.clone();
        let sink = sink.as_ref();

        let mut managed = ManagedEnvVars::load(&state.db)?;
        let old_vars = if matches!(app_type, AppType::Pi) {
            Vec::new()
        } else {
            managed.vars_for_app(app_type.as_str())
        };

        // B5/P2 严格投递模式（逐 app 判定）：绝不把该应用密钥写进 `HKCU\Environment`。切换时把
        // 该应用此前投递过的变量一并收回（开关侧已按需清理，这里是切换侧的安全网），
        // 只保留调用方的 live 文件更新；密钥改由「打开终端」或 `ccs env` 注入到其自起
        // 的终端进程。刻意不 collect/write pending。
        if crate::settings::strict_for(app_type) {
            for var_name in &old_vars {
                if let Err(e) = sink.remove(var_name) {
                    log::warn!("严格模式收回环境变量 {var_name} 失败: {e}");
                    result
                        .warnings
                        .push(format!("env_cleanup_failed:{var_name}"));
                }
                managed.unregister(var_name);
            }
            managed.save(&state.db)?;
            if !old_vars.is_empty() {
                if let Err(e) = sink.broadcast() {
                    log::warn!("严格模式收回环境变量后广播失败: {e}");
                }
            }
            return Ok(());
        }

        let pending = Self::collect_pending_env(state, app_type, provider, result)?;
        Self::reject_if_env_conflicts(state, app_type, &pending)?;

        // 删除旧变量前，先把它们在 sink 里的当前值读出暂存（`Zeroizing`），供写新变量
        // 失败时回滚——否则会出现"旧的已删、新的没写上"的中间态（T-7 事务性）。
        let mut old_snapshot: Vec<(String, Zeroizing<String>)> = Vec::new();
        for var_name in &old_vars {
            match sink.get(var_name) {
                Ok(Some(value)) => old_snapshot.push((var_name.clone(), value)),
                Ok(None) => {}
                Err(e) => log::warn!("读取待删除环境变量 {var_name} 失败（回滚将不完整）: {e}"),
            }
        }

        for var_name in &old_vars {
            if let Err(e) = sink.remove(var_name) {
                log::warn!("Failed to remove env var {var_name}: {e}");
                result
                    .warnings
                    .push(format!("env_cleanup_failed:{var_name}"));
            }
            managed.unregister(var_name);
        }

        // 逐条写新变量。任一条 `sink.set` 失败即回滚到切换前：撤掉本轮已写入的新变量、
        // 把旧变量原值写回、广播，然后原样上抛。此路径刻意不调 `managed.save`——
        // 登记要到全部写成功后才落库，DB 仍是切换前状态（旧变量归上一供应商所有），
        // 与回滚后的 sink 天然一致，无需再改登记。
        let mut written: Vec<String> = Vec::new();
        for (name, value) in &pending {
            // S2：投递给用户环境变量（含 CC_SWITCH_*）的值登记进会话密钥表，
            // 之后任何日志与导出文本命中它都会被脱敏 / 拦下。
            // 缺陷 D-2：Base URL 不是密钥，登记它会让护栏把 `providers.website_url`
            // 这类合法的官网地址判成泄漏，用户同步被永久拒绝。例外是写成
            // `https://user:pass@host` 的地址——那种形态本身就含凭据，照旧登记。
            let holds_credential = !name.ends_with("_BASE_URL") || value.contains('@');
            if holds_credential {
                crate::secrets::scan::note_session_secret(value.as_str());
            }
            if let Err(e) = sink.set(name, value) {
                for w in &written {
                    if let Err(undo) = sink.remove(w) {
                        log::warn!("回滚环境变量投递失败（移除已写入 {w}）: {undo}");
                    }
                }
                for (n, v) in &old_snapshot {
                    // 与主循环同一 D-2 规则：明文 Base URL 不登记为会话密钥。
                    if !n.ends_with("_BASE_URL") || v.contains('@') {
                        crate::secrets::scan::note_session_secret(v.as_str());
                    }
                    if let Err(undo) = sink.set(n, v) {
                        log::warn!("回滚环境变量投递失败（写回旧值 {n}）: {undo}");
                    }
                }
                if let Err(undo) = sink.broadcast() {
                    log::warn!("回滚环境变量投递后广播失败: {undo}");
                }
                return Err(e);
            }
            managed.register(name, app_type.as_str(), &provider.id);
            written.push(name.clone());
        }

        managed.save(&state.db)?;
        if let Err(e) = sink.broadcast() {
            log::warn!("Failed to broadcast WM_SETTINGCHANGE: {e}");
        }

        Ok(())
    }

    /// B5：开启严格投递模式时把已投递到 `HKCU\Environment` 的所有受管变量全部收回并清空登记，
    /// 让开关即时生效（不必等下一次切换）。按登记的名字逐条 remove，不枚举注册表（keyring 式
    /// 枚举有越界风险），因此只清理 cc-switch 自己投递过的变量，不碰用户自设的同名变量之外的东西。
    /// F4-6（P2-7）：与 `ccs env --clear` 一致——**删成功才摘登记**，删除失败的变量保留
    /// 登记以待下次清理，避免「值还在、登记没了」的孤儿态。
    pub(crate) fn purge_all_env_delivery(state: &AppState) -> Result<(), AppError> {
        use crate::env_delivery::ManagedEnvVars;

        let sink = state.env_sink.as_ref();
        let mut managed = ManagedEnvVars::load(&state.db)?;
        let names: Vec<String> = managed.entries.keys().cloned().collect();
        let mut removed = 0usize;
        for name in &names {
            match sink.remove(name) {
                Ok(()) => {
                    managed.unregister(name);
                    removed += 1;
                }
                Err(e) => log::warn!("严格模式清理环境变量 {name} 失败，保留登记待下次清理: {e}"),
            }
        }
        managed.save(&state.db)?;
        if removed > 0 {
            if let Err(e) = sink.broadcast() {
                log::warn!("严格模式清理环境变量后广播失败: {e}");
            }
        }
        Ok(())
    }

    /// 2.2 方案 P2：只收回指定 app 集合的已投递变量（分级清理粒度跟分级走）。
    /// 不误伤仍宽松的其他 app 变量。Pi 按 app 圈定 = 收回该 app 下全部 Pi 变量（additive
    /// 场景由 P1 的 `ccs env pi <id>` 提供 per-provider 精确清理，这里是全局/按-app 开关侧）。
    /// F4-6：同 [`Self::purge_all_env_delivery`]——删成功才摘登记。
    pub fn purge_env_delivery_for_apps(state: &AppState, apps: &[String]) -> Result<(), AppError> {
        use crate::env_delivery::ManagedEnvVars;

        let sink = state.env_sink.as_ref();
        let mut managed = ManagedEnvVars::load(&state.db)?;
        let names: Vec<String> = managed
            .entries
            .iter()
            .filter(|(_, entry)| apps.iter().any(|a| a == &entry.app))
            .map(|(name, _)| name.clone())
            .collect();
        let mut removed = 0usize;
        for name in &names {
            match sink.remove(name) {
                Ok(()) => {
                    managed.unregister(name);
                    removed += 1;
                }
                Err(e) => log::warn!("严格模式清理环境变量 {name} 失败，保留登记待下次清理: {e}"),
            }
        }
        managed.save(&state.db)?;
        if removed > 0 {
            if let Err(e) = sink.broadcast() {
                log::warn!("严格模式清理环境变量后广播失败: {e}");
            }
        }
        Ok(())
    }

    /// §5.4：live 写入失败后撤销刚投递的变量（含登记），避免半态。
    fn undo_env_delivery(state: &AppState, app_type: &AppType, provider: &Provider) {
        use crate::env_delivery::ManagedEnvVars;
        let sink = state.env_sink.clone();
        let mut managed = match ManagedEnvVars::load(&state.db) {
            Ok(managed) => managed,
            Err(e) => {
                log::warn!("回滚环境变量投递失败（读取登记）: {e}");
                return;
            }
        };
        let names = managed.take_vars_for_provider(app_type.as_str(), &provider.id);
        for name in &names {
            if let Err(e) = sink.remove(name) {
                log::warn!("回滚环境变量投递失败（移除 {name}）: {e}");
            }
        }
        if let Err(e) = managed.save(&state.db) {
            log::warn!("回滚环境变量投递失败（写登记）: {e}");
        }
        if let Err(e) = sink.broadcast() {
            log::warn!("回滚环境变量投递后广播失败: {e}");
        }
    }

    fn collect_pending_env(
        state: &AppState,
        app_type: &AppType,
        provider: &Provider,
        result: &mut SwitchResult,
    ) -> Result<Vec<(String, Zeroizing<String>)>, AppError> {
        let mut warnings = Vec::new();
        // §6.1：入口一次性取整包，再交给纯函数映射。
        let secrets = Self::fetch_provider_secrets(state, app_type, &provider.id)?;
        let pending = Self::provider_env_pairs(app_type, provider, &secrets, &mut warnings)?;
        result.warnings.extend(warnings);
        Ok(pending)
    }

    /// 单一真源地计算「某供应商应投递的用户环境变量 (name → value)」。
    /// 切换投递与内置「打开终端」共用，保证非当前供应商也能拿到自己的密钥（§5.3.4）。
    ///
    /// §6.1：纯函数——不读 vault，只把已取好的整包 `secrets` 映射成待投递的环境变量。
    /// 「取」由 [`Self::fetch_provider_secrets`] 在流程入口一次性完成（原则 1）。
    pub(crate) fn provider_env_pairs(
        app_type: &AppType,
        provider: &Provider,
        secrets: &ProviderSecrets,
        warnings: &mut Vec<String>,
    ) -> Result<Vec<(String, Zeroizing<String>)>, AppError> {
        let mut pending: Vec<(String, Zeroizing<String>)> = Vec::new();
        match app_type {
            AppType::Claude => {
                // 缺钥匙（整包里没有 api_key）是业务错误，交由调用方按“请先补全密钥”处理。
                let key = secrets
                    .api_key
                    .clone()
                    .ok_or_else(|| AppError::Message("请先补全密钥".to_string()))?;
                let field = provider
                    .meta
                    .as_ref()
                    .and_then(|m| m.api_key_field.as_deref())
                    .unwrap_or("ANTHROPIC_AUTH_TOKEN");
                pending.push((field.to_string(), key));
                if let Some(url) = secrets.base_url.clone() {
                    pending.push(("ANTHROPIC_BASE_URL".to_string(), url));
                }
                // extra_env（Claude 敏感 env）已在整包里，变量名即键名。
                for (name, value) in &secrets.extra_env {
                    pending.push((name.clone(), value.clone()));
                }
            }
            AppType::Codex => {
                let official = provider.category.as_deref() == Some("official")
                    || crate::codex_config::is_codex_official_provider(provider);
                let name = if official {
                    "OPENAI_API_KEY"
                } else {
                    "CC_SWITCH_CODEX_API_KEY"
                };
                match secrets.api_key.clone() {
                    Some(key) => pending.push((name.to_string(), key)),
                    None => {
                        // 无密钥供应商（header 认证 / preserved login）合法：
                        // 活性安全由 live 写入门控保证，这里只降级为告警。
                        // 回归保护：`provider_service_switch_codex_preserved_login_*` 两个
                        // 用例锁定了"自带 http_headers 认证放行、会回落 auth.json 才拒绝"，
                        // 这里不能改成一律拒绝。
                        log::warn!(
                            "Codex provider {} has no api_key in vault; \
                             relying on config-carried auth",
                            provider.id
                        );
                        warnings.push(format!("codex_missing_api_key:{}", provider.id));
                    }
                }
            }
            AppType::Pi => {
                match secrets.api_key.clone() {
                    Some(api_key) => {
                        pending.push((crate::secrets::pi_api_key_env_name(&provider.id), api_key));
                    }
                    None => {
                        log::warn!("Pi provider {} has no api_key in vault", provider.id);
                        warnings.push(format!("pi_missing_api_key:{}", provider.id));
                    }
                }
                if let Some(headers) = provider
                    .settings_config
                    .get("headers")
                    .and_then(Value::as_object)
                {
                    for (header_name, header_value) in headers {
                        if !crate::secrets::is_sensitive_config_key(header_name) {
                            continue;
                        }
                        let Some(val) = header_value.as_str() else {
                            continue;
                        };
                        if crate::secrets::is_literal_value(val) {
                            continue;
                        }
                        // Pi 敏感 header 存在整包 extra_env 里，键名即 header 名。
                        match secrets.extra_env.get(header_name) {
                            Some(secret) => {
                                pending.push((
                                    crate::secrets::pi_header_env_name(&provider.id, header_name),
                                    secret.clone(),
                                ));
                            }
                            None => {
                                warnings.push(format!(
                                    "pi_missing_header:{}:{header_name}",
                                    provider.id
                                ));
                            }
                        }
                    }
                }
            }
        }
        Ok(pending)
    }

    /// §6.1：唯一调用 `state.vault.fetch` 的地方。一次往返拿整包。
    /// 条目不存在 => 返回空 `ProviderSecrets`（让后续“缺钥匙”逻辑照旧工作）；
    /// 锁定/断网等 => 原样上抛（`VaultError` 经 `From` 归一到本地化 `AppError`）。
    pub(crate) fn fetch_provider_secrets(
        state: &AppState,
        app_type: &AppType,
        provider_id: &str,
    ) -> Result<ProviderSecrets, AppError> {
        let group =
            crate::secrets::SecretGroup::provider(app_type.clone(), provider_id.to_string());
        let bundle = state.vault.fetch(&group);
        let mut secrets = bundle.map(|b| b.map(|b| b.to_provider_secrets()).unwrap_or_default())?;
        // S4-2（P0-3 / §5.4 S4-2）：D3-B 之后 1Password 是端点的真源，本机缓存
        // 只是加速。D3-A 时期规则相反（缓存覆盖 vault 侧可能过期的旧副本），
        // D3-B 反转了真源却没有跟着改读取优先级——于是设备 B 上别的设备改过的
        // 端点永远压不过本机缓存，钥匙被发往旧主机。
        //
        // 1P 模式：vault 整包里有 base_url 就用它，并顺手把缓存校正到一致；vault
        // 没有才回落到缓存。凭据管理器模式维持「缓存优先」——该模式的凭据管理器里
        // 可能留着 D3-A 时期的旧副本，缓存才是本机权威（§9-6）。
        if crate::settings::is_onepassword_backend() {
            if let Some(url) = secrets.base_url.as_deref() {
                Self::reconcile_endpoint_cache(state, app_type, provider_id, url)?;
                return Ok(secrets);
            }
        }
        // F1-2（D3-A）：端点表命中时覆盖 vault 侧的 base_url（vault 里的可能是
        // 拆分前的旧副本）。端点表未命中且 vault 也没有时保持 None。
        // Claude 终端注入 ANTHROPIC_BASE_URL 的行为不变。
        if let Ok(Some(url)) = state
            .db
            .get_provider_endpoint(app_type.as_str(), provider_id)
        {
            secrets.base_url = Some(Zeroizing::new(url));
        }
        Ok(secrets)
    }

    /// S4-2（P0-3 / §9-7）：把本机端点缓存校正到 1Password 真值。
    ///
    /// 非敏感 URL → 写缓存（之后读取 0 次 op）；带凭据的 URL → 删缓存（敏感 URL
    /// 绝不进端点表，§9-7）。值相同时不写库：S1-3 的 `DO UPDATE … WHERE` 让
    /// upsert 幂等，这里先比较再写是为了连读的意图都省掉。
    /// 日志只记 app/id 这个结构定位，不记 URL（§9-13）。
    fn reconcile_endpoint_cache(
        state: &AppState,
        app_type: &AppType,
        provider_id: &str,
        vault_url: &str,
    ) -> Result<(), AppError> {
        let cached = state
            .db
            .get_provider_endpoint(app_type.as_str(), provider_id)?;
        if crate::secrets::is_credential_bearing_url(vault_url) {
            if cached.is_some() {
                state
                    .db
                    .delete_provider_endpoint(app_type.as_str(), provider_id)?;
                log::info!(
                    "S4-2：vault 端点带凭据，已删除本机端点缓存 {}/{}",
                    app_type.as_str(),
                    provider_id
                );
            }
            return Ok(());
        }
        if cached.as_deref() != Some(vault_url) {
            state
                .db
                .upsert_provider_endpoint(app_type.as_str(), provider_id, vault_url)?;
            log::info!(
                "S4-2：端点缓存已按 1Password 真值更新 {}/{}",
                app_type.as_str(),
                provider_id
            );
        }
        Ok(())
    }

    pub fn adopt_env_vars(
        state: &AppState,
        app_type: &AppType,
        provider_id: &str,
        names: &[String],
    ) -> Result<(), AppError> {
        // F3-3（P1-5 / 原方案陷阱 §12.8）：严格投递（含 1P 模式恒严格）下钥匙绝不写
        // `HKCU\Environment`——接管会 sink.set 写入真值，直接拒绝，不 fetch、不写注册表。
        if crate::settings::strict_for(app_type) {
            return Err(AppError::localized(
                "env_delivery.adopt_strict",
                "严格投递模式下无法接管环境变量：密钥不会写入用户环境变量",
                "Environment variables cannot be adopted in strict delivery mode",
            ));
        }
        use crate::env_delivery::ManagedEnvVars;
        let provider = state
            .db
            .get_provider_by_id(provider_id, app_type.as_str())?
            .ok_or_else(|| AppError::Message(format!("供应商 {provider_id} 不存在")))?;
        let sink = state.env_sink.clone();
        let sink = sink.as_ref();
        let mut managed = ManagedEnvVars::load(&state.db)?;

        // 接管 = 用我方凭据覆盖外来变量并登记所有权；值与切换投递同源。
        let mut warnings = Vec::new();
        let secrets = Self::fetch_provider_secrets(state, app_type, provider_id)?;
        let pending = Self::provider_env_pairs(app_type, &provider, &secrets, &mut warnings)?;
        let mut wrote = false;
        for name in names {
            managed.register(name, app_type.as_str(), provider_id);
            if let Some((_, value)) = pending.iter().find(|(k, _)| k == name) {
                sink.set(name, value)?;
                wrote = true;
            }
        }
        managed.save(&state.db)?;
        if wrote {
            if let Err(e) = sink.broadcast() {
                log::warn!("接管环境变量后广播失败: {e}");
            }
        }
        Ok(())
    }

    pub fn migrate_legacy_common_config_usage_if_needed(
        state: &AppState,
        app_type: AppType,
    ) -> Result<(), AppError> {
        if app_type.is_additive_mode() {
            return Ok(());
        }

        let Some(snippet) = state.db.get_config_snippet(app_type.as_str())? else {
            return Ok(());
        };

        if snippet.trim().is_empty() {
            return Ok(());
        }

        Self::migrate_legacy_common_config_usage(state, app_type, &snippet)
    }

    /// 切走某供应商前，把它 live 配置里的可共享部分重新提取并**整体替换**到
    /// 通用配置片段，使在 live 应用里直接做的改动不会因切换而丢失。
    ///
    /// 采用"整体重提取 + 替换"而非"只合并新增"，是为了同时覆盖三种情况：
    /// - **新增**：用户直接在应用里装了插件、加了 hook、改了 env/主题/权限等共享
    ///   偏好，被捕获进通用配置，切到别的供应商也带得过去；
    /// - **删除**：被删掉的键不在新提取结果里，于是从片段里消失、下次切换不会被
    ///   重新注入——否则会出现"插件怎么删也删不掉"的反直觉 bug；
    /// - **密钥安全**：提取器已剥掉 auth / model / endpoint，密钥永不进共享片段。
    ///
    /// 之所以"整体替换"是安全的：每次写 live 都会把当前片段合并进去，所以切走时
    /// 读到的 live 一定是"片段 + 本地改动"的超集，重提取只会丢掉用户真正删掉的键，
    /// 不会误删其它供应商共享的内容。
    ///
    /// **作用域**：Claude + Codex。Codex 提取器（`extract_codex_common_config`）
    /// 已剥离全部供应商专属与 cc-switch 注入内容：`model` / `model_provider` /
    /// 顶层 `base_url` / 整张 `model_providers` 表（含端点与统一会话桶）、
    /// `mcp_servers`（SSOT 在 DB 表）、顶层 `experimental_bearer_token`
    /// fallback、`model_catalog_json`、`web_search = "disabled"` 哨兵——密钥与
    /// 注入产物不会进共享片段。
    ///
    /// 仅对**显式勾选"写入通用配置"**（`meta.common_config_enabled == Some(true)`）的
    /// 供应商生效；用户**显式清空**过片段（`_cleared`）时跳过，避免把用户主动清掉的
    /// 配置又塞回来。所有失败均为非致命，只记 warning，绝不阻断切换。
    fn sync_common_config_snippet_from_live(
        state: &AppState,
        app_type: &AppType,
        provider: &Provider,
        live_config: &Value,
        result: &mut SwitchResult,
    ) {
        // 作用域限定 Claude + Codex（见函数文档）。
        if !matches!(app_type, AppType::Claude | AppType::Codex) {
            return;
        }

        let opted_in = provider
            .meta
            .as_ref()
            .and_then(|meta| meta.common_config_enabled)
            == Some(true);
        if !opted_in {
            return;
        }

        match state.db.is_config_snippet_cleared(app_type.as_str()) {
            Ok(true) => return, // 用户显式清空过通用配置，尊重其选择，不再自动塞回
            Ok(false) => {}
            Err(err) => {
                log::warn!(
                    "Failed to read common config cleared flag for {}: {err}",
                    app_type.as_str()
                );
                return;
            }
        }

        let new_snippet = match Self::extract_common_config_snippet_from_settings(
            app_type.clone(),
            live_config,
        ) {
            Ok(snippet) => snippet,
            Err(err) => {
                log::warn!(
                    "Failed to extract common config from live for {} provider '{}': {err}",
                    app_type.as_str(),
                    provider.id
                );
                return;
            }
        };

        // 未变化则跳过，避免无谓写库（不切 live 配置时这是常态路径）。
        let current = state
            .db
            .get_config_snippet(app_type.as_str())
            .ok()
            .flatten();
        if current.as_deref() == Some(new_snippet.as_str()) {
            return;
        }

        if let Err(err) = state
            .db
            .set_config_snippet(app_type.as_str(), Some(new_snippet))
        {
            log::warn!(
                "Failed to persist synced common config for {} provider '{}': {err}",
                app_type.as_str(),
                provider.id
            );
            result
                .warnings
                .push(format!("common_config_sync_failed:{}", provider.id));
        }
    }

    /// Extract common config snippet from current provider
    ///
    /// Extracts the current provider's configuration and removes provider-specific fields
    /// (API keys, model settings, endpoints) to create a reusable common config snippet.
    pub fn extract_common_config_snippet(
        state: &AppState,
        app_type: AppType,
    ) -> Result<String, AppError> {
        // Get current provider
        let current_id = Self::current(state, app_type.clone())?;
        if current_id.is_empty() {
            return Err(AppError::Message("No current provider".to_string()));
        }

        let providers = state.db.get_all_providers(app_type.as_str())?;
        let provider = providers
            .get(&current_id)
            .ok_or_else(|| AppError::Message(format!("Provider {current_id} not found")))?;

        match app_type {
            AppType::Claude => Self::extract_claude_common_config(&provider.settings_config),
            AppType::Codex => Self::extract_codex_common_config(&provider.settings_config),
            AppType::Pi => Ok(String::new()),
        }
    }

    /// Extract common config snippet from a config value (e.g. editor content).
    pub fn extract_common_config_snippet_from_settings(
        app_type: AppType,
        settings_config: &Value,
    ) -> Result<String, AppError> {
        match app_type {
            AppType::Claude => Self::extract_claude_common_config(settings_config),
            AppType::Codex => Self::extract_codex_common_config(settings_config),
            AppType::Pi => Ok(String::new()),
        }
    }

    /// Extract common config for Claude (JSON format)
    fn extract_claude_common_config(settings: &Value) -> Result<String, AppError> {
        let mut config = settings.clone();

        // 供应商专属的**非机密**字段（模型 + 端点），不应共享。凭据/机密不在此列举，
        // 改由 `is_sensitive_config_key`（模式匹配）统一剥离，新供应商的 `*_API_KEY`
        // 等无需再手工补名单即可被覆盖。
        const ENV_PROVIDER_SPECIFIC_EXCLUDES: &[&str] = &[
            "ANTHROPIC_MODEL",
            "ANTHROPIC_REASONING_MODEL", // legacy: 已废弃，但旧配置可能残留
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
            // Fable 是 v3.16.3 新增的第四档模型映射，与 haiku/sonnet/opus 同属供应商专属，
            // 不得进入通用配置片段，否则会污染其它供应商（issue #4272）。
            "ANTHROPIC_DEFAULT_FABLE_MODEL",
            "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME",
            "CLAUDE_CODE_SUBAGENT_MODEL",
            // Context limits follow the actual upstream model. Sharing these
            // across providers can cap GPT/Kimi to the wrong window and make
            // Claude Code compact too early or miss the upstream limit.
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
            "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
            "ANTHROPIC_BASE_URL",
        ];

        const TOP_LEVEL_EXCLUDES: &[&str] = &[
            "apiBaseUrl",
            // Legacy model fields
            "primaryModel",
            "smallFastModel",
        ];

        // Remove env fields: provider-specific (models/endpoint) + 任何凭据键。
        if let Some(env) = config.get_mut("env").and_then(|v| v.as_object_mut()) {
            let sensitive: Vec<String> = env
                .keys()
                .filter(|k| crate::secrets::is_sensitive_config_key(k))
                .cloned()
                .collect();
            for key in ENV_PROVIDER_SPECIFIC_EXCLUDES {
                env.remove(*key);
            }
            for key in &sensitive {
                env.remove(key);
            }
            // If env is empty after removal, remove the env object itself
            if env.is_empty() {
                config.as_object_mut().map(|obj| obj.remove("env"));
            }
        }

        // Remove top-level fields: legacy model fields + 任何凭据键
        // （例如非标准的顶层 apiKey / api_key / *_TOKEN）。
        if let Some(obj) = config.as_object_mut() {
            let sensitive: Vec<String> = obj
                .keys()
                .filter(|k| crate::secrets::is_sensitive_config_key(k))
                .cloned()
                .collect();
            for key in TOP_LEVEL_EXCLUDES {
                obj.remove(*key);
            }
            for key in &sensitive {
                obj.remove(key);
            }
        }

        // Check if result is empty
        if config.as_object().is_none_or(|obj| obj.is_empty()) {
            return Ok("{}".to_string());
        }

        serde_json::to_string_pretty(&config)
            .map_err(|e| AppError::Message(format!("Serialization failed: {e}")))
    }

    /// Extract common config for Codex (TOML format)
    fn extract_codex_common_config(settings: &Value) -> Result<String, AppError> {
        // Codex config is stored as { "auth": {...}, "config": "toml string" }
        let config_toml = settings
            .get("config")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if config_toml.is_empty() {
            return Ok(String::new());
        }

        let mut doc = config_toml
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| AppError::Message(format!("TOML parse error: {e}")))?;

        // Remove provider-specific fields.
        let root = doc.as_table_mut();
        root.remove("model");
        root.remove("model_provider");
        // Legacy/alt formats might use a top-level base_url.
        root.remove("base_url");
        // wire_api 与 base_url 同属供应商路由语义：无 model_provider 时
        // update_codex_toml_field / 前端 setCodexWireApi 都会把它落在顶层，
        // 进了片段会改写其它供应商的协议选择（chat vs responses）。
        root.remove("wire_api");

        // Remove entire model_providers table (provider-specific configuration)
        root.remove("model_providers");

        // MCP 服务器归 DB mcp_servers 表所有：进了共享片段会绕过按应用的
        // 启用状态被合并进所有勾选通用配置的供应商，且在通用配置编辑框里
        // 显示为一份"重复"的 MCP 配置。
        root.remove("mcp_servers");
        // 历史错误格式 [mcp.servers] 一并剥离（与 strip_codex_mcp_servers_from_settings
        // 一致）：sync_all_enabled 只管理 [mcp_servers.*]，legacy 形态一旦进了
        // 片段就会被合并进所有供应商，且没有任何同步路径能清掉这个孤儿。
        if let Some(mcp_tbl) = root
            .get_mut("mcp")
            .and_then(|item| item.as_table_like_mut())
        {
            mcp_tbl.remove("servers");
            if mcp_tbl.is_empty() {
                root.remove("mcp");
            }
        }

        // cc-switch 写 live 时注入的产物一律不进共享片段：
        // - experimental_bearer_token 正常写在 [model_providers.<id>] 内（上面
        //   整表已剥），但无活跃路由 / 内建保留 id / 路由表缺失三种 fallback
        //   会落在顶层——不剥等于把 API 密钥写进共享片段。
        root.remove("experimental_bearer_token");
        // - model_catalog_json 指向按供应商生成的 catalog 投影文件（DB 为 SSOT）。
        root.remove("model_catalog_json");
        // - web_search 只剥 cc-switch 注入的 "disabled" 哨兵；用户手设的其它值
        //   属于可共享偏好，保留。
        if root
            .get(crate::codex_config::CODEX_WEB_SEARCH_FIELD)
            .and_then(|item| item.as_str())
            == Some(crate::codex_config::CODEX_WEB_SEARCH_DISABLED)
        {
            root.remove(crate::codex_config::CODEX_WEB_SEARCH_FIELD);
        }

        // Clean up multiple empty lines (keep at most one blank line).
        let mut cleaned = String::new();
        let mut blank_run = 0usize;
        for line in doc.to_string().lines() {
            if line.trim().is_empty() {
                blank_run += 1;
                if blank_run <= 1 {
                    cleaned.push('\n');
                }
                continue;
            }
            blank_run = 0;
            cleaned.push_str(line);
            cleaned.push('\n');
        }

        Ok(cleaned.trim().to_string())
    }

    /// Import default configuration from live files (re-export)
    ///
    /// Returns `Ok(true)` if imported, `Ok(false)` if skipped.
    pub fn import_default_config(state: &AppState, app_type: AppType) -> Result<bool, AppError> {
        import_default_config(state, app_type)
    }

    pub fn should_import_default_config_on_startup(
        state: &AppState,
        app_type: &AppType,
    ) -> Result<bool, AppError> {
        should_import_default_config_on_startup(state, app_type)
    }

    /// Read current live settings (re-export)
    pub fn read_live_settings(app_type: AppType) -> Result<Value, AppError> {
        read_live_settings(app_type)
    }

    /// Update provider sort order
    pub fn update_sort_order(
        state: &AppState,
        app_type: AppType,
        updates: Vec<ProviderSortUpdate>,
    ) -> Result<bool, AppError> {
        let mut providers = state.db.get_all_providers(app_type.as_str())?;

        for update in updates {
            if let Some(provider) = providers.get_mut(&update.id) {
                provider.sort_index = Some(update.sort_index);
                state.db.save_provider(app_type.as_str(), provider)?;
            }
        }

        Ok(true)
    }

    fn validate_provider_settings(
        state: &AppState,
        app_type: &AppType,
        provider: &Provider,
        original_id: Option<&str>,
    ) -> Result<(), AppError> {
        Self::validate_provider_settings_shape(app_type, provider)?;
        Self::validate_required_secrets(state, app_type, provider, original_id)
    }

    /// §5.4-②：新增 / 编辑时必须补齐的凭据。
    ///
    /// 密钥可能已经躺在凭据管理器里（编辑时前端不回显、也不重发），所以先看本次
    /// 入参（入参里仍是明文，提取器只用于判定"有没有"），再看凭据管理器里该供应商
    /// （或改名前的原 id）的条目。
    fn validate_required_secrets(
        state: &AppState,
        app_type: &AppType,
        provider: &Provider,
        original_id: Option<&str>,
    ) -> Result<(), AppError> {
        // `official` 走 CLI 自己的登录态（Claude Official 预设的 env 就是空的），
        // `cloud_provider`（Bedrock）用模板变量 / IAM 认证 —— 两者都没有 api_key 字段，
        // 前端软校验也刻意跳过它们，后端必须保持一致。
        //
        // Codex 不纳入强制 api_key 校验：`provider_service_switch_codex_preserved_login_*`
        // 两个既有用例已锁定"自带 http_headers 认证的无 key 卡合法、只有会回落 auth.json
        // 的形态才拒绝"，一刀切会推翻它。Codex 的收口点在 live 写入门控（§5.4-③）。
        let keyless_category = matches!(
            provider.category.as_deref(),
            Some("official") | Some("cloud_provider")
        );
        let (need_api_key, need_base_url) = match app_type {
            AppType::Claude => (!keyless_category, false),
            AppType::Codex => (false, false),
            AppType::Pi => (false, true),
        };
        if !need_api_key && !need_base_url {
            return Ok(());
        }

        let extracted = SecretExtractor::extract_with_meta(
            &provider.id,
            app_type,
            &provider.settings_config,
            provider
                .meta
                .as_ref()
                .and_then(|m| m.api_key_field.as_deref()),
        )?;
        let incoming = extracted.secrets;

        let candidate_ids: Vec<&str> = match original_id {
            Some(original) if original != provider.id => vec![provider.id.as_str(), original],
            _ => vec![provider.id.as_str()],
        };

        if need_api_key && incoming.api_key.is_none() {
            // §6.6/4.3：改查 secret_refs 字段名，不调 vault / store。
            let stored = candidate_ids.iter().any(|id| {
                state
                    .db
                    .get_secret_ref_fields(app_type.as_str(), id)
                    .ok()
                    .flatten()
                    .is_some_and(|fields| fields.iter().any(|f| f == crate::secrets::FIELD_API_KEY))
            });
            if !stored {
                // S4-3：导入后未关联 1P 条目的供应商（不同保险箱、或远端没带引用
                // 行）本来是有钥匙的，只是本机没关联上。要求用户重新输入会诱使他们
                // 在 1P 里建重复条目——给专门错误码，提示先关联。
                if crate::settings::is_onepassword_backend() {
                    let key = format!("{}/{}", app_type.as_str(), provider.id);
                    if crate::settings::get_onepassword_unlinked()
                        .iter()
                        .any(|k| k == &key)
                    {
                        return Err(AppError::localized(
                            "provider.vault_unlinked",
                            "该供应商来自其他设备，尚未关联 1Password 条目：请先在提示中点「从 1Password 关联」，无需重新输入密钥",
                            "This provider comes from another device and is not linked to 1Password yet. Link it first instead of re-entering the key.",
                        ));
                    }
                }
                return Err(AppError::localized(
                    "provider.api_key.required",
                    "请填写 API 密钥后再保存",
                    "An API key is required before saving.",
                ));
            }
        }

        // Pi 的 schema 允许模型级 baseUrl（§5.2.3：不提取、原样保留），这类配置
        // 没有供应商级 baseUrl 也能用，不能当成"缺端点"拒绝。
        let has_model_level_base_url = app_type == &AppType::Pi
            && extracted
                .stripped
                .get("models")
                .and_then(Value::as_array)
                .is_some_and(|models| {
                    models.iter().any(|model| {
                        model
                            .get("baseUrl")
                            .and_then(Value::as_str)
                            .is_some_and(|url| !url.trim().is_empty())
                    })
                });

        if need_base_url && incoming.base_url.is_none() && !has_model_level_base_url {
            // F1-2（D3-A）：端点表**或** secret_refs 任一命中即视为已有 base_url
            //（非敏感 base_url 已搬到端点表，refs 只兜底拆分前的历史数据）。
            let stored = candidate_ids.iter().any(|id| {
                state
                    .db
                    .get_provider_endpoint(app_type.as_str(), id)
                    .ok()
                    .flatten()
                    .is_some()
                    || state
                        .db
                        .get_secret_ref_fields(app_type.as_str(), id)
                        .ok()
                        .flatten()
                        .is_some_and(|fields| {
                            fields.iter().any(|f| f == crate::secrets::FIELD_BASE_URL)
                        })
            });
            if !stored {
                return Err(AppError::localized(
                    "provider.base_url.required",
                    "请填写 Base URL 后再保存",
                    "A Base URL is required before saving.",
                ));
            }
        }

        Ok(())
    }

    fn validate_provider_settings_shape(
        app_type: &AppType,
        provider: &Provider,
    ) -> Result<(), AppError> {
        match app_type {
            AppType::Claude => {
                if !provider.settings_config.is_object() {
                    return Err(AppError::localized(
                        "provider.claude.settings.not_object",
                        "Claude 配置必须是 JSON 对象",
                        "Claude configuration must be a JSON object",
                    ));
                }
            }
            AppType::Codex => {
                let settings = provider.settings_config.as_object().ok_or_else(|| {
                    AppError::localized(
                        "provider.codex.settings.not_object",
                        "Codex 配置必须是 JSON 对象",
                        "Codex configuration must be a JSON object",
                    )
                })?;

                let auth = settings.get("auth").ok_or_else(|| {
                    AppError::localized(
                        "provider.codex.auth.missing",
                        format!("供应商 {} 缺少 auth 配置", provider.id),
                        format!("Provider {} is missing auth configuration", provider.id),
                    )
                })?;
                if !auth.is_object() {
                    return Err(AppError::localized(
                        "provider.codex.auth.not_object",
                        format!("供应商 {} 的 auth 配置必须是 JSON 对象", provider.id),
                        format!(
                            "Provider {} auth configuration must be a JSON object",
                            provider.id
                        ),
                    ));
                }

                if let Some(config_value) = settings.get("config") {
                    if !(config_value.is_string() || config_value.is_null()) {
                        return Err(AppError::localized(
                            "provider.codex.config.invalid_type",
                            "Codex config 字段必须是字符串",
                            "Codex config field must be a string",
                        ));
                    }
                    if let Some(cfg_text) = config_value.as_str() {
                        crate::codex_config::validate_config_toml(cfg_text)?;
                    }
                }
            }
            AppType::Pi => {
                crate::pi_config::validate_provider_node(&provider.id, &provider.settings_config)?;
            }
        }

        Ok(())
    }
}

/// Normalize Claude model keys in a JSON value
///
/// Reads old key (ANTHROPIC_SMALL_FAST_MODEL), writes new keys (DEFAULT_*), and deletes old key.
pub(crate) fn normalize_claude_models_in_value(settings: &mut Value) -> bool {
    let mut changed = false;
    let env = match settings.get_mut("env").and_then(|v| v.as_object_mut()) {
        Some(obj) => obj,
        None => return changed,
    };

    let model = env
        .get("ANTHROPIC_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let small_fast = env
        .get("ANTHROPIC_SMALL_FAST_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let current_haiku = env
        .get("ANTHROPIC_DEFAULT_HAIKU_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let current_sonnet = env
        .get("ANTHROPIC_DEFAULT_SONNET_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let current_opus = env
        .get("ANTHROPIC_DEFAULT_OPUS_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let target_haiku = current_haiku
        .or_else(|| small_fast.clone())
        .or_else(|| model.clone());
    let target_sonnet = current_sonnet
        .or_else(|| model.clone())
        .or_else(|| small_fast.clone());
    let target_opus = current_opus
        .or_else(|| model.clone())
        .or_else(|| small_fast.clone());

    if env.get("ANTHROPIC_DEFAULT_HAIKU_MODEL").is_none() {
        if let Some(v) = target_haiku {
            env.insert(
                "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
                Value::String(v),
            );
            changed = true;
        }
    }
    if env.get("ANTHROPIC_DEFAULT_SONNET_MODEL").is_none() {
        if let Some(v) = target_sonnet {
            env.insert(
                "ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(),
                Value::String(v),
            );
            changed = true;
        }
    }
    if env.get("ANTHROPIC_DEFAULT_OPUS_MODEL").is_none() {
        if let Some(v) = target_opus {
            env.insert("ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(), Value::String(v));
            changed = true;
        }
    }

    if env.remove("ANTHROPIC_SMALL_FAST_MODEL").is_some() {
        changed = true;
    }

    changed
}

pub(super) fn delete_provider_secrets(state: &AppState, app_type: &AppType, id: &str) {
    // §6.6：整组删除走 vault（旧后端下 = 逐字段删 + 清 known_secret_targets）。
    // 保持 best-effort：删除失败只告警，不阻断删供应商。
    let group = crate::secrets::SecretGroup::provider(app_type.clone(), id.to_string());
    match state.vault.delete(&group) {
        Ok(()) => {
            // F3-8：这次真的清掉了——若此前因删除失败记过孤儿，出队。
            let key = format!("{}/{}", app_type.as_str(), id);
            let mut orphans = crate::settings::get_onepassword_orphans();
            if orphans.iter().any(|k| k == &key) {
                orphans.retain(|k| k != &key);
                if let Err(e) = crate::settings::set_onepassword_orphans(orphans) {
                    log::warn!("更新 1Password 孤儿清单失败: {e}");
                }
            }
        }
        Err(e) => {
            // F3-8（D12 / P1-10）：1P 模式下删除失败（锁定/断网/超时）时供应商照删，
            // 但 1P 里的条目不能当作「已清理」——记入本机设置 `onepassword_orphans`，
            // 稍后由「清理孤儿凭据」按组归档，避免变成 CCS 看不见的孤儿。
            // Windows 模式的删除失败维持旧行为（只告警），孤儿清单对它无意义。
            log::warn!("删除供应商凭据失败 {}/{id}: {e}", app_type.as_str());
            if crate::settings::is_onepassword_backend() {
                let key = format!("{}/{}", app_type.as_str(), id);
                let mut orphans = crate::settings::get_onepassword_orphans();
                if !orphans.iter().any(|k| k == &key) {
                    orphans.push(key);
                    if let Err(err) = crate::settings::set_onepassword_orphans(orphans) {
                        log::warn!("记录 1Password 孤儿条目失败: {err}");
                    }
                }
            }
        }
    }
    // §4.3：同步抹掉引用行（best-effort）。
    if let Err(e) = state.db.delete_secret_ref(app_type.as_str(), id) {
        log::warn!("删除 secret_refs 失败 {}/{id}: {e}", app_type.as_str());
    }
}

fn parse_secret_target(target: &str) -> Option<crate::secrets::SecretTarget> {
    let rest = target.strip_prefix("cc-switch/v1/provider/")?;
    let mut parts = rest.splitn(3, '/');
    let app = AppType::from_str(parts.next()?).ok()?;
    let provider_id = parts.next()?;
    let field = parts.next()?;
    match field {
        "api_key" => Some(crate::secrets::SecretTarget::provider_api_key(
            app,
            provider_id,
        )),
        "base_url" => Some(crate::secrets::SecretTarget::provider_base_url(
            app,
            provider_id,
        )),
        other => other
            .strip_prefix("env/")
            .map(|var| crate::secrets::SecretTarget::provider_env(app, provider_id, var)),
    }
}

/// §6.6：抽取（纯函数）+ 整包写入 vault。
///
/// `merge_existing`：编辑时表单可能不回传未改字段（“保留原值”），此时先 fetch
/// 旧整包再叠加新抽取值再整包 put，语义等同旧的“只增不删”持久化；
/// 新增时表单自带完整值，`merge_existing=false` 直接 put（不多一次 fetch）。
fn strip_and_store_provider_secrets(
    state: &AppState,
    app_type: &AppType,
    provider: &mut Provider,
    merge_existing: bool,
) -> Result<(), AppError> {
    let field = provider
        .meta
        .as_ref()
        .and_then(|m| m.api_key_field.as_deref());
    let extracted = SecretExtractor::extract_with_meta(
        &provider.id,
        app_type,
        &provider.settings_config,
        field,
    )?;
    provider.settings_config = extracted.stripped;
    store_provider_bundle(
        state,
        app_type,
        &provider.id,
        &extracted.secrets,
        merge_existing,
        Some(&provider.name),
    )?;
    Ok(())
}

/// P3（安全方案 §7.2）：编辑提交里抽出的秘密的分类结果。
enum EditSecretClassification {
    /// 抽取结果为空，或只有与本地已知状态一致的端点：零 vault 调用，只写配置。
    ConfigOnly,
    /// 需要进入既有 vault 流程（显式新钥匙、extra_env/敏感头、无法本地判等的端点）。
    VaultRequired(crate::secrets::ProviderSecrets),
}

/// 编辑路径的秘密分类（§7.2-3/5）。后端以规范化的抽取结果 + 已保存状态决定
/// 差异，不信任前端 dirty 提示（§2-2）；所有写入入口共享本规则。
///
/// 规则（按 §6.2 行为矩阵的默认值）：
/// - 出现 api_key 或任何 extra_env/敏感头 → 显式凭据分支（旧入口的真实秘密
///   仍剥离并持久化，不会悄悄清空，§7.2-4）。
/// - 只有 base_url：敏感（带凭据）URL 不落缓存、本地无法判等 → 凭据分支；
///   与端点缓存同值 → keep（保留 1P 当前端点，不把缓存值回写成“真值”）；
///   缓存未知/miss 不当作空值或无变化 → 保守进入凭据分支。
fn classify_edit_secrets(
    state: &AppState,
    app_type: &AppType,
    provider_id: &str,
    secrets: &crate::secrets::ProviderSecrets,
) -> Result<EditSecretClassification, AppError> {
    if secrets.api_key.is_some() || !secrets.extra_env.is_empty() {
        return Ok(EditSecretClassification::VaultRequired(secrets.clone()));
    }
    let Some(url) = secrets.base_url.as_ref() else {
        return Ok(EditSecretClassification::ConfigOnly);
    };
    if crate::secrets::is_credential_bearing_url(url.as_str()) {
        return Ok(EditSecretClassification::VaultRequired(secrets.clone()));
    }
    let cached = state
        .db
        .get_provider_endpoint(app_type.as_str(), provider_id)?;
    if cached.as_deref() == Some(url.as_str()) {
        return Ok(EditSecretClassification::ConfigOnly);
    }
    Ok(EditSecretClassification::VaultRequired(secrets.clone()))
}

/// 把抽出的 `ProviderSecrets` 整包写入 vault（§6.6）。只在写入时登记会话脱敏名单。
///
/// 2026-09-27（用户决策，D3-A → D3-B 演进）：`base_url` **保留在 vault 整包里**
/// （非敏感 URL 以可见 STRING 字段、敏感 URL 以 CONCEALED 写入）——1Password 成为
/// 钥匙 + 端点的持久真源，数据库被重置 / 换设备后「重建引用」即可整体恢复。
/// 本地端点表 `provider_endpoints` 降级为**读取缓存**：写入时照常 upsert，
/// 读取（[`resolve_base_url`]）端点表命中即 0 次 op，未命中再 fetch 一次并回填缓存。
pub(crate) fn store_provider_bundle(
    state: &AppState,
    app_type: &AppType,
    provider_id: &str,
    secrets: &ProviderSecrets,
    merge_existing: bool,
    display_name: Option<&str>,
) -> Result<(), AppError> {
    use crate::secrets::{SecretBundle, SecretGroup};
    let group = SecretGroup::provider(app_type.clone(), provider_id.to_string());
    let new_bundle = SecretBundle::from_provider_secrets(secrets);
    // 端点表缓存：非敏感 URL 写入，读取路径不用碰 vault。
    if let Some(url) = secrets.base_url.as_ref() {
        if !crate::secrets::is_credential_bearing_url(url.as_str()) {
            state
                .db
                .upsert_provider_endpoint(app_type.as_str(), provider_id, url.as_str())?;
        }
    }
    // §6.2：本次抽取无新密钥（如切换时回填、live 已剥钥）→ 无需写入，
    // 直接返回（既不 fetch 也不 put）。否则 1Password 模式下每次切换都会白白触发解锁。
    if new_bundle.is_empty() {
        return Ok(());
    }
    if let Some(key) = secrets.api_key.as_ref() {
        // §5.5：登记进“本次会话已知密钥”，导出护栏按字面量兜底。
        crate::secrets::scan::note_session_secret(key.as_str());
    }
    let old_bundle = if merge_existing {
        state.vault.fetch(&group)?.unwrap_or_default()
    } else {
        SecretBundle::new()
    };
    // merge 语义：新值覆盖旧值，本次未传的字段保留旧值（表单“保留原值”）。
    let mut bundle = old_bundle.clone();
    for (name, value) in new_bundle.iter() {
        bundle.insert(name.clone(), value.clone());
    }
    if bundle == old_bundle {
        // 整包没有任何变化（例如端点表缓存已含同一 URL、钥匙也没改）→ 不 put。
        // fetch 已经发生，但写侧零往返（避免每次切换白白触发解锁）。
        return Ok(());
    }
    let vref = state.vault.put_titled(&group, &bundle, display_name)?;
    // §4.3：整包写入后登记引用（只字段名，不含值），列表/校验/删除据此零 vault 往返。
    state.db.upsert_secret_ref(
        app_type.as_str(),
        provider_id,
        &vref.vault_id,
        &vref.item_id,
        &vref.fields,
    )?;
    Ok(())
}

/// F1-2（§7 D3-A）：base_url 统一读取入口。
///
/// 1. 端点表命中 → 直接返回（0 次 op）；
/// 2. `secret_refs` 登记了 `base_url` → fetch 一次；若是非敏感 URL，顺手写入
///    端点表（懒迁移，之后都是 0 次 op）；vault 里的旧副本留给回填 / 后续 put 清理；
/// 3. 都没有 → `None`。
///
/// S4-2（P0-3）：这里**刻意仍然「缓存命中即返回」**。列表、Codex/Pi 切换的性能
/// 都依赖这条路径 0 次 op。缓存现在只可能由本机写入——S4-1 保证导入不会带进外来的
/// 缓存（1P 模式整表忽略文件值），`fetch_provider_secrets` 又在每次真正取整包时按
/// 1P 真值校正缓存，所以「别的设备改了 URL」的分歧窗口被收窄到「本机下一次 fetch
/// 之前」，而 fetch 一定发生在真正要用钥匙的时候。
/// P3/P5（安全方案 §7.4）：live 投影的凭据访问策略。用显式参数而非全局开关，
/// 避免改变所有 resolve 行为。
///
/// - `Allowed`：切换、显式应用等动作；端点缓存 miss 可 fetch（含懒迁移）。
/// - `NoVaultAccess`：纯配置编辑的投影上下文；绝不触碰 vault。缓存 miss 时
///   由调用方「安全保留既有投影」或显式报错，不得偷偷 fetch、也不得删掉
///   endpoint 让 CLI 落回默认主机。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VaultAccessPolicy {
    Allowed,
    NoVaultAccess,
}

pub(crate) fn resolve_base_url(
    state: &AppState,
    app_type: &AppType,
    provider_id: &str,
) -> Result<Option<Zeroizing<String>>, AppError> {
    resolve_base_url_with_policy(state, app_type, provider_id, VaultAccessPolicy::Allowed)
}

/// P3/P5（安全方案 §7.4）：带凭据访问策略的端点解析。`NoVaultAccess` 供纯配置
/// 编辑的 live 投影使用：端点缓存命中即返回，miss 时不 fetch、不做懒迁移——
/// 由调用方决定「安全保留既有投影」还是显式报错。
pub(crate) fn resolve_base_url_with_policy(
    state: &AppState,
    app_type: &AppType,
    provider_id: &str,
    policy: VaultAccessPolicy,
) -> Result<Option<Zeroizing<String>>, AppError> {
    if let Some(url) = state
        .db
        .get_provider_endpoint(app_type.as_str(), provider_id)?
    {
        return Ok(Some(Zeroizing::new(url)));
    }
    let registered = state
        .db
        .get_secret_ref_fields(app_type.as_str(), provider_id)?
        .is_some_and(|fields| fields.iter().any(|f| f == crate::secrets::FIELD_BASE_URL));
    if !registered {
        return Ok(None);
    }
    if policy == VaultAccessPolicy::NoVaultAccess {
        return Ok(None);
    }
    let group = crate::secrets::SecretGroup::provider(app_type.clone(), provider_id.to_string());
    let Some(bundle) = state.vault.fetch(&group)? else {
        return Ok(None);
    };
    let Some(url) = bundle.get(crate::secrets::FIELD_BASE_URL) else {
        return Ok(None);
    };
    if !crate::secrets::is_credential_bearing_url(url.as_str()) {
        // 懒迁移：非敏感 URL 落端点表，之后读取都是 0 次 op。
        state
            .db
            .upsert_provider_endpoint(app_type.as_str(), provider_id, url.as_str())?;
    }
    Ok(Some(url.clone()))
}

/// F1-5：1Password 模式下的导入明文清理（P0-5 / 原则 3）。
///
/// SQL 导入 / 备份恢复 / 云同步下载后，DB 里可能带明文钥匙。1P 模式下绝不写
/// 凭据管理器（守卫也会拦）：逐行纯提取 → 整包写入 vault（merge 语义）→
/// **写入成功才**在 DB 剥离该行；失败的行保留明文并记入本机设置
/// `secrets_import_pending`，UI 提示「解锁 1Password 后重试导入钥匙」。
/// 导出护栏 `assert_no_secret_patterns` 会拒绝带明文的同步上传——fail-closed。
///
/// 返回 pending 清单（空 = 全部成功）。
pub(crate) fn scrub_imported_plaintext_via_vault(
    state: &AppState,
) -> Result<Vec<String>, AppError> {
    let mut pending: Vec<String> = Vec::new();
    // 收集「vault 写入成功」的行的 stripped 配置，最后事务写回。
    let mut stripped_rows: Vec<(String, String, Value)> = Vec::new();

    for app_type in [AppType::Claude, AppType::Codex, AppType::Pi] {
        let providers = state.db.get_all_providers(app_type.as_str())?;
        for (id, provider) in providers {
            let key = format!("{}/{}", app_type.as_str(), id);
            let extracted = match SecretExtractor::extract_with_meta(
                &id,
                &app_type,
                &provider.settings_config,
                provider
                    .meta
                    .as_ref()
                    .and_then(|m| m.api_key_field.as_deref()),
            ) {
                Ok(e) => e,
                Err(e) => {
                    // 提取失败：保留原样，等用户重试（只记 id，不记值）。
                    pending.push(key.clone());
                    log::warn!("1P 模式导入清理：{key} 提取失败，保留原样待重试: {e}");
                    continue;
                }
            };
            // 无秘密的行不写 vault，但仍要落 stripped——与凭据管理器迁移一致：
            // 只含 OAuth 登录态的行必须把被丢弃的 tokens 从 DB 里剥掉。
            if !extracted.secrets.is_empty() {
                if let Err(e) =
                    store_provider_bundle(state, &app_type, &id, &extracted.secrets, true, None)
                {
                    pending.push(key.clone());
                    log::warn!(
                        "1P 模式导入清理：{key} 写 vault 失败（可能锁定），保留明文待重试: {e}"
                    );
                    continue;
                }
            }
            // S4-6（P1-7 / §5.4 S4-6）：只有真的被剥离过的行才写回。1P 模式下
            // 已处理过的行（只剩模型等非敏感字段）提取后与原样一致，逐行 UPDATE
            // 只会白白触发 update_hook，并让下面的 VACUUM 空跑一次。
            if extracted.stripped != provider.settings_config {
                stripped_rows.push((app_type.as_str().to_string(), id, extracted.stripped));
            }
        }
    }

    // 事务写回 stripped（secure_delete 让被覆盖的明文立刻离开 free page）。
    if !stripped_rows.is_empty() {
        let conn = crate::database::lock_conn!(state.db.conn);
        conn.execute_batch("PRAGMA secure_delete = ON;")
            .map_err(|e| AppError::Database(format!("开启 secure_delete 失败: {e}")))?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| AppError::Database(format!("开启导入清理事务失败: {e}")))?;
        for (app_str, id, stripped) in &stripped_rows {
            let json = serde_json::to_string(stripped)
                .map_err(|e| AppError::Database(format!("序列化 stripped 失败: {e}")))?;
            tx.execute(
                "UPDATE providers SET settings_config = ?1 WHERE app_type = ?2 AND id = ?3",
                rusqlite::params![json, app_str, id],
            )
            .map_err(|e| AppError::Database(format!("剥离 {app_str}/{id} 失败: {e}")))?;
        }
        tx.commit()
            .map_err(|e| AppError::Database(format!("提交导入清理事务失败: {e}")))?;
        conn.execute_batch("VACUUM;")
            .map_err(|e| AppError::Database(format!("导入清理后 VACUUM 失败: {e}")))?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA secure_delete = ON;")
            .map_err(|e| AppError::Database(format!("恢复连接 pragma 失败: {e}")))?;
    }

    crate::settings::set_secrets_import_pending(pending.clone())?;
    if !pending.is_empty() {
        log::warn!(
            "1P 模式导入清理：{} 行保留明文待重试（解锁 1Password 后在设置里重试导入）",
            pending.len()
        );
    }
    Ok(pending)
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderSortUpdate {
    pub id: String,
    #[serde(rename = "sortIndex")]
    pub sort_index: usize,
}

#[cfg(test)]
mod required_secret_tests {
    use super::*;
    use crate::secrets::InMemorySecretStore;
    use serde_json::json;
    use std::sync::Arc;

    fn provider_with(id: &str, category: Option<&str>, settings: Value) -> Provider {
        let mut p = Provider::from_parts(id.to_string(), id.to_string(), settings, None);
        p.category = category.map(str::to_string);
        p
    }

    fn state() -> AppState {
        AppState::new(
            Arc::new(crate::database::Database::memory().expect("memory db")),
            Arc::new(InMemorySecretStore::new()),
        )
    }

    fn check(state: &AppState, app: &AppType, provider: &Provider) -> Result<(), AppError> {
        ProviderService::validate_required_secrets(state, app, provider, None)
    }

    #[test]
    fn claude_requires_an_api_key_unless_official_or_cloud_provider() {
        let state = state();
        let app = AppType::Claude;

        let bare = provider_with(
            "bare",
            Some("third_party"),
            json!({"env": {"ANTHROPIC_BASE_URL": "https://x.example.com"}}),
        );
        assert!(
            check(&state, &app, &bare).is_err(),
            "第三方卡缺 key 必须拒绝"
        );

        // 密钥已在保险箱里（编辑时前端不回显、也不重发）→ 放行。
        // §4.3：存在性现由 secret_refs 判定。
        state
            .db
            .upsert_secret_ref(
                app.as_str(),
                "bare",
                "",
                "provider/claude/bare",
                &["api_key".to_string()],
            )
            .expect("upsert ref");
        assert!(check(&state, &app, &bare).is_ok(), "已存凭据必须放行编辑");

        // Claude Official 预设的 env 就是空的
        let official = provider_with("official", Some("official"), json!({"env": {}}));
        assert!(check(&state, &app, &official).is_ok());

        // Bedrock IAM 走模板变量 / 环境变量认证
        let bedrock = provider_with(
            "bedrock",
            Some("cloud_provider"),
            json!({"env": {"CLAUDE_CODE_USE_BEDROCK": "1"}}),
        );
        assert!(check(&state, &app, &bedrock).is_ok());
    }

    #[test]
    fn codex_keyless_cards_are_left_to_the_live_write_gate() {
        // Codex 的收口点在 live 写入门控，不在新增/编辑校验：无 key 但自带
        // http_headers 认证的卡合法（见 provider_service_switch_codex_preserved_login_*）。
        let state = state();
        let app = AppType::Codex;

        let keyless = provider_with(
            "cdx-keyless",
            Some("third_party"),
            json!({
                "auth": {},
                "config": "model_provider = \"custom\"\n[model_providers.custom]\nname = \"Custom\"\nbase_url = \"https://relay.example/v1\"\nwire_api = \"responses\"\nhttp_headers = { Authorization = \"Bearer t\" }\n"
            }),
        );
        assert!(check(&state, &app, &keyless).is_ok());
    }

    #[test]
    fn pi_requires_a_base_url() {
        let state = state();
        let app = AppType::Pi;

        let bare = provider_with("pi-bare", None, json!({"apiKey": "k", "model": "m"}));
        assert!(check(&state, &app, &bare).is_err());

        let with_url = provider_with(
            "pi-ok",
            None,
            json!({"baseUrl": "https://pi.example.com", "apiKey": "k"}),
        );
        assert!(check(&state, &app, &with_url).is_ok());

        // 模型级 baseUrl 也算有端点（§5.2.3：不提取但原样保留）
        let model_level = provider_with(
            "pi-model-url",
            None,
            json!({"models": [{"id": "m", "baseUrl": "https://pi.example.com"}]}),
        );
        assert!(check(&state, &app, &model_level).is_ok());

        // 端点已在保险箱里→ §4.3 存在性由 secret_refs 判定。
        state
            .db
            .upsert_secret_ref(
                app.as_str(),
                "pi-stored",
                "",
                "provider/pi/pi-stored",
                &["base_url".to_string()],
            )
            .expect("upsert ref");
        let stored = provider_with("pi-stored", None, json!({"apiKey": "k"}));
        assert!(check(&state, &app, &stored).is_ok());
    }
}

#[cfg(test)]
mod env_adopt_tests {
    use super::*;
    use crate::env_delivery::{EnvSink, InMemoryEnvSink, ManagedEnvVars};
    use crate::secrets::{InMemorySecretStore, SecretTarget};
    use serde_json::json;
    use std::sync::Arc;

    /// 缺陷 D-4：回滚到旧版再升级时，恢复出来的旧库没有 `managed_env_vars`，
    /// 而注册表里还留着上一轮投递的值——冲突检测判成 foreign 后每次启动都拒写。
    /// 自动重写路径必须先认领这些按我们命名规则存在的变量。
    #[test]
    fn adopt_registers_leftover_names_that_conflict() {
        let mut state = AppState::new(
            Arc::new(crate::database::Database::memory().expect("memory db")),
            Arc::new(InMemorySecretStore::new()),
        );
        let sink = InMemoryEnvSink::default();
        state.env_sink = Arc::new(sink.clone());

        let provider = Provider::from_parts(
            "old".to_string(),
            "Old".to_string(),
            json!({ "env": {} }),
            None,
        );
        state
            .db
            .save_provider("claude", &provider)
            .expect("save provider");
        state
            .db
            .set_current_provider("claude", "old")
            .expect("set current");
        futures::executor::block_on(state.secrets.store(
            &SecretTarget::provider_api_key(AppType::Claude, "old".to_string()),
            "fresh-key",
        ))
        .expect("store key");

        // 上一轮投递残留：同名变量已在注册表里，且值与当前凭据不同。
        let leftover = zeroize::Zeroizing::new("stale-key".to_string());
        sink.set("ANTHROPIC_AUTH_TOKEN", &leftover)
            .expect("seed leftover");

        let managed = ManagedEnvVars::load(&state.db).expect("load managed");
        assert!(
            !managed.is_managed("ANTHROPIC_AUTH_TOKEN"),
            "前置条件：旧库里没有该登记，投递会被判成冲突"
        );
        let pending = vec![(
            "ANTHROPIC_AUTH_TOKEN".to_string(),
            zeroize::Zeroizing::new("fresh-key".to_string()),
        )];
        assert!(
            ProviderService::reject_if_env_conflicts(&state, &AppType::Claude, &pending).is_err(),
            "认领之前必须确实复现出 foreign 冲突"
        );

        ProviderService::adopt_unregistered_managed_env(&state).expect("adopt");

        let managed = ManagedEnvVars::load(&state.db).expect("reload managed");
        assert!(
            managed.is_managed("ANTHROPIC_AUTH_TOKEN"),
            "认领后应把变量归属登记到当前供应商"
        );
        assert_eq!(
            managed.vars_for_provider("claude", "old"),
            vec!["ANTHROPIC_AUTH_TOKEN".to_string()],
            "登记的归属要落在真正投递它的供应商上，删除时才知道收回"
        );
        assert!(
            ProviderService::reject_if_env_conflicts(&state, &AppType::Claude, &pending).is_ok(),
            "认领之后同一轮投递不再被拒"
        );
    }
}

#[cfg(test)]
mod secret_read_failure_tests {
    //! §1.4 回归护栏：凭据后端读取失败（1Password 锁定/取消/断网）必须向上
    //! 传播，绝不能被吞成"没有钥匙"，否则会给终端注入空钥匙。
    use super::*;
    use crate::secrets::{SecretStore, SecretTarget};
    use async_trait::async_trait;
    use std::sync::Arc;
    use zeroize::Zeroizing;

    /// 所有 `get` 一律失败，模拟 vault 锁定/断网。
    struct FailingSecretStore;

    #[async_trait]
    impl SecretStore for FailingSecretStore {
        async fn set(&self, _t: &SecretTarget, _v: Zeroizing<String>) -> Result<(), AppError> {
            Err(AppError::Message("backend locked".to_string()))
        }
        async fn get(&self, _t: &SecretTarget) -> Result<Option<Zeroizing<String>>, AppError> {
            Err(AppError::Message("backend locked".to_string()))
        }
        async fn delete(&self, _t: &SecretTarget) -> Result<(), AppError> {
            Err(AppError::Message("backend locked".to_string()))
        }
        async fn probe(&self) -> Result<(), AppError> {
            Err(AppError::Message("backend locked".to_string()))
        }
        async fn list_targets(&self, _prefix: &str) -> Result<Vec<String>, AppError> {
            Err(AppError::Message("backend locked".to_string()))
        }
        async fn get_target_raw(
            &self,
            _target_name: &str,
        ) -> Result<Option<Zeroizing<String>>, AppError> {
            Err(AppError::Message("backend locked".to_string()))
        }
        async fn set_target_raw(&self, _target_name: &str, _value: &str) -> Result<(), AppError> {
            Err(AppError::Message("backend locked".to_string()))
        }
    }

    fn failing_state() -> AppState {
        AppState::new(
            Arc::new(crate::database::Database::memory().expect("memory db")),
            Arc::new(FailingSecretStore),
        )
    }

    #[test]
    fn claude_env_pairs_propagate_backend_error() {
        let state = failing_state();
        let result = ProviderService::fetch_provider_secrets(&state, &AppType::Claude, "p");
        assert!(
            result.is_err(),
            "读取失败必须传播为 Err，绝不能降级成空钥匙注入终端"
        );
    }

    #[test]
    fn codex_env_pairs_propagate_backend_error() {
        let state = failing_state();
        let result = ProviderService::fetch_provider_secrets(&state, &AppType::Codex, "p");
        assert!(
            result.is_err(),
            "Codex 读取失败必须传播为 Err，而不是当成缺钥匙告警"
        );
    }

    #[test]
    fn pi_env_pairs_propagate_backend_error() {
        let state = failing_state();
        let result = ProviderService::fetch_provider_secrets(&state, &AppType::Pi, "p");
        assert!(
            result.is_err(),
            "Pi 读取失败必须传播为 Err，而不是当成缺钥匙告警"
        );
    }
}

#[cfg(test)]
mod vault_call_count_tests {
    //! §9.3 / 原则 1 的护栏：锁死每流程的 vault.fetch 往返次数。
    use super::*;
    use crate::secrets::{CountingVault, InMemorySecretStore, SecretStore, SecretTarget};
    use serde_json::json;
    use std::sync::Arc;

    fn counting_state() -> (AppState, Arc<CountingVault>) {
        let store: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::new());
        let db = Arc::new(crate::database::Database::memory().expect("memory db"));
        let mut state = AppState::new(db, store);
        let counting = Arc::new(CountingVault::new(state.vault.clone()));
        state.vault = counting.clone();
        (state, counting)
    }

    #[test]
    fn fetch_provider_secrets_is_exactly_one_fetch() {
        let (state, counting) = counting_state();
        futures::executor::block_on(state.secrets.store(
            &SecretTarget::provider_api_key(AppType::Claude, "p1"),
            "sk-1",
        ))
        .expect("seed");
        counting.reset();
        let secrets =
            ProviderService::fetch_provider_secrets(&state, &AppType::Claude, "p1").expect("fetch");
        assert_eq!(counting.fetch_count(), 1, "取整包只能一次往返");
        assert_eq!(counting.put_count(), 0);
        assert_eq!(secrets.api_key.as_deref().map(String::as_str), Some("sk-1"));
    }

    #[test]
    fn reveal_provider_secret_is_exactly_one_fetch() {
        let (state, counting) = counting_state();
        let provider =
            Provider::from_parts("p1".to_string(), "P1".to_string(), json!({"env": {}}), None);
        state.db.save_provider("claude", &provider).expect("save");
        futures::executor::block_on(state.secrets.store(
            &SecretTarget::provider_api_key(AppType::Claude, "p1"),
            "sk-reveal",
        ))
        .expect("seed");
        counting.reset();
        let value =
            crate::reveal_provider_secret_internal(&state, AppType::Claude, "p1", "api_key")
                .expect("reveal");
        assert_eq!(value.as_deref(), Some("sk-reveal"));
        assert_eq!(counting.fetch_count(), 1, "显示明文只能一次往返");
    }

    #[test]
    fn add_provider_secrets_is_put_only() {
        let (state, counting) = counting_state();
        let mut provider = Provider::from_parts(
            "p1".to_string(),
            "P1".to_string(),
            json!({"env": {"ANTHROPIC_AUTH_TOKEN": "sk-x"}}),
            None,
        );
        counting.reset();
        super::strip_and_store_provider_secrets(&state, &AppType::Claude, &mut provider, false)
            .expect("store");
        assert_eq!(counting.put_count(), 1, "新增 put 一次");
        assert_eq!(counting.fetch_count(), 0, "新增不该 fetch");
    }

    #[test]
    fn edit_provider_secrets_without_new_key_is_noop_and_preserves_old() {
        let (state, counting) = counting_state();
        let mut seed = Provider::from_parts(
            "p1".to_string(),
            "P1".to_string(),
            json!({"env": {"ANTHROPIC_AUTH_TOKEN": "sk-old"}}),
            None,
        );
        super::strip_and_store_provider_secrets(&state, &AppType::Claude, &mut seed, false)
            .expect("seed");
        counting.reset();
        // 编辑：表单未回传密钥（已剥离）→ 无新密钥 → 不写（不 fetch 不 put），旧值保留。
        let mut edited =
            Provider::from_parts("p1".to_string(), "P1".to_string(), json!({"env": {}}), None);
        super::strip_and_store_provider_secrets(&state, &AppType::Claude, &mut edited, true)
            .expect("edit");
        assert_eq!(counting.fetch_count(), 0, "无新密钥不该 fetch");
        assert_eq!(counting.put_count(), 0, "无新密钥不该 put");
        // 验证旧值保留（这里才 fetch）。
        let secrets =
            ProviderService::fetch_provider_secrets(&state, &AppType::Claude, "p1").expect("read");
        assert_eq!(
            secrets.api_key.as_deref().map(String::as_str),
            Some("sk-old"),
            "未回传的密钥应保留原值"
        );
    }

    #[test]
    fn edit_provider_secrets_with_new_key_merges() {
        let (state, counting) = counting_state();
        let mut seed = Provider::from_parts(
            "p1".to_string(),
            "P1".to_string(),
            json!({"env": {"ANTHROPIC_AUTH_TOKEN": "sk-old", "ANTHROPIC_BASE_URL": "https://old"}}),
            None,
        );
        super::strip_and_store_provider_secrets(&state, &AppType::Claude, &mut seed, false)
            .expect("seed");
        counting.reset();
        // 编辑：只改 api_key（新值），base_url 未回传 → merge 保留旧 base_url。
        let mut edited = Provider::from_parts(
            "p1".to_string(),
            "P1".to_string(),
            json!({"env": {"ANTHROPIC_AUTH_TOKEN": "sk-new"}}),
            None,
        );
        super::strip_and_store_provider_secrets(&state, &AppType::Claude, &mut edited, true)
            .expect("edit");
        assert_eq!(counting.fetch_count(), 1, "有新密钥时先 fetch 合并");
        assert_eq!(counting.put_count(), 1, "有新密钥时 put");
        let secrets =
            ProviderService::fetch_provider_secrets(&state, &AppType::Claude, "p1").expect("read");
        assert_eq!(
            secrets.api_key.as_deref().map(String::as_str),
            Some("sk-new")
        );
        assert_eq!(
            secrets.base_url.as_deref().map(String::as_str),
            Some("https://old"),
            "未回传的 base_url 应被 merge 保留"
        );
    }

    #[test]
    fn delete_provider_secrets_is_one_delete() {
        let (state, counting) = counting_state();
        counting.reset();
        super::delete_provider_secrets(&state, &AppType::Claude, "p1");
        assert_eq!(counting.delete_count(), 1, "删除一次整组");
    }
}

#[cfg(test)]
mod onepassword_scrub_tests {
    //! F1-5 回归（P0-5）：1P 模式下导入明文只进 vault，凭据管理器零触碰；
    //! vault 写入失败时保留明文并记 pending。
    use super::*;
    use crate::secrets::{
        GuardedSecretStore, InMemorySecretStore, InMemoryVault, SecretGroup, SecretVault,
    };
    use serial_test::serial;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::sync::Mutex;

    /// 记录调用次数的存储包装：守卫拦截时调用数必须为 0。
    struct CountingStore {
        inner: InMemorySecretStore,
        calls: Mutex<AtomicUsize>,
    }

    impl CountingStore {
        fn new() -> Self {
            Self {
                inner: InMemorySecretStore::new(),
                calls: Mutex::new(AtomicUsize::new(0)),
            }
        }

        fn count(&self) -> usize {
            self.calls.lock().unwrap().load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl crate::secrets::SecretStore for CountingStore {
        async fn set(
            &self,
            target: &crate::secrets::SecretTarget,
            value: Zeroizing<String>,
        ) -> Result<(), AppError> {
            self.calls.lock().unwrap().fetch_add(1, Ordering::SeqCst);
            self.inner.set(target, value).await
        }
        async fn get(
            &self,
            target: &crate::secrets::SecretTarget,
        ) -> Result<Option<Zeroizing<String>>, AppError> {
            self.inner.get(target).await
        }
        async fn delete(&self, target: &crate::secrets::SecretTarget) -> Result<(), AppError> {
            self.inner.delete(target).await
        }
        async fn probe(&self) -> Result<(), AppError> {
            self.inner.probe().await
        }
        async fn list_targets(&self, prefix: &str) -> Result<Vec<String>, AppError> {
            self.inner.list_targets(prefix).await
        }
        async fn get_target_raw(
            &self,
            target_name: &str,
        ) -> Result<Option<Zeroizing<String>>, AppError> {
            self.inner.get_target_raw(target_name).await
        }
        async fn set_target_raw(&self, target_name: &str, value: &str) -> Result<(), AppError> {
            self.inner.set_target_raw(target_name, value).await
        }
    }

    /// 隔离本机设置文件（set_secrets_import_pending 落盘路径）。
    struct TempHome {
        dir: std::path::PathBuf,
        prev: Option<std::ffi::OsString>,
    }

    impl TempHome {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("cc-switch-scrub-{tag}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join(".cc-switch")).expect("mkdir");
            std::fs::write(dir.join(".cc-switch").join("cc-switch.db"), b"").expect("placeholder");
            let prev = std::env::var_os("CC_SWITCH_TEST_HOME");
            std::env::set_var("CC_SWITCH_TEST_HOME", &dir);
            Self { dir, prev }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("CC_SWITCH_TEST_HOME", v),
                None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn onepassword_state() -> (AppState, Arc<InMemoryVault>, Arc<CountingStore>) {
        let counting = Arc::new(CountingStore::new());
        // 守卫恒开：任何对凭据管理器的调用都应让断言失败。
        let guarded: Arc<dyn crate::secrets::SecretStore> = Arc::new(
            GuardedSecretStore::with_predicate(counting.clone(), Arc::new(|| true)),
        );
        let db = Arc::new(crate::database::Database::memory().expect("memory db"));
        let mut state = AppState::new(db, guarded);
        let vault = Arc::new(InMemoryVault::new());
        state.vault = vault.clone();
        (state, vault, counting)
    }

    fn seed_provider(state: &AppState, id: &str, settings: serde_json::Value) {
        let provider = Provider::from_parts(id.to_string(), id.to_string(), settings, None);
        state
            .db
            .save_provider(AppType::Claude.as_str(), &provider)
            .expect("seed provider");
    }

    #[test]
    #[serial]
    fn scrub_routes_plaintext_into_vault_without_touching_credential_manager() {
        let _home = TempHome::new("ok");
        let (state, vault, counting) = onepassword_state();
        seed_provider(
            &state,
            "p1",
            serde_json::json!({
                "env": {"ANTHROPIC_AUTH_TOKEN": "sk-import-1", "ANTHROPIC_BASE_URL": "https://x.example"}
            }),
        );

        let pending = super::scrub_imported_plaintext_via_vault(&state).expect("scrub");
        assert!(pending.is_empty(), "全部成功时无 pending");
        assert!(
            crate::settings::get_secrets_import_pending().is_empty(),
            "成功后清空 pending 标记"
        );
        assert_eq!(counting.count(), 0, "凭据管理器必须零调用");

        // 钥匙进入 vault。
        let bundle = vault
            .fetch(&SecretGroup::provider(AppType::Claude, "p1"))
            .expect("fetch")
            .expect("存在");
        assert_eq!(
            bundle.get("api_key").map(|v| v.to_string()),
            Some("sk-import-1".into())
        );
        // secret_refs 已登记。
        let fields = state
            .db
            .get_secret_ref_fields("claude", "p1")
            .expect("refs")
            .expect("非空");
        assert!(fields.iter().any(|f| f == "api_key"));
        // DB 已剥离明文。
        let row = state.db.get_provider_by_id("p1", "claude").expect("row");
        let config = row.expect("存在").settings_config.to_string();
        assert!(!config.contains("sk-import-1"), "明文必须剥离: {config}");
    }

    #[test]
    #[serial]
    fn scrub_keeps_plaintext_and_marks_pending_when_vault_write_fails() {
        let _home = TempHome::new("fail");
        let counting = Arc::new(CountingStore::new());
        let guarded: Arc<dyn crate::secrets::SecretStore> = Arc::new(
            GuardedSecretStore::with_predicate(counting.clone(), Arc::new(|| true)),
        );
        let db = Arc::new(crate::database::Database::memory().expect("memory db"));
        let mut state = AppState::new(db, guarded);
        // vault 一律失败（模拟 1P 锁定 / 断网）。
        state.vault = Arc::new(crate::secrets::UnavailableVault::new(
            crate::secrets::VaultError::Locked,
        ));
        seed_provider(
            &state,
            "p1",
            serde_json::json!({"env": {"ANTHROPIC_AUTH_TOKEN": "sk-keep-1"}}),
        );

        let pending = super::scrub_imported_plaintext_via_vault(&state).expect("scrub");
        assert_eq!(pending, vec!["claude/p1".to_string()], "失败行记 pending");
        assert_eq!(
            crate::settings::get_secrets_import_pending(),
            vec!["claude/p1".to_string()],
            "pending 标记写入本机设置"
        );
        // DB 保持原样（明文未剥离）。
        let row = state.db.get_provider_by_id("p1", "claude").expect("row");
        let config = row.expect("存在").settings_config.to_string();
        assert!(config.contains("sk-keep-1"), "失败行必须保留明文: {config}");
        assert!(counting.count() == 0);
    }

    /// S4-6（P1-7）：干净的快照导入后 scrub 必须零写入——逐行 UPDATE 会把
    /// 整库写放大成 N 次 update_hook 事件，VACUUM 还会在下载后处理里空跑一次。
    #[test]
    #[serial]
    fn scrub_writes_nothing_for_already_clean_snapshot() {
        let _home = TempHome::new("clean");
        let (state, vault, counting) = onepassword_state();
        // 只剩模型等非敏感字段：提取后与原样一致，secrets 为空。
        seed_provider(
            &state,
            "clean",
            serde_json::json!({"env": {"ANTHROPIC_MODEL": "claude-opus-4"}}),
        );

        let hooks = crate::test_support::HookCounts::install(&state.db);
        hooks.reset();

        let pending = super::scrub_imported_plaintext_via_vault(&state).expect("scrub");

        assert!(pending.is_empty(), "干净行无 pending");
        assert_eq!(
            hooks.count_for_table("providers"),
            0,
            "干净快照不得产生任何 providers 写入（否则也不会走 VACUUM 分支）"
        );
        assert_eq!(hooks.total(), 0, "整库零写入");
        assert_eq!(counting.count(), 0, "凭据管理器必须零调用");
        assert!(
            vault
                .fetch(&SecretGroup::provider(AppType::Claude, "clean"))
                .expect("fetch")
                .is_none(),
            "无秘密的行不该建 vault 条目"
        );
    }

    /// S4-6（P1-7）：混合快照只重写真正被剥离的那一行。
    #[test]
    #[serial]
    fn scrub_only_rewrites_rows_it_actually_stripped() {
        let _home = TempHome::new("mixed");
        let (state, _vault, _counting) = onepassword_state();
        seed_provider(
            &state,
            "clean",
            serde_json::json!({"env": {"ANTHROPIC_MODEL": "claude-opus-4"}}),
        );
        seed_provider(
            &state,
            "dirty",
            serde_json::json!({"env": {"ANTHROPIC_AUTH_TOKEN": "sk-mixed-1"}}),
        );

        let hooks = crate::test_support::HookCounts::install(&state.db);
        hooks.reset();

        let pending = super::scrub_imported_plaintext_via_vault(&state).expect("scrub");

        assert!(pending.is_empty());
        assert_eq!(
            hooks.count_for_table("providers"),
            1,
            "只有脏行该被 UPDATE，干净行不得跟着重写"
        );
        let clean = state
            .db
            .get_provider_by_id("clean", "claude")
            .expect("row")
            .expect("存在")
            .settings_config;
        assert_eq!(clean["env"]["ANTHROPIC_MODEL"], "claude-opus-4");
    }
}

#[cfg(test)]
mod onepassword_edit_rollback_tests {
    //! F1-3 回归（P0-3）：编辑供应商时 DB 写失败不得删除该供应商已有的钥匙。
    use super::*;
    use crate::secrets::{InMemorySecretStore, InMemoryVault};
    use std::sync::Arc;

    /// 用 SQLite 触发器注入 `save_provider` 的 UPDATE 失败：
    /// 只拦 UPDATE，不影响前置的 SELECT 校验路径。
    fn block_provider_updates(state: &AppState) -> Result<(), AppError> {
        let conn = crate::database::lock_conn!(state.db.conn);
        conn.execute_batch(
            "CREATE TRIGGER block_provider_update BEFORE UPDATE ON providers
             BEGIN SELECT RAISE(ABORT, 'injected update failure'); END;",
        )
        .map_err(|e| AppError::Database(e.to_string()))
    }

    #[test]
    fn update_keeps_old_secrets_when_save_provider_fails() {
        let store: Arc<dyn crate::secrets::SecretStore> = Arc::new(InMemorySecretStore::new());
        let mut state = AppState::new(
            Arc::new(crate::database::Database::memory().expect("memory db")),
            store,
        );
        let vault = Arc::new(InMemoryVault::new());
        state.vault = vault.clone();

        // 旧供应商已存有 api_key + extra_env（vault 与 secret_refs 均有）。
        let mut seed = Provider::from_parts(
            "p1".to_string(),
            "P1".to_string(),
            serde_json::json!({
                "env": {"ANTHROPIC_AUTH_TOKEN": "sk-old", "OPENROUTER_API_KEY": "or-old"}
            }),
            None,
        );
        super::strip_and_store_provider_secrets(&state, &AppType::Claude, &mut seed, false)
            .expect("seed");
        state
            .db
            .save_provider(AppType::Claude.as_str(), &seed)
            .expect("seed db row");

        // 注入 save_provider 失败，再编辑（表单不回传密钥 → merge 语义）。
        block_provider_updates(&state).expect("create trigger");
        let edited = Provider::from_parts(
            "p1".to_string(),
            "P1 renamed".to_string(),
            serde_json::json!({"env": {}}),
            None,
        );
        let result = ProviderService::update(&state, AppType::Claude, None, edited);
        assert!(result.is_err(), "注入失败后 update 必须报错");

        // 关键断言：旧钥匙与 extra_env 仍可 fetch 到，secret_refs 行仍在。
        let secrets = ProviderService::fetch_provider_secrets(&state, &AppType::Claude, "p1")
            .expect("旧钥匙仍可读取");
        assert_eq!(
            secrets.api_key.as_deref().map(String::as_str),
            Some("sk-old"),
            "编辑失败不得删掉已有 api_key"
        );
        assert!(
            secrets.extra_env.contains_key("OPENROUTER_API_KEY"),
            "编辑失败不得删掉已有 extra_env"
        );
        let fields = state
            .db
            .get_secret_ref_fields("claude", "p1")
            .expect("refs")
            .expect("secret_refs 行必须仍在");
        assert!(fields.iter().any(|f| f == "api_key"));
        let _ = vault;
    }
}

#[cfg(test)]
mod onepassword_endpoint_tests {
    //! F1-2 回归（P0-1 / §7 D3-A）：非敏感 base_url 存本地端点表（0 次 op 读取），
    //! 敏感 URL 仍存 vault；resolve_base_url 端点表优先 + 懒迁移。
    use super::*;
    use crate::secrets::{
        CountingVault, InMemorySecretStore, InMemoryVault, SecretGroup, SecretVault,
    };
    use std::sync::Arc;

    fn state_with_counting_vault() -> (AppState, Arc<InMemoryVault>, Arc<CountingVault>) {
        let store: Arc<dyn crate::secrets::SecretStore> = Arc::new(InMemorySecretStore::new());
        let mut state = AppState::new(
            Arc::new(crate::database::Database::memory().expect("memory db")),
            store,
        );
        let vault = Arc::new(InMemoryVault::new());
        let counting = Arc::new(CountingVault::new(vault.clone()));
        state.vault = counting.clone();
        (state, vault, counting)
    }

    #[test]
    fn non_sensitive_base_url_goes_to_vault_and_endpoint_cache() {
        let (state, vault, _) = state_with_counting_vault();
        let secrets = ProviderSecrets::new()
            .with_api_key("sk-1")
            .with_base_url("https://api.example.com");
        store_provider_bundle(&state, &AppType::Codex, "p1", &secrets, false, None).expect("store");

        // D3-B：端点表有值（缓存）；vault 整包含 base_url；refs 登记 base_url。
        assert_eq!(
            state
                .db
                .get_provider_endpoint("codex", "p1")
                .expect("endpoint"),
            Some("https://api.example.com".to_string())
        );
        let bundle = vault
            .fetch(&SecretGroup::provider(AppType::Codex, "p1".to_string()))
            .expect("fetch")
            .expect("条目存在");
        assert!(bundle.contains(crate::secrets::FIELD_API_KEY));
        assert_eq!(
            bundle
                .get(crate::secrets::FIELD_BASE_URL)
                .map(|v| v.to_string()),
            Some("https://api.example.com".into()),
            "base_url 随整包进 vault（D3-B）"
        );
        let fields = state
            .db
            .get_secret_ref_fields("codex", "p1")
            .expect("refs")
            .expect("非空");
        assert!(fields.iter().any(|f| f == crate::secrets::FIELD_BASE_URL));
    }

    /// D3-B：base_url 进 vault 后，端点-only 更新会触发一次 fetch + put（vault
    /// 整包的 base_url 变了），端点表同步更新。
    #[test]
    fn endpoint_only_update_rewrites_vault_bundle() {
        let (state, vault, counting) = state_with_counting_vault();
        // 先放一个旧整包进 vault。
        let mut old = crate::secrets::SecretBundle::new();
        old.insert(
            crate::secrets::FIELD_API_KEY.to_string(),
            Zeroizing::new("sk-old".to_string()),
        );
        old.insert(
            crate::secrets::FIELD_BASE_URL.to_string(),
            Zeroizing::new("https://old.example.com".to_string()),
        );
        counting
            .put(
                &SecretGroup::provider(AppType::Codex, "p1".to_string()),
                &old,
            )
            .expect("seed");
        counting.reset();

        // 只更新端点（api_key 未回传，merge 保留旧值）。
        let secrets = ProviderSecrets::new().with_base_url("https://new.example.com");
        store_provider_bundle(&state, &AppType::Codex, "p1", &secrets, true, None).expect("store");

        assert_eq!(counting.fetch_count(), 1, "merge 需要一次 fetch");
        assert_eq!(counting.put_count(), 1, "base_url 变化需要一次 put");
        assert_eq!(
            state
                .db
                .get_provider_endpoint("codex", "p1")
                .expect("endpoint"),
            Some("https://new.example.com".to_string())
        );
        let bundle = vault
            .fetch(&SecretGroup::provider(AppType::Codex, "p1".to_string()))
            .expect("fetch")
            .expect("条目存在");
        assert_eq!(
            bundle
                .get(crate::secrets::FIELD_BASE_URL)
                .map(|v| v.to_string()),
            Some("https://new.example.com".into())
        );
        assert_eq!(
            bundle
                .get(crate::secrets::FIELD_API_KEY)
                .map(|v| v.to_string()),
            Some("sk-old".into()),
            "merge 保留旧钥匙"
        );
    }

    /// 同值整包不重复写：merge 后与 vault 现状一致 → fetch 1 次但 put 0 次。
    #[test]
    fn identical_bundle_skips_put() {
        let (state, _, counting) = state_with_counting_vault();
        let secrets = ProviderSecrets::new()
            .with_api_key("sk-1")
            .with_base_url("https://api.example.com");
        store_provider_bundle(&state, &AppType::Codex, "p1", &secrets, false, None).expect("store");
        counting.reset();

        store_provider_bundle(&state, &AppType::Codex, "p1", &secrets, true, None).expect("store");
        assert_eq!(counting.fetch_count(), 1, "merge 需要一次 fetch");
        assert_eq!(counting.put_count(), 0, "整包无变化不得 put");
    }

    #[test]
    fn sensitive_base_url_stays_in_vault() {
        let (state, vault, _) = state_with_counting_vault();
        let secrets = ProviderSecrets::new()
            .with_api_key("sk-1")
            .with_base_url("https://user:pass@api.example.com");
        store_provider_bundle(&state, &AppType::Codex, "p1", &secrets, false, None).expect("store");

        assert_eq!(
            state
                .db
                .get_provider_endpoint("codex", "p1")
                .expect("endpoint"),
            None,
            "带 userinfo 的 URL 不进端点表"
        );
        let bundle = vault
            .fetch(&SecretGroup::provider(AppType::Codex, "p1".to_string()))
            .expect("fetch")
            .expect("条目存在");
        assert_eq!(
            bundle
                .get(crate::secrets::FIELD_BASE_URL)
                .map(|v| v.to_string()),
            Some("https://user:pass@api.example.com".into())
        );
    }

    #[test]
    fn resolve_base_url_prefers_endpoint_and_lazy_migrates() {
        let (state, vault, counting) = state_with_counting_vault();

        // 情形 1：端点表命中 → 0 次 op。
        state
            .db
            .upsert_provider_endpoint("codex", "p1", "https://from-table.example.com")
            .expect("seed endpoint");
        let url = resolve_base_url(&state, &AppType::Codex, "p1").expect("resolve");
        assert_eq!(
            url.map(|u| u.to_string()),
            Some("https://from-table.example.com".to_string())
        );
        assert_eq!(counting.fetch_count(), 0, "端点表命中必须 0 次 fetch");

        // 情形 2：端点表没有、vault 有（拆分前历史数据）→ fetch 1 次 + 懒迁移落端点表。
        let mut bundle = crate::secrets::SecretBundle::new();
        bundle.insert(
            crate::secrets::FIELD_BASE_URL.to_string(),
            Zeroizing::new("https://lazy.example.com".to_string()),
        );
        vault
            .put(
                &SecretGroup::provider(AppType::Codex, "p2".to_string()),
                &bundle,
            )
            .expect("seed vault");
        state
            .db
            .upsert_secret_ref(
                "codex",
                "p2",
                "v",
                "i2",
                &[crate::secrets::FIELD_BASE_URL.to_string()],
            )
            .expect("seed ref");
        counting.reset();
        let url = resolve_base_url(&state, &AppType::Codex, "p2").expect("resolve");
        assert_eq!(counting.fetch_count(), 1, "懒迁移首次读取 fetch 1 次");
        assert_eq!(
            url.map(|u| u.to_string()),
            Some("https://lazy.example.com".to_string())
        );
        assert_eq!(
            state
                .db
                .get_provider_endpoint("codex", "p2")
                .expect("endpoint"),
            Some("https://lazy.example.com".to_string()),
            "懒迁移应写端点表"
        );
        // 情形 3：懒迁移完成后 → 0 次 op。
        counting.reset();
        let url = resolve_base_url(&state, &AppType::Codex, "p2").expect("resolve");
        assert_eq!(counting.fetch_count(), 0, "懒迁移后读取 0 次 fetch");
        assert_eq!(
            url.map(|u| u.to_string()),
            Some("https://lazy.example.com".to_string())
        );

        // 情形 4：都没有 → None。
        let none = resolve_base_url(&state, &AppType::Codex, "missing").expect("resolve");
        assert!(none.is_none());
    }

    #[test]
    fn delete_provider_cascades_endpoint_row() {
        let store: Arc<dyn crate::secrets::SecretStore> = Arc::new(InMemorySecretStore::new());
        let state = AppState::new(
            Arc::new(crate::database::Database::memory().expect("memory db")),
            store,
        );
        state
            .db
            .upsert_provider_endpoint("codex", "p1", "https://x")
            .expect("seed");
        state.db.delete_provider("codex", "p1").expect("delete");
        assert_eq!(
            state
                .db
                .get_provider_endpoint("codex", "p1")
                .expect("endpoint"),
            None,
            "删除供应商应级联删除端点行"
        );
    }
}

#[cfg(test)]
mod onepassword_live_strip_tests {
    //! F1-8 回归（P0-8）：启动剥离改「就地、定点、有备份才剥」——无明文零写入；
    //! 有明文且 refs 有 api_key 才定点剥离（其余内容原样）；没有备份不动文件、记
    //! `live_plaintext_pending`；「导入到 1Password 并剥离」收进 vault 后再剥。
    use super::*;
    use crate::secrets::{CountingVault, InMemorySecretStore, InMemoryVault, SecretGroup};
    use serial_test::serial;
    use std::sync::Arc;

    /// Claude live 夹具：含敏感键 + 应保留的自定义内容。
    const CLAUDE_DIRTY: &str = r#"{
  "model": "claude-opus-5",
  "custom": "keep-me",
  "env": {
    "ANTHROPIC_AUTH_TOKEN": "sk-claude-plain",
    "ANTHROPIC_BASE_URL": "https://claude.example",
    "ANTHROPIC_MODEL": "claude-opus-5"
  }
}"#;

    /// Codex config.toml 夹具：注释与其它键必须原样保留。
    const CODEX_CONFIG_DIRTY: &str = "# user comment\n[model_providers.a]\nname = \"A\"\nbase_url = \"https://a.example/v1\"\nexperimental_bearer_token = \"sk-codex-plain\"\n";

    struct TempHome {
        dir: std::path::PathBuf,
        prev: Option<std::ffi::OsString>,
    }

    impl TempHome {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("cc-switch-live-strip-{tag}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join(".cc-switch")).expect("mkdir");
            std::fs::write(dir.join(".cc-switch").join("cc-switch.db"), b"").expect("placeholder");
            let prev = std::env::var_os("CC_SWITCH_TEST_HOME");
            std::env::set_var("CC_SWITCH_TEST_HOME", &dir);
            Self { dir, prev }
        }

        fn path(&self, rel: &str) -> std::path::PathBuf {
            self.dir.join(rel)
        }

        fn write(&self, rel: &str, content: &str) -> std::path::PathBuf {
            let path = self.path(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(&path, content).expect("write fixture");
            path
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("CC_SWITCH_TEST_HOME", v),
                None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// 种下 Claude/Codex 当前供应商（DB 行不含钥匙）；`with_backup` 决定是否登记 refs api_key。
    fn strip_state(with_backup: bool) -> (AppState, Arc<CountingVault>) {
        let store: Arc<dyn crate::secrets::SecretStore> = Arc::new(InMemorySecretStore::new());
        let mut state = AppState::new(
            Arc::new(crate::database::Database::memory().expect("memory db")),
            store,
        );
        let counting = Arc::new(CountingVault::new(Arc::new(InMemoryVault::new())));
        state.vault = counting.clone();
        for (app, id) in [(AppType::Claude, "p1"), (AppType::Codex, "c1")] {
            let provider =
                Provider::from_parts(id.to_string(), id.to_string(), serde_json::json!({}), None);
            state
                .db
                .save_provider(app.as_str(), &provider)
                .expect("seed provider");
            crate::settings::set_current_provider(&app, Some(id)).expect("set current");
            if with_backup {
                state
                    .db
                    .upsert_secret_ref(app.as_str(), id, "", "item", &["api_key".to_string()])
                    .expect("seed ref");
            }
        }
        (state, counting)
    }

    #[test]
    #[serial]
    fn strip_leaves_clean_live_files_byte_identical() {
        let home = TempHome::new("clean");
        let claude = home.write(".claude/settings.json", "{\n  \"model\": \"opus\"\n}");
        let config = home.write(
            ".codex/config.toml",
            "# keep\n[model_providers.a]\nname = \"A\"\n",
        );
        let (state, counting) = strip_state(false);

        strip_current_live_plaintext(&state).expect("strip");

        assert_eq!(
            std::fs::read_to_string(&claude).expect("read"),
            "{\n  \"model\": \"opus\"\n}",
            "无明文的 live 文件必须字节不变"
        );
        assert_eq!(
            std::fs::read_to_string(&config).expect("read"),
            "# keep\n[model_providers.a]\nname = \"A\"\n"
        );
        assert!(crate::settings::get_live_plaintext_pending().is_empty());
        assert_eq!(counting.fetch_count(), 0, "启动剥离不得 fetch");
        assert_eq!(counting.put_count(), 0, "启动剥离不得 put");
    }

    #[test]
    #[serial]
    fn strip_removes_only_sensitive_keys_when_backup_exists() {
        let home = TempHome::new("strip");
        let claude = home.write(".claude/settings.json", CLAUDE_DIRTY);
        home.write(
            ".codex/auth.json",
            r#"{"OPENAI_API_KEY": "sk-codex-plain"}"#,
        );
        let config = home.write(".codex/config.toml", CODEX_CONFIG_DIRTY);
        let (state, _counting) = strip_state(true);

        strip_current_live_plaintext(&state).expect("strip");

        // Claude：敏感键没了，其余键原样保留。
        let claude_after = std::fs::read_to_string(&claude).expect("read");
        assert!(!claude_after.contains("sk-claude-plain"));
        assert!(claude_after.contains("\"model\": \"claude-opus-5\""));
        assert!(claude_after.contains("\"custom\": \"keep-me\""));
        assert!(claude_after.contains("\"ANTHROPIC_MODEL\": \"claude-opus-5\""));
        // Codex：auth.json 的 key 没了；config.toml 只少 token 行，注释和其它键保留。
        let auth_after = std::fs::read_to_string(home.path(".codex/auth.json")).expect("read");
        assert!(!auth_after.contains("sk-codex-plain"));
        let config_after = std::fs::read_to_string(&config).expect("read");
        assert!(!config_after.contains("sk-codex-plain"));
        assert!(!config_after.contains("experimental_bearer_token"));
        assert!(config_after.contains("# user comment"));
        assert!(config_after.contains("base_url = \"https://a.example/v1\""));
        assert!(crate::settings::get_live_plaintext_pending().is_empty());
    }

    #[test]
    #[serial]
    fn strip_defers_and_keeps_plaintext_when_no_backup() {
        let home = TempHome::new("defer");
        let claude = home.write(".claude/settings.json", CLAUDE_DIRTY);
        home.write(
            ".codex/auth.json",
            r#"{"OPENAI_API_KEY": "sk-codex-plain"}"#,
        );
        let config = home.write(".codex/config.toml", CODEX_CONFIG_DIRTY);
        let (state, counting) = strip_state(false);

        strip_current_live_plaintext(&state).expect("strip");

        // 文件全部原样（含明文），pending 有记录，op 零调用。
        assert!(std::fs::read_to_string(&claude)
            .expect("read")
            .contains("sk-claude-plain"));
        assert!(std::fs::read_to_string(&config)
            .expect("read")
            .contains("sk-codex-plain"));
        let pending = crate::settings::get_live_plaintext_pending();
        assert!(
            pending.contains(&"claude/p1".to_string()),
            "pending: {pending:?}"
        );
        assert!(
            pending.contains(&"codex/c1".to_string()),
            "pending: {pending:?}"
        );
        assert_eq!(counting.fetch_count(), 0);
        assert_eq!(counting.put_count(), 0);
    }

    #[test]
    #[serial]
    fn import_moves_live_plaintext_into_vault_then_strips() {
        let home = TempHome::new("import");
        let claude = home.write(".claude/settings.json", CLAUDE_DIRTY);
        home.write(
            ".codex/auth.json",
            r#"{"OPENAI_API_KEY": "sk-codex-plain"}"#,
        );
        let config = home.write(".codex/config.toml", CODEX_CONFIG_DIRTY);
        let (state, counting) = strip_state(false);
        strip_current_live_plaintext(&state).expect("strip → deferred");
        assert_eq!(counting.put_count(), 0);

        let imported = import_live_plaintext_to_vault(&state).expect("import");
        assert_eq!(imported, 2, "Claude + Codex 各导入一个");

        // vault 里有两把 key（对应两个 provider 组）。
        for (app, id) in [(AppType::Claude, "p1"), (AppType::Codex, "c1")] {
            let bundle = state
                .vault
                .fetch(&SecretGroup::provider(app.clone(), id.to_string()))
                .expect("fetch")
                .unwrap_or_else(|| panic!("{app:?}/{id} 应有整包"));
            assert!(bundle.get("api_key").is_some(), "{app:?}/{id} 缺 api_key");
        }
        // live 已剥离、注释与其它键保留。
        assert!(!std::fs::read_to_string(&claude)
            .expect("read")
            .contains("sk-claude-plain"));
        let config_after = std::fs::read_to_string(&config).expect("read");
        assert!(!config_after.contains("sk-codex-plain"));
        assert!(config_after.contains("# user comment"));
        // 非敏感 base_url 落端点表。
        assert_eq!(
            state
                .db
                .get_provider_endpoint("claude", "p1")
                .expect("endpoint"),
            Some("https://claude.example".to_string())
        );
        // pending 清空。
        assert!(crate::settings::get_live_plaintext_pending().is_empty());
    }
}

#[cfg(test)]
mod f3_boundary_tests {
    //! F3 回归：线程/命令边界与孤儿处理（P1-4 / P1-5 / P1-10）。
    use super::*;
    use crate::secrets::{
        CountingVault, InMemoryVault, SecretBundle, SecretGroup, SecretStore, SecretVault,
        VaultError, VaultRef, VaultStatus,
    };
    use serial_test::serial;
    use std::sync::Arc;

    /// 隔离本机设置文件（`onepassword_orphans` 等本机标记的落盘路径）。
    struct TempHome {
        dir: std::path::PathBuf,
        prev: Option<std::ffi::OsString>,
    }

    impl TempHome {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("cc-switch-f3-{tag}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join(".cc-switch")).expect("mkdir");
            let prev = std::env::var_os("CC_SWITCH_TEST_HOME");
            std::env::set_var("CC_SWITCH_TEST_HOME", &dir);
            crate::settings::update_settings(crate::settings::AppSettings::default())
                .expect("reset settings");
            Self { dir, prev }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("CC_SWITCH_TEST_HOME", v),
                None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// delete 恒失败的 vault（模拟 1P 锁定/断网）。
    struct FailingDeleteVault {
        inner: Arc<InMemoryVault>,
        delete_count: std::sync::atomic::AtomicUsize,
    }

    impl FailingDeleteVault {
        fn new() -> Self {
            Self {
                inner: Arc::new(InMemoryVault::new()),
                delete_count: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl SecretVault for FailingDeleteVault {
        fn fetch(&self, group: &SecretGroup) -> Result<Option<SecretBundle>, VaultError> {
            self.inner.fetch(group)
        }
        fn put(&self, group: &SecretGroup, bundle: &SecretBundle) -> Result<VaultRef, VaultError> {
            self.inner.put(group, bundle)
        }
        fn delete(&self, _group: &SecretGroup) -> Result<(), VaultError> {
            self.delete_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(VaultError::Locked)
        }
        fn status(&self) -> VaultStatus {
            VaultStatus::Ready
        }
        fn vault_id(&self) -> String {
            String::new()
        }
        fn backend_name(&self) -> &'static str {
            "failing-delete"
        }
    }

    /// F3-8（P1-10 / D12）：vault.delete 失败时供应商侧照删 best-effort，但组键必须
    /// 记入 `onepassword_orphans`（不能当作「已清理」）；vault 可用后由清理动作出队。
    #[test]
    #[serial]
    fn delete_provider_secrets_records_orphan_when_delete_fails() {
        let _home = TempHome::new("orphan-record");
        // 孤儿记录只在 1P 模式下生效（见 delete_provider_secrets 的门控）。
        crate::settings::mutate_settings(|s| s.secret_backend = Some("onepassword".to_string()))
            .expect("set backend");
        let store: Arc<dyn SecretStore> = Arc::new(crate::secrets::InMemorySecretStore::new());
        let db = Arc::new(crate::database::Database::memory().expect("memory db"));
        let mut state = AppState::new(db, store);
        let failing = Arc::new(FailingDeleteVault::new());
        state.vault = failing.clone();

        super::delete_provider_secrets(&state, &AppType::Claude, "p1");
        assert_eq!(
            failing
                .delete_count
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            crate::settings::get_onepassword_orphans(),
            vec!["claude/p1".to_string()],
            "删除失败的组必须记孤儿"
        );

        // vault 恢复可用后：清理动作归档孤儿并出队。
        state.vault = Arc::new(InMemoryVault::new());
        let removed = super::cleanup_onepassword_orphans(&state, &[]).expect("cleanup");
        assert_eq!(removed, 1);
        assert!(crate::settings::get_onepassword_orphans().is_empty());
    }

    /// F3-8：删除成功时不应残留孤儿记录（此前失败过、本次重删成功的组出队）。
    #[test]
    #[serial]
    fn delete_provider_secrets_drops_orphan_record_on_success() {
        let _home = TempHome::new("orphan-clear");
        let store: Arc<dyn SecretStore> = Arc::new(crate::secrets::InMemorySecretStore::new());
        let db = Arc::new(crate::database::Database::memory().expect("memory db"));
        let mut state = AppState::new(db, store);
        state.vault = Arc::new(InMemoryVault::new());
        crate::settings::set_onepassword_orphans(vec!["claude/p1".to_string()])
            .expect("seed orphans");

        super::delete_provider_secrets(&state, &AppType::Claude, "p1");
        assert!(crate::settings::get_onepassword_orphans().is_empty());
    }

    /// F3-3（P1-5 / 陷阱 §12.8）：严格模式（默认开，1P 恒严格）下 `adopt_env_vars`
    /// 直接拒绝——不 fetch、不写注册表。
    #[test]
    #[serial]
    fn adopt_env_vars_rejected_in_strict_mode_without_fetch() {
        let _home = TempHome::new("adopt-strict");
        assert!(
            crate::settings::strict_for(&AppType::Claude),
            "默认设置应为严格投递"
        );

        let store: Arc<dyn SecretStore> = Arc::new(crate::secrets::InMemorySecretStore::new());
        let db = Arc::new(crate::database::Database::memory().expect("memory db"));
        let mut state = AppState::new(db, store);
        let counting = Arc::new(CountingVault::new(state.vault.clone()));
        state.vault = counting.clone();

        let provider = Provider::from_parts(
            "p1".to_string(),
            "P1".to_string(),
            serde_json::json!({}),
            None,
        );
        state
            .db
            .save_provider(AppType::Claude.as_str(), &provider)
            .expect("seed provider");

        let err = ProviderService::adopt_env_vars(
            &state,
            &AppType::Claude,
            "p1",
            &["ANTHROPIC_AUTH_TOKEN".to_string()],
        )
        .expect_err("严格模式必须拒绝");
        assert!(
            matches!(&err, crate::error::AppError::Localized { key, .. } if *key == "env_delivery.adopt_strict"),
            "错误应为 env_delivery.adopt_strict，实际: {err:?}"
        );
        assert_eq!(counting.fetch_count(), 0, "拒绝路径不得触发任何取钥匙");
    }
}

#[cfg(test)]
mod s0_p0_3_repro_tests {
    //! S0 复现测试（施工方案 §3.1 P0-3 / §5.4 S4-2）：1P 模式下端点缓存压过
    //! vault 真值。S4-2 修复后 vault 的 base_url 必须胜出并顺带校正缓存。

    use super::*;
    use crate::secrets::{
        CountingVault, InMemorySecretStore, SecretBundle, SecretGroup, SecretStore, FIELD_BASE_URL,
    };
    use serial_test::serial;
    use std::sync::Arc;

    #[test]
    #[serial]
    fn in_1p_mode_vault_base_url_wins_over_endpoint_cache() {
        let _test_home = crate::test_support::TestHomeGuard::new();
        let _backend = crate::test_support::OnePasswordBackendGuard::new();

        let store: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::new());
        let db = Arc::new(crate::database::Database::memory().expect("memory db"));
        let mut state = AppState::new(db, store);
        let counting = Arc::new(CountingVault::new(state.vault.clone()));
        state.vault = counting.clone();

        // 1P 整包里是新端点（真源）；本机缓存里是旧端点。
        let group = SecretGroup::provider(AppType::Claude, "p1");
        let mut bundle = SecretBundle::new();
        bundle.insert(
            FIELD_BASE_URL,
            Zeroizing::new("https://vault.example".to_string()),
        );
        state.vault.put(&group, &bundle).expect("seed vault bundle");
        state
            .db
            .upsert_provider_endpoint("claude", "p1", "https://cache.example")
            .expect("seed endpoint cache");

        let secrets =
            ProviderService::fetch_provider_secrets(&state, &AppType::Claude, "p1").expect("fetch");
        assert_eq!(
            secrets.base_url.as_deref().map(|s| s.as_str()),
            Some("https://vault.example"),
            "1P 模式下 vault 是端点真源，本机缓存不得压过 vault 值（P0-3 / S4-2）"
        );
        // 顺带把缓存校正到真值：下次走 resolve_base_url 就是 0 次 op 的新值。
        assert_eq!(
            state
                .db
                .get_provider_endpoint("claude", "p1")
                .expect("read cache")
                .as_deref(),
            Some("https://vault.example"),
            "fetch 必须顺带校正本机端点缓存"
        );
        assert_eq!(counting.fetch_count(), 1, "取整包仍然只有一次往返");
    }

    /// S4-2：凭据管理器模式不回归——该模式 vault 侧可能留着 D3-A 时期的旧副本，
    /// 缓存才是本机权威（§9-6）。
    #[test]
    #[serial]
    fn credential_manager_mode_keeps_cache_priority() {
        let _test_home = crate::test_support::TestHomeGuard::new();

        let store: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::new());
        let db = Arc::new(crate::database::Database::memory().expect("memory db"));
        let mut state = AppState::new(db, store);
        let counting = Arc::new(CountingVault::new(state.vault.clone()));
        state.vault = counting.clone();

        let group = SecretGroup::provider(AppType::Claude, "p1");
        let mut bundle = SecretBundle::new();
        bundle.insert(
            FIELD_BASE_URL,
            Zeroizing::new("https://vault-old.example".to_string()),
        );
        state.vault.put(&group, &bundle).expect("seed vault bundle");
        state
            .db
            .upsert_provider_endpoint("claude", "p1", "https://cache.example")
            .expect("seed endpoint cache");

        let secrets =
            ProviderService::fetch_provider_secrets(&state, &AppType::Claude, "p1").expect("fetch");
        assert_eq!(
            secrets.base_url.as_deref().map(|s| s.as_str()),
            Some("https://cache.example"),
            "凭据管理器模式维持缓存优先"
        );
        assert_eq!(
            state
                .db
                .get_provider_endpoint("claude", "p1")
                .expect("read cache")
                .as_deref(),
            Some("https://cache.example"),
            "凭据管理器模式不得被 vault 值改写缓存"
        );
    }

    /// S4-2：vault 整包里没有 base_url 时回落到本机缓存。
    #[test]
    #[serial]
    fn in_1p_mode_falls_back_to_cache_when_vault_has_no_base_url() {
        let _test_home = crate::test_support::TestHomeGuard::new();
        let _backend = crate::test_support::OnePasswordBackendGuard::new();

        let store: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::new());
        let db = Arc::new(crate::database::Database::memory().expect("memory db"));
        let mut state = AppState::new(db, store);
        let counting = Arc::new(CountingVault::new(state.vault.clone()));
        state.vault = counting.clone();

        // 整包只有 api_key，没有 base_url。
        let group = SecretGroup::provider(AppType::Claude, "p1");
        let mut bundle = SecretBundle::new();
        bundle.insert(
            crate::secrets::FIELD_API_KEY,
            Zeroizing::new("sk-only-key".to_string()),
        );
        state.vault.put(&group, &bundle).expect("seed vault bundle");
        state
            .db
            .upsert_provider_endpoint("claude", "p1", "https://cache.example")
            .expect("seed endpoint cache");

        let secrets =
            ProviderService::fetch_provider_secrets(&state, &AppType::Claude, "p1").expect("fetch");
        assert_eq!(
            secrets.base_url.as_deref().map(|s| s.as_str()),
            Some("https://cache.example"),
            "vault 没有 base_url 时应回落到本机缓存"
        );
    }

    /// S4-2 / §9-7：vault 里的端点带凭据时删除本机缓存——敏感 URL 绝不落端点表。
    #[test]
    #[serial]
    fn in_1p_mode_drops_cache_when_vault_url_is_credential_bearing() {
        let _test_home = crate::test_support::TestHomeGuard::new();
        let _backend = crate::test_support::OnePasswordBackendGuard::new();

        let store: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::new());
        let db = Arc::new(crate::database::Database::memory().expect("memory db"));
        let mut state = AppState::new(db, store);
        let counting = Arc::new(CountingVault::new(state.vault.clone()));
        state.vault = counting.clone();

        let group = SecretGroup::provider(AppType::Claude, "p1");
        let mut bundle = SecretBundle::new();
        bundle.insert(
            FIELD_BASE_URL,
            Zeroizing::new("https://user:pw@vault.example/v1".to_string()),
        );
        state.vault.put(&group, &bundle).expect("seed vault bundle");
        state
            .db
            .upsert_provider_endpoint("claude", "p1", "https://cache.example")
            .expect("seed endpoint cache");

        let secrets =
            ProviderService::fetch_provider_secrets(&state, &AppType::Claude, "p1").expect("fetch");
        assert_eq!(
            secrets.base_url.as_deref().map(|s| s.as_str()),
            Some("https://user:pw@vault.example/v1"),
            "带凭据的 URL 照常返回给调用方（凭据是用户的）"
        );
        assert_eq!(
            state
                .db
                .get_provider_endpoint("claude", "p1")
                .expect("read cache"),
            None,
            "带凭据的 URL 不得留在本机端点缓存（§9-7）"
        );
    }

    /// S4-2：缓存已与 vault 一致时不写库（否则每次 fetch 都产生 update_hook 事件，
    /// 凭据管理器模式下会推高自动同步的触发频率）。
    #[test]
    #[serial]
    fn in_1p_mode_does_not_rewrite_matching_cache() {
        let _test_home = crate::test_support::TestHomeGuard::new();
        let _backend = crate::test_support::OnePasswordBackendGuard::new();

        let store: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::new());
        let db = Arc::new(crate::database::Database::memory().expect("memory db"));
        let mut state = AppState::new(db, store);
        let counting = Arc::new(CountingVault::new(state.vault.clone()));
        state.vault = counting.clone();

        let group = SecretGroup::provider(AppType::Claude, "p1");
        let mut bundle = SecretBundle::new();
        bundle.insert(
            FIELD_BASE_URL,
            Zeroizing::new("https://same.example".to_string()),
        );
        state.vault.put(&group, &bundle).expect("seed vault bundle");
        state
            .db
            .upsert_provider_endpoint("claude", "p1", "https://same.example")
            .expect("seed endpoint cache");

        let hooks = crate::test_support::HookCounts::install(&state.db);
        hooks.reset();

        ProviderService::fetch_provider_secrets(&state, &AppType::Claude, "p1").expect("fetch");

        assert_eq!(
            hooks.count_for_table("provider_endpoints"),
            0,
            "缓存与真值一致时不得写库"
        );
    }
}

// ─── SEC-03：孤儿判定与提交复检（安全方案 §5 SEC-03） ─────────────────

#[cfg(test)]
mod orphan_tests {
    use super::{filter_archivable, orphan_candidates, OnePasswordOrphan};
    use crate::app_config::AppType;
    use crate::secrets::OpItemListEntry;
    use crate::secrets::SecretGroup;
    use std::collections::HashSet;

    fn entry(id: &str, title: &str) -> OpItemListEntry {
        OpItemListEntry {
            id: id.to_string(),
            title: title.to_string(),
            updated_at: "2026-09-28T00:00:00Z".to_string(),
        }
    }

    fn ids(out: &[OnePasswordOrphan]) -> Vec<&str> {
        out.iter().map(|o| o.item_id.as_str()).collect()
    }

    /// 在用判定只看 item id：新格式标题（<app>/<显示名>）的在用条目不得被列为
    /// 孤儿（SEC-03 的核心缺陷——旧实现按旧标题字符串匹配，会把它们全列出来）。
    #[test]
    fn in_use_items_with_new_title_are_not_orphans() {
        let items = vec![
            entry("item-new-title", "claude/OpenRouter"),
            entry("item-legacy-title", "cc-switch/claude/p1"),
        ];
        let in_use: HashSet<String> = ["item-new-title", "item-legacy-title"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let out = orphan_candidates(&items, &in_use, &[], |_| false);
        assert!(
            ids(&out).is_empty(),
            "在用条目（新旧标题）都不得被列为孤儿，实际: {out:?}"
        );
    }

    /// AppSync 条目始终在用；重复标题下只要任一条目在用，另一条也按候选处理但
    /// 不得归档在用那条（在用条目被排除，孤儿判定不按标题整组排除）。
    #[test]
    fn appsync_and_empty_ids_are_excluded() {
        let items = vec![
            entry("item-sync", "cc-switch/app/sync"),
            entry("", "claude/Ghost"),
            entry("item-dup-in-use", "claude/OpenRouter"),
            entry("item-dup-orphan", "claude/OpenRouter"),
        ];
        let in_use: HashSet<String> = ["item-sync", "item-dup-in-use"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let out = orphan_candidates(&items, &in_use, &[], |_| false);
        assert_eq!(ids(&out), vec!["item-dup-orphan"]);
    }

    /// 有本机显式删除记录（待重试组键）→ confirmed；其余一律 needs_review，
    /// 且 reason 必须说明不确定原因（不得宣称「安全可删除」）。
    #[test]
    fn confidence_and_reason_reflect_evidence() {
        let items = vec![
            entry("item-pending", "cc-switch/claude/gone"),
            entry("item-exists", "cc-switch/claude/Live"),
            entry("item-stray", "claude/Ghost"),
            entry("item-unparsed", "user-made-entry"),
        ];
        let in_use: HashSet<String> = HashSet::new();
        let pending = vec!["claude/gone".to_string()];
        let out = orphan_candidates(&items, &in_use, &pending, |group| {
            matches!(group, SecretGroup::Provider{ app, provider_id }
                if app == &AppType::Claude && provider_id == "Live")
        });

        let by_id = |id: &str| out.iter().find(|o| o.item_id == id).unwrap();
        let pending_row = by_id("item-pending");
        assert_eq!(pending_row.confidence, "confirmed");
        let exists_row = by_id("item-exists");
        assert_eq!(exists_row.confidence, "needs_review");
        assert!(exists_row.reason.contains("重建引用"));
        let stray_row = by_id("item-stray");
        assert_eq!(stray_row.confidence, "needs_review");
        let unparsed_row = by_id("item-unparsed");
        assert_eq!(unparsed_row.confidence, "needs_review");
        for row in &out {
            assert!(
                !row.reason.contains("安全"),
                "候选 reason 不得宣称「安全可删除」: {row:?}"
            );
        }
    }

    /// 提交复检：已恢复引用（在用）与不在合法候选范围的 id 一律拒绝。
    #[test]
    fn filter_archivable_rejects_relinked_and_out_of_scope_ids() {
        let in_use: HashSet<String> = ["item-relabeled".to_string()].into_iter().collect();
        let legal: HashSet<String> = ["item-still-orphan".to_string(), "item-pending".to_string()]
            .into_iter()
            .collect();
        let confirmed = vec![
            "item-still-orphan".to_string(),
            "item-relabeled".to_string(),
            "item-out-of-scope".to_string(),
        ];
        let archivable = filter_archivable(&confirmed, &in_use, &legal);
        assert_eq!(archivable, vec!["item-still-orphan".to_string()]);
    }
}
