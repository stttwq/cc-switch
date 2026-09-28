use serde_json::{json, Value};

use crate::error::AppError;
use crate::services::{PromptService, ProviderService};
use crate::settings;
use crate::store::AppState;

/// S4-7（P1-6）：导入前拍一份本机 Pi DB 行，供 [`run_post_import_sync`] 判断
/// 「哪些 Pi 供应商在导入中变了」。只读本地库，0 次 op。
pub(crate) fn snapshot_pi_providers(
    app_state: &AppState,
) -> Result<indexmap::IndexMap<String, crate::provider::Provider>, AppError> {
    app_state
        .db
        .get_all_providers(crate::app_config::AppType::Pi.as_str())
}

/// S4-7（P1-6 / D-S6 决策 A）：后处理把导入带来的 Pi 变化写回 `models.json`。
///
/// Pi 的原生契约是「`models.json` 为真源」，历来后处理跳过 Pi，代价是 Pi 的
/// 模型设置**同步了也不生效**（B 下载后，下次开 Pi 页就被本机旧值改回去）。
/// 传 `pi_before`（导入前快照）即启用这一步；为 `None` 时行为不变。
pub(crate) fn run_post_import_sync(
    app_state: &AppState,
    pi_before: Option<&indexmap::IndexMap<String, crate::provider::Provider>>,
) -> Result<(), AppError> {
    let mut failures = Vec::new();

    // S1-4（§9-9）：导入会改写 Pi 的 DB 行，让 models.json 指纹失效，
    // 下次进入 Pi 列表必须与原生重新对齐（S4 起所有导入入口统一走这里）。
    crate::services::provider::invalidate_native_fingerprint();

    // S4-7：把「已启用且本次导入有变化」的 Pi 供应商写回原生配置。
    if let Some(before) = pi_before {
        match crate::services::provider::apply_imported_configs_to_native(app_state, before) {
            Ok(0) => {}
            Ok(applied) => log::info!("[Import] Pi 模型设置已写回 models.json：{applied} 个"),
            Err(error) => failures.push(format!("pi native apply: {error}")),
        }
    }

    // S4-3：全量重建「未关联 1Password」清单。三条导入路径（同步下载 / SQL 导入 /
    // `.db` 恢复）都走这个入口，所以清单只需在这里算一次。必须是全量重建而非
    // 增量：供应商可能在别处已关联，保留陈旧条目会让横幅一直挂着。
    let backend = if crate::settings::is_onepassword_backend() {
        crate::database::snapshot_policy::BackendKind::OnePassword
    } else {
        crate::database::snapshot_policy::BackendKind::CredentialManager
    };
    match crate::database::snapshot_policy::list_unlinked_providers(app_state.db.as_ref(), backend)
        .and_then(crate::settings::set_onepassword_unlinked)
    {
        Ok(()) => {}
        Err(error) => failures.push(format!("unlinked providers: {error}")),
    }

    if let Err(error) = ProviderService::sync_current_to_live(app_state) {
        failures.push(format!("live configuration: {error}"));
    }
    if let Err(error) = PromptService::sync_all_to_live(app_state) {
        failures.push(format!("prompts: {error}"));
    }
    if let Err(error) = settings::reload_settings() {
        failures.push(format!("settings cache: {error}"));
    }

    match app_state.db.get_log_config() {
        Ok(log_config) => log::set_max_level(log_config.to_level_filter()),
        Err(error) => {
            log::set_max_level(log::LevelFilter::Info);
            failures.push(format!("runtime log level: {error}"));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(AppError::Message(format!(
            "部分导入后同步失败: {}",
            failures.join("; ")
        )))
    }
}

fn post_sync_warning<E: std::fmt::Display>(err: E) -> String {
    AppError::localized(
        "sync.post_operation_sync_failed",
        format!("后置同步状态失败: {err}"),
        format!("Post-operation synchronization failed: {err}"),
    )
    .to_string()
}

/// S5-3（P1-5）：下载结果是「回滚冲突」时，快照没有落地到本机，
/// 后处理（scrub / live 刷新）应整体跳过，直接把冲突返回给前端。
pub(crate) fn is_rollback_conflict(result: &Value) -> bool {
    result.get("status").and_then(Value::as_str) == Some("rollbackConflict")
}

/// S5-3（P1-5）：后处理各步（scrub、live 刷新……）独立执行后，把所有失败合并为
/// 一个错误（由调用方降级为 warning）。原先用 `.and_then` 串行，scrub 失败会
/// 吞掉整个 live 刷新。
pub(crate) fn combine_post_sync_results(
    results: Vec<Result<(), AppError>>,
) -> Result<(), AppError> {
    let failures: Vec<String> = results
        .into_iter()
        .filter_map(|result| result.err().map(|e| e.to_string()))
        .collect();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(AppError::Message(format!(
            "部分导入后同步失败: {}",
            failures.join("; ")
        )))
    }
}

