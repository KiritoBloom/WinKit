//! Chrome adapter: the first deep application provider.
//!
//! Implements the [`ApplicationProvider`] trait against a
//! [`ChromeSession`], distinguishing availability states honestly (§56):
//! not installed → installed → running → endpoint available → connected.

pub mod cdp;
pub mod discovery;
pub mod managed;
pub mod session;

use crate::config::ChromeConfig;
use crate::errors::WinkitError;
use crate::models::*;
use crate::permissions::Capability;
use crate::providers::applications::chrome::discovery::ChromeState;
use crate::providers::applications::chrome::session::ChromeSession;
use crate::providers::applications::{ApplicationProvider, TabDiagnostics};
use crate::providers::windows::WindowsBackend;
use crate::providers::BoxFuture;
use std::sync::Arc;

/// Chrome adapter state guard so tools can query availability cheaply.
#[derive(Clone)]
pub struct ChromeProvider {
    session: Arc<ChromeSession>,
}

impl std::fmt::Debug for ChromeProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChromeProvider").finish_non_exhaustive()
    }
}

impl ChromeProvider {
    pub fn new(config: ChromeConfig, windows: Arc<dyn WindowsBackend>) -> Self {
        Self {
            session: Arc::new(ChromeSession::new(config, windows)),
        }
    }

    pub fn session(&self) -> &Arc<ChromeSession> {
        &self.session
    }

    /// Map the discovery state into the generic application-state model.
    pub async fn application_state(&self) -> ApplicationState {
        match self.session.state().await {
            ChromeState::NotInstalled => ApplicationState::NotInstalled,
            ChromeState::Installed => ApplicationState::InstalledNotRunning,
            ChromeState::Running => ApplicationState::RunningNotInspectable,
            ChromeState::EndpointUnavailable => ApplicationState::RunningNotInspectable,
            ChromeState::EndpointAvailable => ApplicationState::RunningInspectable,
            ChromeState::Connected => ApplicationState::Connected,
        }
    }
}

