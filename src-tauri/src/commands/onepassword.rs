//! 1Password 后端设置命令（§8）：状态探测、账户/vault 列举、保存配置、测试取钥匙。
//!
//! 所有会调 `op` 的命令都是 async + `spawn_blocking`（`op` 是阻塞子进程，可能等解锁）。

use serde_json::{json, Value};
use tauri::{Emitter, State};

use crate::secrets;
use crate::secrets::SecretVault as _;
use crate::store::AppState;

/// F1-8：「导入到 1Password 并剥离」——把启动剥离检测到的 live 文件明文钥匙
/// （`live_plaintext_pending`）收进 vault 后就地剥离。会触发 op（可能弹解锁），
/// 故 async + spawn_blocking。返回成功导入的数量。
#[tauri::command]
pub async fn import_live_plaintext_to_onepassword(
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::services::provider::import_live_plaintext_to_vault(&state)
    })
    .await
    .map_err(|e| format!("导入 live 明文任务失败: {e}"))?
    .map_err(|e| e.to_string())
}

/// S6-3（P1-8）：重试导入「明文导入待重试」清单里的供应商钥匙（1P 模式）。
///
/// 整表重跑 `scrub_imported_plaintext_via_vault`：该函数幂等——已干净的行
/// 提取后与原样一致，不写 vault 也不 UPDATE，只有 pending 的行会真正产生
/// op 往返；比按清单逐行重跑更简单且不会漏掉清单外的漏网行。会触发 op
/// （可能弹解锁），故 async + spawn_blocking。返回重试后仍待处理的清单
/// （空 = 全部成功，前端据此刷新横幅）。
#[tauri::command]
pub async fn retry_secrets_import_pending(
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::services::provider::scrub_imported_plaintext_via_vault(&state)
    })
    .await
    .map_err(|e| format!("重试导入明文任务失败: {e}"))?
    .map_err(|e| e.to_string())
}

/// 状态探测（不需解锁）：是否安装、op 版本、路径、是否已登录、签名校验结果，
/// 外加当前后端与已配置的 account/vault。
#[tauri::command]
pub async fn onepassword_status(_state: State<'_, AppState>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let verify = crate::settings::onepassword_verify_signature();
        let configured = crate::settings::get_onepassword_op_path();
        let probe = secrets::onepassword_probe(configured.as_deref(), verify);
        json!({
            "installed": probe.installed,
            "opPath": probe.op_path,
            "version": probe.version,
            "signedIn": probe.signed_in,
            "signatureOk": probe.signature_ok,
            "backend": crate::settings::get_secret_backend(),
            "account": crate::settings::get_onepassword_account(),
            "vault": crate::settings::get_onepassword_vault(),
            "verifySignature": verify,
        })
    })
    .await
    .map_err(|e| format!("1Password 状态探测任务失败: {e}"))
}

/// 列出账户（不需解锁）。
#[tauri::command]
pub async fn onepassword_list_accounts(_state: State<'_, AppState>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let verify = crate::settings::onepassword_verify_signature();
        let path = secrets::locate_op(crate::settings::get_onepassword_op_path().as_deref())
            .ok_or_else(|| crate::error::AppError::from(secrets::VaultError::NotInstalled))?;
        if verify {
            secrets::verify_op_signature(&path).map_err(crate::error::AppError::from)?;
        }
        let accounts = secrets::list_accounts(&path).map_err(crate::error::AppError::from)?;
        Ok::<Value, crate::error::AppError>(json!(accounts))
    })
    .await
    .map_err(|e| format!("列出 1Password 账户任务失败: {e}"))?
    .map_err(|e| e.to_string())
}

/// 列出 vault（需解锁：会触发授权弹窗）。`account` 传当前下拉选中值（尚未保存），
/// 缺失时回落已保存设置。
#[tauri::command]
pub async fn onepassword_list_vaults(
    _state: State<'_, AppState>,
    account: Option<String>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let verify = crate::settings::onepassword_verify_signature();
        let path = secrets::locate_op(crate::settings::get_onepassword_op_path().as_deref())
            .ok_or_else(|| crate::error::AppError::from(secrets::VaultError::NotInstalled))?;
        if verify {
            secrets::verify_op_signature(&path).map_err(crate::error::AppError::from)?;
        }
        let account = account
            .filter(|s| !s.trim().is_empty())
            .or_else(crate::settings::get_onepassword_account)
            .ok_or_else(|| {
                crate::error::AppError::from(secrets::VaultError::Other("请先选择账户".to_string()))
            })?;
        let vaults = secrets::list_vaults(&path, &account).map_err(crate::error::AppError::from)?;
        Ok::<Value, crate::error::AppError>(json!(vaults))
    })
    .await
    .map_err(|e| format!("列出 1Password vault 任务失败: {e}"))?
    .map_err(|e| e.to_string())
}

