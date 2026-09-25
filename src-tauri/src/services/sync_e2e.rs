//! 同步载荷的端到端加密（2.1 方案第 2 部分，P3）。
//!
//! 口令 →(Argon2id)→ KEK →(AEAD 封装)→ DEK →(XChaCha20-Poly1305)→ 密文 blob。
//! WebDAV / S3 服务器只见 v3 布局下的密文与一个不含隐私信息的外层 manifest；
//! 篡改、换拼（A 快照的 blob 配 B 快照的 manifest）、回滚（seq 倒退）都在此拒绝。
//!
//! 本模块只做密码学与线上格式，不碰网络与磁盘；与传输层的接线在 E2E-3 完成。

use std::collections::BTreeMap;

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::KeyInit;
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand_core::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::AppError;

use super::sync_protocol::{sha256_hex, REMOTE_DB_SQL, REMOTE_MANIFEST, REMOTE_SKILLS_ZIP};

// ─── 常量（线上契约，方案 2.4.3） ────────────────────────────

pub(crate) const E2E_FORMAT: &str = "cc-switch-sync-e2e";
pub(crate) const E2E_VERSION: u32 = 3;
/// v3 布局目录名：`{root}/v3/{profile}/`（与 v2 的 `v2/db-v6` 分目录并存）
pub(crate) const E2E_DIR: &str = "v3";
pub(crate) const DB_SQL_ENC: &str = "db.sql.enc";
pub(crate) const SKILLS_ZIP_ENC: &str = "skills.zip.enc";

pub(crate) const KDF_ALG: &str = "argon2id";
/// m = 64 MiB、t = 3、p = 1、盐 16 B（方案 2.4.1；参数写进 manifest 便于日后升级）
pub(crate) const KDF_M_KIB: u32 = 64 * 1024;
pub(crate) const KDF_T: u32 = 3;
pub(crate) const KDF_P: u32 = 1;
pub(crate) const KDF_SALT_LEN: usize = 16;
pub(crate) const KEY_LEN: usize = 32;
pub(crate) const NONCE_LEN: usize = 24;

// ─── 密钥类型 ────────────────────────────────────────────────

/// 口令派生的密钥加密密钥（KEK）。
pub(crate) struct Kek(Zeroizing<[u8; KEY_LEN]>);

impl Kek {
    /// 供凭据便携包（`secrets::portable`）复用同一套 AEAD。
    pub(crate) fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}
/// 每次快照随机生成的数据加密密钥（DEK）。
pub(crate) struct Dek(Zeroizing<[u8; KEY_LEN]>);

pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    rand_core::OsRng.fill_bytes(&mut buf);
    buf
}

pub(crate) fn aead_seal(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    pt: &[u8],
) -> Result<Vec<u8>, AppError> {
    let cipher = XChaCha20Poly1305::new_from_slice(key)
        .map_err(|e| AppError::Config(format!("AEAD 密钥长度错误: {e}")))?;
    cipher
        .encrypt(XNonce::from_slice(nonce), Payload { msg: pt, aad })
        .map_err(|e| AppError::Config(format!("AEAD 加密失败: {e:?}")))
}

pub(crate) fn aead_open(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    ct: &[u8],
) -> Result<Vec<u8>, AppError> {
    let cipher = XChaCha20Poly1305::new_from_slice(key)
        .map_err(|e| AppError::Config(format!("AEAD 密钥长度错误: {e}")))?;
    cipher
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
        .map_err(|_| e2e_failure())
}

/// 口令错误或载荷被篡改。两者刻意共用同一错误、不区分（方案 2.4.2）。
pub(crate) fn e2e_failure() -> AppError {
    AppError::localized(
        "sync.e2e.auth_failed",
        "同步口令不正确或远端数据已被篡改",
        "Sync passphrase is incorrect or the remote data has been tampered with",
    )
}

/// 远端载荷结构无法识别（非本模块产物）。
fn payload_shape_invalid() -> AppError {
    AppError::localized(
        "sync.e2e.payload_invalid",
        "远端 v3 加密载荷结构无法识别",
        "Remote v3 encrypted payload is unrecognized",
    )
}

// ─── KDF ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KdfParams {
    pub alg: String,
    #[serde(rename = "m")]
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
    #[serde(with = "base64_vec")]
    pub salt: Vec<u8>,
}

