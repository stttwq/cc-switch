//! Environment variable ownership tracking and conflict detection
//!
//! Tracks which environment variables are managed by cc-switch (stored in DB)
//! and detects conflicts with foreign variables.

use crate::error::AppError;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Metadata about a managed environment variable
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManagedEnvEntry {
    pub app: String,
    pub provider: String,
    pub set_at: String,
}

/// Environment variable ownership registry
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManagedEnvVars {
    #[serde(flatten)]
    pub entries: BTreeMap<String, ManagedEnvEntry>,
}

impl ManagedEnvVars {
    pub fn load_from_disk() -> Result<Self, AppError> {
        let path = crate::config::get_app_config_dir().join("cc-switch.db");
        let conn = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        let json: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'managed_env_vars'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| AppError::Database(e.to_string()))?;
        match json {
            Some(json) => serde_json::from_str(&json)
                .map_err(|e| AppError::Config(format!("Failed to parse managed_env_vars: {e}"))),
            None => Ok(Self::default()),
        }
    }

    /// Load from settings DB
    pub fn load(db: &crate::database::Database) -> Result<Self, AppError> {
        match db.get_setting("managed_env_vars")? {
            Some(json) => serde_json::from_str(&json)
                .map_err(|e| AppError::Config(format!("Failed to parse managed_env_vars: {e}"))),
            None => Ok(Self::default()),
        }
    }

    /// Save to settings DB
    pub fn save(&self, db: &crate::database::Database) -> Result<(), AppError> {
        let json = serde_json::to_string(&self)
            .map_err(|e| AppError::Config(format!("Failed to serialize managed_env_vars: {e}")))?;
        db.set_setting("managed_env_vars", &json)
    }

    /// Mark a variable as managed by cc-switch
    pub fn register(&mut self, name: &str, app: &str, provider: &str) {
        self.entries.insert(
            name.to_string(),
            ManagedEnvEntry {
                app: app.to_string(),
                provider: provider.to_string(),
                set_at: chrono::Utc::now().to_rfc3339(),
            },
        );
    }

    /// Unregister a variable
    pub fn unregister(&mut self, name: &str) {
        self.entries.remove(name);
    }

    /// Check if a variable is managed by cc-switch
    pub fn is_managed(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    /// Get all variable names for a specific app
    pub fn vars_for_app(&self, app: &str) -> Vec<String> {
        self.entries
            .iter()
            .filter(|(_, entry)| entry.app == app)
            .map(|(name, _)| name.clone())
            .collect()
    }
}

/// Information about an environment variable conflict
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvConflict {
    pub name: String,
    pub owner: String,        // "foreign" or app name
    pub masked_value: String, // last 4 chars only
}

/// Check for conflicts before setting an environment variable
pub fn check_conflict(
    sink: &dyn crate::env_delivery::EnvSink,
    managed: &ManagedEnvVars,
    name: &str,
    new_value: &str,
) -> Result<Option<EnvConflict>, AppError> {
    let current = sink.get(name)?;

    match current {
        None => Ok(None), // No conflict, variable doesn't exist
        Some(existing_value) if existing_value == new_value => {
            Ok(None) // Same value, no conflict
        }
        Some(_existing_value) if managed.is_managed(name) => {
            Ok(None) // We own it, can overwrite
        }
        Some(existing_value) => {
            // Foreign variable with different value
            let masked = mask_value(&existing_value);
            Ok(Some(EnvConflict {
                name: name.to_string(),
                owner: "foreign".to_string(),
                masked_value: masked,
            }))
        }
    }
}

/// Mask a value, showing only last 4 characters
fn mask_value(value: &str) -> String {
    let n = value.chars().count();
    if n <= 4 {
        "*".repeat(n)
    } else {
        format!("***{}", crate::secrets::last_chars(value, 4))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env_delivery::EnvSink;
    use zeroize::Zeroizing;

    #[test]
    fn managed_env_vars_roundtrip() {
        let mut managed = ManagedEnvVars::default();
        managed.register("ANTHROPIC_API_KEY", "claude", "provider-1");
        managed.register("CC_SWITCH_CODEX_API_KEY", "codex", "provider-2");

        assert!(managed.is_managed("ANTHROPIC_API_KEY"));
        assert!(managed.is_managed("CC_SWITCH_CODEX_API_KEY"));
        assert!(!managed.is_managed("OPENAI_API_KEY"));

        let claude_vars = managed.vars_for_app("claude");
        assert_eq!(claude_vars.len(), 1);
        assert_eq!(claude_vars[0], "ANTHROPIC_API_KEY");

        managed.unregister("ANTHROPIC_API_KEY");
        assert!(!managed.is_managed("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn mask_value_hides_prefix() {
        assert_eq!(mask_value("sk-ant-1234567890"), "***7890");
        assert_eq!(mask_value("short"), "***hort");
        assert_eq!(mask_value("abc"), "***");
        assert_eq!(mask_value("ab"), "**");
    }

    #[test]
    fn check_conflict_detects_foreign_variable() {
        use crate::env_delivery::InMemoryEnvSink;

        let sink = InMemoryEnvSink::new();
        let managed = ManagedEnvVars::default();

        // Set a foreign variable
        sink.set(
            "ANTHROPIC_API_KEY",
            &Zeroizing::new("foreign-key-1234".to_string()),
        )
        .unwrap();

        let conflict = check_conflict(&sink, &managed, "ANTHROPIC_API_KEY", "our-key-5678")
            .unwrap()
            .expect("Should detect conflict");

        assert_eq!(conflict.name, "ANTHROPIC_API_KEY");
        assert_eq!(conflict.owner, "foreign");
        assert_eq!(conflict.masked_value, "***1234");
    }

    #[test]
    fn check_conflict_allows_managed_overwrite() {
        use crate::env_delivery::InMemoryEnvSink;

        let sink = InMemoryEnvSink::new();
        let mut managed = ManagedEnvVars::default();

        sink.set(
            "ANTHROPIC_API_KEY",
            &Zeroizing::new("old-value".to_string()),
        )
        .unwrap();
        managed.register("ANTHROPIC_API_KEY", "claude", "provider-1");

        let conflict = check_conflict(&sink, &managed, "ANTHROPIC_API_KEY", "new-value").unwrap();
        assert!(conflict.is_none(), "Should allow overwriting managed var");
    }

    #[test]
    fn check_conflict_allows_same_value() {
        use crate::env_delivery::InMemoryEnvSink;

        let sink = InMemoryEnvSink::new();
        let managed = ManagedEnvVars::default();

        sink.set(
            "ANTHROPIC_API_KEY",
            &Zeroizing::new("same-value".to_string()),
        )
        .unwrap();

        let conflict = check_conflict(&sink, &managed, "ANTHROPIC_API_KEY", "same-value").unwrap();
        assert!(conflict.is_none(), "Same value should not conflict");
    }
}
