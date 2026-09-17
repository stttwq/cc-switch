mod extractor;
pub mod cleanup;
pub mod migration;
pub mod scan;
mod rules;
mod store;
pub mod sync_secrets;
mod target;
mod types;

pub use extractor::{hydrate, SecretExtractor};
pub use migration::{CredentialMigrator, MigrationReport, MigratedProviderInfo};
pub use rules::{escape_literal, is_literal_value, is_sensitive_config_key, normalize_env_key_segment, unescape_literal};
pub use types::Extracted;
pub use store::{InMemorySecretStore, SecretStore, WindowsSecretStore};
pub use sync_secrets::{
    extract_s3_credentials, extract_webdav_password, restore_s3_credentials,
    restore_webdav_password,
};
pub use target::SecretTarget;
pub use types::ProviderSecrets;