/// 保存 1Password 配置（account / vault / 是否校验签名）。同时把定位到的 op 绝对路径固定下来。
///
/// F2-3：已处于 1Password 后端时**拒绝修改 account / vault**——运行中的 vault 不跟随
/// 设置变化，换 vault 会让所有已迁移条目变成孤儿（§7 D11）。签名校验开关仍可改。
#[tauri::command]
pub async fn onepassword_save_config(
    _state: State<'_, AppState>,
    account: Option<String>,
    vault: Option<String>,
    #[allow(non_snake_case)] verifySignature: Option<bool>,
) -> Result<Value, String> {
    let mut settings = crate::settings::get_settings();
    let mut op = settings.onepassword.clone().unwrap_or_default();
    if crate::settings::is_onepassword_backend() {
        let changed_account = account
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_some_and(|a| op.account.as_deref() != Some(a));
        let changed_vault = vault
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_some_and(|v| op.vault.as_deref() != Some(v));
        if changed_account || changed_vault {
            return Err(crate::error::AppError::localized(
                "onepassword.account_vault_locked",
                "已切换到 1Password 后端，无法修改账户或保险箱（需整体重新迁移）",
                "Account/vault cannot be changed while the 1Password backend is active",
            )
            .to_string());
        }
    }
    if let Some(a) = account {
        op.account = Some(a).filter(|s| !s.trim().is_empty());
    }
    if let Some(v) = vault {
        op.vault = Some(v).filter(|s| !s.trim().is_empty());
    }
    if let Some(vs) = verifySignature {
        op.verify_signature = Some(vs);
    }
    // 固定 op 绝对路径，之后每次直接用它（防搜索路径劫持）。
    // F1-7：固定前先校验签名（信任根不能是被篡改的二进制），失败则不写入任何设置。
    if op.op_path.is_none() {
        if let Some(path) = secrets::locate_op(None) {
            let verify = op.verify_signature.unwrap_or(true);
            if verify {
                secrets::verify_op_signature(&path).map_err(|e| e.to_string())?;
            }
            op.op_path = Some(path.to_string_lossy().to_string());
        }
    }
    settings.onepassword = Some(op);
    crate::settings::update_settings(settings).map_err(|e| e.to_string())?;
    Ok(json!({ "success": true }))
}

/// 测试取钥匙：用当前配置构造 1Password 后端，触发一次需要解锁的调用（列 vault）。
/// 成功即证明「解锁 + 账户 + vault」链路可用；失败返回分类错误码供前端提示。
#[tauri::command]
pub async fn onepassword_test_fetch(_state: State<'_, AppState>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let verify = crate::settings::onepassword_verify_signature();
        let path = secrets::locate_op(crate::settings::get_onepassword_op_path().as_deref())
            .ok_or_else(|| crate::error::AppError::from(secrets::VaultError::NotInstalled))?;
        if verify {
            secrets::verify_op_signature(&path).map_err(crate::error::AppError::from)?;
        }
        let account = crate::settings::get_onepassword_account().ok_or_else(|| {
            crate::error::AppError::from(secrets::VaultError::Other("请先选择账户".to_string()))
        })?;
        // 列 vault 需要解锁，用作「测试取钥匙」的最小可用性验证。
        secrets::list_vaults(&path, &account).map_err(crate::error::AppError::from)?;
        Ok::<Value, crate::error::AppError>(json!({ "ok": true }))
    })
    .await
    .map_err(|e| format!("测试取钥匙任务失败: {e}"))?
    .map_err(|e| e.to_string())
}

/// 迁移向导（§7）：把凭据管理器里的 cc-switch 条目迁到 1Password，校验后删除并切后端。
/// 需先在设置里选好 account/vault。会触发解锁弹窗。
///
/// F2-1：已处于 1Password 后端时拒绝再次执行（迁移是单向的，残留清理走专用命令）；
/// 每完成一组上报 `onepassword-migrate-progress` 事件。
#[tauri::command]
pub async fn onepassword_migrate(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    if crate::settings::is_onepassword_backend() {
        return Err(crate::error::AppError::localized(
            "onepassword.migrate.already_migrated",
            "已处于 1Password 后端，无需再次迁移",
            "Already on the 1Password backend; no migration needed",
        )
        .to_string());
    }
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let vault = crate::secrets::onepassword_from_settings(state.db.clone())
            .map_err(crate::error::AppError::from)?;
        // F2-1：每组完成即向前端上报进度（约 7 秒/个）。
        let mut progress = |done: usize, total: usize| {
            let _ = app_handle.emit(
                "onepassword-migrate-progress",
                serde_json::json!({ "done": done, "total": total }),
            );
        };
        crate::secrets::migrate_to_onepassword(&state, &vault, &mut progress)
            .map(|report| json!(report))
    })
    .await
    .map_err(|e| format!("迁移任务失败: {e}"))?
    .map_err(|e| e.to_string())
}

