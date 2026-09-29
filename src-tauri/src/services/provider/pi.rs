use super::{ProviderService, SwitchResult};
use crate::app_config::AppType;
use crate::error::AppError;
use crate::provider::Provider;
use crate::secrets::SecretExtractor;
use crate::store::AppState;
use indexmap::IndexMap;
use serde_json::Value;
use std::sync::Mutex;

const PI_APP: &str = "pi";

/// S1-4：models.json 指纹（进程级缓存）。`None` = 需要完整原生同步
/// （首次进入，或被 [`invalidate_native_fingerprint`] 失效）。
static NATIVE_FINGERPRINT: Mutex<Option<NativeFingerprint>> = Mutex::new(None);

/// 一次成功原生同步时 models.json 的指纹：先比 len + mtime（不同直接视为变化），
/// 相同再比 sha256（models.json 很小，哈希微秒级，§4.2 S1-4）。
#[derive(Clone, PartialEq, Eq)]
struct NativeFingerprint {
    len: u64,
    modified: std::time::SystemTime,
    sha256: [u8; 32],
}

/// S1-4：外部事件使指纹失效，强制下次 `list` 完整原生同步。
///
/// 调用点：Pi 的 add / update / delete / remove / enable、导入明文
/// （`import_pi_plaintext_to_vault`）、（S4 起）SQL 导入 / `.db` 恢复 / 云同步
/// 下载、后端切换。漏掉一个就会出现「DB 已被覆盖，但列表不再和原生对齐」
/// （§4.2 S1-4 / §9-9）。
pub(crate) fn invalidate_native_fingerprint() {
    if let Ok(mut fp) = NATIVE_FINGERPRINT.lock() {
        *fp = None;
    }
}

fn compute_native_fingerprint() -> Option<NativeFingerprint> {
    let path = crate::pi_config::get_pi_models_path().ok()?;
    let bytes = std::fs::read(&path).ok()?;
    let meta = std::fs::metadata(&path).ok()?;
    let len = meta.len();
    if len != bytes.len() as u64 {
        // models.json 是原子写入，正常读不到半写文件；读到就当作「未对齐」。
        return None;
    }
    let modified = meta.modified().ok()?;
    Some(NativeFingerprint {
        len,
        modified,
        sha256: sha256(&bytes),
    })
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// 指纹未变（可短路）时返回 `true`。
fn native_fingerprint_unchanged() -> bool {
    let cached = match NATIVE_FINGERPRINT.lock() {
        Ok(guard) => guard.clone(),
        Err(_) => return false,
    };
    let Some(cached) = cached else {
        return false;
    };
    let Some(current) = compute_native_fingerprint() else {
        return false;
    };
    cached.len == current.len
        && cached.modified == current.modified
        && cached.sha256 == current.sha256
}

/// 记录当前 models.json 指纹（仅在原生同步成功后调用）。
fn store_native_fingerprint() {
    if let Ok(mut fp) = NATIVE_FINGERPRINT.lock() {
        *fp = compute_native_fingerprint();
    }
}

pub(super) fn list(state: &AppState) -> Result<IndexMap<String, Provider>, AppError> {
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(PI_APP));
    // S1-4：models.json 未变且未被事件失效时，跳过全量原生同步直接读 DB。
    // 原生契约「每次进入列表都同步外部修改」只在文件变化时才需要兑现；短路省掉
    // 提取、清洗、比较的全部 CPU / IO，语义不变（§4.2 S1-4）。
    if !native_fingerprint_unchanged() {
        match crate::pi_config::read_pi_native_providers() {
            Ok(native) => match sync_native_locked(state, &native) {
                // 同步失败不记指纹，下次进入列表重试。
                Ok(_) => store_native_fingerprint(),
                Err(error) => {
                    log::warn!("Failed to sync Pi providers from native config: {error}");
                }
            },
            Err(error) => {
                log::warn!("Failed to read Pi providers; showing saved catalog: {error}");
            }
        }
    }
    state.db.get_all_providers(PI_APP)
}

pub(super) fn import_from_live(state: &AppState) -> Result<usize, AppError> {
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(PI_APP));
    let native = crate::pi_config::read_pi_native_providers()?;
    sync_native_locked(state, &native)
}

pub(super) fn reapply_live(state: &AppState) -> Result<usize, AppError> {
    let native = crate::pi_config::read_pi_native_providers()?;
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(PI_APP));
    sync_native_locked(state, &native)?;
    drop(_guard);
    let mut applied = 0;
    for id in native.keys() {
        enable(state, id)?;
        applied += 1;
    }
    Ok(applied)
}

pub(super) fn add(
    state: &AppState,
    mut provider: Provider,
    add_to_live: bool,
) -> Result<bool, AppError> {
    let app_type = AppType::Pi;
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(app_type.as_str()));
    // S1-4（§9-9）：写路径让指纹失效，下次 list 强制与原生重新对齐。
    invalidate_native_fingerprint();
    strip_unsupported_pi_metadata(&mut provider);
    ProviderService::validate_provider_settings(state, &app_type, &provider, None)?;
    align_native_display_name(&mut provider);

    if state
        .db
        .get_provider_by_id(&provider.id, app_type.as_str())?
        .is_some()
    {
        return Err(AppError::InvalidInput(format!(
            "Pi provider '{}' already exists",
            provider.id
        )));
    }

    if !add_to_live && crate::pi_config::pi_provider_exists(&provider.id)? {
        return Err(AppError::InvalidInput(format!(
            "Pi provider key '{}' already exists in models.json",
            provider.id
        )));
    }

    let live_config = provider.settings_config.clone();
    strip_and_store_pi_secrets(state, &mut provider, false)?;

    let native_inserted = if add_to_live {
        crate::pi_config::insert_pi_provider(&provider.id, &live_config)?
    } else {
        false
    };

    if let Err(error) = state.db.save_provider(app_type.as_str(), &provider) {
        if native_inserted {
            if let Err(rollback) =
                crate::pi_config::remove_pi_provider_if_matches(&provider.id, &live_config)
            {
                return Err(AppError::Config(format!(
                    "failed to save Pi provider: {error}; native rollback failed: {rollback}"
                )));
            }
        }
        return Err(error);
    }
    Ok(true)
}

