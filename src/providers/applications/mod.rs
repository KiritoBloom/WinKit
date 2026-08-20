//! Application provider architecture.
//!
//! An application provider understands one user-facing application (Chrome,
//! and later Edge, VS Code, ...). Providers declare capabilities and
//! availability honestly; the core never assumes an application can do
//! something its provider has not implemented.

pub mod chrome;

use crate::errors::WinkitError;
use crate::models::{
    ApplicationInfo, ApplicationState, BrowserInfo, MemoryInfo, NetworkSummary, PerformanceMetrics,
    RuntimeInfo, TabInfo, TrendInfo,
};
use crate::permissions::Capability;
use crate::providers::windows::WindowsBackend;
use crate::providers::BoxFuture;
use crate::providers::Provider;
use std::collections::HashMap;

/// A combined tab diagnostics payload (models::diagnostics + browser data).
#[derive(Debug, Clone)]
pub struct TabDiagnostics {
    pub tab: TabInfo,
    pub resource_usage: serde_json::Value,
    pub performance: PerformanceMetrics,
    pub memory: MemoryInfo,
    pub network: NetworkSummary,
    pub runtime: RuntimeInfo,
    pub report: crate::models::DiagnosticReport,
}

/// Application adapter interface. Async operations return boxed
/// futures so implementors can drive WebSocket clients.
pub trait ApplicationProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn version(&self) -> &'static str;
    fn capabilities(&self) -> Vec<Capability>;
    /// Current availability state, re-evaluated cheaply.
    fn state(&self) -> ApplicationState;
    /// Full availability + connection information.
    fn info(&self) -> BoxFuture<'_, Result<ApplicationInfo, WinkitError>>;

    // Capability dispatch. The default implementations return
    // `UnsupportedCapability`; adapters override what they implement.

    fn list_tabs(&self) -> BoxFuture<'_, Result<Vec<TabInfo>, WinkitError>> {
        Box::pin(async {
            Err(WinkitError::unsupported_capability(
                "this application provider does not implement tab listing",
            ))
        })
    }

    fn get_tab(&self, _tab_id: &str) -> BoxFuture<'_, Result<TabInfo, WinkitError>> {
        Box::pin(async {
            Err(WinkitError::unsupported_capability(
                "this application provider does not implement tab lookup",
            ))
        })
    }

    fn get_active_tab(&self) -> BoxFuture<'_, Result<TabInfo, WinkitError>> {
        Box::pin(async {
            Err(WinkitError::unsupported_capability(
                "this application provider does not implement active-tab detection",
            ))
        })
    }

    fn tab_performance(
        &self,
        _tab_id: &str,
    ) -> BoxFuture<'_, Result<PerformanceMetrics, WinkitError>> {
        Box::pin(async {
            Err(WinkitError::unsupported_capability(
                "this application provider does not implement performance inspection",
            ))
        })
    }

    fn tab_memory(&self, _tab_id: &str) -> BoxFuture<'_, Result<MemoryInfo, WinkitError>> {
        Box::pin(async {
            Err(WinkitError::unsupported_capability(
                "this application provider does not implement memory inspection",
            ))
        })
    }

    fn tab_network(&self, _tab_id: &str) -> BoxFuture<'_, Result<NetworkSummary, WinkitError>> {
        Box::pin(async {
            Err(WinkitError::unsupported_capability(
                "this application provider does not implement network inspection",
            ))
        })
    }

    fn tab_runtime(&self, _tab_id: &str) -> BoxFuture<'_, Result<RuntimeInfo, WinkitError>> {
        Box::pin(async {
            Err(WinkitError::unsupported_capability(
                "this application provider does not implement runtime inspection",
            ))
        })
    }

    fn tab_diagnostics(
        &self,
        _tab_id: &str,
        _windows: &dyn WindowsBackend,
    ) -> BoxFuture<'_, Result<TabDiagnostics, WinkitError>> {
        Box::pin(async {
            Err(WinkitError::unsupported_capability(
                "this application provider does not implement tab diagnostics",
            ))
        })
    }

    /// Time-series view of a tab over an observation window.
    fn tab_trend(
        &self,
        _tab_id: &str,
        _observe_ms: u64,
    ) -> BoxFuture<'_, Result<TrendInfo, WinkitError>> {
        Box::pin(async {
            Err(WinkitError::unsupported_capability(
                "this application provider does not implement tab trends",
            ))
        })
    }

    /// Browser-wide info (Chrome-only concept for now).
    fn browser_info(&self) -> BoxFuture<'_, Result<BrowserInfo, WinkitError>> {
        Box::pin(async {
            Err(WinkitError::unsupported_capability(
                "this application provider does not expose browser-wide info",
            ))
        })
    }
}

/// Registry of application providers, keyed by id.
#[derive(Default)]
pub struct ApplicationRegistry {
    providers: HashMap<String, Box<dyn ApplicationProvider>>,
}

impl ApplicationRegistry {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }

    pub fn register(&mut self, provider: Box<dyn ApplicationProvider>) {
        self.providers.insert(provider.id().to_string(), provider);
    }

    pub fn get(&self, id: &str) -> Option<&dyn ApplicationProvider> {
        self.providers.get(id).map(|p| p.as_ref())
    }

    pub fn all(&self) -> Vec<&dyn ApplicationProvider> {
        let mut v: Vec<_> = self.providers.values().map(|p| p.as_ref()).collect();
        v.sort_by(|a, b| a.id().cmp(b.id()));
        v
    }

    pub fn is_registered(&self, id: &str) -> bool {
        self.providers.contains_key(id)
    }
}

impl Provider for Box<dyn ApplicationProvider> {
    fn id(&self) -> &'static str {
        (**self).id()
    }

    fn name(&self) -> &'static str {
        (**self).display_name()
    }

    fn version(&self) -> &'static str {
        (**self).version()
    }

    fn availability(&self) -> crate::providers::ProviderAvailability {
        crate::providers::ProviderAvailability::Conditional
    }

    fn capabilities(&self) -> Vec<Capability> {
        (**self).capabilities()
    }
}