/// F2-1：清理凭据管理器残留——迁移后 1Password 是唯一真源，本命令删除凭据管理器里
/// 剩余的 `cc-switch/*` 条目（不含探针）。仅 Windows；前端需先让用户确认。
#[tauri::command]
pub async fn onepassword_cleanup_credential_residue(
    _state: State<'_, AppState>,
) -> Result<Value, String> {
    #[cfg(not(target_os = "windows"))]
    {
        Err("仅支持 Windows".to_string())
    }
    #[cfg(target_os = "windows")]
    {
        tauri::async_runtime::spawn_blocking(move || {
            if !crate::settings::is_onepassword_backend() {
                return Err(crate::error::AppError::localized(
                    "onepassword.cleanup.not_1p",
                    "仅 1Password 模式下可清理凭据管理器残留",
                    "Credential residue cleanup is only available in 1Password mode",
                ));
            }
            let targets = crate::secrets::windows_enumerate_targets("cc-switch/")?;
            let mut deleted = 0usize;
            let mut failed = 0usize;
            for target in &targets {
                // 探针条目不属于用户数据，跳过。
                if target == "cc-switch/v1/probe" {
                    continue;
                }
                match crate::secrets::windows_delete_credential(target) {
                    Ok(()) => deleted += 1,
                    Err(e) => {
                        log::warn!("清理凭据管理器残留失败 {target}: {e}");
                        failed += 1;
                    }
                }
            }
            Ok::<_, crate::error::AppError>(
                serde_json::json!({ "deleted": deleted, "failed": failed }),
            )
        })
        .await
        .map_err(|e| format!("清理凭据管理器残留任务失败: {e}"))?
        .map_err(|e| e.to_string())
    }
}

/// F4-5（D14）：「从 1Password 重建引用」——云同步恢复 / 换设备后 `secret_refs`
/// 与真实条目可能脱节（引用不随云同步，本机保留）。枚举 vault 中带 `cc-switch`
/// 标签的条目（`op item list`，结构信息不含值），按标题解析归属组，再逐条
/// `op item get`（不带 `--reveal`，只取托管字段 label）重建 `secret_refs`。
/// 用户显式动作：N+1 次 op，逐个上报 `onepassword-rebuild-refs-progress` 事件。
///
/// S4-4 三处增强：
/// 1. `only`（`<app>/<id>` 列表）把 `op item get` 限制在目标供应商上——先用条目
///    标题在本地粗筛（`op item list` 的结果不花解锁），只对候选读详情。
/// 2. 同一次 `op item get` 顺带回填端点缓存，列表端点不再为空（P1-1）。
/// 3. 全量重建时清理「本机 vault 匹配、但 1P 里已不存在」的引用行（原先只 upsert）。
#[tauri::command]
pub async fn onepassword_rebuild_refs(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
    only: Option<Vec<String>>,
) -> Result<Value, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if !crate::settings::is_onepassword_backend() {
            return Err(crate::error::AppError::localized(
                "onepassword.rebuild_refs.not_1p",
                "仅 1Password 模式下可重建引用",
                "Ref rebuild is only available in 1Password mode",
            ));
        }
        let vault = crate::secrets::onepassword_from_settings(state.db.clone())
            .map_err(crate::error::AppError::from)?;
        let items = vault
            .list_tagged_items()
            .map_err(crate::error::AppError::from)?;

        // S4-4-1：给定 only 时按标题本地粗筛。方案 B 之后条目标题就是供应商显示名；
        // 旧格式条目标题自带归属，用标题精确解析。两类都不匹配的条目直接跳过，
        // 不花一次 op item get。
        let only_set: Option<std::collections::HashSet<String>> =
            only.map(|keys| keys.into_iter().collect());
        let target_titles: std::collections::HashSet<String> = only_set
            .iter()
            .flat_map(|set| set.iter())
            .filter_map(|key| {
                let (app_str, provider_id) = key.split_once('/')?;
                state
                    .db
                    .get_provider_by_id(provider_id, app_str)
                    .ok()
                    .flatten()
                    .map(|p| p.name)
            })
            .collect();
        let candidates: Vec<&crate::secrets::OpItemListEntry> = match &only_set {
            None => items.iter().collect(),
            Some(set) => items
                .iter()
                .filter(|item| {
                    crate::secrets::parse_group_from_title(&item.title)
                        .map(|g| {
                            let (app, provider) = g.ref_key();
                            set.contains(&format!("{app}/{provider}"))
                        })
                        .unwrap_or(false)
                        || target_titles.contains(&item.title)
                })
                .collect(),
        };
        let total = candidates.len();
        let mut rebuilt = 0usize;
        let mut linked: Vec<String> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        for (idx, item) in candidates.iter().enumerate() {
            let _ = app_handle.emit(
                "onepassword-rebuild-refs-progress",
                serde_json::json!({ "done": idx, "total": total }),
            );
            // 归属识别：优先读条目里的 cc-switch-group 字段（方案 B，标题只显示名）；
            // 旧格式条目没有该字段时回落到标题解析。两者都识别不出（用户手工建的
            // 同前缀条目、AppSync 之外的形态）跳过。
            // S4-4-2：同一次调用顺带取回非 CONCEALED 的 base_url（D3-B 起它在 1P
            // 里是可见 STRING 字段），零额外 op 调用。
            match vault.read_item_meta_and_endpoint(&item.id) {
                Ok((labels, group_field, endpoint)) => {
                    let group = group_field
                        .as_deref()
                        .and_then(crate::secrets::parse_group_from_group_value)
                        .or_else(|| crate::secrets::parse_group_from_title(&item.title));
                    let Some(group) = group else {
                        skipped.push(item.title.clone());
                        continue;
                    };
                    // S2（P0-1）：粗筛只认标题，这里必须按 cc-switch-group 核对归属，
                    // 否则同名供应商会互相登记对方条目。only 给定时尤其重要——
                    // 标题是粗筛依据，绝不能直接当成归属。
                    if let Some(set) = &only_set {
                        let (app, provider) = group.ref_key();
                        if !set.contains(&format!("{app}/{provider}")) {
                            skipped.push(item.title.clone());
                            continue;
                        }
                    }
                    let (app, provider) = group.ref_key();
                    state.db.upsert_secret_ref(
                        &app,
                        &provider,
                        &vault.vault_id(),
                        &item.id,
                        &labels,
                    )?;
                    if let Some(url) = endpoint {
                        // parse_item_meta 已挡掉带凭据的 URL（§9-7）。
                        state.db.upsert_provider_endpoint(&app, &provider, &url)?;
                    }
                    linked.push(format!("{app}/{provider}"));
                    rebuilt += 1;
                }
                Err(e) => {
                    log::warn!("重建引用失败（{}）: {e}", item.title);
                    skipped.push(item.title.clone());
                }
            }
        }
        let _ = app_handle.emit(
            "onepassword-rebuild-refs-progress",
            serde_json::json!({ "done": total, "total": total }),
        );

        // S4-4-3（P1-1）：全量重建时清理失效引用行。只删本机 vault 匹配的行——
        // 别的保险箱写下的引用本机无权处置。
        let mut pruned = 0usize;
        if only_set.is_none() {
            let live_ids: std::collections::HashSet<&str> =
                items.iter().map(|i| i.id.as_str()).collect();
            let vault_id = vault.vault_id();
            for (app, provider_id, ref_vault, item_id) in state.db.list_secret_ref_identities()? {
                if ref_vault == vault_id && !live_ids.contains(item_id.as_str()) {
                    state.db.delete_secret_ref(&app, &provider_id)?;
                    pruned += 1;
                }
            }
        }

        // S4-3：关联成功的供应商从「未关联」清单出队。
        if !linked.is_empty() {
            let remaining: Vec<String> = crate::settings::get_onepassword_unlinked()
                .into_iter()
                .filter(|key| !linked.contains(key))
                .collect();
            crate::settings::set_onepassword_unlinked(remaining)?;
        }

        // S1-2：重建引用本就是用户主动触发的 op 动作，顺带把 Pi 端点改动
        // merge 进 1Password；失败只记日志（本次重建结果不受影响）。
        if let Err(error) = crate::services::provider::flush_endpoint_vault_pending(&state) {
            log::warn!("重建引用后写回 Pi 端点改动失败: {error}");
        }
        Ok::<_, crate::error::AppError>(serde_json::json!({
            "total": total,
            "rebuilt": rebuilt,
            "pruned": pruned,
            "skipped": skipped,
        }))
    })
    .await
    .map_err(|e| format!("重建引用任务失败: {e}"))?
    .map_err(|e| e.to_string())
}

