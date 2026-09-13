# Phase 5 S1: IPC Zero Secrets Audit

## Objective
Ensure all `#[tauri::command]` functions do not expose sensitive fields (api_key, apiKey, password, secret, token) to the frontend.

## Priority Commands (from plan line 499)
1. ✅ **get_providers** - ISSUE FOUND: returns full Provider with settings_config
2. ✅ **get_settings** - SAFE: AppSettings has no sensitive fields, passwords moved to SecretStore
3. ✅ **read_live_settings** - ISSUE FOUND: returns raw live config files with secrets
4. ⏳ **check_env_conflicts** - IN PROGRESS

---

## Detailed Findings

### 1. get_providers (commands/provider.rs:18)
```rust
#[tauri::command]
pub fn get_providers(
    state: State<'_, AppState>,
    app: String,
) -> Result<IndexMap<String, Provider>, String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    ProviderService::list(state.inner(), app_type).map_err(|e| e.to_string())
}
```

**ISSUE**: Returns `Provider` struct (provider.rs:11) which includes:
- `pub settings_config: Value` - Could contain plaintext secrets or `literal:***` markers

**FIX REQUIRED**: Create sanitized `ProviderForFrontend` struct that excludes or redacts settings_config.

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

### 3. read_live_settings (commands/provider.rs:175)
```rust
#[tauri::command]
pub fn read_live_provider_settings(app: String) -> Result<serde_json::Value, String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    ProviderService::read_live_settings(app_type).map_err(|e| e.to_string())
}
```

**ISSUE**: Returns raw live config file contents:
- **Codex** (live.rs:1182): Returns `{ auth: {...}, config: "..." }` from auth.json + config.toml
  - auth.json could contain bearer tokens (before Phase 4 migration)
  - config.toml text returned as-is
- **Claude** (live.rs:1206): Returns raw settings.json content
  - Could contain ANTHROPIC_API_KEY if not migrated

**FIX REQUIRED**: Sanitize live config before returning to frontend, similar to Phase 4 live_sanitizer logic.

---

### 4. check_env_conflicts (commands/env.rs:8)
```rust
#[tauri::command]
pub fn check_env_conflicts(app: String) -> Result<Vec<EnvConflict>, String> {
    check_conflicts(&app)
}
```

Returns `EnvConflict` (services/env_checker.rs:7):
```rust
pub struct EnvConflict {
    pub var_name: String,
    pub var_value: String,  // <-- SENSITIVE: full env var value
    pub source_type: String,
    pub source_path: String,
}
```

**ISSUE**: `var_value` contains full plaintext value of environment variables (API keys, tokens).

**FIX REQUIRED**: Per plan line 511 (S11), should only return `masked_value` (last 4 chars).

---

## Next Steps
1. Create `ProviderForFrontend` struct without settings_config
2. Implement sanitizer for read_live_settings return values
3. Add `masked_value` field to EnvConflict, remove var_value from IPC
4. Audit remaining command files with sensitive keywords
