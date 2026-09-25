//! 凭据便携包：把 Windows 凭据管理器里的 `cc-switch/*` 条目加密导出成一个文件，
//! 换机时导入回凭据管理器。
//!
//! 为什么需要它：凭据**不参与** WebDAV/S3 同步（`docs/user-manual/zh/1-getting-started/1.5-settings.md`
//! 「同步不包含密钥」），而卸载清理会删掉本机全部 `cc-switch/*` 条目。两者相加
//! 就是「卸载 + 重装 + 同步 → 密钥永久丢失」。同步的威胁模型不允许放明文密钥，
//! 所以迁移能力走这条独立路径：用户显式操作、独立口令、密文落盘。
//!
//! 密码学与 `services::sync_e2e` 同源（Argon2id 派生 KEK → XChaCha20-Poly1305），
//! 但格式独立：便携包不参与同步的序号/回滚语义，`format` 也另起一个，避免两个
//! 载荷互相冒充。
//!
//! 明文生命周期：凭据值全程由 `Zeroizing<String>` 承载，序列化出的 JSON 缓冲在
//! 写入后立即清零（`zeroize::Zeroize`），避免在堆上留不受控的副本。

use std::collections::BTreeMap;

use zeroize::{Zeroize, Zeroizing};

use crate::error::AppError;
use crate::secrets::SecretStore;
use crate::services::sync_e2e::{
    aead_open, aead_seal, derive_kek, new_kdf_params, random_bytes, validate_kdf_params, KdfParams,
    NONCE_LEN,
};

/// 便携包格式标识（与同步 v3 的 `cc-switch-sync-e2e` 刻意不同）。
const PORTABLE_FORMAT: &str = "cc-switch-portable-secrets";
const PORTABLE_VERSION: u32 = 1;
/// 只导出本前缀下的条目（`cc-switch/v1/...`）。
const SECRET_PREFIX: &str = "cc-switch/";
/// 口令最短长度。用户用 1Password 生成随机口令，20 字符是下限而非建议值。
pub(crate) const MIN_PASSPHRASE_CHARS: usize = 20;
/// 便携包文件大小上限：纯文本凭据，1 MiB 足够，防止导入超大文件当解压炸弹。
const MAX_BUNDLE_BYTES: usize = 1024 * 1024;

/// 口令强度校验（`validate` 与导入共用）。
///
/// 只查长度：便携包的口令由用户自选，做强字典比对既不完整又容易误伤。20 字符
/// 的随机口令熵已远超暴力破解成本，长度是这里唯一有意义的门槛。
fn validate_passphrase(passphrase: &str) -> Result<(), AppError> {
    let len = passphrase.chars().count();
    if len < MIN_PASSPHRASE_CHARS {
        return Err(AppError::localized(
            "secrets.portable.passphrase_too_short",
            format!("口令至少需要 {MIN_PASSPHRASE_CHARS} 个字符（当前 {len}）"),
            format!(
                "Passphrase must be at least {MIN_PASSPHRASE_CHARS} characters (currently {len})"
            ),
        ));
    }
    Ok(())
}

/// 线上结构（JSON，密文 base64）。
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PortableBundle {
    format: String,
    version: u32,
    /// 导出时的应用版本，仅供排查，导入端不做校验。
    app_version: String,
    kdf: KdfParams,
    #[serde(with = "base64_24")]
    nonce: [u8; NONCE_LEN],
    #[serde(with = "base64_vec")]
    ciphertext: Vec<u8>,
}

/// 加密前的明文载荷：`target 名 → 值`。
///
/// 用 `BTreeMap<String, Zeroizing<String>>`：值在 drop 时自动清零；`target` 名
/// 本身不是秘密（形如 `cc-switch/v1/provider/pi/jm/api_key`），无需包裹。
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PortablePayload {
    /// 导出时 CC Switch 的 DB 里存在的供应商，供导入端提示"多出来的条目"。
    /// 只放 id 与展示名，不含任何凭据。
    #[serde(default)]
    providers: Vec<PortableProvider>,
    #[serde(with = "zeroizing_map")]
    secrets: BTreeMap<String, Zeroizing<String>>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PortableProvider {
    app: String,
    id: String,
    name: String,
}

/// 导出结果统计（不含任何凭据值）。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportReport {
    /// 写入便携包的条目数。
    pub exported: usize,
    /// 应用级条目（`cc-switch/v1/app/*`，如 webdav 密码）数量。
    pub app_secrets: usize,
}