/// 轻量查询当前凭据后端标识（不调 op）。前端据此置灰/隐藏相关区块。
#[tauri::command]
pub async fn secret_backend_name(_state: State<'_, AppState>) -> Result<String, String> {
    Ok(crate::settings::get_secret_backend())
}

/// S4-5（§5.4 S4-5）：端点对账（只读）的核心比对。
///
/// vault 交互（`list_tagged_items` / `fetch`）抽成注入的闭包，生产实现走真实
/// `op`，测试注入内存 vault——审计的判定逻辑不依赖能否装上 1Password。
/// **只返回 app/id 和状态词，绝不返回 URL 本身**（§9-13）。
pub(crate) fn audit_endpoints_with(
    state: &AppState,
    progress: &mut dyn FnMut(usize, usize),
    list_live_item_ids: impl FnOnce()
        -> Result<std::collections::HashSet<String>, crate::error::AppError>,
    fetch_vault_url: &dyn Fn(&crate::app_config::AppType, &str) -> Option<String>,
) -> Result<Value, crate::error::AppError> {
    use crate::app_config::AppType;
    use std::str::FromStr;

    let refs = state.db.list_secret_ref_identities()?;
    let total = refs.len();
    let live_item_ids = list_live_item_ids()?;

    let mut matched = Vec::new();
    let mut mismatched = Vec::new();
    let mut vault_missing = Vec::new();
    let mut cache_missing = Vec::new();
    let mut orphan_refs = Vec::new();

    for (idx, (app_str, provider_id, _vault_id, item_id)) in refs.iter().enumerate() {
        progress(idx, total);
        let key = format!("{app_str}/{provider_id}");
        if state
            .db
            .get_provider_by_id(provider_id, app_str)
            .map(|r| r.is_none())
            .unwrap_or(true)
        {
            orphan_refs.push(key.clone());
            continue;
        }
        if !live_item_ids.contains(item_id) {
            vault_missing.push(key.clone());
            continue;
        }
        let Ok(app) = AppType::from_str(app_str) else {
            continue;
        };
        let vault_url = fetch_vault_url(&app, provider_id);
        let cached = state
            .db
            .get_provider_endpoint(app_str, provider_id)
            .unwrap_or(None);
        match (vault_url.as_deref(), cached.as_deref()) {
            (None, None) => matched.push(key),
            (Some(v), Some(c)) if v == c => matched.push(key),
            (Some(_), None) => cache_missing.push(key),
            // vault 没有 base_url 但缓存有：条目被手工改过，或用旧客户端写的。
            (None, Some(_)) | (Some(_), Some(_)) => mismatched.push(key),
        }
    }
    progress(total, total);

    Ok(json!({
        "vaultConfigured": true,
        "checked": total,
        "matched": matched.len(),
        "mismatched": mismatched,
        "vaultMissing": vault_missing,
        "cacheMissing": cache_missing,
        "orphanRefs": orphan_refs,
    }))
}

