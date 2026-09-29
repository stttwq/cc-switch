use indexmap::IndexMap;
use std::collections::HashMap;

use crate::app_config::{AppType, McpServer};
use crate::error::AppError;
use crate::mcp;
use crate::store::AppState;

/// MCP 相关业务逻辑（v3.7.0 统一结构）
pub struct McpService;

/// SEC-A：内容未经本机批准（或批准已因内容变化失效）时投影/启用被拒绝。
/// 前端捕获该错误后应打开审批确认对话框，而不是当作普通失败提示。
pub(crate) const MCP_APPROVAL_REQUIRED_KEY: &str = "mcp.approval_required";

fn approval_required_error() -> AppError {
    AppError::localized(
        MCP_APPROVAL_REQUIRED_KEY,
        "该 MCP 服务器的行为内容尚未经本机批准（或内容已变化使批准失效），请在查看完整配置后重新确认。",
        "This MCP server's behavior content has not been approved locally (or its approval was invalidated by a content change). Review the full config and confirm again.",
    )
}

/// SEC-A：把服务器行为内容（`server` JSON）规范化为审批修订串。
///
/// 对象键递归排序、数组严格保序、字符串/数字/布尔原样保留——不做任何会改变
/// 执行语义的 trim 或大小写转换。transport、command、args、cwd、env、URL、
/// headers 及其余字段全部参与比较，改任意一项都会使旧批准失效。
pub(crate) fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let inner = keys
                .into_iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_else(|_| "\"\"".to_string()),
                        canonical_json(&map[k])
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{inner}}}")
        }
        serde_json::Value::Array(items) => {
            let inner = items
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",");
            format!("[{inner}]")
        }
        other => serde_json::to_string(other).unwrap_or_else(|_| "\"\"".to_string()),
    }
}

/// 单个应用维度的审批状态（供审批确认 UI 使用）。
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAppApprovalState {
    pub enabled: bool,
    pub approved: bool,
    /// 当前内容的修订；与提交的 expectedRevision 绑定确认（§4.3-6）
    pub revision: String,
}

/// 单个服务器的审批状态（每个条目始终含 claude / codex 两个应用）。
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerApprovalState {
    pub server_id: String,
    pub apps: HashMap<String, McpAppApprovalState>,
}

impl McpService {
    /// 当前内容的审批修订（规范 JSON 全文，入库即与 server_config 同级明文，
    /// 不另存可离线猜测的秘密摘要，施工方案 §4.3-4）。
    pub fn approval_revision(server: &McpServer) -> String {
        canonical_json(&server.server)
    }
    /// 指定 (服务器, 应用) 是否持有与当前内容一致的批准。
    pub fn is_approved(state: &AppState, server: &McpServer, app: &AppType) -> bool {
        match state.db.get_mcp_approved_revision(&server.id, app.as_str()) {
            Ok(Some(approved)) => approved == Self::approval_revision(server),
            Ok(None) => false,
            Err(err) => {
                log::warn!(
                    "读取 MCP 审批状态失败 ({} / {}): {err}",
                    server.id,
                    app.as_str()
                );
                false
            }
        }
    }

    /// 记录对当前内容的批准（表单保存、启用开关、审批确认等显式本机动作）。
    pub fn record_approval(
        state: &AppState,
        server: &McpServer,
        app: &AppType,
    ) -> Result<(), AppError> {
        state
            .db
            .upsert_mcp_approval(&server.id, app.as_str(), &Self::approval_revision(server))
    }

    /// 获取所有 MCP 服务器（统一结构）
    pub fn get_all_servers(state: &AppState) -> Result<IndexMap<String, McpServer>, AppError> {
        state.db.get_all_mcp_servers()
    }

