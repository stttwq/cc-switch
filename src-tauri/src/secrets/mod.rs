pub mod cleanup;
mod extractor;
pub mod legacy;
pub mod migration;
mod migration_1p;
mod onepassword;
pub mod portable;
mod rules;
pub mod scan;
mod store;
pub mod sync_secrets;
mod target;
mod types;
mod vault;

pub use extractor::{
    hydrate, load_known_targets, provider_target_prefix, save_known_targets, SecretExtractor,
};
pub use migration::{CredentialMigrator, MigratedProviderInfo, MigrationReport};
pub use rules::{
    escape_literal, is_literal_value, is_sensitive_config_key, last_chars,
    normalize_env_key_segment, pi_api_key_env_name, pi_header_env_name, unescape_literal,
};
#[cfg(target_os = "windows")]
pub(crate) use store::{windows_delete_credential, windows_enumerate_targets};
pub use store::{
    InMemorySecretStore, SecretError, SecretStore, UnsupportedSecretStore, WindowsSecretStore,
};
pub use sync_secrets::{
    fetch_sync_credentials, store_s3_credentials, store_sync_passphrase, store_webdav_password,
    SyncCredentials,
};
pub use target::SecretTarget;
pub use types::Extracted;
pub use types::ProviderSecrets;
pub use vault::{
    CountingVault, InMemoryVault, LegacyWindowsVault, SecretBundle, SecretGroup, SecretVault,
    UnavailableVault, VaultError, VaultRef, VaultStatus, FIELD_API_KEY, FIELD_APP_PREFIX,
    FIELD_BASE_URL, FIELD_ENV_PREFIX,
};
pub use onepassword::{
    build_runtime_vault, from_settings as onepassword_from_settings, list_accounts, list_vaults,
    locate_op, op_version, probe as onepassword_probe, verify_op_signature, OnePasswordVault,
    OpAccount, OpProbe, OpVault,
};
pub use migration_1p::{migrate_to_onepassword, MigrationReport as OnePasswordMigrationReport};
