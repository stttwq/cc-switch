//! 模型列表获取命令
//!
//! 提供 Tauri 命令，供前端在供应商表单中获取可用模型列表。

use crate::app_config::AppType;
use crate::services::model_fetch::{self, FetchedModel, ModelFetchError};
use crate::services::ProviderService;
use crate::store::AppState;
use std::collections::BTreeMap;
use std::str::FromStr;
use tauri::Manager;
use zeroize::Zeroizing;

/// 获取供应商的可用模型列表
///
/// 使用 OpenAI 兼容的 GET /v1/models 端点。优先使用 `models_url` 精确覆写；
/// 否则对 baseURL 生成候选列表（含「剥离 Anthropic 兼容子路径」兜底），按序尝试。
/// 错误以结构化 [`ModelFetchError`] 返回（SEC-C），不携带原始请求 URL。
#[tauri::command(rename_all = "camelCase")]
pub async fn fetch_models_for_config(
    base_url: String,
    api_key: String,
    is_full_url: Option<bool>,
    models_url: Option<String>,
    api_format: Option<String>,
    request_headers: Option<BTreeMap<String, String>>,
) -> Result<Vec<FetchedModel>, ModelFetchError> {
    model_fetch::fetch_models(
        &base_url,
        &api_key,
        is_full_url.unwrap_or(false),
        models_url.as_deref(),
        api_format.as_deref(),
        request_headers.as_ref(),
    )
    .await
}

/// 按供应商身份获取模型列表（编辑态「获取模型」）
///
/// P3 安全设计（原方案 §7.1-2）：回显读到的明文只进输入框本地展示状态、绝不写
/// 回表单受控值，因此编辑已存供应商时表单里的 API Key 恒为空（空白 = 保留）。
/// 此命令由后端按 `app + providerId` 从凭据后端解析**已存**的 API Key 再发起
/// 请求——明文密钥不过 IPC。这是用户的显式动作（零调用承诺的显式动作清单），
/// 1Password 模式下计入一次 `op`。
///
/// 错误按类型返回（SEC-C）：密钥未配置 → `key_not_configured`；凭据后端锁定 /
/// 读取失败 → `key_unavailable`（可重试）。具体原因只进本机日志，不进错误载荷。
#[tauri::command(rename_all = "camelCase")]
#[allow(clippy::too_many_arguments)] // Tauri 命令的参数即 IPC 面，与 fetch_models_for_config 对齐
pub async fn fetch_models_for_provider(
    app_handle: tauri::AppHandle,
    app: String,
    provider_id: String,
    base_url: String,
    is_full_url: Option<bool>,
    models_url: Option<String>,
    api_format: Option<String>,
    request_headers: Option<BTreeMap<String, String>>,
) -> Result<Vec<FetchedModel>, ModelFetchError> {
    let app_type =
        AppType::from_str(&app).map_err(|_| ModelFetchError::new("invalid_request", false))?;
    // 凭据读取是阻塞调用（可能等待 `op` 子进程），不能在 async 上下文直接跑。
    let key = tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle
            .try_state::<AppState>()
            .ok_or_else(|| ModelFetchError::new("invalid_request", false))?;
        resolve_stored_api_key(state.inner(), &app_type, &provider_id)
    })
    .await
    .map_err(|e| {
        log::warn!("模型获取密钥解析任务失败: {e}");
        ModelFetchError::new("key_unavailable", true)
    })??;
    model_fetch::fetch_models(
        &base_url,
        key.as_str(),
        is_full_url.unwrap_or(false),
        models_url.as_deref(),
        api_format.as_deref(),
        request_headers.as_ref(),
    )
    .await
}

/// 从凭据后端解析供应商已存 API Key。供应商不存在或未配置密钥时返回
/// `key_not_configured`（前端映射为「请先填写 API Key」），不区分这两种情况
/// 以免本命令成为「探测某供应商是否存有凭据」的侧信道。
pub(crate) fn resolve_stored_api_key(
    state: &AppState,
    app_type: &AppType,
    provider_id: &str,
) -> Result<Zeroizing<String>, ModelFetchError> {
    let secrets =
        ProviderService::fetch_provider_secrets(state, app_type, provider_id).map_err(|e| {
            // AppError 的本地化文案不含秘密与 URL，可安全进本机日志。
            log::warn!(
                "模型获取解析 {}/{} 已存密钥失败: {e}",
                app_type.as_str(),
                provider_id
            );
            ModelFetchError::new("key_unavailable", true)
        })?;
    match secrets.api_key {
        Some(key) if !key.is_empty() => Ok(key),
        _ => Err(ModelFetchError::new("key_not_configured", false)),
    }
}