    /// 添加或更新 MCP 服务器
    pub fn upsert_server(state: &AppState, server: McpServer) -> Result<(), AppError> {
        // 读取旧状态：用于处理“编辑时取消勾选某个应用”的场景（需要从对应 live 配置中移除）
        let prev_apps = state
            .db
            .get_all_mcp_servers()?
            .get(&server.id)
            .map(|s| s.apps.clone())
            .unwrap_or_default();

        state.db.save_mcp_server(&server)?;

        // SEC-A：表单保存是用户对当前完整内容的显式决定——内容的批准与启用位
        // 解耦：保存即批准该内容（两个本机应用），启用位只决定投影到哪个应用，
        // 之后在 UI 里启用应用不再需要重复审批。
        let supported = [AppType::Claude, AppType::Codex];
        for app in supported {
            Self::record_approval(state, &server, &app)?;
        }

        // 处理禁用：若旧版本启用但新版本取消，则需要从该应用的 live 配置移除
        if prev_apps.claude && !server.apps.claude {
            Self::remove_server_from_app(state, &server.id, &AppType::Claude)?;
        }
        if prev_apps.codex && !server.apps.codex {
            Self::remove_server_from_app(state, &server.id, &AppType::Codex)?;
        }

        // 同步到各个启用的应用
        Self::sync_server_to_apps(state, &server)?;

        Ok(())
    }

