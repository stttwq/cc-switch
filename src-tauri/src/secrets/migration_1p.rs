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

/// AppSync 组在 secret_refs 里的键（§4.3）。
const APP_SYNC_REF_APP: &str = "_app";
const APP_SYNC_REF_PROVIDER: &str = "_sync";

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
    match group {
        SecretGroup::Provider { app, provider_id } => {
            (app.as_str().to_string(), provider_id.clone())
        }
        SecretGroup::AppSync => (
            APP_SYNC_REF_APP.to_string(),
            APP_SYNC_REF_PROVIDER.to_string(),
        ),
    }
}

/// 执行迁移（§7）。`vault` 是目标 1Password 后端；`state.secrets` 是凭据管理器迁移源。
pub fn migrate_to_onepassword(
    state: &AppState,
    vault: &dyn SecretVault,
) -> Result<MigrationReport, AppError> {
    let groups = inventory(state)?;
    let mut report = MigrationReport::default();

    // 收集本轮真正写入的凭据管理器 target，全部写+校验通过后再统一删除（先写后删，§7 步骤 7）。
    let mut written_targets: Vec<String> = Vec::new();

    for pending in groups.values() {
        let (ref_app, ref_provider) = ref_keys(&pending.group);

        // 可重跑：secret_refs 有行且 1P 条目存在 => 已迁移，跳过。
        let already = state
            .db
            .get_secret_ref_fields(&ref_app, &ref_provider)?
            .is_some()
            && vault.fetch(&pending.group)?.is_some();
        if already {
            // 仍需把这组的凭据管理器 target 纳入删除清单（迁移完成的收尾）。
            written_targets.extend(pending.fields.values().cloned());
            continue;
        }

        // 读出（本地凭据管理器，快）。
        let mut bundle = SecretBundle::new();
        for (field, target_name) in &pending.fields {
            let value =
                futures::executor::block_on(state.secrets.get_target_raw(target_name))?;
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

        // 写入 1Password（vault_item_conflict 时 put 内部按标题复用覆盖）。
        let vref = vault.put(&pending.group, &bundle)?;

        // 校验：回读逐字段比对。
        let fetched = vault
            .fetch(&pending.group)?
            .ok_or_else(|| AppError::Message("迁移校验失败：写入后回读不到条目".to_string()))?;
        for (field, value) in bundle.iter() {
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
        report.migrated_fields += bundle.len();
        written_targets.extend(pending.fields.values().cloned());
    }

    // 全部写入并校验通过后：切后端 + 记时间（§7 步骤 6）。
    let mut settings = crate::settings::get_settings();
    settings.secret_backend = Some("onepassword".to_string());
    let mut op = settings.onepassword.clone().unwrap_or_default();
    op.migrated_at = Some(chrono::Utc::now().to_rfc3339());
    settings.onepassword = Some(op);
    crate::settings::update_settings(settings)?;

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
    use std::sync::Arc;

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
    fn migrate_writes_verifies_and_registers_refs() {
        let (state, _targets) = state_with_seed();
        let vault = InMemoryVault::new();
        let report = migrate_to_onepassword(&state, &vault).expect("migrate");

        // provider 组迁移成功（AppSync 在非 Windows 下没进 known_targets，不强求）。
        assert!(report.migrated_groups >= 1);
        // 1P 里能读回。
        let got = vault
            .fetch(&SecretGroup::provider(AppType::Claude, "p1"))
            .unwrap()
            .unwrap();
        assert_eq!(got.get("api_key").map(|v| v.to_string()), Some("sk-1".into()));
        assert_eq!(
            got.get("base_url").map(|v| v.to_string()),
            Some("https://x".into())
        );
        // secret_refs 已登记。
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
    fn migrate_is_rerunnable() {
        let (state, _t) = state_with_seed();
        let vault = InMemoryVault::new();
        migrate_to_onepassword(&state, &vault).expect("first");
        // 第二遍：已迁移组跳过，不报错。
        let report2 = migrate_to_onepassword(&state, &vault).expect("second");
        assert_eq!(report2.migrated_groups, 0, "已迁移组应跳过");
    }
}
