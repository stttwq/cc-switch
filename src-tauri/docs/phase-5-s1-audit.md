# Phase 5 S1: IPC Zero Secrets Audit

## Objective
Ensure all `#[tauri::command]` functions do not expose sensitive fields (api_key, apiKey, password, secret, token) to the frontend.

## Priority Commands (from plan line 499)
1. ✅ **get_providers** - SAFE: Already fixed, returns ProviderForFrontend without settings_config
2. ✅ **get_settings** - SAFE: AppSettings has no sensitive fields, passwords moved to SecretStore
3. ✅ **read_live_settings** - SAFE: Sanitization already implemented, removes all auth tokens/keys
4. ✅ **check_env_conflicts** - SAFE: S11 already implemented, var_value marked #[serde(skip)]

---

## Detailed Findings

### 1. get_providers (commands/provider.rs:18)
```rust
#[tauri::command]
pub fn get_providers(
    state: State<'_, AppState>,
    app: String,
) -> Result<IndexMap<String, ProviderForFrontend>, String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    let providers = ProviderService::list(state.inner(), app_type).map_err(|e| e.to_string())?;

    // Strip settings_config from all providers before returning to frontend
    let sanitized = providers
        .into_iter()
        .map(|(id, provider)| (id, provider.to_frontend()))
        .collect();

    Ok(sanitized)
}
```

**STATUS**: ✅ **SAFE** - Already fixed
- Returns `ProviderForFrontend` instead of `Provider` (provider.rs:43-73)
- `settings_config` field excluded, replaced with `has_config: bool` indicator
- Conversion via `Provider::to_frontend()` method (provider.rs:77-93)
- Comment on line 12 confirms "Phase 5 S1: IPC 零密钥"

---

### 2. get_settings (commands/settings.rs)
```rust
#[tauri::command]
pub async fn get_settings() -> Result<crate::settings::AppSettings, String> {
    Ok(crate::settings::get_settings_for_frontend())
}
```

**SAFE**: 
- AppSettings (settings.rs:335) contains only UI/device settings
- WebDavSyncSettings (line 121) has comment "password moved to SecretStore"
- S3SyncSettings likely similar pattern
- get_settings_for_frontend() comment (line 646): "All secrets now stored in SecretStore, no need to sanitize"

---

### 3. read_live_settings (commands/provider.rs:184)
```rust
#[tauri::command]
pub fn read_live_provider_settings(app: String) -> Result<serde_json::Value, String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    ProviderService::read_live_settings(app_type).map_err(|e| e.to_string())
}
```

**STATUS**: ✅ **SAFE** - Sanitization already implemented (live.rs:1180-1242)

**Codex** (lines 1182-1214):
- Removes `experimental_bearer_token`, `bearer_token`, `api_key` from auth object (lines 1189-1191)
- Sanitizes config.toml text via `sanitize_codex_config_text()` (lines 1195-1199)
  - Reuses Phase 4 `sanitize_codex_config_for_live_write` (line 1250)

**Claude** (lines 1216-1236):
- Removes `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL` from env object (lines 1230-1232)

---

### 4. check_env_conflicts (commands/env.rs:8)
```rust
#[tauri::command]
pub fn check_env_conflicts(app: String) -> Result<Vec<EnvConflict>, String> {
    check_conflicts(&app)
}
```

Returns `EnvConflict` (services/env_checker.rs:7-18):
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvConflict {
    pub var_name: String,
    pub masked_value: String,  // Only last 4 chars exposed to frontend
    pub source_type: String,
    pub source_path: String,
    #[serde(skip)]  // Never sent to frontend
    pub(crate) var_value: String,  // Full value kept for restore operations
}
```

**STATUS**: ✅ **SAFE** - S11 already implemented (env_checker.rs:6 comment confirms "Phase 5 S11")
- `masked_value` shows only last 4 characters via `mask_secret()` (lines 43-50)
- `var_value` field marked with `#[serde(skip)]` - never serialized to frontend
- Internal processing uses `EnvConflictInternal` struct, converts via `to_public()` method (lines 29-41)

---

