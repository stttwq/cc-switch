//! S0（施工方案 §5.1 / §6）：同步与 SQL 导入导出的数据边界——策略集中定义。
//!
//! 施工方案 §2.2-2：「哪些表/键算设备本地、导出时怎么裁剪、导入时怎么合并」
//! **只允许在本模块定义**。同步导出、手动导出、同步导入、手动导入都必须调用
//! 本模块，禁止在各个命令里各写一套 `if is_1p`。
//!
//! 本提交只是骨架（S0）：类型先立起来锁定接口形状，行为由 S3（导出裁剪）
//! 与 S4（导入合并）填充。当前对外无任何调用方，`dead_code` 显式豁免。

/// 导出用途（S3-1）：云同步快照与手动配置导出共用同一套裁剪规则，仅
/// `provider_endpoints`（C 级）与 meta 用途标记不同（D-S1 / S3-2）。
#[allow(dead_code)] // S0 骨架：S3-1 接线后启用
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExportPurpose {
    /// WebDAV / S3 云同步快照
    Sync,
    /// 手动 SQL 配置导出
    ConfigFile,
}

/// 导出元信息（S3-2）：写入 SQL 头部 `-- cc-switch-meta:` 行。
/// 只用于导入确认框的提示与统计，**绝不参与导入侧的安全决策**（§2.2-4：
/// 按本机模式决定，不信任文件）。
#[allow(dead_code)] // S0 骨架：S3-2 生成、S6-2 展示后启用
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExportMeta {
    /// 导出方后端（`onepassword` / `credential_manager`）
    pub(crate) backend: String,
    /// 是否携带端点缓存（1P 模式恒为 false，D-S1）
    pub(crate) endpoints_included: bool,
    /// 携带的 1P 引用行数（D-S3）
    pub(crate) refs_included: usize,
    /// 导出设备名（`normalize_device_name` 产物，不得含换行）
    pub(crate) device: String,
}

/// 本机凭据后端（S4-1）：导入侧的采纳/保留规则由**导入方**模式决定。
#[allow(dead_code)] // S0 骨架：S4-1 接线后启用
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackendKind {
    OnePassword,
    CredentialManager,
}

/// 导入合并策略（S4-1）：`merge_for_import` 的输入。`local_vault` 取
/// `settings.onepassword.vault`，与 `OnePasswordVault` 的 `self.vault` 同源
/// （施工时核实，见 §5.4 S4-1）。
#[allow(dead_code)] // S0 骨架：S4-1 接线后启用
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportPolicy {
    pub(crate) local_backend: BackendKind,
    /// 本机保险箱 id；仅凭据管理器模式为 `None`
    pub(crate) local_vault: Option<String>,
}

/// 导入报告（S4-1/S4-3）：三条导入路径（同步下载 / SQL 导入 / `.db` 恢复）
/// 共用的统计与「未关联供应商」清单。
#[allow(dead_code)] // S0 骨架：S4-1 生成、S4-3 消费后启用
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ImportReport {
    /// 采纳的远端引用行数（vault 匹配才采纳，D-S3）
    pub(crate) adopted_refs: usize,
    /// 导入后存在、但本机既无引用也未采纳到引用的供应商 `(app, id)`
    pub(crate) unlinked_providers: Vec<(String, String)>,
    /// 清理掉的失效引用行数（供应商已不存在）
    pub(crate) pruned_refs: usize,
    /// 清理掉的失效端点行数（供应商已不存在）
    pub(crate) pruned_endpoints: usize,
}

#[cfg(test)]
mod tests {
    //! S0：仅锁定类型的可构造性与接口形状，行为断言由 S3/S4 的测试补齐。

    use super::*;

    #[test]
    fn skeleton_types_are_constructible() {
        assert_eq!(ExportPurpose::Sync, ExportPurpose::Sync);
        assert_ne!(BackendKind::OnePassword, BackendKind::CredentialManager);
        let policy = ImportPolicy {
            local_backend: BackendKind::OnePassword,
            local_vault: Some("vault-x".to_string()),
        };
        assert_eq!(policy.local_vault.as_deref(), Some("vault-x"));
        let report = ImportReport::default();
        assert_eq!(report.adopted_refs, 0);
        assert!(report.unlinked_providers.is_empty());
    }
}
