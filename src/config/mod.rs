//! Configuration subsystem: schema and loader.

pub mod loader;
pub mod schema;

pub use schema::{
    Config, DiagnosticsConfig, HealthConfig, LimitsConfig, PermissionConfig,
    ProvidersConfig, ServerConfig, ToolsConfig, TrendsConfig, WebConfig, WorkspacesConfig,
};
