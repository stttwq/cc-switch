//! 从凭据管理器一次性迁移到 1Password（施工方案 §7）。
//!
//! 由设置页「迁移到 1Password」向导触发。原则（§3 / §7）：
//! - 先写后删：只有在 1Password 写入并回读校验一致后，才删凭据管理器条目。
//! - 可中断、可重跑：已迁移的组（secret_refs 有行且 1P 条目存在）跳过。
//! - 失败任何一步不删凭据管理器数据，停在当前组并报分类错误。
//! - 成功后切 `secret_backend=onepassword` 并清注册表投递（§6.8）。

use std::collections::BTreeMap;

use crate::app_config::AppType;
use crate::error::AppError;
use crate::secrets::vault::{app_sync_bundle_field, SecretBundle, SecretGroup, SecretVault};
use crate::store::AppState;

#[derive(Debug, Default, serde::Serialize)]
pub struct MigrationReport {
    /// 成功迁移的组数。
    pub migrated_groups: usize,
    /// 成功迁移的字段总数。
    pub migrated_fields: usize,
    /// 已删除的凭据管理器条目数。
    pub deleted_targets: usize,
    /// 不阻断的告警（只含字段名/分类，不含值）。
    pub warnings: Vec<String>,
}

/// 一个待迁移组：目标组 + 字段名→凭据管理器 target 名。
struct PendingGroup {
    group: SecretGroup,
    /// bundle 字段名 → 凭据管理器 target 字符串（用于读值与迁移后删除）。
    fields: BTreeMap<String, String>,
}

