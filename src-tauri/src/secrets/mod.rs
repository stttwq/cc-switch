pub mod cleanup;
mod extractor;
pub mod legacy;
pub mod migration;
mod rules;
pub mod scan;
mod store;
pub mod sync_secrets;
mod target;
mod types;

pub use extractor::{
    hydrate, load_known_targets, provider_target_prefix, save_known_targets, SecretExtractor,
};
pub use migration::{CredentialMigrator, MigratedProviderInfo, MigrationReport};
pub use rules::{
    escape_literal, is_literal_value, is_sensitive_config_key, last_chars,
    normalize_env_key_segment, pi_api_key_env_name, pi_header_env_name, unescape_literal,
};
pub use store::{
    InMemorySecretStore, SecretError, SecretStore, UnsupportedSecretStore, WindowsSecretStore,
};
pub use sync_secrets::{
    extract_s3_credentials, extract_webdav_password, restore_s3_credentials,
    restore_webdav_password,
};
pub use target::SecretTarget;
pub use types::Extracted;
pub use types::ProviderSecrets;
