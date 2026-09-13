mod extractor;
pub mod migration;
mod rules;
mod store;
pub mod sync_secrets;
mod target;
mod types;

pub use extractor::SecretExtractor;
pub use migration::{CredentialMigrator, MigrationReport, MigratedProviderInfo};
pub use rules::{escape_literal, is_literal_value, is_sensitive_config_key, unescape_literal};
pub use store::{InMemorySecretStore, SecretStore, WindowsSecretStore};
pub use sync_secrets::{
    extract_s3_credentials, extract_webdav_password, restore_s3_credentials,
    restore_webdav_password,
};
pub use target::SecretTarget;
pub use types::ProviderSecrets;