/// 导入结果统计（不含任何凭据值）。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    /// 新写入的条目数（本地原本没有）。
    pub imported: usize,
    /// 被云端值覆盖的条目数（本地已有且值不同）。
    pub overwritten: usize,
    /// 本地已有且值相同、跳过的条目数。
    pub unchanged: usize,
    /// 应用级条目数量。
    pub app_secrets: usize,
}

/// 导出：枚举本机 `cc-switch/*` 凭据，加密成便携包字节。
///
/// 返回 `(字节, 统计)`。口令不足 20 字符直接拒绝，不产生文件。
pub async fn export(
    store: &dyn SecretStore,
    passphrase: &str,
    app_version: &str,
) -> Result<(Vec<u8>, ExportReport), AppError> {
    validate_passphrase(passphrase)?;

    let targets = store.list_targets(SECRET_PREFIX).await?;
    let mut secrets: BTreeMap<String, Zeroizing<String>> = BTreeMap::new();
    let mut report = ExportReport::default();

    for target in targets {
        // 探针条目（`cc-switch/v1/probe`）是启动自检的临时残留，不导出。
        if target == "cc-switch/v1/probe" {
            continue;
        }
        let Some(value) = store.get_target_raw(&target).await? else {
            continue;
        };
        if target.starts_with("cc-switch/v1/app/") {
            report.app_secrets += 1;
        }
        secrets.insert(target, value);
    }
    report.exported = secrets.len();

    if secrets.is_empty() {
        return Err(AppError::localized(
            "secrets.portable.nothing_to_export",
            "本机没有可导出的 CC Switch 凭据",
            "There are no CC Switch credentials on this machine to export",
        ));
    }

    let payload = PortablePayload {
        providers: Vec::new(),
        secrets,
    };

    let bytes = seal_payload(&payload, passphrase, app_version)?;
    Ok((bytes, report))
}

/// 导入：解密便携包，把条目写回凭据管理器。
///
/// 冲突策略（用户已确认）：**云端优先 + 本地独有保留**——包里有的条目一律以包里
/// 的值为准（值不同则覆盖），本地有而包里没有的条目原样保留（不删除）。
pub async fn import(
    store: &dyn SecretStore,
    bundle_bytes: &[u8],
    passphrase: &str,
) -> Result<ImportReport, AppError> {
    validate_passphrase(passphrase)?;
    if bundle_bytes.len() > MAX_BUNDLE_BYTES {
        return Err(AppError::localized(
            "secrets.portable.bundle_too_large",
            "便携包超出大小上限",
            "Portable bundle exceeds the size limit",
        ));
    }

    // 反序列化后 payload 内的值由 Zeroizing 承载；走到函数末尾自动清零。
    let payload = open_payload(bundle_bytes, passphrase)?;
    let mut report = ImportReport::default();

    for (target, value) in &payload.secrets {
        if target.starts_with("cc-switch/v1/app/") {
            report.app_secrets += 1;
        }
        let existing = store.get_target_raw(target).await?;
        match existing {
            Some(current) if current.as_str() == value.as_str() => report.unchanged += 1,
            Some(_) => {
                store.set_target_raw(target, value).await?;
                report.overwritten += 1;
            }
            None => {
                store.set_target_raw(target, value).await?;
                report.imported += 1;
            }
        }
    }

    Ok(report)
}