/// 无远端时本机生成新盐（方案 2.4.2：盐来自远端，远端为空才新建）。
pub(crate) fn new_kdf_params() -> KdfParams {
    KdfParams {
        alg: KDF_ALG.to_string(),
        m_kib: KDF_M_KIB,
        t: KDF_T,
        p: KDF_P,
        salt: random_bytes::<KDF_SALT_LEN>().to_vec(),
    }
}

/// 校验远端下发的 KDF 参数：拒绝降级（低于本机默认）与极端值（防派生 DoS）。
pub(crate) fn validate_kdf_params(params: &KdfParams) -> Result<(), AppError> {
    if params.alg != KDF_ALG {
        return Err(AppError::localized(
            "sync.e2e.kdf_alg_unsupported",
            format!("不支持的同步加密 KDF 算法: {}", params.alg),
            format!("Unsupported sync encryption KDF algorithm: {}", params.alg),
        ));
    }
    if params.salt.len() < KDF_SALT_LEN {
        return Err(AppError::localized(
            "sync.e2e.kdf_salt_invalid",
            "远端 KDF 盐长度不足",
            "Remote KDF salt is too short",
        ));
    }
    if !(KDF_M_KIB..=1024 * 1024).contains(&params.m_kib)
        || !(KDF_T..=16).contains(&params.t)
        || !(1..=4).contains(&params.p)
    {
        return Err(AppError::localized(
            "sync.e2e.kdf_params_out_of_range",
            format!(
                "远端 KDF 参数超界: m={}, t={}, p={}",
                params.m_kib, params.t, params.p
            ),
            format!(
                "Remote KDF params out of range: m={}, t={}, p={}",
                params.m_kib, params.t, params.p
            ),
        ));
    }
    Ok(())
}

/// 口令 + 盐 → KEK。参数取自远端外层 manifest，先验证再派生。
pub(crate) fn derive_kek(passphrase: &str, params: &KdfParams) -> Result<Kek, AppError> {
    validate_kdf_params(params)?;
    let argon = Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(params.m_kib, params.t, params.p, Some(KEY_LEN))
            .map_err(|e| AppError::Config(format!("Argon2 参数无效: {e}")))?,
    );
    let mut out = [0u8; KEY_LEN];
    argon
        .hash_password_into(passphrase.as_bytes(), &params.salt, &mut out)
        .map_err(|e| AppError::Config(format!("Argon2 派生失败: {e}")))?;
    Ok(Kek(Zeroizing::new(out)))
}

// ─── 线上结构（方案 2.4.3） ──────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BlobRef {
    pub nonce: String,
    pub ct: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct EncArtifactMeta {
    /// 密文 SHA-256：服务器端工具不解密也能做完整性校验（方案 2.4.1）
    pub sha256: String,
    pub size: u64,
    pub nonce: String,
}

/// 明文外层 manifest：只有协议参数与密文摘要，不含设备名、时间等用户数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OuterManifest {
    pub format: String,
    pub version: u32,
    pub db_compat_version: u32,
    pub kdf: KdfParams,
    pub wrapped_dek: BlobRef,
    pub sealed: BlobRef,
    pub artifacts: BTreeMap<String, EncArtifactMeta>,
    /// 明文哈希的哈希，不泄露隐私；参与所有 AAD 绑定。
    pub snapshot_id: String,
    pub seq: u64,
}

/// 加密在 `sealed` 里的内层 manifest（设备名、创建时间、明文哈希）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InnerManifest {
    pub device_name: String,
    pub created_at: String,
    pub plain_hashes: BTreeMap<String, String>,
}

/// AAD 绑定（方案 2.4.1）：`format ‖ version ‖ snapshotId ‖ artifactName ‖ seq`。
/// KEK 封装与 sealed 分别用 "dek" / "manifest.json" 作为 artifactName。
pub(crate) fn artifact_aad(snapshot_id: &str, artifact_name: &str, seq: u64) -> Vec<u8> {
    format!("E2E1\0{E2E_FORMAT}\0{E2E_VERSION}\0{snapshot_id}\0{artifact_name}\0{seq}").into_bytes()
}

// ─── seal ────────────────────────────────────────────────────

pub(crate) struct SealInputs<'a> {
    pub db_sql: &'a [u8],
    pub skills_zip: &'a [u8],
    pub device_name: String,
    pub created_at: String,
    /// 本机算好的新序号：max(本地已知, 远端) + 1
    pub seq: u64,
    pub db_compat_version: u32,
    /// 沿用远端口令盐；None = 新远端，随机生成
    pub kdf: Option<KdfParams>,
}