impl ApplicationProvider for ChromeProvider {
    fn id(&self) -> &'static str {
        "chrome"
    }

    fn display_name(&self) -> &'static str {
        "Google Chrome"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::ApplicationDiscover,
            Capability::ApplicationTabsRead,
            Capability::ApplicationPerformanceRead,
            Capability::ApplicationMemoryRead,
            Capability::ApplicationNetworkRead,
            Capability::ApplicationRuntimeRead,
            Capability::ApplicationDiagnosticsRead,
        ]
    }

    fn state(&self) -> ApplicationState {
        // Cheap synchronous approximation; the async `info()` re-evaluates.
        // The first `info()` call drives the real discovery.
        ApplicationState::RunningInspectable
    }

    fn info(&self) -> BoxFuture<'_, Result<ApplicationInfo, WinkitError>> {
        let session = self.session.clone();
        Box::pin(async move {
            let result = session.discover().await?;
            let state = match session.state().await {
                ChromeState::NotInstalled => ApplicationState::NotInstalled,
                ChromeState::Installed => ApplicationState::InstalledNotRunning,
                ChromeState::Running => ApplicationState::RunningNotInspectable,
                ChromeState::EndpointUnavailable => ApplicationState::RunningNotInspectable,
                ChromeState::EndpointAvailable => ApplicationState::RunningInspectable,
                ChromeState::Connected => ApplicationState::Connected,
            };
            let endpoint = result.endpoint.clone();
            let tabs = session.list_tabs().await.unwrap_or_default().len();
            Ok(ApplicationInfo {
                id: "chrome".to_string(),
                display_name: "Google Chrome".to_string(),
                version: endpoint
                    .as_ref()
                    .map(|e| e.browser_version.clone())
                    .unwrap_or_else(|| "unknown".to_string()),
                state,
                capabilities: [
                    "application.discover",
                    "application.tabs.read",
                    "application.performance.read",
                    "application.memory.read",
                    "application.network.read",
                    "application.runtime.read",
                    "application.diagnostics.read",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect(),
                details: serde_json::json!({
                    "state_detail": result.state.describe(),
                    "installed_path": result.installed_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
                    "running_processes": result.running_processes.len(),
                    "devtools_port": endpoint.as_ref().map(|e| e.port),
                    "endpoint": endpoint.as_ref().map(|e| serde_json::json!({
                        "port": e.port,
                        "browser_version": e.browser_version,
                        "protocol_version": e.protocol_version,
                    })),
                    "tabs": tabs,
                }),
            })
        })
    }

    fn list_tabs(&self) -> BoxFuture<'_, Result<Vec<TabInfo>, WinkitError>> {
        let session = self.session.clone();
        Box::pin(async move { session.list_tabs().await })
    }

    fn get_tab(&self, tab_id: &str) -> BoxFuture<'_, Result<TabInfo, WinkitError>> {
        let session = self.session.clone();
        let tab_id = tab_id.to_string();
        Box::pin(async move { session.get_tab(&tab_id).await })
    }

    fn get_active_tab(&self) -> BoxFuture<'_, Result<TabInfo, WinkitError>> {
        let session = self.session.clone();
        Box::pin(async move { session.get_active_tab().await })
    }

    fn tab_performance(
        &self,
        tab_id: &str,
    ) -> BoxFuture<'_, Result<PerformanceMetrics, WinkitError>> {
        let session = self.session.clone();
        let tab_id = tab_id.to_string();
        Box::pin(async move {
            let bundle = session.collect_tab_metrics(&tab_id, 0).await?;
            Ok(session.to_performance(&bundle))
        })
    }

    fn tab_memory(&self, tab_id: &str) -> BoxFuture<'_, Result<MemoryInfo, WinkitError>> {
        let session = self.session.clone();
        let tab_id = tab_id.to_string();
        Box::pin(async move {
            let bundle = session.collect_tab_metrics(&tab_id, 0).await?;
            Ok(session.to_memory(&bundle))
        })
    }

    fn tab_network(&self, tab_id: &str) -> BoxFuture<'_, Result<NetworkSummary, WinkitError>> {
        let session = self.session.clone();
        let tab_id = tab_id.to_string();
        let observe = session.observe_window_ms();
        Box::pin(async move {
            let bundle = session.collect_tab_metrics(&tab_id, observe).await?;
            Ok(session.to_network(&bundle))
        })
    }

    fn tab_runtime(&self, tab_id: &str) -> BoxFuture<'_, Result<RuntimeInfo, WinkitError>> {
        let session = self.session.clone();
        let tab_id = tab_id.to_string();
        let observe = session.observe_window_ms();
        Box::pin(async move {
            let bundle = session.collect_tab_metrics(&tab_id, observe).await?;
            Ok(session.to_runtime(&bundle))
        })
    }

    fn tab_diagnostics(
        &self,
        tab_id: &str,
        _windows: &dyn WindowsBackend,
    ) -> BoxFuture<'_, Result<TabDiagnostics, WinkitError>> {
        let session = self.session.clone();
        let tab_id = tab_id.to_string();
        let engine = crate::diagnostics::DiagnosticsEngine::with_defaults();
        let observe = session.observe_window_ms();
        Box::pin(async move { session.tab_diagnostics(&tab_id, observe, &engine).await })
    }

    fn tab_trend(
        &self,
        tab_id: &str,
        observe_ms: u64,
    ) -> BoxFuture<'_, Result<TrendInfo, WinkitError>> {
        let session = self.session.clone();
        let tab_id = tab_id.to_string();
        Box::pin(async move { session.tab_trend(&tab_id, observe_ms).await })
    }

    fn browser_info(&self) -> BoxFuture<'_, Result<BrowserInfo, WinkitError>> {
        let session = self.session.clone();
        Box::pin(async move { session.browser_info().await })
    }
}