pub(super) fn update(
    state: &AppState,
    original_id: Option<&str>,
    mut provider: Provider,
    credential_patch: Option<crate::provider::CredentialPatch>,
) -> Result<bool, AppError> {
    let app_type = AppType::Pi;
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(app_type.as_str()));
    // S1-4（§9-9）：写路径让指纹失效，下次 list 强制与原生重新对齐。
    invalidate_native_fingerprint();
    let original_id = original_id.unwrap_or(&provider.id).to_string();
    if original_id != provider.id {
        return Err(AppError::InvalidInput(
            "Pi provider keys cannot be renamed".to_string(),
        ));
    }

    let existing_provider = state
        .db
        .get_provider_by_id(&original_id, app_type.as_str())?
        .ok_or_else(|| AppError::InvalidInput(format!("Pi provider '{original_id}' not found")))?;
    strip_unsupported_pi_metadata(&mut provider);
    ProviderService::validate_provider_settings(state, &app_type, &provider, None)?;

    let live_config = provider.settings_config.clone();
    // P3/P4（安全方案 §7.2）：Pi 编辑先纯提取 → 显式意图合并 → 分类——普通配置
    // 编辑（模型、备注等）零 vault 调用，且钥匙未变时不重投环境变量。
    let extracted =
        SecretExtractor::extract(&provider.id, &AppType::Pi, &provider.settings_config)?;
    provider.settings_config = extracted.stripped;
    // P4（§7.2-6）：显示名有效变化（ID 不变）→ 标题显式传入 patch，不被空包吞掉。
    let name_changed = existing_provider.name != provider.name;
    let has_explicit_intent = credential_patch
        .as_ref()
        .is_some_and(|p| p.has_explicit_intent());
    let (merged_secrets, explicit_clear) = match credential_patch {
        Some(patch) if patch.has_explicit_intent() => {
            super::apply_credential_intents(extracted.secrets, &patch)
        }
        _ => (extracted.secrets, Vec::new()),
    };
    let classification = if has_explicit_intent {
        super::EditSecretClassification::VaultRequired(merged_secrets)
    } else {
        super::classify_edit_secrets(state, &app_type, &provider.id, &merged_secrets)?
    };
    // P5（§7.5-5）：vault 是否已在本轮提交（patch 成功）。之后的本地失败必须
    // 报告阶段「1Password 已更新，本地未保存」，不能伪装成整体失败。
    let (vault_committed, credentials_changed) = match classification {
        super::EditSecretClassification::ConfigOnly => {
            // 零调用分支：显示名变化时仅更新标题（条目不存在则不创建），
            // 其余情况 vault 全 0。
            if name_changed {
                super::patch_provider_secrets(
                    state,
                    &app_type,
                    &provider.id,
                    &crate::secrets::ProviderSecrets::new(),
                    Some(&provider.name),
                    &[],
                )?;
                (true, false)
            } else {
                (false, false)
            }
        }
        super::EditSecretClassification::VaultRequired(secrets) => {
            // P4（§7.3/§7.5）：一次定位 + 至多一次 edit；vault 成功后才写引用/缓存。
            super::patch_provider_secrets(
                state,
                &app_type,
                &provider.id,
                &secrets,
                Some(&provider.name),
                &explicit_clear,
            )?;
            (true, true)
        }
    };

    // 缺陷 D-1：live 节点只留 `$CC_SWITCH_PI_<ID>_API_KEY` 引用，编辑密钥后不重投该变量
    // 就会继续解析到旧 key。次序沿用 enable：②投变量 → ③写节点；节点本就不在 models.json
    // （未启用）时不投，免得留下无人引用的变量。零调用分支钥匙未变，跳过重投。
    if credentials_changed
        && crate::pi_config::pi_provider_exists(&original_id)?
        && ProviderService::provider_has_stored_key(state, &app_type, &provider.id)?
    {
        let mut delivered = SwitchResult::default();
        if let Err(err) = ProviderService::deliver_env_credentials_pub(
            state,
            &app_type,
            &provider,
            &mut delivered,
        ) {
            if vault_committed {
                return Err(super::phase_vault_saved_local_failed(&err));
            }
            return Err(err);
        }
        for warning in &delivered.warnings {
            log::warn!("编辑 Pi 供应商后重投环境变量的提醒: {warning}");
        }
    }

    let previous_native =
        crate::pi_config::replace_pi_provider_if_present(&original_id, &live_config).map_err(
            |err| {
                if vault_committed {
                    super::phase_vault_saved_local_failed(&err)
                } else {
                    err
                }
            },
        )?;
    if let Err(error) = state.db.save_provider(app_type.as_str(), &provider) {
        // P5（§9.2-5）：报告阶段，禁止「已经回滚」泛称——回滚结果如实并入消息。
        let failure = if let Some(previous_native) = previous_native.as_ref() {
            if let Err(rollback) =
                crate::pi_config::replace_pi_provider(&original_id, &live_config, previous_native)
            {
                format!("{error}; native rollback failed: {rollback}")
            } else {
                format!("{error}（live 节点已回滚到原样）")
            }
        } else {
            format!("{error}")
        };
        if vault_committed {
            return Err(super::phase_vault_saved_local_failed(&AppError::Config(
                failure,
            )));
        }
        return Err(AppError::Config(failure));
    }
    Ok(true)
}

pub(super) fn delete(state: &AppState, id: &str) -> Result<(), AppError> {
    let app_type = AppType::Pi;
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(app_type.as_str()));
    // S1-4（§9-9）：写路径让指纹失效，下次 list 强制与原生重新对齐。
    invalidate_native_fingerprint();
    let Some(_) = state.db.get_provider_by_id(id, app_type.as_str())? else {
        return Ok(());
    };
    // Delete is intentionally keyed by provider ID. Once the user confirms
    // deleting the provider itself, supported field edits do not change that
    // intent; the latest native value is retained only for rollback.
    let removed = crate::pi_config::remove_pi_provider(id)?;
    // §5.3.3 第 3 条 + §5.4「删除供应商」：live 节点 → 托管环境变量 → 凭据 → DB 行。
    super::ProviderService::release_provider_managed_env(state, &app_type, id);
    super::delete_provider_secrets(state, &app_type, id);

    if let Err(error) = state.db.delete_provider(app_type.as_str(), id) {
        if let Some(removed) = removed.as_ref() {
            if let Err(rollback) = crate::pi_config::restore_pi_provider_if_missing(id, removed) {
                return Err(AppError::Config(format!(
                    "failed to delete Pi provider: {error}; native rollback failed: {rollback}"
                )));
            }
        }
        return Err(error);
    }
    Ok(())
}

pub(super) fn remove(state: &AppState, id: &str) -> Result<(), AppError> {
    let app_type = AppType::Pi;
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(app_type.as_str()));
    // S1-4（§9-9）：写路径让指纹失效，下次 list 强制与原生重新对齐。
    invalidate_native_fingerprint();
    let provider = state
        .db
        .get_provider_by_id(id, app_type.as_str())?
        .ok_or_else(|| AppError::InvalidInput(format!("Pi provider '{id}' not found")))?;
    let Some(removed) = crate::pi_config::remove_pi_provider(id)? else {
        return Ok(());
    };
    let mut synced = provider;
    merge_native_config(&mut synced, removed.clone());
    if let Err(error) = state.db.save_provider(app_type.as_str(), &synced) {
        if let Err(rollback) = crate::pi_config::restore_pi_provider_if_missing(id, &removed) {
            return Err(AppError::Config(format!(
                "failed to preserve Pi provider before removal: {error}; native rollback failed: {rollback}"
            )));
        }
        return Err(error);
    }
    // 从 live 移除后不再持有该供应商的环境变量（DB 行保留，下次启用会重新投递）。
    ProviderService::release_provider_managed_env(state, &app_type, id);
    Ok(())
}

/// §5.3.1：把 baseUrl 合入待写入 models.json 的节点。
/// DB 行已剥掉 baseUrl，只有 live 侧需要它（Pi CLI 不支持 baseUrl 的环境变量引用）。
/// F1-2（D3-A）：baseUrl 走端点表 / 懒迁移读取，不再整包 fetch（避免顺带取回钥匙）。
fn hydrate_pi_base_url_for_live(state: &AppState, provider: &Provider) -> Result<Value, AppError> {
    let mut config = provider.settings_config.clone();
    if let Some(url) = super::resolve_base_url(state, &AppType::Pi, &provider.id)? {
        if let Some(obj) = config.as_object_mut() {
            obj.insert("baseUrl".to_string(), Value::String(url.to_string()));
        }
    }
    Ok(config)
}