/// 盘点凭据管理器里的 `cc-switch/*` 目标，按组归拢（§7 步骤 2）。
fn inventory(state: &AppState) -> Result<BTreeMap<String, PendingGroup>, AppError> {
    // 枚举 + known_secret_targets 合并去重（枚举拿全，名册补漏）。
    #[cfg(target_os = "windows")]
    let mut targets: Vec<String> = crate::secrets::windows_enumerate_targets("cc-switch/")?;
    #[cfg(not(target_os = "windows"))]
    let mut targets: Vec<String> = Vec::new();
    for t in crate::secrets::load_known_targets(state.db.as_ref())? {
        if !targets.iter().any(|x| x == &t) {
            targets.push(t);
        }
    }

    let mut grouped: BTreeMap<String, PendingGroup> = BTreeMap::new();
    for target in targets {
        // probe 丢弃。
        if target == "cc-switch/v1/probe" {
            continue;
        }
        if let Some(rest) = target.strip_prefix("cc-switch/v1/provider/") {
            // <app>/<id>/<field...>
            let mut parts = rest.splitn(3, '/');
            let (Some(app_str), Some(id), Some(field_raw)) =
                (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let Ok(app) = app_str.parse::<AppType>() else {
                continue;
            };
            let field = if let Some(var) = field_raw.strip_prefix("env/") {
                if var.is_empty() {
                    continue;
                }
                format!("env.{var}")
            } else if field_raw == "api_key" || field_raw == "base_url" {
                field_raw.to_string()
            } else {
                continue;
            };
            let group = SecretGroup::provider(app, id.to_string());
            grouped
                .entry(group.key())
                .or_insert_with(|| PendingGroup {
                    group,
                    fields: BTreeMap::new(),
                })
                .fields
                .insert(field, target);
        } else if let Some(rest) = target.strip_prefix("cc-switch/v1/app/") {
            // <sub>/<field>
            let mut parts = rest.splitn(2, '/');
            let (Some(sub), Some(field)) = (parts.next(), parts.next()) else {
                continue;
            };
            let Some(bundle_field) = app_sync_bundle_field(sub, field) else {
                continue;
            };
            let group = SecretGroup::AppSync;
            grouped
                .entry(group.key())
                .or_insert_with(|| PendingGroup {
                    group,
                    fields: BTreeMap::new(),
                })
                .fields
                .insert(bundle_field.to_string(), target);
        }
        // 其它形态（含 legacy `<user>.<name>`）不在标准迁移内；legacy 找回是单独一条路径。
    }
    Ok(grouped)
}

fn ref_keys(group: &SecretGroup) -> (String, String) {
    group.ref_key()
}

/// F2-1：「已迁移」的本地标记判定——`secret_refs` 行的 `vault_id` 等于目标 vault，
/// 且 `item_id` 是真实的 1P id（非空、不是 `provider/<app>/<id>` / `app/sync` 这类
/// v20 回填与旧后端写入的占位形式，§12.9）。纯本地查询，0 次 op：首次迁移不再
/// 为「判断已迁移」白白 fetch 一次（P1-1）。
fn is_already_migrated(
    state: &AppState,
    group: &SecretGroup,
    target_vault_id: &str,
) -> Result<bool, AppError> {
    let (ref_app, ref_provider) = ref_keys(group);
    let already = state
        .db
        .get_secret_ref_identity(&ref_app, &ref_provider)?
        .map(|(vault_id, item_id)| {
            vault_id == target_vault_id && !item_id.is_empty() && item_id != group.key()
        })
        .unwrap_or(false);
    Ok(already)
}

/// 执行迁移（§7）。`vault` 是目标 1Password 后端；`state.secrets` 是凭据管理器迁移源。
/// `progress(done, total)` 在每处理完一组后回调（前端进度条用）。
pub fn migrate_to_onepassword(
    state: &AppState,
    vault: &dyn SecretVault,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<MigrationReport, AppError> {
    let groups = inventory(state)?;
    let total = groups.len();
    let mut report = MigrationReport::default();

    // 收集本轮真正写入的凭据管理器 target，全部写+校验通过后再统一删除（先写后删，§7 步骤 7）。
    let mut written_targets: Vec<String> = Vec::new();
    let mut done = 0usize;

    for pending in groups.values() {
        let (ref_app, ref_provider) = ref_keys(&pending.group);

        // F2-1：可重跑判定改用本地标记（0 次 op）。
        if is_already_migrated(state, &pending.group, &vault.vault_id())? {
            // 已迁移：仍需把这组的凭据管理器 target 纳入删除清单（迁移完成的收尾）。
            written_targets.extend(pending.fields.values().cloned());
            done += 1;
            progress(done, total);
            continue;
        }

        // 读出（本地凭据管理器，快）。
        let mut bundle = SecretBundle::new();
        for (field, target_name) in &pending.fields {
            let value = futures::executor::block_on(state.secrets.get_target_raw(target_name))?;
            let Some(value) = value else {
                report
                    .warnings
                    .push(format!("missing_value:{ref_app}/{ref_provider}/{field}"));
                continue;
            };
            bundle.insert(field.clone(), value);
        }
        if bundle.is_empty() {
            report
                .warnings
                .push(format!("empty_group:{ref_app}/{ref_provider}"));
            continue;
        }

        // F1-2（§7 D3-A / F2-1）：非敏感 base_url 直接写本地端点表，**不进 1P**——
        // Codex / Pi 本来就把 URL 明文写进各自的配置文件，放 vault 挡不住文件泄露，
        // 只会让每次切换多 7 秒并弹解锁。敏感 URL（userinfo / 敏感 query 参数）仍走 1P。
        let mut vault_bundle = SecretBundle::new();
        for (field, value) in bundle.iter() {
            if field == crate::secrets::FIELD_BASE_URL
                && !crate::secrets::is_credential_bearing_url(value.as_str())
            {
                if let SecretGroup::Provider { app, provider_id } = &pending.group {
                    state
                        .db
                        .upsert_provider_endpoint(app.as_str(), provider_id, value.as_str())?;
                    report.migrated_fields += 1;
                }
            } else {
                vault_bundle.insert(field.clone(), value.clone());
            }
        }

        // 拆分后 vault 侧没有要写的字段（例如这组只有 base_url）→ 跳过写 1P，
        // 也不建 refs（端点表才是 base_url 的读取来源）。
        if vault_bundle.is_empty() {
            report.migrated_groups += 1;
            written_targets.extend(pending.fields.values().cloned());
            done += 1;
            progress(done, total);
            continue;
        }

        // 写入 1Password（F2-2：put 内部按 id 直达 / 标题兜底 / 同名冲突取最新；
        // create 阶段撞同名时仍会返回 ItemConflict——见下方停止逻辑）。
        let vref = match vault.put(&pending.group, &vault_bundle) {
            Ok(vref) => vref,
            // F2-1：同名条目冲突 → 迁移停在当前组，列出组键交给用户处理，
            // **不自动归档**、不删除任何凭据管理器数据（可修复后重跑）。
            Err(crate::secrets::VaultError::ItemConflict) => {
                return Err(AppError::localized(
                    "onepassword.migrate.item_conflict",
                    format!(
                        "迁移在 {ref_app}/{ref_provider} 停止：1Password 中已存在同名条目，请在 1Password 中重命名或删除重复条目后重试（凭据管理器数据未改动）"
                    ),
                    format!(
                        "Migration stopped at {ref_app}/{ref_provider}: a conflicting item already exists in 1Password; resolve it and retry (credential manager data untouched)"
                    ),
                ));
            }
            Err(e) => return Err(e.into()),
        };

        // 校验：回读逐字段比对。
        let fetched = vault
            .fetch(&pending.group)?
            .ok_or_else(|| AppError::Message("迁移校验失败：写入后回读不到条目".to_string()))?;
        for (field, value) in vault_bundle.iter() {
            let ok = fetched.get(field).map(|v| v.as_str()) == Some(value.as_str());
            if !ok {
                return Err(AppError::localized(
                    "onepassword.migrate.verify_failed",
                    format!("迁移校验失败：{ref_app}/{ref_provider} 的 {field} 回读不一致"),
                    format!("Migration verification failed for {ref_app}/{ref_provider}/{field}"),
                ));
            }
        }

        // 提交引用（§4.3）。
        state.db.upsert_secret_ref(
            &ref_app,
            &ref_provider,
            &vref.vault_id,
            &vref.item_id,
            &vref.fields,
        )?;
        report.migrated_groups += 1;
        report.migrated_fields += vault_bundle.len();
        written_targets.extend(pending.fields.values().cloned());
        done += 1;
        // F2-1：每组完成即上报进度（前端显示「第 k/N 个」）。
        progress(done, total);
    }

    // 全部写入并校验通过后：切后端 + 记时间（§7 步骤 6）。
    // F2-1：改用 mutate_settings——避免「迁移开始时取快照、几分钟整份写回」
    // 覆盖迁移期间用户的其它设置改动（P1-1）。
    crate::settings::mutate_settings(|settings| {
        settings.secret_backend = Some("onepassword".to_string());
        let mut op = settings.onepassword.clone().unwrap_or_default();
        op.migrated_at = Some(chrono::Utc::now().to_rfc3339());
        settings.onepassword = Some(op);
    })?;
    // F1-6：后端已切换，运行中的旧凭据管理器后端立即退役（P0-6）。
    // 迁移后续的凭据管理器删除走 Win32 直调，不经旧 vault，不受影响。
    crate::secrets::vault::retire_legacy_vault();

    // 切到 1Password 后必须重写一次 live（剥掉 Codex auth.json / Claude settings.json 里的
    // 明文钥匙）。置标志，重启时（新进程用 OnePasswordVault）执行 reapply。
    let _ = state.db.set_setting("live_reapply_pending", "1");

    // 删除凭据管理器条目（§7 步骤 7）。best-effort：删失败只告警，不回滚（1P 已是真源）。
    #[cfg(target_os = "windows")]
    for target in &written_targets {
        match crate::secrets::windows_delete_credential(target) {
            Ok(()) => report.deleted_targets += 1,
            Err(e) => {
                log::warn!("迁移后删除凭据管理器条目失败: {e}");
                report.warnings.push("delete_failed".to_string());
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    let _ = &written_targets;

    // 清注册表历史投递（§6.8）——用户诉求 1 的一部分。
    if let Err(e) = crate::services::provider::ProviderService::purge_all_env_delivery(state) {
        log::warn!("迁移后清理注册表投递失败: {e}");
        report.warnings.push("purge_env_failed".to_string());
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{InMemorySecretStore, InMemoryVault, SecretStore, SecretTarget};
    use serial_test::serial;
    use std::sync::Arc;

    /// 测试卫生：迁移会切全局设置并退役旧 vault（进程级），离开测试前复位，
    /// 避免污染并行的其它测试。
    struct GlobalStateGuard;
    impl Drop for GlobalStateGuard {
        fn drop(&mut self) {
            crate::secrets::vault::reset_legacy_vault_retirement_for_tests();
            let mut settings = crate::settings::get_settings();
            if settings.secret_backend.as_deref() == Some("onepassword") {
                settings.secret_backend = None;
                settings.onepassword = None;
                let _ = crate::settings::update_settings(settings);
            }
        }
    }

    fn state_with_seed() -> (AppState, Vec<String>) {
        let db = Arc::new(crate::database::Database::memory().expect("db"));
        let store: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::new());
        // 播种凭据管理器：一个 Claude 供应商 + 一个 AppSync 口令。
        futures::executor::block_on(store.store(
            &SecretTarget::provider_api_key(AppType::Claude, "p1"),
            "sk-1",
        ))
        .unwrap();
        futures::executor::block_on(store.store(
            &SecretTarget::provider_base_url(AppType::Claude, "p1"),
            "https://x",
        ))
        .unwrap();
        futures::executor::block_on(store.store(&SecretTarget::app("sync", "passphrase"), "pass"))
            .unwrap();
        let state = AppState::new(db, store);
        // 登记 known_secret_targets（枚举在非 Windows 下拿不到，靠名册补）。
        let targets = vec![
            SecretTarget::provider_api_key(AppType::Claude, "p1").to_target_string(),
            SecretTarget::provider_base_url(AppType::Claude, "p1").to_target_string(),
        ];
        crate::secrets::save_known_targets(state.db.as_ref(), &targets).unwrap();
        (state, targets)
    }

    #[test]
    #[serial]
    fn migrate_writes_verifies_and_registers_refs() {
        let _guard = GlobalStateGuard;
        let (state, _targets) = state_with_seed();
        let vault = InMemoryVault::new();
        let mut progress_seen: Vec<(usize, usize)> = Vec::new();
        let report = migrate_to_onepassword(&state, &vault, &mut |done, total| {
            progress_seen.push((done, total));
        })
        .expect("migrate");

        // provider 组迁移成功（AppSync 在非 Windows 下没进 known_targets，不强求）。
        assert!(report.migrated_groups >= 1);
        // F2-1：每组完成都上报进度，最后一帧 done == total。
        assert!(!progress_seen.is_empty(), "应上报迁移进度");
        let &(last_done, last_total) = progress_seen.last().unwrap();
        assert_eq!(last_done, last_total, "最后一帧进度应为 done == total");
        // 1P 里能读回（F1-2：非敏感 base_url 不进 1P，只落端点表）。
        let got = vault
            .fetch(&SecretGroup::provider(AppType::Claude, "p1"))
            .unwrap()
            .unwrap();
        assert_eq!(
            got.get("api_key").map(|v| v.to_string()),
            Some("sk-1".into())
        );
        assert!(
            got.get("base_url").is_none(),
            "非敏感 base_url 不应进 vault"
        );
        // base_url 已写端点表。
        assert_eq!(
            state.db.get_provider_endpoint("claude", "p1").unwrap(),
            Some("https://x".to_string())
        );
        // secret_refs 已登记（vault_id / item_id 为真实 1P 形态）。
        let (vault_id, item_id) = state
            .db
            .get_secret_ref_identity("claude", "p1")
            .unwrap()
            .unwrap();
        assert_eq!(vault_id, "in-memory");
        assert!(
            item_id.starts_with("mem-"),
            "item_id 应是真实形态而非占位: {item_id}"
        );
        let fields = state
            .db
            .get_secret_ref_fields("claude", "p1")
            .unwrap()
            .unwrap();
        assert!(fields.contains(&"api_key".to_string()));
        // 后端已切换。
        assert_eq!(crate::settings::get_secret_backend(), "onepassword");
    }

    #[test]
    #[serial]
    fn migrate_is_rerunnable() {
        let _guard = GlobalStateGuard;
        let (state, _t) = state_with_seed();
        let vault = InMemoryVault::new();
        migrate_to_onepassword(&state, &vault, &mut |_, _| {}).expect("first");
        // 第二遍：已迁移组跳过，不报错，且**不发起任何 vault 往返**（F2-1：本地标记判定）。
        let counting = crate::secrets::CountingVault::new(Arc::new(vault));
        let report2 = migrate_to_onepassword(&state, &counting, &mut |_, _| {}).expect("second");
        assert_eq!(report2.migrated_groups, 0, "已迁移组应跳过");
        assert_eq!(counting.fetch_count(), 0, "重跑判定不得触发 fetch（P1-1）");
        assert_eq!(counting.put_count(), 0);
    }

    /// F2-1 验收：写入侧冲突（ItemConflict）时迁移停止，凭据管理器源数据完好，
    /// 后端未切换，已迁移组的引用保留。
    #[test]
    #[serial]
    fn migrate_stops_on_item_conflict_without_touching_source() {
        let _guard = GlobalStateGuard;
        let (state, targets) = state_with_seed();
        struct ConflictingVault;
        impl SecretVault for ConflictingVault {
            fn fetch(
                &self,
                _group: &SecretGroup,
            ) -> Result<Option<SecretBundle>, crate::secrets::VaultError> {
                Ok(None)
            }
            fn put(
                &self,
                _group: &SecretGroup,
                _bundle: &SecretBundle,
            ) -> Result<crate::secrets::VaultRef, crate::secrets::VaultError> {
                Err(crate::secrets::VaultError::ItemConflict)
            }
            fn delete(&self, _group: &SecretGroup) -> Result<(), crate::secrets::VaultError> {
                Ok(())
            }
            fn status(&self) -> crate::secrets::VaultStatus {
                crate::secrets::VaultStatus::Ready
            }
            fn vault_id(&self) -> String {
                "conflict".to_string()
            }
            fn backend_name(&self) -> &'static str {
                "conflicting"
            }
        }
        let err = migrate_to_onepassword(&state, &ConflictingVault, &mut |_, _| {})
            .expect_err("冲突应让迁移失败");
        assert!(
            err.to_string().contains("同名条目"),
            "错误应提示同名条目冲突: {err}"
        );
        // 凭据管理器源数据完好（api_key 还能读到）。
        let key = futures::executor::block_on(state.secrets.get_target_raw(
            &SecretTarget::provider_api_key(AppType::Claude, "p1").to_target_string(),
        ))
        .unwrap();
        assert_eq!(key.map(|v| v.to_string()), Some("sk-1".into()));
        // 后端未切换。
        assert_ne!(crate::settings::get_secret_backend(), "onepassword");
        // 名册未被改动。
        assert_eq!(
            crate::secrets::load_known_targets(state.db.as_ref())
                .unwrap()
                .len(),
            targets.len()
        );
    }
}