    /// 删除 MCP 服务器
    pub fn delete_server(state: &AppState, id: &str) -> Result<bool, AppError> {
        let server = state.db.get_all_mcp_servers()?.shift_remove(id);

        if let Some(server) = server {
            state.db.delete_mcp_server(id)?;
            // SEC-A：审批是按 (服务器, 应用) 绑定的，条目删除后不留孤儿行。
            state.db.delete_mcp_approvals_for_server(id)?;

            // 从所有应用的 live 配置中移除
            Self::remove_server_from_all_apps(state, id, &server)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// 切换指定应用的启用状态
    pub fn toggle_app(
        state: &AppState,
        server_id: &str,
        app: AppType,
        enabled: bool,
    ) -> Result<(), AppError> {
        if let Some(server) = state
            .db
            .update_mcp_server_app_enabled(server_id, &app, enabled)?
        {
            // 同步到对应应用
            if enabled {
                Self::sync_server_to_app(state, &server, &app)?;
            } else {
                Self::remove_server_from_app(state, server_id, &app)?;
            }
        }

        Ok(())
    }

    /// 将 MCP 服务器同步到所有启用的应用
    fn sync_server_to_apps(state: &AppState, server: &McpServer) -> Result<(), AppError> {
        for app in server.apps.enabled_apps() {
            Self::sync_server_to_app(state, server, &app)?;
        }

        Ok(())
    }

    /// 将 MCP 服务器同步到指定应用。
    ///
    /// SEC-A：这是**所有**投影路径（表单保存、启用开关、启动重投影、手动同步、
    /// 导入后处理）的唯一入口，批准门禁放在这里，任何调用方都不能绕过：
    /// 未批准或内容已变化即拒绝，返回 [`approval_required_error`]。
    fn sync_server_to_app(
        state: &AppState,
        server: &McpServer,
        app: &AppType,
    ) -> Result<(), AppError> {
        if !Self::is_approved(state, server, app) {
            return Err(approval_required_error());
        }
        Self::sync_server_to_app_no_config(server, app)
    }

    fn sync_server_to_app_no_config(server: &McpServer, app: &AppType) -> Result<(), AppError> {
        match app {
            AppType::Claude => {
                mcp::sync_single_server_to_claude(&Default::default(), &server.id, &server.server)?;
            }
            AppType::Codex => {
                // Codex uses TOML format, must use the correct function
                mcp::sync_single_server_to_codex(&Default::default(), &server.id, &server.server)?;
            }
            AppType::Pi => {}
        }
        Ok(())
    }

    /// 从所有曾启用过该服务器的应用中移除
    fn remove_server_from_all_apps(
        state: &AppState,
        id: &str,
        server: &McpServer,
    ) -> Result<(), AppError> {
        // 从所有曾启用的应用中移除
        for app in server.apps.enabled_apps() {
            Self::remove_server_from_app(state, id, &app)?;
        }
        Ok(())
    }

    fn remove_server_from_app(_state: &AppState, id: &str, app: &AppType) -> Result<(), AppError> {
        match app {
            AppType::Claude => mcp::remove_server_from_claude(id)?,
            AppType::Codex => mcp::remove_server_from_codex(id)?,
            AppType::Pi => {}
        }
        Ok(())
    }

    /// 手动同步所有启用的 MCP 服务器到对应的应用。
    ///
    /// Best-effort：单个应用投影失败（如 ~/.claude.json 坏 JSON）不阻断
    /// 其余应用——各应用的 live 文件互相独立，一处损坏没有理由让其他
    /// 应用的 MCP 状态陈旧。全部跑完后若有失败，聚合成一个错误上报，
    /// 保留调用方的可见性。
    pub fn sync_all_enabled(state: &AppState) -> Result<(), AppError> {
        let servers = Self::get_all_servers(state)?;

        let mut failures: Vec<String> = Vec::new();
        for app in AppType::all() {
            if let Err(err) = Self::project_servers_to_app(state, &servers, &app) {
                log::warn!("同步 MCP 到 {app:?} 失败: {err}");
                failures.push(format!("{}: {err}", app.as_str()));
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(AppError::Message(format!(
                "部分应用 MCP 同步失败: {}",
                failures.join("; ")
            )))
        }
    }

    /// 只把启用状态投影到单个应用。某个应用的 live 被整体重写后用它做
    /// 定向重投影，避免把无关应用的失败面（如 ~/.claude.json 坏 JSON）
    /// 牵连进目标应用的关键路径。
    pub fn sync_enabled_for_app(state: &AppState, app: &AppType) -> Result<(), AppError> {
        let servers = Self::get_all_servers(state)?;
        Self::project_servers_to_app(state, &servers, app)
    }

    fn project_servers_to_app(
        state: &AppState,
        servers: &IndexMap<String, McpServer>,
        app: &AppType,
    ) -> Result<(), AppError> {
        if matches!(app, AppType::Pi) {
            return Ok(());
        }

        for server in servers.values() {
            if server.apps.is_enabled_for(app) {
                if Self::is_approved(state, server, app) {
                    Self::sync_server_to_app(state, server, app)?;
                } else {
                    // SEC-A（§4.3-7）：未批准的内容不投影；live 里可能还留着
                    // 旧的已批准内容（内容被外部改动后），一并移除——停用该
                    // CCS 托管条目并等待用户重新审批，不用旧命令继续执行。
                    log::info!(
                        "MCP 服务器 '{}' 在 {app:?} 上待审批，已从 live 配置移除投影",
                        server.id
                    );
                    Self::remove_server_from_app(state, &server.id, app)?;
                }
            } else {
                Self::remove_server_from_app(state, &server.id, app)?;
            }
        }

        Ok(())
    }

    // ========================================================================
    // 兼容层：支持旧的 v3.6.x 命令（已废弃，将在 v4.0 移除）
    // ========================================================================

    /// [已废弃] 获取指定应用的 MCP 服务器（兼容旧 API）
    #[deprecated(since = "3.7.0", note = "Use get_all_servers instead")]
    pub fn get_servers(
        state: &AppState,
        app: AppType,
    ) -> Result<HashMap<String, serde_json::Value>, AppError> {
        let all_servers = Self::get_all_servers(state)?;
        let mut result = HashMap::new();

        for (id, server) in all_servers {
            if server.apps.is_enabled_for(&app) {
                result.insert(id, server.server);
            }
        }

        Ok(result)
    }

    /// [已废弃] 设置 MCP 服务器在指定应用的启用状态（兼容旧 API）
    #[deprecated(since = "3.7.0", note = "Use toggle_app instead")]
    pub fn set_enabled(
        state: &AppState,
        app: AppType,
        id: &str,
        enabled: bool,
    ) -> Result<bool, AppError> {
        Self::toggle_app(state, id, app, enabled)?;
        Ok(true)
    }

    /// [已废弃] 同步启用的 MCP 到指定应用（兼容旧 API）
    #[deprecated(since = "3.7.0", note = "Use sync_all_enabled instead")]
    pub fn sync_enabled(state: &AppState, app: AppType) -> Result<(), AppError> {
        let servers = Self::get_all_servers(state)?;

        for server in servers.values() {
            if server.apps.is_enabled_for(&app) {
                Self::sync_server_to_app(state, server, &app)?;
            }
        }

        Ok(())
    }

    /// 从 Claude 导入 MCP（v3.7.0 已更新为统一结构）
    pub fn import_from_claude(state: &AppState) -> Result<usize, AppError> {
        // 创建临时 MultiAppConfig 用于导入
        let mut temp_config = crate::app_config::MultiAppConfig::default();

        // 调用原有的导入逻辑（从 mcp.rs）
        let count = crate::mcp::import_from_claude(&mut temp_config)?;

        let mut new_count = 0;

        // 如果有导入的服务器，保存到数据库
        if count > 0 {
            if let Some(servers) = &temp_config.mcp.servers {
                let mut existing = state.db.get_all_mcp_servers()?;
                for server in servers.values() {
                    // 已存在：仅启用 Claude，不覆盖其他字段（与导入模块语义保持一致）
                    let to_save = if let Some(existing_server) = existing.get(&server.id) {
                        let mut merged = existing_server.clone();
                        merged.apps.claude = true;
                        merged
                    } else {
                        // 真正的新服务器
                        new_count += 1;
                        server.clone()
                    };

                    // SEC-A：从本机 live 配置导入的新服务器，其内容就是该机器
                    // CLI 当前已加载运行的配置，属于「本机 DB 与本机托管 live
                    // 内容一致」的继承场景（§4.3-8），批准当前内容。已存在条目
                    // 只翻启用位、不改内容，不新授批准——此前若因外部导入进入
                    // 待审批，保持待审批。
                    let is_new_server = existing.get(&server.id).is_none();
                    state.db.save_mcp_server(&to_save)?;
                    if is_new_server {
                        Self::record_approval(state, &to_save, &AppType::Claude)?;
                    }
                    existing.insert(to_save.id.clone(), to_save.clone());

                    // 导入是读取已有配置，不应反向写回任何应用的 live 配置。
                    // 显式编辑、启用/禁用或手动同步时再执行写回。
                }
            }
        }

        Ok(new_count)
    }

    /// 从 Codex 导入 MCP（v3.7.0 已更新为统一结构）
    pub fn import_from_codex(state: &AppState) -> Result<usize, AppError> {
        // 创建临时 MultiAppConfig 用于导入
        let mut temp_config = crate::app_config::MultiAppConfig::default();

        // 调用原有的导入逻辑（从 mcp.rs）
        let count = crate::mcp::import_from_codex(&mut temp_config)?;

        let mut new_count = 0;

        // 如果有导入的服务器，保存到数据库
        if count > 0 {
            if let Some(servers) = &temp_config.mcp.servers {
                let mut existing = state.db.get_all_mcp_servers()?;
                for server in servers.values() {
                    // 已存在：仅启用 Codex，不覆盖其他字段（与导入模块语义保持一致）
                    let to_save = if let Some(existing_server) = existing.get(&server.id) {
                        let mut merged = existing_server.clone();
                        merged.apps.codex = true;
                        merged
                    } else {
                        // 真正的新服务器
                        new_count += 1;
                        server.clone()
                    };

                    // SEC-A：同 import_from_claude——新条目内容来自本机 live，
                    // 批准；已存在条目只翻启用位，不新授批准。
                    let is_new_server = existing.get(&server.id).is_none();
                    state.db.save_mcp_server(&to_save)?;
                    if is_new_server {
                        Self::record_approval(state, &to_save, &AppType::Codex)?;
                    }
                    existing.insert(to_save.id.clone(), to_save.clone());

                    // 导入是读取已有配置，不应反向写回任何应用的 live 配置。
                    // 显式编辑、启用/禁用或手动同步时再执行写回。
                }
            }
        }

        Ok(new_count)
    }

    /// 从所有支持 MCP 的应用导入服务器，返回新导入的数量。
    ///
    /// Best-effort：单个应用导入失败（如坏 config.toml）不阻断其余应用；
    /// 全部跑完后若有失败，聚合成一个错误上报——历史实现逐应用
    /// `unwrap_or(0)` 吞错，坏文件只会表现为"导入成功 0 个"，用户
    /// 无从得知哪个应用出了问题。
    pub fn import_from_all_apps(state: &AppState) -> Result<usize, AppError> {
        let mut total = 0;
        let mut failures: Vec<String> = Vec::new();

        let results: [(&str, Result<usize, AppError>); 2] = [
            ("claude", Self::import_from_claude(state)),
            ("codex", Self::import_from_codex(state)),
        ];
        for (app, result) in results {
            match result {
                Ok(count) => total += count,
                Err(err) => {
                    log::warn!("从 {app} 导入 MCP 失败: {err}");
                    failures.push(format!("{app}: {err}"));
                }
            }
        }

        if failures.is_empty() {
            Ok(total)
        } else {
            Err(AppError::Message(format!(
                "已导入 {total} 个，部分应用导入失败: {}",
                failures.join("; ")
            )))
        }
    }

    // ========================================================================
    // SEC-A：审批状态查询、确认与一次性继承迁移
    // ========================================================================

    /// 汇总所有服务器的审批状态。`!approved` 即待审批（无论启用位如何——
    /// 禁用的未批准条目在启用前同样必须先过审批）。
    pub fn approval_states(state: &AppState) -> Result<Vec<McpServerApprovalState>, AppError> {
        let servers = Self::get_all_servers(state)?;
        let mut out = Vec::new();
        for server in servers.values() {
            let mut apps: HashMap<String, McpAppApprovalState> = HashMap::new();
            for app in [AppType::Claude, AppType::Codex] {
                apps.insert(
                    app.as_str().to_string(),
                    McpAppApprovalState {
                        enabled: server.apps.is_enabled_for(&app),
                        approved: Self::is_approved(state, server, &app),
                        revision: Self::approval_revision(server),
                    },
                );
            }
            out.push(McpServerApprovalState {
                server_id: server.id.clone(),
                apps,
            });
        }
        Ok(out)
    }

    /// 审批确认：绑定预览修订，锁内重读当前内容比对（§4.3-6）；一致才批准，
    /// 随后启用并立即投影到该应用。
    pub fn approve_server(
        state: &AppState,
        server_id: &str,
        app: &AppType,
        expected_revision: &str,
    ) -> Result<(), AppError> {
        if !matches!(app, AppType::Claude | AppType::Codex) {
            return Err(AppError::InvalidInput(
                "MCP 审批仅支持 Claude / Codex 应用".to_string(),
            ));
        }

        let servers = Self::get_all_servers(state)?;
        let server = servers
            .get(server_id)
            .cloned()
            .ok_or_else(|| AppError::InvalidInput(format!("MCP 服务器不存在: {server_id}")))?;

        let current_revision = Self::approval_revision(&server);
        if current_revision != expected_revision {
            return Err(AppError::localized(
                "mcp.approval_content_changed",
                "MCP 服务器内容在确认前已发生变化，请基于最新内容重新确认。",
                "The MCP server content changed before confirmation; review the latest content and confirm again.",
            ));
        }

        Self::record_approval(state, &server, app)?;
        let updated = state
            .db
            .update_mcp_server_app_enabled(server_id, app, true)?;
        if let Some(updated) = updated {
            Self::sync_server_to_app(state, &updated, app)?;
        }
        Ok(())
    }

    /// SEC-A（§4.3-8）：旧版升级的一次性有限继承。只对「本机 DB 内容与本机
    /// 当前托管 live 内容一致」的既有条目补记批准；读不到 live 或内容有歧义
    /// 一律保持待审批，绝不在启动时批准整库。
    ///
    /// 由启动流程在 DB 初始化后调用一次；守卫键写入本机 settings（B 级），
    /// 不随导出/导入迁移。
    pub fn migrate_local_approval_inheritance(state: &AppState) -> Result<(), AppError> {
        const GUARD_KEY: &str = "mcp_approvals_migrated";
        if state.db.get_setting(GUARD_KEY)?.is_some() {
            return Ok(());
        }

        let servers = Self::get_all_servers(state)?;
        for server in servers.values() {
            for app in [AppType::Claude, AppType::Codex] {
                if !server.apps.is_enabled_for(&app) {
                    continue;
                }
                let live_matches = match &app {
                    AppType::Claude => {
                        crate::mcp::claude_live_matches_spec(&server.id, &server.server)
                    }
                    AppType::Codex => {
                        crate::mcp::codex_live_matches_spec(&server.id, &server.server)
                    }
                    AppType::Pi => false,
                };
                if live_matches {
                    log::info!(
                        "MCP 服务器 '{}' 在 {app:?} 上与本机 live 一致，继承批准",
                        server.id
                    );
                    Self::record_approval(state, server, &app)?;
                }
            }
        }

        state.db.set_setting(GUARD_KEY, "true")
    }
}

#[cfg(test)]
mod tests {
    //! SEC-A 验收矩阵（施工方案 §4.4）：投影门禁、审批生命周期、内容绑定、
    //! 一次性继承。live 文件全部落在 TestHomeGuard 的临时家目录，只写哨兵
    //! 配置（echo 命令），不执行任何进程。
    use super::*;
    use crate::app_config::McpApps;
    use crate::secrets::InMemorySecretStore;
    use crate::test_support::TestHomeGuard;
    use serde_json::json;
    use std::sync::Arc;

    fn test_state() -> AppState {
        AppState::new(
            Arc::new(crate::database::Database::memory().expect("create memory db")),
            Arc::new(InMemorySecretStore::new()),
        )
    }

    fn server(id: &str, spec: serde_json::Value) -> McpServer {
        McpServer {
            id: id.to_string(),
            name: id.to_string(),
            server: spec,
            apps: McpApps {
                claude: true,
                codex: false,
            },
            description: None,
            homepage: None,
            docs: None,
            tags: Vec::new(),
        }
    }

    fn claude_mcp_path() -> std::path::PathBuf {
        crate::config::get_home_dir().join(".claude.json")
    }

    fn live_claude_ids() -> Vec<String> {
        crate::claude_mcp::read_mcp_servers_map()
            .expect("read live claude mcp")
            .into_keys()
            .collect()
    }

    #[test]
    fn canonical_json_ignores_key_order_but_preserves_array_order_and_values() {
        let a = json!({"command": "echo", "args": ["a", "b"], "env": {"K": "v"}});
        let b = json!({"env": {"K": "v"}, "args": ["a", "b"], "command": "echo"});
        assert_eq!(canonical_json(&a), canonical_json(&b), "键序不应影响修订");

        let c = json!({"command": "echo", "args": ["b", "a"], "env": {"K": "v"}});
        assert_ne!(canonical_json(&a), canonical_json(&c), "数组必须保序");

        let d = json!({"command": "echo ", "args": ["a", "b"], "env": {"K": "v"}});
        assert_ne!(
            canonical_json(&a),
            canonical_json(&d),
            "不得做改变语义的 trim"
        );

        let e = json!({"command": "echo", "args": ["a", "b"], "env": {"K": "V"}});
        assert_ne!(canonical_json(&a), canonical_json(&e), "大小写参与比较");
    }

    /// 外部新增（无批准）不投影；审批后恢复投影。
    #[test]
    #[serial_test::serial]
    fn unapproved_new_server_is_not_projected_until_approved() {
        let _home = TestHomeGuard::new();
        let state = test_state();
        std::fs::write(claude_mcp_path(), "{}").expect("seed live file");

        let s = server(
            "ext-server",
            json!({"type": "stdio", "command": "echo", "args": ["hi"]}),
        );
        // 模拟外部导入：直接写 DB 行（绕过表单 upsert 的自动批准），启用位为 true
        state.db.save_mcp_server(&s).expect("save imported server");

        McpService::sync_enabled_for_app(&state, &AppType::Claude).expect("project");
        assert!(
            !live_claude_ids().contains(&"ext-server".to_string()),
            "未批准的新服务器不得写入 live"
        );

        // 审批确认（绑定当前修订）→ 投影
        let revision = McpService::approval_revision(&s);
        McpService::approve_server(&state, "ext-server", &AppType::Claude, &revision)
            .expect("approve");
        assert!(
            live_claude_ids().contains(&"ext-server".to_string()),
            "批准后应恢复投影"
        );
    }

    /// 同 ID 内容变化使旧批准失效，且投影时从 live 移除旧内容。
    #[test]
    #[serial_test::serial]
    fn content_change_invalidates_approval_and_removes_stale_live_entry() {
        let _home = TestHomeGuard::new();
        let state = test_state();
        std::fs::write(claude_mcp_path(), "{}").expect("seed live file");

        let s = server(
            "svc",
            json!({"type": "stdio", "command": "echo", "args": ["ok"]}),
        );
        McpService::upsert_server(&state, s.clone()).expect("upsert");
        McpService::sync_enabled_for_app(&state, &AppType::Claude).expect("project");
        assert!(live_claude_ids().contains(&"svc".to_string()));

        // 模拟外部导入改写了 args（命令内容变化），启用位不变
        let mutated = server(
            "svc",
            json!({"type": "stdio", "command": "echo", "args": ["pwned"]}),
        );
        state.db.save_mcp_server(&mutated).expect("import mutation");

        let servers = state.db.get_all_mcp_servers().expect("read servers");
        let current = servers.get("svc").expect("server exists");
        assert!(!McpService::is_approved(&state, current, &AppType::Claude));

        McpService::sync_enabled_for_app(&state, &AppType::Claude).expect("project");
        assert!(
            !live_claude_ids().contains(&"svc".to_string()),
            "旧批准失效后不得沿用 ID 信任，live 旧内容应被移除"
        );
    }

    /// 完全一致的已批准条目同步后保持批准，不反复骚扰。
    #[test]
    #[serial_test::serial]
    fn identical_synced_content_keeps_approval() {
        let _home = TestHomeGuard::new();
        let state = test_state();
        let s = server("stable", json!({"type": "stdio", "command": "echo"}));
        McpService::upsert_server(&state, s.clone()).expect("upsert");
        assert!(McpService::is_approved(&state, &s, &AppType::Claude));

        // 再跑一次「导入」同样内容（写入 DB 相同内容）后仍批准
        state
            .db
            .save_mcp_server(&s)
            .expect("re-import same content");
        assert!(McpService::is_approved(&state, &s, &AppType::Claude));
    }

    /// 确认绑定预览修订：内容在确认前变化则拒绝。
    #[test]
    #[serial_test::serial]
    fn approve_rejects_stale_revision() {
        let _home = TestHomeGuard::new();
        let state = test_state();
        let s = server(
            "svc",
            json!({"type": "stdio", "command": "echo", "args": ["a"]}),
        );
        state.db.save_mcp_server(&s).expect("save");

        let stale = McpService::approval_revision(&s);
        let mutated = server(
            "svc",
            json!({"type": "stdio", "command": "echo", "args": ["b"]}),
        );
        state
            .db
            .save_mcp_server(&mutated)
            .expect("mutate before confirm");

        let err = McpService::approve_server(&state, "svc", &AppType::Claude, &stale)
            .expect_err("内容已变化必须拒绝");
        assert!(
            err.to_string().contains("内容"),
            "错误应提示内容变化: {err}"
        );
    }

    /// 单应用批准不自动批准另一应用；未批准应用启用开关被拒。
    #[test]
    #[serial_test::serial]
    fn approval_is_per_app_and_pending_toggle_is_rejected() {
        let _home = TestHomeGuard::new();
        let state = test_state();
        let mut s = server("both", json!({"type": "stdio", "command": "echo"}));
        s.apps = McpApps {
            claude: true,
            codex: true,
        };
        // 模拟外部导入（直接写 DB，不批准）
        state.db.save_mcp_server(&s).expect("save");

        let revision = McpService::approval_revision(&s);
        McpService::approve_server(&state, "both", &AppType::Claude, &revision)
            .expect("approve claude");

        let servers = state.db.get_all_mcp_servers().expect("read");
        let current = servers.get("both").expect("exists");
        assert!(McpService::is_approved(&state, current, &AppType::Claude));
        assert!(!McpService::is_approved(&state, current, &AppType::Codex));

        // 未批准应用走 toggle 也必须被门禁拦下（§4.3-5）
        let err = McpService::toggle_app(&state, "both", AppType::Codex, true)
            .expect_err("未批准的 toggle 必须被拒");
        assert!(err.to_string().contains("批准"), "错误应指向审批: {err}");
    }

    /// 手工表单路径（upsert）自动批准，正常编辑可用；删除清理审批行。
    #[test]
    #[serial_test::serial]
    fn upsert_auto_approves_and_delete_cleans_approvals() {
        let _home = TestHomeGuard::new();
        let state = test_state();
        let s = server("manual", json!({"type": "stdio", "command": "echo"}));
        McpService::upsert_server(&state, s.clone()).expect("upsert");
        let servers = state.db.get_all_mcp_servers().expect("read");
        let saved = servers.get("manual").expect("exists");
        assert!(McpService::is_approved(&state, saved, &AppType::Claude));

        McpService::delete_server(&state, "manual").expect("delete");
        assert_eq!(
            state
                .db
                .get_mcp_approved_revision("manual", "claude")
                .expect("read approval"),
            None,
            "删除条目后审批行不应残留"
        );
    }

    /// 一次性继承：只批准「本机 DB 与本机托管 live 一致」的条目，其余待审批；
    /// 守卫键保证只跑一次。
    #[test]
    #[serial_test::serial]
    fn migration_inherits_only_live_matching_entries_once() {
        let _home = TestHomeGuard::new();
        let state = test_state();

        // live：svc-match 与 DB 一致；svc-drift 与 DB 不一致
        std::fs::write(
            claude_mcp_path(),
            r#"{"mcpServers": {"svc-match": {"type": "stdio", "command": "echo", "args": ["same"]}, "svc-drift": {"type": "stdio", "command": "echo", "args": ["live"]}}}"#,
        )
        .expect("seed live");
        let matching = server(
            "svc-match",
            json!({"type": "stdio", "command": "echo", "args": ["same"]}),
        );
        let drifting = server(
            "svc-drift",
            json!({"type": "stdio", "command": "echo", "args": ["db"]}),
        );
        state.db.save_mcp_server(&matching).expect("save");
        state.db.save_mcp_server(&drifting).expect("save");

        McpService::migrate_local_approval_inheritance(&state).expect("migrate");

        let servers = state.db.get_all_mcp_servers().expect("read");
        let m = servers.get("svc-match").expect("exists");
        let d = servers.get("svc-drift").expect("exists");
        assert!(
            McpService::is_approved(&state, m, &AppType::Claude),
            "一致条目应继承批准"
        );
        assert!(
            !McpService::is_approved(&state, d, &AppType::Claude),
            "歧义条目必须待审批"
        );

        // 守卫键：迁移后内容改成与 live 一致也不再自动批准
        let now_matching = server(
            "svc-drift",
            json!({"type": "stdio", "command": "echo", "args": ["live"]}),
        );
        state.db.save_mcp_server(&now_matching).expect("save");
        let servers = state.db.get_all_mcp_servers().expect("read");
        let d2 = servers.get("svc-drift").expect("exists");
        assert!(!McpService::is_approved(&state, d2, &AppType::Claude));
    }
}