pub(super) fn enable(state: &AppState, id: &str) -> Result<SwitchResult, AppError> {
    let app_type = AppType::Pi;
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(app_type.as_str()));
    // S1-4（§9-9）：写路径让指纹失效，下次 list 强制与原生重新对齐。
    invalidate_native_fingerprint();
    let provider = state
        .db
        .get_provider_by_id(id, app_type.as_str())?
        .ok_or_else(|| AppError::InvalidInput(format!("Pi provider '{id}' not found")))?;

    if crate::pi_config::read_pi_native_provider(id)?.is_some() {
        let mut result = SwitchResult::default();
        ProviderService::deliver_env_credentials_pub(state, &app_type, &provider, &mut result)?;
        return Ok(result);
    }

    ProviderService::validate_provider_settings(state, &app_type, &provider, None)?;
    ProviderService::preflight_env_delivery(state, &app_type, &provider)?;
    // §5.3.1：Pi 的 baseUrl 没有环境变量间接引用，live 节点必须写凭据管理器里的
    // 那一份；DB 行已剥离 baseUrl，直接写会让模型不可用。
    let live_config = hydrate_pi_base_url_for_live(state, &provider)?;

    // 次序按 §5.3.1 的 ②投变量 → ③写节点 → ④broadcast：反过来的话，写节点成功、
    // 投递失败会留下指向不存在变量的 `apiKey: "$CC_SWITCH_PI_…"`，而且没有撤销路径
    // （注册表权限、白名单拒绝、变量名超限都会让 sink.set 失败）。现在写节点失败
    // 可以整体撤销刚投递的变量。
    let mut result = SwitchResult::default();
    ProviderService::deliver_env_credentials_pub(state, &app_type, &provider, &mut result)?;
    if let Err(error) = crate::pi_config::insert_pi_provider(id, &live_config) {
        ProviderService::undo_env_delivery(state, &app_type, &provider);
        return Err(error);
    }
    Ok(result)
}

fn sync_native_locked(
    state: &AppState,
    native: &IndexMap<String, Value>,
) -> Result<usize, AppError> {
    let saved = state.db.get_all_providers(PI_APP)?;
    let is_1p = crate::settings::is_onepassword_backend();
    let mut changed = 0;
    let mut plaintext_pending: Vec<String> = Vec::new();
    let mut endpoint_vault_pending: Vec<String> = Vec::new();

    for (id, config) in native {
        let mut provider = saved.get(id).cloned().unwrap_or_else(|| {
            let name = native_provider_name(config).unwrap_or(id).to_string();
            let mut imported = Provider::with_id(id.clone());
            imported.name = name;
            imported.settings_config = config.clone();
            imported.category = Some("custom".to_string());
            imported.icon = Some("pi".to_string());
            imported
        });
        let is_new = !saved.contains_key(id);
        let previous_name = provider.name.clone();
        let previous_config = provider.settings_config.clone();
        merge_native_config(&mut provider, config.clone());
        let extracted =
            SecretExtractor::extract(&provider.id, &AppType::Pi, &provider.settings_config)?;

        // S1-1（D3-B）：把「真正的钥匙」和「非敏感 base_url」分开判断。Pi 的
        // models.json 里必有明文 baseUrl（Pi CLI 不支持 baseUrl 的 $VAR 引用），
        // D3-B 把 base_url 放进整包后 `store_provider_bundle` 不再是空操作——
        // 「拆掉非敏感 baseUrl 后整包为空、直接返回」已不成立，每个供应商每次
        // 列表都会触发一次 6~9 秒的 op fetch（§4.1）。原则 1（列表 0 次 op）：
        // 无真钥匙时完全不碰 vault，只处理端点缓存。
        let has_real_secret = extracted.secrets.api_key.is_some()
            || !extracted.secrets.extra_env.is_empty()
            || extracted
                .secrets
                .base_url
                .as_ref()
                .is_some_and(|url| crate::secrets::is_credential_bearing_url(url.as_str()));

        // F1-4 步骤 2（1P 模式）：models.json 里有明文钥匙时，不改写 models.json、
        // 不保存 DB 行（行保持原样；新供应商不入库），记入 pending 等用户点
        // 「导入到 1Password」。原则 5 的启动 0 次 op 不能拿丢数据换——明文本来就在
        // 用户自己的 models.json 里，推迟处理不会更糟；凭据管理器模式照旧立即收编。
        if is_1p && has_real_secret {
            // F1-4 步骤 1 保留：非敏感 baseUrl 照旧落端点表——pending 供应商尚未
            // 入 1P，之后「导入到 1Password」前读取端点要靠这行做到 0 次 op。
            if let Some(url) = extracted.secrets.base_url.as_ref() {
                if !crate::secrets::is_credential_bearing_url(url.as_str()) {
                    state
                        .db
                        .upsert_provider_endpoint(PI_APP, &provider.id, url.as_str())?;
                }
            }
            log::info!("Pi 供应商 {id} 的 models.json 里有明文钥匙，待用户导入 1Password");
            plaintext_pending.push(provider.id.clone());
            continue;
        }

        // S1-1：无真钥匙（绝大多数情况：$VAR 引用 + 非敏感 baseUrl）→ 0 次 vault
        // 往返，只处理端点缓存；vault 写回推迟到用户主动触发 op 的动作（D-S9）。
        if !has_real_secret {
            sync_pi_endpoint_cache_without_vault(
                state,
                is_1p,
                &provider.id,
                extracted
                    .secrets
                    .base_url
                    .as_deref()
                    .map(|url| url.as_str()),
                &mut endpoint_vault_pending,
            )?;
        }

        let live_rewritten =
            crate::services::provider::pi_sanitizer::sanitize_pi_provider_for_live_write(
                &provider.id,
                config,
            )?;
        if live_rewritten != *config {
            match crate::pi_config::replace_pi_provider(id, config, &live_rewritten) {
                Ok(()) => {
                    if has_real_secret {
                        // 仅凭据管理器模式到达（1P 的真钥匙已在上面走 pending）。
                        if let Err(error) =
                            persist_pi_sync_secrets(state, &provider.id, &extracted.secrets)
                        {
                            log::warn!(
                                "Pi native extract after live rewrite failed for {id}: {error}"
                            );
                        }
                    }
                    provider.settings_config = extracted.stripped;
                }
                Err(error) => {
                    log::warn!(
                        "Failed to rewrite Pi models.json for '{id}', keeping original: {error}"
                    );
                    continue;
                }
            }
        } else {
            if has_real_secret {
                if let Err(error) = persist_pi_sync_secrets(state, &provider.id, &extracted.secrets)
                {
                    log::warn!("Pi native extract failed for {id}: {error}");
                    continue;
                }
            }
            provider.settings_config = extracted.stripped;
        }
        if !is_new && provider.name == previous_name && provider.settings_config == previous_config
        {
            continue;
        }

        state.db.save_provider(PI_APP, &provider)?;
        changed += 1;
    }

    // pending 全量重建：本轮没触发的 id（明文被用户自己处理、或条目已从
    // models.json 移除）自动出队；无变化不落盘。
    if crate::settings::get_pi_plaintext_pending() != plaintext_pending {
        crate::settings::set_pi_plaintext_pending(plaintext_pending)?;
    }
    // S1-1：端点写回清单同样全量重建；用户主动触发 op 的动作消化后（或 baseUrl
    // 改回原值）自动出队，无变化不落盘。
    if crate::settings::get_pi_endpoint_vault_pending() != endpoint_vault_pending {
        crate::settings::set_pi_endpoint_vault_pending(endpoint_vault_pending)?;
    }

    Ok(changed)
}