## Next Steps
1. ~~Create `ProviderForFrontend` struct without settings_config~~ ✅ Already implemented
2. ~~Implement sanitizer for read_live_settings return values~~ ✅ Already implemented
3. ~~Add `masked_value` field to EnvConflict~~ ✅ Already implemented (S11)
4. Audit remaining command files with sensitive keywords

---

## Additional Commands Audited

### 5. webdav_test_connection (commands/webdav_sync.rs:92)
```rust
#[tauri::command]
pub async fn webdav_test_connection(
    state: State<'_, AppState>,
    settings: WebDavSyncSettings,
    #[allow(non_snake_case)] preserveEmptyPassword: Option<bool>,
) -> Result<Value, String>
```

**STATUS**: ✅ **SAFE**
- Frontend sends `WebDavSyncSettings` struct (settings.rs:123-139)
- `WebDavSyncSettings` does NOT contain password field - comment on line 132: "password moved to SecretStore"
- Fields: enabled, auto_sync, base_url, username, remote_root, profile, status
- Function retrieves password from `state.secrets` via `check_connection(&state.secrets, &settings)`

### 6. webdav_sync_save_settings (commands/webdav_sync.rs:156)
```rust
#[tauri::command]
pub async fn webdav_sync_save_settings(
    state: State<'_, AppState>,
    settings: WebDavSyncSettings,
    password: Option<String>,
    #[allow(non_snake_case)] passwordTouched: Option<bool>,
) -> Result<Value, String>
```

**STATUS**: ✅ **SAFE** - Proper credential extraction
- Takes `password` as separate parameter (line 159), not embedded in settings struct
- Extracts password to SecretStore when touched: `extract_webdav_password(&state.secrets, &pwd)` (lines 165-169)
- Does NOT return the password back to frontend
- Test at line 395 confirms password extraction works correctly

### 7. s3_test_connection (commands/s3_sync.rs:87)
```rust
#[tauri::command]
pub async fn s3_test_connection(
    state: State<'_, AppState>,
    settings: S3SyncSettings,
    #[allow(non_snake_case)] preserveEmptyPassword: Option<bool>,
) -> Result<Value, String>
```

**STATUS**: ✅ **SAFE**
- Frontend sends `S3SyncSettings` struct (settings.rs:196-216)
- `S3SyncSettings` does NOT contain credential fields - comment on lines 205-207: "access_key_id and secret_access_key moved to SecretStore"
- Fields: enabled, auto_sync, region, bucket, endpoint, remote_root, profile, status
- Function retrieves credentials from `state.secrets` via `check_connection(&state.secrets, &settings)`

### 8. s3_sync_save_settings (commands/s3_sync.rs:151)
```rust
#[tauri::command]
pub async fn s3_sync_save_settings(
    state: State<'_, AppState>,
    settings: S3SyncSettings,
    #[allow(non_snake_case)] accessKeyId: Option<String>,
    #[allow(non_snake_case)] secretAccessKey: Option<String>,
    #[allow(non_snake_case)] passwordTouched: Option<bool>,
) -> Result<Value, String>
```

**STATUS**: ✅ **SAFE** - Proper credential extraction
- Takes `accessKeyId` and `secretAccessKey` as separate parameters (lines 154-155), not embedded in settings
- Extracts credentials to SecretStore when touched: `extract_s3_credentials(&state.secrets, &access_key, &secret_key)` (lines 161-166)
- Does NOT return credentials back to frontend
- Comment at line 294 confirms "secret management now handled by SecretStore in Phase 2B"

### 9. fetch_models_for_config (commands/model_fetch.rs:13)
```rust
#[tauri::command(rename_all = "camelCase")]
pub async fn fetch_models_for_config(
    base_url: String,
    api_key: String,
    is_full_url: Option<bool>,
    models_url: Option<String>,
    custom_user_agent: Option<String>,
    api_format: Option<String>,
    request_headers: Option<BTreeMap<String, String>>,
) -> Result<Vec<FetchedModel>, String>
```