/// 加密载荷。序列化缓冲用完立即清零（`Zeroize`），不在堆上留明文副本。
fn seal_payload(
    payload: &PortablePayload,
    passphrase: &str,
    app_version: &str,
) -> Result<Vec<u8>, AppError> {
    let mut plaintext =
        serde_json::to_vec(payload).map_err(|e| AppError::JsonSerialize { source: e })?;

    let kdf = new_kdf_params();
    let kek = derive_kek(passphrase, &kdf)?;
    let nonce: [u8; NONCE_LEN] = random_bytes();

    // AAD 绑定格式与版本：换格式/版本号的载荷无法复用同一段密文。
    let aad = format!("{PORTABLE_FORMAT}\0{PORTABLE_VERSION}").into_bytes();
    let sealed = aead_seal(kek.as_bytes(), &nonce, &aad, &plaintext);

    // 无论加密成败都清零明文缓冲。
    plaintext.zeroize();
    let ciphertext = sealed?;

    let bundle = PortableBundle {
        format: PORTABLE_FORMAT.to_string(),
        version: PORTABLE_VERSION,
        app_version: app_version.to_string(),
        kdf,
        nonce,
        ciphertext,
    };

    let bytes =
        serde_json::to_vec_pretty(&bundle).map_err(|e| AppError::JsonSerialize { source: e })?;
    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err(AppError::localized(
            "secrets.portable.bundle_too_large",
            "导出的便携包超出大小上限",
            "Exported portable bundle exceeds the size limit",
        ));
    }
    Ok(bytes)
}

/// 解密载荷。口令错、被篡改、格式不符都在这里失败。
fn open_payload(bundle_bytes: &[u8], passphrase: &str) -> Result<PortablePayload, AppError> {
    let bundle: PortableBundle = serde_json::from_slice(bundle_bytes).map_err(|_| {
        AppError::localized(
            "secrets.portable.bundle_invalid",
            "便携包格式无法识别",
            "Portable bundle format is unrecognized",
        )
    })?;

    if bundle.format != PORTABLE_FORMAT || bundle.version != PORTABLE_VERSION {
        return Err(AppError::localized(
            "secrets.portable.bundle_invalid",
            "便携包格式无法识别",
            "Portable bundle format is unrecognized",
        ));
    }
    validate_kdf_params(&bundle.kdf)?;

    let kek = derive_kek(passphrase, &bundle.kdf)?;
    let aad = format!("{PORTABLE_FORMAT}\0{PORTABLE_VERSION}").into_bytes();
    // 口令错与密文被改都落在同一个 AEAD 失败上，此处刻意不区分（同 sync_e2e）。
    let mut plaintext = aead_open(kek.as_bytes(), &bundle.nonce, &aad, &bundle.ciphertext)?;

    let parsed = serde_json::from_slice::<PortablePayload>(&plaintext);
    // 反序列化会复制出被 Zeroizing 包裹的值，原始缓冲用完立即清零。
    plaintext.zeroize();

    parsed.map_err(|_| {
        AppError::localized(
            "secrets.portable.payload_invalid",
            "便携包内容无法解析",
            "Portable bundle content could not be parsed",
        )
    })
}

// ─── base64 serde 辅助 ───────────────────────────────────────

/// `Zeroizing<String>` 没有 serde 实现（`zeroize` 默认不带 serde feature）。
/// 这里手写一个 map 模块：序列化时直接写内部字符串，反序列化时逐个包裹成
/// `Zeroizing`，保证解密出的明文在 drop 时自动清零。
mod zeroizing_map {
    use std::collections::BTreeMap;

    use serde::de::{Deserialize, Deserializer};
    use serde::ser::{Serialize, Serializer};
    use zeroize::Zeroizing;

    pub(super) fn serialize<S: Serializer>(
        map: &BTreeMap<String, Zeroizing<String>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let plain: BTreeMap<&str, &str> =
            map.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        plain.serialize(s)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<String, Zeroizing<String>>, D::Error> {
        let raw = BTreeMap::<String, String>::deserialize(d)?;
        Ok(raw
            .into_iter()
            .map(|(k, v)| (k, Zeroizing::new(v)))
            .collect())
    }
}

mod base64_vec {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(v: &Vec<u8>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(v))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(serde::de::Error::custom)
    }
}