/// S4-5：端点对账（只读）。
///
/// 对每个有引用的供应商取一次 1P 整包，比较 vault 与本机缓存的 base_url。
/// 逐个上报 `onepassword-endpoint-audit-progress`。
///
/// 三重校验：
/// 1. **vault 配置可用性**——2026-09-27 出现过 `settings.onepassword.vault` 被写成
///    坏值（不是可用 vault）的情况，那时所有取钥匙都失败而界面毫无提示；
///    这里直接把它报成 `vaultConfigured: false`（§8.2 建议增补项）。
/// 2. **item 是否还在 1P**——用 `list_tagged_items` 一次拿全量 id 集合比对，
///    比逐条 fetch 快一个数量级。
/// 3. **端点一致性**——只在 item 确实存在时才 fetch，避免为死引用白付解锁。
pub(crate) fn audit_endpoints(
    state: &AppState,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<Value, crate::error::AppError> {
    use std::collections::HashSet;

    let configured_vault = crate::settings::get_onepassword_vault();
    if configured_vault
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        return Ok(json!({
            "vaultConfigured": false,
            "checked": 0,
            "matched": 0,
            "mismatched": [],
            "vaultMissing": [],
            "cacheMissing": [],
            "orphanRefs": [],
        }));
    }
    // 1P 模式下构造 vault 失败 = 配置坏了（未装 op / 签名不信任 / vault 值损坏）。
    let vault = crate::secrets::onepassword_from_settings(state.db.clone())
        .map_err(crate::error::AppError::from)?;

    audit_endpoints_with(
        state,
        progress,
        || {
            // 1 次 op 拿到全部条目 id，用于校验引用有效性（不含值、不弹解锁）。
            vault
                .list_tagged_items()
                .map(|items| {
                    items
                        .into_iter()
                        .filter(|i| !i.id.is_empty())
                        .map(|i| i.id)
                        .collect::<HashSet<String>>()
                })
                .map_err(crate::error::AppError::from)
        },
        &|app, provider_id| {
            state
                .vault
                .fetch(&crate::secrets::SecretGroup::provider(
                    app.clone(),
                    provider_id.to_string(),
                ))
                .ok()
                .flatten()
                .and_then(|bundle| {
                    bundle
                        .get(crate::secrets::FIELD_BASE_URL)
                        .map(|v| v.to_string())
                })
        },
    )
}

/// S4-3：读「未关联 1Password 的供应商」清单（`<app>/<id>` 形式）。
///
/// 0 次 op——只查本机 settings（清单由 `run_post_import_sync` 在每次导入后全量
/// 重建）。前端横幅据此显示「N 个供应商来自其他设备」并提供「从 1Password 关联」。
#[tauri::command]
pub async fn onepassword_unlinked_status(_state: State<'_, AppState>) -> Result<Value, String> {
    if !crate::settings::is_onepassword_backend() {
        return Ok(json!({ "unlinked": [] }));
    }
    Ok(json!({ "unlinked": crate::settings::get_onepassword_unlinked() }))
}

/// S4-5：端点对账（只读诊断）。会触发 `op`（可能弹解锁），故 async + spawn_blocking。
#[tauri::command]
pub async fn onepassword_endpoint_audit(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if !crate::settings::is_onepassword_backend() {
            return Err(crate::error::AppError::localized(
                "onepassword.endpoint_audit.not_1p",
                "仅 1Password 模式下可做端点对账",
                "Endpoint audit is only available in 1Password mode",
            ));
        }
        let mut progress = |done: usize, total: usize| {
            let _ = app_handle.emit(
                "onepassword-endpoint-audit-progress",
                json!({ "done": done, "total": total }),
            );
        };
        audit_endpoints(&state, &mut progress)
    })
    .await
    .map_err(|e| format!("端点对账任务失败: {e}"))?
    .map_err(|e| e.to_string())
}