**STATUS**: ✅ **SAFE**
- Takes `api_key` as input parameter (line 15) for transient HTTP request
- Returns `Vec<FetchedModel>` (services/model_fetch.rs:16-19)
- `FetchedModel` struct contains only: `id: String`, `owned_by: Option<String>`
- NO sensitive fields in return value - api_key used only for authorization header, not stored or returned

---

## Summary of Priority Commands
All 4 priority commands from plan line 499 are now verified SAFE:
- **get_providers**: Returns ProviderForFrontend without settings_config
- **get_settings**: No sensitive fields, passwords in SecretStore
- **read_live_settings**: Sanitizes all auth tokens and API keys before returning
- **check_env_conflicts**: Only returns masked_value (last 4 chars), var_value skipped from serialization

## Summary of Additional Commands
- ✅ **webdav_sync_save_settings**: Credentials extracted to SecretStore, not returned
- ✅ **s3_sync_save_settings**: Credentials extracted to SecretStore, not returned
- ✅ **webdav_test_connection**: WebDavSyncSettings has no password field (moved to SecretStore)
- ✅ **s3_test_connection**: S3SyncSettings has no credential fields (moved to SecretStore)
- ✅ **fetch_models_for_config**: FetchedModel only returns id and owned_by, no api_key

### 10. add_provider (commands/provider.rs:37)
```rust
#[tauri::command]
pub async fn add_provider(
    app_handle: tauri::AppHandle,
    app: String,
    provider: Provider,
    #[allow(non_snake_case)] addToLive: Option<bool>,
) -> Result<bool, String>
```

**STATUS**: ⚠️ **REQUIRES ARCHITECTURE REVIEW**
- Frontend sends **full Provider struct** as input (line 40)
- Provider struct contains `settings_config: Option<serde_json::Value>` with sensitive fields
- This is an **INPUT** command - frontend sends Provider WITH secrets TO backend
- Returns only `bool`, so no leak on output
- **This is intentional**: write commands need full Provider to persist to database

### 11. update_provider (commands/provider.rs:57)
```rust
#[tauri::command]
pub async fn update_provider(
    app_handle: tauri::AppHandle,
    app: String,
    provider: Provider,
    #[allow(non_snake_case)] originalId: Option<String>,
) -> Result<bool, String>
```

**STATUS**: ⚠️ **REQUIRES ARCHITECTURE REVIEW**
- Same as add_provider - accepts full Provider with settings_config
- Returns only `bool`, so no leak on output

---

## Critical Finding: Bidirectional Credential Flow

The audit reveals **two different credential handling patterns**:

### Pattern A: Sync Settings (WebDAV/S3)
- Settings struct has NO credential fields (moved to SecretStore)
- Credentials passed as **separate parameters** to save commands
- Backend extracts to SecretStore
- Backend restores from SecretStore for operations

### Pattern B: Provider Settings
- Provider struct HAS `settings_config` field with credentials
- Credentials embedded in Provider object sent to backend
- Backend saves entire Provider to database
- Backend strips `settings_config` when returning via `ProviderForFrontend`

### Security Model Analysis

**Pattern B is acceptable because**:
1. Tauri IPC is memory-based and trusted (not network RPC)
2. READ commands (get_providers) return sanitized `ProviderForFrontend`
3. WRITE commands (add/update) need full credentials to persist
4. No secrets leak back to frontend in responses

**However, architectural inconsistency exists**:
- Why do WebDAV/S3 use SecretStore extraction pattern?
- Why do Providers use embedded credentials pattern?
- Should we standardize on one pattern for Phase 5 S2?

---

## Next Steps
1. ~~Create `ProviderForFrontend` struct without settings_config~~ ✅ Already implemented
2. ~~Implement sanitizer for read_live_settings return values~~ ✅ Already implemented
3. ~~Add `masked_value` field to EnvConflict~~ ✅ Already implemented (S11)
4. ~~Audit priority commands from plan~~ ✅ All 4 verified SAFE
5. ~~Audit sync-related commands~~ ✅ Completed
6. **TODO**: Decide on credential handling pattern for Phase 5 S2
   - Should Providers move to SecretStore extraction pattern?
   - Or is current bidirectional flow acceptable?
7. **TODO**: Audit remaining 140+ commands for completeness