/// S1-1（§4.2 S1-1 / D-S9）：无真钥匙时只同步端点缓存，0 次 vault 往返。
///
/// - live 没有 baseUrl → 缓存维持原样（与既有行为一致）；
/// - 缓存与 live 一致 → 什么都不做（不动行、不触发 update_hook）；
/// - 不一致（用户在 CCS 之外改了 models.json 的 baseUrl）：
///   - 1P 模式：更新缓存，并把 id 记入 `pi_endpoint_vault_pending`，等用户下次
///     主动触发 op 的动作（编辑保存 / 导入明文 / 重建引用，S1-2）时顺带写回
///     1Password——原则 1：列表路径绝不调 op；
///   - 凭据管理器模式：没有「自带同步」的真源，照旧立即收编（本地调用，快）。
fn sync_pi_endpoint_cache_without_vault(
    state: &AppState,
    is_1p: bool,
    provider_id: &str,
    base_url: Option<&str>,
    pending: &mut Vec<String>,
) -> Result<(), AppError> {
    let Some(url) = base_url else {
        return Ok(());
    };
    if crate::secrets::is_credential_bearing_url(url) {
        // 敏感 URL 不落缓存，正常不会到这里（has_real_secret 已拦截）；防御性兜底。
        return Ok(());
    }
    let cached = state.db.get_provider_endpoint(PI_APP, provider_id)?;
    if cached.as_deref() == Some(url) {
        return Ok(());
    }
    if is_1p {
        state
            .db
            .upsert_provider_endpoint(PI_APP, provider_id, url)?;
        pending.push(provider_id.to_string());
        log::info!("Pi 供应商 {provider_id} 的 baseUrl 有改动，待用户下次操作时写回 1Password");
    } else {
        // 凭据管理器：store_provider_bundle 会同时更新缓存与 vault（merge 语义）。
        let secrets = crate::secrets::ProviderSecrets {
            base_url: Some(url.to_owned().into()),
            ..Default::default()
        };
        super::store_provider_bundle(state, &AppType::Pi, provider_id, &secrets, true, None)?;
    }
    Ok(())
}

fn merge_native_config(provider: &mut Provider, config: Value) {
    if let Some(name) = native_provider_name(&config) {
        provider.name = name.to_string();
    }
    provider.settings_config = config;
}

fn native_provider_name(config: &Value) -> Option<&str> {
    config
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
}

fn align_native_display_name(provider: &mut Provider) {
    let Some(config) = provider.settings_config.as_object_mut() else {
        return;
    };
    if config.contains_key("name") {
        config.insert("name".to_string(), Value::String(provider.name.clone()));
    }
}

fn strip_unsupported_pi_metadata(provider: &mut Provider) {
    // Pi doesn't support most metadata fields, so clear them
    provider.meta = None;
}

fn strip_and_store_pi_secrets(
    state: &AppState,
    provider: &mut Provider,
    merge_existing: bool,
) -> Result<(), AppError> {
    let extracted =
        SecretExtractor::extract(&provider.id, &AppType::Pi, &provider.settings_config)?;
    provider.settings_config = extracted.stripped;
    super::store_provider_bundle(
        state,
        &AppType::Pi,
        &provider.id,
        &extracted.secrets,
        merge_existing,
        Some(&provider.name),
    )
}

/// 原生 sync（启动重建 DB）专用：把抽取结果落库。
///
/// S1-1（D3-B）：调用点已收窄——`sync_native_locked` 仅在 `has_real_secret`
/// 且非 1P 明文 pending（即仅凭据管理器模式）时调用本函数。1P 模式下无真钥匙的
/// 供应商在上游经 [`sync_pi_endpoint_cache_without_vault`] 与 vault 分流，有真
/// 钥匙的走 `pi_plaintext_pending`，都不会到达这里；因此不再出现「非敏感
/// baseUrl 使整包非空 → merge 每次都 fetch」的 D3-B 回归（§4.1 / §4.2 S1-1）。
/// 凭据管理器模式本地调用快，照常写（merge 语义）。
fn persist_pi_sync_secrets(
    state: &AppState,
    provider_id: &str,
    secrets: &crate::secrets::ProviderSecrets,
) -> Result<(), AppError> {
    super::store_provider_bundle(state, &AppType::Pi, provider_id, secrets, true, None)
}

/// F1-4：「导入到 1Password」——把 pending 里 Pi 供应商的明文钥匙收进 vault，
/// 再把 models.json 改写为 $VAR 引用、DB 行保存剥离后的配置、出队 pending。
/// 用户主动触发（允许 op 往返与解锁弹窗）。返回本轮成功导入的供应商数。
pub(crate) fn import_pi_plaintext_to_vault(state: &AppState) -> Result<usize, AppError> {
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(PI_APP));
    // S1-4（§9-9）：写路径让指纹失效，下次 list 强制与原生重新对齐。
    invalidate_native_fingerprint();
    let pending = crate::settings::get_pi_plaintext_pending();
    let mut imported = 0;
    let mut remaining = Vec::new();
    for id in &pending {
        match import_one_provider(state, id) {
            Ok(true) => imported += 1,
            Ok(false) => {}
            Err(error) => {
                log::warn!("Pi 供应商 {id} 导入 1Password 失败，保留 pending 待重试: {error}");
                remaining.push(id.clone());
            }
        }
    }
    if remaining.len() != pending.len() {
        crate::settings::set_pi_plaintext_pending(remaining)?;
    }
    // S1-2：这里已经调过 op（解锁后），顺带消化端点写回清单——merge 复用同一
    // 次解锁窗口，不额外增加用户感知成本。失败保留 pending 待重试，不影响导入结果。
    if let Err(error) = flush_endpoint_vault_pending_locked(state) {
        log::warn!("端点改动写回 1Password 失败，保留 pending 待重试: {error}");
    }
    Ok(imported)
}

/// S1-2（D-S9）：把 `pi_endpoint_vault_pending` 里供应商的端点改动 merge 进
/// 1Password（每个供应商一次 fetch + put，无新增字段则不 put）。供用户主动
/// 触发的 op 动作顺带调用（编辑保存 / 导入明文 / 重建引用 / 横幅「立即写入」）。
/// 单个失败保留该 id 待重试，其余出队（全量重建语义）。
/// 调用方必须已持有 Pi 切换锁；对外入口见 [`flush_endpoint_vault_pending`]。
fn flush_endpoint_vault_pending_locked(state: &AppState) -> Result<usize, AppError> {
    let pending = crate::settings::get_pi_endpoint_vault_pending();
    let mut flushed = 0;
    let mut remaining = Vec::new();
    for id in &pending {
        let result = match state.db.get_provider_endpoint(PI_APP, id)? {
            // 供应商已删 / 缓存已清：无需写回，直接出队。
            None => Ok(()),
            Some(url) => {
                let secrets = crate::secrets::ProviderSecrets {
                    base_url: Some(url.into()),
                    ..Default::default()
                };
                super::store_provider_bundle(state, &AppType::Pi, id, &secrets, true, None)
            }
        };
        match result {
            Ok(()) => flushed += 1,
            Err(error) => {
                log::warn!("Pi 供应商 {id} 端点写回 1Password 失败，保留待重试: {error}");
                remaining.push(id.clone());
            }
        }
    }
    if remaining.len() != pending.len() {
        crate::settings::set_pi_endpoint_vault_pending(remaining)?;
    }
    Ok(flushed)
}

/// S1-2：[`flush_endpoint_vault_pending_locked`] 的对外入口（自带 Pi 切换锁）。
pub(crate) fn flush_endpoint_vault_pending(state: &AppState) -> Result<usize, AppError> {
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(PI_APP));
    flush_endpoint_vault_pending_locked(state)
}