/// S4-5：按对账结果收敛端点。`prefer` 取 `vault`（以 1Password 为准，校正本机缓存）
/// 或 `cache`（以本机缓存为准，写回 1Password）。对 `mismatched` / `cacheMissing`
/// 里的每个 `app/id` 执行一次，两种模式都只用整包 merge 语义，不动 api_key。
#[tauri::command]
pub async fn onepassword_endpoint_reconcile(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
    prefer: String,
) -> Result<Value, String> {
    use crate::app_config::AppType;
    use std::str::FromStr;

    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if !crate::settings::is_onepassword_backend() {
            return Err(crate::error::AppError::localized(
                "onepassword.endpoint_reconcile.not_1p",
                "仅 1Password 模式下可收敛端点",
                "Endpoint reconcile is only available in 1Password mode",
            ));
        }
        let report = audit_endpoints(&state, &mut |_, _| {})?;
        let mut targets: Vec<String> = Vec::new();
        for list in ["mismatched", "cacheMissing"] {
            if let Some(items) = report.get(list).and_then(Value::as_array) {
                targets.extend(items.iter().filter_map(Value::as_str).map(str::to_string));
            }
        }
        let prefer_cache = match prefer.as_str() {
            "vault" => false,
            "cache" => true,
            other => {
                return Err(crate::error::AppError::Message(format!(
                    "未知的收敛方向: {other}"
                )));
            }
        };
        let total = targets.len();
        let mut applied = 0usize;
        let mut failed: Vec<String> = Vec::new();
        for (idx, key) in targets.iter().enumerate() {
            let _ = app_handle.emit(
                "onepassword-endpoint-audit-progress",
                json!({ "done": idx, "total": total }),
            );
            let Some((app_str, provider_id)) = key.split_once('/') else {
                failed.push(key.clone());
                continue;
            };
            let Ok(app) = AppType::from_str(app_str) else {
                failed.push(key.clone());
                continue;
            };
            let result = if prefer_cache {
                // 以本机缓存为准写回 1P：只有缓存里有非敏感 URL 才写得动。
                match state.db.get_provider_endpoint(app_str, provider_id)? {
                    Some(url) if !crate::secrets::is_credential_bearing_url(&url) => {
                        let secrets = crate::secrets::ProviderSecrets {
                            base_url: Some(zeroize::Zeroizing::new(url)),
                            ..Default::default()
                        };
                        crate::services::provider::store_provider_bundle(
                            &state,
                            &app,
                            provider_id,
                            &secrets,
                            true,
                        )
                    }
                    // 敏感 URL 不落端点表（§9-7），缓存里不可能有，这里只能跳过。
                    _ => Ok(()),
                }
            } else {
                // S4-5：以 1Password 为准做**强制对齐**——这里不能用
                // `fetch_provider_secrets`：它在 vault 没有端点时会回落到本机缓存
                // （读取容错，S4-2），而收敛的语义是「真源说没有就没有」，所以
                // vault 缺端点时必须删掉本机缓存，否则这类分歧永远消不掉。
                let group =
                    crate::secrets::SecretGroup::provider(app.clone(), provider_id.to_string());
                match state.vault.fetch(&group) {
                    Ok(Some(bundle)) => {
                        let vault_url = bundle
                            .get(crate::secrets::FIELD_BASE_URL)
                            .map(|v| v.to_string());
                        match vault_url {
                            // 敏感 URL 绝不落端点表（§9-7）：清缓存。
                            Some(ref url) if crate::secrets::is_credential_bearing_url(url) => {
                                state.db.delete_provider_endpoint(app_str, provider_id)
                            }
                            Some(url) => {
                                let cached =
                                    state.db.get_provider_endpoint(app_str, provider_id)?;
                                if cached.as_deref() != Some(url.as_str()) {
                                    state
                                        .db
                                        .upsert_provider_endpoint(app_str, provider_id, &url)
                                } else {
                                    Ok(())
                                }
                            }
                            // 1P 真源没有端点 → 本机缓存是陈旧残留，删。
                            None => state.db.delete_provider_endpoint(app_str, provider_id),
                        }
                    }
                    // 条目已不在 1P：缓存无据可依，删（引用行留给用户处置）。
                    Ok(None) => state.db.delete_provider_endpoint(app_str, provider_id),
                    Err(e) => Err(crate::error::AppError::from(e)),
                }
            };
            match result {
                Ok(()) => applied += 1,
                Err(e) => {
                    log::warn!("端点收敛失败 {key}: {e}");
                    failed.push(key.clone());
                }
            }
        }
        let _ = app_handle.emit(
            "onepassword-endpoint-audit-progress",
            json!({ "done": total, "total": total }),
        );
        Ok::<_, crate::error::AppError>(json!({
            "applied": applied,
            "failed": failed,
            "direction": if prefer_cache { "cache" } else { "vault" },
        }))
    })
    .await
    .map_err(|e| format!("端点收敛任务失败: {e}"))?
    .map_err(|e| e.to_string())
}

/// F1-2：端点回填待办数量（0 次 op，只查本地表）。
/// 大于 0 时前端在 1Password 区 / 主界面横幅提示「回填端点」。
#[tauri::command]
pub async fn secrets_endpoint_backfill_status(state: State<'_, AppState>) -> Result<Value, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let pending = state.db.list_endpoint_backfill_pending()?;
        Ok::<_, crate::error::AppError>(json!({ "pending": pending.len() }))
    })
    .await
    .map_err(|e| format!("端点回填状态任务失败: {e}"))?
    .map_err(|e| e.to_string())
}