pub(crate) struct SealedSnapshot {
    pub manifest_bytes: Vec<u8>,
    pub manifest_hash: String,
    pub db_sql_enc: Vec<u8>,
    pub skills_zip_enc: Vec<u8>,
}

/// 口令 → KEK → 封装随机 DEK → 加密内层 manifest 与两个 blob。
/// 仅测试用的便捷包装；生产走 `seal_with_kek`（KEK 由 AppState 缓存派生）。
#[cfg(test)]
pub(crate) fn seal(snapshot: &SealInputs, passphrase: &str) -> Result<SealedSnapshot, AppError> {
    let kdf = snapshot.kdf.clone().unwrap_or_else(new_kdf_params);
    let kek = derive_kek(passphrase, &kdf)?;
    seal_with_kek(snapshot, &kek)
}

/// 用已派生的 KEK 封装。KEK 缓存路径（AppState）走这个，避免每次同步重跑 Argon2。
/// 盐取自 `snapshot.kdf`（生产路径必先填；None 时才新派生一份，仅测试可能命中）。
pub(crate) fn seal_with_kek(snapshot: &SealInputs, kek: &Kek) -> Result<SealedSnapshot, AppError> {
    let kdf = snapshot.kdf.clone().unwrap_or_else(new_kdf_params);
    let dek = Dek(Zeroizing::new(random_bytes::<KEY_LEN>()));

    let plain_hashes: BTreeMap<String, String> = BTreeMap::from([
        (REMOTE_DB_SQL.to_string(), sha256_hex(snapshot.db_sql)),
        (
            REMOTE_SKILLS_ZIP.to_string(),
            sha256_hex(snapshot.skills_zip),
        ),
    ]);
    let snapshot_id = sha256_hex(
        plain_hashes
            .iter()
            .map(|(name, hash)| format!("{name}:{hash}"))
            .collect::<Vec<_>>()
            .join("|")
            .as_bytes(),
    );

    let seal_blob = |key: &Zeroizing<[u8; KEY_LEN]>,
                     name: &str,
                     pt: &[u8]|
     -> Result<(Vec<u8>, Vec<u8>, EncArtifactMeta), AppError> {
        let nonce = random_bytes::<NONCE_LEN>();
        let ct = aead_seal(
            key,
            &nonce,
            &artifact_aad(&snapshot_id, name, snapshot.seq),
            pt,
        )?;
        Ok((
            nonce.to_vec(),
            ct.clone(),
            EncArtifactMeta {
                sha256: sha256_hex(&ct),
                size: ct.len() as u64,
                nonce: b64_encode(&nonce),
            },
        ))
    };

    let inner = InnerManifest {
        device_name: snapshot.device_name.clone(),
        created_at: snapshot.created_at.clone(),
        plain_hashes,
    };
    let inner_bytes =
        serde_json::to_vec(&inner).map_err(|e| AppError::JsonSerialize { source: e })?;

    let dek_nonce = random_bytes::<NONCE_LEN>();
    let dek_ct = aead_seal(
        &kek.0,
        &dek_nonce,
        &artifact_aad(&snapshot_id, "dek", snapshot.seq),
        dek.0.as_slice(),
    )?;
    let wrapped_dek = BlobRef {
        nonce: b64_encode(&dek_nonce),
        ct: b64_encode(&dek_ct),
    };

    let (sealed_nonce, sealed_ct_bytes, _) = seal_blob(&dek.0, REMOTE_MANIFEST, &inner_bytes)?;
    let sealed = BlobRef {
        nonce: b64_encode(&sealed_nonce),
        ct: b64_encode(&sealed_ct_bytes),
    };

    let (_, db_sql_enc, db_meta) = seal_blob(&dek.0, DB_SQL_ENC, snapshot.db_sql)?;
    let (_, skills_zip_enc, skills_meta) = seal_blob(&dek.0, SKILLS_ZIP_ENC, snapshot.skills_zip)?;

    let mut artifacts = BTreeMap::new();
    artifacts.insert(DB_SQL_ENC.to_string(), db_meta);
    artifacts.insert(SKILLS_ZIP_ENC.to_string(), skills_meta);

    let outer = OuterManifest {
        format: E2E_FORMAT.to_string(),
        version: E2E_VERSION,
        db_compat_version: snapshot.db_compat_version,
        kdf,
        wrapped_dek,
        sealed,
        artifacts,
        snapshot_id,
        seq: snapshot.seq,
    };
    let manifest_bytes =
        serde_json::to_vec_pretty(&outer).map_err(|e| AppError::JsonSerialize { source: e })?;
    let manifest_hash = sha256_hex(&manifest_bytes);

    Ok(SealedSnapshot {
        manifest_bytes,
        manifest_hash,
        db_sql_enc,
        skills_zip_enc,
    })
}