pub(crate) fn post_sync_warning_from_result(
    result: Result<Result<(), AppError>, String>,
) -> Option<String> {
    match result {
        Ok(Ok(())) => None,
        Ok(Err(err)) => Some(post_sync_warning(err)),
        Err(err) => Some(post_sync_warning(err)),
    }
}

pub(crate) fn attach_warning(mut value: Value, warning: Option<String>) -> Value {
    if let Some(message) = warning {
        if let Some(obj) = value.as_object_mut() {
            obj.insert("warning".to_string(), Value::String(message));
        }
    }
    value
}

/// S4-3：给同步下载结果补上「未关联供应商」计数。
///
/// 后处理（`run_post_import_sync`）刚全量重建过清单，这里直接读回（0 次 op，
/// 只读本机 settings）。为 0 时不写字段，前端按缺省处理。
pub(crate) fn attach_unlinked_count(mut value: Value) -> Value {
    let count = crate::settings::get_onepassword_unlinked().len();
    if count > 0 {
        if let Some(obj) = value.as_object_mut() {
            obj.insert("unlinkedProviders".to_string(), json!(count));
        }
    }
    value
}

/// S4-3：导入成功的结果载荷。
///
/// `adopted_refs` / `unlinkedProviders` 让前端能直接告诉用户「采纳了几条 1P
/// 关联、还有几个没关联上」，不必再发一次查询。计数均为 0 时省略，前端按
/// `?? 0` 兜底。
pub(crate) fn success_payload_with_warning(
    backup_id: String,
    warning: Option<String>,
    adopted_refs: usize,
    unlinked_providers: usize,
) -> Value {
    let mut payload = json!({
        "success": true,
        "message": "SQL imported successfully",
        "backupId": backup_id
    });
    if let Some(obj) = payload.as_object_mut() {
        if adopted_refs > 0 {
            obj.insert("adoptedRefs".to_string(), json!(adopted_refs));
        }
        if unlinked_providers > 0 {
            obj.insert("unlinkedProviders".to_string(), json!(unlinked_providers));
        }
    }
    attach_warning(payload, warning)
}

#[cfg(test)]
mod tests {
    use super::{
        attach_warning, combine_post_sync_results, is_rollback_conflict,
        post_sync_warning_from_result,
    };
    use crate::error::AppError;
    use serde_json::json;

    #[test]
    fn is_rollback_conflict_matches_only_rollback_status() {
        // S5-3：只有 rollbackConflict 跳过后处理；正常下载照常执行。
        assert!(is_rollback_conflict(&json!({
            "status": "rollbackConflict",
            "remoteSeq": 3,
            "lastApplied": 9,
        })));
        assert!(!is_rollback_conflict(&json!({ "status": "downloaded" })));
        assert!(!is_rollback_conflict(&json!({})));
    }

    #[test]
    fn combine_post_sync_results_merges_all_failures() {
        // S5-3：scrub 与 live 刷新独立执行后，所有失败都要出现在合并结果里，
        // 不能因为第一步失败就丢掉后面步骤的失败信息。
        assert!(combine_post_sync_results(vec![Ok(()), Ok(())]).is_ok());

        let merged = combine_post_sync_results(vec![
            Err(AppError::Config("scrub boom".into())),
            Err(AppError::Config("live boom".into())),
        ])
        .expect_err("failures must merge into one error");
        let text = merged.to_string();
        assert!(text.contains("scrub boom"), "unexpected: {text}");
        assert!(text.contains("live boom"), "unexpected: {text}");
    }

    #[test]
    fn post_sync_warning_from_result_returns_none_on_success() {
        let warning = post_sync_warning_from_result(Ok(Ok(())));
        assert!(warning.is_none());
    }

    #[test]
    fn post_sync_warning_from_result_returns_some_on_sync_error() {
        let warning =
            post_sync_warning_from_result(Ok(Err(crate::error::AppError::Config("boom".into()))));
        assert!(warning.is_some());
    }

    #[tokio::test]
    async fn post_sync_warning_from_result_returns_some_on_join_error() {
        let handle = tokio::spawn(async move {
            panic!("forced join error");
        });
        let join_err = handle.await.expect_err("task should panic");
        let warning = post_sync_warning_from_result(Err(join_err.to_string()));
        assert!(warning.is_some());
    }

    #[test]
    fn attach_warning_adds_warning_without_dropping_existing_fields() {
        let payload = json!({ "status": "downloaded" });
        let updated = attach_warning(payload, Some("post sync warning".to_string()));
        assert_eq!(
            updated.get("status").and_then(|v| v.as_str()),
            Some("downloaded")
        );
        assert_eq!(
            updated.get("warning").and_then(|v| v.as_str()),
            Some("post sync warning")
        );
    }
}