mod base64_24 {
    use super::NONCE_LEN;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(v: &[u8; NONCE_LEN], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(v))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<[u8; NONCE_LEN], D::Error> {
        let s = String::deserialize(d)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(serde::de::Error::custom)?;
        <[u8; NONCE_LEN]>::try_from(bytes.as_slice()).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::InMemorySecretStore;

    const GOOD: &str = "correct-horse-battery-staple-20";

    async fn store_with_secrets() -> InMemorySecretStore {
        let store = InMemorySecretStore::new();
        store
            .set_target_raw("cc-switch/v1/provider/pi/jm/api_key", "sk-pi-jm-literal")
            .await
            .unwrap();
        store
            .set_target_raw(
                "cc-switch/v1/provider/pi/jm/base_url",
                "https://a.example/v1",
            )
            .await
            .unwrap();
        store
            .set_target_raw("cc-switch/v1/provider/claude/p1/api_key", "sk-claude-1")
            .await
            .unwrap();
        store
            .set_target_raw("cc-switch/v1/app/webdav/password", "dav-pass")
            .await
            .unwrap();
        store
    }

    #[test]
    fn short_passphrase_is_rejected() {
        let err = validate_passphrase("short").unwrap_err();
        assert!(err.to_string().contains("20"), "应提示最低长度: {err}");
        assert!(validate_passphrase(GOOD).is_ok());
        // 边界：恰好 20 个字符通过。
        assert!(validate_passphrase(&"x".repeat(MIN_PASSPHRASE_CHARS)).is_ok());
        assert!(validate_passphrase(&"x".repeat(MIN_PASSPHRASE_CHARS - 1)).is_err());
    }

    #[tokio::test]
    async fn export_import_roundtrip_restores_every_entry() {
        let source = store_with_secrets().await;
        let (bytes, report) = export(&source, GOOD, "2.2.9").await.expect("export");
        assert_eq!(report.exported, 4);
        assert_eq!(report.app_secrets, 1);

        let dest = InMemorySecretStore::new();
        let imported = import(&dest, &bytes, GOOD).await.expect("import");
        assert_eq!(imported.imported, 4, "空目标应全部新写入");
        assert_eq!(imported.overwritten, 0);
        assert_eq!(imported.app_secrets, 1);

        for target in [
            "cc-switch/v1/provider/pi/jm/api_key",
            "cc-switch/v1/provider/pi/jm/base_url",
            "cc-switch/v1/provider/claude/p1/api_key",
            "cc-switch/v1/app/webdav/password",
        ] {
            assert!(
                dest.get_target_raw(target).await.unwrap().is_some(),
                "{target} 应已恢复"
            );
        }
    }

    #[tokio::test]
    async fn overlay_is_remote_first_and_keeps_local_only_entries() {
        let source = store_with_secrets().await;
        let (bytes, _) = export(&source, GOOD, "2.2.9").await.expect("export");

        let dest = InMemorySecretStore::new();
        // 本地已有、值与包里不同 → 应被覆盖。
        dest.set_target_raw("cc-switch/v1/provider/pi/jm/api_key", "sk-stale")
            .await
            .unwrap();
        // 本地已有、值与包里相同 → 不动。
        dest.set_target_raw("cc-switch/v1/app/webdav/password", "dav-pass")
            .await
            .unwrap();
        // 本地独有、包里没有 → 必须保留。
        dest.set_target_raw("cc-switch/v1/provider/pi/local-only/api_key", "keep-me")
            .await
            .unwrap();

        let report = import(&dest, &bytes, GOOD).await.expect("import");

        assert_eq!(report.overwritten, 1, "仅 jm/api_key 被覆盖");
        assert_eq!(report.unchanged, 1, "webdav 密码值相同");
        assert_eq!(report.imported, 2, "jm/base_url 与 claude/p1 是新写入");
        assert_eq!(
            dest.get_target_raw("cc-switch/v1/provider/pi/jm/api_key")
                .await
                .unwrap()
                .unwrap()
                .as_str(),
            "sk-pi-jm-literal",
            "云端值应覆盖本地旧值"
        );
        assert_eq!(
            dest.get_target_raw("cc-switch/v1/provider/pi/local-only/api_key")
                .await
                .unwrap()
                .unwrap()
                .as_str(),
            "keep-me",
            "本地独有条目不得被删除"
        );
    }

    #[tokio::test]
    async fn wrong_passphrase_fails_without_writing_anything() {
        let source = store_with_secrets().await;
        let (bytes, _) = export(&source, GOOD, "2.2.9").await.expect("export");

        let dest = InMemorySecretStore::new();
        let err = import(&dest, &bytes, "wrong-horse-battery-staple-20")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("口令") || err.to_string().contains("篡改"),
            "应报口令错或篡改: {err}"
        );
        assert!(
            dest.dump().is_empty(),
            "解密失败不得写入任何条目: {:?}",
            dest.dump()
        );
    }