/// F1-2：存量端点回填——把 vault 里托管的非敏感 base_url 逐个搬到端点表。
/// 每组 1 次 fetch（1P 模式会请求解锁，约 7 秒/个），完成后读取全部 0 次 op。
/// 逐个上报 `secrets-backfill-progress` 事件；单项失败不中断，记入 failed 清单。
#[tauri::command]
pub async fn secrets_backfill_endpoints(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    use crate::app_config::AppType;
    use std::str::FromStr;

    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let pending = state.db.list_endpoint_backfill_pending()?;
        let total = pending.len();
        let mut backfilled = 0usize;
        let mut failed: Vec<String> = Vec::new();
        for (app_str, id) in pending.iter() {
            let done = backfilled + failed.len();
            let _ = app_handle.emit(
                "secrets-backfill-progress",
                serde_json::json!({ "done": done, "total": total }),
            );
            let Ok(app) = AppType::from_str(app_str) else {
                failed.push(format!("{app_str}/{id}"));
                continue;
            };
            match crate::services::provider::resolve_base_url(&state, &app, id) {
                Ok(Some(_)) => backfilled += 1,
                Ok(None) => {
                    // vault 里没有该条目的 base_url（可能条目已删）：无从回填，记为失败
                    // 让用户感知；refs 行保持原样。
                    failed.push(format!("{app_str}/{id}"));
                }
                Err(e) => {
                    log::warn!("端点回填失败 {app_str}/{id}: {e}");
                    failed.push(format!("{app_str}/{id}"));
                }
            }
        }
        let _ = app_handle.emit(
            "secrets-backfill-progress",
            serde_json::json!({ "done": total, "total": total }),
        );
        Ok::<_, crate::error::AppError>(serde_json::json!({
            "total": total,
            "backfilled": backfilled,
            "failed": failed,
        }))
    })
    .await
    .map_err(|e| format!("端点回填任务失败: {e}"))?
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod endpoint_audit_tests {
    //! S4-5（§5.4 S4-5）：端点对账的判定逻辑。vault 交互注入内存实现，
    //! 覆盖「一致 / 不一致 / 1P 缺失 / 缓存缺失 / 孤儿引用」五种结论。
    use super::audit_endpoints_with;
    use crate::app_config::AppType;
    use crate::database::Database;
    use crate::secrets::{InMemorySecretStore, SecretStore};
    use crate::store::AppState;
    use serde_json::Value;
    use serial_test::serial;
    use std::collections::HashSet;
    use std::sync::Arc;

    fn state() -> AppState {
        let store: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::new());
        let db = Arc::new(Database::memory().expect("memory db"));
        AppState::new(db, store)
    }

    fn seed(state: &AppState, id: &str) {
        let provider = crate::provider::Provider::from_parts(
            id.to_string(),
            id.to_string(),
            serde_json::json!({"env": {"ANTHROPIC_MODEL": "claude-opus-4"}}),
            None,
        );
        state
            .db
            .save_provider(AppType::Claude.as_str(), &provider)
            .expect("seed provider");
    }

    fn ref_row(state: &AppState, id: &str, item_id: &str) {
        state
            .db
            .upsert_secret_ref("claude", id, "vault-1", item_id, &["api_key".to_string()])
            .expect("seed ref");
    }

    fn list_of(items: Vec<String>) -> HashSet<String> {
        items.into_iter().collect()
    }

    #[test]
    #[serial]
    fn audit_classifies_every_endpoint_relation() {
        let _home = crate::test_support::TestHomeGuard::new();
        let state = state();

        // 一致
        seed(&state, "same");
        ref_row(&state, "same", "item-same");
        state
            .db
            .upsert_provider_endpoint("claude", "same", "https://same.example")
            .expect("cache");
        // 不一致（两端都有值但不同）
        seed(&state, "diff");
        ref_row(&state, "diff", "item-diff");
        state
            .db
            .upsert_provider_endpoint("claude", "diff", "https://cache.example")
            .expect("cache");
        // 缓存缺失（vault 有、缓存无）
        seed(&state, "nocache");
        ref_row(&state, "nocache", "item-nocache");
        // 1P 缺失（引用指向的 item 不在 1P）
        seed(&state, "gone");
        ref_row(&state, "gone", "item-gone");
        // 孤儿引用（供应商已不存在）
        ref_row(&state, "deleted", "item-deleted");

        let report = audit_endpoints_with(
            &state,
            &mut |_, _| {},
            || {
                Ok(list_of(vec![
                    "item-same".into(),
                    "item-diff".into(),
                    "item-nocache".into(),
                ]))
            },
            &|app: &AppType, id: &str| match (app, id) {
                (AppType::Claude, "same") => Some("https://same.example".to_string()),
                (AppType::Claude, "diff") => Some("https://vault.example".to_string()),
                (AppType::Claude, "nocache") => Some("https://nocache.example".to_string()),
                _ => None,
            },
        )
        .expect("audit");

        let list = |k: &str| -> Vec<String> {
            report
                .get(k)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        assert_eq!(report.get("checked").and_then(Value::as_u64), Some(5));
        assert_eq!(report.get("matched").and_then(Value::as_u64), Some(1));
        assert_eq!(list("mismatched"), vec!["claude/diff"], "两端有值但不同");
        assert_eq!(
            list("cacheMissing"),
            vec!["claude/nocache"],
            "vault 有缓存无"
        );
        assert_eq!(
            list("vaultMissing"),
            vec!["claude/gone"],
            "引用指向已删条目"
        );
        assert_eq!(list("orphanRefs"), vec!["claude/deleted"], "供应商已不存在");
        // 绝不把 URL 本身回传（§9-13）。
        let rendered = report.to_string();
        assert!(!rendered.contains("https://"), "报告不得含 URL: {rendered}");
    }

    /// S4-5：item 已不在 1P 时不得再 fetch 整包——为死引用白付一次解锁没有必要。
    #[test]
    #[serial]
    fn audit_skips_fetch_for_items_missing_in_1p() {
        let _home = crate::test_support::TestHomeGuard::new();
        let state = state();
        seed(&state, "gone");
        ref_row(&state, "gone", "item-gone");

        let fetched = std::cell::Cell::new(0usize);
        let report = audit_endpoints_with(
            &state,
            &mut |_, _| {},
            || Ok(list_of(vec![])),
            &|_app: &AppType, _id: &str| {
                fetched.set(fetched.get() + 1);
                Some("https://never.example".to_string())
            },
        )
        .expect("audit");

        assert_eq!(fetched.get(), 0, "item 不在 1P 时不得 fetch");
        assert_eq!(
            report
                .get("vaultMissing")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );
    }

    /// 进度回调走完全部条目（前端进度条据此收尾）。
    #[test]
    #[serial]
    fn audit_reports_progress_for_every_ref() {
        let _home = crate::test_support::TestHomeGuard::new();
        let state = state();
        for id in ["a", "b", "c"] {
            seed(&state, id);
            ref_row(&state, id, &format!("item-{id}"));
        }

        let mut ticks: Vec<(usize, usize)> = Vec::new();
        audit_endpoints_with(
            &state,
            &mut |done, total| ticks.push((done, total)),
            || {
                Ok(list_of(vec![
                    "item-a".into(),
                    "item-b".into(),
                    "item-c".into(),
                ]))
            },
            &|_app: &AppType, _id: &str| None,
        )
        .expect("audit");

        assert_eq!(ticks.first().copied(), Some((0, 3)));
        assert_eq!(ticks.last().copied(), Some((3, 3)), "末尾必须补齐收尾进度");
    }
}

