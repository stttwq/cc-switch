use crate::services::pi_state::{PiCurrentState, PiStateService};
use crate::session_manager::providers::pi::PiSessionDiscovery;
use crate::store::AppState;
use tauri::State;

/// S1-5：改为 async + spawn_blocking。历史版本是同步命令（跑在主线程），
/// 而 `ProviderService::list` 可能在同一把 Pi 切换锁内做长时间的原生同步，
/// 主线程 block_on 等锁就是「整个窗口冻结」的直接原因；服务层同时已去掉
/// 读侧等锁（pi_state.rs），这里是双保险——任何重活都不占用主线程。
#[tauri::command]
pub(crate) async fn get_pi_current_state(
    state: State<'_, AppState>,
) -> Result<PiCurrentState, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        PiStateService::current(&state).map_err(|error| error.to_string())
    })
    .await
    .map_err(|e| format!("读取 Pi 状态任务执行失败: {e}"))?
}

/// S1-5：会话发现会扫描 Pi 会话目录（潜在磁盘 IO），同样不占主线程。
#[tauri::command]
pub(crate) async fn get_pi_session_discovery() -> PiSessionDiscovery {
    tauri::async_runtime::spawn_blocking(crate::session_manager::providers::pi::session_discovery)
        .await
        .unwrap_or_else(|error| {
            log::warn!("Pi 会话发现任务失败（视为不可用）: {error}");
            PiSessionDiscovery::Unavailable {
                reason: error.to_string(),
            }
        })
}

/// F1-4：「导入到 1Password」——把 Pi 原生同步检测到的明文钥匙收进 vault，
/// models.json 改写为 $VAR 引用。会触发 op（可能弹解锁），故 async + spawn_blocking。
#[tauri::command]
pub(crate) async fn import_pi_plaintext_to_onepassword(
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::services::provider::import_pi_plaintext_to_vault(&state)
    })
    .await
    .map_err(|e| format!("导入 Pi 明文钥匙任务失败: {e}"))?
    .map_err(|e| e.to_string())
}

/// S1-2（D-S9）：把 Pi 端点缓存的改动 merge 进 1Password。用户主动触发
/// （横幅「立即写入」），会调 op（可能弹解锁），async + spawn_blocking。
#[tauri::command]
pub(crate) async fn flush_pi_endpoint_vault_to_onepassword(
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::services::provider::flush_endpoint_vault_pending(&state)
    })
    .await
    .map_err(|e| format!("端点写回任务失败: {e}"))?
    .map_err(|e| e.to_string())
}