/// S4-7（P1-6 / D-S6 决策 A）：把导入后变化的 Pi 供应商写回 `models.json`。
///
/// Pi 的原生契约是「`models.json` 为真源」，后处理（`run_post_import_sync`）
/// 历来跳过 Pi。于是设备 A 改了 Pi 供应商的模型列表并上传，设备 B 下载后 DB 里
/// 是新值，但 B 下次打开 Pi 页面时 `sync_native_locked` 又用 B 本机 `models.json`
/// 里的旧值改回去——**Pi 的模型设置事实上无法同步**。
///
/// 下载属于「用户明确要求远端覆盖本机」，契约没覆盖这个场景，所以在这里补写回：
/// - 只处理「`models.json` 里已存在（已启用）**且** DB 行在导入前后变化」的供应商；
///   未启用的供应商保持只存在于 DB（原生契约「启停即增删节点」）。
/// - 1P 模式下端点缓存缺失且 vault 里登记了 `base_url` 时**跳过并告警**，不调
///   op——这里处在导入后处理的热路径上，原则 1 是硬约束。
///
/// `before` 是导入前的 Pi DB 行快照（`snapshot_pi_providers` 拍摄，0 次 op）。
/// 返回实际写回的节点数。
pub(crate) fn apply_imported_configs_to_native(
    state: &AppState,
    before: &IndexMap<String, Provider>,
) -> Result<usize, AppError> {
    let _guard = futures::executor::block_on(state.switch_locks.lock_for_app(PI_APP));
    let after = state.db.get_all_providers(PI_APP)?;
    let is_1p = crate::settings::is_onepassword_backend();

    let mut applied = 0usize;
    for (id, provider) in after.iter() {
        // ① 导入前后没变的供应商不动手。
        if before
            .get(id)
            .is_some_and(|old| old.settings_config == provider.settings_config)
        {
            continue;
        }
        // ② models.json 里没有这个节点 = 未启用，保持只存在于 DB。
        let Ok(Some(current)) = crate::pi_config::read_pi_native_provider(id) else {
            continue;
        };
        // ③ 端点只能从本机缓存取；1P 模式下缓存缺失就别为它付一次解锁。
        let needs_vault_endpoint = is_1p
            && state.db.get_provider_endpoint(PI_APP, id)?.is_none()
            && state
                .db
                .get_secret_ref_fields(PI_APP, id)?
                .is_some_and(|fields| fields.iter().any(|f| f == crate::secrets::FIELD_BASE_URL));
        if needs_vault_endpoint {
            log::warn!(
                "S4-7：Pi 供应商 {id} 的端点缓存缺失，导入后的模型设置未写回 models.json（不触发 1Password 调用）"
            );
            continue;
        }
        // replace_pi_provider 自带 sanitizer（apiKey 改写成 $VAR 引用）与
        // revision 冲突检测（冲突就报错，这里记日志跳过）。
        let Ok(live) = hydrate_pi_base_url_for_live(state, provider) else {
            log::warn!("S4-7：Pi 供应商 {id} 合入端点失败，模型设置未写回");
            continue;
        };
        match crate::pi_config::replace_pi_provider(id, &current, &live) {
            Ok(()) => applied += 1,
            Err(error) => log::warn!("S4-7：写回 Pi 供应商 {id} 的模型设置失败: {error}"),
        }
    }
    if applied > 0 {
        // 写回后 models.json 变了，必须让指纹失效，否则下次 list 会用旧指纹
        // 短路掉原生同步，把刚写进去的节点又「同步」成 DB 里的值（§9-9）。
        invalidate_native_fingerprint();
    }
    Ok(applied)
}

/// 导入单个供应商。返回 `false` 表示 live 与 DB 里都已不存在（pending 直接出队）。
/// 次序与 enable / update 一致：①写 vault → ②投变量 → ③改写 models.json → ④存 DB。
fn import_one_provider(state: &AppState, id: &str) -> Result<bool, AppError> {
    let existing = state.db.get_provider_by_id(id, PI_APP)?;
    let native = crate::pi_config::read_pi_native_provider(id)?;
    let (config, in_native) = match native {
        Some(config) => (config, true),
        None => match &existing {
            Some(provider) => (provider.settings_config.clone(), false),
            None => return Ok(false),
        },
    };
    let mut provider = existing.unwrap_or_else(|| {
        let name = native_provider_name(&config).unwrap_or(id).to_string();
        let mut imported = Provider::with_id(id.to_string());
        imported.name = name;
        imported.category = Some("custom".to_string());
        imported.icon = Some("pi".to_string());
        imported
    });

    let extracted = SecretExtractor::extract(id, &AppType::Pi, &config)?;
    // ① 钥匙进 vault（merge 语义；非敏感 baseUrl 由 store_provider_bundle 拆去端点表）。
    super::store_provider_bundle(state, &AppType::Pi, id, &extracted.secrets, true, None)?;
    // ② 与 update 同序：live 节点马上要引用 $VAR，先走一次投递。严格模式（1P 恒
    //    严格）下这是 no-op——钥匙由「打开终端 / ccs env」注入，不写用户环境变量；
    //    非严格模式（凭据管理器）才真正把变量写入 HKCU\Environment。
    if in_native {
        let mut delivered = SwitchResult::default();
        ProviderService::deliver_env_credentials_pub(
            state,
            &AppType::Pi,
            &provider,
            &mut delivered,
        )?;
        for warning in &delivered.warnings {
            log::warn!("导入 Pi 供应商后投递环境变量的提醒: {warning}");
        }
        // ③ expected 传当前原生配置：期间被用户改过会冲突报错，pending 保留待重试。
        crate::pi_config::replace_pi_provider(id, &config, &config)?;
    }
    // ④ DB 行保存剥离后的配置（新供应商在此入库）。
    provider.settings_config = extracted.stripped;
    state.db.save_provider(PI_APP, &provider)?;
    Ok(true)
}

#[cfg(test)]
mod plaintext_pending_tests {
    //! F1-4 回归（P0-4）：1P 模式下原生同步检测到 models.json 里的明文钥匙时，
    //! 不改写 models.json、不动 DB 行，记 `pi_plaintext_pending`，vault 零往返；
    //! 「导入到 1Password」再把钥匙收进 vault、改写 live、入库剥离后的行。
    use super::*;
    use crate::secrets::{
        CountingVault, InMemorySecretStore, InMemoryVault, SecretGroup, SecretStore, SecretTarget,
        SecretVault,
    };
    use serial_test::serial;
    use std::sync::Arc;

    const PLAINTEXT_MODELS: &str = r#"{"providers":{"pi-one":{"name":"One","baseUrl":"https://x.example/v1","apiKey":"sk-plain-pi-1"}}}"#;

    /// 隔离本机设置文件（settings 落盘路径），同时让 env sink 走内存实现。
    pub(super) struct TempHome {
        dir: std::path::PathBuf,
        prev: Option<std::ffi::OsString>,
    }

    impl TempHome {
        pub(super) fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("cc-switch-pi-1p-{tag}"));
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

    /// 测试卫生：设置是进程级全局，离开测试前复位后端与 pending 标记。
    pub(super) struct OnePBackend;

    impl OnePBackend {
        pub(super) fn enable() -> Self {
            let mut settings = crate::settings::get_settings();
            settings.secret_backend = Some("onepassword".to_string());
            settings.pi_plaintext_pending = None;
            crate::settings::update_settings(settings).expect("switch backend");
            Self
        }
    }