// ─── open ────────────────────────────────────────────────────

pub(crate) struct OpenedManifest {
    pub outer: OuterManifest,
    pub inner: InnerManifest,
    dek: Dek,
}

/// 打开外层 manifest：结构、格式、版本、KDF 参数、大小上限（不解密）。
pub(crate) fn parse_outer_manifest(bytes: &[u8]) -> Result<OuterManifest, AppError> {
    if bytes.len() > crate::services::sync_protocol::MAX_MANIFEST_BYTES {
        return Err(AppError::localized(
            "sync.e2e.manifest_too_large",
            "远端加密 manifest 超出大小上限",
            "Remote encrypted manifest exceeds size limit",
        ));
    }
    let outer: OuterManifest =
        serde_json::from_slice(bytes).map_err(|_| payload_shape_invalid())?;
    if outer.format != E2E_FORMAT || outer.version != E2E_VERSION {
        return Err(payload_shape_invalid());
    }
    validate_kdf_params(&outer.kdf)?;
    if outer.artifacts.len() != 2
        || !outer.artifacts.contains_key(DB_SQL_ENC)
        || !outer.artifacts.contains_key(SKILLS_ZIP_ENC)
    {
        return Err(payload_shape_invalid());
    }
    Ok(outer)
}

/// 口令 → 派生 KEK → 解 DEK → 开内层 manifest。口令错或任何字节被改都在这里失败。
/// 仅测试用的便捷包装；生产走 `parse_outer_manifest` + `open_manifest_with_kek`。
#[cfg(test)]
pub(crate) fn open_manifest(
    outer_bytes: &[u8],
    passphrase: &str,
) -> Result<OpenedManifest, AppError> {
    let outer = parse_outer_manifest(outer_bytes)?;
    let kek = derive_kek(passphrase, &outer.kdf)?;
    open_manifest_with_kek(outer, &kek)
}

/// 用已派生 KEK 打开（KEK 缓存路径）。调用方须保证 `kek` 由 `outer.kdf` 派生。
pub(crate) fn open_manifest_with_kek(
    outer: OuterManifest,
    kek: &Kek,
) -> Result<OpenedManifest, AppError> {
    let dek_nonce = b64_decode(&outer.wrapped_dek.nonce)?;
    let dek_ct = b64_decode(&outer.wrapped_dek.ct)?;
    let dek_bytes = aead_open(
        &kek.0,
        &fixed_nonce(&dek_nonce)?,
        &artifact_aad(&outer.snapshot_id, "dek", outer.seq),
        &dek_ct,
    )?;
    let dek = Dek(Zeroizing::new(
        <[u8; KEY_LEN]>::try_from(dek_bytes.as_slice()).map_err(|_| e2e_failure())?,
    ));

    let sealed_nonce = b64_decode(&outer.sealed.nonce)?;
    let sealed_ct = b64_decode(&outer.sealed.ct)?;
    let inner_bytes = aead_open(
        &dek.0,
        &fixed_nonce(&sealed_nonce)?,
        &artifact_aad(&outer.snapshot_id, REMOTE_MANIFEST, outer.seq),
        &sealed_ct,
    )?;
    let inner: InnerManifest = serde_json::from_slice(&inner_bytes).map_err(|_| e2e_failure())?;
    if inner.plain_hashes.len() != 2
        || !inner.plain_hashes.contains_key(REMOTE_DB_SQL)
        || !inner.plain_hashes.contains_key(REMOTE_SKILLS_ZIP)
    {
        return Err(payload_shape_invalid());
    }
    Ok(OpenedManifest { outer, inner, dek })
}

/// seq 回滚检测（方案 2.4.4）。返回 `Some((remote_seq, last_applied))` 表示远端更旧、
/// 需用户显式放行；`last_applied == 0`（从未应用过）或 `allow_rollback` 时返回 `None`。
pub(crate) fn detect_seq_regression(
    remote_seq: u64,
    last_applied: u64,
    allow_rollback: bool,
) -> Option<(u64, u64)> {
    if !allow_rollback && last_applied > 0 && remote_seq < last_applied {
        Some((remote_seq, last_applied))
    } else {
        None
    }
}

