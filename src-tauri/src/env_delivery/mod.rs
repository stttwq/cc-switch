//! Environment variable delivery for credential injection
//!
//! This module provides the mechanism to deliver credentials to CLI applications
//! via user-level environment variables (HKCU\Environment on Windows).

mod ownership;
mod sink;

pub use ownership::{check_conflict, EnvConflict, ManagedEnvVars};
pub use sink::default_sink;
pub use sink::EnvSink;

#[cfg(test)]
pub use sink::InMemoryEnvSink;