    impl Drop for OnePBackend {
        fn drop(&mut self) {
            let mut settings = crate::settings::get_settings();
            settings.secret_backend = None;
            settings.pi_plaintext_pending = None;
            settings.secrets_import_pending = None;
            let _ = crate::settings::update_settings(settings);
        }
    }

    pub(super) fn onepassword_pi_state() -> (AppState, Arc<InMemoryVault>, Arc<CountingVault>) {
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

    pub(super) fn write_models(content: &str) -> std::path::PathBuf {
        let path = crate::pi_config::get_pi_models_path().expect("models path");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, content).expect("write models");
        path
    }

    pub(super) fn read_native() -> IndexMap<String, Value> {
        crate::pi_config::read_pi_native_providers().expect("read native")
    }

    #[test]
    #[serial]
    fn sync_1p_keeps_plaintext_models_marks_pending_and_never_touches_vault() {
        let _home = TempHome::new("sync-1p");
        let _agent = crate::pi_config::test_support::TestAgentDir::new();
        let path = write_models(PLAINTEXT_MODELS);
        let before = std::fs::read_to_string(&path).expect("read before");
        let _onep = OnePBackend::enable();
        let (state, _vault, counting) = onepassword_pi_state();

        sync_native_locked(&state, &read_native()).expect("sync");

        // models.json 字节不变；新供应商不入库（等导入命令）。
        assert_eq!(
            std::fs::read_to_string(&path).expect("read after"),
            before,
            "models.json 必须字节不变"
        );
        assert!(
            state
                .db
                .get_provider_by_id("pi-one", PI_APP)
                .expect("db")
                .is_none(),
            "检测到明文时不得保存 DB 行"
        );
        // vault 零往返（= 0 次 op）。
        assert_eq!(counting.fetch_count(), 0, "同步不得 fetch");
        assert_eq!(counting.put_count(), 0, "同步不得 put");
        assert_eq!(counting.delete_count(), 0);
        // pending 已记录；非敏感 baseUrl 已落端点表（之后读取 0 次 op 的前提）。
        assert_eq!(
            crate::settings::get_pi_plaintext_pending(),
            vec!["pi-one".to_string()]
        );
        assert_eq!(
            state
                .db
                .get_provider_endpoint(PI_APP, "pi-one")
                .expect("endpoint"),
            Some("https://x.example/v1".to_string())
        );
    }

    #[test]
    #[serial]
    fn import_moves_key_into_vault_rewrites_models_and_clears_pending() {
        let _home = TempHome::new("import-1p");
        let _agent = crate::pi_config::test_support::TestAgentDir::new();
        let path = write_models(PLAINTEXT_MODELS);
        let _onep = OnePBackend::enable();
        let (state, vault, counting) = onepassword_pi_state();
        sync_native_locked(&state, &read_native()).expect("sync");
        counting.reset();

        let imported = import_pi_plaintext_to_vault(&state).expect("import");
        assert_eq!(imported, 1, "应导入 1 个供应商");

        // vault 里有这把 key。
        let bundle = vault
            .fetch(&SecretGroup::provider(AppType::Pi, "pi-one"))
            .expect("fetch")
            .expect("vault 应有整包");
        assert_eq!(
            bundle.get("api_key").map(|v| v.to_string()),
            Some("sk-plain-pi-1".into())
        );
        // models.json 变成 $VAR 引用，明文消失，baseUrl 保留。
        let live = std::fs::read_to_string(&path).expect("read live");
        assert!(
            live.contains("$CC_SWITCH_PI_PI_ONE_API_KEY"),
            "live 应改写为 $VAR 引用: {live}"
        );
        assert!(!live.contains("sk-plain-pi-1"), "live 明文必须消失: {live}");
        assert!(
            live.contains("https://x.example/v1"),
            "baseUrl 保留: {live}"
        );
        // DB 行已入库且剥离明文。
        let row = state
            .db
            .get_provider_by_id("pi-one", PI_APP)
            .expect("db")
            .expect("导入后入库");
        assert!(
            !row.settings_config.to_string().contains("sk-plain-pi-1"),
            "DB 行必须剥离明文: {}",
            row.settings_config
        );
        // pending 清空。
        assert!(crate::settings::get_pi_plaintext_pending().is_empty());
        // 钥匙确实进了 vault（≥1 次 put；fetch 来自投递/回读，允许但不强制）。
        assert!(counting.put_count() >= 1, "导入必须写 vault");
    }

    #[test]
    #[serial]
    fn sync_windows_mode_still_stores_plaintext_locally() {
        let _home = TempHome::new("sync-win");
        let _agent = crate::pi_config::test_support::TestAgentDir::new();
        let path = write_models(PLAINTEXT_MODELS);
        let store = Arc::new(InMemorySecretStore::new());
        let state = AppState::new(
            Arc::new(crate::database::Database::memory().expect("memory db")),
            store.clone(),
        );

        sync_native_locked(&state, &read_native()).expect("sync");

        // 凭据管理器（此处为内存替身）收到钥匙。
        let key = futures::executor::block_on(
            store.get(&SecretTarget::provider_api_key(AppType::Pi, "pi-one")),
        )
        .expect("get")
        .expect("Windows 模式钥匙应照常落本地存储");
        assert_eq!(key.as_str(), "sk-plain-pi-1");
        // live 已改写为 $VAR 引用，DB 行已剥离入库，无 pending。
        let live = std::fs::read_to_string(&path).expect("read live");
        assert!(
            live.contains("$CC_SWITCH_PI_PI_ONE_API_KEY"),
            "live: {live}"
        );
        assert!(
            live.contains("https://x.example/v1"),
            "baseUrl 保留: {live}"
        );
        let row = state
            .db
            .get_provider_by_id("pi-one", PI_APP)
            .expect("db")
            .expect("Windows 模式照常入库");
        assert!(!row.settings_config.to_string().contains("sk-plain-pi-1"));
        assert!(crate::settings::get_pi_plaintext_pending().is_empty());
    }
}

#[cfg(test)]
mod list_perf_tests {
    //! S1-1 验收（施工方案 §4.3）：Pi 原生同步路径 0 次 vault 往返。
    //! 场景取自真机最常见形态：models.json 全部是 $VAR 引用 + 非敏感 baseUrl。
    use super::plaintext_pending_tests::{
        onepassword_pi_state, read_native, write_models, OnePBackend, TempHome,
    };
    use super::*;
    use serial_test::serial;

    /// 5 个供应商，全部 $VAR 引用 + 非敏感 baseUrl（1P 下最常见形态）。
    fn perf_models(override_url: Option<(usize, &str)>) -> String {
        let mut providers = serde_json::Map::new();
        for i in 0..5 {
            let url = match override_url {
                Some((idx, url)) if idx == i => url.to_string(),
                _ => format!("https://perf{i}.example/v1"),
            };
            providers.insert(
                format!("pi-perf-{i}"),
                serde_json::json!({
                    "name": format!("Perf {i}"),
                    "baseUrl": url,
                    "apiKey": format!("$CC_SWITCH_PI_PERF_{i}_API_KEY"),
                }),
            );
        }
        serde_json::json!({ "providers": providers }).to_string()
    }