/// 上传序号（方案 2.4.4）：`max(本机已上传, 远端现有) + 1`。两台设备都读到同一远端
/// seq 时会算出相同候选值，真正串行化由条件写（If-Match/412）裁决——落败方收到冲突、
/// 重新下载后再传，序号严格不回退。
pub(crate) fn next_seq(last_uploaded: u64, remote_seq: Option<u64>) -> u64 {
    last_uploaded.max(remote_seq.unwrap_or(0)) + 1
}

/// 解密单个 artifact：先核对密文大小/哈希（外层 manifest），再用 DEK + AAD 解密，
/// 最后核对明文哈希（内层 manifest）。`enc_name`/`plain_name` 由调用方指定，
/// 拿 A 快照的 blob 配 B 快照的 manifest 会在 AAD 或哈希处失败。
pub(crate) fn open_artifact(
    opened: &OpenedManifest,
    enc_name: &str,
    plain_name: &str,
    ciphertext: &[u8],
) -> Result<Vec<u8>, AppError> {
    let meta = opened
        .outer
        .artifacts
        .get(enc_name)
        .ok_or_else(e2e_failure)?;
    if meta.size != ciphertext.len() as u64 || meta.sha256 != sha256_hex(ciphertext) {
        return Err(AppError::localized(
            "sync.e2e.artifact_ciphertext_mismatch",
            format!("密文 {enc_name} 与 manifest 登记不符（大小或哈希）"),
            format!("Ciphertext {enc_name} does not match the manifest (size or hash)"),
        ));
    }
    let nonce = b64_decode(&meta.nonce)?;
    let plaintext = aead_open(
        &opened.dek.0,
        &fixed_nonce(&nonce)?,
        &artifact_aad(&opened.outer.snapshot_id, enc_name, opened.outer.seq),
        ciphertext,
    )?;
    let expected = opened
        .inner
        .plain_hashes
        .get(plain_name)
        .ok_or_else(payload_shape_invalid)?;
    if expected != &sha256_hex(&plaintext) {
        return Err(AppError::localized(
            "sync.e2e.artifact_plaintext_mismatch",
            format!("{plain_name} 解密后哈希与内层 manifest 不符"),
            format!("{plain_name} plaintext hash mismatches the sealed manifest"),
        ));
    }
    Ok(plaintext)
}

// ─── 杂项 ────────────────────────────────────────────────────

fn fixed_nonce(bytes: &[u8]) -> Result<[u8; NONCE_LEN], AppError> {
    <[u8; NONCE_LEN]>::try_from(bytes).map_err(|_| e2e_failure())
}

