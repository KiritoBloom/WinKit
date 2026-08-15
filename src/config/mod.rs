//! Configuration subsystem: schema and loader.

pub mod loader;
pub mod schema;

pub use schema::{
    ChromeConfig, Config, DiagnosticsConfig, HealthConfig, LimitsConfig, PermissionConfig,
    ProvidersConfig, ServerConfig, ToolsConfig, TrendsConfig, WebConfig, WorkspacesConfig,
};
