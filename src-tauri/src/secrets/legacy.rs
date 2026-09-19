//! 历史凭据条目名的找回。
//!
//! 早期开发版把条目存成 `<user>.<规范名>`（keyring 的限定名写法），当前版本按
//! 附录 B 原样存 `cc-switch/v1/...`。只装过中间开发版的机器上，规范名下查不到，
//! 界面就会误报「需要密钥」而切换被拒——密钥其实好好躺在凭据管理器里。
//!
//! 这里做一次性的搬迁：规范名有值 → 补登记并清掉旧名副本；规范名空而旧名有值 →
//! 按规范名重写、删掉旧名、补登记。旧名里的 `env/<VAR>` 无法枚举（keyring 不提供
//! 枚举，名册也没记过），那部分只能由用户重填，故不在本模块范围内。

use super::{load_known_targets, save_known_targets, SecretStore, SecretTarget};
use crate::app_config::AppType;
use crate::database::Database;
use crate::error::AppError;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct LegacyRecovery {
    /// 从旧名搬到规范名的条数
    pub moved: usize,
    /// 规范名已存在、顺手删掉的旧名副本数
    pub pruned: usize,
    /// 补进 `known_secret_targets` 的条数
    pub registered: usize,
}

fn register_target(targets: &mut Vec<String>, out: &mut LegacyRecovery, name: &str) {
    if !targets.iter().any(|t| t == name) {
        targets.push(name.to_string());
        out.registered += 1;
    }
}

pub async fn recover_legacy_named_secrets(
    db: &Database,
    store: &dyn SecretStore,
) -> Result<LegacyRecovery, AppError> {
    let mut out = LegacyRecovery::default();
    let mut targets = load_known_targets(db)?;

    for app in [AppType::Claude, AppType::Codex, AppType::Pi] {
        let ids: Vec<String> = db
            .get_all_providers(app.as_str())?
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        for id in ids {
            for plain in [
                SecretTarget::provider_api_key(app.clone(), id.clone()),
                SecretTarget::provider_base_url(app.clone(), id.clone()),
            ] {
                let legacy = SecretTarget::legacy(plain.clone());
                let name = plain.to_target_string();

                match store.get(&plain).await? {
                    Some(_) => {
                        register_target(&mut targets, &mut out, &name);
                        // 同一份凭据可能新旧两名并存（开发版之间来回升级过），
                        // 规范名既然能用，旧名就是纯残留。
                        if store.get(&legacy).await?.is_some() {
                            store.delete(&legacy).await?;
                            out.pruned += 1;
                        }
                    }
                    None => {
                        if let Some(value) = store.get(&legacy).await? {
                            store.set(&plain, value).await?;
                            store.delete(&legacy).await?;
                            register_target(&mut targets, &mut out, &name);
                            out.moved += 1;
                        }
                    }
                }
            }
        }
    }

    if out.registered > 0 {
        save_known_targets(db, &targets)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::InMemorySecretStore;
    use zeroize::Zeroizing;

    async fn seed(store: &dyn SecretStore, target: &SecretTarget, value: &str) {
        store
            .set(target, Zeroizing::new(value.to_string()))
            .await
            .expect("seed");
    }

    fn claude_target() -> SecretTarget {
        SecretTarget::provider_api_key(AppType::Claude, "p1")
    }

    /// 找回按 DB 里的供应商行逐个探测，所以测试里得先有这一行。
    fn seed_provider(db: &Database) {
        let provider = crate::provider::Provider::from_parts(
            "p1".to_string(),
            "P1".to_string(),
            serde_json::json!({ "env": {} }),
            None,
        );
        db.save_provider("claude", &provider).expect("存供应商");
    }

    #[tokio::test]
    async fn moves_a_legacy_named_credential_onto_the_canonical_name() {
        let db = Database::memory().expect("内存库");
        seed_provider(&db);
        let store = InMemorySecretStore::new();
        seed(&store, &SecretTarget::legacy(claude_target()), "sk-legacy").await;

        let out = recover_legacy_named_secrets(&db, &store)
            .await
            .expect("找回");
        assert_eq!(out.moved, 1, "应搬一条: {out:?}");

        let value = store
            .get(&claude_target())
            .await
            .expect("读")
            .expect("规范名应有值");
        assert_eq!(value.as_str(), "sk-legacy");
        assert!(
            store
                .get(&SecretTarget::legacy(claude_target()))
                .await
                .expect("读旧名")
                .is_none(),
            "旧名条目必须删除，避免留下第二份凭据副本"
        );
        let registry = load_known_targets(&db).expect("名册");
        assert!(
            registry.contains(&claude_target().to_target_string()),
            "搬完必须补登记，否则界面还是报「需要密钥」: {registry:?}"
        );
    }

    #[tokio::test]
    async fn prunes_the_legacy_copy_when_the_canonical_name_already_works() {
        let db = Database::memory().expect("内存库");
        seed_provider(&db);
        let store = InMemorySecretStore::new();
        seed(&store, &claude_target(), "sk-new").await;
        seed(&store, &SecretTarget::legacy(claude_target()), "sk-old").await;

        let out = recover_legacy_named_secrets(&db, &store)
            .await
            .expect("找回");
        assert_eq!(out.pruned, 1);
        assert_eq!(out.moved, 0);
        assert_eq!(
            store
                .get(&claude_target())
                .await
                .expect("读")
                .expect("规范名保留")
                .as_str(),
            "sk-new"
        );
    }

    #[tokio::test]
    async fn does_nothing_for_providers_without_any_entry() {
        let db = Database::memory().expect("内存库");
        let store = InMemorySecretStore::new();
        let out = recover_legacy_named_secrets(&db, &store)
            .await
            .expect("找回");
        assert_eq!(out.moved, 0);
        assert_eq!(out.registered, 0);
        assert!(load_known_targets(&db).expect("名册").is_empty());
    }
}