#[cfg(test)]
mod rebuild_refs_scope_tests {
    //! S4-4-1：给定的 `only` 必须把 `op item get` 限制在目标供应商上。
    //!
    //! `op item list` 的结果只带 id / title / updatedAt（不花解锁），所以粗筛只能
    //! 在本地按标题做：方案 B 的条目标题 = 供应商显示名，旧格式条目标题自带归属。
    //! 粗筛之后仍必须按 `cc-switch-group` 核对归属——标题会撞名（S2 / P0-1）。
    use crate::secrets::OpItemListEntry;

    fn item(id: &str, title: &str) -> OpItemListEntry {
        OpItemListEntry {
            id: id.to_string(),
            title: title.to_string(),
            updated_at: String::new(),
        }
    }

    /// 与 `onepassword_rebuild_refs` 里的粗筛保持一致——单点定义另见
    /// `rebuild_candidates`，这里只验证判据本身。
    fn in_scope(
        item: &OpItemListEntry,
        only_set: &std::collections::HashSet<String>,
        target_titles: &std::collections::HashSet<String>,
    ) -> bool {
        crate::secrets::parse_group_from_title(&item.title)
            .map(|g| {
                let (app, provider) = g.ref_key();
                only_set.contains(&format!("{app}/{provider}"))
            })
            .unwrap_or(false)
            || target_titles.contains(&item.title)
    }

    #[test]
    fn scope_keeps_legacy_title_matches() {
        let set: std::collections::HashSet<String> =
            ["claude/old-style".to_string()].into_iter().collect();
        let titles = std::collections::HashSet::new();
        assert!(in_scope(
            &item("i1", "cc-switch/claude/old-style"),
            &set,
            &titles
        ));
        assert!(!in_scope(
            &item("i2", "cc-switch/claude/other"),
            &set,
            &titles
        ));
    }

    #[test]
    fn scope_keeps_display_name_matches() {
        let set: std::collections::HashSet<String> =
            ["pi/pi-one".to_string()].into_iter().collect();
        let titles: std::collections::HashSet<String> =
            ["Pi One".to_string()].into_iter().collect();
        assert!(in_scope(&item("i1", "Pi One"), &set, &titles));
        assert!(!in_scope(&item("i2", "Pi Two"), &set, &titles));
    }

    /// 同名条目（Claude 与 Pi 都叫「OpenRouter」）：粗筛会把两条都留下，归属核对
    /// 交给 read_item_meta 之后的 group 比对——这里锁住「粗筛不得假装能区分」。
    #[test]
    fn scope_cannot_distinguish_same_title_across_apps() {
        let set: std::collections::HashSet<String> = ["pi/or".to_string()].into_iter().collect();
        let titles: std::collections::HashSet<String> =
            ["OpenRouter".to_string()].into_iter().collect();
        assert!(
            in_scope(&item("claude-item", "OpenRouter"), &set, &titles)
                && in_scope(&item("pi-item", "OpenRouter"), &set, &titles),
            "同名条目两条都进候选，由 cc-switch-group 核对区分"
        );
    }
}