    #[tokio::test]
    async fn tampered_ciphertext_is_rejected() {
        let source = store_with_secrets().await;
        let (bytes, _) = export(&source, GOOD, "2.2.9").await.expect("export");

        let mut bundle: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let ct = bundle["ciphertext"].as_str().unwrap().to_string();
        let mut flipped = ct.into_bytes();
        // 改一个 base64 字符，破坏密文。
        flipped[0] = if flipped[0] == b'A' { b'B' } else { b'A' };
        bundle["ciphertext"] = serde_json::Value::String(String::from_utf8(flipped).unwrap());
        let tampered = serde_json::to_vec(&bundle).unwrap();

        let dest = InMemorySecretStore::new();
        assert!(import(&dest, &tampered, GOOD).await.is_err());
        assert!(dest.dump().is_empty());
    }

    #[tokio::test]
    async fn foreign_format_is_rejected() {
        // 同步 v3 的 manifest 不能冒充便携包。
        let fake = br#"{"format":"cc-switch-sync-e2e","version":3}"#;
        let dest = InMemorySecretStore::new();
        assert!(import(&dest, fake, GOOD).await.is_err());
    }

    #[tokio::test]
    async fn oversized_bundle_is_rejected_before_decrypt() {
        let dest = InMemorySecretStore::new();
        let huge = vec![b' '; MAX_BUNDLE_BYTES + 1];
        assert!(import(&dest, &huge, GOOD).await.is_err());
    }

    #[tokio::test]
    async fn export_with_empty_store_reports_error() {
        let empty = InMemorySecretStore::new();
        assert!(export(&empty, GOOD, "2.2.9").await.is_err());
    }

    #[tokio::test]
    async fn probe_entry_is_not_exported() {
        let store = store_with_secrets().await;
        store
            .set_target_raw("cc-switch/v1/probe", "cc-switch-probe")
            .await
            .unwrap();

        let (bytes, report) = export(&store, GOOD, "2.2.9").await.expect("export");
        assert_eq!(report.exported, 4, "探针条目不应计入");

        let payload = open_payload(&bytes, GOOD).expect("open");
        assert!(
            !payload.secrets.contains_key("cc-switch/v1/probe"),
            "探针条目不得进便携包"
        );
    }

    #[tokio::test]
    async fn plaintext_never_appears_in_bundle() {
        let source = store_with_secrets().await;
        let (bytes, _) = export(&source, GOOD, "2.2.9").await.expect("export");
        let text = String::from_utf8_lossy(&bytes);

        for secret in [
            "sk-pi-jm-literal",
            "https://a.example/v1",
            "sk-claude-1",
            "dav-pass",
        ] {
            assert!(!text.contains(secret), "便携包中不得出现明文凭据: {secret}");
        }
        // 口令本身也不得落盘。
        assert!(!text.contains(GOOD), "便携包中不得出现口令");
    }
}
