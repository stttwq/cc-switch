use crate::services::pi_state::{PiCurrentState, PiStateService};
use crate::session_manager::providers::pi::PiSessionDiscovery;
use crate::store::AppState;
use tauri::State;

#[tauri::command]
pub(crate) fn get_pi_current_state(state: State<'_, AppState>) -> Result<PiCurrentState, String> {
    PiStateService::current(state.inner()).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn get_pi_session_discovery() -> PiSessionDiscovery {
    crate::session_manager::providers::pi::session_discovery()
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
