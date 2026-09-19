//! Environment variable delivery for credential injection
//!
//! This module provides the mechanism to deliver credentials to CLI applications
//! via user-level environment variables (HKCU\Environment on Windows).

mod ownership;
mod sink;

pub use ownership::{check_conflict, EnvConflict, ManagedEnvVars};
pub use sink::default_sink;
pub use sink::EnvSink;
// 与 `secrets::InMemorySecretStore` 同一形态：测试支撑类型对集成测试可见，
// 这样 §5.3.3 的所有权/冲突逻辑能注入同一个 sink 实例后被子断言（不能只在单测里活着）。
pub use sink::InMemoryEnvSink;
