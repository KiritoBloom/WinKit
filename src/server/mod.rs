//! MCP server layer: protocol handling, tool dispatch, stdio transport.
//!
//! The server owns the shared application state and is deliberately
//! independent of Windows implementation details — everything below it goes
//! through provider traits.

pub mod lifecycle;
pub mod profiles;
pub mod protocol;
pub mod registry;
pub mod transport;

use crate::config::Config;
use crate::errors::WinkitError;
use crate::permissions::PermissionManager;
use crate::providers::windows::{RealWindowsBackend, WindowsBackend, WindowsProvider};
use crate::providers::ProviderRegistry;
use crate::tools::ToolRegistry;
use std::sync::Arc;

/// Shared application state, handed to every tool handler.
pub struct AppState {
    pub config: Config,
    pub permissions: PermissionManager,
    /// All registered providers (metadata registry).
    pub providers: ProviderRegistry,
    /// The OS-level backend every tool reads through.
    pub windows: Arc<dyn WindowsBackend>,
    /// Tool definitions and dispatch.
    pub tools: ToolRegistry,
}

impl AppState {
    /// Build state with the real Windows backend.
    pub fn build(config: Config) -> Result<Arc<Self>, WinkitError> {
        let backend = RealWindowsBackend::with_options(
            crate::platform::windows::hardware::HardwareOptions::from_config(&config.hardware),
        );
        Self::with_backend(config, Arc::new(backend))
    }

    /// Build state with an explicit backend (tests inject mocks here).
    pub fn with_backend(
        config: Config,
        windows: Arc<dyn WindowsBackend>,
    ) -> Result<Arc<Self>, WinkitError> {
        let mode = config.permission_mode()?;
        let permissions = PermissionManager::new(mode);

        let mut providers = ProviderRegistry::new();

        // `enabled: []` means "all built-in providers".
        let enabled = &config.providers.enabled;
        let all_enabled = enabled.is_empty();

        if all_enabled || enabled.iter().any(|p| p == "windows") {
            providers.register(&WindowsProvider::new(windows.clone()));
        }

        let tools = ToolRegistry::build(&config);

        Ok(Arc::new(Self {
            config,
            permissions,
            providers,
            windows,
            tools,
        }))
    }
}