fn b64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn b64_decode(s: &str) -> Result<Vec<u8>, AppError> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|_| e2e_failure())
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

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_salt() -> Vec<u8> {
        b"0123456789abcdef".to_vec()
    }

    fn test_kdf() -> KdfParams {
        // 用生产参数：validate_kdf_params 拒绝降级（m/t 低于默认），测试不能走后门。
        KdfParams {
            alg: KDF_ALG.to_string(),
            m_kib: KDF_M_KIB,
            t: KDF_T,
            p: KDF_P,
            salt: test_salt(),
        }
    }

    fn snapshot(seq: u64) -> SealInputs<'static> {
        static DB: &[u8] = b"BEGIN;INSERT INTO providers VALUES('x');COMMIT;";
        static ZIP: &[u8] = b"PK\x03\x04fake-zip-bytes";
        SealInputs {
            db_sql: DB,
            skills_zip: ZIP,
            device_name: "DEV-A".to_string(),
            created_at: "2026-09-21T00:00:00Z".to_string(),
            seq,
            db_compat_version: 6,
            kdf: Some(test_kdf()),
        }
    }

    fn seal_fast(seq: u64) -> SealedSnapshot {
        seal(
            &SealInputs {
                kdf: Some(test_kdf()),
                ..snapshot(seq)
            },
            "hunter2",
        )
        .expect("seal")
    }

    #[test]
    fn seal_open_roundtrip_recovers_plaintexts() {
        let sealed = seal_fast(7);
        let opened = open_manifest(&sealed.manifest_bytes, "hunter2").expect("open");
        assert_eq!(opened.outer.seq, 7);
        assert_eq!(opened.inner.device_name, "DEV-A");
        let db = open_artifact(&opened, DB_SQL_ENC, REMOTE_DB_SQL, &sealed.db_sql_enc).unwrap();
        let skills = open_artifact(
            &opened,
            SKILLS_ZIP_ENC,
            REMOTE_SKILLS_ZIP,
            &sealed.skills_zip_enc,
        )
        .unwrap();
        assert_eq!(db, snapshot(7).db_sql);
        assert_eq!(skills, snapshot(7).skills_zip);
    }

    #[test]
    fn tampered_ciphertext_byte_fails() {
        let sealed = seal_fast(3);
        let opened = open_manifest(&sealed.manifest_bytes, "hunter2").expect("open");
        let mut bad = sealed.db_sql_enc.clone();
        let last = bad.len() - 1;
        bad[last] ^= 0x01;
        assert!(open_artifact(&opened, DB_SQL_ENC, REMOTE_DB_SQL, &bad).is_err());
    }

    #[test]
    fn wrong_passphrase_fails_without_distinguishing_tampering() {
        let sealed = seal_fast(4);
        let err = open_manifest(&sealed.manifest_bytes, "wrong-pass")
            .err()
            .expect("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("口令不正确或远端数据已被篡改")
                || msg.contains("passphrase is incorrect or the remote data has been tampered"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn aad_cross_binding_rejected() {
        // 把快照 A 的 db blob 配到快照 B 的 manifest 上
        let a = seal_fast(10);
        let b = seal_fast(11);
        let opened_b = open_manifest(&b.manifest_bytes, "hunter2").expect("open b");
        assert!(open_artifact(&opened_b, DB_SQL_ENC, REMOTE_DB_SQL, &a.db_sql_enc).is_err());
    }

    #[test]
    fn tampered_manifest_byte_fails_open() {
        let sealed = seal_fast(5);
        let mut bytes = sealed.manifest_bytes.clone();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0x20;
        assert!(open_manifest(&bytes, "hunter2").is_err());
    }

    #[test]
    fn seq_regression_detected() {
        assert_eq!(detect_seq_regression(3, 9, false), Some((3, 9)));
        assert_eq!(
            detect_seq_regression(3, 9, true),
            None,
            "用户显式接受回滚应放行"
        );
        assert_eq!(detect_seq_regression(1, 0, false), None, "从未应用过不应拦");
        assert_eq!(detect_seq_regression(9, 9, false), None);
        assert_eq!(detect_seq_regression(10, 9, false), None);
    }

    #[test]
    fn next_seq_is_monotonic_and_respects_remote() {
        // 首次上传：无远端、本机没传过 → 1
        assert_eq!(next_seq(0, None), 1);
        // 本机已传到 5，远端也是 5 → 6
        assert_eq!(next_seq(5, Some(5)), 6);
        // 别的设备已把远端推到 9，本机 last_uploaded 仍 5 → 取远端 9+1=10（不回退）
        assert_eq!(next_seq(5, Some(9)), 10);
        // 本机领先远端（离线攒了几次）→ 以本机为准 +1
        assert_eq!(next_seq(12, Some(9)), 13);
        // 两设备同读远端 seq=7：候选都是 8，谁的条件写先成功谁占，另一个收 412 重下
        assert_eq!(next_seq(0, Some(7)), next_seq(3, Some(7)));
    }

    #[test]
    fn kdf_param_validation_rejects_downgrade_and_dos() {
        assert!(validate_kdf_params(&test_kdf()).is_ok());
        let mut alg_bad = test_kdf();
        alg_bad.alg = "scrypt".into();
        assert!(validate_kdf_params(&alg_bad).is_err());
        let mut m_down = test_kdf();
        m_down.m_kib = 1024;
        assert!(validate_kdf_params(&m_down).is_err());
        let mut m_huge = test_kdf();
        m_huge.m_kib = 64 * 1024 * 1024;
        assert!(validate_kdf_params(&m_huge).is_err());
        let mut short_salt = test_kdf();
        short_salt.salt = b"tiny".to_vec();
        assert!(validate_kdf_params(&short_salt).is_err());
    }

    #[test]
    fn outer_manifest_hides_user_data() {
        let sealed = seal_fast(6);
        let text = String::from_utf8(sealed.manifest_bytes.clone()).expect("utf8 json");
        assert!(!text.contains("DEV-A"), "设备名不得出现在明文外层");
        assert!(!text.contains("2026-09-21"), "创建时间不得出现在明文外层");
        assert!(!text.contains("hunter2"), "口令相关材料不得入库");
    }
}