    #[test]
    #[serial]
    fn sync_1p_refs_and_baseurl_never_touches_vault_across_repeated_syncs() {
        let _home = TempHome::new("perf-1p-zero");
        let _agent = crate::pi_config::test_support::TestAgentDir::new();
        write_models(&perf_models(None));
        let _onep = OnePBackend::enable();
        let (state, _vault, counting) = onepassword_pi_state();

        // 第一轮：新供应商入库。
        sync_native_locked(&state, &read_native()).expect("first sync");
        assert_eq!(
            counting.fetch_count(),
            0,
            "仅 $VAR 引用 + 非敏感 baseUrl，绝不能触发 vault fetch（S1-1 / 原则 1）"
        );
        assert_eq!(counting.put_count(), 0, "同步路径绝不能 put");

        // 连续 3 轮重复同步（等价于连续 3 次进入 Pi 页面）。
        counting.reset();
        for _ in 0..3 {
            sync_native_locked(&state, &read_native()).expect("resync");
        }
        assert_eq!(
            counting.fetch_count(),
            0,
            "重复同步仍必须 0 次 fetch（每供应商每次 6~9 秒的 op 是卡顿根因）"
        );
        assert_eq!(counting.put_count(), 0);
    }

    #[test]
    #[serial]
    fn external_baseurl_change_in_1p_updates_cache_and_defers_vault_write() {
        let _home = TempHome::new("perf-1p-change");
        let _agent = crate::pi_config::test_support::TestAgentDir::new();
        write_models(&perf_models(None));
        let _onep = OnePBackend::enable();
        let (state, _vault, counting) = onepassword_pi_state();
        sync_native_locked(&state, &read_native()).expect("first sync");

        // 用户在 CCS 之外改了其中一个的 baseUrl。
        write_models(&perf_models(Some((2, "https://changed.example/v1"))));
        counting.reset();
        sync_native_locked(&state, &read_native()).expect("resync");
        assert_eq!(
            counting.fetch_count(),
            0,
            "外部 baseUrl 改动只进缓存，不得调 op（D-S9）"
        );
        assert_eq!(counting.put_count(), 0);
        assert_eq!(
            state.db.get_provider_endpoint(PI_APP, "pi-perf-2").unwrap(),
            Some("https://changed.example/v1".to_string()),
            "缓存必须更新为本机新值"
        );
        assert_eq!(
            crate::settings::get_pi_endpoint_vault_pending(),
            vec!["pi-perf-2".to_string()],
            "改动必须记入待写回清单（S1-1 / D-S9）"
        );

        // 全量重建语义：下一轮无改动时 pending 自动出队。
        sync_native_locked(&state, &read_native()).expect("resync unchanged");
        assert!(
            crate::settings::get_pi_endpoint_vault_pending().is_empty(),
            "无改动的同步轮次应清空 pending（全量重建语义）"
        );
    }

    #[test]
    #[serial]
    fn external_baseurl_change_in_credential_manager_writes_vault_immediately() {
        let _home = TempHome::new("perf-cm-change");
        let _agent = crate::pi_config::test_support::TestAgentDir::new();
        write_models(&perf_models(None));
        // 默认凭据管理器模式（不开 1P）。
        let (state, _vault, counting) = onepassword_pi_state();
        sync_native_locked(&state, &read_native()).expect("first sync");

        // 首轮：5 个 baseUrl 都是「缓存不同」→ 各写一次（1 fetch + 1 put）。
        assert_eq!(
            counting.fetch_count(),
            5,
            "凭据管理器模式首轮收编 5 次 fetch"
        );
        assert_eq!(counting.put_count(), 5);

        // 重复同步无改动 → 0 次往返（修复前：每次都要 5 次 fetch）。
        counting.reset();
        sync_native_locked(&state, &read_native()).expect("resync");
        assert_eq!(
            counting.fetch_count(),
            0,
            "凭据管理器模式下无改动也必须 0 次 fetch"
        );
        assert_eq!(counting.put_count(), 0);

        // 改一个 baseUrl → 恰好 1 fetch + 1 put。
        write_models(&perf_models(Some((3, "https://changed-cm.example/v1"))));
        counting.reset();
        sync_native_locked(&state, &read_native()).expect("resync changed");
        assert_eq!(counting.fetch_count(), 1, "只处理改动的那个供应商");
        assert_eq!(
            counting.put_count(),
            1,
            "base_url 变化时写入 vault（1 次 put）"
        );
        assert!(
            crate::settings::get_pi_endpoint_vault_pending().is_empty(),
            "凭据管理器模式不产生待写回清单"
        );
    }

    #[test]
    #[serial]
    fn list_short_circuits_on_unchanged_models_and_realigned_after_invalidation() {
        let _home = TempHome::new("fp-short");
        let _agent = crate::pi_config::test_support::TestAgentDir::new();
        write_models(&perf_models(None));
        let _onep = OnePBackend::enable();
        let (state, _vault, counting) = onepassword_pi_state();
        let counts = crate::test_support::HookCounts::install(&state.db);

        // 第一次 list：完整同步，产生 DB 写入。
        list(&state).expect("first list");
        let writes_after_first = counts.total();
        assert!(
            writes_after_first > 0,
            "首次 list 必须完整同步（新供应商入库）"
        );
        counting.reset();

        // 第 2、3 次 list：指纹短路——0 次 DB 写入、0 次 vault 往返（§4.3）。
        list(&state).expect("second list");
        list(&state).expect("third list");
        assert_eq!(
            counts.total(),
            writes_after_first,
            "models.json 未变时 list 不得产生任何 DB 写入（S1-4 指纹短路）"
        );
        assert_eq!(counting.fetch_count(), 0);
        assert_eq!(counting.put_count(), 0);

        // 外部修改 models.json → 短路失效，同步恢复并更新端点缓存。
        write_models(&perf_models(Some((0, "https://fingerprint.example/v1"))));
        list(&state).expect("list after external change");
        assert!(
            counts.total() > writes_after_first,
            "外部修改必须打破短路并触发重新同步"
        );
        assert_eq!(
            state.db.get_provider_endpoint(PI_APP, "pi-perf-0").unwrap(),
            Some("https://fingerprint.example/v1".to_string())
        );

        // 事件失效（§9-9）：DB 行被改而 models.json 未变时，invalidate 后的
        // list 必须把 DB 拉回与原生一致——这就是失效清单不能漏的原因。
        let mut row = state
            .db
            .get_provider_by_id("pi-perf-0", PI_APP)
            .expect("read")
            .expect("row exists");
        row.name = "Tampered".to_string();
        state.db.save_provider(PI_APP, &row).expect("tamper db");
        invalidate_native_fingerprint();
        list(&state).expect("list after invalidate");
        let row = state
            .db
            .get_provider_by_id("pi-perf-0", PI_APP)
            .expect("read")
            .expect("row exists");
        assert_eq!(
            row.name, "Perf 0",
            "指纹失效后 list 必须把 DB 重新对齐到 models.json（S1-4）"
        );
    }

