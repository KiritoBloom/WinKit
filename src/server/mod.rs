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
use crate::diagnostics::DiagnosticsEngine;
use crate::errors::WinkitError;
use crate::permissions::PermissionManager;
use crate::providers::applications::chrome::managed::ManagedChromeManager;
use crate::providers::applications::chrome::ChromeProvider;
use crate::providers::applications::{ApplicationProvider, ApplicationRegistry};
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
    /// Registered application adapters (capability-bearing).
    pub applications: ApplicationRegistry,
    /// The OS-level backend every tool reads through.
    pub windows: Arc<dyn WindowsBackend>,
    /// Deterministic diagnostics engine.
    pub engine: DiagnosticsEngine,
    /// Tool definitions and dispatch.
    pub tools: ToolRegistry,
    /// WinKit-owned managed Chrome sessions (feature-gated, permission-gated).
    pub managed: Arc<ManagedChromeManager>,
}

impl AppState {
    /// Build state with the real Windows backend.
    pub fn build(config: Config) -> Result<Arc<Self>, WinkitError> {
        Self::with_backend(config, Arc::new(RealWindowsBackend::new()))
    }

    /// Build state with an explicit backend (tests inject mocks here).
    pub fn with_backend(
        config: Config,
        windows: Arc<dyn WindowsBackend>,
    ) -> Result<Arc<Self>, WinkitError> {
        let mode = config.permission_mode()?;
        let permissions = PermissionManager::new(mode);

        let mut providers = ProviderRegistry::new();
        let mut applications = ApplicationRegistry::new();

        // `enabled: []` means "all built-in providers".
        let enabled = &config.providers.enabled;
        let all_enabled = enabled.is_empty();

        if all_enabled || enabled.iter().any(|p| p == "windows") {
            providers.register(&WindowsProvider::new(windows.clone()));
        }

        // The managed-Chrome manager needs the discovery session so a
        // managed browser exit can invalidate the discovery cache.
        let mut chrome_exit_hook: Option<Arc<dyn Fn() + Send + Sync>> = None;
        if all_enabled || enabled.iter().any(|p| p == "chrome") {
            let provider = Arc::new(ChromeProvider::new(config.chrome.clone(), windows.clone()));
            let hook_session = provider.session().clone();
            chrome_exit_hook = Some(Arc::new(move || {
                let s = hook_session.clone();
                tokio::spawn(async move {
                    s.invalidate_discovery().await;
                });
            }));
            let chrome: Box<dyn ApplicationProvider> = Box::new((*provider).clone());
            providers.register(&chrome);
            applications.register(chrome);
        }

        let managed = Arc::new(ManagedChromeManager::new(
            config.chrome.clone(),
            chrome_exit_hook,
        ));

        let engine = DiagnosticsEngine::with_config(config.diagnostics.clone());
        let tools = ToolRegistry::build(&config);

        Ok(Arc::new(Self {
            config,
            permissions,
            providers,
            applications,
            windows,
            engine,
            tools,
            managed,
        }))
    }
}