    #[test]
    #[serial]
    fn flush_endpoint_pending_merges_into_vault_and_clears_list() {
        use crate::secrets::{SecretGroup, SecretVault, FIELD_BASE_URL};

        let _home = TempHome::new("flush-1p");
        let _agent = crate::pi_config::test_support::TestAgentDir::new();
        write_models(&perf_models(None));
        let _onep = OnePBackend::enable();
        let (state, vault, counting) = onepassword_pi_state();
        sync_native_locked(&state, &read_native()).expect("first sync");

        // 人为记一笔端点改动（等价于外部改过 baseUrl 且已进缓存），外加一个
        // 已不存在的 ghost id（应静默出队，不产生任何往返）。
        state
            .db
            .upsert_provider_endpoint(PI_APP, "pi-perf-1", "https://flush.example/v1")
            .expect("seed cache");
        crate::settings::set_pi_endpoint_vault_pending(vec![
            "pi-perf-1".to_string(),
            "ghost".to_string(),
        ])
        .expect("seed pending");
        counting.reset();

        let flushed = flush_endpoint_vault_pending(&state).expect("flush");
        assert_eq!(flushed, 2, "真实供应商写入 + ghost 出队都算消化成功");
        assert_eq!(
            counting.fetch_count(),
            1,
            "只有真实存在的供应商产生 vault 往返"
        );
        assert_eq!(counting.put_count(), 1);

        let bundle = vault
            .fetch(&SecretGroup::provider(AppType::Pi, "pi-perf-1"))
            .expect("fetch")
            .expect("bundle exists");
        assert_eq!(
            bundle.get(FIELD_BASE_URL).map(|s| s.to_string()),
            Some("https://flush.example/v1".to_string()),
            "端点改动必须 merge 进 1Password（D-S9 消化）"
        );
        assert!(
            crate::settings::get_pi_endpoint_vault_pending().is_empty(),
            "成功的消化必须清空清单"
        );
    }
}

#[cfg(test)]
mod s4_7_import_apply_tests {
    //! S4-7（P1-6 / D-S6 决策 A）验收：下载后 Pi 的模型设置要真的写进
    //! `models.json`，否则「同步了但没生效」。
    //!
    //! 三条判据：
    //! 1. 已启用 + 导入前后有变化 → `models.json` 被更新；
    //! 2. 未启用（`models.json` 里没有该节点）→ 不写（原生契约「启停即增删」）；
    //! 3. 1P 模式端点缓存缺失且 vault 登记了 base_url → 跳过，vault 零往返。
    use super::plaintext_pending_tests::{
        onepassword_pi_state, read_native, write_models, OnePBackend, TempHome,
    };
    use super::*;
    use serial_test::serial;

    const TWO_MODELS: &str = r#"{"providers":{
        "pi-a":{"name":"A","baseUrl":"https://a.example/v1","apiKey":"$CC_SWITCH_PI_A_API_KEY","models":["m1"]},
        "pi-b":{"name":"B","baseUrl":"https://b.example/v1","apiKey":"$CC_SWITCH_PI_B_API_KEY","models":["m1"]}
    }}"#;

    /// 远端带来的新模型列表（模拟设备 A 改了 Pi 供应商的模型设置）。
    fn imported_config(models: &[&str]) -> Value {
        serde_json::json!({
            "name": "A",
            "baseUrl": "https://a.example/v1",
            "apiKey": "$CC_SWITCH_PI_A_API_KEY",
            "models": models,
        })
    }

    fn state_with_two() -> AppState {
        let (state, _vault, _counting) = onepassword_pi_state();
        sync_native_locked(&state, &read_native()).expect("seed from native");
        state
    }

    #[test]
    #[serial]
    fn applies_changed_enabled_provider_to_native() {
        let _home = TempHome::new("s47-apply");
        let _agent = crate::pi_config::test_support::TestAgentDir::new();
        write_models(TWO_MODELS);
        let _onep = OnePBackend::enable();
        let state = state_with_two();

        // 导入前的快照：与当前 DB 一致。
        let before = state.db.get_all_providers(PI_APP).expect("before");
        // 模拟导入覆盖 DB 行（模型列表变了）。
        let mut after = before.get("pi-a").expect("pi-a").clone();
        after.settings_config = imported_config(&["m1", "m2", "m3"]);
        state
            .db
            .save_provider(PI_APP, &after)
            .expect("simulate import");

        let applied = apply_imported_configs_to_native(&state, &before).expect("apply");
        assert_eq!(applied, 1, "只有 pi-a 的配置变了");

        let native = read_native();
        let live_models = native["pi-a"]["models"].as_array().expect("models");
        assert_eq!(
            live_models.len(),
            3,
            "导入带来的模型列表必须写进 models.json，否则下次进入 Pi 页就被本机旧值改回去"
        );
        // 未变化的供应商不应被动。
        assert_eq!(
            native["pi-b"]["models"].as_array().expect("b models").len(),
            1
        );
    }

    #[test]
    #[serial]
    fn skips_providers_absent_from_native() {
        let _home = TempHome::new("s47-unenabled");
        let _agent = crate::pi_config::test_support::TestAgentDir::new();
        write_models(TWO_MODELS);
        let _onep = OnePBackend::enable();
        let state = state_with_two();

        let before = state.db.get_all_providers(PI_APP).expect("before");
        // 远端新增了一个本机未启用的供应商（不在 models.json 里）。
        let mut fresh = Provider::with_id("pi-new".to_string());
        fresh.name = "New".to_string();
        fresh.settings_config = imported_config(&["m1", "m2"]);
        state.db.save_provider(PI_APP, &fresh).expect("save new");

        let applied = apply_imported_configs_to_native(&state, &before).expect("apply");
        assert_eq!(applied, 0, "未启用的供应商保持只存在于 DB（原生契约）");
        assert!(
            !read_native().contains_key("pi-new"),
            "不得凭导入凭空往 models.json 塞节点"
        );
    }

    /// 1P 模式下端点缓存缺失且 vault 登记了 base_url 时必须跳过——
    /// 导入后处理不能为它触发一次 6~9 秒的解锁（原则 1）。
    #[test]
    #[serial]
    fn skips_when_1p_endpoint_cache_missing_without_touching_vault() {
        let _home = TempHome::new("s47-nocache");
        let _agent = crate::pi_config::test_support::TestAgentDir::new();
        write_models(TWO_MODELS);
        let _onep = OnePBackend::enable();
        let (state, _vault, counting) = onepassword_pi_state();
        sync_native_locked(&state, &read_native()).expect("seed");

        // 端点缓存被清掉，但引用行仍登记着 base_url —— hydrate 会去 fetch。
        state
            .db
            .delete_provider_endpoint(PI_APP, "pi-a")
            .expect("clear cache");
        state
            .db
            .upsert_secret_ref("pi", "pi-a", "vault-1", "item-a", &["base_url".to_string()])
            .expect("mark base_url ref");

        let before = state.db.get_all_providers(PI_APP).expect("before");
        let mut after = before.get("pi-a").expect("pi-a").clone();
        after.settings_config = imported_config(&["m1", "m2"]);
        state
            .db
            .save_provider(PI_APP, &after)
            .expect("simulate import");

        counting.reset();
        let applied = apply_imported_configs_to_native(&state, &before).expect("apply");

        assert_eq!(applied, 0, "端点只能来自本机缓存，缓存缺失就跳过");
        assert_eq!(
            counting.fetch_count(),
            0,
            "绝不能为导入后处理触发 1Password 调用"
        );
        assert_eq!(counting.put_count(), 0);
    }

    /// 导入没改动任何 Pi 供应商时不该碰 models.json。
    #[test]
    #[serial]
    fn no_change_means_no_native_write() {
        let _home = TempHome::new("s47-nochange");
        let _agent = crate::pi_config::test_support::TestAgentDir::new();
        let path = write_models(TWO_MODELS);
        let _onep = OnePBackend::enable();
        let state = state_with_two();
        let before = state.db.get_all_providers(PI_APP).expect("before");
        let content_before = std::fs::read_to_string(&path).expect("read");

        let applied = apply_imported_configs_to_native(&state, &before).expect("apply");

        assert_eq!(applied, 0);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            content_before,
            "没有变化的导入不得重写 models.json"
        );
    }
}
